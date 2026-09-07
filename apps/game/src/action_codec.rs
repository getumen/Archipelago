//! JSON &lt;-&gt; `Action` conversion for `--record`/`--replay` (docs/phase7-spec.md
//! "Stage 7B — 遊ぶ" §"決定論"), plus the record-file read/write helpers.
//! No `bevy` import anywhere in this file - same "sim-driving logic stays
//! Bevy-free" discipline `crate::sim_driver`/`crate::layout` already follow,
//! since a recording is exactly as much a determinism artifact as the tick
//! loop that produces it.
//!
//! The wire format is a JSON array of per-day arrays:
//! `[[<action>, ...], [], [<action>, ...], ...]`, one entry per
//! `SimDriver::tick()` call the human/replay faction was alive for - an
//! empty `[]` for a day with no orders still occupies a slot, so a replay's
//! day-N action list lines up with the original run's day-N regardless of
//! how many of those days were quiet. Reuses `archipelago_sim::json`
//! (already zero-dependency, already used by `crate::scenario`/the API
//! crate) rather than adding a `serde` dependency anywhere in the
//! workspace.
//!
//! `action_to_value`/`action_from_value` cover every `Action` variant (not
//! just the ones the Stage 7B UI can currently issue) so this codec never
//! silently drops a future action kind - mirrors
//! `archipelago-api`'s own `action_codec.rs` decoder, which this file is
//! deliberately kept close to in shape (same field names/action-type
//! strings) even though it can't reuse that crate directly (`apps/game`
//! depends on `archipelago-sim`/`archipelago-agents` only).

use std::path::Path;

use archipelago_sim::action::{Action, ActionError, Layer, ALL_LAYERS};
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Treaty, TreatyTerm};
use archipelago_sim::focus::NationalFocus;
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, TransportLineId, TransportNodeId, UnitId};
use archipelago_sim::json::{self, Value};
use archipelago_sim::world::{Domain, Station};

fn good_key(good: Good) -> &'static str {
    good.key()
}

fn good_from_key(key: &str) -> Option<Good> {
    ALL_GOODS.iter().copied().find(|g| g.key() == key)
}

fn layer_key(layer: Layer) -> &'static str {
    layer.key()
}

fn layer_from_key(key: &str) -> Result<Layer, String> {
    ALL_LAYERS.into_iter().find(|l| l.key() == key).ok_or_else(|| format!("unknown layer `{key}`"))
}

fn treaty_key(t: Treaty) -> &'static str {
    t.key()
}

fn treaty_from_key(key: &str) -> Result<Treaty, String> {
    match key {
        "ceasefire" => Ok(Treaty::Ceasefire),
        "non_aggression" => Ok(Treaty::NonAggression),
        "alliance" => Ok(Treaty::Alliance),
        "military_access" => Ok(Treaty::MilitaryAccess),
        "port_access" => Ok(Treaty::PortAccess),
        "trade_agreement" => Ok(Treaty::TradeAgreement),
        other => Err(format!("unknown treaty `{other}`")),
    }
}

fn focus_key(f: NationalFocus) -> &'static str {
    f.key()
}

fn focus_from_key(key: &str) -> Result<NationalFocus, String> {
    match key {
        "military_unification" => Ok(NationalFocus::MilitaryUnification),
        "economic_sphere" => Ok(NationalFocus::EconomicSphere),
        "alliance_network" => Ok(NationalFocus::AllianceNetwork),
        "maritime_trade" => Ok(NationalFocus::MaritimeTrade),
        "technocracy" => Ok(NationalFocus::Technocracy),
        "defensive_posture" => Ok(NationalFocus::DefensivePosture),
        other => Err(format!("unknown national focus `{other}`")),
    }
}

