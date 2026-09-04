//! Supply network: how far production reaches the front, and how it is
//! rationed to units once there. This is design.md §8's core loop — cutting
//! a single corridor region starves everything behind it.

use std::collections::VecDeque;

use crate::balance::{
    ARMS_SUPPLY_NEED_PER_GAP, PROJECTED_SUPPLY_FACTOR, SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING,
    UNIT_EQUIPMENT,
};
use crate::good::Good;
use crate::ids::RegionId;
use crate::naval;
use crate::world::{LinkKind, World};

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
    // Stage 2D (docs/phase2-spec.md "2. 港の封鎖"): a blockaded port's own
    // contribution to its region's supply base is zeroed here, independent
    // of `contested` — a region can be blockaded without a single enemy
    // land unit ever setting foot on it.
    let blockaded: Vec<bool> = (0..n)
        .map(|i| naval::is_port_blockaded(world, world.regions[i].id))
        .collect();
    let node_throughput: Vec<f32> = world.regions.iter().map(|r| r.node_throughput()).collect();

    let mut cap: Vec<f32> = world
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| r.supply_source_blockaded(blockaded[i]))
        .collect();

    // Stage 6C (docs/phase6-spec.md "Stage 6C"): `cap` only ever grows
    // during this relaxation (`if v > cap[j]`) and each region's outgoing
    // contribution is a pure function of its own current `cap[i]`, so the
    // fixed point this converges to does not depend on the order regions
    // are visited in — only on having visited every region enough times
    // for its cap to stop changing. That means a worklist (process a
    // region only when *its own* cap has just grown, propagate to its
    // same-owner neighbors, and re-enqueue any of them whose cap grows in
    // turn) reaches the exact same fixed point as the old fixed `0..n`
    // sweep, but without re-scanning every region on every round — at 47
    // regions this is the single biggest per-tick cost (docs/phase6-spec.md
    // §0's O(n²)-ish risk). Every region starts in the queue once (its own
    // `supply_source`/blockade contribution is itself a "change" from the
    // implicit zero the relaxation begins from); a contested region is
    // never enqueued or re-enqueued, matching the old loop's `if
    // contested[i] { continue }` — it still receives whatever a
    // non-contested neighbor relays into it, it just never relays onward.
    let mut in_queue = vec![false; n];
    let mut queue: VecDeque<usize> = VecDeque::with_capacity(n);
    for i in 0..n {
        if !contested[i] {
            queue.push_back(i);
            in_queue[i] = true;
        }
    }

    while let Some(i) = queue.pop_front() {
        in_queue[i] = false;
        let owner_i = world.regions[i].owner;
        let cap_i = cap[i];
        for link in &world.regions[i].links {
            let j = link.to.index();
            if world.regions[j].owner != owner_i {
                continue;
            }
            // Stage 2D (docs/phase2-spec.md "1. 海峡リンクの遮断"): a
            // `Strait` link's throughput is throttled by the highest
            // sea control any other faction holds in the zone it
            // crosses — `None` (every non-Strait link, and the 中国—
            // 九州 `Tunnel` deliberately) is unaffected.
            let strait = match link.strait_zone {
                Some(zone) => naval::strait_factor(world, zone, owner_i),
                None => 1.0,
            };
            let infra_j = world.regions[j].effective_infrastructure();
            let v = (cap_i * link.kind.retention() * (0.55 + 0.45 * infra_j))
                .min(link.kind.max_throughput() * strait)
                .min(node_throughput[j]);
            if v > cap[j] {
                cap[j] = v;
                if !contested[j] && !in_queue[j] {
                    queue.push_back(j);
                    in_queue[j] = true;
                }
            }
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
///
/// Stage 2D (docs/phase2-spec.md "艦隊の補給"): fleets draw on the exact
/// same national `Faction::stock[Munitions]` pool land units do, so their
/// demand (`naval::fleet_demand_and_avail`, per sea zone rather than per
/// region) is folded into this same pass and shares the same
/// stock-limited `scale[f]` — computed once, from land's and sea's combined
/// `total_served`/`total_demand` — before either domain's stock is
/// deducted. Running the sea pass afterward against whatever land left in
/// stock would give land an unconditional first claim on the shared pool,
/// exactly the hardcoded-precedence defect shape this project keeps finding
/// and fixing (see this function's own doc above for the land-side version
/// of the same rule).
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
        let Some(region) = unit.station.region() else {
            continue; // fleets are handled by the sea-zone pass below
        };
        let r = region.index();
        let f = unit.owner.index();
        let in_combat = world.has_enemy_units(region, unit.owner);
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
            if avail[r][f] <= 0.0 {
                continue;
            }
            let faction = &world.factions[f];
            let w_munitions = faction.logistics_priority[Good::Munitions.index()];
            let w_arms = faction.logistics_priority[Good::Arms.index()];
            let (m, a) =
                split_munitions_arms(avail[r][f], w_munitions, w_arms, demand_munitions[r][f], demand_arms[r][f]);
            served_munitions[r][f] = m;
            served_arms[r][f] = a;
        }
    }

    // Stage 2D: the sea-zone counterpart of the region-indexed arrays above,
    // split by the same priority weights via the same shared helper.
    let (demand_munitions_zone, demand_arms_zone, avail_zone) = naval::fleet_demand_and_avail(world);
    let n_zones = world.sea_zones.len();
    let mut served_munitions_zone = vec![vec![0.0f32; n_factions]; n_zones];
    let mut served_arms_zone = vec![vec![0.0f32; n_factions]; n_zones];
    for z in 0..n_zones {
        for f in 0..n_factions {
            if avail_zone[z][f] <= 0.0 {
                continue;
            }
            let faction = &world.factions[f];
            let w_munitions = faction.logistics_priority[Good::Munitions.index()];
            let w_arms = faction.logistics_priority[Good::Arms.index()];
            let (m, a) = split_munitions_arms(
                avail_zone[z][f],
                w_munitions,
                w_arms,
                demand_munitions_zone[z][f],
                demand_arms_zone[z][f],
            );
            served_munitions_zone[z][f] = m;
            served_arms_zone[z][f] = a;
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
    for z in 0..n_zones {
        for f in 0..n_factions {
            total_served[f] += served_munitions_zone[z][f];
            total_demand[f] += demand_munitions_zone[z][f];
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
        let Some(region) = unit.station.region() else {
            continue; // fleets are finished off by naval::apply_fleet_supply below
        };
        let r = region.index();
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
        // delivers equipment; stamping `arms_delivery_station` alongside it
        // records which station this budget was computed for, so a unit
        // that moves before its next `ReinforceUnit` action is detected as
        // stale and recomputed on the spot instead of trusted -
        // `logistics::instantaneous_arms_delivery`.
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        unit.arms_budget = equipment_gap * unit.arms_delivery;
        unit.arms_delivery_station = unit.station;
    }

    naval::apply_fleet_supply(
        world,
        &served_munitions_zone,
        &demand_munitions_zone,
        &served_arms_zone,
        &demand_arms_zone,
        &scale,
    );
}

/// Splits `total_avail` throughput between Munitions and Arms demand by
/// `logistics_priority` weight (falling back to an even split if both
/// weights are zero), then hands back to the other good whatever share
/// either good's priority left unused, in proportion to its own remaining
/// unmet demand — so a good with no demand this tick doesn't ring-fence
/// throughput a hungry demand could actually use, and neither good ever
/// gets a hardcoded first claim on the leftover. Shared by `distribute_supply`'s
/// land (per-region) and sea (per-zone, via `naval::fleet_demand_and_avail`)
/// passes — the pool being split is the same contention in both cases, only
/// the place it's keyed by differs.
fn split_munitions_arms(
    total_avail: f32,
    w_munitions: f32,
    w_arms: f32,
    demand_munitions: f32,
    demand_arms: f32,
) -> (f32, f32) {
    let w_munitions = w_munitions.max(0.0);
    let w_arms = w_arms.max(0.0);
    let w_sum = w_munitions + w_arms;
    let (share_munitions_frac, share_arms_frac) = if w_sum > 0.0 {
        (w_munitions / w_sum, w_arms / w_sum)
    } else {
        (0.5, 0.5)
    };
    let share_munitions = total_avail * share_munitions_frac;
    let share_arms = total_avail * share_arms_frac;

    let served_m1 = demand_munitions.min(share_munitions);
    let served_a1 = demand_arms.min(share_arms);

    let leftover = (share_munitions - served_m1) + (share_arms - served_a1);
    let remaining_m = demand_munitions - served_m1;
    let remaining_a = demand_arms - served_a1;
    let remaining_total = remaining_m + remaining_a;
    let (extra_m, extra_a) = if remaining_total > 0.0 && leftover > 0.0 {
        let extra = leftover.min(remaining_total);
        (extra * remaining_m / remaining_total, extra * remaining_a / remaining_total)
    } else {
        (0.0, 0.0)
    };

    (served_m1 + extra_m, served_a1 + extra_a)
}

/// External code review fix (Stage 2C): recomputes a single land unit's Arms
/// delivery ratio and per-tick budget from its *current* region, for
/// `action::apply_reinforce` to call when the unit's cached
/// `Unit::arms_delivery_station` no longer matches `Unit::station` -
/// `distribute_supply` above only runs once a tick, before movement, so a
/// unit that has since moved carries numbers stamped from a region it has
/// already left. Land only — `apply_reinforce` calls
/// `naval::instantaneous_fleet_arms_delivery` instead for a fleet.
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

    let region = unit
        .station
        .region()
        .expect("instantaneous_arms_delivery is land-only; callers must route fleets to naval::instantaneous_fleet_arms_delivery");
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

// ---------------------------------------------------------------------
// Stage 7C (docs/phase7-spec.md "1. 補給網の可視化"): read-only display
// support. Nothing below this line is ever called from `Simulation::step`
// or any tick system, and nothing here writes to `World` - it only ever
// reads `world.supply` (already computed by `recompute_supply` this tick)
// and re-derives, per region/link, the same numbers that produced it.
// ---------------------------------------------------------------------

/// A same-owner link's actual relayed throughput this tick against its own
/// ceiling (`LinkKind::max_throughput() * strait_factor`). `flow` is
/// clamped into `0.0..=capacity` at construction, so this type can never
/// represent "flowing more than its own cap allows" - `ratio`/
/// `is_saturated` are derived from the two stored numbers, never cached
/// separately, so they can't drift out of sync with them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LinkThroughput {
    flow: f32,
    capacity: f32,
}

/// How close to its own ceiling counts as "stuck there" (docs/phase7-spec.md
/// "1.": "上限に張り付いているリンクを明示する") - guards against float
/// noise in the `min()` chain `recompute_supply`'s own per-link formula
/// runs, not a design threshold of its own.
const SATURATION_EPSILON: f32 = 1e-3;

impl LinkThroughput {
    fn new(flow: f32, capacity: f32) -> Self {
        let capacity = capacity.max(0.0);
        LinkThroughput { flow: flow.clamp(0.0, capacity), capacity }
    }

    pub fn flow(self) -> f32 {
        self.flow
    }

    pub fn capacity(self) -> f32 {
        self.capacity
    }

    /// `flow / capacity`, `0.0` for a link with no capacity at all (should
    /// not occur - every `LinkKind::max_throughput()` is positive - but a
    /// division by exactly `0.0` is still not something to hand back as a
    /// ratio).
    pub fn ratio(self) -> f32 {
        if self.capacity > 0.0 {
            self.flow / self.capacity
        } else {
            0.0
        }
    }

    /// A chokepoint: this link's flow has reached its own ceiling, so
    /// widening whatever's upstream of it would do nothing - the ceiling
    /// itself is what's holding supply back.
    pub fn is_saturated(self) -> bool {
        self.capacity > 0.0 && self.flow >= self.capacity - SATURATION_EPSILON
    }
}

/// Which same-owner neighbor (if any) is relaying enough to explain a
/// region's `world.supply` entry - the "供給がどの経路で来ているか"
/// docs/phase7-spec.md asks the overlay to reconstruct. Never a guess: see
/// `region_route`'s doc for the exact identity this is read off of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SupplySource {
    /// This region's own industry/port base (`Region::supply_source_blockaded`)
    /// is at least as large as anything any same-owner, non-contested
    /// neighbor currently relays in - there is no route to draw.
    Own,
    /// This same-owner neighbor's relay is what the region's throughput is
    /// actually attributable to.
    Relay(RegionId),
}

