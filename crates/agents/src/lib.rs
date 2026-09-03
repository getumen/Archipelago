//! A rule-based `Agent` for `archipelago-sim` (design.md §19 / mvp-spec.md §7):
//! set economic policy, reinforce weakened units, recruit where it's safe,
//! pick offensives by value-per-threat, and walk idle interior units toward
//! the front. No randomness of its own, so a run stays fully determined by
//! the simulation's seed.

use std::collections::{BTreeSet, VecDeque};

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{
    COMBAT_SUPPLY_MULT, SUPPLY_NEED_PER_MANPOWER, UNIT_EQUIPMENT, UNIT_MANPOWER,
};
use archipelago_sim::ids::{FactionId, RegionId, UnitId};
use archipelago_sim::observation::Observation;
use archipelago_sim::world::World;

/// Minimum unit-count headroom (as a multiple of a fresh unit's cost) a
/// faction keeps in reserve before it will spend on a new recruit.
const RECRUIT_STOCK_MARGIN: f32 = 1.5;
/// A unit below this fraction of full manpower/equipment gets reinforced.
const REINFORCE_THRESHOLD: f32 = 0.75;
/// Days of stockpiled supply below which the agent shifts production toward
/// supplies instead of equipment (design.md §9's war/economy trade-off).
const LOW_SUPPLY_DAYS: f32 = 20.0;
/// `production_mix` (the equipment share of output) used when the stockpile
/// is running low.
const SUPPLY_FOCUSED_MIX: f32 = 0.3;
/// `production_mix` used otherwise, favoring equipment.
const EQUIPMENT_FOCUSED_MIX: f32 = 0.7;
/// Manpower pool above which conscription is throttled hard - hoarding
/// manpower has a real cost since it suppresses labour via `region.mobilized`.
const CONSCRIPTION_THROTTLE_MANPOWER: f32 = 25.0;

/// Decides for one faction every `period` days (offset by faction id so the
/// three AIs don't all act on the same day), per mvp-spec.md §7.
pub struct HeuristicAgent {
    faction: FactionId,
    caution: f32,
    period: u32,
    offset: u32,
}

impl HeuristicAgent {
    /// `caution` is the force-ratio margin required before the agent will
    /// launch an offensive: it attacks only when
    /// `own_power >= caution * (defended_power + 0.6)`. Higher values make
    /// the agent more cautious (it waits for a bigger edge); ~1.0 means it
    /// will attack at parity. Values around 1.1-1.5 work well (the MVP
    /// scenario uses 1.15 / 1.30 / 1.45 for its three factions).
    pub fn new(faction: FactionId, caution: f32) -> Self {
        const PERIOD: u32 = 4;
        HeuristicAgent {
            faction,
            caution,
            period: PERIOD,
            offset: faction.0 % PERIOD,
        }
    }
}

impl Agent for HeuristicAgent {
    fn name(&self) -> &str {
        "HeuristicAgent"
    }

    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        if obs.world.day % self.period != self.offset {
            return Vec::new();
        }

        let mut actions = Vec::new();
        set_policy(self.faction, obs, &mut actions);
        reinforce(self.faction, obs, &mut actions);
        recruit(self.faction, obs, &mut actions);
        offensive(self.faction, self.caution, obs, &mut actions);

        let already_moved: BTreeSet<UnitId> = actions
            .iter()
            .filter_map(|a| match a {
                Action::MoveUnit { unit, .. } => Some(*unit),
                _ => None,
            })
            .collect();
        advance_interior(self.faction, obs, &already_moved, &mut actions);

        actions
    }
}