fn station_to_value(s: Station) -> Value {
    match s {
        Station::Region(r) => Value::obj(vec![("kind", Value::str("region")), ("id", Value::num(r.0 as f64))]),
        Station::Sea(z) => Value::obj(vec![("kind", Value::str("sea")), ("id", Value::num(z.0 as f64))]),
        // Stage 10A: round-trip support only - 10A ships no `Domain::Air`
        // `MoveUnit`/recording support, so this arm is never actually
        // produced by anything this crate's own UI issues yet, but keeping
        // it means `every_action_variant_round_trips` can exercise the
        // shape as soon as some future stage starts recording one, rather
        // than the encoder silently lacking a case the decoder already has.
        Station::Airfield(n) => Value::obj(vec![("kind", Value::str("airfield")), ("id", Value::num(n.0 as f64))]),
    }
}

fn station_from_value(v: &Value) -> Result<Station, String> {
    let kind =
        v.get("kind").and_then(Value::as_str).ok_or("station needs a `kind` of \"region\", \"sea\" or \"airfield\"")?;
    match kind {
        "region" => Ok(Station::Region(RegionId(v.get("id").and_then(Value::as_u32).ok_or("region station needs an integer `id`")?))),
        "sea" => Ok(Station::Sea(SeaZoneId(v.get("id").and_then(Value::as_u32).ok_or("sea station needs an integer `id`")?))),
        "airfield" => Ok(Station::Airfield(TransportNodeId(
            v.get("id").and_then(Value::as_u32).ok_or("airfield station needs an integer `id`")?,
        ))),
        other => Err(format!("unknown station kind `{other}`")),
    }
}

fn project_to_value(p: Project) -> Value {
    match p {
        Project::Infrastructure => Value::str("infrastructure"),
        Project::Port => Value::str("port"),
        Project::Repair => Value::str("repair"),
        Project::Capacity(good) => Value::obj(vec![("capacity", Value::str(good_key(good)))]),
        Project::TransportLine(line) => Value::obj(vec![("transport_line", Value::num(line.0 as f64))]),
    }
}

fn project_from_value(v: &Value) -> Result<Project, String> {
    match v {
        Value::String(s) => match s.as_str() {
            "infrastructure" => Ok(Project::Infrastructure),
            "port" => Ok(Project::Port),
            "repair" => Ok(Project::Repair),
            other => Err(format!("unknown project `{other}`")),
        },
        Value::Object(_) if v.get("capacity").is_some() => {
            let good_key = v.get("capacity").and_then(Value::as_str).ok_or("expected `capacity` to name a good")?;
            Ok(Project::Capacity(good_from_key(good_key).ok_or_else(|| format!("unknown good `{good_key}`"))?))
        }
        Value::Object(_) if v.get("transport_line").is_some() => {
            let line = v.get("transport_line").and_then(Value::as_u32).ok_or("expected integer `transport_line`")?;
            Ok(Project::TransportLine(TransportLineId(line)))
        }
        _ => Err("project must be a string, {\"capacity\":<good>}, or {\"transport_line\":<id>}".to_string()),
    }
}

fn treaty_term_to_value(t: TreatyTerm) -> Value {
    match t {
        TreatyTerm::Sign(treaty) => Value::obj(vec![("kind", Value::str("sign")), ("treaty", Value::str(treaty_key(treaty)))]),
        TreatyTerm::Withdraw { from } => Value::obj(vec![("kind", Value::str("withdraw")), ("from", Value::num(from.0 as f64))]),
        TreatyTerm::Cede { region } => Value::obj(vec![("kind", Value::str("cede")), ("region", Value::num(region.0 as f64))]),
        TreatyTerm::Deliver { good, amount } => {
            Value::obj(vec![("kind", Value::str("deliver")), ("good", Value::str(good_key(good))), ("amount", Value::f32num(amount))])
        }
    }
}

