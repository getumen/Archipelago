//! Supply network: how far production reaches the front, and how it is
//! rationed to units once there. This is design.md §8's core loop — cutting
//! a single corridor region starves everything behind it.

use crate::balance::{
    ARMS_SUPPLY_NEED_PER_GAP, PROJECTED_SUPPLY_FACTOR, SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING,
    UNIT_EQUIPMENT,
};
use crate::good::Good;
use crate::world::World;

/// Recomputes `world.supply`: the maximum throughput each region can draw
/// on, propagated from every region's own industry/port base through
/// same-owner links. A region held by anyone but its owner cannot relay
/// supply onward (though it still receives what reaches it), which is what
/// makes contested chokepoints cut off everything behind them.
///
/// Stage 2C (docs/phase2-spec.md "2. 港湾・インフラによるノード側の上限") adds
/// a node-side cap alongside the existing link-side one: even an intact
/// Rail line can't relay more than the *receiving* region's own
/// infrastructure/port base (`Region::node_throughput`) can actually pass
/// through, so a devastated or underdeveloped relay point chokes supply
/// regardless of what arrives at its doorstep.
pub fn recompute_supply(world: &mut World) {
    let n = world.regions.len();
    let contested: Vec<bool> = (0..n)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();
    let node_throughput: Vec<f32> = world.regions.iter().map(|r| r.node_throughput()).collect();

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
                let infra_j = world.regions[j].effective_infrastructure();
                let v = (cap_i * link.kind.retention() * (0.55 + 0.45 * infra_j))
                    .min(link.kind.max_throughput())
                    .min(node_throughput[j]);
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

/// Rations each faction's stockpiled Munitions across its units, region by
/// region, and eases every unit's `supply` ratio toward its funded target.
///
/// Stage 2C (docs/phase2-spec.md "3. 品目別の到達率") widens this from
/// Munitions alone to Munitions *and* Arms delivery, contending for the same
/// regional throughput (`avail`, from `world.supply`) rather than each
/// getting their own. The split is controlled by
/// `Faction::logistics_priority[Munitions]/[Arms]` — never a hardcoded
/// order, per the standing rule against giving one consumer a fixed first
/// claim on a shared resource. Within a region/faction cell, each good's
/// priority-weighted share is a first pass; whatever either good's share
/// goes unused (because its own demand is smaller than its share — Arms
/// demand is usually zero outside active reinforcement) is then handed to
/// the other in proportion to its remaining unmet demand, so idle Arms
/// throughput doesn't sit ring-fenced away from Munitions units that could
/// actually use it, and vice versa.
///
/// Munitions delivery still drains `Faction::stock[Munitions]` exactly as
/// before (`unit.supply`, the arrival ratio units consume upkeep from). Arms
/// delivery does *not* touch `Faction::stock[Arms]` here — it only computes
/// `unit.arms_delivery`, the arrival-rate ceiling `action::apply_reinforce`
/// applies on top of its own separate stock check, so a faction with a full
/// armory still can't instantly re-equip a unit the network can't reach.
pub fn distribute_supply(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let contested: Vec<bool> = (0..n_regions)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();

    let mut demand_munitions = vec![vec![0.0f32; n_factions]; n_regions];
    let mut demand_arms = vec![vec![0.0f32; n_factions]; n_regions];
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
        demand_munitions[r][f] += unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult;
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        demand_arms[r][f] += equipment_gap * ARMS_SUPPLY_NEED_PER_GAP;
    }

    let mut avail = vec![vec![0.0f32; n_factions]; n_regions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            if demand_munitions[r][f] <= 0.0 && demand_arms[r][f] <= 0.0 {
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

    let mut served_munitions = vec![vec![0.0f32; n_factions]; n_regions];
    let mut served_arms = vec![vec![0.0f32; n_factions]; n_regions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            let total_avail = avail[r][f];
            if total_avail <= 0.0 {
                continue;
            }
            let faction = &world.factions[f];
            let w_munitions = faction.logistics_priority[Good::Munitions.index()].max(0.0);
            let w_arms = faction.logistics_priority[Good::Arms.index()].max(0.0);
            let w_sum = w_munitions + w_arms;
            let (share_munitions_frac, share_arms_frac) = if w_sum > 0.0 {
                (w_munitions / w_sum, w_arms / w_sum)
            } else {
                (0.5, 0.5)
            };
            let share_munitions = total_avail * share_munitions_frac;
            let share_arms = total_avail * share_arms_frac;

            let dm = demand_munitions[r][f];
            let da = demand_arms[r][f];
            let served_m1 = dm.min(share_munitions);
            let served_a1 = da.min(share_arms);

            // Redistribute whatever either good's priority share left
            // unused to the other, in proportion to its own remaining
            // unmet demand - so a good with no demand this tick (Arms,
            // most days) doesn't ring-fence throughput a hungry Munitions
            // demand could actually use, without ever hardcoding which
            // good gets first claim on the leftover.
            let leftover = (share_munitions - served_m1) + (share_arms - served_a1);
            let remaining_m = dm - served_m1;
            let remaining_a = da - served_a1;
            let remaining_total = remaining_m + remaining_a;
            let (extra_m, extra_a) = if remaining_total > 0.0 && leftover > 0.0 {
                let extra = leftover.min(remaining_total);
                (extra * remaining_m / remaining_total, extra * remaining_a / remaining_total)
            } else {
                (0.0, 0.0)
            };

            served_munitions[r][f] = served_m1 + extra_m;
            served_arms[r][f] = served_a1 + extra_a;
        }
    }

    let mut total_served = vec![0.0f32; n_factions];
    let mut total_demand = vec![0.0f32; n_factions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            total_served[f] += served_munitions[r][f];
            total_demand[f] += demand_munitions[r][f];
        }
    }

    let mut scale = vec![1.0f32; n_factions];
    for faction in world.factions.iter_mut() {
        let f = faction.id.index();
        let munitions = faction.stock[Good::Munitions.index()];
        scale[f] = if total_served[f] > 0.0 {
            (munitions / total_served[f]).min(1.0)
        } else {
            1.0
        };
        faction.stock[Good::Munitions.index()] = (munitions - total_served[f] * scale[f]).max(0.0);
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
        let target_munitions = if demand_munitions[r][f] > 0.0 {
            served_munitions[r][f] / demand_munitions[r][f] * scale[f]
        } else {
            1.0
        };
        unit.supply =
            (unit.supply + (target_munitions - unit.supply) * SUPPLY_SMOOTHING).clamp(0.0, 1.0);

        let target_arms = if demand_arms[r][f] > 0.0 {
            (served_arms[r][f] / demand_arms[r][f]).min(1.0)
        } else {
            1.0
        };
        unit.arms_delivery = (unit.arms_delivery + (target_arms - unit.arms_delivery) * SUPPLY_SMOOTHING)
            .clamp(0.0, 1.0);

        // External code review fix (Stage 2C): reset the unit's per-tick
        // Arms delivery *budget* here, once, from scratch - the absolute
        // equipment `arms_delivery`'s freshly-eased ratio allows against
        // *this instant's* gap, not accumulated or rolled over from
        // before. `action::apply_reinforce` spends this down as it
        // delivers equipment; stamping `arms_delivery_region` alongside it
        // records which region this budget was computed for, so a unit
        // that moves before its next `ReinforceUnit` action is detected as
        // stale and recomputed on the spot instead of trusted -
        // `logistics::instantaneous_arms_delivery`.
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        unit.arms_budget = equipment_gap * unit.arms_delivery;
        unit.arms_delivery_region = unit.location;
    }
}

