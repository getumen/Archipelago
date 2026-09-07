//! Stage 10A (docs/phase10-spec.md "1. 基地" / "0. 方針"): air units, based
//! at a `transport::TransportNodeKind::Airfield` node
//! (`world::Station::Airfield`), and their Munitions/Arms upkeep demand.
//! Deliberately its own module, mirroring `naval.rs`'s "land/sea are
//! different topologies, a small amount of structural duplication beats a
//! forced shared abstraction" stance one domain further: an airfield node
//! is neither a region nor a sea zone, so this keeps its own near-identical
//! copies of `logistics::region_demand`'s per-unit formula and
//! `naval::fleet_demand_and_avail`/`apply_fleet_supply`'s shape rather than
//! bending either into a third case.
//!
//! **No second supply path.** CLAUDE.md's own record of Phase 9's most
//! expensive defect - a fleet's demand answered by a separate, never-
//! consumed structural capacity figure while genuine flow accounting moved
//! on without it - is exactly the mistake this module does not get to
//! repeat a third time. Every function here only ever *reads* a demand
//! sink's already-computed share of `logistics::compute_transport_flow`'s
//! own contended, capacity-constrained rounds (`World::supply_air`, fed by
//! `logistics::build_transport_graph`'s `TransportNodeKind::Airfield` arm)
//! or *asks* for an instantaneous grant against this tick's own genuinely
//! unclaimed leftover (`logistics::instantaneous_air_avail`/
//! `commit_instantaneous_air_grant`) - never a fresh, independent capacity
//! query of its own.
//!
//! Stage 10A ships no air-vs-air combat, air superiority or interdiction
//! (10B/10C) - "in combat" for an air unit's own upkeep multiplier below
//! means only that the airfield's own region is currently contested by a
//! hostile land force, the same fact `military::is_pinned`'s `Station::
//! Airfield` arm already keys "pinned" off; there is no separate air-combat
//! flag to fold in yet.

use crate::balance::{
    ARMS_SUPPLY_NEED_PER_GAP, COMBAT_SUPPLY_MULT, SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING, UNIT_EQUIPMENT,
};
use crate::good::Good;
use crate::ids::UnitId;
use crate::logistics;
use crate::world::World;

/// Per-node, per-faction Munitions/Arms upkeep demand this tick - the
/// air-domain twin of `logistics::region_demand`/`naval::sea_demand`.
/// Sized `world.transport_nodes.len()` so it indexes directly by
/// `TransportNodeId`; every row for a node that isn't an `Airfield` simply
/// stays zero, since no unit's `Station::Airfield` can ever name one.
pub(crate) fn air_demand(world: &World) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let n_nodes = world.transport_nodes.len();
    let n_factions = world.factions.len();

    let mut demand_munitions = vec![vec![0.0f32; n_factions]; n_nodes];
    let mut demand_arms = vec![vec![0.0f32; n_factions]; n_nodes];
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let Some(node) = unit.station.airfield() else {
            continue;
        };
        let ni = node.index();
        let f = unit.owner.index();
        let in_combat = world.has_enemy_units(world.transport_node(node).region, unit.owner);
        let mult = if in_combat {
            COMBAT_SUPPLY_MULT
        } else {
            1.0
        };
        demand_munitions[ni][f] += unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult;
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        demand_arms[ni][f] += equipment_gap * ARMS_SUPPLY_NEED_PER_GAP;
    }
    (demand_munitions, demand_arms)
}

/// Per-node, per-faction Munitions/Arms demand and available throughput for
/// air units, mirroring `naval::fleet_demand_and_avail`'s per-zone arrays
/// but keyed by `TransportNodeId`. Pure (no mutation) - `logistics::
/// distribute_supply` folds the result into the very same national-stock
/// scaling pass land and sea already share, rather than letting either
/// domain spend the shared Munitions stock first.
///
/// `avail[n][f]` is exactly `world.supply_air[n][f]` - air-unit demand
/// already went through `logistics::compute_transport_flow`'s own
/// contended, capacity-constrained rounds (via each `Airfield` node's own
/// `-> AirDemand` edge), competing for the network on the same terms every
/// other candidate does (this module's own doc, "No second supply path").
pub fn air_demand_and_avail(world: &World) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let (demand_munitions, demand_arms) = air_demand(world);
    let n_nodes = world.transport_nodes.len();
    let n_factions = world.factions.len();

    let mut avail = vec![vec![0.0f32; n_factions]; n_nodes];
    for n in 0..n_nodes {
        for f in 0..n_factions {
            if demand_munitions[n][f] <= 0.0 && demand_arms[n][f] <= 0.0 {
                continue;
            }
            avail[n][f] = world.supply_air[n][f];
        }
    }

    (demand_munitions, demand_arms, avail)
}