fn treaty_term_from_value(v: &Value) -> Result<TreatyTerm, String> {
    let kind = v.get("kind").and_then(Value::as_str).ok_or("treaty term needs a `kind`")?;
    match kind {
        "sign" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`sign` term needs `treaty`")?;
            Ok(TreatyTerm::Sign(treaty_from_key(treaty_key)?))
        }
        "withdraw" => Ok(TreatyTerm::Withdraw {
            from: RegionId(v.get("from").and_then(Value::as_u32).ok_or("`withdraw` term needs integer `from`")?),
        }),
        "cede" => Ok(TreatyTerm::Cede {
            region: RegionId(v.get("region").and_then(Value::as_u32).ok_or("`cede` term needs integer `region`")?),
        }),
        "deliver" => {
            let good_key = v.get("good").and_then(Value::as_str).ok_or("`deliver` term needs `good`")?;
            let amount = v.get("amount").and_then(Value::as_f32).ok_or("`deliver` term needs numeric `amount`")?;
            Ok(TreatyTerm::Deliver { good: good_from_key(good_key).ok_or_else(|| format!("unknown good `{good_key}`"))?, amount })
        }
        other => Err(format!("unknown treaty term kind `{other}`")),
    }
}

/// One `Action` -> one JSON object, `{"type": "<kind>", ...fields}` -
/// `action_from_value` is the exact inverse.
pub fn action_to_value(action: &Action) -> Value {
    match action.clone() {
        Action::MoveUnit { unit, to } => {
            Value::obj(vec![("type", Value::str("move_unit")), ("unit", Value::num(unit.0 as f64)), ("to", station_to_value(to))])
        }
        Action::HoldUnit { unit } => Value::obj(vec![("type", Value::str("hold_unit")), ("unit", Value::num(unit.0 as f64))]),
        Action::DisbandUnit { unit } => Value::obj(vec![("type", Value::str("disband_unit")), ("unit", Value::num(unit.0 as f64))]),
        Action::RecruitUnit { region, domain } => Value::obj(vec![
            ("type", Value::str("recruit_unit")),
            ("region", Value::num(region.0 as f64)),
            ("domain", Value::str(domain.key())),
        ]),
        Action::ReinforceUnit { unit } => Value::obj(vec![("type", Value::str("reinforce_unit")), ("unit", Value::num(unit.0 as f64))]),
        Action::SetConscription(v) => Value::obj(vec![("type", Value::str("set_conscription")), ("value", Value::f32num(v))]),
        Action::SetIndustryPriority { good, weight } => Value::obj(vec![
            ("type", Value::str("set_industry_priority")),
            ("good", Value::str(good_key(good))),
            ("weight", Value::f32num(weight)),
        ]),
        Action::SetCivilianRation(v) => Value::obj(vec![("type", Value::str("set_civilian_ration")), ("value", Value::f32num(v))]),
        Action::Build { region, project } => {
            Value::obj(vec![("type", Value::str("build")), ("region", Value::num(region.0 as f64)), ("project", project_to_value(project))])
        }
        Action::CancelBuild { region } => Value::obj(vec![("type", Value::str("cancel_build")), ("region", Value::num(region.0 as f64))]),
        Action::SetImportPlan { good, rate } => {
            Value::obj(vec![("type", Value::str("set_import_plan")), ("good", Value::str(good_key(good))), ("rate", Value::f32num(rate))])
        }
        Action::SetLogisticsPriority { good, weight } => Value::obj(vec![
            ("type", Value::str("set_logistics_priority")),
            ("good", Value::str(good_key(good))),
            ("weight", Value::f32num(weight)),
        ]),
        Action::ProposeTreaty { to, treaty } => {
            Value::obj(vec![("type", Value::str("propose_treaty")), ("to", Value::num(to.0 as f64)), ("treaty", Value::str(treaty_key(treaty)))])
        }
        Action::AcceptTreaty { from, treaty } => Value::obj(vec![
            ("type", Value::str("accept_treaty")),
            ("from", Value::num(from.0 as f64)),
            ("treaty", Value::str(treaty_key(treaty))),
        ]),
        Action::RejectTreaty { from, treaty } => Value::obj(vec![
            ("type", Value::str("reject_treaty")),
            ("from", Value::num(from.0 as f64)),
            ("treaty", Value::str(treaty_key(treaty))),
        ]),
        Action::DeclareWar { to } => Value::obj(vec![("type", Value::str("declare_war")), ("to", Value::num(to.0 as f64))]),
        Action::BreakTreaty { with, treaty } => Value::obj(vec![
            ("type", Value::str("break_treaty")),
            ("with", Value::num(with.0 as f64)),
            ("treaty", Value::str(treaty_key(treaty))),
        ]),
        Action::SetNationalFocus(focus) => Value::obj(vec![("type", Value::str("set_national_focus")), ("focus", Value::str(focus_key(focus)))]),
        Action::ProposeInNaturalLanguage { to, text } => {
            Value::obj(vec![("type", Value::str("propose_in_natural_language")), ("to", Value::num(to.0 as f64)), ("text", Value::str(text))])
        }
        Action::RespondToNaturalLanguageProposal { from, terms, accept } => Value::obj(vec![
            ("type", Value::str("respond_to_natural_language_proposal")),
            ("from", Value::num(from.0 as f64)),
            ("accept", Value::Bool(accept)),
            ("terms", Value::arr(terms.into_iter().map(treaty_term_to_value).collect())),
        ]),
        Action::InterdictLine { line } => {
            Value::obj(vec![("type", Value::str("interdict_line")), ("line", Value::num(line.0 as f64))])
        }
        Action::StrikeNode { node } => {
            Value::obj(vec![("type", Value::str("strike_node")), ("node", Value::num(node.0 as f64))])
        }
    }
}

