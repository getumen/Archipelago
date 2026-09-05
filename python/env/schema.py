"""Turns `GET /schema` into the two things `ArchipelagoEnv` needs: the
observation vector's length, and a flattened `Discrete` action table.

Nothing in this file hardcodes a vocabulary, an enum, or a count that
`GET /schema` reports (docs/phase5-spec.md "Stage 5B": "Rust 側の長さ定数
を API から取得し、Python 側でハードコードしない") - every good/treaty/
focus/domain/project name, and every region/sea-zone/faction count, is read
out of the schema response fetched at construction time. The only numbers
this file *does* choose on the Python side are `MAX_UNIT_SLOTS` and
`VALUE_LEVELS` just below, and neither is something the schema could supply
in the first place: unit ids are a dynamic, growing set (new units appear
mid-episode via `recruit_unit`), and `set_conscription`/`weight`/`rate`
fields are continuous - a `Discrete` action space needs a *fixed* size, so
turning "any unit" and "any real number" into a finite table requires a
client-side choice no server schema could remove.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable, Optional

# How many unit "slots" the flattened action space reserves for
# move/hold/reinforce actions. Slot `i` is interpreted literally as
# `UnitId(i)`: since `RecruitUnit` assigns ids sequentially starting at 0,
# this covers every unit that can exist as long as the controlled faction's
# side of the game never recruits more than this many units across one
# episode. A unit id at or beyond this bound simply has no action that can
# address it - not a crash, just an address space limit; raise it by
# constructing `ArchipelagoEnv(max_unit_slots=...)` if a longer episode or a
# more aggressive recruiting policy needs more room.
DEFAULT_MAX_UNIT_SLOTS = 48

# Discretization levels used for every continuous `Action` field (weights,
# rates, the conscription/ration fraction). Evenly spaced across [0, 1],
# which covers every one of this schema's continuous fields as they're
# documented in crates/sim/src/action.rs.
DEFAULT_VALUE_LEVELS: tuple[float, ...] = (0.0, 0.25, 0.5, 0.75, 1.0)


@dataclass(frozen=True)
class ActionEntry:
    """One flattened `Discrete` action. `payload` is `None` for the no-op;
    otherwise it's the exact JSON object `POST /action`'s `actions[]` expects
    (see `crates/api/src/action_codec.rs::action_from_value`). `label` is a
    short human-readable description, useful for logging/debugging a
    trained policy's action choices. `layer` is the decision layer
    (`archipelago_sim::action::Layer`'s key - "military", "economy",
    "grand_strategy" or "diplomacy") this action's `payload["type"]` belongs
    to per `GET /schema`'s `actions[].layer`, or `None` for the no-op, which
    belongs to every layer equally rather than one in particular.
    """

    label: str
    payload: Optional[dict[str, Any]]
    layer: Optional[str] = None


class ActionTable:
    """The flattened action vocabulary described in docs/phase5-spec.md
    Stage 5B: "部隊移動 / 徴募 / 補充 / 建設 / 政策変更 / 条約提案 / 何もし
    ない" as one `Discrete(len(table))`.

    Every entry also carries which `Layer` (`archipelago_sim::action::Layer`,
    exposed as `schema["enums"]["layer"]`/`schema["actions"][i]["layer"]`) its
    action type belongs to - `layers`/`indices_for_layer` let a caller build a
    *per-layer* policy (e.g. a military-only RL agent) that only ever sees
    the slice of this table its layer actually needs, instead of the full
    flattened space.
    """

    def __init__(self, schema: dict[str, Any], max_unit_slots: int = DEFAULT_MAX_UNIT_SLOTS,
                 value_levels: tuple[float, ...] = DEFAULT_VALUE_LEVELS):
        self.schema = schema
        self.max_unit_slots = max_unit_slots
        self.value_levels = value_levels

        enums = schema["enums"]
        self.goods: list[str] = enums["good"]
        self.treaties: list[str] = enums["treaty"]
        self.foci: list[str] = enums["focus"]
        self.domains: list[str] = enums["domain"]
        self.layers: list[str] = enums["layer"]

        scenario = schema["scenario"]
        self.region_count: int = scenario["region_count"]
        self.sea_zone_count: int = scenario["sea_zone_count"]
        self.faction_count: int = scenario["faction_count"]

        # Read off the same per-type `layer` the server's `/schema` already
        # reports for the actions this table's payloads use (`crates/api/src
        # /action_codec.rs::actions_schema`), rather than re-declaring the
        # military/economy/grand_strategy/diplomacy split a second time on
        # the Python side - a type this table doesn't otherwise reference
        # (e.g. one only `Layer::Diplomacy`'s `declare_war`/`break_treaty`
        # ever needs) simply never gets looked up.
        type_to_layer: dict[str, str] = {entry["type"]: entry["layer"] for entry in schema["actions"]}

        self.entries: list[ActionEntry] = [
            ActionEntry(label, payload, None if payload is None else type_to_layer[payload["type"]])
            for label, payload in self._build()
        ]

    # -- table construction -------------------------------------------------

    def _stations(self):
        for region in range(self.region_count):
            yield {"kind": "region", "id": region}
        for zone in range(self.sea_zone_count):
            yield {"kind": "sea", "id": zone}

    def _projects(self):
        for simple in ("infrastructure", "port", "repair"):
            yield simple, {"type": "build", "project": simple}
        for good in self.goods:
            yield f"capacity:{good}", {"type": "build", "project": {"capacity": good}}

    def _build(self):
        yield "no_op", None

        for unit in range(self.max_unit_slots):
            yield f"hold_unit(unit={unit})", {"type": "hold_unit", "unit": unit}

        for unit in range(self.max_unit_slots):
            for station in self._stations():
                yield (
                    f"move_unit(unit={unit}, to={station['kind']}:{station['id']})",
                    {"type": "move_unit", "unit": unit, "to": station},
                )

        for unit in range(self.max_unit_slots):
            yield f"reinforce_unit(unit={unit})", {"type": "reinforce_unit", "unit": unit}

        for unit in range(self.max_unit_slots):
            yield f"disband_unit(unit={unit})", {"type": "disband_unit", "unit": unit}

        for region in range(self.region_count):
            for domain in self.domains:
                yield (
                    f"recruit_unit(region={region}, domain={domain})",
                    {"type": "recruit_unit", "region": region, "domain": domain},
                )

        for region in range(self.region_count):
            for label, payload in self._projects():
                payload = dict(payload, region=region)
                yield f"build(region={region}, project={label})", payload
            yield f"cancel_build(region={region})", {"type": "cancel_build", "region": region}

        for level in self.value_levels:
            yield f"set_conscription({level})", {"type": "set_conscription", "value": level}
        for level in self.value_levels:
            yield f"set_civilian_ration({level})", {"type": "set_civilian_ration", "value": level}
        for good in self.goods:
            for level in self.value_levels:
                yield (
                    f"set_industry_priority(good={good}, weight={level})",
                    {"type": "set_industry_priority", "good": good, "weight": level},
                )
        for good in self.goods:
            for level in self.value_levels:
                yield (
                    f"set_import_plan(good={good}, rate={level})",
                    {"type": "set_import_plan", "good": good, "rate": level},
                )
        for good in self.goods:
            for level in self.value_levels:
                yield (
                    f"set_logistics_priority(good={good}, weight={level})",
                    {"type": "set_logistics_priority", "good": good, "weight": level},
                )
        for focus in self.foci:
            yield f"set_national_focus({focus})", {"type": "set_national_focus", "focus": focus}

        for to in range(self.faction_count):
            for treaty in self.treaties:
                yield (
                    f"propose_treaty(to={to}, treaty={treaty})",
                    {"type": "propose_treaty", "to": to, "treaty": treaty},
                )
        for frm in range(self.faction_count):
            for treaty in self.treaties:
                yield (
                    f"accept_treaty(from={frm}, treaty={treaty})",
                    {"type": "accept_treaty", "from": frm, "treaty": treaty},
                )
        for frm in range(self.faction_count):
            for treaty in self.treaties:
                yield (
                    f"reject_treaty(from={frm}, treaty={treaty})",
                    {"type": "reject_treaty", "from": frm, "treaty": treaty},
                )

    def __len__(self) -> int:
        return len(self.entries)

    # -- per-layer views ------------------------------------------------

    def indices_for_layer(self, layer: str, include_no_op: bool = True) -> list[int]:
        """Every index in this table whose action belongs to `layer`
        (one of `self.layers`), for building a per-layer `Discrete` a
        single-layer policy actually needs instead of the full table - the
        Python-side counterpart of `archipelago_agents::CompositeAgent`
        routing a `Layer` to one `Agent` on the Rust side. `no_op` belongs to
        no one layer in particular (see `ActionEntry.layer`'s doc) but is
        included by default since every policy, single-layer or not, needs
        a legal way to do nothing on a tick it has no order to give.

        Raises `ValueError` if `layer` isn't one of `self.layers` - a typo
        here should fail loudly, not silently return an empty table that
        looks like "this layer has no actions".
        """
        if layer not in self.layers:
            raise ValueError(f"unknown layer {layer!r}, expected one of {self.layers}")
        return [
            i for i, entry in enumerate(self.entries)
            if entry.layer == layer or (include_no_op and entry.layer is None)
        ]

    def decode(self, action: int) -> ActionEntry:
        if not isinstance(action, (int,)) or isinstance(action, bool):
            # numpy integer types (e.g. from `Discrete.sample()`) are fine -
            # only reject genuinely non-integer input.
            try:
                action = int(action)
            except (TypeError, ValueError) as e:
                raise ValueError(f"action must be an integer index, got {action!r}") from e
        if not (0 <= action < len(self.entries)):
            raise ValueError(f"action index {action} out of range [0, {len(self.entries)})")
        return self.entries[action]


def observation_length(schema: dict[str, Any]) -> int:
    return int(schema["observation"]["length"])
