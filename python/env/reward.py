"""The swappable reward function (docs/phase5-spec.md "Stage 5B" ->
"報酬関数は差し替え可能にする。研究用途で最も触りたい部分であるため").

`ArchipelagoEnv` never computes a reward formula itself - it only builds a
`RewardContext` each step and calls whatever `reward_fn` was passed to its
constructor (default: `default_reward`, below). Swap it by passing a
different callable; nothing else about the env needs to change.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable

import numpy as np


@dataclass(frozen=True)
class RewardContext:
    """Everything a reward function could plausibly need, gathered from one
    `step()` call. `observation`/`prev_observation` are the same fixed-length
    vectors `ArchipelagoEnv.step` returns, so a custom reward function can
    read out particular fields (see `python/README.md` for the segment
    layout, itself read from `GET /schema` rather than hardcoded).
    """

    prev_observation: np.ndarray
    observation: np.ndarray
    action: int
    decoded_action: dict[str, Any] | None
    accepted: bool
    rejected_reason: str | None
    events: list[dict[str, Any]]
    outcome: dict[str, Any]
    server_reward: float
    day: int


RewardFn = Callable[[RewardContext], float]


def default_reward(ctx: RewardContext) -> float:
    """Uses the API's own built-in default (`crates/api/src/reward.rs`):
    a weighted day-over-day change in region count, industrial capacity and
    manpower for the controlled faction. Reasonable out of the box, but only
    one of many things a researcher might want to optimize for - that's the
    entire point of `RewardFn` being swappable.
    """
    return ctx.server_reward
