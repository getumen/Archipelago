"""Minimal usage example: run one episode with a uniformly random policy.

    cd python
    python -m venv .venv && source .venv/bin/activate
    pip install -r requirements.txt
    cargo build --workspace --manifest-path ../Cargo.toml   # once, from repo root
    python examples/random_policy.py

`ArchipelagoEnv` finds and launches the `archipelago-api` binary itself
(`start_server=True`, the default) - no server needs to be running first.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from env import ArchipelagoEnv


def main() -> None:
    env = ArchipelagoEnv(seed=1, faction=0, opponents="heuristic", max_days=60)
    try:
        obs, info = env.reset(seed=1)
        print(f"reset: day={info['day']} observation_space={env.observation_space.shape} "
              f"action_space={env.action_space}")

        terminated = truncated = False
        total_reward = 0.0
        steps = 0
        while not (terminated or truncated):
            action = env.action_space.sample()
            obs, reward, terminated, truncated, info = env.step(action)
            total_reward += reward
            steps += 1
            if info["rejected"]:
                print(f"day {info['day']}: action rejected ({info['rejected'][0]['reason']})")
            if info["events"]:
                for event in info["events"]:
                    print(f"day {info['day']}: {event['kind']} - {event['text']}")

        print(f"episode finished after {steps} steps, total_reward={total_reward:.3f}")
        print(f"outcome: {info['outcome']}")
    finally:
        env.close()


if __name__ == "__main__":
    main()
