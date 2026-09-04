//! What an agent gets to see: its own faction's view of the world, plus a
//! flat float encoding for RL-style consumers.

use std::collections::VecDeque;

use crate::diplomacy::Treaty;
use crate::good::GOOD_COUNT;
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::world::{Station, World};

/// Per-region field count in `Observation::encode()`: `[owned, population,
/// infrastructure, supply, unrest, own_power, enemy_power]` (7 fixed
/// fields) followed by `capacity[GOOD_COUNT]`, then `[devastation,
/// construction_progress]` (Stage 2B, 2 fixed fields), then `[import_flow,
/// node_throughput]` (Stage 2C, 2 fixed fields).
pub const REGION_FIELD_COUNT: usize = 7 + GOOD_COUNT + 2 + 2;

/// Per-sea-zone field count in `Observation::encode()` (Stage 2D):
/// `[own_control, enemy_control_max, own_power, enemy_power]`.
pub const SEA_ZONE_FIELD_COUNT: usize = 4;

/// Faction-scalar field count in `Observation::encode()`: `manpower`,
/// `stock[GOOD_COUNT]`, `war_support`, `stability`,
/// `group_support[GROUP_COUNT]` (Stage 3A), `unit_count`,
/// `[national_focus_code, focus_transition_days]` (Stage 3C).
pub const FACTION_FIELD_COUNT: usize = 4 + GOOD_COUNT + GROUP_COUNT + 2;

/// Stage 3B per-relation field count in `Observation::encode()`, one block
/// per *other* faction (own row zeroed - see `encode`'s doc): `[stance_code,
/// opinion, military_access, port_access, trade_agreement,
/// pending_incoming, pending_incoming_treaty, pending_outgoing,
/// pending_outgoing_treaty]`. `stance_code` is `Stance::index()` as an
/// `f32`; the `*_treaty` fields are `Treaty::index()` as an `f32`, or `-1.0`
/// when there's no pending proposal in that direction.
pub const DIPLOMACY_FIELD_COUNT: usize = 9;

/// `Observation::encode()`'s output length for a scenario with the given
/// region/sea-zone/faction counts - the general form of `ENCODING_LEN`
/// below. Stage 6A (docs/phase6-spec.md "Stage 6A"): scenario data is no
/// longer fixed at compile time (`--scenario` can load a differently-sized
/// map), so `encode()` itself computes its expected length this way rather
/// than trusting the compile-time `ENCODING_LEN` constant, which only ever
/// describes the embedded default scenario.
pub const fn encoding_len(region_count: usize, sea_zone_count: usize, faction_count: usize) -> usize {
    region_count * REGION_FIELD_COUNT + sea_zone_count * SEA_ZONE_FIELD_COUNT + FACTION_FIELD_COUNT + faction_count * DIPLOMACY_FIELD_COUNT
}

/// Fixed total length of `Observation::encode()`'s output for the embedded
/// default scenario (`scenario::REGION_COUNT` regions,
/// `scenario::SEA_ZONE_COUNT` sea zones, `scenario::FACTION_COUNT`
/// factions) - i.e. `scenarios/mvp.json`. A `--scenario`-loaded world with
/// different counts has a different real length; compute it with
/// `encoding_len` from that world's actual sizes instead of assuming this
/// constant, the same way `encode()` itself does.
pub const ENCODING_LEN: usize = encoding_len(crate::scenario::REGION_COUNT, crate::scenario::SEA_ZONE_COUNT, crate::scenario::FACTION_COUNT);

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
        let expected_len = encoding_len(self.world.regions.len(), self.world.sea_zones.len(), self.world.factions.len());
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
        out.push(self.own_units().len() as f32);
        out.push(faction.national_focus.index() as f32);
        out.push(faction.focus_transition_days as f32);

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

        debug_assert_eq!(out.len(), expected_len);
        out
    }
}