/// Decodes one JSON action object into an `Action`. Mirrors
/// `archipelago-api`'s `action_codec::action_from_value` field-for-field
/// (see this module's own doc) - never panics, every failure is a `String`
/// reason.
pub fn action_from_value(v: &Value) -> Result<Action, String> {
    let kind = v.get("type").and_then(Value::as_str).ok_or("action object needs a string `type`")?;
    let u32_field = |field: &str| -> Result<u32, String> { v.get(field).and_then(Value::as_u32).ok_or_else(|| format!("expected integer `{field}`")) };
    let f32_field = |field: &str| -> Result<f32, String> { v.get(field).and_then(Value::as_f32).ok_or_else(|| format!("expected finite number `{field}`")) };
    match kind {
        "move_unit" => {
            let unit = UnitId(u32_field("unit")?);
            let to = v.get("to").ok_or("`move_unit` needs `to`")?;
            Ok(Action::MoveUnit { unit, to: station_from_value(to)? })
        }
        "hold_unit" => Ok(Action::HoldUnit { unit: UnitId(u32_field("unit")?) }),
        "disband_unit" => Ok(Action::DisbandUnit { unit: UnitId(u32_field("unit")?) }),
        "recruit_unit" => {
            let region = RegionId(u32_field("region")?);
            let domain = match v.get("domain").and_then(Value::as_str) {
                None => Domain::Land,
                Some(key) => Domain::from_key(key).ok_or_else(|| format!("unknown domain `{key}`"))?,
            };
            Ok(Action::RecruitUnit { region, domain })
        }
        "reinforce_unit" => Ok(Action::ReinforceUnit { unit: UnitId(u32_field("unit")?) }),
        "set_conscription" => Ok(Action::SetConscription(f32_field("value")?)),
        "set_industry_priority" => {
            let good_key = v.get("good").and_then(Value::as_str).ok_or("expected string `good`")?;
            Ok(Action::SetIndustryPriority {
                good: good_from_key(good_key).ok_or_else(|| format!("unknown good `{good_key}`"))?,
                weight: f32_field("weight")?,
            })
        }
        "set_civilian_ration" => Ok(Action::SetCivilianRation(f32_field("value")?)),
        "build" => Ok(Action::Build {
            region: RegionId(u32_field("region")?),
            project: project_from_value(v.get("project").ok_or("`build` needs `project`")?)?,
        }),
        "cancel_build" => Ok(Action::CancelBuild { region: RegionId(u32_field("region")?) }),
        "set_import_plan" => {
            let good_key = v.get("good").and_then(Value::as_str).ok_or("expected string `good`")?;
            Ok(Action::SetImportPlan { good: good_from_key(good_key).ok_or_else(|| format!("unknown good `{good_key}`"))?, rate: f32_field("rate")? })
        }
        "set_logistics_priority" => {
            let good_key = v.get("good").and_then(Value::as_str).ok_or("expected string `good`")?;
            Ok(Action::SetLogisticsPriority {
                good: good_from_key(good_key).ok_or_else(|| format!("unknown good `{good_key}`"))?,
                weight: f32_field("weight")?,
            })
        }
        "propose_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`propose_treaty` needs `treaty`")?;
            Ok(Action::ProposeTreaty { to: FactionId(u32_field("to")?), treaty: treaty_from_key(treaty_key)? })
        }
        "accept_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`accept_treaty` needs `treaty`")?;
            Ok(Action::AcceptTreaty { from: FactionId(u32_field("from")?), treaty: treaty_from_key(treaty_key)? })
        }
        "reject_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`reject_treaty` needs `treaty`")?;
            Ok(Action::RejectTreaty { from: FactionId(u32_field("from")?), treaty: treaty_from_key(treaty_key)? })
        }
        "declare_war" => Ok(Action::DeclareWar { to: FactionId(u32_field("to")?) }),
        "break_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`break_treaty` needs `treaty`")?;
            Ok(Action::BreakTreaty { with: FactionId(u32_field("with")?), treaty: treaty_from_key(treaty_key)? })
        }
        "set_national_focus" => {
            let key = v.get("focus").and_then(Value::as_str).ok_or("`set_national_focus` needs `focus`")?;
            Ok(Action::SetNationalFocus(focus_from_key(key)?))
        }
        "propose_in_natural_language" => {
            let text = v.get("text").and_then(Value::as_str).ok_or("`propose_in_natural_language` needs `text`")?;
            Ok(Action::ProposeInNaturalLanguage { to: FactionId(u32_field("to")?), text: text.to_string() })
        }
        "respond_to_natural_language_proposal" => {
            let from = FactionId(u32_field("from")?);
            let accept = v.get("accept").and_then(Value::as_bool).ok_or("expected bool `accept`")?;
            let terms_v = v.get("terms").and_then(Value::as_array).ok_or("expected array `terms`")?;
            let mut terms = Vec::with_capacity(terms_v.len());
            for t in terms_v {
                terms.push(treaty_term_from_value(t)?);
            }
            Ok(Action::RespondToNaturalLanguageProposal { from, terms, accept })
        }
        "interdict_line" => Ok(Action::InterdictLine { line: TransportLineId(u32_field("line")?) }),
        "strike_node" => Ok(Action::StrikeNode { node: TransportNodeId(u32_field("node")?) }),
        other => Err(format!("unknown action type `{other}`")),
    }
}

