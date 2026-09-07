//! Stage 6A (docs/phase6-spec.md "Stage 6A — データ駆動化とベンチマーク"):
//! reads the map (regions, links, sea zones, factions and their starting
//! territory) from a JSON scenario file instead of a hardcoded Rust table.
//! `scenarios/mvp.json` is the current 10-region/3-faction MVP map (design
//! doc's §19), embedded into the binary (`include_str!`) as this crate's
//! built-in default - `build_world()` is a thin wrapper that loads it, so
//! every existing caller keeps working unchanged.
//!
//! Everything *uniform across factions* (starting manpower, stockpile,
//! conscription, group influence weights, the default national focus, unit
//! stats, ...) stays a Rust constant here rather than JSON data - per
//! docs/phase6-spec.md's own "共通の制約": "バランス定数は `balance.rs`".
//! Only the things that actually vary *per scenario* - which regions exist,
//! how they connect, which sea zones exist, and which faction starts
//! owning what - are externalized. Loading never falls back to a default on
//! failure (docs/phase6-spec.md "壊れたデータで暗黙に既定値へ落ちない
//! こと"): every malformed or inconsistent file becomes a specific
//! `ScenarioError`, not a silently-substituted `build_world()`.

use std::fmt;

use crate::diplomacy::Diplomacy;
use crate::focus::NationalFocus;
use crate::good::{ALL_GOODS, GOOD_COUNT};
use crate::group::GROUP_COUNT;
use crate::ids::{FactionId, RegionId, SeaZoneId, TransportLineId, TransportNodeId, UnitId};
use crate::json::{self, Value};
use crate::military::Unit;
use crate::transport::{Capacity, Condition, TransportLine, TransportLineKind, TransportNode, TransportNodeKind};
use crate::world::{AirSuperiority, DominationShare, Faction, Link, LinkKind, Region, SeaZone, Station, Terrain, VictoryCondition, VictoryDeclaration, World};

/// The embedded default scenario (docs/phase6-spec.md "ファイルは
/// `scenarios/` に置く。既定は現行の 10 地域（`mvp.json`）"). Baked into the
/// binary at compile time so `build_world()` never touches the filesystem -
/// exactly like the old hardcoded tables it replaces.
const MVP_JSON: &str = include_str!("../../../scenarios/mvp.json");

/// Region/sea-zone/faction counts of the embedded default scenario, used
/// only where a compile-time constant is genuinely needed
/// (`observation::ENCODING_LEN`, and anywhere else that specifically wants
/// "the default scenario's shape" without loading it). A `--scenario`
/// loaded at runtime may have different counts - nothing in `crate::world`
/// or `crate::observation` actually depends on these being right for every
/// scenario (see `observation::encoding_len`), and `default_scenario_
/// dimensions_match_embedded_json` below guards against these three ever
/// silently drifting from what `scenarios/mvp.json` actually contains.
pub const REGION_COUNT: usize = 10;
pub const SEA_ZONE_COUNT: usize = 5;
pub const FACTION_COUNT: usize = 3;
/// Stage 9D (docs/phase9-spec.md "4. 観測ベクトル"): the embedded default
/// scenario's transport-network shape, alongside `REGION_COUNT`/
/// `SEA_ZONE_COUNT`/`FACTION_COUNT` above for exactly the same reason
/// (`observation::ENCODING_LEN` needs a compile-time constant) - guarded by
/// the same `default_scenario_dimensions_match_embedded_json` test.
pub const TRANSPORT_NODE_COUNT: usize = 30;
pub const TRANSPORT_LINE_COUNT: usize = 32;

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
/// Stage 3C (docs/phase3-spec.md "Stage 3C — 国家方針"): every faction starts
/// with this focus already active (`focus_transition_days: 0` below - the
/// very first tick isn't a "switch" with anything to transition away from).
/// Specifically `AllianceNetwork`, not an arbitrary pick: it is the one
/// focus with no unconditional day-zero footprint against a freshly built
/// world - it touches only `Diplomacy::opinion` recovery (a no-op until some
/// pair's opinion actually goes negative) and treaty-acceptance ease (a
/// no-op until a proposal actually arrives), unlike every other focus, each
/// of which changes something Phase 1/2/3A/3B tests already assert exact or
/// near-exact numbers for from tick one (group-support targets, import
/// capacity, construction rate, production output, or combat power).
/// `HeuristicAgent` replaces it with its own opening-situation choice via
/// `Action::SetNationalFocus` within its first few days of play
/// (docs/phase3-spec.md "AI" under "Stage 3C"), which *does* pay the normal
/// `FOCUS_SWITCH_DAYS` transition like any other switch.
const FACTION_NATIONAL_FOCUS_DEFAULT: NationalFocus = NationalFocus::AllianceNetwork;

const UNITS_PER_FACTION: usize = 3;

