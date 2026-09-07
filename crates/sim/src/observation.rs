//! What an agent gets to see: its own faction's view of the world, plus a
//! flat float encoding for RL-style consumers.

use std::collections::VecDeque;

use crate::diplomacy::Treaty;
use crate::good::GOOD_COUNT;
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::logistics;
use crate::naval;
use crate::transport::TransportNodeKind;
use crate::world::{Domain, Station, World};

/// Per-region field count in `Observation::encode()`: `[owned, population,
/// infrastructure, supply, unrest, own_power, enemy_power]` (7 fixed
/// fields) followed by `capacity[GOOD_COUNT]`, then `[devastation,
/// construction_progress]` (Stage 2B, 2 fixed fields), then `[import_flow,
/// node_throughput]` (Stage 2C, 2 fixed fields), then `[own_air_superiority,
/// enemy_air_superiority_max]` (Stage 10D, docs/phase10-spec.md "Stage 10D":
/// "観測に...制空権が出る", 2 fixed fields) - `Region::air_superiority`'s own
/// share for this observing faction, then the highest share any *other*
/// faction holds (`Region::enemy_air_superiority_max`, the region-domain
/// mirror of `SeaZone::enemy_control_max` just below - same "any other
/// faction, war or peace" semantics, not `World::hostile_air_superiority_max`'s
/// war-gated one, which is a different, throttle-specific question).
pub const REGION_FIELD_COUNT: usize = 7 + GOOD_COUNT + 2 + 2 + 2;

/// Per-sea-zone field count in `Observation::encode()` (Stage 2D):
/// `[own_control, enemy_control_max, own_power, enemy_power]`.
pub const SEA_ZONE_FIELD_COUNT: usize = 4;

/// Faction-scalar field count in `Observation::encode()`: `manpower`,
/// `stock[GOOD_COUNT]`, `war_support`, `stability`,
/// `group_support[GROUP_COUNT]` (Stage 3A), `unit_count`,
/// `[national_focus_code, focus_transition_days]` (Stage 3C), then
/// `air_unit_count` (Stage 10D, 1 trailing field) - the `Domain::Air` slice
/// of `unit_count` (which already silently included air units once they
/// existed at all - `Observation::own_units()` filters only on
/// owner/`alive`, never on domain), broken out on its own since air power
/// isn't a raw regional/zone power addend the way land/sea combat power is
/// (`Observation::power_tables`'s own doc) - this is the one faction-level
/// place an agent can read "how many squadrons do I have" without deriving
/// it from the per-node transport-network segment below.
pub const FACTION_FIELD_COUNT: usize = 4 + GOOD_COUNT + GROUP_COUNT + 2 + 1;

/// Stage 3B per-relation field count in `Observation::encode()`, one block
/// per *other* faction (own row zeroed - see `encode`'s doc): `[stance_code,
/// opinion, military_access, port_access, trade_agreement,
/// pending_incoming, pending_incoming_treaty, pending_outgoing,
/// pending_outgoing_treaty]`. `stance_code` is `Stance::index()` as an
/// `f32`; the `*_treaty` fields are `Treaty::index()` as an `f32`, or `-1.0`
/// when there's no pending proposal in that direction.
pub const DIPLOMACY_FIELD_COUNT: usize = 9;

/// Stage 9D (docs/phase9-spec.md "4. 観測ベクトル": "路線ごとに出す... 少なく
/// とも `capacity` / `condition` / 現在の流量 / 自勢力が使えるか"): per-line
/// field count in `Observation::encode()`, in `world.transport_lines` order
/// (`ids::TransportLineId`) - `[capacity, condition, flow, usable_by_self]`.
/// `capacity` is the line's raw declared `Capacity` (before `condition`/
/// devastation scale it down - a consumer that wants the current ceiling
/// multiplies the two fields itself, the same way `TransportLine::
/// effective_capacity` does internally); `flow` is this tick's committed
/// throughput in both directions combined (`logistics::
/// transport_line_flows`); `usable_by_self` is `1.0` only when *both*
/// endpoint regions currently belong to the observing faction - the same
/// same-owner gate `logistics::compute_transport_flow` itself enforces
/// before a line can carry any flow at all, so this tells an agent, without
/// re-deriving it, whether flow reading `0.0` here means "cut/contested" or
/// "not even yours to route across".
pub const TRANSPORT_LINE_FIELD_COUNT: usize = 4;