fn set_policy(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let manpower = f.manpower;
    let conscription = if manpower < 5.0 {
        0.9
    } else if manpower > CONSCRIPTION_THROTTLE_MANPOWER {
        0.15
    } else {
        0.6
    };
    actions.push(Action::SetConscription(conscription));

    let daily_demand: f32 = obs
        .own_units()
        .into_iter()
        .map(|unit_id| {
            let unit = obs.world.unit(unit_id);
            let mult = if obs.world.has_enemy_units(unit.location, faction) {
                COMBAT_SUPPLY_MULT
            } else {
                1.0
            };
            unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult
        })
        .sum();
    let production_mix = if daily_demand > 0.0 && f.supplies / daily_demand < LOW_SUPPLY_DAYS {
        SUPPLY_FOCUSED_MIX
    } else {
        EQUIPMENT_FOCUSED_MIX
    };
    actions.push(Action::SetProductionMix(production_mix));
}

/// Tops up under-strength units sitting safely in friendly, uncontested territory.
fn reinforce(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let mut units = obs.own_units();
    units.sort_by_key(|u| u.0);

    for unit_id in units {
        let unit = obs.world.unit(unit_id);
        let region = unit.location;
        if obs.world.region(region).owner != faction {
            continue;
        }
        if obs.world.has_enemy_units(region, faction) {
            continue;
        }
        if unit.strength() < REINFORCE_THRESHOLD {
            actions.push(Action::ReinforceUnit { unit: unit_id });
        }
    }
}

/// Raises a new corps at the capital, or failing that the safest, most
/// industrious region held, as long as the faction can afford it and isn't
/// already well-manned relative to its industrial base.
fn recruit(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let unit_cap = 3.0 + obs.world.industry_total(faction) / 5.0;
    if obs.own_units().len() as f32 >= unit_cap {
        return;
    }
    if f.manpower < UNIT_MANPOWER * RECRUIT_STOCK_MARGIN
        || f.equipment < UNIT_EQUIPMENT * RECRUIT_STOCK_MARGIN
    {
        return;
    }

    let capital = f.capital;
    let region = if is_safe_own_region(faction, obs, capital) {
        Some(capital)
    } else {
        safe_own_regions(faction, obs)
            .into_iter()
            .fold(None, |best: Option<(RegionId, f32)>, r| {
                let industry = obs.world.region(r).industry;
                match best {
                    Some((_, best_industry)) if industry <= best_industry => best,
                    _ => Some((r, industry)),
                }
            })
            .map(|(r, _)| r)
    };

    if let Some(region) = region {
        actions.push(Action::RecruitUnit { region });
    }
}

fn is_safe_own_region(faction: FactionId, obs: &Observation, region: RegionId) -> bool {
    obs.world.region(region).owner == faction && !obs.world.has_enemy_units(region, faction)
}

/// Own regions with no enemy units present, in ascending id order.
fn safe_own_regions(faction: FactionId, obs: &Observation) -> Vec<RegionId> {
    let mut regions: Vec<RegionId> = obs
        .own_regions()
        .into_iter()
        .filter(|&r| !obs.world.has_enemy_units(r, faction))
        .collect();
    regions.sort_by_key(|r| r.0);
    regions
}

