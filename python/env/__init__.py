"""Gymnasium-compatible environment for the Archipelago Stage 5A API
(docs/phase5-spec.md "Stage 5B — Gymnasium 環境").

    from env import ArchipelagoEnv

See python/README.md for the minimal usage loop.
"""

from .archipelago_env import ArchipelagoEnv
from .reward import RewardContext, RewardFn, default_reward
from .schema import ActionEntry, ActionTable

__all__ = [
    "ArchipelagoEnv",
    "RewardContext",
    "RewardFn",
    "default_reward",
    "ActionEntry",
    "ActionTable",
]
