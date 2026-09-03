//! Supply network: how far production reaches the front, and how it is
//! rationed to units once there. This is design.md §8's core loop — cutting
//! a single corridor region starves everything behind it.

use crate::balance::{PROJECTED_SUPPLY_FACTOR, SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING};
use crate::world::World;

/// Recomputes `world.supply`: the maximum throughput each region can draw
/// on, propagated from every region's own industry/port base through
/// same-owner links. A region held by anyone but its owner cannot relay
/// supply onward (though it still receives what reaches it), which is what
/// makes contested chokepoints cut off everything behind them.
pub fn recompute_supply(world: &mut World) {
    let n = world.regions.len();
    let contested: Vec<bool> = (0..n)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();

    let mut cap: Vec<f32> = world.regions.iter().map(|r| r.supply_source()).collect();

    for _ in 0..n {
        let mut changed = false;
        for i in 0..n {
            if contested[i] {
                continue;
            }
            let owner_i = world.regions[i].owner;
            let cap_i = cap[i];
            for link in world.regions[i].links.clone() {
                let j = link.to.index();
                if world.regions[j].owner != owner_i {
                    continue;
                }
                let infra_j = world.regions[j].infrastructure;
                let v = (cap_i * link.kind.retention() * (0.55 + 0.45 * infra_j))
                    .min(link.kind.max_throughput());
                if v > cap[j] {
                    cap[j] = v;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    world.supply = cap;
}

/// Rations each faction's stockpiled `supplies` across its units, region by
/// region, and eases every unit's `supply` ratio toward its funded target.
pub fn distribute_supply(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let contested: Vec<bool> = (0..n_regions)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();

    let mut demand = vec![vec![0.0f32; n_factions]; n_regions];
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let r = unit.location.index();
        let f = unit.owner.index();
        let in_combat = world.has_enemy_units(unit.location, unit.owner);
        let mult = if in_combat {
            crate::balance::COMBAT_SUPPLY_MULT
        } else {
            1.0
        };
        demand[r][f] += unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult;
    }

    let mut avail = vec![vec![0.0f32; n_factions]; n_regions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            if demand[r][f] <= 0.0 {
                continue;
            }
            avail[r][f] = if world.regions[r].owner.index() == f {
                world.supply[r]
            } else {
                world
                    .regions[r]
                    .links
                    .iter()
                    .map(|link| link.to.index())
                    .filter(|&j| world.regions[j].owner.index() == f && !contested[j])
                    .map(|j| world.supply[j])
                    .fold(0.0f32, f32::max)
                    * PROJECTED_SUPPLY_FACTOR
            };
        }
    }

    let mut served = vec![vec![0.0f32; n_factions]; n_regions];
    let mut total_served = vec![0.0f32; n_factions];
    let mut total_demand = vec![0.0f32; n_factions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            served[r][f] = demand[r][f].min(avail[r][f]);
            total_served[f] += served[r][f];
            total_demand[f] += demand[r][f];
        }
    }

    let mut scale = vec![1.0f32; n_factions];
    for faction in world.factions.iter_mut() {
        let f = faction.id.index();
        scale[f] = if total_served[f] > 0.0 {
            (faction.supplies / total_served[f]).min(1.0)
        } else {
            1.0
        };
        faction.supplies = (faction.supplies - total_served[f] * scale[f]).max(0.0);
        faction.supply_ratio = if total_demand[f] > 0.0 {
            (total_served[f] * scale[f] / total_demand[f]).min(1.0)
        } else {
            1.0
        };
    }

    for unit in world.units.iter_mut() {
        if !unit.alive {
            continue;
        }
        let r = unit.location.index();
        let f = unit.owner.index();
        let target = if demand[r][f] > 0.0 {
            served[r][f] / demand[r][f] * scale[f]
        } else {
            1.0
        };
        unit.supply = (unit.supply + (target - unit.supply) * SUPPLY_SMOOTHING).clamp(0.0, 1.0);
    }
}
