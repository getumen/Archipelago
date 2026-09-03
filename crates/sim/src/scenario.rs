//! Builds the fixed 10-region, 3-faction MVP map described in the design
//! doc's §19 (10 地域・3 勢力), all factions starting at war with each other.
//! Stage 2A (docs/phase2-spec.md) replaces the old single `industry`/`food`
//! pair with a per-commodity `capacity` table, deliberately profiled so
//! Kanto/Tokai are the nation's Machinery hub and losing them chokes Arms.

use crate::good::GOOD_COUNT;
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::military::Unit;
use crate::world::{Faction, Link, LinkKind, Region, SeaZone, Station, Terrain, World};

struct RegionSpec {
    name: &'static str,
    terrain: Terrain,
    population: f32,
    /// Capacity per commodity, in `Good::index()` order: Food, Energy,
    /// Steel, Machinery, Munitions, Arms.
    capacity: [f32; GOOD_COUNT],
    infrastructure: f32,
    port: f32,
}

struct FactionSpec {
    name: &'static str,
    capital: u32,
    regions: &'static [u32],
}

const REGION_SPECS: [RegionSpec; 10] = [
    RegionSpec { name: "北海道", terrain: Terrain::Plain, population: 510.0, capacity: [12.0, 2.0, 0.5, 0.3, 0.2, 0.0], infrastructure: 0.55, port: 1.0 },
    RegionSpec { name: "北東北", terrain: Terrain::Hill, population: 330.0, capacity: [9.0, 1.5, 0.5, 0.3, 0.2, 0.0], infrastructure: 0.50, port: 0.6 },
    RegionSpec { name: "南東北", terrain: Terrain::Hill, population: 550.0, capacity: [8.0, 2.5, 1.5, 0.8, 0.5, 0.2], infrastructure: 0.65, port: 0.7 },
    RegionSpec { name: "関東", terrain: Terrain::Urban, population: 4300.0, capacity: [3.0, 4.0, 3.5, 6.5, 3.0, 3.0], infrastructure: 1.00, port: 1.5 },
    RegionSpec { name: "信越・北陸", terrain: Terrain::Mountain, population: 480.0, capacity: [6.0, 3.0, 1.0, 0.7, 0.3, 0.0], infrastructure: 0.55, port: 0.5 },
    RegionSpec { name: "東海", terrain: Terrain::Plain, population: 1500.0, capacity: [4.0, 2.0, 3.0, 7.0, 2.0, 2.0], infrastructure: 0.90, port: 1.2 },
    RegionSpec { name: "近畿", terrain: Terrain::Urban, population: 2200.0, capacity: [2.0, 2.5, 3.5, 4.5, 2.0, 1.5], infrastructure: 0.95, port: 1.3 },
    RegionSpec { name: "中国", terrain: Terrain::Hill, population: 740.0, capacity: [3.5, 2.0, 2.5, 1.0, 0.5, 0.0], infrastructure: 0.70, port: 0.9 },
    RegionSpec { name: "四国", terrain: Terrain::Hill, population: 370.0, capacity: [4.0, 0.8, 0.5, 0.4, 0.3, 0.0], infrastructure: 0.60, port: 0.6 },
    RegionSpec { name: "九州", terrain: Terrain::Plain, population: 1300.0, capacity: [7.0, 2.5, 2.0, 1.5, 1.5, 0.5], infrastructure: 0.75, port: 1.4 },
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

/// Stage 2D (docs/phase2-spec.md "海域"): the 5 sea zones of the MVP map.
/// `coast`/`adjacent` are region/zone indices, resolved into `RegionId`/
/// `SeaZoneId` in `build_world`.
struct SeaZoneSpec {
    name: &'static str,
    coast: &'static [u32],
    adjacent: &'static [u32],
}

const SEA_ZONE_SPECS: [SeaZoneSpec; 5] = [
    SeaZoneSpec { name: "北方海域", coast: &[0, 1], adjacent: &[1, 3] },
    SeaZoneSpec { name: "太平洋北", coast: &[1, 2, 3], adjacent: &[0, 2] },
    SeaZoneSpec { name: "太平洋南", coast: &[3, 5, 6, 8], adjacent: &[1, 4] },
    SeaZoneSpec { name: "日本海", coast: &[0, 1, 2, 4, 7], adjacent: &[0, 4] },
    SeaZoneSpec { name: "西方海域", coast: &[6, 7, 8, 9], adjacent: &[2, 3] },
];

/// Stage 2D (docs/phase2-spec.md "海峡リンクとの対応"): which sea zone each
/// `Strait` link physically passes through, resolved into `Link::strait_zone`
/// in `build_world`. The 中国—九州 `Tunnel` (7, 9) is deliberately absent —
/// it stays open under any blockade, per the spec's "関門トンネルが封鎖の影響
/// を受けないのは意図的である".
const STRAIT_ZONE_SPECS: [(u32, u32, u32); 3] = [
    (0, 1, 0), // 北海道—北東北 -> 北方海域
    (6, 8, 2), // 近畿—四国 -> 太平洋南
    (7, 8, 4), // 中国—四国 -> 西方海域
];

const FACTION_SPECS: [FactionSpec; 3] = [
    FactionSpec { name: "東方連合", capital: 3, regions: &[0, 1, 2, 3] },
    FactionSpec { name: "中央同盟", capital: 6, regions: &[4, 5, 6] },
    FactionSpec { name: "西方同盟", capital: 9, regions: &[7, 8, 9] },
];

const FACTION_MANPOWER: f32 = 12.0;
/// Initial stock per commodity, in `Good::index()` order (Food, Energy,
/// Steel, Machinery, Munitions, Arms). Munitions/Arms keep Phase 1's
/// `supplies`/`equipment` starting values; the upstream goods start with a
/// modest buffer so the chain isn't starved on day one.
const FACTION_STOCK: [f32; GOOD_COUNT] = [200.0, 100.0, 80.0, 40.0, 400.0, 250.0];
/// `pub(crate)`, not private: Stage 3A regime change
/// (`politics::apply_political_events`, docs/phase3-spec.md "政権交代の扱
/// い") resets a faction's policy back to exactly these same starting
/// values rather than duplicating them as separate balance constants that
/// could drift out of sync with what a fresh faction actually starts at.
pub(crate) const FACTION_CONSCRIPTION: f32 = 0.5;
/// Initial industry priority: an even three-way split of Energy between
/// Steel, Machinery and Munitions, and an even split of Steel between
/// Machinery and Munitions - the goods that contend for shared Energy/Steel
/// input in Stage 2A (see `economy::tick_economy`).
pub(crate) const FACTION_INDUSTRY_PRIORITY: [f32; GOOD_COUNT] = [0.0, 0.0, 0.5, 0.5, 0.5, 0.0];
/// Initial import plan (Stage 2C): no imports requested until an agent or
/// player sets one via `Action::SetImportPlan`.
pub(crate) const FACTION_IMPORT_PLAN: [f32; GOOD_COUNT] = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
/// Initial logistics priority (Stage 2C): an even split of the shared
/// regional throughput between Munitions and Arms delivery, the same
/// even-split convention `FACTION_INDUSTRY_PRIORITY` uses for its contended
/// inputs.
pub(crate) const FACTION_LOGISTICS_PRIORITY: [f32; GOOD_COUNT] = [0.0, 0.0, 0.0, 0.0, 0.5, 0.5];
const FACTION_WAR_SUPPORT: f32 = 60.0;
const FACTION_STABILITY: f32 = 80.0;
/// Stage 3A (docs/phase3-spec.md "Stage 3A": "初期値は全勢力共通で支持 60"):
/// every `Group` starts at the same support level regardless of influence.
const FACTION_GROUP_SUPPORT: [f32; GROUP_COUNT] = [60.0; GROUP_COUNT];
/// Stage 3A fixed influence weights, in `Group::index()` order (Government,
/// LocalGovernment, Bureaucracy, Military, Business, Labor, Citizens) per
/// docs/phase3-spec.md's "初期値は... 影響力は Government 0.20 /
/// LocalGovernment 0.10 / Bureaucracy 0.10 / Military 0.20 / Business 0.15 /
/// Labor 0.10 / Citizens 0.15" — sums to exactly `1.0`.
const FACTION_GROUP_INFLUENCE: [f32; GROUP_COUNT] = [0.20, 0.10, 0.10, 0.20, 0.15, 0.10, 0.15];

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
            capacity: spec.capacity,
            infrastructure: spec.infrastructure,
            port: spec.port,
            mobilized: 0.0,
            unrest: 0.0,
            occupation: 0.0,
            occupier: None,
            occupation_kind: None,
            links: Vec::new(),
            devastation: 0.0,
            construction: None,
            import_flow: 0.0,
        })
        .collect();

    let strait_zone_of = |a: u32, b: u32| -> Option<SeaZoneId> {
        STRAIT_ZONE_SPECS
            .iter()
            .find(|&&(sa, sb, _)| (sa, sb) == (a, b) || (sa, sb) == (b, a))
            .map(|&(_, _, zone)| SeaZoneId(zone))
    };

    for &(a, b, kind) in &LINK_SPECS {
        let strait_zone = strait_zone_of(a, b);
        regions[a as usize].links.push(Link { to: RegionId(b), kind, strait_zone });
        regions[b as usize].links.push(Link { to: RegionId(a), kind, strait_zone });
    }

    let sea_zones: Vec<SeaZone> = SEA_ZONE_SPECS
        .iter()
        .enumerate()
        .map(|(i, spec)| SeaZone {
            id: SeaZoneId(i as u32),
            name: spec.name.to_string(),
            coast: spec.coast.iter().map(|&r| RegionId(r)).collect(),
            adjacent: spec.adjacent.iter().map(|&z| SeaZoneId(z)).collect(),
            control: vec![0.0; FACTION_SPECS.len()],
        })
        .collect();

    let factions: Vec<Faction> = FACTION_SPECS
        .iter()
        .enumerate()
        .map(|(i, spec)| Faction {
            id: FactionId(i as u32),
            name: spec.name.to_string(),
            capital: RegionId(spec.capital),
            manpower: FACTION_MANPOWER,
            stock: FACTION_STOCK,
            conscription: FACTION_CONSCRIPTION,
            industry_priority: FACTION_INDUSTRY_PRIORITY,
            civilian_ration: crate::balance::CIVILIAN_RATION_DEFAULT,
            war_support: FACTION_WAR_SUPPORT,
            stability: FACTION_STABILITY,
            shortage: 0.0,
            shortage_by_good: [0.0; crate::good::GOOD_COUNT],
            casualties: 0.0,
            supply_ratio: 1.0,
            import_plan: FACTION_IMPORT_PLAN,
            logistics_priority: FACTION_LOGISTICS_PRIORITY,
            group_support: FACTION_GROUP_SUPPORT,
            group_influence: FACTION_GROUP_INFLUENCE,
            machinery_output_ratio: 0.0,
            strike_days: 0,
            regime_change_days: 0,
            protest_active: false,
            mutiny_active: false,
            capital_flight_active: false,
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
            let station = Station::Region(location);
            let id = UnitId(units.len() as u32);
            units.push(Unit {
                id,
                owner: faction_id,
                name: format!("{} Corps {}", spec.name, i + 1),
                station,
                movement: None,
                manpower: crate::balance::UNIT_MANPOWER,
                equipment: crate::balance::UNIT_EQUIPMENT,
                organization: crate::balance::UNIT_ORG,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: station,
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
        sea_zones,
        day: 0,
    }
}

/// Number of regions in the fixed MVP map — used by `observation::ENCODING_LEN`.
pub const REGION_COUNT: usize = REGION_SPECS.len();

/// Number of sea zones in the fixed MVP map — used by
/// `observation::ENCODING_LEN` (Stage 2D).
pub const SEA_ZONE_COUNT: usize = SEA_ZONE_SPECS.len();
