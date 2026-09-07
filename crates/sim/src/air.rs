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
//! Stage 10A shipped no air-vs-air combat, air superiority or interdiction
//! (10B/10C) - "in combat" for an air unit's own upkeep multiplier below
//! meant only that the airfield's own region is currently contested by a
//! hostile land force, the same fact `military::is_pinned`'s `Station::
//! Airfield` arm already keys "pinned" off; there was no separate
//! air-combat flag to fold in yet.
//!
//! Stage 10B (docs/phase10-spec.md "2. 制空権") adds `tick_air_superiority`
//! below: air superiority stands over a *region*, for whichever factions'
//! airfields' committed air power reaches it within `AIR_OPERATING_RADIUS_
//! KM` (straight-line geographic distance between `Region::position`s,
//! never the transport network - aircraft don't fly along railways). It
//! deliberately still does nothing else this stage - not wired into
//! interdiction, supply or ground combat (10C's job) - so its correctness
//! can be verified in isolation first, the same 8A/8B, 9A/9B split this
//! phase's own §5 calls out by name.

use crate::balance::{
    AIR_OPERATING_RADIUS_KM, ARMS_SUPPLY_NEED_PER_GAP, COMBAT_SUPPLY_MULT, SUPPLY_NEED_PER_MANPOWER,
    SUPPLY_SMOOTHING, UNIT_EQUIPMENT,
};
use crate::good::Good;
use crate::ids::{RegionId, UnitId};
use crate::logistics;
use crate::transport::TransportNodeKind;
use crate::world::{AirSuperiority, World};

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

/// Per-`Airfield`-node, per-faction committed air power - `combat_power()`
/// summed over every alive air unit based there, in ascending `UnitId`
/// order (`World::units`'s own storage order, never a `HashMap`). Split out
/// of `tick_air_superiority` below so that function's O(regions ×
/// airfields) reach test walks this once-computed, node-indexed table
/// rather than re-scanning every unit for every region - the same
/// single-pass-over-units shape `air_demand` above already uses, reused
/// rather than reinvented (docs/conventions.md §1, "エクストリームプログラ
/// ミング禁止" cuts the other way too: don't refuse to share a shape that
/// already fits). A node that isn't an `Airfield` simply never has any
/// `Station::Airfield(that node)` unit, so its row stays all zero.
fn node_air_power(world: &World) -> Vec<Vec<f32>> {
    let n_nodes = world.transport_nodes.len();
    let n_factions = world.factions.len();
    let mut power = vec![vec![0.0f32; n_factions]; n_nodes];
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let Some(node) = unit.station.airfield() else {
            continue;
        };
        power[node.index()][unit.owner.index()] += unit.combat_power();
    }
    power
}

/// Straight-line distance between two `Region::position`s - deliberately
/// plain Euclidean `sqrt`, no map projection: docs/phase10-spec.md "2. 制空
/// 権" only asks for "地理的な距離" (geographic distance) as opposed to
/// distance through the transport network, not a geodesic on an ellipsoid,
/// and every position `crate::sim` ever sees already went through whatever
/// projection produced it (`tools/hexmap/hexgrid.py`'s equirectangular
/// approximation, for `scenarios/japan_hex.json`) before reaching this crate.
fn geographic_distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

/// Recomputes every region's `Region::air_superiority` from the committed
/// air power of whichever factions can currently reach it - mirrors
/// `naval::tick_sea_control`'s `control[f] = power[f] / sum(power[*])`
/// shape exactly (all `AirSuperiority::NEUTRAL` when nobody reaches),
/// generalized one domain further: from "the fleets sitting in this zone"
/// to "every `Airfield` node within `AIR_OPERATING_RADIUS_KM` of this
/// region that currently hosts at least one air unit" (docs/phase10-spec.md
/// "2. 制空権": "0 を中立として、両勢力の投入戦力から比率で導く。固定の優
/// 先順位を置かない" - CLAUDE.md §6's first listed defect, so this divides
/// by the *shared* total rather than picking a side to favor).
///
/// Must run every tick, never cached or sampled once: an air unit that
/// died, disbanded, or was freshly recruited today is reflected the same
/// tick this function next runs, which is exactly what gives
/// `Region::air_superiority` its required recovery path back to
/// `NEUTRAL` once no reaching faction has any air unit left
/// (CLAUDE.md「繰り返し踏んだ欠陥」: "発令時点の値を焼き込まない" /
/// "状態には必ず回復経路を持たせる").
///
/// Fixed iteration order throughout for determinism (CLAUDE.md's own record
/// of `enemy_power`'s `total - own` rewrite flipping an AI decision 300
/// days later from a bitwise-inequivalent-but-algebraically-equal
/// reassociation): regions in ascending `RegionId` order, airfield nodes in
/// ascending `TransportNodeId` order (`World::transport_nodes`'s own
/// storage order), factions in ascending `FactionId` order - never a
/// `HashMap`/`HashSet` anywhere in the accumulation.
pub fn tick_air_superiority(world: &mut World) {
    let n_factions = world.factions.len();
    let node_power = node_air_power(world);

    for region_idx in 0..world.regions.len() {
        let region_id = RegionId(region_idx as u32);
        let target_pos = world.region(region_id).position;

        let mut power = vec![0.0f32; n_factions];
        for node in &world.transport_nodes {
            if node.kind != TransportNodeKind::Airfield {
                continue;
            }
            let node_power_row = &node_power[node.id.index()];
            if node_power_row.iter().all(|&p| p <= 0.0) {
                continue;
            }
            let base_pos = world.region(node.region).position;
            if geographic_distance(base_pos, target_pos) > AIR_OPERATING_RADIUS_KM {
                continue;
            }
            for f in 0..n_factions {
                power[f] += node_power_row[f];
            }
        }

        let total: f32 = power.iter().fold(0.0, |acc, &p| acc + p);
        let shares: Vec<AirSuperiority> = if total > 0.0 {
            power
                .iter()
                .map(|&p| AirSuperiority::new(p / total).expect("a share of a positive total lies in 0.0..=1.0"))
                .collect()
        } else {
            vec![AirSuperiority::NEUTRAL; n_factions]
        };
        world.region_mut(region_id).air_superiority = shares;
    }
}
