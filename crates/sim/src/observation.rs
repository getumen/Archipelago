//! What an agent gets to see: its own faction's view of the world, plus a
//! flat float encoding for RL-style consumers.

use std::collections::VecDeque;

use crate::good::GOOD_COUNT;
use crate::ids::{FactionId, RegionId, UnitId};
use crate::world::World;

/// Per-region field count in `Observation::encode()`: `[owned, population,
/// infrastructure, supply, unrest, own_power, enemy_power]` (7 fixed
/// fields) followed by `capacity[GOOD_COUNT]`.
pub const REGION_FIELD_COUNT: usize = 7 + GOOD_COUNT;

/// Faction-scalar field count in `Observation::encode()`: `manpower`,
/// `stock[GOOD_COUNT]`, `war_support`, `stability`, `unit_count`.
pub const FACTION_FIELD_COUNT: usize = 4 + GOOD_COUNT;

/// Fixed total length of `Observation::encode()`'s output for the MVP map
/// (`scenario::REGION_COUNT` regions).
pub const ENCODING_LEN: usize = crate::scenario::REGION_COUNT * REGION_FIELD_COUNT + FACTION_FIELD_COUNT;

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
    /// FACTION_FIELD_COUNT`): per-region `[owned, population,
    /// infrastructure, supply, unrest, own_power, enemy_power,
    /// capacity[GOOD_COUNT]...]`, then faction scalars `[manpower,
    /// stock[GOOD_COUNT]..., war_support, stability, unit_count]`.
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
        }
        let faction = self.world.faction(self.faction);
        out.push(faction.manpower);
        for g in 0..GOOD_COUNT {
            out.push(faction.stock[g]);
        }
        out.push(faction.war_support);
        out.push(faction.stability);
        out.push(self.own_units().len() as f32);
        debug_assert_eq!(out.len(), ENCODING_LEN);
        out
    }
}
