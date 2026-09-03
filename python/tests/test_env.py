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
