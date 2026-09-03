//! The map (regions, links, terrain) and the mutable game state
//! (factions, units, supply) that every tick system reads and writes.

use crate::balance::WORKFORCE_SHARE;
use crate::ids::{FactionId, RegionId, UnitId};
use crate::military::Unit;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Terrain {
    Plain,
    Hill,
    Mountain,
    Urban,
}

impl Terrain {
    pub fn defense_bonus(self) -> f32 {
        match self {
            Terrain::Plain => 1.00,
            Terrain::Hill => 1.25,
            Terrain::Mountain => 1.60,
            Terrain::Urban => 1.40,
        }
    }

    pub fn move_cost(self) -> f32 {
        match self {
            Terrain::Plain => 1.0,
            Terrain::Hill => 1.3,
            Terrain::Mountain => 1.7,
            Terrain::Urban => 1.2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkKind {
    Rail,
    Road,
    Tunnel,
    Strait,
    Sea,
}

impl LinkKind {
    pub fn retention(self) -> f32 {
        match self {
            LinkKind::Rail => 0.93,
            LinkKind::Road => 0.80,
            LinkKind::Tunnel => 0.86,
            LinkKind::Strait => 0.62,
            LinkKind::Sea => 0.55,
        }
    }

    pub fn max_throughput(self) -> f32 {
        match self {
            LinkKind::Rail => 25.0,
            LinkKind::Road => 12.0,
            LinkKind::Tunnel => 8.0,
            LinkKind::Strait => 6.0,
            LinkKind::Sea => 7.0,
        }
    }

    pub fn travel_days(self) -> f32 {
        match self {
            LinkKind::Rail => 2.0,
            LinkKind::Road => 3.0,
            LinkKind::Tunnel => 3.0,
            LinkKind::Strait => 4.5,
            LinkKind::Sea => 6.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Link {
    pub to: RegionId,
    pub kind: LinkKind,
}

#[derive(Clone, Debug)]
pub struct Region {
    pub id: RegionId,
    pub name: String,
    pub terrain: Terrain,
    pub owner: FactionId,
    /// Owner at the outbreak of war; used to tell home soil from occupied land.
    pub core: FactionId,
    pub population: f32,
    pub industry: f32,
    pub food: f32,
    pub infrastructure: f32,
    pub port: f32,
    pub mobilized: f32,
    pub unrest: f32,
    pub occupation: f32,
    pub occupier: Option<FactionId>,
    pub links: Vec<Link>,
}

impl Region {
    pub fn supply_source(&self) -> f32 {
        self.industry * 0.5 + self.port * 4.0
    }

    pub fn labor_ratio(&self) -> f32 {
        let workforce = self.population * WORKFORCE_SHARE;
        ((workforce - self.mobilized) / workforce).clamp(0.15, 1.0)
    }

    /// Rough strategic value of this region, used by AI agents to weigh targets.
    pub fn value(&self) -> f32 {
        self.industry * 1.5 + self.population * 0.05 + self.port * 3.0
    }
}

#[derive(Clone, Debug)]
pub struct Faction {
    pub id: FactionId,
    pub name: String,
    pub capital: RegionId,
    pub manpower: f32,
    pub supplies: f32,
    pub equipment: f32,
    pub conscription: f32,
    pub production_mix: f32,
    pub war_support: f32,
    pub stability: f32,
    pub shortage: f32,
    pub casualties: f32,
    pub supply_ratio: f32,
    pub alive: bool,
}

#[derive(Clone, Debug)]
pub struct World {
    pub regions: Vec<Region>,
    pub factions: Vec<Faction>,
    pub units: Vec<Unit>,
    /// Supply throughput available at each region, indexed by `RegionId`.
    pub supply: Vec<f32>,
    pub day: u32,
}

impl World {
    pub fn region(&self, id: RegionId) -> &Region {
        &self.regions[id.index()]
    }

    pub fn region_mut(&mut self, id: RegionId) -> &mut Region {
        &mut self.regions[id.index()]
    }

    pub fn faction(&self, id: FactionId) -> &Faction {
        &self.factions[id.index()]
    }

    pub fn faction_mut(&mut self, id: FactionId) -> &mut Faction {
        &mut self.factions[id.index()]
    }

    pub fn neighbors(&self, id: RegionId) -> impl Iterator<Item = RegionId> + '_ {
        self.region(id).links.iter().map(|link| link.to)
    }

    pub fn link_between(&self, from: RegionId, to: RegionId) -> Option<Link> {
        self.region(from)
            .links
            .iter()
            .copied()
            .find(|link| link.to == to)
    }

    pub fn units_in(&self, region: RegionId) -> impl Iterator<Item = &Unit> {
        self.units
            .iter()
            .filter(move |unit| unit.alive && unit.location == region)
    }

    pub fn has_enemy_units(&self, region: RegionId, faction: FactionId) -> bool {
        self.units_in(region).any(|unit| unit.owner != faction)
    }

    /// Sum of `combat_power` for a faction's alive units present in `region`.
    pub fn region_power(&self, region: RegionId, faction: FactionId) -> f32 {
        self.units_in(region)
            .filter(|unit| unit.owner == faction)
            .map(Unit::combat_power)
            .fold(0.0, |acc, p| acc + p)
    }

    pub fn regions_of(&self, faction: FactionId) -> Vec<RegionId> {
        self.regions
            .iter()
            .filter(|r| r.owner == faction)
            .map(|r| r.id)
            .collect()
    }

    pub fn region_count(&self, faction: FactionId) -> usize {
        self.regions.iter().filter(|r| r.owner == faction).count()
    }

    pub fn industry_total(&self, faction: FactionId) -> f32 {
        self.regions
            .iter()
            .filter(|r| r.owner == faction)
            .fold(0.0, |acc, r| acc + r.industry)
    }

    pub fn unit(&self, id: UnitId) -> &Unit {
        &self.units[id.index()]
    }

    pub fn unit_mut(&mut self, id: UnitId) -> &mut Unit {
        &mut self.units[id.index()]
    }
}