pub fn action_error_ja(e: ActionError) -> &'static str {
    match e {
        ActionError::NotOwner => "自分の部隊・地域ではない",
        ActionError::UnitDead => "その部隊は存在しない（すでに撃破された）",
        ActionError::NotAdjacent => "隣接していない、または輸送手段がない",
        ActionError::Pinned => "敵部隊がいるため移動できない",
        ActionError::RegionNotOwned => "自国の地域ではない",
        ActionError::RegionContested => "地域が係争中（敵部隊がいる）",
        ActionError::InsufficientManpower => "人的資源が不足している",
        ActionError::InsufficientEquipment => "装備が不足している",
        ActionError::InvalidValue => "指定した値が範囲外",
        ActionError::AlreadyBuilding => "この地域はすでに建設中",
        ActionError::NoConstruction => "この地域に建設中の工事がない",
        ActionError::NoPort => "この地域に港湾がない",
        ActionError::NoAirfield => "この地域に飛行場がない",
        ActionError::InsufficientMachinery => "機械が不足している",
        ActionError::InvalidLine => "指定した輸送路線が存在しない",
        ActionError::LineNotOwned => "自国の輸送路線ではない",
        ActionError::LineNotHostile => "交戦中の敵の輸送路線ではない",
        ActionError::InvalidNode => "指定した輸送ノードが存在しない",
        ActionError::NodeNotStrikeable => "飛行場・港以外は攻撃対象にできない",
        ActionError::NodeNotHostile => "交戦中の敵の拠点ではない",
    }
}

