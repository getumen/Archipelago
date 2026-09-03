//! Player/agent-facing commands and the validation that turns them into
//! world mutations. Invalid actions are rejected, never panicked on.

use crate::balance::{
    CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN, IMPORT_PLAN_RATE_MAX, UNIT_EQUIPMENT, UNIT_MANPOWER,
    UNIT_ORG, UNIT_START_ORG_RATIO,
};
use crate::construction::{required_points, Construction, Project};
use crate::good::Good;
use crate::ids::{FactionId, RegionId, UnitId};
use crate::logistics;
use crate::military::{fleet_move_required, move_required, Movement, Unit};
use crate::naval;
use crate::world::{Domain, Station, World};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    /// A land unit's `to` must be `Station::Region`; a fleet's must be
    /// `Station::Sea` — `apply_move` validates the destination matches the
    /// moving unit's own domain and rejects it otherwise
    /// (`ActionError::NotAdjacent`).
    MoveUnit { unit: UnitId, to: Station },
    HoldUnit { unit: UnitId },
    /// Stage 2D (docs/phase2-spec.md "艦隊"): `domain` picks land or sea.
    /// A fleet can only be built in an owned, uncontested region that has a
    /// port (`region.port > 0.0`); it launches into that port's lowest-id
    /// facing sea zone (`naval::home_zone`).
    RecruitUnit { region: RegionId, domain: Domain },
    ReinforceUnit { unit: UnitId },
    SetConscription(f32),
    SetIndustryPriority { good: Good, weight: f32 },
    SetCivilianRation(f32),
    Build { region: RegionId, project: Project },
    CancelBuild { region: RegionId },
    /// Stage 2C sea imports (docs/phase2-spec.md "1. 海上輸入"): set the
    /// desired daily import rate for `good`. Only `Food` and `Energy` are
    /// importable - any other good is rejected with `ActionError::InvalidValue`.
    SetImportPlan { good: Good, rate: f32 },
    /// Stage 2C per-commodity delivery (docs/phase2-spec.md "3. 品目別の
    /// 到達率"): set the priority weight `logistics::distribute_supply` uses
    /// to split contended regional throughput between Munitions and Arms
    /// delivery for `good`.
    SetLogisticsPriority { good: Good, weight: f32 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionError {
    NotOwner,
    UnitDead,
    NotAdjacent,
    Pinned,
    RegionNotOwned,
    RegionContested,
    InsufficientManpower,
    InsufficientEquipment,
    InvalidValue,
    /// `Action::Build` on a region that already has a project in progress.
    AlreadyBuilding,
    /// `Action::CancelBuild` on a region with no project in progress.
    NoConstruction,
    /// Stage 2D: `Action::RecruitUnit { domain: Domain::Sea, .. }` against a
    /// region with no port (or, in principle, no facing sea zone at all).
    NoPort,
}

pub fn apply_action(
    world: &mut World,
    faction: FactionId,
    action: Action,
) -> Result<(), ActionError> {
    match action {
        Action::MoveUnit { unit, to } => apply_move(world, faction, unit, to),
        Action::HoldUnit { unit } => apply_hold(world, faction, unit),
        Action::RecruitUnit { region, domain } => apply_recruit(world, faction, region, domain),
        Action::ReinforceUnit { unit } => apply_reinforce(world, faction, unit),
        Action::SetConscription(value) => apply_set_conscription(world, faction, value),
        Action::SetIndustryPriority { good, weight } => {
            apply_set_industry_priority(world, faction, good, weight)
        }
        Action::SetCivilianRation(value) => apply_set_civilian_ration(world, faction, value),
        Action::Build { region, project } => apply_build(world, faction, region, project),
        Action::CancelBuild { region } => apply_cancel_build(world, faction, region),
        Action::SetImportPlan { good, rate } => apply_set_import_plan(world, faction, good, rate),
        Action::SetLogisticsPriority { good, weight } => {
            apply_set_logistics_priority(world, faction, good, weight)
        }
    }
}

fn owned_unit<'a>(
    world: &'a World,
    faction: FactionId,
    unit: UnitId,
) -> Result<&'a Unit, ActionError> {
    let unit = world.units.get(unit.index()).ok_or(ActionError::UnitDead)?;
    if !unit.alive {
        return Err(ActionError::UnitDead);
    }
    if unit.owner != faction {
        return Err(ActionError::NotOwner);
    }
    Ok(unit)
}