/// Stage 9D (docs/phase9-spec.md "4. 観測ベクトル": "ノードについても封鎖・
/// 所属を出す"): per-node field count, in `world.transport_nodes` order
/// (`ids::TransportNodeId`) - `[owned_by_self, blockaded, condition, kind]`.
/// `owned_by_self` mirrors the per-region "owned" flag's own convention
/// (`encode`'s per-region loop); `blockaded` is `naval::is_port_blockaded`
/// for the node's own region - `false` for every non-`Port` node and for an
/// unblockaded port.
///
/// `condition` (Stage 10D, docs/phase10-spec.md "Stage 10D") is
/// `TransportNode::condition`'s own raw `0.0..=1.0` health - carried over
/// from a P1 the same Stage 10D pass found while adding air observability:
/// before this, an external agent could issue `Action::StrikeNode` but had
/// no way to see whether its target was already wrecked (or had since
/// repaired), and `GET /state` had the identical gap (`state::regions_value`'s
/// own fix). Raw, not the thresholded `operational` bool
/// (`balance::NODE_OPERATIONAL_THRESHOLD`) - the exact same choice
/// `TransportLine::condition`'s own `condition` field already made for lines,
/// so a consumer that wants "is it currently usable" derives it the same way
/// either place, and one that wants the continuous "how close to
/// repaired/wrecked" signal isn't reduced to a single bit.
///
/// `kind` (Stage 10D, `codex review` P1) is `TransportNodeKind::index()` as
/// an `f32` - added after an external review of this same stage found
/// `blockaded`'s original doc reasoning ("a consumer that doesn't already
/// know this node's kind gains nothing from a third state") had quietly
/// generalized into "kind is never encoded here at all", which broke this
/// stage's own "airfields must be observable" requirement: without it, a
/// flat-vector consumer had `owned_by_self`/`blockaded`/`condition` for
/// every node but no way to tell *which* node is an airfield (or a port) at
/// all, short of parsing the scenario JSON out of band - exactly what this
/// vector exists to avoid. See `TransportNodeKind::index()`'s own doc for why
/// this is a stable numeric code and not a fifth boolean.
pub const TRANSPORT_NODE_FIELD_COUNT: usize = 2 + 1 + 1;

/// `Observation::encode()`'s output length for a scenario with the given
/// region/sea-zone/faction/transport-line/transport-node counts - the
/// general form of `ENCODING_LEN` below. Stage 6A (docs/phase6-spec.md
/// "Stage 6A"): scenario data is no longer fixed at compile time
/// (`--scenario` can load a differently-sized map), so `encode()` itself
/// computes its expected length this way rather than trusting the
/// compile-time `ENCODING_LEN` constant, which only ever describes the
/// embedded default scenario.
pub const fn encoding_len(
    region_count: usize,
    sea_zone_count: usize,
    faction_count: usize,
    transport_line_count: usize,
    transport_node_count: usize,
) -> usize {
    region_count * REGION_FIELD_COUNT
        + sea_zone_count * SEA_ZONE_FIELD_COUNT
        + FACTION_FIELD_COUNT
        + faction_count * DIPLOMACY_FIELD_COUNT
        + transport_line_count * TRANSPORT_LINE_FIELD_COUNT
        + transport_node_count * TRANSPORT_NODE_FIELD_COUNT
}

/// Fixed total length of `Observation::encode()`'s output for the embedded
/// default scenario (`scenario::REGION_COUNT` regions,
/// `scenario::SEA_ZONE_COUNT` sea zones, `scenario::FACTION_COUNT`
/// factions, `scenario::TRANSPORT_LINE_COUNT`/`TRANSPORT_NODE_COUNT` transport
/// lines/nodes) - i.e. `scenarios/mvp.json`. A `--scenario`-loaded world with
/// different counts has a different real length; compute it with
/// `encoding_len` from that world's actual sizes instead of assuming this
/// constant, the same way `encode()` itself does.
///
/// Stage 9D grew this length (docs/phase9-spec.md "4. 観測ベクトル":
/// "観測長は伸びる...保存済みの方策は無効になる。これは受け入れる") - a
/// policy trained against the pre-Stage-9D length is no longer valid; `GET
/// /schema`'s `observation.length` is what a live client should read
/// instead of assuming this constant never moves. Stage 10D grows it again
/// the same way (`REGION_FIELD_COUNT`/`FACTION_FIELD_COUNT`/
/// `TRANSPORT_NODE_FIELD_COUNT`'s own docs) - accepted for the same reason.
pub const ENCODING_LEN: usize = encoding_len(
    crate::scenario::REGION_COUNT,
    crate::scenario::SEA_ZONE_COUNT,
    crate::scenario::FACTION_COUNT,
    crate::scenario::TRANSPORT_LINE_COUNT,
    crate::scenario::TRANSPORT_NODE_COUNT,
);

pub struct Observation<'a> {
    pub faction: FactionId,
    pub world: &'a World,
}

impl<'a> Observation<'a> {
    pub fn own_regions(&self) -> Vec<RegionId> {
        self.world.regions_of(self.faction)
    }

    /// Own regions that border at least one region owned by another faction.
    pub fn front_regions(&self) -> Vec<RegionId> {
        self.own_regions()
            .into_iter()
            .filter(|&r| {
                self.world
                    .neighbors(r)
                    .any(|n| self.world.region(n).owner != self.faction)
            })
            .collect()
    }

    pub fn own_units(&self) -> Vec<UnitId> {
        self.world
            .units
            .iter()
            .filter(|u| u.alive && u.owner == self.faction)
            .map(|u| u.id)
            .collect()
    }

    pub fn enemy_power(&self, region: RegionId) -> f32 {
        self.world
            .factions
            .iter()
            .filter(|f| f.id != self.faction)
            .fold(0.0, |acc, f| acc + self.world.region_power(region, f.id))
    }