/// Encodes a full recording - one `Vec<Action>` per day - as compact JSON.
pub fn encode_days(days: &[Vec<Action>]) -> String {
    days_to_value(days).to_json()
}

fn days_to_value(days: &[Vec<Action>]) -> Value {
    Value::arr(days.iter().map(|day| Value::arr(day.iter().map(action_to_value).collect())).collect())
}

fn days_from_value(value: &Value) -> Result<Vec<Vec<Action>>, String> {
    let days = value.as_array().ok_or("expected a JSON array of days")?;
    let mut out = Vec::with_capacity(days.len());
    for (i, day) in days.iter().enumerate() {
        let actions_v = day.as_array().ok_or_else(|| format!("day {i}: expected an array of actions"))?;
        let mut actions = Vec::with_capacity(actions_v.len());
        for (j, a) in actions_v.iter().enumerate() {
            actions.push(action_from_value(a).map_err(|e| format!("day {i} action {j}: {e}"))?);
        }
        out.push(actions);
    }
    Ok(out)
}

/// The inverse of `encode_days`. Rejects anything that isn't "array of
/// arrays of action objects" - the same "never panic on malformed input"
/// discipline `archipelago_sim::json`/`action_from_value` already apply,
/// since a `--replay` file is just as untrusted as any other input this
/// workspace reads from disk.
pub fn decode_days(text: &str) -> Result<Vec<Vec<Action>>, String> {
    let value = json::parse(text, 32).map_err(|e| e.to_string())?;
    days_from_value(&value).map_err(|e| format!("recording must be a top-level JSON array (one entry per day): {e}"))
}

/// Writes a full recording to `path` (overwriting it) - `--record`'s file
/// format, and the *full-scope* half of `--replay`'s file format (see
/// `decode_replay`'s own doc): a bare JSON array of days, exactly as before
/// layer-scoped replay existed. Called after every tick with the recording
/// accumulated so far (Stage 7B has no other flush point; the client
/// doesn't shut down cleanly), so a recording on disk is always at most one
/// day stale.
pub fn write_record(path: &Path, days: &[Vec<Action>]) -> std::io::Result<()> {
    std::fs::write(path, encode_days(days))
}

/// Reads and decodes a `--replay <path>` recording written by
/// `write_record` (full-scope only, no `layers` field to speak of). Kept
/// alongside `read_replay` (which accepts *either* shape) purely because
/// every existing full-scope-only call site - `--record`'s own round-trip
/// tests, and anything that only ever wrote via `write_record` - has no
/// reason to start handling a `layers` field it never asked for.
pub fn read_record(path: &Path) -> Result<Vec<Vec<Action>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    decode_days(&text)
}

/// Encodes a *layer-scoped* recording: `{"layers": [...key strings...],
/// "days": [...]}`. `layers` is the set of `Layer`s this recording's
/// author declares it drives for the played faction - see
/// `crate::sim_driver::Replay`'s own doc for why this has to be declared
/// explicitly rather than inferred from which actions happen to appear on
/// any given day.
pub fn encode_scoped_days(layers: &[Layer], days: &[Vec<Action>]) -> String {
    Value::obj(vec![
        ("layers", Value::arr(layers.iter().map(|&l| Value::str(layer_key(l))).collect())),
        ("days", days_to_value(days)),
    ])
    .to_json()
}

/// Writes a layer-scoped recording (`encode_scoped_days`'s own doc) to
/// `path`, overwriting it - the format `--replay` reads back a `layers`
/// field out of, distinct from `write_record`'s bare-array, always-full-scope
/// format.
pub fn write_scoped_record(path: &Path, layers: &[Layer], days: &[Vec<Action>]) -> std::io::Result<()> {
    std::fs::write(path, encode_scoped_days(layers, days))
}