/// External code review fix (Stage 2C): recomputes a single unit's Arms
/// delivery ratio and per-tick budget from its *current* region, for
/// `action::apply_reinforce` to call when the unit's cached
/// `Unit::arms_delivery_region` no longer matches `Unit::location` -
/// `distribute_supply` above only runs once a tick, before movement, so a
/// unit that has since moved carries numbers stamped from a region it has
/// already left.
///
/// Mirrors `distribute_supply`'s per-region Arms throughput share, but
/// treats `unit`'s own equipment gap as the *only* Arms demand in its
/// region rather than re-running the full multi-unit contention pass
/// above. That is exact when no other same-faction unit shares the region,
/// and otherwise a deliberately conservative simplification kept simple on
/// purpose: it is a single self-contained calculation for one unit at
/// action-apply time, not a second copy of the batched, multi-unit
/// leftover-redistribution logic above. It never grants more of the
/// region's throughput than a from-scratch pass would give this unit
/// alone, and the national Arms stock check in `apply_reinforce` still
/// bounds what actually leaves the armory regardless.
///
/// Returns `(ratio, budget)`: `ratio` in `0..=1`, `budget` the absolute
/// equipment units deliverable this instant (`gap * ratio`).
pub fn instantaneous_arms_delivery(world: &World, unit_id: crate::ids::UnitId) -> (f32, f32) {
    let unit = world.unit(unit_id);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
    if need_equipment <= 0.0 {
        return (1.0, 0.0);
    }

    let region = unit.location;
    let r = region.index();
    let faction = unit.owner;
    let owner = world.regions[r].owner;

    let avail = if owner == faction {
        world.supply[r]
    } else {
        world.regions[r]
            .links
            .iter()
            .map(|link| link.to.index())
            .filter(|&j| {
                world.regions[j].owner == faction
                    && !world.has_enemy_units(world.regions[j].id, faction)
            })
            .map(|j| world.supply[j])
            .fold(0.0f32, f32::max)
            * PROJECTED_SUPPLY_FACTOR
    };

    if avail <= 0.0 {
        return (0.0, 0.0);
    }

    let faction_ref = &world.factions[faction.index()];
    let w_munitions = faction_ref.logistics_priority[Good::Munitions.index()].max(0.0);
    let w_arms = faction_ref.logistics_priority[Good::Arms.index()].max(0.0);
    let w_sum = w_munitions + w_arms;
    let share_arms_frac = if w_sum > 0.0 { w_arms / w_sum } else { 0.5 };
    let share_arms = avail * share_arms_frac;

    let demand_arms = need_equipment * ARMS_SUPPLY_NEED_PER_GAP;
    let ratio = (share_arms / demand_arms).min(1.0);
    (ratio, need_equipment * ratio)
}