/// Every way loading a `--scenario` file can fail. Each variant carries
/// enough detail (an id, a field name, a reason) that the printed message
/// alone tells a human exactly what to fix - never just "invalid scenario".
#[derive(Clone, Debug, PartialEq)]
pub enum ScenarioError {
    /// The file itself could not be read (`scenario::load_file` only).
    Io(String),
    /// The bytes weren't valid JSON at all.
    Json(String),
    /// Valid JSON, but missing a required field or a field has the wrong
    /// shape/type for where it appears.
    Schema(String),
    /// Something referenced an id (region/sea zone/faction) that doesn't
    /// exist anywhere in the file.
    UnknownId { context: String, id: String },
    /// Two entries of the same kind declared the same `id`.
    DuplicateId { kind: &'static str, id: String },
    /// Region `from` lists a link to `to`, but `to` lists no link back to
    /// `from` at all.
    OneWayLink { from: String, to: String },
    /// Region `from` lists a link to `to` and `to` lists one back, but they
    /// disagree (different `kind` and/or `strait_zone`).
    LinkMismatch { from: String, to: String, reason: String },
    /// A `strait` link names no `strait_zone`, or a non-`strait` link names
    /// one anyway. `Link::strait_zone`'s mere *presence* (not its value)
    /// controls whether sea control throttles this link at all
    /// (`logistics::tick_logistics` and `military::tick_movement` both
    /// branch on `Option::is_some()`), so getting this wrong changes
    /// mechanics with no other visible symptom: an unnamed zone silently
    /// makes a strait immune to blockade, and a stray zone silently
    /// subjects an ordinary rail/road/tunnel link to blockade throttling -
    /// exactly the thing the 中国—九州 Kanmon `tunnel` deliberately relies
    /// on *not* happening to it. Checked per direction, so a reciprocal
    /// pair that agrees on the same wrong pairing (which `LinkMismatch`
    /// only catches when the two directions *disagree*) is still rejected.
    InvalidStraitZone { from: String, to: String, reason: String },
    /// No faction's `regions` list claims this region.
    UnclaimedRegion { region: String },
    /// Two different factions both claim this region.
    RegionClaimedTwice { region: String, first: String, second: String },
    /// A faction's `regions` list is empty.
    FactionWithoutTerritory { faction: String },
    /// A `diplomacy.blocs` entry names fewer than 2 factions - an alliance
    /// of one is meaningless: a faction absent from every bloc is already,
    /// implicitly, at war with everyone (`DiplomacyDef`'s doc).
    BlocTooSmall { bloc: String, size: usize },
    /// The same faction is named in two different `diplomacy.blocs`
    /// entries, or twice within the same one - every faction may start in
    /// at most one bloc.
    FactionInMultipleBlocs { faction: String, first_bloc: String, second_bloc: String },
    /// The region graph (built from every region's `links`) isn't
    /// connected - these regions can't reach the rest by any link at all,
    /// so supply could never reach them (docs/phase6-spec.md "グラフが連結
    /// か（孤立地域は補給が届かず、意図しない限り誤り）").
    Disconnected { unreachable: Vec<String> },
    /// Stage 9A (docs/phase9-spec.md "1. 層の分離"): a transport `Port`
    /// node's own region reports `Region::port <= 0.0` - the one
    /// consistency rule Stage 9A keeps between `Region::port` and the new
    /// node-based representation, so the two can never disagree about
    /// whether a region has a port at all (`transport::TransportNodeKind::Port`'s
    /// doc). The converse - a region with a port but no `Port` node yet -
    /// is not rejected here.
    PortNodeWithoutRegionPort { node: String, region: String },
    /// The converse of `PortNodeWithoutRegionPort`, added alongside it in
    /// Stage 9A (docs/phase9-spec.md §1 "港の扱い": "同じ事実が2か所に別々に
    /// 存在する状態は作らない"): a region reports `port > 0.0` but declares
    /// no `Port` node at all, so naval logic (`Region::port`) and the
    /// transport layer would disagree about whether the region has a port.
    /// This is validation only - it changes no behaviour and picks no
    /// source of truth between the two representations. Stage 9B is what
    /// makes the `Port` node itself the thing blockade/import actually key
    /// off (`transport::TransportNodeKind::Port`'s doc).
    RegionPortWithoutPortNode { region: String },
    /// Stage 9A (docs/phase9-spec.md §1 "輸送路線": "`kind` は... `Sea`
    /// （港と港を海域経由で結ぶ）"): a `Sea` line names an endpoint whose
    /// node `kind` isn't `Port`. `Rail`/`Road` are unrestricted - a `Rail`
    /// line legitimately ends at a `Port` node too (that's how the port
    /// reaches inland; `tools/transport_network.py`'s module doc), so this
    /// check is `Sea`-only.
    SeaLineNotBetweenPorts { from: String, to: String, offending: String },
    /// `regions` or `factions` is an empty array. Every validation loop
    /// below iterates over one or the other, so an empty collection makes
    /// every one of them a no-op and the file "passes" - and then a caller
    /// that assumes at least one faction exists (`--bench`'s `Observation`
    /// for `FactionId(0)`, for one) panics instead of getting a clean
    /// error. Checked before anything else, so it's always the *first*
    /// problem reported for a scenario missing either.
    Empty { what: &'static str },
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScenarioError::Io(msg) => write!(f, "could not read scenario file: {msg}"),
            ScenarioError::Json(msg) => write!(f, "scenario file is not valid JSON: {msg}"),
            ScenarioError::Schema(msg) => write!(f, "scenario schema error: {msg}"),
            ScenarioError::UnknownId { context, id } => {
                write!(f, "{context} references unknown id `{id}`")
            }
            ScenarioError::DuplicateId { kind, id } => write!(f, "duplicate {kind} id `{id}`"),
            ScenarioError::OneWayLink { from, to } => {
                write!(f, "link `{from}` -> `{to}` has no reciprocal link `{to}` -> `{from}`")
            }
            ScenarioError::LinkMismatch { from, to, reason } => {
                write!(f, "link `{from}` <-> `{to}` is inconsistent: {reason}")
            }
            ScenarioError::InvalidStraitZone { from, to, reason } => {
                write!(f, "link `{from}` -> `{to}`: {reason}")
            }
            ScenarioError::UnclaimedRegion { region } => {
                write!(f, "region `{region}` is not claimed by any faction")
            }
            ScenarioError::RegionClaimedTwice { region, first, second } => {
                write!(f, "region `{region}` is claimed by both faction `{first}` and faction `{second}`")
            }
            ScenarioError::FactionWithoutTerritory { faction } => {
                write!(f, "faction `{faction}` owns no regions")
            }
            ScenarioError::BlocTooSmall { bloc, size } => {
                write!(f, "diplomacy bloc `{bloc}` names {size} faction(s); an alliance needs at least 2")
            }
            ScenarioError::FactionInMultipleBlocs { faction, first_bloc, second_bloc } => {
                write!(f, "faction `{faction}` is named in both diplomacy bloc `{first_bloc}` and `{second_bloc}`")
            }
            ScenarioError::Disconnected { unreachable } => {
                write!(f, "region graph is not connected: unreachable from the rest of the map: {}", unreachable.join(", "))
            }
            ScenarioError::PortNodeWithoutRegionPort { node, region } => {
                write!(f, "transport node `{node}` is a port, but region `{region}` reports no port (`port <= 0.0`)")
            }
            ScenarioError::RegionPortWithoutPortNode { region } => {
                write!(f, "region `{region}` reports a port (`port > 0.0`) but declares no `port` transport node")
            }
            ScenarioError::SeaLineNotBetweenPorts { from, to, offending } => {
                write!(f, "sea line `{from}` -> `{to}`: node `{offending}` is not a `port` node")
            }
            ScenarioError::Empty { what } => write!(f, "scenario has no {what}"),
        }
    }
}

