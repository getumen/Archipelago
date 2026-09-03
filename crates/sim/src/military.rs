//! Units, their movement across the map, and the combat/recovery/occupation
//! systems that resolve what happens to them each tick.

use crate::balance::{
    ATTRITION_MANPOWER, ATTRITION_ORG, ATTRITION_SUPPLY_THRESHOLD, BROKEN_LOSS_MULT,
    CAPTURE_UNREST, COMBAT_DAMAGE, DEVASTATION_ON_CAPTURE, DEVASTATION_PER_COMBAT_DAMAGE,
    EQUIPMENT_LOSS_PER_DAMAGE, MANPOWER_LOSS_PER_DAMAGE, MORALE_REGEN, OCCUPATION_DECAY,
    OCCUPATION_RATE, ORG_DAMAGE_MULT, ORG_MARCH_DRAIN, ORG_REGEN, UNIT_DEATH_MANPOWER,
    UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG, WAR_SUPPORT_CAPTURE_GAIN, WAR_SUPPORT_LOSS_PENALTY,
};
use crate::event::Event;
use crate::ids::{FactionId, RegionId, UnitId};
use crate::rng::Rng;
use crate::world::{Region, Terrain, World};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Movement {
    pub from: RegionId,
    pub to: RegionId,
    pub progress: f32,
    pub required: f32,
    pub retreat: bool,
}

#[derive(Clone, Debug)]
pub struct Unit {
    pub id: UnitId,
    pub owner: FactionId,
    pub name: String,
    pub location: RegionId,
    pub movement: Option<Movement>,
    pub manpower: f32,
    pub equipment: f32,
    pub organization: f32,
    pub morale: f32,
    pub supply: f32,
    pub experience: f32,
    pub alive: bool,
}

impl Unit {
    pub fn manpower_ratio(&self) -> f32 {
        self.manpower / UNIT_MANPOWER
    }

    pub fn equipment_ratio(&self) -> f32 {
        (self.equipment / UNIT_EQUIPMENT).min(1.0)
    }

    pub fn org_ratio(&self) -> f32 {
        self.organization / UNIT_ORG
    }

    /// Blended manpower/equipment fullness, used by AI to judge if a unit
    /// needs reinforcing.
    pub fn strength(&self) -> f32 {
        0.5 * self.manpower_ratio() + 0.5 * self.equipment_ratio()
    }

    pub fn combat_power(&self) -> f32 {
        self.manpower_ratio()
            * (0.35 + 0.65 * self.equipment_ratio())
            * (0.25 + 0.75 * self.org_ratio())
            * (0.50 + 0.50 * self.morale)
            * (0.35 + 0.65 * self.supply)
            * (1.00 + 0.35 * self.experience)
    }
}

/// Days required to traverse `link` into `dest`, `hostile` being true when
/// the destination is not friendly-owned (slows movement down).
pub fn move_required(link_kind: crate::world::LinkKind, dest_terrain: Terrain, hostile: bool) -> f32 {
    link_kind.travel_days() * dest_terrain.move_cost() * if hostile { 1.5 } else { 1.0 }
}

/// Advances in-progress movement, resolving arrivals. Units whose current
/// region holds enemy forces are pinned and make no progress unless retreating.
pub fn tick_movement(world: &mut World) {
    struct Progress {
        id: UnitId,
        arrived: bool,
        new_progress: f32,
    }

    let mut updates = Vec::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let Some(mv) = unit.movement else { continue };
        let pinned = !mv.retreat && world.has_enemy_units(unit.location, unit.owner);
        if pinned {
            continue;
        }
        let step = 0.5 + 0.5 * unit.supply;
        let new_progress = mv.progress + step;
        updates.push(Progress {
            id: unit.id,
            arrived: new_progress >= mv.required,
            new_progress,
        });
    }

    for update in updates {
        let unit = world.unit_mut(update.id);
        unit.organization = (unit.organization - ORG_MARCH_DRAIN).max(0.0);
        if update.arrived {
            let to = unit.movement.unwrap().to;
            unit.location = to;
            unit.movement = None;
        } else {
            unit.movement.as_mut().unwrap().progress = update.new_progress;
        }
    }
}

pub struct CombatReport {
    /// Indexed like `World::units`: true for units that fought this tick.
    pub fought: Vec<bool>,
    /// Indexed like `World::factions`: manpower lost this tick.
    pub casualties: Vec<f32>,
}