/// Decodes a `--replay <path>` recording in *either* of the two shapes
/// `--replay` accepts:
///
/// - a bare JSON array of days (`decode_days`'s own format, everything
///   `write_record`/`--record` has ever produced) - the replay claims
///   `ALL_LAYERS`, reproducing today's "the whole faction is scripted"
///   behavior with no change at all;
/// - an object `{"layers": [...], "days": [...]}` (`encode_scoped_days`) -
///   the replay claims exactly the declared `layers` and nothing else, for
///   `crate::sim_driver::SimDriver` to hand every other `Layer` to a fresh
///   AI agent instead (see that module's own doc).
///
/// The `layers` field is never optional on the object shape and never
/// inferred from the recorded actions themselves - a scripted faction that
/// happens not to touch a layer on any given day must stay distinguishable
/// from one that never owned that layer at all, which is exactly the
/// distinction an inferred scope could never make (docs/conventions.md's
/// no-fallback principle: a missing/absent declaration is reported as
/// missing, not silently guessed at from data that cannot express it).
pub fn decode_replay(text: &str) -> Result<(Vec<Layer>, Vec<Vec<Action>>), String> {
    let value = json::parse(text, 32).map_err(|e| e.to_string())?;
    match &value {
        Value::Array(_) => Ok((ALL_LAYERS.to_vec(), days_from_value(&value)?)),
        Value::Object(_) => {
            let layers_v = value.get("layers").and_then(Value::as_array).ok_or("scoped replay needs an array `layers` field")?;
            let mut layers = Vec::with_capacity(layers_v.len());
            for l in layers_v {
                let key = l.as_str().ok_or("`layers` entries must be strings")?;
                let layer = layer_from_key(key)?;
                if layers.contains(&layer) {
                    return Err(format!("`layers` names `{key}` more than once"));
                }
                layers.push(layer);
            }
            let days_v = value.get("days").ok_or("scoped replay needs a `days` field")?;
            let days = days_from_value(days_v)?;
            Ok((layers, days))
        }
        _ => Err("replay must be either a JSON array of days, or an object with `layers` and `days`".to_string()),
    }
}