/// One `Region`'s outgoing link, as authored in the scenario file. Kept
/// per-region (mirroring `Region::links` directly) rather than as a single
/// global edge list specifically so a one-way authoring mistake - a link
/// listed under one region's `links` with no reciprocal entry under the
/// other's - is expressible at all, and so `validate` can catch it
/// (`ScenarioError::OneWayLink`).
#[derive(Clone, Debug, PartialEq)]
pub struct LinkDef {
    pub to: String,
    pub kind: LinkKind,
    /// The sea zone this link's strait physically passes through, if any -
    /// `Link::strait_zone`'s doc. `None` for every non-`Strait` link and
    /// for a `Tunnel` deliberately left unaffected by sea control.
    pub strait_zone: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegionDef {
    pub id: String,
    pub name: String,
    pub terrain: Terrain,
    pub population: f32,
    /// Capacity per commodity, indexed by `Good::index()`.
    pub capacity: [f32; GOOD_COUNT],
    pub infrastructure: f32,
    pub port: f32,
    pub links: Vec<LinkDef>,
    /// Stage 7A (docs/phase7-spec.md "地域の座標"): where `apps/game` draws
    /// this region on the map, `[x, y]` in an arbitrary client-side unit -
    /// nothing in `crate::world`/`crate::sim` ever reads it. **Required**:
    /// every scenario file must name a `position` for every region
    /// (`parse_position`), rejected at load time with `ScenarioError::Schema`
    /// if it doesn't - `apps/game::layout` no longer computes a fallback
    /// layout for a region left unplaced (docs/conventions.md §3).
    pub position: [f32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeaZoneDef {
    pub id: String,
    pub name: String,
    pub coast: Vec<String>,
    pub adjacent: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FactionDef {
    pub id: String,
    pub name: String,
    pub capital: String,
    pub regions: Vec<String>,
}

/// One `TransportNode`, as authored in the scenario file (docs/phase9-spec.md
/// "輸送ノード"). `region` is a string id, resolved by `build_world` like
/// every other cross-reference in this file.
#[derive(Clone, Debug, PartialEq)]
pub struct TransportNodeDef {
    pub id: String,
    pub name: String,
    pub kind: TransportNodeKind,
    pub region: String,
}

/// One `TransportLine`, as authored in the scenario file (docs/phase9-spec.md
/// "輸送路線"). Unlike `LinkDef` (kept per-region, mirroring `Region::links`),
/// this is a flat, undirected edge between two node ids in the scenario's
/// top-level `transport.lines` array - a transport line has no "owning"
/// node the way a region link is authored under one region's own list, so
/// there is no reciprocal-entry rule to check here (`Scenario::validate`'s
/// doc for what *is* checked).
#[derive(Clone, Debug, PartialEq)]
pub struct TransportLineDef {
    pub from: String,
    pub to: String,
    pub kind: TransportLineKind,
    pub capacity: Capacity,
    pub condition: Condition,
}

/// One starting alliance bloc (`DiplomacyDef`'s doc). Every faction listed
/// here starts allied (`Stance::Alliance`) with every other faction in the
/// same bloc - `id`/`name` exist purely for readability (error messages,
/// `to_json` round-tripping) and are never looked up by `build_world`.
#[derive(Clone, Debug, PartialEq)]
pub struct BlocDef {
    pub id: String,
    pub name: String,
    pub factions: Vec<String>,
}

/// A scenario's required starting-diplomacy declaration
/// (docs/conventions.md §3 "壊れたデータで暗黙に既定値へ落ちないこと",
/// applied here to a state Stage 3B previously hard-coded rather than to a
/// malformed field: "初期状態は全勢力が相互に War" - docs/phase3-spec.md's
/// Stage 3B baseline - was always true for `Diplomacy::new`, but nothing in
/// a scenario file ever *said* so, which made it an implied default no
/// scenario could opt out of. Every scenario must now spell out its own
/// starting diplomatic state; `scenarios/mvp.json`'s `"blocs": []` is that
/// same all-at-war baseline written down explicitly, which is exactly why
/// its `--json` hash is unchanged by this field existing.
///
/// `blocs` partitions *some* factions into alliance groups; any faction
/// named in no bloc is (implicitly) a bloc of one - it starts at war with
/// everyone, exactly as `Diplomacy::new` always made every faction start.
/// Grouping by bloc (a named roster of members) rather than a flat list of
/// allied pairs is deliberate: a reader has to hold `blocs.len()` rosters in
/// their head to see the whole starting alignment, not
/// `sum(bloc.len() choose 2)` individual pairs - see
/// `scenarios/japan47.json`'s two 3-faction blocs, 2 rosters but 6 implied
/// alliance pairs.
#[derive(Clone, Debug, PartialEq)]
pub struct DiplomacyDef {
    pub blocs: Vec<BlocDef>,
}

/// The parsed (but not necessarily valid) contents of a scenario file - the
/// data `docs/phase6-spec.md`'s "シナリオの外部化" moves out of Rust source.
/// `RegionDef` order fixes `RegionId` assignment (first region in the file
/// is `RegionId(0)`, and so on) - the same for `SeaZoneDef`/`FactionDef` -
/// so a scenario file's array order is significant, not just cosmetic.
#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    pub regions: Vec<RegionDef>,
    pub sea_zones: Vec<SeaZoneDef>,
    /// Stage 9A (docs/phase9-spec.md "3. データ", "フォールバック禁止":
    /// every scenario must declare its own transport network - there is no
    /// default derived from `regions[].links`). `TransportNodeDef` order
    /// fixes `TransportNodeId` assignment, the same convention `regions`/
    /// `sea_zones`/`factions` already use.
    pub transport_nodes: Vec<TransportNodeDef>,
    pub transport_lines: Vec<TransportLineDef>,
    pub factions: Vec<FactionDef>,
    pub diplomacy: DiplomacyDef,
    /// This scenario's required, declared victory conditions
    /// (design.md §5: "勝利条件は一つに限定しない"), checked by
    /// `Simulation::outcome` in this same order - see `parse_victory`'s doc
    /// for the file format and why this has no default. Always non-empty -
    /// `VictoryDeclaration`'s doc.
    pub victory: VictoryDeclaration,
}

fn schema_err(msg: impl Into<String>) -> ScenarioError {
    ScenarioError::Schema(msg.into())
}

fn require_array<'a>(v: &'a Value, path: &str) -> Result<&'a Vec<Value>, ScenarioError> {
    v.as_array().ok_or_else(|| schema_err(format!("`{path}` must be an array")))
}

fn require_object_field<'a>(v: &'a Value, path: &str, field: &str) -> Result<&'a Value, ScenarioError> {
    v.get(field).ok_or_else(|| schema_err(format!("`{path}` is missing required field `{field}`")))
}

fn require_str(v: &Value, path: &str, field: &str) -> Result<String, ScenarioError> {
    require_object_field(v, path, field)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| schema_err(format!("`{path}.{field}` must be a string")))
}

fn require_f32(v: &Value, path: &str, field: &str) -> Result<f32, ScenarioError> {
    require_object_field(v, path, field)?
        .as_f32()
        .ok_or_else(|| schema_err(format!("`{path}.{field}` must be a finite number")))
}

fn optional_str_array(v: &Value, path: &str, field: &str) -> Result<Vec<String>, ScenarioError> {
    match v.get(field) {
        None => Ok(Vec::new()),
        Some(value) => string_array(value, &format!("{path}.{field}")),
    }
}

fn string_array(v: &Value, path: &str) -> Result<Vec<String>, ScenarioError> {
    require_array(v, path)?
        .iter()
        .enumerate()
        .map(|(i, item)| item.as_str().map(str::to_string).ok_or_else(|| schema_err(format!("`{path}[{i}]` must be a string"))))
        .collect()
}

fn parse_capacity(v: &Value, path: &str) -> Result<[f32; GOOD_COUNT], ScenarioError> {
    let obj = require_object_field(v, path, "capacity")?;
    let mut out = [0.0f32; GOOD_COUNT];
    for good in ALL_GOODS {
        let key = good.key();
        let value = obj
            .get(key)
            .ok_or_else(|| schema_err(format!("`{path}.capacity` is missing good `{key}`")))?
            .as_f32()
            .ok_or_else(|| schema_err(format!("`{path}.capacity.{key}` must be a finite number")))?;
        out[good.index()] = value;
    }
    Ok(out)
}

/// Parses the required `position` field (docs/phase7-spec.md "地域の座標":
/// `{ "position": [x, y] }`). Every region must carry one: `apps/game`
/// no longer computes a fallback layout for a region a scenario left
/// unplaced (docs/conventions.md §3, フォールバック原則禁止 - the project
/// owner rejected the graph-derived layout that used to stand in), so a
/// missing `position` is a hard `ScenarioError::Schema` here, exactly like
/// every other required region field `parse_region` reads - never a silent
/// `None` a renderer downstream would have to guess a placement for.
///
/// **Since Stage 10B this is no longer a rendering-only field.**
/// `air::tick_air_superiority` measures an airfield's reach against it in
/// kilometres (`balance::AIR_OPERATING_RADIUS_KM`), so a scenario that
/// places `Domain::Air` units needs coordinates on a real physical scale.
/// `scenarios/japan_hex.json` has them (`tools/hexmap/build_scenario.py`
/// divides metres by 1000); `mvp.json` and `japan47.json` carry an unscaled
/// schematic layout, which is harmless only for as long as neither deploys
/// an air unit. `Region::position`'s own doc records the reversal of Phase
/// 7A's 「座標はシミュレーションに一切影響しない」 invariant in full. The
/// schema deliberately does not try to guess or validate a scale here -
/// there is nothing in the data to check it against, and inventing one
/// would be the kind of silent default docs/conventions.md §3 forbids.
fn parse_position(v: &Value, path: &str) -> Result<[f32; 2], ScenarioError> {
    let value = require_object_field(v, path, "position")?;
    let arr = value.as_array().ok_or_else(|| schema_err(format!("`{path}.position` must be an array of 2 numbers")))?;
    if arr.len() != 2 {
        return Err(schema_err(format!("`{path}.position` must have exactly 2 elements, got {}", arr.len())));
    }
    let x = arr[0].as_f32().ok_or_else(|| schema_err(format!("`{path}.position[0]` must be a finite number")))?;
    let y = arr[1].as_f32().ok_or_else(|| schema_err(format!("`{path}.position[1]` must be a finite number")))?;
    Ok([x, y])
}

