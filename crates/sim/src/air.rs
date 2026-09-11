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
//!
//! Stage 10C (docs/phase10-spec.md "3. 阻止") wires `Region::air_superiority`
//! into the one thing 10B deliberately left alone: `air_line_factor` below
//! is `naval::sea_line_factor`'s exact shape, one domain further - `1 -` the
//! highest share held, over either of a line's own two endpoint regions, by
//! a faction actually at war with the hauler (`World::hostile_air_
//! superiority_max` - deliberately diplomacy-aware, unlike `naval::
//! strait_factor`'s own raw form; see that function's own doc for why an
//! explicitly hostile act like interdiction needs the distinction a shared
//! sea lane's throttle does not). `logistics::
//! TransportGraph::line_faction_factor` folds this into the very same
//! per-(line, faction) decreasing budget `naval::sea_line_factor` already
//! seeds there once a tick (`residual_line_faction`) - never a second,
//! independently-spent throttle - so air interdiction is `naval`'s own
//! Defect-2 fix applied to a second cause, not a new mechanism: a `Sea` line
//! now answers to *both* sea control and air superiority at once, and every
//! other line kind (Rail/Road, previously never faction-throttled at all)
//! gains this one new cause. Recovery is automatic and requires no separate
//! bookkeeping: `Region::air_superiority` already recomputes to `NEUTRAL`
//! the moment nothing reaches a region (10B's own recovery path), and
//! `line_faction_factor` re-reads it fresh every tick, so a line's capacity
//! is never sampled once and held - it tracks today's airspace, every day.
//!
//! Striking a node (§3 "飛行場と港への攻撃") is a different mechanism from
//! `Region::air_superiority`'s own recovery loop - see `transport::
//! TransportNode::condition`/`transport::tick_node_condition` and `action::
//! apply_strike_node` - but it does feed into this module: `node_air_power`
//! below excludes every unit based at a currently non-`operational` node
//! from `tick_air_superiority`'s committed-power tally (codex review P2,
//! Stage 10C), and `action::apply_recruit`'s `Domain::Air` arm refuses a new
//! recruit there the same way. Both read `TransportNode::operational`
//! fresh, never a value sampled once, so the same automatic repair that
//! reopens supply routing (`transport::tick_node_condition`'s own doc)
//! restores air superiority projection and recruitment the instant
//! `condition` crosses back over `balance::NODE_OPERATIONAL_THRESHOLD` -
//! no second recovery path to keep in sync with the first.

