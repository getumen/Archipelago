//! Builds the fixed 10-region, 3-faction MVP map described in the design
//! doc's §19 (10 地域・3 勢力), all factions starting at war with each other.

use crate::ids::{FactionId, RegionId, UnitId};
use crate::military::Unit;
use crate::world::{Faction, Link, LinkKind, Region, Terrain, World};

struct RegionSpec {
    name: &'static str,
    terrain: Terrain,
    population: f32,
    industry: f32,
    food: f32,
    infrastructure: f32,
    port: f32,
}

struct FactionSpec {
    name: &'static str,
    capital: u32,
    regions: &'static [u32],
}

const REGION_SPECS: [RegionSpec; 10] = [
    RegionSpec { name: "北海道", terrain: Terrain::Plain, population: 510.0, industry: 3.0, food: 12.0, infrastructure: 0.55, port: 1.0 },
    RegionSpec { name: "北東北", terrain: Terrain::Hill, population: 330.0, industry: 2.5, food: 9.0, infrastructure: 0.50, port: 0.6 },
    RegionSpec { name: "南東北", terrain: Terrain::Hill, population: 550.0, industry: 5.0, food: 8.0, infrastructure: 0.65, port: 0.7 },
    RegionSpec { name: "関東", terrain: Terrain::Urban, population: 4300.0, industry: 20.0, food: 3.0, infrastructure: 1.00, port: 1.5 },
    RegionSpec { name: "信越・北陸", terrain: Terrain::Mountain, population: 480.0, industry: 4.0, food: 6.0, infrastructure: 0.55, port: 0.5 },
    RegionSpec { name: "東海", terrain: Terrain::Plain, population: 1500.0, industry: 16.0, food: 4.0, infrastructure: 0.90, port: 1.2 },
    RegionSpec { name: "近畿", terrain: Terrain::Urban, population: 2200.0, industry: 14.0, food: 2.0, infrastructure: 0.95, port: 1.3 },
    RegionSpec { name: "中国", terrain: Terrain::Hill, population: 740.0, industry: 6.0, food: 3.5, infrastructure: 0.70, port: 0.9 },
    RegionSpec { name: "四国", terrain: Terrain::Hill, population: 370.0, industry: 2.0, food: 4.0, infrastructure: 0.60, port: 0.6 },
    RegionSpec { name: "九州", terrain: Terrain::Plain, population: 1300.0, industry: 8.0, food: 7.0, infrastructure: 0.75, port: 1.4 },
];

const LINK_SPECS: [(u32, u32, LinkKind); 12] = [
    (0, 1, LinkKind::Strait),
    (1, 2, LinkKind::Rail),
    (2, 3, LinkKind::Rail),
    (3, 4, LinkKind::Rail),
    (3, 5, LinkKind::Rail),
    (4, 5, LinkKind::Road),
    (4, 6, LinkKind::Rail),
    (5, 6, LinkKind::Rail),
    (6, 7, LinkKind::Rail),
    (6, 8, LinkKind::Strait),
    (7, 8, LinkKind::Strait),
    (7, 9, LinkKind::Tunnel),
];

const FACTION_SPECS: [FactionSpec; 3] = [
    FactionSpec { name: "東方連合", capital: 3, regions: &[0, 1, 2, 3] },
    FactionSpec { name: "中央同盟", capital: 6, regions: &[4, 5, 6] },
    FactionSpec { name: "西方同盟", capital: 9, regions: &[7, 8, 9] },
];

const FACTION_MANPOWER: f32 = 12.0;
const FACTION_SUPPLIES: f32 = 400.0;
const FACTION_EQUIPMENT: f32 = 250.0;
const FACTION_CONSCRIPTION: f32 = 0.5;
const FACTION_PRODUCTION_MIX: f32 = 0.6;
const FACTION_WAR_SUPPORT: f32 = 60.0;
const FACTION_STABILITY: f32 = 80.0;

const UNITS_PER_FACTION: usize = 3;

pub fn build_world() -> World {
    let owner_of: Vec<FactionId> = {
        let mut owner = vec![FactionId(0); REGION_SPECS.len()];
        for (f_idx, spec) in FACTION_SPECS.iter().enumerate() {
            for &r in spec.regions {
                owner[r as usize] = FactionId(f_idx as u32);
            }
        }
        owner
    };

    let mut regions: Vec<Region> = REGION_SPECS
        .iter()
        .enumerate()
        .map(|(i, spec)| Region {
            id: RegionId(i as u32),
            name: spec.name.to_string(),
            terrain: spec.terrain,
            owner: owner_of[i],
            core: owner_of[i],
            population: spec.population,
            industry: spec.industry,
            food: spec.food,
            infrastructure: spec.infrastructure,
            port: spec.port,
            mobilized: 0.0,
            unrest: 0.0,
            occupation: 0.0,
            occupier: None,
            links: Vec::new(),
        })
        .collect();

    for &(a, b, kind) in &LINK_SPECS {
        regions[a as usize].links.push(Link { to: RegionId(b), kind });
        regions[b as usize].links.push(Link { to: RegionId(a), kind });
    }

    let factions: Vec<Faction> = FACTION_SPECS
        .iter()
        .enumerate()
        .map(|(i, spec)| Faction {
            id: FactionId(i as u32),
            name: spec.name.to_string(),
            capital: RegionId(spec.capital),
            manpower: FACTION_MANPOWER,
            supplies: FACTION_SUPPLIES,
            equipment: FACTION_EQUIPMENT,
            conscription: FACTION_CONSCRIPTION,
            production_mix: FACTION_PRODUCTION_MIX,
            war_support: FACTION_WAR_SUPPORT,
            stability: FACTION_STABILITY,
            shortage: 0.0,
            casualties: 0.0,
            supply_ratio: 1.0,
            alive: true,
        })
        .collect();

    let supply: Vec<f32> = regions.iter().map(Region::supply_source).collect();

    let mut units = Vec::new();
    for (f_idx, spec) in FACTION_SPECS.iter().enumerate() {
        let faction_id = FactionId(f_idx as u32);
        let capital = RegionId(spec.capital);

        let mut locations = vec![capital];
        let mut owned_neighbors: Vec<RegionId> = regions[capital.index()]
            .links
            .iter()
            .map(|l| l.to)
            .filter(|&r| regions[r.index()].owner == faction_id)
            .collect();
        owned_neighbors.sort_by_key(|r| r.0);
        locations.extend(owned_neighbors);

        for i in 0..UNITS_PER_FACTION {
            let location = locations[i % locations.len()];
            let id = UnitId(units.len() as u32);
            units.push(Unit {
                id,
                owner: faction_id,
                name: format!("{} Corps {}", spec.name, i + 1),
                location,
                movement: None,
                manpower: crate::balance::UNIT_MANPOWER,
                equipment: crate::balance::UNIT_EQUIPMENT,
                organization: crate::balance::UNIT_ORG,
                morale: 1.0,
                supply: 1.0,
                experience: 0.0,
                alive: true,
            });
        }
    }

    World {
        regions,
        factions,
        units,
        supply,
        day: 0,
    }
}