fn parse_link(v: &Value, path: &str) -> Result<LinkDef, ScenarioError> {
    let to = require_str(v, path, "to")?;
    let kind_key = require_str(v, path, "kind")?;
    let kind = LinkKind::from_key(&kind_key).ok_or_else(|| schema_err(format!("`{path}.kind` names unknown link kind `{kind_key}`")))?;
    let strait_zone = match v.get("strait_zone") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_str().map(str::to_string).ok_or_else(|| schema_err(format!("`{path}.strait_zone` must be a string")))?),
    };
    Ok(LinkDef { to, kind, strait_zone })
}

fn parse_region(v: &Value, path: &str) -> Result<RegionDef, ScenarioError> {
    let id = require_str(v, path, "id")?;
    let name = require_str(v, path, "name")?;
    let terrain_key = require_str(v, path, "terrain")?;
    let terrain = Terrain::from_key(&terrain_key).ok_or_else(|| schema_err(format!("`{path}.terrain` names unknown terrain `{terrain_key}`")))?;
    let population = require_f32(v, path, "population")?;
    let capacity = parse_capacity(v, path)?;
    let infrastructure = require_f32(v, path, "infrastructure")?;
    let port = require_f32(v, path, "port")?;
    let links_value = require_object_field(v, path, "links")?;
    let links_path = format!("{path}.links");
    let links = require_array(links_value, &links_path)?
        .iter()
        .enumerate()
        .map(|(i, l)| parse_link(l, &format!("{links_path}[{i}]")))
        .collect::<Result<Vec<_>, _>>()?;
    let position = parse_position(v, path)?;
    Ok(RegionDef { id, name, terrain, population, capacity, infrastructure, port, links, position })
}

fn parse_sea_zone(v: &Value, path: &str) -> Result<SeaZoneDef, ScenarioError> {
    let id = require_str(v, path, "id")?;
    let name = require_str(v, path, "name")?;
    let coast = string_array(require_object_field(v, path, "coast")?, &format!("{path}.coast"))?;
    let adjacent = optional_str_array(v, path, "adjacent")?;
    Ok(SeaZoneDef { id, name, coast, adjacent })
}

fn parse_faction(v: &Value, path: &str) -> Result<FactionDef, ScenarioError> {
    let id = require_str(v, path, "id")?;
    let name = require_str(v, path, "name")?;
    let capital = require_str(v, path, "capital")?;
    let regions = string_array(require_object_field(v, path, "regions")?, &format!("{path}.regions"))?;
    Ok(FactionDef { id, name, capital, regions })
}

fn parse_transport_node(v: &Value, path: &str) -> Result<TransportNodeDef, ScenarioError> {
    let id = require_str(v, path, "id")?;
    let name = require_str(v, path, "name")?;
    let kind_key = require_str(v, path, "kind")?;
    let kind = TransportNodeKind::from_key(&kind_key)
        .ok_or_else(|| schema_err(format!("`{path}.kind` names unknown transport node kind `{kind_key}`")))?;
    let region = require_str(v, path, "region")?;
    Ok(TransportNodeDef { id, name, kind, region })
}

fn parse_transport_line(v: &Value, path: &str) -> Result<TransportLineDef, ScenarioError> {
    let from = require_str(v, path, "from")?;
    let to = require_str(v, path, "to")?;
    let kind_key = require_str(v, path, "kind")?;
    let kind = TransportLineKind::from_key(&kind_key)
        .ok_or_else(|| schema_err(format!("`{path}.kind` names unknown transport line kind `{kind_key}`")))?;
    let capacity_raw = require_f32(v, path, "capacity")?;
    let capacity = Capacity::new(capacity_raw)
        .ok_or_else(|| schema_err(format!("`{path}.capacity` must be non-negative and finite, got {capacity_raw}")))?;
    let condition_raw = require_f32(v, path, "condition")?;
    let condition = Condition::new(condition_raw)
        .ok_or_else(|| schema_err(format!("`{path}.condition` must be between 0.0 and 1.0, got {condition_raw}")))?;
    Ok(TransportLineDef { from, to, kind, capacity, condition })
}

/// Parses the scenario's required `transport` field - docs/phase9-spec.md
/// "3. データ": "すべてのシナリオが輸送網を宣言する... フォールバック禁止。
/// 輸送網のないシナリオを地域リンクで補うことはしない". A scenario missing
/// this field entirely (or missing `nodes`/`lines` under it) is a hard
/// `ScenarioError::Schema`, exactly like every other required top-level
/// field (`diplomacy`, `victory`) - never an implied empty network.
fn parse_transport(v: &Value, path: &str) -> Result<(Vec<TransportNodeDef>, Vec<TransportLineDef>), ScenarioError> {
    let nodes_value = require_object_field(v, path, "nodes")?;
    let nodes_path = format!("{path}.nodes");
    let nodes = require_array(nodes_value, &nodes_path)?
        .iter()
        .enumerate()
        .map(|(i, n)| parse_transport_node(n, &format!("{nodes_path}[{i}]")))
        .collect::<Result<Vec<_>, _>>()?;

    let lines_value = require_object_field(v, path, "lines")?;
    let lines_path = format!("{path}.lines");
    let lines = require_array(lines_value, &lines_path)?
        .iter()
        .enumerate()
        .map(|(i, l)| parse_transport_line(l, &format!("{lines_path}[{i}]")))
        .collect::<Result<Vec<_>, _>>()?;

    Ok((nodes, lines))
}

fn parse_bloc(v: &Value, path: &str) -> Result<BlocDef, ScenarioError> {
    let id = require_str(v, path, "id")?;
    let name = require_str(v, path, "name")?;
    let factions = string_array(require_object_field(v, path, "factions")?, &format!("{path}.factions"))?;
    Ok(BlocDef { id, name, factions })
}

