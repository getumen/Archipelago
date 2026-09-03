# Archipelago Gymnasium environment

`python/env` is a [Gymnasium](https://gymnasium.farama.org/)-compatible
environment that drives one faction of an Archipelago game over HTTP,
talking to the Stage 5A `archipelago-api` server (`crates/api`). It is pure
Python - no FFI, no Rust build step on the Python side - and depends on
nothing but `gymnasium` and `numpy`.

Nothing about the observation length or the action vocabulary is
hardcoded here: both are read from the server's `GET /schema` response at
environment-construction time, so this package stays correct even if the
scenario (region count, faction count, goods, treaties, ...) changes on the
Rust side.

## Minimal loop

```bash
cd python
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt

# once, from the repository root:
cargo build --workspace

python examples/random_policy.py
```

`ArchipelagoEnv` builds/locates and launches the `archipelago-api` binary
itself by default (`start_server=True`), so no server needs to already be
running. If you'd rather run the server yourself (e.g. to watch it with
`GET /sessions` or connect a second client), start it first and pass its
URL:

```bash
./target/debug/archipelago-api --bind 127.0.0.1:8080
```

```python
from env import ArchipelagoEnv

env = ArchipelagoEnv(base_url="http://127.0.0.1:8080", start_server=False, faction=0)
```

The full loop, either way:

```python
from env import ArchipelagoEnv

env = ArchipelagoEnv(seed=1, faction=0, opponents="heuristic", max_days=60)
try:
    obs, info = env.reset(seed=1)

    terminated = truncated = False
    total_reward = 0.0
    while not (terminated or truncated):
        action = env.action_space.sample()  # replace with your policy
        obs, reward, terminated, truncated, info = env.step(action)
        total_reward += reward

    print(f"outcome: {info['outcome']}, total_reward: {total_reward}")
finally:
    env.close()
```

Every faction other than `faction` is played by the server's own built-in
heuristic AI (`crates/agents`) - `opponents="heuristic"` is currently the
only supported value, matching what the server implements.

## Observation and action spaces

- `env.observation_space` is a `Box(shape=(N,), dtype=float32)`, where `N`
  comes from `GET /schema`'s `observation.length` field (236 for the
  default MVP scenario: 10 regions x 17 fields, 5 sea zones x 4 fields, 19
  faction scalars, 3 factions x 9 diplomacy fields - see `observation.layout`
  in the schema response, or `crates/sim/src/observation.rs`'s doc comment,
  for the exact field order).
- `env.action_space` is a `Discrete(M)`, flattening the categories
  docs/phase5-spec.md calls for - unit move / recruit / reinforce / build /
  policy change / treaty proposal / no-op - into one integer per concrete
  action. `env.action_table.entries[i].label` gives a human-readable
  description of action `i` (e.g. `"move_unit(unit=3, to=region:6)"`),
  useful for inspecting what a trained policy actually does.
  - Two client-side constants shape this table, since a fixed-size
    `Discrete` space can't accommodate a dynamically-growing unit id or a
    continuous field on its own: `max_unit_slots` (default 48 - unit slot
    `i` addresses `UnitId(i)` directly; raise it if a run recruits enough
    units to run past it) and `value_levels` (default `(0, 0.25, 0.5, 0.75,
    1.0)` - the discretization used for continuous fields like
    `set_conscription`'s value or `set_industry_priority`'s weight). Both
    are constructor arguments.
- An action the simulation refuses (wrong owner, no such unit, insufficient
  manpower, ...) is never an exception - it comes back in
  `info["rejected"]` as `{"index": 0, "reason": "..."}`, exactly per
  `crates/api/src/action_codec.rs::action_error_key`. Only a genuinely
  out-of-range `Discrete` index (`action_space.n <= action`) raises
  `ValueError` - that is a bug in the caller, not a normal game outcome.

## Reward is swappable

`ArchipelagoEnv`'s default reward (`env.reward.default_reward`) just
reports the server's own weighted territory/industry/manpower delta
(`crates/api/src/reward.rs`). Pass a different callable to replace it
entirely - this is the one thing docs/phase5-spec.md calls out as what
researchers will most want to change:

```python
from env import ArchipelagoEnv, RewardContext

def region_count_reward(ctx: RewardContext) -> float:
    # ctx.observation is the same fixed-length float32 vector step()
    # returns; region 0's "owned" flag is observation index 0 (see
    # observation.layout in GET /schema for every other field's offset).
    return float(ctx.observation[0])

env = ArchipelagoEnv(seed=1, faction=0, reward_fn=region_count_reward)
```

`RewardContext` (see `python/env/reward.py`) carries the previous and
current observation, the raw and decoded action, whether it was accepted,
the tick's `Event`s, the game outcome, and the server's own default reward
value - enough to build essentially any reward shaping without another
round trip to the server.

## `info`

Every `step()` call returns an `info` dict with:

- `info["accepted"]` / `info["rejected"]`: the `POST /action` response for
  the one action `step()` submitted.
- `info["events"]`: the `Event`s the simulation emitted this tick (battles,
  treaties, regime changes, ...), as `{"kind": ..., "text": ...}`.
- `info["outcome"]`: `{"type": "ongoing" | "victory" | "stalemate", ...}`.
- `info["day"]` / `info["server_reward"]`: the current simulated day and the
  API's own built-in reward value (present even when `reward_fn` overrides
  what `step()` actually returns).

## Tests

```bash
cd python
python -m venv .venv && source .venv/bin/activate
pip install -r requirements-dev.txt   # requirements.txt + pytest
cargo build --workspace --manifest-path ../Cargo.toml   # once, from anywhere

pytest tests/ -v
```

The test suite starts one `archipelago-api` subprocess for the whole
session (`tests/conftest.py`) and creates/discards a fresh session per test
- it never talks to a server you already have running, and it cleans its
subprocess up afterward. It covers: `reset(seed=n)` determinism, a fixed
action sequence's trajectory being reproducible across independent envs, a
random policy completing a full episode without raising, `observation_space`
matching `GET /schema`, invalid actions surfacing in `info["rejected"]`
instead of raising, and a custom `reward_fn` actually being used.
