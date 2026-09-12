import numpy as np
import pytest

from env import ArchipelagoEnv, RewardContext


def make_env(server_url, **kwargs):
    kwargs.setdefault("max_days", 20)
    return ArchipelagoEnv(server_url, start_server=False, faction=0, **kwargs)


def test_reset_seed_is_deterministic(server_url):
    env = make_env(server_url)
    try:
        obs1, info1 = env.reset(seed=42)
        obs2, info2 = env.reset(seed=42)
    finally:
        env.close()
    assert info1["day"] == info2["day"] == 0
    np.testing.assert_array_equal(obs1, obs2)


def test_fixed_action_sequence_is_deterministic(server_url):
    # A short, fixed sequence of concrete action indices (not just no-ops),
    # exercised against two independent envs so this can't be explained by
    # one env carrying leftover state between runs.
    actions = [0, 1, 2, 50, 0, 3, 0, 0]

    def run():
        env = make_env(server_url)
        obs, _ = env.reset(seed=7)
        trajectory = [obs.copy()]
        rewards = []
        for a in actions:
            obs, reward, terminated, truncated, info = env.step(a)
            trajectory.append(obs.copy())
            rewards.append(reward)
            if terminated or truncated:
                break
        env.close()
        return trajectory, rewards

    traj1, rewards1 = run()
    traj2, rewards2 = run()

    assert len(traj1) == len(traj2)
    for a, b in zip(traj1, traj2):
        np.testing.assert_array_equal(a, b)
    assert rewards1 == rewards2


def test_random_policy_completes_one_episode(server_url):
    env = make_env(server_url, max_days=15)
    obs, info = env.reset(seed=123)
    assert env.observation_space.contains(obs)

    terminated = truncated = False
    steps = 0
    outcome = None
    while not (terminated or truncated):
        action = env.action_space.sample()
        obs, reward, terminated, truncated, info = env.step(action)
        assert env.observation_space.contains(obs)
        assert np.isfinite(reward)
        outcome = info["outcome"]
        steps += 1
        assert steps <= 15, "episode should terminate by max_days"
    env.close()
    assert outcome is not None
    assert outcome["type"] in ("victory", "stalemate")


def test_observation_space_length_matches_schema(server_url):
    env = make_env(server_url)
    try:
        schema = env.schema
        assert env.observation_space.shape == (schema["observation"]["length"],)
        obs, _ = env.reset(seed=1)
        assert obs.shape == env.observation_space.shape
    finally:
        env.close()


def test_invalid_actions_are_rejected_not_raised(server_url):
    env = make_env(server_url)
    try:
        env.reset(seed=1)
        # Only 9 units exist at day 0 across all 3 factions (ids 0..8), so
        # `hold_unit` on unit slot 40 addresses a unit that doesn't exist -
        # the server must reject this, not raise, and report why.
        action = next(
            i
            for i, e in enumerate(env.action_table.entries)
            if e.payload is not None and e.payload.get("type") == "hold_unit" and e.payload.get("unit") == 40
        )
        obs, reward, terminated, truncated, info = env.step(action)
        assert len(info["rejected"]) == 1
        assert info["rejected"][0]["reason"]
        assert info["accepted"] == []
        # No exception was raised getting here - that's the actual assertion
        # this test exists to make.
    finally:
        env.close()


def test_out_of_range_action_index_raises(server_url):
    env = make_env(server_url)
    try:
        env.reset(seed=1)
        with pytest.raises(ValueError):
            env.step(len(env.action_table))
    finally:
        env.close()


# The subset of crates/api/src/action_codec.rs::action_error_key's return
# values relevant to a well-formed action being *legitimately* refused by
# game rules. Kept here (not read off the schema, which doesn't enumerate
# these) purely so `test_new_action_types_round_trip_through_live_server`
# can tell "the sim rejected this because the move isn't legal right now"
# apart from "the decoder rejected this because schema.py sent a malformed
# payload" - the latter is a bug in ActionTable._build(), and would
# otherwise be indistinguishable from ordinary, expected in-game rejection.
KNOWN_ACTION_ERROR_KEYS = {
    "not_owner", "unit_dead", "not_adjacent", "pinned", "region_not_owned", "region_contested",
    "insufficient_manpower", "insufficient_equipment", "invalid_value", "already_building",
    "no_construction", "no_port", "no_airfield", "insufficient_machinery", "invalid_line",
    "line_not_owned", "line_not_hostile", "invalid_node", "node_not_strikeable", "node_not_hostile",
    "no_aircraft_in_range", "no_force_in_range", "already_at_war",
}

# The six action types docs/phase5-spec.md's audit found present in
# crates/sim/src/action.rs and GET /schema but absent from the flattened
# Discrete table - everything except propose_in_natural_language, which
# ActionTable's own doc explains is deliberately excluded (free text has no
# finite Discrete encoding).
NEWLY_ADDED_ACTION_TYPES = {
    "declare_war",
    "break_treaty",
    "respond_to_natural_language_proposal",
    "interdict_line",
    "strike_node",
}