/// One region's reconstructed place in the supply network, alongside the
/// raw `contested`/`blockaded` reads `recompute_supply` itself judged it
/// by - a region that can't currently relay onward (`contested`) still
/// shows here with whatever it receives; `region_route` never invents a
/// value for it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SupplyRegionRoute {
    pub region: RegionId,
    /// `world.supply[region]` - repeated here so a caller can read
    /// everything about one region off a single value.
    pub cap: f32,
    pub contested: bool,
    pub blockaded: bool,
    pub own_source: f32,
    pub source: SupplySource,
}

/// Reconstructs every region's `SupplyRegionRoute` and every currently-
/// relay-capable same-owner link's `LinkThroughput`, purely by re-reading
/// `world.supply` (already populated by `recompute_supply` this tick) - see
/// `supply_route_reconstruction_matches_logistics` for why this is
/// guaranteed to agree with it, not merely tested to.
///
/// `recompute_supply`'s relaxation is a monotone fixed point: each region's
/// `cap` only ever grows, each link's contribution to its target is a
/// non-decreasing function of its source's own `cap`, and the loop keeps
/// re-visiting a region's outgoing links every time that region's `cap`
/// grows until nothing changes anywhere. A monotone relaxation like that
/// always converges to exactly `cap[j] == max(own_source[j], max over every
/// same-owner, non-contested-source neighbor i of that link's own formula
/// applied to i's *final* cap[i])` - regardless of what order the queue
/// happened to visit regions in. So this function does not need to replay
/// that queue (and risk drifting from it if the two implementations were
/// ever edited out of step): it recomputes the same per-link formula
/// directly from the already-converged `world.supply`, and the result is
/// the same maximum by construction.
pub fn supply_routes(world: &World) -> Vec<SupplyRegionRoute> {
    let n = world.regions.len();
    let contested: Vec<bool> = (0..n)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();
    let blockaded: Vec<bool> = (0..n).map(|i| naval::is_port_blockaded(world, world.regions[i].id)).collect();
    let node_throughput: Vec<f32> = world.regions.iter().map(|r| r.node_throughput()).collect();

    (0..n)
        .map(|j| {
            let region_j = &world.regions[j];
            let own_source = region_j.supply_source_blockaded(blockaded[j]);
            let infra_j = region_j.effective_infrastructure();

            let mut best_source = SupplySource::Own;
            let mut best_value = own_source;
            for link in &region_j.links {
                let i = link.to.index();
                if world.regions[i].owner != region_j.owner || contested[i] {
                    continue; // exactly `recompute_supply`'s own relay eligibility check
                }
                let strait = match link.strait_zone {
                    Some(zone) => naval::strait_factor(world, zone, region_j.owner),
                    None => 1.0,
                };
                let v = (world.supply[i] * link.kind.retention() * (0.55 + 0.45 * infra_j))
                    .min(link.kind.max_throughput() * strait)
                    .min(node_throughput[j]);
                if v > best_value {
                    best_value = v;
                    best_source = SupplySource::Relay(link.to);
                }
            }

            SupplyRegionRoute {
                region: region_j.id,
                cap: world.supply[j],
                contested: contested[j],
                blockaded: blockaded[j],
                own_source,
                source: best_source,
            }
        })
        .collect()
}