/// Reads and decodes a `--replay <path>` recording - see `decode_replay`'s
/// own doc for the two shapes accepted.
pub fn read_replay(path: &Path) -> Result<(Vec<Layer>, Vec<Vec<Action>>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    decode_replay(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::good::Good;

    /// Every `Action` variant round-trips through `action_to_value`/
    /// `action_from_value` unchanged - the codec `recorded_play_replays_
    /// identically` (in `crate::sim_driver`) relies on to make a recording
    /// mean anything at all.
    #[test]
    fn every_action_variant_round_trips() {
        let samples = vec![
            Action::MoveUnit { unit: UnitId(3), to: Station::Region(RegionId(5)) },
            Action::MoveUnit { unit: UnitId(3), to: Station::Sea(SeaZoneId(1)) },
            Action::HoldUnit { unit: UnitId(2) },
            Action::DisbandUnit { unit: UnitId(2) },
            Action::RecruitUnit { region: RegionId(1), domain: Domain::Land },
            Action::RecruitUnit { region: RegionId(1), domain: Domain::Sea },
            Action::ReinforceUnit { unit: UnitId(4) },
            Action::SetConscription(0.42),
            Action::SetIndustryPriority { good: Good::Steel, weight: 0.6 },
            Action::SetCivilianRation(0.8),
            Action::Build { region: RegionId(2), project: Project::Infrastructure },
            Action::Build { region: RegionId(2), project: Project::Capacity(Good::Munitions) },
            Action::CancelBuild { region: RegionId(2) },
            Action::SetImportPlan { good: Good::Food, rate: 12.5 },
            Action::SetLogisticsPriority { good: Good::Arms, weight: 0.3 },
            Action::ProposeTreaty { to: FactionId(1), treaty: Treaty::Alliance },
            Action::AcceptTreaty { from: FactionId(1), treaty: Treaty::Ceasefire },
            Action::RejectTreaty { from: FactionId(1), treaty: Treaty::NonAggression },
            Action::DeclareWar { to: FactionId(2) },
            Action::BreakTreaty { with: FactionId(2), treaty: Treaty::Alliance },
            Action::SetNationalFocus(NationalFocus::MaritimeTrade),
            Action::ProposeInNaturalLanguage { to: FactionId(1), text: "港湾利用を認めてほしい".to_string() },
            Action::RespondToNaturalLanguageProposal {
                from: FactionId(1),
                terms: vec![TreatyTerm::Sign(Treaty::PortAccess), TreatyTerm::Deliver { good: Good::Steel, amount: 3.0 }],
                accept: true,
            },
            Action::InterdictLine { line: archipelago_sim::ids::TransportLineId(4) },
            Action::Build { region: RegionId(2), project: Project::TransportLine(archipelago_sim::ids::TransportLineId(4)) },
        ];
        for action in samples {
            let value = action_to_value(&action);
            let decoded = action_from_value(&value).unwrap_or_else(|e| panic!("failed to decode {value:?}: {e}"));
            assert_eq!(decoded, action, "round-trip mismatch for {action:?}");
        }
    }

    #[test]
    fn decode_days_rejects_malformed_input() {
        assert!(decode_days("{}").is_err(), "a top-level object (not an array) must be rejected");
        assert!(decode_days("[[{\"type\":\"nonsense\"}]]").is_err(), "an unknown action type must be rejected");
        assert!(decode_days("not json at all").is_err());
    }

    #[test]
    fn empty_days_round_trip() {
        let days: Vec<Vec<Action>> = vec![vec![], vec![Action::HoldUnit { unit: UnitId(0) }], vec![]];
        let encoded = encode_days(&days);
        let decoded = decode_days(&encoded).unwrap();
        assert_eq!(decoded, days, "an empty day must still occupy a slot, not collapse away");
    }

    /// `decode_replay` on a bare array (everything `write_record` has ever
    /// produced) must report `ALL_LAYERS` - a full-scope replay file, read
    /// through the new dual-shape decoder, must mean exactly what it always
    /// meant.
    #[test]
    fn decode_replay_treats_a_bare_array_as_full_scope() {
        let days: Vec<Vec<Action>> = vec![vec![Action::SetConscription(0.4)]];
        let (layers, decoded) = decode_replay(&encode_days(&days)).unwrap();
        assert_eq!(layers, ALL_LAYERS.to_vec(), "a bare-array replay file must claim every layer");
        assert_eq!(decoded, days);
    }

    /// `encode_scoped_days`/`decode_replay` must round-trip a declared
    /// layer subset exactly, in whatever order it was given - the whole
    /// point of the new format is that this set is read back verbatim, not
    /// normalized or inferred.
    #[test]
    fn scoped_replay_round_trips_its_declared_layers() {
        let layers = vec![Layer::Economy, Layer::Diplomacy];
        let days: Vec<Vec<Action>> = vec![vec![Action::SetConscription(0.1)], vec![]];
        let encoded = encode_scoped_days(&layers, &days);
        let (decoded_layers, decoded_days) = decode_replay(&encoded).unwrap();
        assert_eq!(decoded_layers, layers, "the declared layer set must come back exactly as written");
        assert_eq!(decoded_days, days);
    }

    #[test]
    fn decode_replay_rejects_a_scoped_file_missing_layers_or_days() {
        assert!(decode_replay("{\"days\":[]}").is_err(), "a scoped object with no `layers` field must be rejected, not defaulted to full scope");
        assert!(decode_replay("{\"layers\":[\"economy\"]}").is_err(), "a scoped object with no `days` field must be rejected");
        assert!(decode_replay("{\"layers\":[\"not_a_real_layer\"],\"days\":[]}").is_err(), "an unknown layer key must be rejected");
        assert!(
            decode_replay("{\"layers\":[\"economy\",\"economy\"],\"days\":[]}").is_err(),
            "a layer named twice in `layers` must be rejected rather than silently deduplicated"
        );
    }
}
