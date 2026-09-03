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
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::json::{self, Value};
use crate::military::Unit;
use crate::world::{Faction, Link, LinkKind, Region, SeaZone, Station, Terrain, World};

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
    /// The region graph (built from every region's `links`) isn't
    /// connected - these regions can't reach the rest by any link at all,
    /// so supply could never reach them (docs/phase6-spec.md "グラフが連結
    /// か（孤立地域は補給が届かず、意図しない限り誤り）").
    Disconnected { unreachable: Vec<String> },
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
            ScenarioError::Disconnected { unreachable } => {
                write!(f, "region graph is not connected: unreachable from the rest of the map: {}", unreachable.join(", "))
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

/// The parsed (but not necessarily valid) contents of a scenario file - the
/// data `docs/phase6-spec.md`'s "シナリオの外部化" moves out of Rust source.
/// `RegionDef` order fixes `RegionId` assignment (first region in the file
/// is `RegionId(0)`, and so on) - the same for `SeaZoneDef`/`FactionDef` -
/// so a scenario file's array order is significant, not just cosmetic.
#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    pub regions: Vec<RegionDef>,
    pub sea_zones: Vec<SeaZoneDef>,
    pub factions: Vec<FactionDef>,
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
    Ok(RegionDef { id, name, terrain, population, capacity, infrastructure, port, links })
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

        let factions_value = require_object_field(&root, "$", "factions")?;
        let factions = require_array(factions_value, "factions")?
            .iter()
            .enumerate()
            .map(|(i, f)| parse_faction(f, &format!("factions[{i}]")))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Scenario { regions, sea_zones, factions })
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

        let supply: Vec<f32> = regions.iter().map(Region::supply_source).collect();

        // Stage 3B (docs/phase3-spec.md "Stage 3B": "初期状態は全勢力が相互に
        // War"): `Diplomacy::new` starts every pair at `Stance::War`, exactly
        // reproducing the pre-Stage-3B assumption every earlier
        // scenario/test already relies on.
        let diplomacy = Diplomacy::new(self.factions.len());

        World { regions, factions, units, supply, sea_zones, day: 0, diplomacy }
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
                    Value::obj(vec![
                        ("id", Value::str(r.id.clone())),
                        ("name", Value::str(r.name.clone())),
                        ("terrain", Value::str(r.terrain.key())),
                        ("population", Value::f32num(r.population)),
                        ("capacity", capacity),
                        ("infrastructure", Value::f32num(r.infrastructure)),
                        ("port", Value::f32num(r.port)),
                        ("links", links),
                    ])
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
        Value::obj(vec![("regions", regions), ("sea_zones", sea_zones), ("factions", factions)]).to_json()
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