/// From every uncontested own region, picks the best adjacent target
/// (highest `value / (1 + enemy_power)`) and, if the garrison is strong
/// enough relative to it, sends units in - leaving one unit behind as a
/// garrison when attacking out of a front region with a defended target.
/// Units already moving toward that same target fill part of that quota
/// without a fresh order (see `is_en_route_here` below).
fn offensive(faction: FactionId, caution: f32, obs: &Observation, actions: &mut Vec<Action>) {
    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);

    for region in safe_own_regions(faction, obs) {
        let mut targets: Vec<RegionId> = obs
            .world
            .neighbors(region)
            .filter(|&n| obs.world.region(n).owner != faction)
            .collect();
        targets.sort_by_key(|r| r.0);
        if targets.is_empty() {
            continue;
        }

        let best_target = targets
            .into_iter()
            .fold(None, |best: Option<(RegionId, f32)>, t| {
                let value = obs.world.region(t).value();
                let score = value / (1.0 + obs.enemy_power(t));
                match best {
                    Some((_, best_score)) if score <= best_score => best,
                    _ => Some((t, score)),
                }
            })
            .map(|(t, _)| t);
        let Some(target) = best_target else { continue };

        let enemy_power = obs.enemy_power(target);
        let terrain_bonus = obs.world.region(target).terrain.defense_bonus();
        let own_power = obs.own_power(region);
        if own_power < caution * (enemy_power * terrain_bonus + 0.6) {
            continue;
        }

        let is_front = front.binary_search(&region).is_ok();
        let target_undefended = enemy_power <= 0.0;
        let mut present: Vec<UnitId> = obs
            .world
            .units_in(region)
            .filter(|u| u.owner == faction)
            .map(|u| u.id)
            .collect();
        present.sort_by_key(|u| u.0);

        // Garrison sizing counts every unit physically here, whether idle
        // or already departing.
        let send_capacity = if is_front && !target_undefended {
            present.len().saturating_sub(1)
        } else {
            present.len()
        };

        let is_en_route_here =
            |u: &UnitId| matches!(obs.world.unit(*u).movement, Some(mv) if mv.to == target);

        // A unit already under way toward `target` occupies one of the
        // capacity slots without needing a fresh order - re-issuing
        // `MoveUnit` for it would reset `Movement::progress` to zero, and
        // since the agent re-plans every 4 days while a hostile crossing
        // can take longer than that, the attack would never land. A unit
        // moving toward somewhere else still counts as idle here and can be
        // redirected - that's a deliberate change of target, not an
        // accident of re-planning.
        let already_en_route = present.iter().filter(|u| is_en_route_here(u)).count();
        let new_orders = send_capacity.saturating_sub(already_en_route);

        let idle: Vec<UnitId> = present.into_iter().filter(|u| !is_en_route_here(u)).collect();
        for &unit_id in idle.iter().take(new_orders) {
            actions.push(Action::MoveUnit { unit: unit_id, to: target });
        }
    }
}

/// Walks units that aren't already at (or ordered toward) the front one
/// step closer, via `Observation::path_next`, toward whichever front region
/// is nearest by raw map distance.
fn advance_interior(
    faction: FactionId,
    obs: &Observation,
    already_moved: &BTreeSet<UnitId>,
    actions: &mut Vec<Action>,
) {
    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);
    if front.is_empty() {
        return;
    }

    let mut units = obs.own_units();
    units.sort_by_key(|u| u.0);

    for unit_id in units {
        if already_moved.contains(&unit_id) {
            continue;
        }
        let unit = obs.world.unit(unit_id);
        if unit.movement.is_some() {
            continue;
        }
        let location = unit.location;
        if front.binary_search(&location).is_ok() {
            continue;
        }
        if obs.world.has_enemy_units(location, faction) {
            continue;
        }

        let distances = map_distances(obs.world, location);
        let nearest_front = front
            .iter()
            .copied()
            .fold(None, |best: Option<(RegionId, u32)>, f| {
                let d = distances[f.index()];
                match best {
                    Some((_, best_d)) if d >= best_d => best,
                    _ => Some((f, d)),
                }
            })
            .map(|(f, _)| f);
        let Some(nearest_front) = nearest_front else { continue };

        if let Some(next) = obs.path_next(location, nearest_front) {
            actions.push(Action::MoveUnit { unit: unit_id, to: next });
        }
    }
}

#[cfg(test)]
mod tests;

/// Breadth-first hop count from `from` to every region, over the full map
/// graph (not restricted to friendly territory - this is just "how far
/// away is the front", not a route the unit is forced to take).
fn map_distances(world: &World, from: RegionId) -> Vec<u32> {
    let mut dist = vec![u32::MAX; world.regions.len()];
    dist[from.index()] = 0;
    let mut queue = VecDeque::new();
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        let mut neighbors: Vec<RegionId> = world.neighbors(current).collect();
        neighbors.sort_by_key(|r| r.0);
        for next in neighbors {
            if dist[next.index()] == u32::MAX {
                dist[next.index()] = dist[current.index()] + 1;
                queue.push_back(next);
            }
        }
    }

    dist
}
