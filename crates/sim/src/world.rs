//! The map (regions, links, terrain) and the mutable game state
//! (factions, units, supply) that every tick system reads and writes.

use crate::balance::{INFRA_DAMAGE_SHARE, NODE_BASE, NODE_INFRA, NODE_PORT, WORKFORCE_SHARE};
use crate::construction::Construction;
use crate::diplomacy::Diplomacy;
use crate::event::Event;
use crate::focus::NationalFocus;
use crate::good::{Good, GOOD_COUNT};
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, TransportNodeId, UnitId};
use crate::logistics::SupplyLeftover;
use crate::military::Unit;
use crate::transport::{TransportLine, TransportNode};

/// Stage 2D (docs/phase2-spec.md "Stage 2D — 海軍・制海権・海上封鎖"): the two
/// kinds of terrain a `Unit` can occupy. A land unit is always
/// `Station::Region`; a fleet is always `Station::Sea` — the two never mix,
/// which is what lets every region-scoped system (`units_in`,
/// `has_enemy_units`, land `tick_combat`/`tick_occupation`) keep working
/// unmodified: `Station::Sea` can never match a `Station::Region` filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Domain {
    Land,
    Sea,
    /// Stage 10A (docs/phase10-spec.md "1. 基地"): a unit based at a
    /// `transport::TransportNodeKind::Airfield` node - see `Station::
    /// Airfield`'s own doc for why the third `Station` variant, not a
    /// sidecar field, is what carries this.
    Air,
}

impl Domain {
    /// Lowercase English key, the same `Good::key()`/`Terrain::key()`
    /// convention - used everywhere a `Domain` crosses a text boundary
    /// (`action::Action::RecruitUnit`'s wire encoding in both
    /// `archipelago-api`/`apps/game`'s own `action_codec.rs`) so encode and
    /// decode can never drift onto two different three-way spellings.
    pub const fn key(self) -> &'static str {
        match self {
            Domain::Land => "land",
            Domain::Sea => "sea",
            Domain::Air => "air",
        }
    }

    pub fn from_key(key: &str) -> Option<Domain> {
        match key {
            "land" => Some(Domain::Land),
            "sea" => Some(Domain::Sea),
            "air" => Some(Domain::Air),
            _ => None,
        }
    }
}

/// Where a `Unit` currently is: a land region, a sea zone, or (Stage 10A)
/// an airfield. Replaces the Phase 1/2A-2C `Unit::location: RegionId`
/// (docs/phase2-spec.md Stage 2D: "Unit の location: RegionId を station:
/// Station に置き換える").
///
/// docs/phase10-spec.md "1. 基地" leaves it to the implementation whether
/// an air unit's location extends this enum or is held another way, on the
/// condition that whatever is chosen keeps the exhaustive, wildcard-free
/// matching every existing `Station` consumer already relies on to catch a
/// new domain at compile time rather than silently mishandle it. A
/// sidecar field on `Unit` (`airfield: Option<TransportNodeId>`, populated
/// exactly when some other field says "this is an air unit") would
/// duplicate the same "existence lives in two places" shape `transport::
/// TransportNodeKind::Port`'s own doc already flags as the thing to avoid -
/// two facts that must be kept in lockstep by hand instead of one that
/// simply cannot disagree with itself. A third `Station` variant has no
/// such seam: `Unit::station` is already the single source of truth for
/// "where is this unit and therefore what domain is it," for land and sea
/// alike, so extending it is the same design applied a third time, not a
/// new one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Station {
    Region(RegionId),
    Sea(SeaZoneId),
    Airfield(TransportNodeId),
}

impl Station {
    pub fn domain(self) -> Domain {
        match self {
            Station::Region(_) => Domain::Land,
            Station::Sea(_) => Domain::Sea,
            Station::Airfield(_) => Domain::Air,
        }
    }

    pub fn region(self) -> Option<RegionId> {
        match self {
            Station::Region(r) => Some(r),
            Station::Sea(_) => None,
            Station::Airfield(_) => None,
        }
    }

    pub fn sea_zone(self) -> Option<SeaZoneId> {
        match self {
            Station::Region(_) => None,
            Station::Sea(z) => Some(z),
            Station::Airfield(_) => None,
        }
    }

