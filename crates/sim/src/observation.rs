//! What an agent gets to see: its own faction's view of the world, plus a
//! flat float encoding for RL-style consumers.

use std::collections::VecDeque;

use crate::diplomacy::Treaty;
use crate::good::GOOD_COUNT;
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::world::World;

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

/// Fixed total length of `Observation::encode()`'s output for the MVP map
/// (`scenario::REGION_COUNT` regions, `scenario::SEA_ZONE_COUNT` sea zones,
/// `scenario::FACTION_COUNT` factions).
pub const ENCODING_LEN: usize = crate::scenario::REGION_COUNT * REGION_FIELD_COUNT
    + crate::scenario::SEA_ZONE_COUNT * SEA_ZONE_FIELD_COUNT
    + FACTION_FIELD_COUNT
    + crate::scenario::FACTION_COUNT * DIPLOMACY_FIELD_COUNT;

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

    /// Fixed length `ENCODING_LEN` (`regions.len() * REGION_FIELD_COUNT +
    /// sea_zones.len() * SEA_ZONE_FIELD_COUNT + FACTION_FIELD_COUNT`):
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
        let mut out = Vec::with_capacity(ENCODING_LEN);
        for region in &self.world.regions {
            out.push(if region.owner == self.faction { 1.0 } else { 0.0 });
            out.push(region.population);
            out.push(region.infrastructure);
            out.push(self.world.supply[region.id.index()]);
            out.push(region.unrest);
            out.push(self.own_power(region.id));
            out.push(self.enemy_power(region.id));
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
            out.push(self.own_zone_power(zone.id));
            out.push(self.enemy_zone_power(zone.id));
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
        for g_idx in 0..crate::scenario::FACTION_COUNT {
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

        debug_assert_eq!(out.len(), ENCODING_LEN);
        out
    }
}
