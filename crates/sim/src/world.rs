//! The map (regions, links, terrain) and the mutable game state
//! (factions, units, supply) that every tick system reads and writes.

use crate::balance::{INFRA_DAMAGE_SHARE, NODE_BASE, NODE_INFRA, NODE_PORT, WORKFORCE_SHARE};
use crate::construction::Construction;
use crate::diplomacy::Diplomacy;
use crate::focus::NationalFocus;
use crate::good::{Good, GOOD_COUNT};
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::military::Unit;

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
}

/// Where a `Unit` currently is: a land region or a sea zone. Replaces the
/// Phase 1/2A-2C `Unit::location: RegionId` (docs/phase2-spec.md Stage 2D:
/// "Unit の location: RegionId を station: Station に置き換える").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Station {
    Region(RegionId),
    Sea(SeaZoneId),
}

impl Station {
    pub fn domain(self) -> Domain {
        match self {
            Station::Region(_) => Domain::Land,
            Station::Sea(_) => Domain::Sea,
        }
    }

    pub fn region(self) -> Option<RegionId> {
        match self {
            Station::Region(r) => Some(r),
            Station::Sea(_) => None,
        }
    }

    pub fn sea_zone(self) -> Option<SeaZoneId> {
        match self {
            Station::Region(_) => None,
            Station::Sea(z) => Some(z),
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
    /// coordinate, `[x, y]`, if the scenario authored one
    /// (`scenario::RegionDef::position`'s doc). `None` when it didn't -
    /// `apps/game::layout` computes a deterministic fallback for the whole
    /// map in that case rather than this crate guessing a default.
    /// Cosmetic only: nothing in `crate::sim`/`crate::world`/any tick system
    /// ever reads this field, so it cannot affect - and Stage 7A's own
    /// regression guard confirms it does not affect - simulation outcomes.
    pub position: Option<[f32; 2]>,
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
    /// Supply throughput available at each region, indexed by `RegionId`.
    pub supply: Vec<f32>,
    /// Stage 2D (docs/phase2-spec.md "海域"): the map's sea zones, separate
    /// from the region graph.
    pub sea_zones: Vec<SeaZone>,
    pub day: u32,
    /// Stage 3B (docs/phase3-spec.md "Stage 3B — 外交関係と条約"): every
    /// pair's `Stance`/`opinion`/treaty grants and the pending-proposal
    /// queue.
    pub diplomacy: Diplomacy,
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
}