/// One directed, currently-relay-capable same-owner link's throughput this
/// tick - `from` is the relaying source, `to` the region it feeds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SupplyLinkFlow {
    pub from: RegionId,
    pub to: RegionId,
    pub kind: LinkKind,
    pub throughput: LinkThroughput,
}

/// Every same-owner, currently-relay-capable directed link's
/// `LinkThroughput` this tick - one entry per direction a source region
/// could actually relay in (contested sources are skipped entirely, same
/// as `supply_routes`/`recompute_supply` - there is nothing to compute a
/// flow for on a link out of a region that cannot relay at all). A
/// bidirectional link pair with both endpoints eligible appears as two
/// entries, since the two directions can genuinely disagree (different
/// target infrastructure, different node throughput, different upstream
/// `cap`).
pub fn supply_link_flows(world: &World) -> Vec<SupplyLinkFlow> {
    let n = world.regions.len();
    let contested: Vec<bool> = (0..n)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();
    let node_throughput: Vec<f32> = world.regions.iter().map(|r| r.node_throughput()).collect();

    let mut out = Vec::new();
    for j in 0..n {
        let region_j = &world.regions[j];
        let infra_j = region_j.effective_infrastructure();
        for link in &region_j.links {
            let i = link.to.index();
            if world.regions[i].owner != region_j.owner || contested[i] {
                continue;
            }
            let strait = match link.strait_zone {
                Some(zone) => naval::strait_factor(world, zone, region_j.owner),
                None => 1.0,
            };
            let capacity = link.kind.max_throughput() * strait;
            let flow = (world.supply[i] * link.kind.retention() * (0.55 + 0.45 * infra_j))
                .min(capacity)
                .min(node_throughput[j]);
            out.push(SupplyLinkFlow { from: link.to, to: region_j.id, kind: link.kind, throughput: LinkThroughput::new(flow, capacity) });
        }
    }
    out
}