    /// Stage 10A: the airfield node an air unit is based at, `None` for a
    /// land/sea unit - the domain-generic accessor `crate::air`'s own
    /// per-node demand/supply functions key off, mirroring `region()`/
    /// `sea_zone()` exactly.
    pub fn airfield(self) -> Option<TransportNodeId> {
        match self {
            Station::Region(_) => None,
            Station::Sea(_) => None,
            Station::Airfield(n) => Some(n),
        }
    }
}

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

    /// Lowercase English key used by `scenario`'s JSON schema (Stage 6A,
    /// docs/phase6-spec.md "Stage 6A"), the same `Good::key()` convention.
    pub const fn key(self) -> &'static str {
        match self {
            Terrain::Plain => "plain",
            Terrain::Hill => "hill",
            Terrain::Mountain => "mountain",
            Terrain::Urban => "urban",
        }
    }

    pub fn from_key(key: &str) -> Option<Terrain> {
        match key {
            "plain" => Some(Terrain::Plain),
            "hill" => Some(Terrain::Hill),
            "mountain" => Some(Terrain::Mountain),
            "urban" => Some(Terrain::Urban),
            _ => None,
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

    /// Lowercase English key used by `scenario`'s JSON schema (Stage 6A,
    /// docs/phase6-spec.md "Stage 6A"), the same `Good::key()` convention.
    /// `Sea` has no key: it's never authored in a scenario file (it marks a
    /// unit's own transit through a `SeaZone`, not a `Region`-to-`Region`
    /// link - see `Station`), so `from_key` never accepts it either.
    pub const fn key(self) -> &'static str {
        match self {
            LinkKind::Rail => "rail",
            LinkKind::Road => "road",
            LinkKind::Tunnel => "tunnel",
            LinkKind::Strait => "strait",
            LinkKind::Sea => "sea",
        }
    }

    pub fn from_key(key: &str) -> Option<LinkKind> {
        match key {
            "rail" => Some(LinkKind::Rail),
            "road" => Some(LinkKind::Road),
            "tunnel" => Some(LinkKind::Tunnel),
            "strait" => Some(LinkKind::Strait),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Link {
    pub to: RegionId,
    pub kind: LinkKind,
    /// Stage 2D (docs/phase2-spec.md "海峡リンクとの対応"): the sea zone a
    /// `Strait` link physically passes through, if any — sea control there
    /// throttles this link's throughput and movement speed. `None` for every
    /// non-`Strait` link and for the 中国—九州 `Tunnel` deliberately: a
    /// tunnel is unaffected by who controls the sea above it, which is the
    /// whole point of it staying open under blockade.
    pub strait_zone: Option<SeaZoneId>,
}

/// Which system currently drives a region's shared `occupation`/`occupier`
/// meter (Stage 3A external code review fix — Fix 2/3, docs/phase3-spec.md
/// "地方独立運動"): `military::tick_occupation`'s real invasion progress, or
/// `politics::tick_separatism`'s political drift toward the region's `core`
/// faction. Both systems read and write the same two fields, so without an
/// explicit marker a coincidence (the invader happens to be the same faction
/// separatism was already drifting toward) can't be told apart from a
/// genuine continuation — this is exactly what let a real invasion inherit
/// separatist progress, or a stale separatist marker survive under a
/// military occupier's decay logic. `None` (on `Region::occupation_kind`)
/// means the meter is currently idle (`occupation == 0`, `occupier == None`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OccupationKind {
    Military,
    Separatist,
}

/// One faction's share of the contested airspace over a region, `0.0..=1.0`
/// (docs/phase10-spec.md "2. 制空権": "0 を中立として... 比率で導く"). Per
/// docs/conventions.md §1 ("政策値は素の `f32` ではなく 0〜1 を保証する型で
/// 持ち、不正な値をそもそも構築できなくする"): only ever constructed
/// through `new` or reached at `NEUTRAL`, so a value outside `0.0..=1.0` can
/// never exist regardless of how many places later read it - the same
/// discipline `transport::Condition`/`transport::Capacity`/
/// `DominationShare` already apply to their own bounded quantities.
/// `air::tick_air_superiority` is the type's sole writer, every tick, from
/// current air-unit positions and strength alone - never sampled once at
/// order-issue time and cached (CLAUDE.md's own "発令時点の値を焼き込まな
/// い"), which is what gives `Region::air_superiority` its required
/// recovery path: a region every reaching faction's air units have since
/// left or lost is simply recomputed to `NEUTRAL` the next tick, not
/// nudged back down from some remembered peak.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct AirSuperiority(f32);

impl AirSuperiority {
    /// No faction's air power reaches a region at all - every region starts
    /// here, and every region returns here once no reaching faction has any
    /// air unit left.
    pub const NEUTRAL: AirSuperiority = AirSuperiority(0.0);

    pub fn new(value: f32) -> Option<AirSuperiority> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Some(AirSuperiority(value))
        } else {
            None
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
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
    /// Port capacity *magnitude* only, in `0.0..` - never read as an
    /// existence flag (`port > 0.0`) since Stage 9B: whether this region has
    /// a port at all is `World::has_port_node`'s question to answer, off the
    /// transport layer's own `TransportNodeKind::Port` node, so the two
    /// representations can never disagree about which regions have one
    /// (`scenario::ScenarioError::PortNodeWithoutRegionPort`/
    /// `RegionPortWithoutPortNode` keep them in lockstep). This field still
    /// drives `trade::tick_imports`' import ceiling, `logistics::
    /// recompute_supply`'s import injection at that node
    /// (`balance::PORT_SUPPLY_PER_PORT`), `node_throughput`, and `value`.
    pub port: f32,
    pub mobilized: f32,
    pub unrest: f32,
    pub occupation: f32,
    pub occupier: Option<FactionId>,
    /// See `OccupationKind`'s doc: which of `military::tick_occupation` or
    /// `politics::tick_separatism` currently owns `occupation`/`occupier`.
    /// Kept in lockstep with `occupier` (`Some` iff `occupier.is_some()`).
    pub occupation_kind: Option<OccupationKind>,
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
    /// Stage 2C (docs/phase2-spec.md "1. 海上輸入"): the volume this port
    /// actually imported *today*, recomputed every tick by
    /// `trade::tick_imports`. Kept per region rather than folded into a
    /// single national figure — Stage 2D blockades individual ports, which
    /// only a per-port number can express. Zero for non-port and for
    /// contested/foreign regions.
    pub import_flow: f32,
    /// Stage 7A (docs/phase7-spec.md "地域の座標"): this region's map
    /// coordinate, `[x, y]`, as the scenario authored it
    /// (`scenario::RegionDef::position`'s doc) - required, so this is never
    /// absent once a `World` exists at all: `scenario::parse_position`
    /// rejects a scenario missing it before `build_world` ever runs.
    ///
    /// No longer cosmetic-only as of Stage 10B (docs/phase10-spec.md "2.
    /// 制空権": "半径は... 地理的な距離で測る... 地域は position を持って
    /// いるのでこれを使う") - a deliberate, spec-mandated reversal of Phase
    /// 7A's own original invariant ("座標はシミュレーションに一切影響しな
    /// い"), whose old enforcement mechanism (the `scenarios/mvp.json`
    /// seed-1/720-day hash) is exactly the one CLAUDE.md records retiring.
    /// `air::tick_air_superiority` is now the one tick system that reads
    /// this field for more than rendering, comparing it against every
    /// `transport::TransportNodeKind::Airfield` node's own region's
    /// `position` to decide whether that airfield's committed air power
    /// reaches this region at all (`balance::AIR_OPERATING_RADIUS_KM`).
    ///
    /// All three shipped scenarios now carry that coordinate on the same
    /// real kilometre scale. `scenarios/japan_hex.json` always did
    /// (`tools/hexmap/build_scenario.py`'s own `x_m / 1000.0`);
    /// `mvp.json`/`japan47.json` used to be an unscaled schematic layout
    /// with no physical unit at all, which was harmless only as long as no
    /// scenario placed a `Domain::Air` unit - a gap this repo's own AI does
    /// close at runtime (`agents::` recruits `Domain::Air` units in every
    /// scenario, none pre-placed at load time), so the mismatch was real
    /// and measured, not theoretical: at the old scale, 68%/75% of mvp/
    /// japan47's own regions sat inside one `AIR_OPERATING_RADIUS_KM`
    /// radius of any given region on average, against `japan_hex`'s own
    /// 25% - air superiority there was close to global reach no matter
    /// what the constant said. Fixed by rescaling `mvp.json`/`japan47.json`
    /// themselves (`tools/rescale_positions.py`, run once and committed -
    /// its own doc has the full derivation and citation for every
    /// prefecture-to-region grouping) onto the exact same projection
    /// `tools/hexmap/hexgrid.py` already uses for `japan_hex`, rather than
    /// by touching this constant to paper over the mismatch. Post-rescale
    /// the same three figures are 30%/34%/25%.
    pub position: [f32; 2],
    /// Stage 10B (docs/phase10-spec.md "2. 制空権"): each faction's current
    /// share of the contested airspace over this region, recomputed every
    /// tick by `air::tick_air_superiority` from whichever airfields'
    /// committed air power currently reaches it - the region-domain mirror
    /// of `SeaZone::control`, one domain further (same `power[f] /
    /// sum(power[*])` shape, all `AirSuperiority::NEUTRAL` when nobody
    /// reaches). Sized to `World::factions.len()`. Never sampled once and
    /// cached: a region a faction's air units have since left or lost
    /// returns to `NEUTRAL` the very next tick, the "回復経路" docs/phase10
    /// -spec.md's own Stage 10B acceptance criterion requires.
    pub air_superiority: Vec<AirSuperiority>,
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
    /// that currently equips or sustains a fielded unit — this region's
    /// contribution to war-relevant industry (Stage 2A redefinition of the
    /// Phase 1 `industry` field that `World::industry_total` and
    /// `supply_source`/`value` below depend on, which in turn feed
    /// `agents::unit_cap`'s recruitment ceiling).
    ///
    /// **Deliberately not "every non-`Food` `Good`."** Stage 11A
    /// (docs/phase11-spec.md §3) gave `Armour`/`Artillery` real,
    /// region-varying capacity as data before any unit type drew on either
    /// stock ("部隊種別は入れない"), and this list deliberately excluded them
    /// until that changed - blanket-iterating `ALL_GOODS` back then would
    /// have inflated `unit_cap`/`supply_source`/`value` the instant the data
    /// existed, with no matching demand anywhere, which Stage 11A's own
    /// acceptance bar ("3 シナリオの結果が変わらない") forbade.
    ///
    /// Stage 11B wires up exactly that: `military::Branch` now draws
    /// `Armour`/`Artillery` (`Branch::equipment_good`), and `Domain::Sea`/
    /// `Domain::Air` now draw their own `Good::Naval`/`Good::Aircraft`
    /// instead of sharing `Good::Infantry` (`Good`'s own module doc) - so
    /// all five now genuinely equip or sustain a fielded unit, the same test
    /// this list has always applied, and belong in the sum on identical
    /// terms to `Good::Infantry`.
    pub fn industry_total(&self) -> f32 {
        const INDUSTRY_GOODS: [Good; 9] = [
            Good::Energy,
            Good::Steel,
            Good::Machinery,
            Good::Munitions,
            Good::Infantry,
            Good::Armour,
            Good::Artillery,
            Good::Naval,
            Good::Aircraft,
        ];
        INDUSTRY_GOODS.iter().map(|&good| self.effective_capacity(good)).sum()
    }

    pub fn supply_source(&self) -> f32 {
        self.supply_source_blockaded(false)
    }

    /// `supply_source`, but with the port contribution zeroed when Stage 2D
    /// sea blockade (docs/phase2-spec.md "2. 港の封鎖": "supply_source の
    /// 港湾寄与も 0 にする") has cut this region's port off — the port
    /// itself still physically exists, but nothing can move through it, so
    /// it stops contributing to the region's own supply-network base.
    /// `industry_total`'s contribution is untouched: a blockaded port can
    /// still relay what its own hinterland produces.
    pub fn supply_source_blockaded(&self, blockaded: bool) -> f32 {
        self.industry_total() * 0.5 + if blockaded { 0.0 } else { self.port * 4.0 }
    }

    /// Stage 2C node-side throughput cap (docs/phase2-spec.md "2. 港湾・
    /// インフラによるノード側の上限"): how much this region can relay
    /// *itself*, independent of what any single incoming link allows.
    /// `logistics::recompute_supply` applies this as an extra `min()` term
    /// alongside each link's own `max_throughput()`, so a region with wrecked
    /// or absent infrastructure chokes supply passing through it even when
    /// the rail line into it is intact.
    pub fn node_throughput(&self) -> f32 {
        NODE_BASE + self.effective_infrastructure() * NODE_INFRA + self.port * NODE_PORT
    }

    pub fn labor_ratio(&self) -> f32 {
        let workforce = self.population * WORKFORCE_SHARE;
        ((workforce - self.mobilized) / workforce).clamp(0.15, 1.0)
    }

    /// Rough strategic value of this region, used by AI agents to weigh targets.
    pub fn value(&self) -> f32 {
        self.industry_total() * 1.5 + self.population * 0.05 + self.port * 3.0
    }

    /// Stage 10D (docs/phase10-spec.md "Stage 10D"): the highest
    /// `air_superiority` share held by any faction other than `faction` -
    /// `SeaZone::enemy_control_max`'s exact shape, one domain further, for
    /// `Observation::encode`'s own per-region block to read the same way that
    /// function's own `enemy_control_max` feeds the per-sea-zone one.
    /// Deliberately *not* `World::hostile_air_superiority_max` - that
    /// additionally requires the other faction to be at war with `faction`,
    /// which is the right question for the interdiction throttle
    /// (`air::air_line_factor`) but the wrong one here: `enemy_control_max`'s
    /// own observation field asks "any other faction, war or peace", and this
    /// mirrors it rather than quietly answering a different question under
    /// the same naming convention.
    pub fn enemy_air_superiority_max(&self, faction: FactionId) -> f32 {
        self.air_superiority
            .iter()
            .enumerate()
            .filter(|&(f, _)| f != faction.index())
            .map(|(_, &s)| s.get())
            .fold(0.0f32, f32::max)
    }
}

/// Stage 2D (docs/phase2-spec.md "海域"): a body of water, separate from the
/// region graph, that fleets occupy and fight over. `control` is
/// recomputed every tick by `naval::tick_sea_control` from the
/// `combat_power` of every faction's fleets currently in the zone; a `Vec`
/// sized to `World::factions.len()` rather than a fixed-size array, matching
/// the rest of the codebase's convention of deriving faction-indexed
/// collections from `factions.len()` at runtime (`economy`, `politics`,
/// `trade`, `military::tick_combat` all do the same) instead of a hardcoded
/// faction count.
#[derive(Clone, Debug)]
pub struct SeaZone {
    pub id: SeaZoneId,
    pub name: String,
    /// Regions this zone's coastline touches — a port here can trade
    /// through, and be blockaded via, this zone. Not every coastal region
    /// need have a port.
    pub coast: Vec<RegionId>,
    /// Sea zones a fleet can sail directly into from this one.
    pub adjacent: Vec<SeaZoneId>,
    /// Sea control per faction, `power[f] / sum(power[*])`, `0.0` for every
    /// faction when no fleet is present anywhere in the zone.
    pub control: Vec<f32>,
}

impl SeaZone {
    /// The highest `control` held by any faction other than `faction` — the
    /// "敵の制海権の最大値" the strait-throttle and port-blockade effects
    /// (docs/phase2-spec.md Stage 2D, effects 1 and 2) both key off.
    pub fn enemy_control_max(&self, faction: FactionId) -> f32 {
        self.control
            .iter()
            .enumerate()
            .filter(|&(f, _)| f != faction.index())
            .map(|(_, &c)| c)
            .fold(0.0f32, f32::max)
    }
}

/// A fraction in `(0.0, 1.0]` - the map-share threshold
/// `VictoryCondition::Domination` requires. Per docs/conventions.md §1
/// ("政策値は素の `f32` ではなく 0〜1 を保証する型で持ち、不正な値をそもそも
/// 構築できなくする"): only ever constructed through `new`, so a threshold
/// of `0.0` (every scenario would "dominate" from the very first tick, since
/// every faction always holds at least one region) or anything above `1.0`
/// (unsatisfiable - no group can ever hold more than the whole map) can
/// never exist as a `DominationShare` at all, and every call site that has
/// one in hand never needs to re-check its range.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct DominationShare(f32);

impl DominationShare {
    pub fn new(share: f32) -> Option<Self> {
        if share.is_finite() && share > 0.0 && share <= 1.0 {
            Some(DominationShare(share))
        } else {
            None
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// One victory condition a scenario can declare active
/// (design.md §5: "勝利条件は一つに限定しない"). A scenario's required
/// `victory` array (`scenario::parse_victory`) names the ones in play, in
/// the order `Simulation::outcome` checks them - see that method's doc.
/// `Domination`'s threshold lives in the type itself (`DominationShare`)
/// rather than as a bare `f32` checked at each use site, per
/// docs/conventions.md §1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VictoryCondition {
    /// Exactly one faction still alive - the only rule this project
    /// implemented before scenario-declared victory conditions existed.
    /// `scenarios/mvp.json` declares only this, which is why its behaviour
    /// (and `--json` hash) is unchanged.
    Conquest,
    /// Every surviving faction belongs to one mutually-allied group (a
    /// chain of `Stance::Alliance` edges) - reachable the instant a bloc
    /// has destroyed every faction outside it, without that bloc ever
    /// having to turn on its own allies (docs/future-work.md "japan47 が
    /// 720 日で決着しない": `HeuristicAgent` never issues `DeclareWar` or
    /// `BreakTreaty` against an ally, so `Conquest` alone could freeze
    /// forever above `alive.len() == 1` once the map settles into rival
    /// blocs).
    Coalition,
    /// A faction, or its allied group's *combined* holdings, control at
    /// least `DominationShare` of the map's regions - the same "allied
    /// group" `Coalition` uses, so a bloc's territory counts together
    /// rather than needing one member to hold the whole share alone.
    Domination(DominationShare),
}

/// A scenario's required, **non-empty** set of declared `VictoryCondition`s,
/// in the order `Simulation::outcome`'s `evaluate_victory` checks them - see
/// `VictoryCondition`'s doc. Per docs/conventions.md §1 ("実行時の検証よりも、
/// 不正な状態を表現できなくすることを優先する"): an empty declaration would
/// leave `evaluate_victory` with nothing to ever check, so no game reaching
/// even `alive.len() == 1` could ever end - exactly the permanently-
/// unreachable-victory state requiring a `victory` declaration was
/// introduced to prevent in the first place. Rather than let that state be
/// constructed and check for it (`world.victory.is_empty()`) at every use
/// site, `new` is the only way to build one and refuses an empty `Vec`
/// outright, so a `VictoryDeclaration` that exists is proof it names at
/// least one condition - see `scenario::parse_victory`'s doc and
/// `empty_victory_declaration_is_rejected`.
#[derive(Clone, Debug, PartialEq)]
pub struct VictoryDeclaration(Vec<VictoryCondition>);

impl VictoryDeclaration {
    /// `None` iff `conditions` is empty - the only way construction can
    /// fail.
    pub fn new(conditions: Vec<VictoryCondition>) -> Option<Self> {
        if conditions.is_empty() {
            None
        } else {
            Some(VictoryDeclaration(conditions))
        }
    }

    pub fn as_slice(&self) -> &[VictoryCondition] {
        &self.0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, VictoryCondition> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a VictoryDeclaration {
    type Item = &'a VictoryCondition;
    type IntoIter = std::slice::Iter<'a, VictoryCondition>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
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
    /// Worst of `shortage_by_good[Food]`/`[Energy]`/`[Machinery]` - a single
    /// scalar `politics::tick_politics` uses for unrest pressure. Consumers
    /// that need to know *which* commodity is actually short (e.g. deciding
    /// what to import) must read `shortage_by_good` instead - collapsing to
    /// this one number loses exactly that distinction.
    pub shortage: f32,
    pub casualties: f32,
    pub supply_ratio: f32,
    /// External code review fix (Stage 2C): unmet civilian demand this tick
    /// for `Food`, `Energy` and `Machinery` specifically, in `0..=1`,
    /// indexed by `Good::index()` (every other index stays `0.0` - no other
    /// good is civilian-rationed). `economy::tick_economy` sets these from
    /// the same per-commodity `consume()` calls that already fed the
    /// collapsed `shortage` scalar above; `agents::set_trade_policy` reads
    /// this directly so an import plan for one commodity isn't sized off
    /// whichever commodity happens to be worst.
    pub shortage_by_good: [f32; GOOD_COUNT],
    /// Stage 2C sea imports (docs/phase2-spec.md "1. 海上輸入",
    /// `Action::SetImportPlan`): desired import rate per commodity, indexed
    /// by `Good::index()`. Only `Food` and `Energy` are ever nonzero — the
    /// action layer rejects any other good — but the field stays
    /// `[f32; GOOD_COUNT]` so it lines up with every other per-commodity
    /// array in the codebase.
    pub import_plan: [f32; GOOD_COUNT],
    /// Stage 2C per-commodity delivery (docs/phase2-spec.md "3. 品目別の
    ///到達率", `Action::SetLogisticsPriority`): priority weight per
    /// commodity used to split the shared regional supply throughput
    /// between Munitions upkeep and Arms delivery, the same mechanism
    /// `industry_priority` already uses to split a contended production
    /// input. Only `Munitions` and `Arms` are read by
    /// `logistics::distribute_supply`.
    pub logistics_priority: [f32; GOOD_COUNT],
    /// Stage 3A (docs/phase3-spec.md "Stage 3A — 国内政治勢力", design.md
    /// §11): support (`0..100`) for each of the seven domestic political
    /// `Group`s, indexed by `Group::index()`. Updated by
    /// `politics::tick_politics` with a target-approach model
    /// (`balance::GROUP_ADAPT_RATE`) — never accumulated directly — so no
    /// group can pin at a boundary it can't recover from.
    pub group_support: [f32; GROUP_COUNT],
    /// Fixed weight each `Group` carries in `stability`'s influence-weighted
    /// average, indexed by `Group::index()`. Always sums to `1.0`; set once
    /// at scenario build time and never changed by any Stage 3A system
    /// (Stage 3C's national foci may adjust this later).
    pub group_influence: [f32; GROUP_COUNT],
    /// Today's Machinery output over its input-unconstrained potential,
    /// `0..1` (`economy::tick_economy`) — the "生産（Machinery）が好調" signal
    /// `politics::tick_politics` reads for Business's group-support target.
    /// Kept as a ratio (not an absolute figure) so it stays meaningful
    /// regardless of how much Machinery capacity a faction actually holds.
    pub machinery_output_ratio: f32,
    /// Stage 3A political events (docs/phase3-spec.md "政治イベント"): the
    /// two fixed-duration ones. `0` means inactive; set to
    /// `balance::STRIKE_DAYS`/`balance::REGIME_CHANGE_DAYS` on trigger and
    /// counted down to `0` by `politics::tick_politics` — a real, spendable
    /// duration, not a condition re-checked every tick, so each event
    /// always runs its course once started (see their doc comments in
    /// `balance.rs`).
    pub strike_days: u32,
    pub regime_change_days: u32,
    /// Stage 3A political events (docs/phase3-spec.md "政治イベント"): the
    /// three live conditions, re-evaluated from `group_support` every tick
    /// by `politics::tick_politics` (no timer — they end the instant the
    /// triggering group's support recovers above threshold). These flags
    /// exist so systems elsewhere (`military::tick_recovery`,
    /// `construction::tick_construction`, `economy::tick_economy`) can read
    /// "is this currently in effect" without themselves depending on
    /// `crate::group`, and so `politics::tick_politics` can detect the
    /// rising edge to log an `Event` only once per episode rather than every
    /// tick it stays active.
    pub protest_active: bool,
    pub mutiny_active: bool,
    pub capital_flight_active: bool,
    /// Stage 3C (docs/phase3-spec.md "Stage 3C — 国家方針"): the long-term
    /// strategic posture this faction has committed to. Read only through
    /// `focus::active()` by every system that applies a focus modifier -
    /// never directly - since a mid-switch faction's `national_focus` here
    /// already reflects the *new* target even though its effects aren't live
    /// yet (see `focus_transition_days`).
    pub national_focus: NationalFocus,
    /// Days left until a `national_focus` switch actually takes effect
    /// (`Action::SetNationalFocus`, `focus::tick_national_focus`); `0` means
    /// `national_focus` is already active. A real, decrementing budget, not
    /// a ratio re-applied to a remainder - see `focus.rs`'s module doc for
    /// why that's what keeps rapid `SetNationalFocus` spam from ever
    /// shortening or stacking anything.
    pub focus_transition_days: u32,
    pub alive: bool,
}

#[derive(Clone, Debug)]
pub struct World {
    pub regions: Vec<Region>,
    pub factions: Vec<Faction>,
    pub units: Vec<Unit>,
    /// The amount of Munitions+Arms throughput that actually flowed to each
    /// region's own units this tick, indexed by `RegionId` - Stage 9B
    /// (docs/phase9-spec.md "2. 補給を有限流量にする") redefinition of what
    /// this array means. Before Stage 9B this was a *ceiling* the region
    /// could draw on regardless of how much its own units actually needed
    /// (best-path bottleneck reachability over `Region::links`, never
    /// consumed by contending demand); it is now the *delivered* amount from
    /// `logistics::recompute_supply`'s capacity-constrained flow over the
    /// transport network, already bounded by that region's own demand, so it
    /// can never exceed what `logistics::distribute_supply` goes on to
    /// actually serve. Every reader that used to treat this as an
    /// independent-of-demand capacity figure (`node_throughput_limits_supply`
    /// is the one place that mattered) was re-examined for Stage 9B - see
    /// that test's own doc for how it was adapted.
    ///
    /// Always `supply_by_faction[r][regions[r].owner.index()]` - kept as its
    /// own field rather than computed on every read since it is the one
    /// entry almost every reader outside `logistics`/`naval` themselves ever
    /// wants (JSON/observation export, the API, the headless report, a
    /// fleet's own facing-port lookup).
    pub supply: Vec<f32>,
    /// The same Munitions+Arms throughput, indexed by `[RegionId][FactionId]`
    /// rather than `RegionId` alone - the post-Stage-9B-fix generalization
    /// that lets a faction's demand be met even in a region it does not own.
    /// `supply[r]` is this region's own owner's entry; every other faction's
    /// entry here is `0.0` unless that faction has alive units physically
    /// standing in `r` (an occupier mid-invasion, before
    /// `military::tick_occupation` flips `Region::owner`) - see
    /// `logistics::compute_transport_flow`'s own doc, "Which lines an
    /// occupier may use", for how that demand is routed. Read by
    /// `logistics::distribute_supply`'s and `logistics::land_unit_supply_avail`'s
    /// non-owner branches in place of the pre-fix `0.4 * a neighbor's
    /// world.supply` projection, which read near-zero whenever that neighbor
    /// happened to have no units of its own to draw the figure up regardless
    /// of how much the network could actually carry.
    pub supply_by_faction: Vec<Vec<f32>>,
    /// The Munitions+Arms throughput that actually flowed to each sea
    /// zone's fleets this tick, indexed by `[SeaZoneId][FactionId]` - the
    /// sea-domain counterpart of `supply_by_faction`, read by
    /// `naval::fleet_unit_supply_avail`/`fleet_demand_and_avail`.
    ///
    /// Defect 3 fix (the third sibling of the occupier-supply defect
    /// `supply_by_faction`'s own doc already explains two fixes for): a
    /// fleet's demand used to be answered by a separate structural,
    /// never-consumed figure - `world.port_capacity`, the widest-path
    /// (max-min-capacity) source capacity `logistics::
    /// compute_port_source_capacity` found reaching a `Port` node, entirely
    /// independent of `region_demand` - so two fleets facing the same port
    /// each read that port's *entire* structural capacity independently,
    /// since nothing there was ever debited by a grant. Both are removed:
    /// fleet demand is now folded straight into `logistics::
    /// compute_transport_flow`'s own contended, capacity-constrained rounds
    /// (via each `Port` node's `EdgeKind::PortToSea` edge into its own
    /// sea-zone demand sink), so this field is exactly as demand-bounded and
    /// as genuinely finite as `supply_by_faction` is for land.
    pub supply_sea: Vec<Vec<f32>>,
    /// Stage 10A (docs/phase10-spec.md "1. 基地"): the Munitions+Arms
    /// throughput that actually flowed to each `transport::TransportNode`'s
    /// own air-unit demand this tick, indexed by `[TransportNodeId]
    /// [FactionId]` - the air-domain counterpart of `supply_sea`, read by
    /// `air::air_unit_supply_avail`/`air_demand_and_avail`. Nonzero only at
    /// a node whose `kind` is `transport::TransportNodeKind::Airfield` (the
    /// only kind `logistics::build_transport_graph` ever wires an
    /// `air_demand` sink onto) and only a faction with an alive unit
    /// actually stationed there (`crate::air::air_demand`) - every other
    /// cell simply never has anything granted to it, the same "row exists,
    /// most of it strictly zero" shape `supply_by_faction` already has for
    /// every faction that isn't a region's owner or an occupier there. This
    /// is folded into `compute_transport_flow`'s own contended,
    /// capacity-constrained rounds from the start (docs/phase10-spec.md's
    /// own "航空部隊の補給を、既存の流量計算に最初から入れる" - the Phase 9
    /// fleet-demand defect `supply_sea`'s own doc explains is exactly the
    /// mistake this stage does not get to repeat a third time), never a
    /// second, separately-computed figure.
    pub supply_air: Vec<Vec<f32>>,
    /// `codex review` P1 (second round, `logistics::SupplyLeftover`'s own
    /// doc): the transport network's own per-resource capacity this tick's
    /// `logistics::recompute_supply` did *not* hand to any (region, faction)
    /// or (sea zone, faction) demand it already knew about - reset fresh
    /// every time `recompute_supply` runs, then spent down, once per
    /// arriving unit, by `logistics::commit_instantaneous_land_grant`/
    /// `commit_instantaneous_sea_grant` for a unit that finishes moving into
    /// a region/zone this same tick, so N such arrivals can never together
    /// draw more than this tick's own genuinely unclaimed capacity. Not
    /// `pub`, unlike every field above it: nothing outside `logistics`
    /// itself has a legitimate reason to read or write this directly (the
    /// JSON/observation export, the API, and the headless report all read
    /// `supply`/`supply_by_faction`/`supply_sea` instead, exactly as before
    /// this fix).
    pub(crate) supply_leftover: SupplyLeftover,
    /// Stage 2D (docs/phase2-spec.md "海域"): the map's sea zones, separate
    /// from the region graph.
    pub sea_zones: Vec<SeaZone>,
    /// The transport network's nodes and routes (docs/phase9-spec.md "1. 層
    /// の分離") - a layer separate from both `regions` (politics/economy)
    /// and each region's own `links` (troop movement only, since Stage 9B).
    /// `logistics::recompute_supply` routes actual flow over this network;
    /// `World::port_node`/`has_port_node` is the sole source of truth for
    /// which regions have a port (`Region::port`'s own doc).
    pub transport_nodes: Vec<TransportNode>,
    pub transport_lines: Vec<TransportLine>,
    /// Events emitted by `action::apply_strike_node`/`apply_interdict_line`
    /// the instant a strike or an interdiction actually lands - those
    /// appliers run inside `Simulation::apply`, which has no `&mut
    /// Vec<Event>` of its own to write into, so they queue here instead. The
    /// same shape `Diplomacy::log` already established for `Action::
    /// ProposeTreaty`/`AcceptTreaty`/`RejectTreaty`/`BreakTreaty` (see that
    /// field's own doc); `Simulation::step_timed` drains this into the day's
    /// event list right alongside `Diplomacy::log`, so from the outside
    /// these events appear exactly like every other tick-system event. Not
    /// `pub`: only `action.rs` pushes and only `sim.rs` drains.
    pub(crate) action_log: Vec<Event>,
    pub day: u32,
    /// Stage 3B (docs/phase3-spec.md "Stage 3B — 外交関係と条約"): every
    /// pair's `Stance`/`opinion`/treaty grants and the pending-proposal
    /// queue.
    pub diplomacy: Diplomacy,
    /// This scenario's required, declared victory conditions
    /// (`scenario::parse_victory`), checked by `Simulation::outcome` in
    /// this order - see `VictoryCondition`'s doc. Always non-empty
    /// (`VictoryDeclaration`'s doc): a scenario can never leave this
    /// permanently unable to end the game.
    pub victory: VictoryDeclaration,
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

    /// Alive land units currently at `region`. `Station::Sea` can never
    /// equal `Station::Region(region)`, so this — and every land system
    /// built on it (`has_enemy_units`, `region_power`, land `tick_combat`/
    /// `tick_occupation`) — only ever sees land units; fleets never leak in.
    pub fn units_in(&self, region: RegionId) -> impl Iterator<Item = &Unit> {
        self.units
            .iter()
            .filter(move |unit| unit.alive && unit.station == Station::Region(region))
    }

    /// Stage 3B (docs/phase3-spec.md "Stage 3B"): "enemy" now means
    /// currently at `Stance::War` with `faction`, not merely "a different
    /// owner" - a unit of a faction `faction` holds `Ceasefire`/
    /// `NonAggression`/`Alliance` with no longer pins movement, blocks
    /// recruiting/building, or contests a region for this check. The
    /// pre-Stage-3B behaviour (every other faction always counts as enemy)
    /// is exactly what `Diplomacy::new` reproduces by default - every pair
    /// starts at `Stance::War`, so every existing scenario/test is
    /// unaffected until a treaty actually changes a pair's stance.
    pub fn has_enemy_units(&self, region: RegionId, faction: FactionId) -> bool {
        self.units_in(region)
            .any(|unit| unit.owner != faction && self.diplomacy.is_at_war(faction, unit.owner))
    }

    /// Sum of `combat_power` for a faction's alive units present in `region`.
    pub fn region_power(&self, region: RegionId, faction: FactionId) -> f32 {
        self.units_in(region)
            .filter(|unit| unit.owner == faction)
            .map(Unit::combat_power)
            .fold(0.0, |acc, p| acc + p)
    }

    /// Alive fleets currently in `zone` — the sea-domain counterpart of
    /// `units_in` (see its doc for why the two never overlap).
    pub fn fleets_in(&self, zone: SeaZoneId) -> impl Iterator<Item = &Unit> {
        self.units
            .iter()
            .filter(move |unit| unit.alive && unit.station == Station::Sea(zone))
    }

    /// Stage 3B (docs/phase3-spec.md "Stage 3B"), External code review fix
    /// A1: mirrors `has_enemy_units`'s stance-aware redefinition of "enemy" -
    /// a fleet of a faction `faction` holds `Ceasefire`/`NonAggression`/
    /// `Alliance` with no longer pins fleet movement or triggers combat
    /// supply's `COMBAT_SUPPLY_MULT` for this check. Before this fix, every
    /// other faction's fleet counted as "enemy" here regardless of `Stance`,
    /// so peace never actually reached naval pinning/combat-supply the way
    /// it already did for land via `has_enemy_units`.
    pub fn has_enemy_fleets(&self, zone: SeaZoneId, faction: FactionId) -> bool {
        self.fleets_in(zone)
            .any(|unit| unit.owner != faction && self.diplomacy.is_at_war(faction, unit.owner))
    }

    /// The highest `SeaZone::control` held by a faction currently at
    /// `Stance::War` with `faction` — the stance-aware form of
    /// `SeaZone::enemy_control_max` used by blockade judgement (External
    /// code review fix A1: "a faction at peace should not be blockading its
    /// treaty partner's ports"). A `Ceasefire`/`NonAggression`/`Alliance`
    /// partner's fleet presence, however dominant, never counts toward
    /// blockading a port belonging to `faction` — only a partner actually at
    /// war with it can. `SeaZone::enemy_control_max` itself stays as the raw
    /// "every other faction" figure (strait-crossing throttle and the
    /// hostile-destination check in `action::apply_move` keep using it,
    /// since those are about contested control of the sea itself rather than
    /// a punitive effect targeted at `faction` specifically).
    pub fn hostile_control_max(&self, zone: SeaZoneId, faction: FactionId) -> f32 {
        let zone = self.sea_zone(zone);
        (0..self.factions.len())
            .filter(|&f| f != faction.index() && self.diplomacy.is_at_war(faction, FactionId(f as u32)))
            .map(|f| zone.control[f])
            .fold(0.0f32, f32::max)
    }

    /// Stage 10C (docs/phase10-spec.md "3. 阻止"): `hostile_control_max`'s
    /// exact shape, one domain further - the highest `Region::air_
    /// superiority` share held by a faction currently at `Stance::War` with
    /// `faction`. Deliberately *not* `SeaZone::enemy_control_max`'s raw
    /// "every other faction" form the way `naval::strait_factor` uses for
    /// sea crossings: `hostile_control_max`'s own doc explains that a sea
    /// lane's own throttle is about contested control of the water itself,
    /// not a punitive effect targeted at `faction` - air interdiction has no
    /// such excuse. design.md §8 frames it as an explicitly hostile act
    /// ("敵は...物流拠点を攻撃することも可能"), so an allied, `Ceasefire`,
    /// `NonAggression`, or otherwise at-peace faction's air power over a
    /// region must never throttle `faction`'s own logistics through it,
    /// however dominant that presence is (`codex review` P1: the first
    /// version of this function used the raw form and would have penalized
    /// a faction's own logistics for a friendly air force's presence).
    pub fn hostile_air_superiority_max(&self, region: RegionId, faction: FactionId) -> f32 {
        let region = self.region(region);
        (0..self.factions.len())
            .filter(|&f| f != faction.index() && self.diplomacy.is_at_war(faction, FactionId(f as u32)))
            .map(|f| region.air_superiority[f].get())
            .fold(0.0f32, f32::max)
    }

    /// Sum of `combat_power` for a faction's alive fleets present in `zone`.
    pub fn zone_power(&self, zone: SeaZoneId, faction: FactionId) -> f32 {
        self.fleets_in(zone)
            .filter(|unit| unit.owner == faction)
            .map(Unit::combat_power)
            .fold(0.0, |acc, p| acc + p)
    }

    pub fn sea_zone(&self, id: SeaZoneId) -> &SeaZone {
        &self.sea_zones[id.index()]
    }

    pub fn sea_zone_mut(&mut self, id: SeaZoneId) -> &mut SeaZone {
        &mut self.sea_zones[id.index()]
    }

    /// Sea zones whose coastline includes `region`, in ascending
    /// `SeaZoneId` order (fixed iteration order for determinism) — every
    /// zone a port at `region` can trade through or be blockaded from.
    pub fn zones_touching(&self, region: RegionId) -> Vec<SeaZoneId> {
        self.sea_zones
            .iter()
            .filter(|z| z.coast.contains(&region))
            .map(|z| z.id)
            .collect()
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

    pub fn transport_node(&self, id: crate::ids::TransportNodeId) -> &TransportNode {
        &self.transport_nodes[id.index()]
    }

    pub fn transport_line(&self, id: crate::ids::TransportLineId) -> &TransportLine {
        &self.transport_lines[id.index()]
    }

    /// The first (lowest `TransportNodeId`, so this is deterministic even if
    /// a region ever declared more than one - `docs/phase9-spec.md` "輸送
    /// ノード" allows it in principle, though no shipped scenario does)
    /// `Port` node belonging to `region`, if any. The sole source of truth
    /// for "does this region have a port at all" since Stage 9B - see
    /// `Region::port`'s own doc for why that field is never read as an
    /// existence check any more.
    pub fn port_node(&self, region: RegionId) -> Option<&TransportNode> {
        self.transport_nodes
            .iter()
            .find(|n| n.region == region && n.kind == crate::transport::TransportNodeKind::Port)
    }

    pub fn has_port_node(&self, region: RegionId) -> bool {
        self.port_node(region).is_some()
    }

    /// Whether `region`'s own `Port` node (if any) is currently operational
    /// (`TransportNode::operational`'s own doc) - Stage 10C: `trade::
    /// tick_imports`'s counterpart to `naval::is_port_blockaded`, so a port
    /// wrecked by `Action::StrikeNode` stops accepting imports the same way
    /// `logistics::recompute_supply` already stops routing supply through
    /// it (codex review P2: `tick_imports` used to derive import capacity
    /// from `Region::port` alone and never asked this question at all).
    /// `false` for a region with no `Port` node, matching `has_port_node`.
    ///
    /// **Every** `Port` node of the region is considered, not just the first
    /// (`codex review`, P2). A scenario may declare more than one, and
    /// `logistics::build_transport_graph` already gates each node's own
    /// vertex independently - so answering from `port_node`'s lowest-id node
    /// alone disagreed with the graph in both directions: striking a second
    /// port left imports untouched, and striking the first cut them off even
    /// though another port was still standing. A region's port capacity is
    /// working as long as any of its ports is.
    pub fn port_node_operational(&self, region: RegionId) -> bool {
        self.transport_nodes
            .iter()
            .any(|n| n.region == region && n.kind == crate::transport::TransportNodeKind::Port && n.operational())
    }

    /// Stage 10A: `port_node`'s airfield-domain counterpart - the first
    /// (lowest `TransportNodeId`) `Airfield` node belonging to `region`, if
    /// any. The sole source of truth for "does this region have an
    /// airfield at all" (`transport::TransportNodeKind::Airfield`'s own
    /// doc), read by `action::apply_recruit`'s `Domain::Air` branch exactly
    /// the way `port_node` already gates `Domain::Sea`.
    pub fn airfield_node(&self, region: RegionId) -> Option<&TransportNode> {
        self.transport_nodes
            .iter()
            .find(|n| n.region == region && n.kind == crate::transport::TransportNodeKind::Airfield)
    }

    /// Whether `region` has any operational `Airfield` node - the airfield
    /// twin of `port_node_operational`, and subject to the same
    /// `codex review` P2 finding: a region may declare several airfields,
    /// `logistics::build_transport_graph` gates each independently, so
    /// "can this region fly" must ask all of them rather than the first.
    /// The first **operational** `Airfield` node of `region`, if any.
    ///
    /// `codex review` (P2): asking `airfield_node_operational` whether a
    /// region can host a squadron and then taking `airfield_node`'s
    /// lowest-id node is two different questions. A region whose first
    /// airfield is wrecked and whose second is intact passes the first and
    /// fails the second, so the caller emitted an order `apply_move`
    /// rejected every tick. Callers that need *a node to use* must ask for
    /// one that works, not for the first one that exists.
    /// The first **operational** `Port` node of `region`, if any -
    /// `operational_airfield_node`'s port twin, and there for the same
    /// reason: a caller that needs *a node to use or to hit* must ask for
    /// one that still works, not for the first one that exists.
    pub fn operational_port_node(&self, region: RegionId) -> Option<&TransportNode> {
        self.transport_nodes
            .iter()
            .find(|n| n.region == region && n.kind == crate::transport::TransportNodeKind::Port && n.operational())
    }

    pub fn operational_airfield_node(&self, region: RegionId) -> Option<&TransportNode> {
        self.transport_nodes
            .iter()
            .find(|n| n.region == region && n.kind == crate::transport::TransportNodeKind::Airfield && n.operational())
    }

    pub fn airfield_node_operational(&self, region: RegionId) -> bool {
        self.transport_nodes
            .iter()
            .any(|n| n.region == region && n.kind == crate::transport::TransportNodeKind::Airfield && n.operational())
    }

    pub fn has_airfield_node(&self, region: RegionId) -> bool {
        self.airfield_node(region).is_some()
    }

    /// Alive air units currently based at airfield node `node` - the
    /// air-domain counterpart of `units_in`/`fleets_in` (see `units_in`'s
    /// own doc for why the three never overlap: `Station::Region`/
    /// `Station::Sea`/`Station::Airfield` are mutually exclusive by
    /// construction). Stage 10A's own acceptance criterion ("同じ飛行場の 2
    /// 個航空部隊が容量を分け合う") is exactly two units both answering
    /// `true` to `unit.station == Station::Airfield(node)` here, contending
    /// for the same `air::air_demand` sink in `logistics::
    /// compute_transport_flow` - never a second, per-unit capacity draw.
    pub fn air_units_at(&self, node: TransportNodeId) -> impl Iterator<Item = &Unit> {
        self.units
            .iter()
            .filter(move |unit| unit.alive && unit.station == Station::Airfield(node))
    }
}