fn apply_move(
    world: &mut World,
    faction: FactionId,
    unit_id: UnitId,
    to: Station,
) -> Result<(), ActionError> {
    let unit = owned_unit(world, faction, unit_id)?;
    let from = unit.station;

    let pinned = match from {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    };
    if pinned {
        return Err(ActionError::Pinned);
    }

    let (required, strait_zone) = match (from, to) {
        (Station::Region(from_r), Station::Region(to_r)) => {
            let link = world.link_between(from_r, to_r).ok_or(ActionError::NotAdjacent)?;
            let dest = world.region(to_r);
            let hostile = dest.owner != faction;
            let required = move_required(link.kind, dest.terrain, hostile);
            // External code review fix (Stage 2D): a Strait link's crossing
            // time is throttled the same way its supply throughput is — by
            // the highest sea control any other faction holds in the zone
            // it passes through — but sea control is recomputed every tick,
            // so that factor must NOT be sampled once and baked into
            // `required` here (docs/phase2-spec.md "1. 海峡リンクの遮断":
            // "移動もこの係数で遅くなる" means the crossing tracks *current*
            // control throughout, not the control at the moment it was
            // ordered). `required` stays the control-independent travel
            // cost; `tick_movement` applies the live factor to progress
            // every tick via `strait_zone`.
            (required, link.strait_zone)
        }
        (Station::Sea(from_z), Station::Sea(to_z)) => {
            if !world.sea_zone(from_z).adjacent.contains(&to_z) {
                return Err(ActionError::NotAdjacent);
            }
            let enemy_control = world.sea_zone(to_z).enemy_control_max(faction);
            let hostile = enemy_control > world.sea_zone(to_z).control[faction.index()];
            (fleet_move_required(hostile), None)
        }
        // A land unit can never be ordered into a sea zone, nor a fleet
        // into a region — `fleet_cannot_enter_land` and its converse are
        // exactly this branch.
        _ => return Err(ActionError::NotAdjacent),
    };

    world.unit_mut(unit_id).movement = Some(Movement {
        from,
        to,
        progress: 0.0,
        required,
        retreat: false,
        strait_zone,
    });
    Ok(())
}

fn apply_hold(world: &mut World, faction: FactionId, unit_id: UnitId) -> Result<(), ActionError> {
    owned_unit(world, faction, unit_id)?;
    world.unit_mut(unit_id).movement = None;
    Ok(())
}

fn apply_recruit(
    world: &mut World,
    faction: FactionId,
    region_id: RegionId,
    domain: Domain,
) -> Result<(), ActionError> {
    let region = world
        .regions
        .get(region_id.index())
        .ok_or(ActionError::RegionNotOwned)?;
    if region.owner != faction {
        return Err(ActionError::RegionNotOwned);
    }
    if world.has_enemy_units(region_id, faction) {
        return Err(ActionError::RegionContested);
    }

    // Stage 2D (docs/phase2-spec.md "艦隊": "艦隊は港のある自領地域でのみ建造
    // できる"): a fleet needs a port to launch from; a land unit doesn't
    // care about `region.port` at all.
    let station = match domain {
        Domain::Land => Station::Region(region_id),
        Domain::Sea => {
            if region.port <= 0.0 {
                return Err(ActionError::NoPort);
            }
            let zone = naval::home_zone(world, region_id).ok_or(ActionError::NoPort)?;
            Station::Sea(zone)
        }
    };

    let f = world.faction(faction);
    if f.manpower < UNIT_MANPOWER {
        return Err(ActionError::InsufficientManpower);
    }
    if f.stock[Good::Arms.index()] < UNIT_EQUIPMENT {
        return Err(ActionError::InsufficientEquipment);
    }

    world.faction_mut(faction).manpower -= UNIT_MANPOWER;
    world.faction_mut(faction).stock[Good::Arms.index()] -= UNIT_EQUIPMENT;

    let id = UnitId(world.units.len() as u32);
    let kind = match domain {
        Domain::Land => "Corps",
        Domain::Sea => "Fleet",
    };
    let name = format!("{} {} {}", world.faction(faction).name, kind, id.0);
    world.units.push(Unit {
        id,
        owner: faction,
        name,
        station,
        movement: None,
        manpower: UNIT_MANPOWER,
        equipment: UNIT_EQUIPMENT,
        organization: UNIT_ORG * UNIT_START_ORG_RATIO,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: station,
        experience: 0.0,
        alive: true,
    });
    Ok(())
}

