"""`ArchipelagoEnv`: a Gymnasium-compatible environment that drives one
faction of a Stage 5A `archipelago-api` session over HTTP.

docs/phase5-spec.md "Stage 5B — Gymnasium 環境":

    env = ArchipelagoEnv(seed=1, faction=0, opponents="heuristic")
    obs, info = env.reset(seed=1)
    obs, reward, terminated, truncated, info = env.step(action)

Every faction not equal to `faction` is driven by the server's own built-in
heuristic AI (`crates/agents`) - "1 勢力だけを学習させ、残りは AI に任せる"
is the default the server itself implements (`Session::controlled`); this
class just always asks for exactly one controlled faction.
"""

from __future__ import annotations

import random
from typing import Any, Optional
from urllib.parse import urlencode

import gymnasium as gym
import numpy as np
from gymnasium import spaces

from .client import ApiClient, ArchipelagoConnectionError
from .reward import RewardContext, RewardFn, default_reward
from .schema import DEFAULT_MAX_UNIT_SLOTS, DEFAULT_VALUE_LEVELS, ActionTable, observation_length
from .server import ManagedServer


class ArchipelagoEnv(gym.Env):
    """One controlled faction against `opponents="heuristic"`."""

    metadata = {"render_modes": []}

    def __init__(
        self,
        base_url: Optional[str] = None,
        *,
        seed: Optional[int] = None,
        faction: int = 0,
        opponents: str = "heuristic",
        max_days: Optional[int] = None,
        reward_fn: RewardFn = default_reward,
        start_server: bool = True,
        server_binary: Optional[str] = None,
        max_unit_slots: int = DEFAULT_MAX_UNIT_SLOTS,
        value_levels: tuple = DEFAULT_VALUE_LEVELS,
        idle_timeout_secs: int = 1800,
        request_timeout_secs: float = 30.0,
    ):
        super().__init__()
        if opponents != "heuristic":
            raise ValueError(
                f"opponents={opponents!r} is not supported - the server only ever drives "
                "uncontrolled factions with its built-in heuristic AI (docs/phase5-spec.md "
                "Session.controlled), so 'heuristic' is the only valid value."
            )

        self.faction = faction
        self.default_seed = seed
        self.max_days = max_days
        self.reward_fn = reward_fn

        self._managed_server: Optional[ManagedServer] = None
        if base_url is None:
            if not start_server:
                raise ValueError(
                    "base_url was not given and start_server=False - either pass the URL of an "
                    "already-running `archipelago-api` server, or leave start_server=True (the "
                    "default) so ArchipelagoEnv launches one itself."
                )
            self._managed_server = ManagedServer(idle_timeout_secs=idle_timeout_secs, binary=server_binary)
            base_url = self._managed_server.start()
        self.base_url = base_url
        self.client = ApiClient(base_url, timeout=request_timeout_secs)

        if not start_server or self._managed_server is None:
            # We didn't just watch this server come up ourselves - verify it's
            # reachable now, with the actionable error message, rather than
            # waiting for the first /schema call to raise a less-specific one
            # from deep inside urllib.
            if not self.client.health_check():
                raise ArchipelagoConnectionError(base_url, ConnectionError("GET /health did not return ok"))

        schema = self.client.get("/schema")
        self.schema = schema
        self.action_table = ActionTable(schema, max_unit_slots=max_unit_slots, value_levels=value_levels)
        obs_len = observation_length(schema)

        self.observation_space = spaces.Box(low=-np.inf, high=np.inf, shape=(obs_len,), dtype=np.float32)
        self.action_space = spaces.Discrete(len(self.action_table))

        self.session_id: Optional[str] = None
        self._last_obs: Optional[np.ndarray] = None

    # -- Gymnasium API --------------------------------------------------

    def reset(self, *, seed: Optional[int] = None, options: Optional[dict[str, Any]] = None):
        super().reset(seed=seed)
        self._discard_session()

        effective_seed = seed if seed is not None else self.default_seed
        if effective_seed is None:
            effective_seed = random.SystemRandom().getrandbits(63)

        body: dict[str, Any] = {"seed": int(effective_seed), "controlled": [self.faction]}
        max_days = (options or {}).get("max_days", self.max_days)
        if max_days is not None:
            body["max_days"] = max_days

        response = self.client.post("/reset", body)
        self.session_id = response["session_id"]
        obs = self._extract_observation(response["observations"])
        self._last_obs = obs

        info = {"day": response["day"], "seed": response["seed"], "session_id": self.session_id}
        return obs, info

    def step(self, action: int):
        if self.session_id is None:
            raise RuntimeError("step() called before reset()")

        entry = self.action_table.decode(action)
        accepted: list[Any] = []
        rejected: list[Any] = []
        if entry.payload is not None:
            action_response = self.client.post(
                "/action",
                {"session_id": self.session_id, "faction": self.faction, "actions": [entry.payload]},
            )
            accepted = action_response["accepted"]
            rejected = action_response["rejected"]

        step_response = self.client.post("/step", {"session_id": self.session_id, "steps": 1})
        obs = self._extract_observation(step_response["observations"])
        server_reward = float(step_response["reward"])
        terminated = bool(step_response["terminated"])
        truncated = False

        day_events = step_response["info"]["events"]
        events: list[dict[str, Any]] = day_events[0]["events"] if day_events else []
        outcome = step_response["info"]["outcome"]

        ctx = RewardContext(
            prev_observation=self._last_obs if self._last_obs is not None else obs,
            observation=obs,
            action=int(action),
            decoded_action=entry.payload,
            accepted=entry.payload is not None and not rejected,
            rejected_reason=rejected[0]["reason"] if rejected else None,
            events=events,
            outcome=outcome,
            server_reward=server_reward,
            day=step_response["day"],
        )
        reward = float(self.reward_fn(ctx))

        info = {
            "accepted": accepted,
            "rejected": rejected,
            "events": events,
            "outcome": outcome,
            "day": step_response["day"],
            "server_reward": server_reward,
        }

        self._last_obs = obs
        return obs, reward, terminated, truncated, info

    def close(self):
        self._discard_session()
        if self._managed_server is not None:
            self._managed_server.stop()
            self._managed_server = None

    # -- helpers ----------------------------------------------------------

    def _discard_session(self) -> None:
        if self.session_id is None:
            return
        try:
            self.client.delete("/session", {"session_id": self.session_id})
        except Exception:
            pass
        self.session_id = None

    def _extract_observation(self, observations: dict[str, Any]) -> np.ndarray:
        values = observations[str(self.faction)]
        return np.asarray(values, dtype=np.float32)

    def state(self) -> dict[str, Any]:
        """The full board (`GET /state`), independent of `observation_space`'s
        fixed-length encoding - useful for debugging/logging a trained
        policy, not part of the RL loop itself.
        """
        if self.session_id is None:
            raise RuntimeError("state() called before reset()")
        query = urlencode({"session_id": self.session_id, "faction": self.faction})
        return self.client.get(f"/state?{query}")