def test_every_schema_action_type_is_reachable(server_url):
    """design.md §14 promises human/heuristic/LLM/RL agents all reach the
    same world through the same Observation -> Action interface. This is
    the guard for the RL side of that promise: every action `type`
    `GET /schema` advertises must have at least one entry in the flattened
    `Discrete` action table, with the one deliberate, documented exception
    (`propose_in_natural_language` - free text isn't a finite choice).

    Derived entirely from the live schema response, not a hardcoded list of
    expected types - so a *future* action type added to `crates/sim` and
    wired into `GET /schema` but never wired into `ActionTable._build()`
    fails this test instead of silently shipping unreachable, which is
    exactly the defect this test was written to catch (`declare_war`,
    `break_treaty`, `interdict_line`, `strike_node`, and
    `respond_to_natural_language_proposal` were all in this state before).
    """
    env = make_env(server_url)
    try:
        declared_types = {a["type"] for a in env.schema["actions"]}
        reachable_types = {e.payload["type"] for e in env.action_table.entries if e.payload is not None}
        deliberately_excluded = {"propose_in_natural_language"}

        missing = declared_types - reachable_types - deliberately_excluded
        assert not missing, (
            f"GET /schema advertises action type(s) {missing} with no entry in the flattened "
            "action table (env.action_table) - add them to ActionTable._build() in "
            "python/env/schema.py, or, if one truly cannot be a finite Discrete choice, "
            "add it to this test's `deliberately_excluded` with a stated reason."
        )
        # And the converse: nothing in the table claims to be a type the
        # live schema doesn't actually know about (a stale or typo'd
        # payload "type" would never decode server-side).
        unknown = reachable_types - declared_types
        assert not unknown, f"action table entries reference type(s) {unknown} that GET /schema does not declare"
    finally:
        env.close()


def test_new_action_types_round_trip_through_live_server(server_url):
    """Each of the six action types added to close the DEFECT 1 gap must not
    just be *present* in the table (the previous test) but actually decode
    on a live server: submit one concrete instance of each and check the
    server reports it as either accepted or rejected-for-a-game-reason,
    never a decode failure (a decode failure would mean this action can
    never legally happen no matter the game state, i.e. still effectively
    unreachable) and never an HTTP error/exception.
    """
    env = make_env(server_url)
    try:
        env.reset(seed=1)
        seen_types: set[str] = set()
        for entry in env.action_table.entries:
            if entry.payload is None or entry.payload["type"] not in NEWLY_ADDED_ACTION_TYPES:
                continue
            if entry.payload["type"] in seen_types:
                continue  # one concrete sample per type is enough
            seen_types.add(entry.payload["type"])

            response = env.client.post(
                "/action",
                {"session_id": env.session_id, "faction": env.faction, "actions": [entry.payload]},
            )
            assert response["accepted"] or response["rejected"], f"{entry.label}: neither accepted nor rejected"
            if response["rejected"]:
                reason = response["rejected"][0]["reason"]
                assert reason in KNOWN_ACTION_ERROR_KEYS, (
                    f"{entry.label} was rejected with {reason!r}, which is not one of "
                    "action_codec::action_error_key's reasons - this looks like a decode "
                    "failure (a malformed payload from ActionTable._build()), not a normal "
                    "in-game rule rejection"
                )
        assert seen_types == NEWLY_ADDED_ACTION_TYPES, f"no table entry exercised: {NEWLY_ADDED_ACTION_TYPES - seen_types}"
    finally:
        env.close()


def test_layers_param_restricts_control_to_named_layers(server_url):
    """docs/phase5-spec.md's per-layer control playtest defect fix
    (`POST /reset`'s `{"faction":..,"layers":[...]}` shape) must actually be
    reachable from `ArchipelagoEnv`, not just from a raw HTTP client - a
    balance investigation wants to train one layer (e.g. economic policy)
    while the built-in heuristic AI keeps fighting that faction's war.
    """
    env = make_env(server_url, layers=["economy"])
    try:
        env.reset(seed=1)
        mil_idx = next(
            i for i in env.action_table.indices_for_layer("military") if env.action_table.entries[i].payload is not None
        )
        obs, reward, terminated, truncated, info = env.step(mil_idx)
        assert len(info["rejected"]) == 1
        assert "military" in info["rejected"][0]["reason"] and "does not control" in info["rejected"][0]["reason"]

        econ_idx = next(
            i
            for i in env.action_table.indices_for_layer("economy")
            if env.action_table.entries[i].payload is not None
            and env.action_table.entries[i].payload["type"] == "set_civilian_ration"
        )
        obs, reward, terminated, truncated, info = env.step(econ_idx)
        # Accepted or rejected for an ordinary game reason - either way, not
        # for lack of layer control, which is the one thing this test cares
        # about (an economy action reaching the decoder/layer check at all).
        if info["rejected"]:
            assert "does not control" not in info["rejected"][0]["reason"]
    finally:
        env.close()


def test_unknown_layer_name_raises(server_url):
    with pytest.raises(ValueError):
        make_env(server_url, layers=["not_a_real_layer"])


def test_empty_layer_list_is_rejected_at_construction(server_url):
    """An env that builds must be usable.

    ``layers=[]`` used to pass validation and then fail *every* ``reset()``
    with HTTP 400, because the server rejects an empty ``layers`` array - a
    training run would die on its first step instead of at the call that got
    it wrong. ``None`` is how you say "the whole faction".

    Confirmed this can fail: removing the ``if not self.layers`` guard makes
    ``make_env(..., layers=[])`` return an env, so ``pytest.raises`` sees no
    exception and the test fails.
    """
    with pytest.raises(ValueError):
        make_env(server_url, layers=[])


def test_custom_reward_function_is_used(server_url):
    seen: list[RewardContext] = []

    def custom_reward(ctx: RewardContext) -> float:
        seen.append(ctx)
        return 1234.5

    env = make_env(server_url, reward_fn=custom_reward)
    try:
        env.reset(seed=5)
        obs, reward, terminated, truncated, info = env.step(0)
    finally:
        env.close()

    assert reward == 1234.5
    assert len(seen) == 1
    assert seen[0].action == 0
