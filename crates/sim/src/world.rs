//! The map (regions, links, terrain) and the mutable game state
//! (factions, units, supply) that every tick system reads and writes.

use crate::balance::{INFRA_DAMAGE_SHARE, WORKFORCE_SHARE};
use crate::construction::Construction;
use crate::good::{Good, GOOD_COUNT};
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
    /// Production capacity per commodity, indexed by `Good::index()`.
    pub capacity: [f32; GOOD_COUNT],
    pub infrastructure: f32,
    pub port: f32,
    pub mobilized: f32,
    pub unrest: f32,
    pub occupation: f32,
    pub occupier: Option<FactionId>,
    pub links: Vec<Link>,
    /// War damage, `0..1` (Stage 2B, docs/phase2-spec.md "Stage 2B — インフラ
    /// と建設・戦災"): holding a region isn't the same as being able to use
    /// it. Read through `effective_capacity`/`effective_infrastructure`
    /// rather than directly — every production and supply-propagation read
    /// of `capacity`/`infrastructure` must see the devastated value.
    pub devastation: f32,
    /// The region's single in-progress build project, if any
    /// (`Action::Build`/`Action::CancelBuild`, `construction::tick_construction`).
    /// Discarded (`None`) whenever the region changes hands.
    pub construction: Option<Construction>,
}

impl Region {
    /// `capacity[good]` degraded by war damage — what this region can
    /// actually produce today, as opposed to what it could produce undamaged.
    pub fn effective_capacity(&self, good: Good) -> f32 {
        self.capacity[good.index()] * (1.0 - self.devastation)
    }

    /// `infrastructure` degraded by war damage. Read this everywhere
    /// `infrastructure` used to be read directly for production efficiency,
    /// supply-network propagation (`economy`, `logistics`), or a unit's
    /// organization regeneration (`military::tick_recovery`) — devastation
    /// caps `INFRA_DAMAGE_SHARE` of infrastructure's contribution rather
    /// than all of it, since even a wrecked region keeps some road/rail bed.
    pub fn effective_infrastructure(&self) -> f32 {
        self.infrastructure * (1.0 - self.devastation * INFRA_DAMAGE_SHARE)
    }

    /// Sum of every commodity's *effective* (devastation-adjusted) capacity
    /// except `Food` — this region's contribution to war-relevant industry
    /// (Stage 2A redefinition of the Phase 1 `industry` field that
    /// `World::industry_total` and `supply_source`/`value` below depend on).
    pub fn industry_total(&self) -> f32 {
        let mut total = 0.0;
        for good in crate::good::ALL_GOODS {
            if good != Good::Food {
                total += self.effective_capacity(good);
            }
        }
        total
    }

    pub fn supply_source(&self) -> f32 {
        self.industry_total() * 0.5 + self.port * 4.0
    }

    pub fn labor_ratio(&self) -> f32 {
        let workforce = self.population * WORKFORCE_SHARE;
        ((workforce - self.mobilized) / workforce).clamp(0.15, 1.0)
    }

    /// Rough strategic value of this region, used by AI agents to weigh targets.
    pub fn value(&self) -> f32 {
        self.industry_total() * 1.5 + self.population * 0.05 + self.port * 3.0
    }
}

#[derive(Clone, Debug)]
pub struct Faction {
    pub id: FactionId,
    pub name: String,
    pub capital: RegionId,
    pub manpower: f32,
    /// Stockpile per commodity, indexed by `Good::index()`. Units draw
    /// `Munitions` for upkeep and `Arms` for equipment (Phase 1's
    /// `supplies`/`equipment`).
    pub stock: [f32; GOOD_COUNT],
    pub conscription: f32,
    /// Priority weight per commodity, indexed by `Good::index()`, used to
    /// apportion a shared input (currently: Steel and Energy contended by
    /// Machinery and Munitions) between competing outputs.
    pub industry_priority: [f32; GOOD_COUNT],
    /// Fraction of civilian Food/Energy/Machinery demand the government
    /// actually delivers, in `balance::CIVILIAN_RATION_MIN..=
    /// CIVILIAN_RATION_MAX` (design.md §9: squeezing civilians to feed the
    /// war effort is a policy choice with an unrest cost, not a fixed rule).
    /// Set via `Action::SetCivilianRation`.
    pub civilian_ration: f32,
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
            .fold(0.0, |acc, r| acc + r.industry_total())
    }

    pub fn unit(&self, id: UnitId) -> &Unit {
        &self.units[id.index()]
    }

    pub fn unit_mut(&mut self, id: UnitId) -> &mut Unit {
        &mut self.units[id.index()]
    }
}