/// Applies the already-split, already-national-stock-scaled per-node Arms/
/// Munitions service back onto each air unit's `supply`/`arms_delivery`/
/// `arms_budget`, exactly mirroring `naval::apply_fleet_supply`/
/// `logistics::distribute_supply`'s final per-unit loop for land units.
pub fn apply_air_supply(
    world: &mut World,
    served_munitions: &[Vec<f32>],
    demand_munitions: &[Vec<f32>],
    served_arms: &[Vec<f32>],
    demand_arms: &[Vec<f32>],
    scale: &[f32],
) {
    for unit in world.units.iter_mut() {
        if !unit.alive {
            continue;
        }
        let Some(node) = unit.station.airfield() else {
            continue;
        };
        let ni = node.index();
        let f = unit.owner.index();

        let target_munitions = if demand_munitions[ni][f] > 0.0 {
            served_munitions[ni][f] / demand_munitions[ni][f] * scale[f]
        } else {
            1.0
        };
        unit.supply = (unit.supply + (target_munitions - unit.supply) * SUPPLY_SMOOTHING)
            .clamp(0.0, 1.0);

        let target_arms = if demand_arms[ni][f] > 0.0 {
            (served_arms[ni][f] / demand_arms[ni][f]).min(1.0)
        } else {
            1.0
        };
        unit.arms_delivery = (unit.arms_delivery
            + (target_arms - unit.arms_delivery) * SUPPLY_SMOOTHING)
            .clamp(0.0, 1.0);

        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        unit.arms_budget = equipment_gap * unit.arms_delivery;
        unit.arms_delivery_station = unit.station;
    }
}

/// `logistics::land_unit_supply_avail`/`naval::fleet_unit_supply_avail`'s
/// air-domain counterpart - the transport network's current delivery
/// reaching `unit`'s own airfield station, for its own faction.
///
/// Mirrors `fleet_unit_supply_avail`'s staleness handling exactly: `world.
/// supply_air` is only ever written once a tick, by `logistics::
/// recompute_supply`, before that same tick's `military::tick_movement`
/// runs - but Stage 10A ships no `Domain::Air` movement at all
/// (`military::retreat_candidate`'s `Station::Airfield` arm, `action::
/// apply_move`'s doc), so `arms_delivery_station != station` can only ever
/// arise here the same way it can for a freshly-`RecruitUnit`-ed land/sea
/// unit within the same tick's action batch, before that tick's own
/// `distribute_supply` has stamped it once. The fallback still goes through
/// `logistics::instantaneous_air_avail` - a peek against this tick's
/// already-spent-down leftover network capacity, never a second, fresh
/// full-capacity flow run - for exactly that case.
pub fn air_unit_supply_avail(world: &World, unit_id: UnitId) -> f32 {
    let unit = world.unit(unit_id);
    if unit.station.airfield().is_none() {
        return 0.0;
    }
    if unit.arms_delivery_station != unit.station {
        return logistics::instantaneous_air_avail(world, unit_id);
    }
    let node = unit.station.airfield().expect("checked above");
    world.supply_air[node.index()][unit.owner.index()]
}

/// `logistics::instantaneous_arms_delivery`/`naval::
/// instantaneous_fleet_arms_delivery`'s air-domain counterpart. Needs no
/// staleness fix of its own beyond `air_unit_supply_avail`'s: it never
/// reads `world.supply_air` directly, only through that function's `avail`
/// call below, which already recomputes fresh when needed.
pub fn instantaneous_air_arms_delivery(world: &World, unit_id: UnitId) -> (f32, f32) {
    let unit = world.unit(unit_id);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
    if need_equipment <= 0.0 {
        return (1.0, 0.0);
    }
    if unit.station.airfield().is_none() {
        return (1.0, 0.0);
    }
    let faction = unit.owner;

    let avail = air_unit_supply_avail(world, unit_id);
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