/// Resolves combat in every region held by two or more surviving factions.
pub fn tick_combat(world: &mut World, rng: &mut Rng, events: &mut Vec<Event>) -> CombatReport {
    let mut fought = vec![false; world.units.len()];
    let mut casualties = vec![0.0; world.factions.len()];

    for region_idx in 0..world.regions.len() {
        let region_id = RegionId(region_idx as u32);
        let mut factions_present: Vec<FactionId> = world
            .units_in(region_id)
            .map(|u| u.owner)
            .collect::<Vec<_>>();
        factions_present.sort_by_key(|f| f.0);
        factions_present.dedup();
        if factions_present.len() < 2 {
            continue;
        }

        let region = world.region(region_id);
        let defender = pick_defender(world, region, &factions_present);
        let defense_bonus = region.terrain.defense_bonus();

        // Effective power per side, in the same order as `factions_present`.
        let power: Vec<f32> = factions_present
            .iter()
            .map(|&f| {
                let base = world.region_power(region_id, f);
                if f == defender {
                    base * defense_bonus
                } else {
                    base
                }
            })
            .collect();
        let total_power: f32 = power.iter().sum();

        let mut battle_casualties = 0.0f32;
        // Raw damage dealt in this region today, summed across every side
        // regardless of who inflicts or receives it — the physical
        // destruction that feeds `Region::devastation`, independent of the
        // manpower/equipment casualties it also causes.
        let mut region_damage = 0.0f32;
        for (side_idx, &side_faction) in factions_present.iter().enumerate() {
            let enemy_power = total_power - power[side_idx];
            if enemy_power <= 0.0 {
                continue;
            }
            let dmg_side = enemy_power * COMBAT_DAMAGE * rng.range(0.85, 1.15);
            region_damage += dmg_side;
            let side_power = power[side_idx];
            if side_power <= 0.0 {
                continue;
            }

            let unit_ids: Vec<UnitId> = world
                .units_in(region_id)
                .filter(|u| u.owner == side_faction)
                .map(|u| u.id)
                .collect();

            for unit_id in unit_ids {
                fought[unit_id.index()] = true;
                let unit = world.unit_mut(unit_id);
                let raw_power = if side_faction == defender {
                    unit.combat_power() * defense_bonus
                } else {
                    unit.combat_power()
                };
                let share = raw_power / side_power;
                let dmg = dmg_side * share;

                unit.organization = (unit.organization - dmg * ORG_DAMAGE_MULT).max(0.0);
                let broken = if unit.organization <= 0.0 {
                    BROKEN_LOSS_MULT
                } else {
                    1.0
                };
                let manpower_loss = (dmg * MANPOWER_LOSS_PER_DAMAGE * broken).min(unit.manpower);
                unit.manpower -= manpower_loss;
                unit.equipment = (unit.equipment - dmg * EQUIPMENT_LOSS_PER_DAMAGE * broken).max(0.0);
                unit.morale = (unit.morale - 0.01 * broken).max(0.0);
                unit.experience = (unit.experience + 0.0015).min(1.0);

                casualties[side_faction.index()] += manpower_loss;
                battle_casualties += manpower_loss;
                world.faction_mut(side_faction).casualties += manpower_loss;
            }
        }

        let devastated = world.region_mut(region_id);
        devastated.devastation =
            (devastated.devastation + region_damage * DEVASTATION_PER_COMBAT_DAMAGE).min(1.0);

        events.push(Event::Battle {
            region: region_id,
            factions: factions_present.clone(),
            casualties: battle_casualties,
        });
    }

    CombatReport { fought, casualties }
}

fn pick_defender(world: &World, region: &Region, present: &[FactionId]) -> FactionId {
    if present.contains(&region.owner) {
        return region.owner;
    }
    let mut best = present[0];
    let mut best_power = world.region_power(region.id, best);
    for &f in &present[1..] {
        let p = world.region_power(region.id, f);
        if p > best_power || (p == best_power && f.0 < best.0) {
            best = f;
            best_power = p;
        }
    }
    best
}