fn apply_reinforce(
    world: &mut World,
    faction: FactionId,
    unit_id: UnitId,
) -> Result<(), ActionError> {
    let unit = owned_unit(world, faction, unit_id)?;
    let pinned = match unit.station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    };
    if pinned {
        return Err(ActionError::RegionContested);
    }

    // External code review fix (Stage 2C; Stage 2D extends it to fleets):
    // `arms_delivery`/`arms_budget` are stamped by
    // `logistics::distribute_supply`, which runs once a tick *before*
    // movement. If this unit has moved since that stamp
    // (`arms_delivery_station != station`), the cached numbers describe a
    // place it has already left - trust them and a unit could finish
    // marching (or sailing) out of a well-supplied place into a cut-off one
    // and still reinforce at the old, high ratio. Recompute fresh for the
    // *current* station on the spot instead of trying to invalidate/track
    // the cache from `military::tick_movement` (deriving on demand here is
    // the simpler thing to reason about: one call site, no extra
    // bookkeeping needed anywhere movement happens), then stamp the
    // refreshed numbers back onto the unit so a second `ReinforceUnit`
    // against it later in this same batch sees the already-fresh,
    // already-being-spent budget rather than recomputing - and
    // re-granting - it again.
    if unit.arms_delivery_station != unit.station {
        let (ratio, budget) = match unit.station.domain() {
            Domain::Land => logistics::instantaneous_arms_delivery(world, unit_id),
            Domain::Sea => naval::instantaneous_fleet_arms_delivery(world, unit_id),
        };
        let unit = world.unit_mut(unit_id);
        unit.arms_delivery = ratio;
        unit.arms_budget = budget;
        unit.arms_delivery_station = unit.station;
    }

    let unit = world.unit(unit_id);
    let need_manpower = (UNIT_MANPOWER - unit.manpower).max(0.0);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
    // External code review fix (Stage 2C): `arms_budget` is a real
    // allowance that gets spent down below, not a ratio re-applied to
    // whatever gap remains - the pre-fix `need_equipment * arms_delivery`
    // let repeated `ReinforceUnit` actions in one batch compound past a
    // single tick's delivery allowance (each call recomputed the ratio
    // against the now-smaller remaining gap instead of a shrinking budget).
    let deliverable_equipment = need_equipment.min(unit.arms_budget.max(0.0));

    let f = world.faction(faction);
    let fill_manpower = need_manpower.min(f.manpower);
    let fill_equipment = deliverable_equipment.min(f.stock[Good::Arms.index()]);

    world.faction_mut(faction).manpower -= fill_manpower;
    world.faction_mut(faction).stock[Good::Arms.index()] -= fill_equipment;
    let unit = world.unit_mut(unit_id);
    unit.manpower += fill_manpower;
    unit.equipment += fill_equipment;
    unit.arms_budget = (unit.arms_budget - fill_equipment).max(0.0);
    Ok(())
}

fn apply_set_conscription(
    world: &mut World,
    faction: FactionId,
    value: f32,
) -> Result<(), ActionError> {
    if !(0.0..=1.0).contains(&value) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).conscription = value;
    Ok(())
}

fn apply_set_industry_priority(
    world: &mut World,
    faction: FactionId,
    good: Good,
    weight: f32,
) -> Result<(), ActionError> {
    if !(0.0..=1.0).contains(&weight) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).industry_priority[good.index()] = weight;
    Ok(())
}

fn apply_set_civilian_ration(
    world: &mut World,
    faction: FactionId,
    value: f32,
) -> Result<(), ActionError> {
    if !(CIVILIAN_RATION_MIN..=CIVILIAN_RATION_MAX).contains(&value) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).civilian_ration = value;
    Ok(())
}

/// `Action::SetImportPlan` (docs/phase2-spec.md "1. 海上輸入"): only `Food`
/// and `Energy` are importable - any other good is rejected outright. A
/// valid good's `rate` is clamped to `0.0..=IMPORT_PLAN_RATE_MAX` rather than
/// rejected, per the spec's "rate は 0 以上、上限でクランプ".
fn apply_set_import_plan(
    world: &mut World,
    faction: FactionId,
    good: Good,
    rate: f32,
) -> Result<(), ActionError> {
    if good != Good::Food && good != Good::Energy {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).import_plan[good.index()] = rate.clamp(0.0, IMPORT_PLAN_RATE_MAX);
    Ok(())
}

/// `Action::SetLogisticsPriority` (docs/phase2-spec.md "3. 品目別の到達率"):
/// same validation shape as `apply_set_industry_priority` - only the
/// `Munitions`/`Arms` weights are ever read by `logistics::distribute_supply`.
fn apply_set_logistics_priority(
    world: &mut World,
    faction: FactionId,
    good: Good,
    weight: f32,
) -> Result<(), ActionError> {
    if !(0.0..=1.0).contains(&weight) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).logistics_priority[good.index()] = weight;
    Ok(())
}

/// `Action::Build` (docs/phase2-spec.md Stage 2B): own region, not
/// contested, no project already running.
fn apply_build(
    world: &mut World,
    faction: FactionId,
    region_id: RegionId,
    project: Project,
) -> Result<(), ActionError> {
    let region = world
        .regions
        .get(region_id.index())
        .ok_or(ActionError::RegionNotOwned)?;
    if region.owner != faction {
        return Err(ActionError::RegionNotOwned);
    }
    if world.has_enemy_units(region_id, faction) {
        return Err(ActionError::RegionContested);
    }
    if region.construction.is_some() {
        return Err(ActionError::AlreadyBuilding);
    }

    world.region_mut(region_id).construction = Some(Construction {
        project,
        invested: 0.0,
        required: required_points(project),
    });
    Ok(())
}

/// `Action::CancelBuild` (docs/phase2-spec.md Stage 2B): resources already
/// invested are forfeited — the `Construction` is simply discarded, not
/// refunded.
fn apply_cancel_build(
    world: &mut World,
    faction: FactionId,
    region_id: RegionId,
) -> Result<(), ActionError> {
    let region = world
        .regions
        .get(region_id.index())
        .ok_or(ActionError::RegionNotOwned)?;
    if region.owner != faction {
        return Err(ActionError::RegionNotOwned);
    }
    if region.construction.is_none() {
        return Err(ActionError::NoConstruction);
    }

    world.region_mut(region_id).construction = None;
    Ok(())
}