use crate::balance::{
    AIR_OPERATING_RADIUS_KM, ARMS_SUPPLY_NEED_PER_GAP, BROKEN_LOSS_MULT, COMBAT_DAMAGE, COMBAT_SUPPLY_MULT,
    EQUIPMENT_LOSS_PER_DAMAGE, MANPOWER_LOSS_PER_DAMAGE, MORALE_LOSS_PER_BROKEN_HIT, ORG_DAMAGE_MULT,
    SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING, UNIT_EQUIPMENT,
};
use crate::good::Good;
use crate::ids::{FactionId, RegionId, UnitId};
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
    // Stage 11B: an air unit's own equipment commodity, not `Good::Infantry`.
    let w_arms = faction_ref.logistics_priority[Good::Aircraft.index()].max(0.0);
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
/// Stage 10C (codex review P2): a struck `Airfield` node projects no air
/// power at all, no matter how much strength sits on its own tarmac -
/// gated on `transport::TransportNode::operational` exactly the way
/// `logistics::TransportGraph::node_operational` gates supply routing and
/// `World::port_node_operational` gates `trade::tick_imports`, never a
/// second, differently-shaped "is this node working" check of its own
/// (that drift is exactly how the original defect - `apply_recruit` and
/// this function never asking the question at all - survived alongside the
/// port fix in the same stage). Before this, `Action::StrikeNode` lowered
/// `condition` and stopped supply/transport traversal but left a wrecked
/// airfield's own stationed squadrons contributing full `combat_power()` to
/// `tick_air_superiority` below, unchanged and immediately, with no
/// dependence on the attrition their now-cut-off supply would only start
/// inflicting many ticks later.
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
        if !world.transport_node(node).operational() {
            continue;
        }
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
///
/// Stage 10 follow-up: `action::apply_move`'s own `Station::Airfield` arm
/// reuses this exact function to gate how far a squadron may redeploy in one
/// order, against the same `AIR_OPERATING_RADIUS_KM` ceiling this module
/// already reaches for combat power over a region - not a second,
/// independently-invented notion of "how far can this squadron go" (see
/// `balance::AIR_MOVE_DAYS`'s own doc for why ferry range and combat radius
/// share one constant rather than two).
pub(crate) fn geographic_distance(a: [f32; 2], b: [f32; 2]) -> f32 {
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
/// Recomputes `Region::air_superiority` for every region a change at any of
/// `origins` can affect - each origin itself and everything within
/// `AIR_OPERATING_RADIUS_KM` of it, which is exactly the set
/// `tick_air_superiority` would reach from an airfield sitting there.
///
/// **A strike has more than one origin** (`codex review`, P1). The target's
/// own neighbourhood changes because the struck airfield stops projecting,
/// but the attacking squadrons also lose strength, and *their* power radiates
/// from their own bases - which can be a full radius away from the target, so
/// a target-centred refresh alone left regions near the attacker's fields
/// reading power that no longer exists. `strike_origin_regions` collects both.
///
/// Called by `action::apply_strike_node` because that action changes who can
/// fly (a grounded airfield stops projecting) in the middle of an action
/// batch, between two `tick_air_superiority` runs. Scoped to the radius
/// rather than re-running the whole tick sweep: a strike cannot alter the
/// air anywhere its own airfield could not have reached in the first place,
/// and the full sweep is O(regions * nodes) on a map with 289 regions and
/// 532 nodes.
///
/// Deliberately the same power-and-ratio computation `tick_air_superiority`
/// performs, over the same reach test, so the refreshed values and the
/// tick's own agree whenever nothing else has changed - not a second,
/// independently-invented notion of who holds the air. `Vec` indexing and
/// `World::transport_nodes` order throughout, so the float addition order is
/// fixed and the result is identical to what the next tick would compute.
pub fn refresh_air_superiority_near(world: &mut World, origins: &[RegionId]) {
    let n_factions = world.factions.len();
    let node_power = node_air_power(world);
    let origin_positions: Vec<[f32; 2]> = origins.iter().map(|&r| world.region(r).position).collect();

    for region_idx in 0..world.regions.len() {
        let region_id = RegionId(region_idx as u32);
        let target_pos = world.region(region_id).position;
        if !origin_positions.iter().any(|&o| geographic_distance(o, target_pos) <= AIR_OPERATING_RADIUS_KM) {
            continue;
        }

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

/// Stage 10C (docs/phase10-spec.md "3. 阻止": "制空権を取られた地域を通る輸送
/// 路線の実効容量が落ちる"): the air-interdiction throttle a `transport::
/// TransportLine` between `region_a` and `region_b` suffers, from `faction`'s
/// own point of view as the hauler - `World::hostile_air_superiority_max`'s
/// exact shape (`1 -` the highest share a faction *at war with `faction`*
/// holds - see that function's own doc for why this, and not `naval::
/// strait_factor`'s raw "any other faction" form, is the right analogue for
/// an explicitly hostile act), asked at *both* endpoints and folded together
/// with `f32::min` the same way `naval::sea_line_factor` folds together
/// every shared sea zone: a line is only as open as its most-contested end,
/// so dominating either side of a route is enough to throttle it, not just
/// the "shared zone" case sea crossings have.
///
/// Read fresh every call, never cached (`Region::air_superiority`'s own
/// doc): a front whose airspace changed hands after a route was chosen
/// throttles - or reopens - the very same tick
/// (CLAUDE.md「繰り返し踏んだ欠陥」: "発令時点の値を焼き込まない").
pub fn air_line_factor(world: &World, region_a: RegionId, region_b: RegionId, faction: FactionId) -> f32 {
    air_superiority_factor(world, region_a, faction).min(air_superiority_factor(world, region_b, faction))
}

/// The single-region building block `air_line_factor` above folds together
/// over a line's two endpoints - `1 -` the highest `Region::air_superiority`
/// share held by a faction at `Stance::War` with `faction`
/// (`World::hostile_air_superiority_max`), clamped into `0.0..=1.0`. Pulled
/// out under its own name (rather than kept as `air_line_factor`'s private
/// closure) because `apply_strike_node`'s new strike-degradation effect
/// below needs the exact same one-region question `InterdictLine`'s
/// throughput throttle already answers, not a second formula that happens
/// to compute the same thing under a different name (CLAUDE.md's own
/// warning: ports, airfields, and `world.supply` all drifted apart this
/// phase precisely because a second notion of the same fact crept in
/// somewhere).
///
/// Deliberately answers "how much of the sky is *not* held by a hostile
/// faction", not "how much does `faction` itself hold" - the two coincide
/// whenever only one attacker and one defender contest a region (every
/// scenario shipped today), and the former is what already lets a route -
/// or a strike - through when *nobody* (attacker included) has any air unit
/// near the target at all: an uncontested sky needs no escort. This is the
/// same reading `air_line_factor` has used since Stage 10C; the strike
/// effect below inherits it rather than introducing air superiority's
/// second meaning.
pub fn air_superiority_factor(world: &World, region: RegionId, faction: FactionId) -> f32 {
    (1.0 - world.hostile_air_superiority_max(region, faction)).clamp(0.0, 1.0)
}

/// Every region whose air picture a strike by `faction` against `region` can
/// change: the target itself, plus the home region of every airfield the
/// attacker flies this sortie from (those squadrons take losses, so the power
/// they project around their own bases changes too). Feeds
/// `refresh_air_superiority_near` - see its own doc for why one origin is not
/// enough. Ascending `UnitId` order with `Vec` throughout, never a `HashSet`,
/// so the origin list is a pure function of world state.
pub fn strike_origin_regions(world: &World, region: RegionId, faction: FactionId) -> Vec<RegionId> {
    let mut out = vec![region];
    for unit_id in units_reaching(world, region, faction) {
        let Some(node_id) = world.unit(unit_id).station.airfield() else {
            continue;
        };
        let base = world.transport_node(node_id).region;
        if !out.contains(&base) {
            out.push(base);
        }
    }
    out
}

/// Every alive air unit `faction` owns whose own airfield can currently
/// project power onto `region` - `node_air_power`'s own per-node reach test
/// (`transport::TransportNode::operational`, `geographic_distance` within
/// `AIR_OPERATING_RADIUS_KM`), resolved for one `(region, faction)` pair
/// instead of accumulated into that function's whole node-indexed table.
/// This is exactly the reach `tick_air_superiority` already grants when it
/// lets these same units' `combat_power()` count toward `region`'s own
/// `air_superiority` - not a second, independently-invented notion of
/// "can this squadron reach the target".
///
/// Ascending `UnitId` order (`World::units`'s own storage order, never a
/// `HashMap`) - `apply_strike_losses` below folds over this in a fixed
/// order, so the loss distribution never depends on iteration order
/// (CLAUDE.md's own record of a bitwise-inequivalent reassociation flipping
/// an AI decision 300 days later).
///
/// `pub`, not `pub(crate)` (codex review P1): `action::apply_strike_node`
/// reuses this exact function - not a second, independently-written reach
/// test - to reject a strike from a faction with no air unit able to reach
/// the target at all (`ActionError::NoAircraftInRange`), and the game
/// client's `panels::StrikeKind::reason` reuses it a second time, from a
/// different crate, to grey out the same button for the same reason before
/// the click ever reaches `apply_strike_node`. CLAUDE.md's own warning -
/// "two lookups answering the same question differently" - is exactly what
/// having three call sites share one function instead avoids.
pub fn units_reaching(world: &World, region: RegionId, faction: FactionId) -> Vec<UnitId> {
    let target_pos = world.region(region).position;
    let mut out = Vec::new();
    for unit in &world.units {
        if !unit.alive || unit.owner != faction {
            continue;
        }
        let Some(node_id) = unit.station.airfield() else {
            continue;
        };
        let node = world.transport_node(node_id);
        if !node.operational() {
            continue;
        }
        let base_pos = world.region(node.region).position;
        if geographic_distance(base_pos, target_pos) <= AIR_OPERATING_RADIUS_KM {
            out.push(unit.id);
        }
    }
    out
}

/// The counterplay `Action::StrikeNode`'s own doc says did not exist before
/// this: a squadron sent to strike a node in airspace the defender
/// contests takes real losses, proportional to exactly the same hostile
/// share (`World::hostile_air_superiority_max`) that already degrades the
/// strike's own effect above - not a second, independently-tuned notion of
/// "how dangerous is this airspace".
///
/// **Shape.** `defense` (0..1) scales a single raw "damage" figure the same
/// way `military::tick_combat` derives one per battle - `crate::balance::
/// COMBAT_DAMAGE` itself, reused rather than a new constant invented to
/// mean the same thing under a different name: a strike mission flown
/// into airspace fully held by hostile air power (`defense == 1.0`) costs
/// exactly as much raw damage as one day of ordinary ground combat against
/// a defender of equal committed strength; uncontested airspace
/// (`defense == 0.0`) costs nothing, and every value between scales
/// linearly with how much of the sky the enemy holds - the ratio-by-share
/// principle CLAUDE.md §6 requires for any scarce/contested quantity,
/// applied here to risk instead of throughput. That raw damage is then
/// converted to per-unit manpower/organization/equipment/morale loss by
/// `tick_combat`'s own conversion constants (`ORG_DAMAGE_MULT`,
/// `MANPOWER_LOSS_PER_DAMAGE`, `EQUIPMENT_LOSS_PER_DAMAGE`,
/// `BROKEN_LOSS_MULT`, `MORALE_LOSS_PER_BROKEN_HIT`) - the same physical
/// meaning ("this much raw damage does this much harm to a unit")
/// shouldn't be re-derived a second time for a second kind of engagement.
///
/// **Distribution.** Split across every unit `units_reaching` returns, by
/// each one's own `combat_power()` share of their combined total - never a
/// fixed priority (CLAUDE.md §6's first listed defect) and never applied to
/// a unit that has no way to actually be exposed (one based too far away,
/// or grounded at a struck, non-operational airfield, is excluded by
/// `units_reaching` exactly as it is excluded from projecting power in the
/// first place). A striking faction with no unit within reach here loses
/// nothing, because there is nothing of theirs in the sky to lose - the
/// same "no presence, no risk" reading `air_superiority_factor`'s own doc
/// gives the effect side.
///
/// **Recovery.** Every field this touches already has its own recovery
/// path, so none is invented here: `military::tick_recovery` regenerates
/// `organization`/`morale` for a unit not currently fighting or marching,
/// `action::apply_reinforce` restores `equipment`, and `action::
/// apply_disband` always remains available regardless of how depleted the
/// unit is - a mauled squadron is never stuck (CLAUDE.md §6's last listed
/// defect).
pub fn apply_strike_losses(world: &mut World, region: RegionId, faction: FactionId, defense: f32) {
    if defense <= 0.0 {
        return;
    }
    let reaching = units_reaching(world, region, faction);
    let total_power: f32 = reaching
        .iter()
        .fold(0.0, |acc, &id| acc + world.unit(id).combat_power());
    if total_power <= 0.0 {
        return;
    }

    let dmg_total = defense * COMBAT_DAMAGE;
    for &id in &reaching {
        let share = world.unit(id).combat_power() / total_power;
        let dmg = dmg_total * share;

        let unit = world.unit_mut(id);
        unit.organization = (unit.organization - dmg * ORG_DAMAGE_MULT).max(0.0);
        let broken = if unit.organization <= 0.0 { BROKEN_LOSS_MULT } else { 1.0 };
        let manpower_loss = (dmg * MANPOWER_LOSS_PER_DAMAGE * broken).min(unit.manpower);
        unit.manpower -= manpower_loss;
        unit.equipment = (unit.equipment - dmg * EQUIPMENT_LOSS_PER_DAMAGE * broken).max(0.0);
        unit.morale = (unit.morale - MORALE_LOSS_PER_BROKEN_HIT * broken).max(0.0);

        world.faction_mut(faction).casualties += manpower_loss;
    }
}