fn parse_diplomacy(v: &Value, path: &str) -> Result<DiplomacyDef, ScenarioError> {
    let blocs_value = require_object_field(v, path, "blocs")?;
    let blocs_path = format!("{path}.blocs");
    let blocs = require_array(blocs_value, &blocs_path)?
        .iter()
        .enumerate()
        .map(|(i, b)| parse_bloc(b, &format!("{blocs_path}[{i}]")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DiplomacyDef { blocs })
}

/// Parses one entry of the required `victory` array (`Scenario::victory`'s
/// doc) into the `sim::world::VictoryCondition` `Simulation::outcome` reads
/// directly - there is no separate scenario-level "definition" type to keep
/// in sync with it, since (unlike `diplomacy.blocs`) nothing here names a
/// region/faction id that needs resolving first.
fn parse_victory_condition(v: &Value, path: &str) -> Result<VictoryCondition, ScenarioError> {
    let kind = require_str(v, path, "type")?;
    match kind.as_str() {
        "conquest" => Ok(VictoryCondition::Conquest),
        "coalition" => Ok(VictoryCondition::Coalition),
        "domination" => {
            let share = require_f32(v, path, "share")?;
            let share = DominationShare::new(share).ok_or_else(|| {
                schema_err(format!("`{path}.share` must be greater than 0.0 and at most 1.0, got {share}"))
            })?;
            Ok(VictoryCondition::Domination(share))
        }
        other => Err(schema_err(format!("`{path}.type` names unknown victory condition `{other}`"))),
    }
}

/// Parses the scenario's required `victory` field - the array of
/// `VictoryCondition`s `Simulation::outcome` checks, in this same order,
/// every tick (docs/conventions.md §3: absent or malformed is a load
/// error, never an implied default - see `missing_victory_declaration_is_
/// rejected`). `scenarios/mvp.json`'s `"victory": [{"type":"conquest"}]`
/// spells out exactly the one rule this project ever implemented before
/// scenario-declared victory conditions existed, which is why its
/// behaviour (and `--json` hash) is unchanged by this field existing.
///
/// Rejected here, not just at `validate` time, if the array is empty: an
/// empty declaration would leave `Simulation::outcome` with no condition
/// ever able to fire, so a scenario could walk straight into the same
/// permanently-unreachable-victory state a required `victory` field exists
/// to prevent (see `VictoryDeclaration`'s doc and
/// `empty_victory_declaration_is_rejected`).
fn parse_victory(v: &Value, path: &str) -> Result<VictoryDeclaration, ScenarioError> {
    let conditions = require_array(v, path)?
        .iter()
        .enumerate()
        .map(|(i, c)| parse_victory_condition(c, &format!("{path}[{i}]")))
        .collect::<Result<Vec<_>, _>>()?;
    VictoryDeclaration::new(conditions)
        .ok_or_else(|| schema_err(format!("`{path}` must declare at least one victory condition, got an empty array")))
}

impl Scenario {
    /// Parses (but does not validate) a scenario document. Call `validate`
    /// before `build_world` - `load_str`/`load_file` do both for you.
    pub fn parse(json_text: &str) -> Result<Scenario, ScenarioError> {
        let root = json::parse(json_text, 64).map_err(|e| ScenarioError::Json(e.to_string()))?;
        let regions_value = require_object_field(&root, "$", "regions")?;
        let regions = require_array(regions_value, "regions")?
            .iter()
            .enumerate()
            .map(|(i, r)| parse_region(r, &format!("regions[{i}]")))
            .collect::<Result<Vec<_>, _>>()?;

        let sea_zones_value = require_object_field(&root, "$", "sea_zones")?;
        let sea_zones = require_array(sea_zones_value, "sea_zones")?
            .iter()
            .enumerate()
            .map(|(i, z)| parse_sea_zone(z, &format!("sea_zones[{i}]")))
            .collect::<Result<Vec<_>, _>>()?;

        let transport_value = require_object_field(&root, "$", "transport")?;
        let (transport_nodes, transport_lines) = parse_transport(transport_value, "transport")?;

        let factions_value = require_object_field(&root, "$", "factions")?;
        let factions = require_array(factions_value, "factions")?
            .iter()
            .enumerate()
            .map(|(i, f)| parse_faction(f, &format!("factions[{i}]")))
            .collect::<Result<Vec<_>, _>>()?;

        let diplomacy_value = require_object_field(&root, "$", "diplomacy")?;
        let diplomacy = parse_diplomacy(diplomacy_value, "diplomacy")?;

        let victory_value = require_object_field(&root, "$", "victory")?;
        let victory = parse_victory(victory_value, "victory")?;

        Ok(Scenario { regions, sea_zones, transport_nodes, transport_lines, factions, diplomacy, victory })
    }

    /// Every check from docs/phase6-spec.md's "検証": non-empty, ids exist,
    /// links are bidirectional *and* internally consistent (a `strait` link
    /// names a real `strait_zone`, a non-`strait` link names none), every
    /// faction owns territory, the region graph is connected, and sea zone
    /// coasts name real regions. Returns the first problem found (in a
    /// fixed, deterministic order - file order, not hash order), never a
    /// partial list; run the file through again after fixing each one.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        // Empty collections first: every loop below is a no-op on an empty
        // `regions`/`factions`, so nothing else here would ever catch this
        // (`ScenarioError::Empty`'s doc).
        if self.regions.is_empty() {
            return Err(ScenarioError::Empty { what: "regions" });
        }
        if self.factions.is_empty() {
            return Err(ScenarioError::Empty { what: "factions" });
        }

        let mut region_ids = std::collections::BTreeSet::new();
        for r in &self.regions {
            if !region_ids.insert(r.id.as_str()) {
                return Err(ScenarioError::DuplicateId { kind: "region", id: r.id.clone() });
            }
        }
        let mut zone_ids = std::collections::BTreeSet::new();
        for z in &self.sea_zones {
            if !zone_ids.insert(z.id.as_str()) {
                return Err(ScenarioError::DuplicateId { kind: "sea zone", id: z.id.clone() });
            }
        }
        let mut faction_ids = std::collections::BTreeSet::new();
        for fac in &self.factions {
            if !faction_ids.insert(fac.id.as_str()) {
                return Err(ScenarioError::DuplicateId { kind: "faction", id: fac.id.clone() });
            }
        }
        let mut transport_node_ids = std::collections::BTreeSet::new();
        for n in &self.transport_nodes {
            if !transport_node_ids.insert(n.id.as_str()) {
                return Err(ScenarioError::DuplicateId { kind: "transport node", id: n.id.clone() });
            }
        }

        // Dangling ids: every link target/strait zone, every sea zone's
        // coast/adjacent, and every faction's capital/regions must resolve.
        // Also: a `strait` link must name a `strait_zone`, and a non-
        // `strait` link must not - `ScenarioError::InvalidStraitZone`'s doc
        // for why this matters. Checked per direction (this loop visits
        // every region's own `links`, i.e. both halves of every pair), so
        // it catches a reciprocal pair that agrees on the same wrong
        // pairing, not just a disagreement between the two directions.
        for r in &self.regions {
            for link in &r.links {
                if !region_ids.contains(link.to.as_str()) {
                    return Err(ScenarioError::UnknownId { context: format!("region `{}` link", r.id), id: link.to.clone() });
                }
                match (link.kind, &link.strait_zone) {
                    (LinkKind::Strait, None) => {
                        return Err(ScenarioError::InvalidStraitZone {
                            from: r.id.clone(),
                            to: link.to.clone(),
                            reason: "a `strait` link must name a `strait_zone`".to_string(),
                        });
                    }
                    (kind, Some(_)) if kind != LinkKind::Strait => {
                        return Err(ScenarioError::InvalidStraitZone {
                            from: r.id.clone(),
                            to: link.to.clone(),
                            reason: format!("a `{}` link must not name a `strait_zone`", kind.key()),
                        });
                    }
                    _ => {}
                }
                if let Some(zone) = &link.strait_zone
                    && !zone_ids.contains(zone.as_str())
                {
                    return Err(ScenarioError::UnknownId { context: format!("region `{}` link to `{}`", r.id, link.to), id: zone.clone() });
                }
            }
        }
        for z in &self.sea_zones {
            for region in &z.coast {
                if !region_ids.contains(region.as_str()) {
                    return Err(ScenarioError::UnknownId { context: format!("sea zone `{}` coast", z.id), id: region.clone() });
                }
            }
            for other in &z.adjacent {
                if !zone_ids.contains(other.as_str()) {
                    return Err(ScenarioError::UnknownId { context: format!("sea zone `{}` adjacent", z.id), id: other.clone() });
                }
            }
        }
        for fac in &self.factions {
            if !region_ids.contains(fac.capital.as_str()) {
                return Err(ScenarioError::UnknownId { context: format!("faction `{}` capital", fac.id), id: fac.capital.clone() });
            }
            for region in &fac.regions {
                if !region_ids.contains(region.as_str()) {
                    return Err(ScenarioError::UnknownId { context: format!("faction `{}` regions", fac.id), id: region.clone() });
                }
            }
        }

        // Stage 9A transport network (docs/phase9-spec.md "1. 層の分離",
        // "3. データ"): every node's `region` must resolve, and a `Port`
        // node's region must actually report a port - the one place Stage
        // 9A keeps `Region::port` and the new node-based representation
        // from disagreeing (`ScenarioError::PortNodeWithoutRegionPort`'s
        // doc). Every line's `from`/`to` must resolve to a real node.
        let mut regions_with_port_node = std::collections::BTreeSet::new();
        for n in &self.transport_nodes {
            if !region_ids.contains(n.region.as_str()) {
                return Err(ScenarioError::UnknownId { context: format!("transport node `{}`", n.id), id: n.region.clone() });
            }
            if n.kind == TransportNodeKind::Port {
                let region = self.regions.iter().find(|r| r.id == n.region).expect("dangling region id already rejected above");
                if region.port <= 0.0 {
                    return Err(ScenarioError::PortNodeWithoutRegionPort { node: n.id.clone(), region: n.region.clone() });
                }
                regions_with_port_node.insert(n.region.as_str());
            }
        }
        // The converse check (`ScenarioError::RegionPortWithoutPortNode`'s
        // doc): a region that reports `port > 0.0` must declare at least
        // one `Port` node, so naval logic (still keyed off `Region::port`
        // in Stage 9A) and the transport layer never disagree about
        // whether a region has a port at all.
        for r in &self.regions {
            if r.port > 0.0 && !regions_with_port_node.contains(r.id.as_str()) {
                return Err(ScenarioError::RegionPortWithoutPortNode { region: r.id.clone() });
            }
        }
        for l in &self.transport_lines {
            if !transport_node_ids.contains(l.from.as_str()) {
                return Err(ScenarioError::UnknownId {
                    context: format!("transport line `{}` -> `{}`", l.from, l.to),
                    id: l.from.clone(),
                });
            }
            if !transport_node_ids.contains(l.to.as_str()) {
                return Err(ScenarioError::UnknownId {
                    context: format!("transport line `{}` -> `{}`", l.from, l.to),
                    id: l.to.clone(),
                });
            }
            // docs/phase9-spec.md §1 defines `Sea` as connecting a port to a
            // port via a sea zone (`ScenarioError::SeaLineNotBetweenPorts`'s
            // doc) - both endpoints must already be `Port` nodes, checked
            // now that both are known to resolve. `Rail`/`Road` are left
            // alone: a `Rail` line ending at a `Port` node is how that port
            // reaches inland (see the mvp/japan47/japan_hex data itself).
            if l.kind == TransportLineKind::Sea {
                for end in [&l.from, &l.to] {
                    let node =
                        self.transport_nodes.iter().find(|n| &n.id == end).expect("dangling node id already rejected above");
                    if node.kind != TransportNodeKind::Port {
                        return Err(ScenarioError::SeaLineNotBetweenPorts {
                            from: l.from.clone(),
                            to: l.to.clone(),
                            offending: node.id.clone(),
                        });
                    }
                }
            }
        }

        // Bidirectional links: every outgoing link must have a matching
        // reciprocal on the other side.
        for r in &self.regions {
            for link in &r.links {
                let other = self.regions.iter().find(|x| x.id == link.to).expect("dangling ids already rejected above");
                let Some(back) = other.links.iter().find(|l| l.to == r.id) else {
                    return Err(ScenarioError::OneWayLink { from: r.id.clone(), to: link.to.clone() });
                };
                if back.kind != link.kind || back.strait_zone != link.strait_zone {
                    return Err(ScenarioError::LinkMismatch {
                        from: r.id.clone(),
                        to: link.to.clone(),
                        reason: format!(
                            "`{}` -> `{}` is {:?}/{:?}, but `{}` -> `{}` is {:?}/{:?}",
                            r.id, link.to, link.kind, link.strait_zone, link.to, r.id, back.kind, back.strait_zone
                        ),
                    });
                }
            }
        }

        // Ownership: every region claimed exactly once, every faction owns
        // at least one region (docs/phase6-spec.md "すべての勢力が少なくとも
        // 1 地域を持つか"). Deliberately stricter than the old hardcoded
        // `build_world` (an unclaimed region there silently defaulted to
        // faction 0) - per "壊れたデータで暗黙に既定値へ落ちないこと" that
        // silent default is exactly what scenario data must never do.
        let mut owner: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
        for fac in &self.factions {
            if fac.regions.is_empty() {
                return Err(ScenarioError::FactionWithoutTerritory { faction: fac.id.clone() });
            }
            for region in &fac.regions {
                if let Some(&first) = owner.get(region.as_str()) {
                    return Err(ScenarioError::RegionClaimedTwice {
                        region: region.clone(),
                        first: first.to_string(),
                        second: fac.id.clone(),
                    });
                }
                owner.insert(region.as_str(), &fac.id);
            }
        }
        for r in &self.regions {
            if !owner.contains_key(r.id.as_str()) {
                return Err(ScenarioError::UnclaimedRegion { region: r.id.clone() });
            }
        }

        // Diplomacy blocs: bloc ids unique, every named faction exists, every
        // bloc names at least 2 members (`BlocTooSmall`), and every faction
        // belongs to at most one bloc (`FactionInMultipleBlocs`) - checked
        // before connectivity since starting diplomacy doesn't depend on the
        // region graph at all.
        let mut bloc_ids = std::collections::BTreeSet::new();
        for bloc in &self.diplomacy.blocs {
            if !bloc_ids.insert(bloc.id.as_str()) {
                return Err(ScenarioError::DuplicateId { kind: "diplomacy bloc", id: bloc.id.clone() });
            }
        }
        let mut bloc_member_of: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
        for bloc in &self.diplomacy.blocs {
            if bloc.factions.len() < 2 {
                return Err(ScenarioError::BlocTooSmall { bloc: bloc.id.clone(), size: bloc.factions.len() });
            }
            for faction in &bloc.factions {
                if !faction_ids.contains(faction.as_str()) {
                    return Err(ScenarioError::UnknownId {
                        context: format!("diplomacy bloc `{}`", bloc.id),
                        id: faction.clone(),
                    });
                }
                if let Some(&first) = bloc_member_of.get(faction.as_str()) {
                    return Err(ScenarioError::FactionInMultipleBlocs {
                        faction: faction.clone(),
                        first_bloc: first.to_string(),
                        second_bloc: bloc.id.clone(),
                    });
                }
                bloc_member_of.insert(faction.as_str(), bloc.id.as_str());
            }
        }

        // Connectivity: an isolated region can never receive supply
        // (docs/phase6-spec.md "グラフが連結か").
        if !self.regions.is_empty() {
            let index_of: std::collections::BTreeMap<&str, usize> =
                self.regions.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
            let mut visited = vec![false; self.regions.len()];
            let mut stack = vec![0usize];
            visited[0] = true;
            while let Some(i) = stack.pop() {
                for link in &self.regions[i].links {
                    let j = index_of[link.to.as_str()];
                    if !visited[j] {
                        visited[j] = true;
                        stack.push(j);
                    }
                }
            }
            let unreachable: Vec<String> =
                self.regions.iter().zip(visited.iter()).filter(|&(_, &v)| !v).map(|(r, _)| r.id.clone()).collect();
            if !unreachable.is_empty() {
                return Err(ScenarioError::Disconnected { unreachable });
            }
        }

        Ok(())
    }

    /// Builds a `World` from this scenario. Only ever called after
    /// `validate` returns `Ok` (`load_str`/`load_file` enforce that); the id
    /// lookups below assume every reference already resolves.
    pub fn build_world(&self) -> World {
        let index_of: std::collections::HashMap<&str, u32> =
            self.regions.iter().enumerate().map(|(i, r)| (r.id.as_str(), i as u32)).collect();
        let zone_index_of: std::collections::HashMap<&str, u32> =
            self.sea_zones.iter().enumerate().map(|(i, z)| (z.id.as_str(), i as u32)).collect();
        let faction_index_of: std::collections::HashMap<&str, u32> =
            self.factions.iter().enumerate().map(|(i, f)| (f.id.as_str(), i as u32)).collect();
        let transport_node_index_of: std::collections::HashMap<&str, u32> =
            self.transport_nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i as u32)).collect();

        let mut owner_of: Vec<FactionId> = vec![FactionId(0); self.regions.len()];
        for (f_idx, fac) in self.factions.iter().enumerate() {
            for region in &fac.regions {
                owner_of[index_of[region.as_str()] as usize] = FactionId(f_idx as u32);
            }
        }

        let regions: Vec<Region> = self
            .regions
            .iter()
            .enumerate()
            .map(|(i, def)| Region {
                id: RegionId(i as u32),
                name: def.name.clone(),
                terrain: def.terrain,
                owner: owner_of[i],
                core: owner_of[i],
                population: def.population,
                capacity: def.capacity,
                infrastructure: def.infrastructure,
                port: def.port,
                mobilized: 0.0,
                unrest: 0.0,
                occupation: 0.0,
                occupier: None,
                occupation_kind: None,
                links: def
                    .links
                    .iter()
                    .map(|l| Link {
                        to: RegionId(index_of[l.to.as_str()]),
                        kind: l.kind,
                        strait_zone: l.strait_zone.as_deref().map(|z| SeaZoneId(zone_index_of[z])),
                    })
                    .collect(),
                devastation: 0.0,
                construction: None,
                import_flow: 0.0,
                position: def.position,
                air_superiority: vec![AirSuperiority::NEUTRAL; self.factions.len()],
            })
            .collect();

        let sea_zones: Vec<SeaZone> = self
            .sea_zones
            .iter()
            .enumerate()
            .map(|(i, def)| SeaZone {
                id: SeaZoneId(i as u32),
                name: def.name.clone(),
                coast: def.coast.iter().map(|r| RegionId(index_of[r.as_str()])).collect(),
                adjacent: def.adjacent.iter().map(|z| SeaZoneId(zone_index_of[z.as_str()])).collect(),
                control: vec![0.0; self.factions.len()],
            })
            .collect();

        let factions: Vec<Faction> = self
            .factions
            .iter()
            .enumerate()
            .map(|(i, def)| Faction {
                id: FactionId(i as u32),
                name: def.name.clone(),
                capital: RegionId(index_of[def.capital.as_str()]),
                manpower: FACTION_MANPOWER,
                stock: FACTION_STOCK,
                conscription: FACTION_CONSCRIPTION,
                industry_priority: FACTION_INDUSTRY_PRIORITY,
                civilian_ration: crate::balance::CIVILIAN_RATION_DEFAULT,
                war_support: FACTION_WAR_SUPPORT,
                stability: FACTION_STABILITY,
                shortage: 0.0,
                shortage_by_good: [0.0; GOOD_COUNT],
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
                national_focus: FACTION_NATIONAL_FOCUS_DEFAULT,
                focus_transition_days: 0,
                alive: true,
            })
            .collect();

        let mut units = Vec::new();
        for (f_idx, def) in self.factions.iter().enumerate() {
            let faction_id = FactionId(f_idx as u32);
            let capital = RegionId(index_of[def.capital.as_str()]);

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
                    name: format!("{} Corps {}", def.name, i + 1),
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

        let transport_nodes: Vec<TransportNode> = self
            .transport_nodes
            .iter()
            .enumerate()
            .map(|(i, def)| TransportNode {
                id: TransportNodeId(i as u32),
                name: def.name.clone(),
                kind: def.kind,
                region: RegionId(index_of[def.region.as_str()]),
                // Stage 10C: never scenario-authored - see `TransportNode::
                // condition`'s own doc.
                condition: Condition::FULL,
            })
            .collect();

        let transport_lines: Vec<TransportLine> = self
            .transport_lines
            .iter()
            .enumerate()
            .map(|(i, def)| TransportLine {
                id: TransportLineId(i as u32),
                from: TransportNodeId(transport_node_index_of[def.from.as_str()]),
                to: TransportNodeId(transport_node_index_of[def.to.as_str()]),
                kind: def.kind,
                capacity: def.capacity,
                condition: def.condition,
            })
            .collect();

        let supply: Vec<f32> = regions.iter().map(Region::supply_source).collect();
        // Initial value only - `logistics::recompute_supply` overwrites both
        // this and `supply` from `compute_transport_flow` before either is
        // ever read for real. Seeded the same way `supply` itself is: only
        // each region's own owner has a non-zero entry, since no faction has
        // occupied any foreign territory yet at scenario start.
        let supply_by_faction: Vec<Vec<f32>> = regions
            .iter()
            .zip(supply.iter())
            .map(|(region, &s)| {
                let mut row = vec![0.0f32; factions.len()];
                row[region.owner.index()] = s;
                row
            })
            .collect();

        // Initial value only, like `supply_by_faction` above - and seeded
        // the same local way rather than left at zero.
        //
        // `codex review` (P2): "overwritten before it is ever read" was not
        // actually true. `Simulation::with_world` accepts actions, and an
        // agent may inspect the world, *before* the first `step` ever calls
        // `logistics::recompute_supply`. A zero here would make
        // `naval::fleet_unit_supply_avail` report every starting fleet as
        // having no route home on turn one, so a perfectly valid pre-step
        // `ReinforceUnit` against a healthy owned port would be refused.
        // Seeded from each facing port region's own `supply` estimate
        // (exactly how `supply`/`supply_by_faction` above are seeded) so the
        // pre-step value is a coarse local approximation rather than a wrong
        // one; the first `recompute_supply` replaces it with the real
        // flow-model figure as before.
        let supply_sea: Vec<Vec<f32>> = sea_zones
            .iter()
            .map(|zone| {
                let mut row = vec![0.0f32; factions.len()];
                for &r in &zone.coast {
                    let region = &regions[r.index()];
                    let has_port_node = transport_nodes
                        .iter()
                        .any(|n| n.kind == crate::transport::TransportNodeKind::Port && n.region == r);
                    if has_port_node {
                        let f = region.owner.index();
                        row[f] = row[f].max(supply[r.index()]);
                    }
                }
                row
            })
            .collect();

        // Scenario-scoped starting diplomacy (this type's own doc): every
        // pair inside a declared bloc starts at `Stance::Alliance`, every
        // other pair starts at `Stance::War` - `Diplomacy::new_with_blocs`
        // with an empty `blocs` list (`scenarios/mvp.json`'s declaration)
        // reproduces `Diplomacy::new`'s old unconditional "初期状態は全勢力
        // が相互に War" (docs/phase3-spec.md Stage 3B) exactly.
        let blocs: Vec<Vec<FactionId>> = self
            .diplomacy
            .blocs
            .iter()
            .map(|b| b.factions.iter().map(|f| FactionId(faction_index_of[f.as_str()])).collect())
            .collect();
        let diplomacy = Diplomacy::new_with_blocs(self.factions.len(), &blocs);

        // Stage 10A: unlike `supply`/`supply_by_faction`/`supply_sea` above,
        // no coarse local seed is needed here - a freshly-built `World`
        // never has any `Domain::Air` unit at all (`units` above is
        // synthesized land-only; a `Station::Airfield` unit can only come
        // from a later `Action::RecruitUnit { domain: Domain::Air, .. }`),
        // so there is nothing pre-step `action::apply_reinforce` could ever
        // need to read this for before the first real `logistics::
        // recompute_supply` call populates it for real.
        let supply_air: Vec<Vec<f32>> = vec![vec![0.0f32; factions.len()]; transport_nodes.len()];

        World {
            regions,
            factions,
            units,
            supply,
            supply_by_faction,
            supply_sea,
            supply_air,
            // Never read before the first real `logistics::recompute_supply`
            // call (`World::supply_leftover`'s own doc): a freshly-built
            // `World`'s units are always stationed exactly where their own
            // `arms_delivery_station` says (`action::apply_recruit`'s
            // initial placement, mirrored here), and only `military::
            // tick_movement` - reachable only from inside `Simulation::step`,
            // which always runs `recompute_supply` first - can ever make the
            // two disagree. An empty default is therefore never indexed into
            // by `logistics::instantaneous_land_grant`/`instantaneous_sea_
            // grant`; `Simulation::step`'s first tick overwrites it with a
            // real snapshot before anything could.
            supply_leftover: Default::default(),
            sea_zones,
            transport_nodes,
            transport_lines,
            day: 0,
            diplomacy,
            victory: self.victory.clone(),
        }
    }

    /// The inverse of `parse`: renders this scenario back to the same JSON
    /// schema `parse` reads (`scenario_roundtrip`). Not required to match
    /// any hand-written file byte-for-byte - `crate::json::Value`'s own
    /// compact, deterministic (`BTreeMap`-backed) rendering is all that's
    /// needed for `parse(self.to_json())` to reproduce `self` exactly.
    pub fn to_json(&self) -> String {
        let regions = Value::arr(
            self.regions
                .iter()
                .map(|r| {
                    let capacity =
                        Value::Object(ALL_GOODS.iter().map(|g| (g.key().to_string(), Value::f32num(r.capacity[g.index()]))).collect());
                    let links = Value::arr(
                        r.links
                            .iter()
                            .map(|l| {
                                let mut pairs = vec![("to", Value::str(l.to.clone())), ("kind", Value::str(l.kind.key()))];
                                if let Some(z) = &l.strait_zone {
                                    pairs.push(("strait_zone", Value::str(z.clone())));
                                }
                                Value::obj(pairs)
                            })
                            .collect(),
                    );
                    let mut pairs = vec![
                        ("id", Value::str(r.id.clone())),
                        ("name", Value::str(r.name.clone())),
                        ("terrain", Value::str(r.terrain.key())),
                        ("population", Value::f32num(r.population)),
                        ("capacity", capacity),
                        ("infrastructure", Value::f32num(r.infrastructure)),
                        ("port", Value::f32num(r.port)),
                        ("links", links),
                    ];
                    pairs.push(("position", Value::arr(vec![Value::f32num(r.position[0]), Value::f32num(r.position[1])])));
                    Value::obj(pairs)
                })
                .collect(),
        );
        let sea_zones = Value::arr(
            self.sea_zones
                .iter()
                .map(|z| {
                    Value::obj(vec![
                        ("id", Value::str(z.id.clone())),
                        ("name", Value::str(z.name.clone())),
                        ("coast", Value::arr(z.coast.iter().map(|r| Value::str(r.clone())).collect())),
                        ("adjacent", Value::arr(z.adjacent.iter().map(|a| Value::str(a.clone())).collect())),
                    ])
                })
                .collect(),
        );
        let transport = Value::obj(vec![
            (
                "nodes",
                Value::arr(
                    self.transport_nodes
                        .iter()
                        .map(|n| {
                            Value::obj(vec![
                                ("id", Value::str(n.id.clone())),
                                ("name", Value::str(n.name.clone())),
                                ("kind", Value::str(n.kind.key())),
                                ("region", Value::str(n.region.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "lines",
                Value::arr(
                    self.transport_lines
                        .iter()
                        .map(|l| {
                            Value::obj(vec![
                                ("from", Value::str(l.from.clone())),
                                ("to", Value::str(l.to.clone())),
                                ("kind", Value::str(l.kind.key())),
                                ("capacity", Value::f32num(l.capacity.get())),
                                ("condition", Value::f32num(l.condition.get())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]);
        let factions = Value::arr(
            self.factions
                .iter()
                .map(|fac| {
                    Value::obj(vec![
                        ("id", Value::str(fac.id.clone())),
                        ("name", Value::str(fac.name.clone())),
                        ("capital", Value::str(fac.capital.clone())),
                        ("regions", Value::arr(fac.regions.iter().map(|r| Value::str(r.clone())).collect())),
                    ])
                })
                .collect(),
        );
        let diplomacy = Value::obj(vec![(
            "blocs",
            Value::arr(
                self.diplomacy
                    .blocs
                    .iter()
                    .map(|b| {
                        Value::obj(vec![
                            ("id", Value::str(b.id.clone())),
                            ("name", Value::str(b.name.clone())),
                            ("factions", Value::arr(b.factions.iter().map(|f| Value::str(f.clone())).collect())),
                        ])
                    })
                    .collect(),
            ),
        )]);
        let victory = Value::arr(
            self.victory
                .iter()
                .map(|v| match v {
                    VictoryCondition::Conquest => Value::obj(vec![("type", Value::str("conquest"))]),
                    VictoryCondition::Coalition => Value::obj(vec![("type", Value::str("coalition"))]),
                    VictoryCondition::Domination(share) => {
                        Value::obj(vec![("type", Value::str("domination")), ("share", Value::f32num(share.get()))])
                    }
                })
                .collect(),
        );
        Value::obj(vec![
            ("regions", regions),
            ("sea_zones", sea_zones),
            ("transport", transport),
            ("factions", factions),
            ("diplomacy", diplomacy),
            ("victory", victory),
        ])
        .to_json()
    }
}

/// Parses, validates and builds a `World` from a JSON scenario document
/// already in memory.
pub fn load_str(json_text: &str) -> Result<World, ScenarioError> {
    let scenario = Scenario::parse(json_text)?;
    scenario.validate()?;
    Ok(scenario.build_world())
}

/// `load_str`, but reading the document from a file first
/// (`--scenario <path>` on both `apps/headless` and `archipelago-api`).
pub fn load_file(path: &str) -> Result<World, ScenarioError> {
    let text = std::fs::read_to_string(path).map_err(|e| ScenarioError::Io(format!("{path}: {e}")))?;
    load_str(&text)
}

/// Builds the embedded default scenario (`scenarios/mvp.json`, the current
/// 10-region/3-faction MVP map). Panics only if that embedded file is
/// itself malformed - a build-time invariant of this crate, not something
/// any runtime input can trigger.
pub fn build_world() -> World {
    load_str(MVP_JSON).expect("embedded scenarios/mvp.json must be a valid scenario")
}

#[cfg(test)]
pub(crate) fn embedded_mvp_json() -> &'static str {
    MVP_JSON
}