    pub fn own_power(&self, region: RegionId) -> f32 {
        self.world.region_power(region, self.faction)
    }

    /// Stage 2D sea-domain counterparts of `enemy_power`/`own_power`.
    pub fn enemy_zone_power(&self, zone: SeaZoneId) -> f32 {
        self.world
            .factions
            .iter()
            .filter(|f| f.id != self.faction)
            .fold(0.0, |acc, f| acc + self.world.zone_power(zone, f.id))
    }

    pub fn own_zone_power(&self, zone: SeaZoneId) -> f32 {
        self.world.zone_power(zone, self.faction)
    }

    /// Next hop on a breadth-first path from `from` to `to`, staying within
    /// this faction's own territory (the destination itself is always
    /// allowed even if not owned). Returns `None` if unreachable.
    pub fn path_next(&self, from: RegionId, to: RegionId) -> Option<RegionId> {
        if from == to {
            return None;
        }
        let n = self.world.regions.len();
        let mut visited = vec![false; n];
        let mut prev = vec![None; n];
        let mut queue = VecDeque::new();
        visited[from.index()] = true;
        queue.push_back(from);

        while let Some(current) = queue.pop_front() {
            if current == to {
                break;
            }
            for next in self.world.neighbors(current) {
                let allowed = next == to || self.world.region(next).owner == self.faction;
                if allowed && !visited[next.index()] {
                    visited[next.index()] = true;
                    prev[next.index()] = Some(current);
                    queue.push_back(next);
                }
            }
        }

        if !visited[to.index()] {
            return None;
        }
        let mut step = to;
        while let Some(p) = prev[step.index()] {
            if p == from {
                return Some(step);
            }
            step = p;
        }
        None
    }