/// Regenerates organization/morale for units not in combat, applies
/// unsupplied attrition, and routs or destroys broken units.
pub fn tick_recovery(world: &mut World, fought: &[bool], events: &mut Vec<Event>) {
    struct Delta {
        id: UnitId,
        organization: f32,
        morale: f32,
        manpower: f32,
    }

    let mut deltas = Vec::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let is_fighting = fought[unit.id.index()];
        let marching = unit.movement.is_some();

        let mut organization = unit.organization;
        let mut morale = unit.morale;
        let mut manpower = unit.manpower;

        if !is_fighting && !marching {
            let infra = world.region(unit.location).effective_infrastructure();
            organization += ORG_REGEN * (0.3 + 0.7 * unit.supply) * (0.6 + 0.4 * infra);
        }
        if !is_fighting {
            morale += MORALE_REGEN * unit.supply;
        }
        if unit.supply < ATTRITION_SUPPLY_THRESHOLD {
            manpower -= ATTRITION_MANPOWER * (1.0 - unit.supply / ATTRITION_SUPPLY_THRESHOLD);
            organization -= ATTRITION_ORG;
        }

        deltas.push(Delta {
            id: unit.id,
            organization: organization.clamp(0.0, UNIT_ORG),
            morale: morale.clamp(0.0, 1.0),
            manpower: manpower.max(0.0),
        });
    }

    for d in deltas {
        let unit = world.unit_mut(d.id);
        let owner = unit.owner;
        let attrition_loss = (unit.manpower - d.manpower).max(0.0);
        unit.organization = d.organization;
        unit.morale = d.morale;
        unit.manpower = d.manpower;
        if attrition_loss > 0.0 {
            world.faction_mut(owner).casualties += attrition_loss;
        }
    }

    enum Outcome {
        Retreat { to: RegionId, required: f32 },
        Destroyed,
    }

    let mut outcomes = Vec::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        if unit.manpower <= UNIT_DEATH_MANPOWER {
            // A unit this depleted is dead outright - it must not linger as
            // a phantom retreater that still blocks occupation and counts
            // in presence checks for one extra tick.
            outcomes.push((unit.id, Outcome::Destroyed));
        } else if unit.organization <= 0.0
            && unit.movement.is_none()
            && world.has_enemy_units(unit.location, unit.owner)
        {
            let mut candidates: Vec<RegionId> = world
                .neighbors(unit.location)
                .filter(|&r| {
                    world.region(r).owner == unit.owner && !world.has_enemy_units(r, unit.owner)
                })
                .collect();
            candidates.sort_by_key(|r| r.0);
            if let Some(&dest) = candidates.first() {
                let link = world.link_between(unit.location, dest).unwrap();
                let required = move_required(link.kind, world.region(dest).terrain, false) * 0.5;
                outcomes.push((unit.id, Outcome::Retreat { to: dest, required }));
            } else {
                outcomes.push((unit.id, Outcome::Destroyed));
            }
        }
    }

    for (id, outcome) in outcomes {
        match outcome {
            Outcome::Retreat { to, required } => {
                let unit = world.unit_mut(id);
                unit.movement = Some(Movement {
                    from: unit.location,
                    to,
                    progress: 0.0,
                    required,
                    retreat: true,
                });
            }
            Outcome::Destroyed => {
                let unit = world.unit_mut(id);
                let (region, owner, residual_manpower) = (unit.location, unit.owner, unit.manpower);
                unit.alive = false;
                if residual_manpower > 0.0 {
                    world.faction_mut(owner).casualties += residual_manpower;
                }
                events.push(Event::UnitDestroyed {
                    unit: id,
                    region,
                    owner,
                });
            }
        }
    }
}

/// Advances occupation progress in every region and flips ownership once it
/// reaches 100.
pub fn tick_occupation(world: &mut World, events: &mut Vec<Event>) {
    let n = world.regions.len();
    let mut present_factions: Vec<Vec<FactionId>> = Vec::with_capacity(n);
    for i in 0..n {
        let region_id = RegionId(i as u32);
        let mut factions: Vec<FactionId> = world.units_in(region_id).map(|u| u.owner).collect();
        factions.sort_by_key(|f| f.0);
        factions.dedup();
        present_factions.push(factions);
    }

    for i in 0..n {
        let region_id = RegionId(i as u32);
        let owner = world.region(region_id).owner;
        let present = &present_factions[i];

        if present.is_empty() || present.contains(&owner) {
            let region = world.region_mut(region_id);
            region.occupation = (region.occupation - OCCUPATION_DECAY).max(0.0);
            if region.occupation == 0.0 {
                region.occupier = None;
            }
            continue;
        }

        let occupier = present[0];
        let region = world.region_mut(region_id);
        if region.occupier != Some(occupier) {
            // A newly arrived occupier starts from zero: progress earned by
            // a previous occupying faction must not carry over to this one.
            region.occupation = 0.0;
        }
        region.occupier = Some(occupier);
        region.occupation += OCCUPATION_RATE;

        if region.occupation >= 100.0 {
            region.owner = occupier;
            region.occupation = 0.0;
            region.occupier = None;
            region.unrest += CAPTURE_UNREST;
            // A region changing hands is a war-damage spike of its own
            // (looting, sabotage, the fighting that won it) on top of
            // whatever combat damage already accrued, and any project the
            // previous owner had underway does not carry over.
            region.devastation = (region.devastation + DEVASTATION_ON_CAPTURE).min(1.0);
            region.construction = None;
            events.push(Event::RegionCaptured {
                region: region_id,
                from: owner,
                to: occupier,
            });
            let occupier_support =
                (world.faction(occupier).war_support + WAR_SUPPORT_CAPTURE_GAIN).clamp(0.0, 100.0);
            world.faction_mut(occupier).war_support = occupier_support;
            let owner_support =
                (world.faction(owner).war_support - WAR_SUPPORT_LOSS_PENALTY).clamp(0.0, 100.0);
            world.faction_mut(owner).war_support = owner_support;
        }
    }
}
