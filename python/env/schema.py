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
    trained policy's action choices.
    """

    label: str
    payload: Optional[dict[str, Any]]


class ActionTable:
    """The flattened action vocabulary described in docs/phase5-spec.md
    Stage 5B: "部隊移動 / 徴募 / 補充 / 建設 / 政策変更 / 条約提案 / 何もし
    ない" as one `Discrete(len(table))`.
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

        scenario = schema["scenario"]
        self.region_count: int = scenario["region_count"]
        self.sea_zone_count: int = scenario["sea_zone_count"]
        self.faction_count: int = scenario["faction_count"]

        self.entries: list[ActionEntry] = list(self._build())

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
        yield ActionEntry("no_op", None)

        for unit in range(self.max_unit_slots):
            yield ActionEntry(f"hold_unit(unit={unit})", {"type": "hold_unit", "unit": unit})

        for unit in range(self.max_unit_slots):
            for station in self._stations():
                yield ActionEntry(
                    f"move_unit(unit={unit}, to={station['kind']}:{station['id']})",
                    {"type": "move_unit", "unit": unit, "to": station},
                )

        for unit in range(self.max_unit_slots):
            yield ActionEntry(f"reinforce_unit(unit={unit})", {"type": "reinforce_unit", "unit": unit})

        for region in range(self.region_count):
            for domain in self.domains:
                yield ActionEntry(
                    f"recruit_unit(region={region}, domain={domain})",
                    {"type": "recruit_unit", "region": region, "domain": domain},
                )

        for region in range(self.region_count):
            for label, payload in self._projects():
                payload = dict(payload, region=region)
                yield ActionEntry(f"build(region={region}, project={label})", payload)
            yield ActionEntry(f"cancel_build(region={region})", {"type": "cancel_build", "region": region})

        for level in self.value_levels:
            yield ActionEntry(f"set_conscription({level})", {"type": "set_conscription", "value": level})
        for level in self.value_levels:
            yield ActionEntry(f"set_civilian_ration({level})", {"type": "set_civilian_ration", "value": level})
        for good in self.goods:
            for level in self.value_levels:
                yield ActionEntry(
                    f"set_industry_priority(good={good}, weight={level})",
                    {"type": "set_industry_priority", "good": good, "weight": level},
                )
        for good in self.goods:
            for level in self.value_levels:
                yield ActionEntry(
                    f"set_import_plan(good={good}, rate={level})",
                    {"type": "set_import_plan", "good": good, "rate": level},
                )
        for good in self.goods:
            for level in self.value_levels:
                yield ActionEntry(
                    f"set_logistics_priority(good={good}, weight={level})",
                    {"type": "set_logistics_priority", "good": good, "weight": level},
                )
        for focus in self.foci:
            yield ActionEntry(f"set_national_focus({focus})", {"type": "set_national_focus", "focus": focus})

        for to in range(self.faction_count):
            for treaty in self.treaties:
                yield ActionEntry(
                    f"propose_treaty(to={to}, treaty={treaty})",
                    {"type": "propose_treaty", "to": to, "treaty": treaty},
                )
        for frm in range(self.faction_count):
            for treaty in self.treaties:
                yield ActionEntry(
                    f"accept_treaty(from={frm}, treaty={treaty})",
                    {"type": "accept_treaty", "from": frm, "treaty": treaty},
                )
        for frm in range(self.faction_count):
            for treaty in self.treaties:
                yield ActionEntry(
                    f"reject_treaty(from={frm}, treaty={treaty})",
                    {"type": "reject_treaty", "from": frm, "treaty": treaty},
                )

    def __len__(self) -> int:
        return len(self.entries)

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