    /// One `O(units)` pass precomputing every faction's combat power per
    /// region and per sea zone - what `encode()`'s per-region/per-sea-zone
    /// loop used before this fix: `own_power`/`enemy_power` (and their
    /// sea-zone counterparts) each do their own `O(units)` scan
    /// (`World::region_power`/`zone_power` filters `world.units` by
    /// station), and `enemy_power` additionally loops that scan once *per
    /// other faction*. Calling them once per region turned `encode()`'s
    /// dominant cost into `O(regions × factions × units)` - confirmed by
    /// profiling: on `scenarios/japan_hex.json` (289 regions, 8 factions)
    /// this term alone accounted for ~90% of `encode()`'s wall time even at
    /// day 0 with as few as 24 units, and grew ~9.3x against
    /// `scenarios/japan47.json`'s 47-region/6-faction map versus the map's
    /// own 5.6x/6.15x growth in encoded length/region count - the
    /// super-linear cost `docs/phase8-spec.md`'s Fix 1 asks to find and fix
    /// (the per-tick simulation itself stayed sub-linear; Stage 6C's
    /// diplomacy fix was a different instance of the same shape, in
    /// `crates/agents`).
    ///
    /// Returns `region_power`/`zone_power`, each indexed
    /// `[region_or_zone.index()][faction.index()]` - the exact per-
    /// (region-or-zone, faction) subtotal `World::region_power`/`zone_power`
    /// would themselves compute (same units, same array-order accumulation
    /// per cell - see below), just computed for every cell in one pass over
    /// `world.units` (`O(units)`) instead of one filtered scan per cell
    /// (`O(units)` *each*, `O(regions × factions)` or `O(zones × factions)`
    /// of them). `encode()` then reduces `own_power`/`enemy_power` from
    /// these tables in `O(factions)` per region/zone - `O(regions × factions
    /// + zones × factions)` total, still far below the `O(regions × factions
    /// × units)` this replaces, and, critically, *bit-for-bit identical* to
    /// calling `own_power`/`enemy_power` directly: `enemy_power(r)` folds
    /// `region_power[r][f]` over every other faction in ascending
    /// `FactionId` order, the exact same fold `World::region_power`'s own
    /// `Observation::enemy_power` performs, over the exact same per-cell
    /// values - not derived by subtracting a combined total, which would
    /// reassociate the underlying floating-point sum (a different result in
    /// general, per docs/conventions.md §5's fixed-accumulation-order rule,
    /// even where it happens not to move any single scenario's hash).
    /// `own_power(r)` is simply `region_power[r][self.faction.index()]`,
    /// the same per-cell subtotal `World::region_power` computes directly.
    ///
    /// `own_power`/`enemy_power`/`own_zone_power`/`enemy_zone_power`
    /// themselves are left as they were: `crates/agents`' AI still calls
    /// them directly, once per candidate target it's actually scoring (a
    /// small, front-line-bounded count per decision, not once per region on
    /// the whole map), which profiling found is not the super-linear cost
    /// here.
    fn power_tables(&self) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let n_factions = self.world.factions.len();
        let mut region_power = vec![vec![0.0f32; n_factions]; self.world.regions.len()];
        let mut zone_power = vec![vec![0.0f32; n_factions]; self.world.sea_zones.len()];
        for unit in &self.world.units {
            if !unit.alive {
                continue;
            }
            let power = unit.combat_power();
            match unit.station {
                Station::Region(r) => region_power[r.index()][unit.owner.index()] += power,
                Station::Sea(z) => zone_power[z.index()][unit.owner.index()] += power,
                // Stage 10A ships no observation exposure for air units yet
                // (docs/phase10-spec.md's own staging leaves "観測に航空部隊
                // ... が出る" to Stage 10D) - deliberately not folded into
                // either table, since air power isn't a raw regional/zone
                // addend the way land/sea combat power is (10B's operational
                // radius is a distance-based effect, not a presence sum).
                Station::Airfield(_) => {}
            }
        }
        (region_power, zone_power)
    }

    /// `enemy_power`/`enemy_zone_power`'s fold, reading from a precomputed
    /// `power_tables()` cell-row instead of re-scanning `world.units` per
    /// other faction - see `power_tables`'s doc for why this is bit-for-bit
    /// identical to calling `enemy_power`/`enemy_zone_power` directly.
    fn enemy_power_from_table(&self, cell_row: &[f32]) -> f32 {
        self.world
            .factions
            .iter()
            .filter(|f| f.id != self.faction)
            .fold(0.0, |acc, f| acc + cell_row[f.id.index()])
    }

    /// Length `encoding_len(regions.len(), sea_zones.len(), factions.len())`
    /// (`ENCODING_LEN` for the embedded default scenario specifically):
    /// per-region `[owned, population, infrastructure, supply, unrest,
    /// own_power, enemy_power, capacity[GOOD_COUNT]..., devastation,
    /// construction_progress, import_flow, node_throughput]`, then
    /// per-sea-zone (Stage 2D) `[own_control, enemy_control_max, own_power,
    /// enemy_power]`, then faction scalars `[manpower, stock[GOOD_COUNT]...,
    /// war_support, stability, group_support[GROUP_COUNT]..., unit_count,
    /// national_focus_code, focus_transition_days]` (Stage 3A adds
    /// `group_support`; Stage 3C adds the trailing pair -
    /// `national_focus_code` is `NationalFocus::index()` as an `f32`,
    /// regardless of whether a switch is still transitioning - a consumer
    /// that needs "is it actually active" must additionally check
    /// `focus_transition_days == 0`), then one Stage 3B
    /// `DIPLOMACY_FIELD_COUNT`-sized relation block per faction (own row
    /// zeroed - see the loop below). `construction_progress` is
    /// `invested / required` in `0..=1`, or `0.0` when no project is in
    /// progress. `import_flow`/`node_throughput` are Stage 2C's per-port
    /// import volume and per-node supply throughput cap
    /// (`trade::tick_imports`, `Region::node_throughput`).
    pub fn encode(&self) -> Vec<f32> {
        let expected_len = encoding_len(
            self.world.regions.len(),
            self.world.sea_zones.len(),
            self.world.factions.len(),
            self.world.transport_lines.len(),
            self.world.transport_nodes.len(),
        );
        let mut out = Vec::with_capacity(expected_len);
        let (region_power, zone_power) = self.power_tables();
        for region in &self.world.regions {
            out.push(if region.owner == self.faction { 1.0 } else { 0.0 });
            out.push(region.population);
            out.push(region.infrastructure);
            out.push(self.world.supply[region.id.index()]);
            out.push(region.unrest);
            let cell_row = &region_power[region.id.index()];
            out.push(cell_row[self.faction.index()]);
            out.push(self.enemy_power_from_table(cell_row));
            for g in 0..GOOD_COUNT {
                out.push(region.capacity[g]);
            }
            out.push(region.devastation);
            let progress = match &region.construction {
                Some(c) if c.required > 0.0 => (c.invested / c.required).clamp(0.0, 1.0),
                _ => 0.0,
            };
            out.push(progress);
            out.push(region.import_flow);
            out.push(region.node_throughput());
            // Stage 10D (docs/phase10-spec.md "Stage 10D"): this observing
            // faction's own share of `region.air_superiority`, then the
            // highest share any other faction holds - `own_control`/
            // `enemy_control_max`'s exact pattern just below, one domain
            // earlier. `.get(...).unwrap_or(0.0)` mirrors `own_control`'s own
            // defensive read rather than indexing directly, for the same
            // reason: nothing here should panic if a scenario's faction count
            // and a region's `air_superiority` length were ever to disagree.
            let own_air = region.air_superiority.get(self.faction.index()).map(|s| s.get()).unwrap_or(0.0);
            out.push(own_air);
            out.push(region.enemy_air_superiority_max(self.faction));
        }
        for zone in &self.world.sea_zones {
            let own_control = zone.control.get(self.faction.index()).copied().unwrap_or(0.0);
            out.push(own_control);
            out.push(zone.enemy_control_max(self.faction));
            let cell_row = &zone_power[zone.id.index()];
            out.push(cell_row[self.faction.index()]);
            out.push(self.enemy_power_from_table(cell_row));
        }
        let faction = self.world.faction(self.faction);
        out.push(faction.manpower);
        for g in 0..GOOD_COUNT {
            out.push(faction.stock[g]);
        }
        out.push(faction.war_support);
        out.push(faction.stability);
        for g in 0..GROUP_COUNT {
            out.push(faction.group_support[g]);
        }
        let own_units = self.own_units();
        out.push(own_units.len() as f32);
        out.push(faction.national_focus.index() as f32);
        out.push(faction.focus_transition_days as f32);
        // Stage 10D: `unit_count` above already silently counts air units
        // (`own_units()` filters only on owner/`alive`), but an agent has no
        // way to read the `Domain::Air` slice of it on its own - see
        // `FACTION_FIELD_COUNT`'s own doc for why this is a new trailing
        // field rather than a change to `unit_count`'s existing meaning.
        let air_unit_count =
            own_units.iter().filter(|&&u| self.world.unit(u).station.domain() == Domain::Air).count();
        out.push(air_unit_count as f32);

        // Stage 3B (docs/phase3-spec.md "Stage 3B"): one `DIPLOMACY_FIELD_
        // COUNT`-sized block per faction in ascending `FactionId` order
        // (including self, zeroed, so every faction's encoding has the same
        // fixed shape regardless of which faction it's viewing from - the
        // same convention the region "owned" flag already uses).
        let dip = &self.world.diplomacy;
        for g_idx in 0..self.world.factions.len() {
            let other = FactionId(g_idx as u32);
            if other == self.faction {
                for _ in 0..DIPLOMACY_FIELD_COUNT {
                    out.push(0.0);
                }
                continue;
            }
            out.push(dip.stance(self.faction, other).index() as f32);
            out.push(dip.opinion(self.faction, other));
            out.push(if dip.has_treaty(self.faction, other, Treaty::MilitaryAccess) { 1.0 } else { 0.0 });
            out.push(if dip.has_treaty(self.faction, other, Treaty::PortAccess) { 1.0 } else { 0.0 });
            out.push(if dip.has_treaty(self.faction, other, Treaty::TradeAgreement) { 1.0 } else { 0.0 });
            let incoming = dip.pending.iter().find(|p| p.from == other && p.to == self.faction);
            out.push(if incoming.is_some() { 1.0 } else { 0.0 });
            out.push(incoming.map(|p| p.treaty.index() as f32).unwrap_or(-1.0));
            let outgoing = dip.pending.iter().find(|p| p.from == self.faction && p.to == other);
            out.push(if outgoing.is_some() { 1.0 } else { 0.0 });
            out.push(outgoing.map(|p| p.treaty.index() as f32).unwrap_or(-1.0));
        }

        // Stage 9D (docs/phase9-spec.md "4. 観測ベクトル"): the transport
        // network itself, one `TRANSPORT_LINE_FIELD_COUNT` block per line in
        // `world.transport_lines` order, then one `TRANSPORT_NODE_FIELD_COUNT`
        // block per node in `world.transport_nodes` order - see those two
        // constants' own docs for exactly what each field means.
        let flows = logistics::transport_line_flows(self.world);
        for (line, &flow) in self.world.transport_lines.iter().zip(flows.iter()) {
            let ra = self.world.transport_node(line.from).region;
            let rb = self.world.transport_node(line.to).region;
            // `codex review` (P2): this used to test plain ownership of both
            // endpoints, which contradicts the simulator for exactly the
            // case Stage 9D's occupier-supply fix introduced - a faction
            // that *occupies* both ends without owning them does haul supply
            // over the line (`TransportGraph::line_eligible_for` keys off
            // `controlled`, which is ownership **or** having live units
            // there), so reporting `0.0` would teach an RL or API consumer
            // that a line currently carrying its own supply is unusable.
            // Same rule as the flow model, not a second one that can drift.
            let controls = |r| {
                self.world.region(r).owner == self.faction
                    || self.world.units.iter().any(|u| u.alive && u.owner == self.faction && u.station.region() == Some(r))
            };
            let usable = controls(ra) && controls(rb);
            out.push(line.capacity.get());
            out.push(line.condition.get());
            out.push(flow);
            out.push(if usable { 1.0 } else { 0.0 });
        }
        for node in &self.world.transport_nodes {
            out.push(if self.world.region(node.region).owner == self.faction { 1.0 } else { 0.0 });
            // `naval::is_port_blockaded` only ever asks a *region's*
            // question ("is this region's port(s) blockaded"), so calling
            // it for every node regardless of kind reported every Depot and
            // Junction sharing a blockaded region's territory as blockaded
            // too - wrong on every scenario shipped, since every `mvp.json`
            // region carries both a depot and a port. Gated on the node
            // actually being a `Port`, matching this field's own doc above
            // ("`false` for every non-`Port` node").
            let blockaded =
                node.kind == TransportNodeKind::Port && naval::is_port_blockaded(self.world, node.region);
            out.push(if blockaded { 1.0 } else { 0.0 });
            // Stage 10D (`TRANSPORT_NODE_FIELD_COUNT`'s own doc): the node's
            // raw structural health, so an agent deciding whether to spend an
            // `Action::StrikeNode` (or expect a `Domain::Air`/`Domain::Sea`
            // recruit to succeed there) can see whether the target is
            // already wrecked or has since repaired, instead of only its
            // static owned/blockaded facts.
            out.push(node.condition.get());
            // Stage 10D (`codex review` P1, `TRANSPORT_NODE_FIELD_COUNT`'s
            // own doc): which kind of node this is - the field whose absence
            // left "airfields must be observable" unmet even after the three
            // fields above landed.
            out.push(node.kind.index() as f32);
        }

        debug_assert_eq!(out.len(), expected_len);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UnitId;

    /// Regression guard for exactly the defect class that used to be caught
    /// only by hashing a whole `--seed 1 --days 720` run
    /// (docs/conventions.md §5's now-removed "mvp のハッシュ"): `power_tables`
    /// (the batched per-region/per-zone precompute `encode()` uses) and the
    /// `enemy_power_from_table` fold it feeds must stay bit-for-bit
    /// identical to calling `World::region_power`/`zone_power` and
    /// `Observation::enemy_power`/`enemy_zone_power` directly -
    /// `power_tables`'s own doc above spells out why: `enemy_power_from_table`
    /// must fold over the other factions' precomputed subtotals in the same
    /// ascending order `enemy_power`'s own fold does, never derive itself as
    /// `total - own`, because that reassociates the underlying `f32` sum
    /// into a different (if mathematically equivalent) result.
    ///
    /// This is not a hypothetical: rewriting `enemy_power` that way is the
    /// actual defect this repository hit - correct in isolation, silently
    /// different bit-for-bit, and only visible as an AI threshold
    /// comparison flipping hundreds of simulated days later, past where any
    /// named acceptance test was looking. A whole-run hash caught it by
    /// accident; this test names the property directly and catches it at
    /// the source.
    ///
    /// Builds a world with several factions' units sharing one contested
    /// region *and* one contested sea zone (mixed stations, so both halves
    /// of `power_tables` are exercised), then checks every table cell
    /// against the direct computation with `==` - not an epsilon
    /// comparison, since reassociation is specifically a bit-exactness
    /// defect an epsilon check would hide.
    ///
    /// One faction in each cell is deliberately given a `combat_power`
    /// several orders of magnitude larger than the other two (real
    /// gameplay never produces stats this lopsided; this test isn't
    /// simulating gameplay, it's stressing the arithmetic). This is not
    /// decoration: a same-order-of-magnitude reassociation bug (`total -
    /// own` where every addend is a similar size) frequently rounds back to
    /// the *same* bits by coincidence - confirmed while writing this test,
    /// where several same-magnitude fixtures failed to expose the swap
    /// below at all. A wide magnitude gap forces real precision loss under
    /// `total - own` (the small factions' contribution gets partially or
    /// wholly absorbed into the large one before the subtraction ever
    /// happens) that folding over the small subtotals directly never
    /// incurs, so this fixture actually exercises the failure mode instead
    /// of passing either way.
    ///
    /// Confirmed this fails on the actual defect: temporarily changed
    /// `enemy_power_from_table` (this file, above) to `let total: f32 =
    /// self.world.factions.iter().fold(0.0, |acc, f| acc +
    /// cell_row[f.id.index()]); total - cell_row[self.faction.index()]` -
    /// algebraically the same fold this function already does, just
    /// re-associated - and re-ran: this test failed immediately with a
    /// bitwise mismatch at the magnitude-dominant faction's cell. Reverted
    /// before committing.
    #[test]
    fn power_table_matches_direct_computation_bit_for_bit() {
        let mut world = crate::scenario::build_world();
        assert!(
            world.factions.len() >= 3,
            "this test wants several factions with units to actually collide in one cell"
        );

        // scenarios/mvp.json (`UNITS_PER_FACTION == 3`) assigns unit ids in
        // faction order: 0,1,2 to faction 0, 3,4,5 to faction 1, 6,7,8 to
        // faction 2 (`Scenario::build_world`'s unit-construction loop).
        // One unit per faction goes to the contested land region, one per
        // faction to the contested sea zone, the third stays wherever the
        // scenario put it - so every table cell this test checks has real
        // multi-faction content to disagree about if the optimisation ever
        // diverges from the direct computation. Faction 0 dominates the
        // region, faction 1 dominates the zone - see this test's own doc
        // for why the magnitude gap matters.
        let contested_region = RegionId(3);
        let contested_zone = SeaZoneId(0);
        let land_manpower = [8_000_000.0_f32, 4.37, 6.91];
        let sea_manpower = [3.14_f32, 6_500_000.0, 5.62];
        for faction_idx in 0u32..3 {
            let land_unit = UnitId(faction_idx * 3);
            let sea_unit = UnitId(faction_idx * 3 + 1);
            let i = faction_idx as usize;

            let land = world.unit_mut(land_unit);
            land.station = Station::Region(contested_region);
            land.movement = None;
            land.manpower = land_manpower[i];
            land.equipment = 0.6 + faction_idx as f32 * 0.11;
            land.organization = 41.0 + faction_idx as f32 * 6.7;
            land.morale = 0.7 + faction_idx as f32 * 0.07;
            land.supply = 0.8 + faction_idx as f32 * 0.05;

            let sea = world.unit_mut(sea_unit);
            sea.station = Station::Sea(contested_zone);
            sea.movement = None;
            sea.manpower = sea_manpower[i];
            sea.equipment = 0.5 + faction_idx as f32 * 0.13;
            sea.organization = 37.0 + faction_idx as f32 * 5.3;
            sea.morale = 0.6 + faction_idx as f32 * 0.09;
            sea.supply = 0.75 + faction_idx as f32 * 0.04;
        }

        let obs = Observation { faction: FactionId(0), world: &world };
        let (region_power, zone_power) = obs.power_tables();

        for region in &world.regions {
            let cell_row = &region_power[region.id.index()];
            for faction in &world.factions {
                assert_eq!(
                    cell_row[faction.id.index()],
                    world.region_power(region.id, faction.id),
                    "power_tables()'s region cell (region {}, faction {}) must match \
                     World::region_power bit for bit",
                    region.id.0,
                    faction.id.0,
                );
                let obs_f = Observation { faction: faction.id, world: &world };
                assert_eq!(
                    obs_f.enemy_power_from_table(cell_row),
                    obs_f.enemy_power(region.id),
                    "enemy_power_from_table (region {}, faction {}) must match \
                     Observation::enemy_power bit for bit",
                    region.id.0,
                    faction.id.0,
                );
            }
        }

        for zone in &world.sea_zones {
            let cell_row = &zone_power[zone.id.index()];
            for faction in &world.factions {
                assert_eq!(
                    cell_row[faction.id.index()],
                    world.zone_power(zone.id, faction.id),
                    "power_tables()'s zone cell (zone {}, faction {}) must match \
                     World::zone_power bit for bit",
                    zone.id.0,
                    faction.id.0,
                );
                let obs_f = Observation { faction: faction.id, world: &world };
                assert_eq!(
                    obs_f.enemy_power_from_table(cell_row),
                    obs_f.enemy_zone_power(zone.id),
                    "enemy_power_from_table (zone {}, faction {}) must match \
                     Observation::enemy_zone_power bit for bit",
                    zone.id.0,
                    faction.id.0,
                );
            }
        }
    }

    /// Stage 10D (docs/phase10-spec.md "Stage 10D": "観測に...制空権が出
    /// る"): `encode()`'s per-region block must carry this observing
    /// faction's own `air_superiority` share and the highest share any other
    /// faction holds, at the exact offset `REGION_FIELD_COUNT`'s own doc
    /// says they live. Checked this fails when broken: temporarily deleted
    /// the two new `out.push` calls in `encode()`'s region loop (leaving
    /// `REGION_FIELD_COUNT` at its old, un-bumped value) - this test then
    /// panics on `debug_assert_eq!(out.len(), expected_len)` inside `encode`
    /// itself before its own assertions even run; leaving `REGION_FIELD_COUNT`
    /// bumped but dropping only the `out.push` calls instead makes every
    /// later field silently read from the wrong offset, which this test's own
    /// direct-offset assertions catch immediately. Reverted before committing.
    #[test]
    fn region_air_superiority_is_observable() {
        let mut world = crate::scenario::build_world();
        assert!(world.factions.len() >= 2, "this test wants a second faction to hold the 'enemy' share");
        let region = RegionId(0);
        world.region_mut(region).air_superiority[0] = crate::world::AirSuperiority::new(0.75).unwrap();
        world.region_mut(region).air_superiority[1] = crate::world::AirSuperiority::new(0.25).unwrap();

        let obs = Observation { faction: FactionId(0), world: &world };
        let encoded = obs.encode();
        assert_eq!(
            encoded.len(),
            encoding_len(
                world.regions.len(),
                world.sea_zones.len(),
                world.factions.len(),
                world.transport_lines.len(),
                world.transport_nodes.len(),
            ),
            "encode()'s real length must still follow encoding_len() after the new fields"
        );

        let own_air_offset = region.index() * REGION_FIELD_COUNT + 7 + GOOD_COUNT + 4;
        let enemy_air_offset = own_air_offset + 1;
        assert_eq!(encoded[own_air_offset], 0.75, "this faction's own air-superiority share must be observable");
        assert_eq!(
            encoded[enemy_air_offset], 0.25,
            "the highest other-faction air-superiority share must be observable"
        );
    }

    /// Stage 10D carry-over P1: `TransportNode::condition` used to be
    /// observable nowhere at all - an external agent could issue
    /// `Action::StrikeNode` but never see whether its target was already
    /// wrecked or had since repaired. Checked this fails when broken:
    /// temporarily removed the trailing `out.push(node.condition.get())` from
    /// `encode()`'s node loop (leaving `TRANSPORT_NODE_FIELD_COUNT`
    /// un-bumped) - `debug_assert_eq!` inside `encode` panics immediately the
    /// same way `region_air_superiority_is_observable` documents above.
    /// Reverted before committing.
    #[test]
    fn transport_node_condition_is_observable() {
        let mut world = crate::scenario::build_world();
        assert!(!world.transport_nodes.is_empty());
        world.transport_nodes[0].condition = crate::transport::Condition::new(0.42).unwrap();

        let obs = Observation { faction: FactionId(0), world: &world };
        let encoded = obs.encode();

        let base = world.regions.len() * REGION_FIELD_COUNT
            + world.sea_zones.len() * SEA_ZONE_FIELD_COUNT
            + FACTION_FIELD_COUNT
            + world.factions.len() * DIPLOMACY_FIELD_COUNT
            + world.transport_lines.len() * TRANSPORT_LINE_FIELD_COUNT;
        let condition_offset = base + 2; // node 0's own [owned_by_self, blockaded, condition]
        assert_eq!(
            encoded[condition_offset], 0.42,
            "a struck/repaired node's own condition must be observable, not just its owned/blockaded flags"
        );
    }

    /// Stage 10D (`codex review` P1): a flat-vector consumer must be able to
    /// tell an `Airfield` node apart from a `Depot`/`Port`/`Junction` -
    /// without this, "airfields must be observable" (docs/phase10-spec.md
    /// "Stage 10D") was unmet even after `owned_by_self`/`blockaded`/
    /// `condition` landed, since none of those three say *what kind* of node
    /// this is. Checked this fails when broken: temporarily removed the
    /// trailing `out.push(node.kind.index() as f32)` from `encode()`'s node
    /// loop (leaving `TRANSPORT_NODE_FIELD_COUNT` un-bumped) -
    /// `debug_assert_eq!` inside `encode` panics immediately, the same way
    /// `transport_node_condition_is_observable` above documents. Reverted
    /// before committing.
    #[test]
    fn transport_node_kind_is_observable() {
        let world = crate::scenario::build_world();
        let (airfield_idx, airfield_node) = world
            .transport_nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.kind == crate::transport::TransportNodeKind::Airfield)
            .expect("scenarios/mvp.json declares at least one airfield");
        let (depot_idx, depot_node) = world
            .transport_nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.kind == crate::transport::TransportNodeKind::Depot)
            .expect("scenarios/mvp.json declares at least one depot");
        assert_ne!(
            airfield_node.kind.index(),
            depot_node.kind.index(),
            "an Airfield and a Depot must not share a numeric kind code"
        );

        let obs = Observation { faction: FactionId(0), world: &world };
        let encoded = obs.encode();
        let base = world.regions.len() * REGION_FIELD_COUNT
            + world.sea_zones.len() * SEA_ZONE_FIELD_COUNT
            + FACTION_FIELD_COUNT
            + world.factions.len() * DIPLOMACY_FIELD_COUNT
            + world.transport_lines.len() * TRANSPORT_LINE_FIELD_COUNT;
        let kind_offset = |node_idx: usize| base + node_idx * TRANSPORT_NODE_FIELD_COUNT + 3;
        assert_eq!(
            encoded[kind_offset(airfield_idx)],
            airfield_node.kind.index() as f32,
            "an Airfield node's own kind code must be observable"
        );
        assert_eq!(
            encoded[kind_offset(depot_idx)],
            depot_node.kind.index() as f32,
            "a Depot node's own kind code must be observable, and distinct from an Airfield's"
        );
    }

    /// Stage 10D: the `Domain::Air` slice of a faction's unit count
    /// (`FACTION_FIELD_COUNT`'s own doc explains why this is a new trailing
    /// field rather than a redefinition of the existing `unit_count`).
    /// Checked this fails when broken: temporarily hardcoded `air_unit_count`
    /// in `encode()` to always push `0.0` regardless of `own_units` - the
    /// assertion below then fails once a unit actually sits at an airfield.
    /// Reverted before committing.
    #[test]
    fn air_unit_count_is_observable() {
        let mut world = crate::scenario::build_world();
        let node_id = world
            .transport_nodes
            .iter()
            .find(|n| n.kind == crate::transport::TransportNodeKind::Airfield)
            .map(|n| n.id)
            .expect("every shipped scenario declares at least one airfield");

        // Move an existing faction-0 unit to that airfield - `Station::
        // domain()` derives purely from the station variant, so this alone
        // makes it an air unit for `own_unit_count`'s purposes; no need to
        // hand-build a fresh `Unit`.
        let unit_id = UnitId(0);
        assert_eq!(world.unit(unit_id).owner, FactionId(0), "scenario::build_world assigns unit 0 to faction 0");
        world.unit_mut(unit_id).station = Station::Airfield(node_id);

        let obs = Observation { faction: FactionId(0), world: &world };
        let encoded = obs.encode();
        let air_unit_count_offset = world.regions.len() * REGION_FIELD_COUNT
            + world.sea_zones.len() * SEA_ZONE_FIELD_COUNT
            + FACTION_FIELD_COUNT
            - 1;
        assert_eq!(
            encoded[air_unit_count_offset], 1.0,
            "moving one unit to an airfield must show up as air_unit_count == 1"
        );
    }
}
