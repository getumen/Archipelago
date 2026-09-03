//! Player/agent-facing commands and the validation that turns them into
//! world mutations. Invalid actions are rejected, never panicked on.

use crate::balance::{
    CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN, UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG,
    UNIT_START_ORG_RATIO,
};
use crate::construction::{required_points, Construction, Project};
use crate::good::Good;
use crate::ids::{FactionId, RegionId, UnitId};
use crate::military::{move_required, Movement, Unit};
use crate::world::World;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    MoveUnit { unit: UnitId, to: RegionId },
    HoldUnit { unit: UnitId },
    RecruitUnit { region: RegionId },
    ReinforceUnit { unit: UnitId },
    SetConscription(f32),
    SetIndustryPriority { good: Good, weight: f32 },
    SetCivilianRation(f32),
    Build { region: RegionId, project: Project },
    CancelBuild { region: RegionId },
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
}

pub fn apply_action(
    world: &mut World,
    faction: FactionId,
    action: Action,
) -> Result<(), ActionError> {
    match action {
        Action::MoveUnit { unit, to } => apply_move(world, faction, unit, to),
        Action::HoldUnit { unit } => apply_hold(world, faction, unit),
        Action::RecruitUnit { region } => apply_recruit(world, faction, region),
        Action::ReinforceUnit { unit } => apply_reinforce(world, faction, unit),
        Action::SetConscription(value) => apply_set_conscription(world, faction, value),
        Action::SetIndustryPriority { good, weight } => {
            apply_set_industry_priority(world, faction, good, weight)
        }
        Action::SetCivilianRation(value) => apply_set_civilian_ration(world, faction, value),
        Action::Build { region, project } => apply_build(world, faction, region, project),
        Action::CancelBuild { region } => apply_cancel_build(world, faction, region),
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
    to: RegionId,
) -> Result<(), ActionError> {
    let unit = owned_unit(world, faction, unit_id)?;
    let from = unit.location;
    if world.has_enemy_units(from, faction) {
        return Err(ActionError::Pinned);
    }
    let link = world.link_between(from, to).ok_or(ActionError::NotAdjacent)?;
    let dest = world.region(to);
    let hostile = dest.owner != faction;
    let required = move_required(link.kind, dest.terrain, hostile);

    world.unit_mut(unit_id).movement = Some(Movement {
        from,
        to,
        progress: 0.0,
        required,
        retreat: false,
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
    let name = format!("{} Corps {}", world.faction(faction).name, id.0);
    world.units.push(Unit {
        id,
        owner: faction,
        name,
        location: region_id,
        movement: None,
        manpower: UNIT_MANPOWER,
        equipment: UNIT_EQUIPMENT,
        organization: UNIT_ORG * UNIT_START_ORG_RATIO,
        morale: 1.0,
        supply: 1.0,
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
    if world.has_enemy_units(unit.location, faction) {
        return Err(ActionError::RegionContested);
    }
    let need_manpower = (UNIT_MANPOWER - unit.manpower).max(0.0);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);

    let f = world.faction(faction);
    let fill_manpower = need_manpower.min(f.manpower);
    let fill_equipment = need_equipment.min(f.stock[Good::Arms.index()]);

    world.faction_mut(faction).manpower -= fill_manpower;
    world.faction_mut(faction).stock[Good::Arms.index()] -= fill_equipment;
    let unit = world.unit_mut(unit_id);
    unit.manpower += fill_manpower;
    unit.equipment += fill_equipment;
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
