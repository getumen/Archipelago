//! JSON &lt;-&gt; `archipelago_sim::action::Action` conversion, and the
//! `Event`/`ActionError` -> JSON encoders. This is the one place the API
//! turns untrusted bytes into an `Action` - every other line of validation
//! still goes through `Simulation::apply` unchanged (docs/phase5-spec.md
//! "不正入力の扱い": "既にあるものを外に出すだけ").
//!
//! Decoding never panics: a malformed or unknown action object becomes
//! `Err(String)` with a human-readable reason, which the caller reports in
//! `rejected[]` next to the actions `Simulation::apply` itself rejected -
//! from an RL agent's point of view, "I described an action `Simulation`
//! doesn't recognize" and "I described a well-formed action `Simulation`
//! refused" are the same kind of event (a rejection with a reason), so both
//! flow into the same response array.

use archipelago_sim::action::{Action, ActionError};
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Treaty, TreatyTerm, ALL_TREATIES};
use archipelago_sim::focus::{NationalFocus, ALL_FOCI};
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use archipelago_sim::world::{Domain, Station};

use crate::json::Value;

fn good_from_key(key: &str) -> Option<Good> {
    ALL_GOODS.iter().copied().find(|g| g.key() == key)
}

fn project_from_value(v: &Value) -> Result<Project, String> {
    match v {
        Value::String(s) => match s.as_str() {
            "infrastructure" => Ok(Project::Infrastructure),
            "port" => Ok(Project::Port),
            "repair" => Ok(Project::Repair),
            other => Err(format!("unknown project `{other}`")),
        },
        Value::Object(_) => {
            let good_key = v.get("capacity").and_then(Value::as_str).ok_or("expected `capacity` to name a good")?;
            let good = good_from_key(good_key).ok_or_else(|| format!("unknown good `{good_key}`"))?;
            Ok(Project::Capacity(good))
        }
        _ => Err("project must be a string or {\"capacity\":<good>}".to_string()),
    }
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

fn station_from_value(v: &Value) -> Result<Station, String> {
    let kind = v.get("kind").and_then(Value::as_str).ok_or("station needs a `kind` of \"region\" or \"sea\"")?;
    match kind {
        "region" => {
            let id = v.get("id").and_then(Value::as_u32).ok_or("region station needs an integer `id`")?;
            Ok(Station::Region(RegionId(id)))
        }
        "sea" => {
            let id = v.get("id").and_then(Value::as_u32).ok_or("sea station needs an integer `id`")?;
            Ok(Station::Sea(SeaZoneId(id)))
        }
        other => Err(format!("unknown station kind `{other}`")),
    }
}

fn unit_id(v: &Value, field: &str) -> Result<UnitId, String> {
    v.get(field).and_then(Value::as_u32).map(UnitId).ok_or_else(|| format!("expected integer `{field}`"))
}

fn faction_id(v: &Value, field: &str) -> Result<FactionId, String> {
    v.get(field).and_then(Value::as_u32).map(FactionId).ok_or_else(|| format!("expected integer `{field}`"))
}

fn region_id(v: &Value, field: &str) -> Result<RegionId, String> {
    v.get(field).and_then(Value::as_u32).map(RegionId).ok_or_else(|| format!("expected integer `{field}`"))
}

fn f32_field(v: &Value, field: &str) -> Result<f32, String> {
    v.get(field).and_then(Value::as_f32).ok_or_else(|| format!("expected finite number `{field}`"))
}

fn good_field(v: &Value, field: &str) -> Result<Good, String> {
    let key = v.get(field).and_then(Value::as_str).ok_or_else(|| format!("expected string `{field}`"))?;
    good_from_key(key).ok_or_else(|| format!("unknown good `{key}`"))
}

fn treaty_term_from_value(v: &Value) -> Result<TreatyTerm, String> {
    let kind = v.get("kind").and_then(Value::as_str).ok_or("treaty term needs a `kind`")?;
    match kind {
        "sign" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`sign` term needs `treaty`")?;
            Ok(TreatyTerm::Sign(treaty_from_key(treaty_key)?))
        }
        "withdraw" => Ok(TreatyTerm::Withdraw { from: region_id(v, "from")? }),
        "cede" => Ok(TreatyTerm::Cede { region: region_id(v, "region")? }),
        "deliver" => Ok(TreatyTerm::Deliver { good: good_field(v, "good")?, amount: f32_field(v, "amount")? }),
        other => Err(format!("unknown treaty term kind `{other}`")),
    }
}

/// One request-level bound on top of `Simulation::apply`'s own per-action
/// validation (docs/phase5-spec.md "リクエストサイズに上限を設ける。1 リク
/// エストの行動数にも上限を設ける"): how many `Action`s a single `/action`
/// call may carry. Checked by the HTTP handler before this module is even
/// asked to decode anything.
pub const MAX_ACTIONS_PER_REQUEST: usize = 4096;

/// Decodes one JSON action object into an `Action`. Every failure is a
/// `String` reason, never a panic - unknown fields are ignored (forward
/// compatible), missing/mistyped required fields are rejected.
pub fn action_from_value(v: &Value) -> Result<Action, String> {
    let kind = v.get("type").and_then(Value::as_str).ok_or("action object needs a string `type`")?;
    match kind {
        "move_unit" => {
            let unit = unit_id(v, "unit")?;
            let to = v.get("to").ok_or("`move_unit` needs `to`")?;
            Ok(Action::MoveUnit { unit, to: station_from_value(to)? })
        }
        "hold_unit" => Ok(Action::HoldUnit { unit: unit_id(v, "unit")? }),
        "disband_unit" => Ok(Action::DisbandUnit { unit: unit_id(v, "unit")? }),
        "recruit_unit" => {
            let region = region_id(v, "region")?;
            let domain = match v.get("domain").and_then(Value::as_str) {
                Some("land") | None => Domain::Land,
                Some("sea") => Domain::Sea,
                Some(other) => return Err(format!("unknown domain `{other}`")),
            };
            Ok(Action::RecruitUnit { region, domain })
        }
        "reinforce_unit" => Ok(Action::ReinforceUnit { unit: unit_id(v, "unit")? }),
        "set_conscription" => Ok(Action::SetConscription(f32_field(v, "value")?)),
        "set_industry_priority" => {
            Ok(Action::SetIndustryPriority { good: good_field(v, "good")?, weight: f32_field(v, "weight")? })
        }
        "set_civilian_ration" => Ok(Action::SetCivilianRation(f32_field(v, "value")?)),
        "build" => Ok(Action::Build {
            region: region_id(v, "region")?,
            project: project_from_value(v.get("project").ok_or("`build` needs `project`")?)?,
        }),
        "cancel_build" => Ok(Action::CancelBuild { region: region_id(v, "region")? }),
        "set_import_plan" => {
            Ok(Action::SetImportPlan { good: good_field(v, "good")?, rate: f32_field(v, "rate")? })
        }
        "set_logistics_priority" => {
            Ok(Action::SetLogisticsPriority { good: good_field(v, "good")?, weight: f32_field(v, "weight")? })
        }
        "propose_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`propose_treaty` needs `treaty`")?;
            Ok(Action::ProposeTreaty { to: faction_id(v, "to")?, treaty: treaty_from_key(treaty_key)? })
        }
        "accept_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`accept_treaty` needs `treaty`")?;
            Ok(Action::AcceptTreaty { from: faction_id(v, "from")?, treaty: treaty_from_key(treaty_key)? })
        }
        "reject_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`reject_treaty` needs `treaty`")?;
            Ok(Action::RejectTreaty { from: faction_id(v, "from")?, treaty: treaty_from_key(treaty_key)? })
        }
        "declare_war" => Ok(Action::DeclareWar { to: faction_id(v, "to")? }),
        "break_treaty" => {
            let treaty_key = v.get("treaty").and_then(Value::as_str).ok_or("`break_treaty` needs `treaty`")?;
            Ok(Action::BreakTreaty { with: faction_id(v, "with")?, treaty: treaty_from_key(treaty_key)? })
        }
        "set_national_focus" => {
            let key = v.get("focus").and_then(Value::as_str).ok_or("`set_national_focus` needs `focus`")?;
            Ok(Action::SetNationalFocus(focus_from_key(key)?))
        }
        "propose_in_natural_language" => {
            let text = v.get("text").and_then(Value::as_str).ok_or("`propose_in_natural_language` needs `text`")?;
            Ok(Action::ProposeInNaturalLanguage { to: faction_id(v, "to")?, text: text.to_string() })
        }
        "respond_to_natural_language_proposal" => {
            let from = faction_id(v, "from")?;
            let accept = v.get("accept").and_then(Value::as_bool).ok_or("expected bool `accept`")?;
            let terms_v = v.get("terms").and_then(Value::as_array).ok_or("expected array `terms`")?;
            if terms_v.len() > MAX_ACTIONS_PER_REQUEST {
                return Err("too many treaty terms".to_string());
            }
            let mut terms = Vec::with_capacity(terms_v.len());
            for t in terms_v {
                terms.push(treaty_term_from_value(t)?);
            }
            Ok(Action::RespondToNaturalLanguageProposal { from, terms, accept })
        }
        other => Err(format!("unknown action type `{other}`")),
    }
}

// --- GET /schema (server::handle_schema) --------------------------------
//
// A description of every action type `action_from_value` accepts, built
// next to the decoder it describes so the two can't drift apart silently -
// the same defect shape as A2's `json.rs` truncation and, per this stage's
// brief, one that has already bitten this codebase twice. Two different
// defences against that drift are used here, at two different strengths:
//
// - The *enum vocabularies* (`good`, `treaty`, `focus`) are built by
//   mapping the exact same `ALL_GOODS`/`ALL_TREATIES`/`ALL_FOCI` arrays and
//   `key()` methods `good_from_key`/`treaty_from_key`/`focus_from_key`
//   above already use to decode those same strings - so those three
//   vocabularies literally cannot list a name the decoder doesn't accept,
//   or omit one it does; they're the same data, not a copy of it.
// - The action list itself (names, required/optional fields, nested object
//   shapes) has no such mechanical link - a hand-written `match` has no
//   runtime type to introspect without a proc-macro or build-script this
//   workspace doesn't have (§0: zero external dependencies). Instead,
//   `tests::schema_matches_decoder` decodes a synthesized sample of every
//   action type this function advertises and asserts `action_from_value`
//   accepts it - so an entry added here that doesn't match the decoder (or
//   a decoder change this table falls behind) fails a test, not silently.

fn field(name: &str, ty: &str, required: bool) -> Value {
    Value::obj(vec![("name", Value::str(name)), ("type", Value::str(ty)), ("required", Value::Bool(required))])
}

fn field_enum(name: &str, enum_name: &str, required: bool) -> Value {
    Value::obj(vec![
        ("name", Value::str(name)),
        ("type", Value::str("enum")),
        ("enum", Value::str(enum_name)),
        ("required", Value::Bool(required)),
    ])
}

fn field_object(name: &str, object_name: &str, required: bool) -> Value {
    Value::obj(vec![
        ("name", Value::str(name)),
        ("type", Value::str("object")),
        ("object", Value::str(object_name)),
        ("required", Value::Bool(required)),
    ])
}

fn field_array(name: &str, item_object_name: &str, required: bool) -> Value {
    Value::obj(vec![
        ("name", Value::str(name)),
        ("type", Value::str("array")),
        ("items", Value::str(item_object_name)),
        ("required", Value::Bool(required)),
    ])
}

fn action_entry(kind: &str, fields: Vec<Value>) -> Value {
    Value::obj(vec![("type", Value::str(kind)), ("fields", Value::arr(fields))])
}

/// One entry per `action_from_value` match arm, in the same order, each
/// naming exactly the fields that arm reads off `v`.
fn actions_schema() -> Value {
    Value::arr(vec![
        action_entry("move_unit", vec![field("unit", "integer", true), field_object("to", "station", true)]),
        action_entry("hold_unit", vec![field("unit", "integer", true)]),
        action_entry("disband_unit", vec![field("unit", "integer", true)]),
        action_entry(
            "recruit_unit",
            vec![field("region", "integer", true), field_enum("domain", "domain", false)],
        ),
        action_entry("reinforce_unit", vec![field("unit", "integer", true)]),
        action_entry("set_conscription", vec![field("value", "number", true)]),
        action_entry(
            "set_industry_priority",
            vec![field_enum("good", "good", true), field("weight", "number", true)],
        ),
        action_entry("set_civilian_ration", vec![field("value", "number", true)]),
        action_entry("build", vec![field("region", "integer", true), field_object("project", "project", true)]),
        action_entry("cancel_build", vec![field("region", "integer", true)]),
        action_entry(
            "set_import_plan",
            vec![field_enum("good", "good", true), field("rate", "number", true)],
        ),
        action_entry(
            "set_logistics_priority",
            vec![field_enum("good", "good", true), field("weight", "number", true)],
        ),
        action_entry("propose_treaty", vec![field("to", "integer", true), field_enum("treaty", "treaty", true)]),
        action_entry("accept_treaty", vec![field("from", "integer", true), field_enum("treaty", "treaty", true)]),
        action_entry("reject_treaty", vec![field("from", "integer", true), field_enum("treaty", "treaty", true)]),
        action_entry("declare_war", vec![field("to", "integer", true)]),
        action_entry(
            "break_treaty",
            vec![field("with", "integer", true), field_enum("treaty", "treaty", true)],
        ),
        action_entry("set_national_focus", vec![field_enum("focus", "focus", true)]),
        action_entry(
            "propose_in_natural_language",
            vec![field("to", "integer", true), field("text", "string", true)],
        ),
        action_entry(
            "respond_to_natural_language_proposal",
            vec![
                field("from", "integer", true),
                field("accept", "boolean", true),
                field_array("terms", "treaty_term", true),
            ],
        ),
    ])
}

/// The enum vocabularies referenced by `actions_schema()`'s `"enum"`
/// fields. `good`/`treaty`/`focus` are read from the same `ALL_*` arrays
/// and `key()` methods the decoder itself calls (see this section's own
/// doc); `domain`/`station_kind`/`treaty_term_kind` name literal string
/// arms in `action_from_value`/`station_from_value`/`treaty_term_from_value`
/// that have no backing enum array to share, so `schema_matches_decoder`
/// is what actually keeps those three in sync instead.
fn enums_schema() -> Value {
    Value::obj(vec![
        ("good", Value::arr(ALL_GOODS.iter().map(|g| Value::str(g.key())).collect())),
        ("treaty", Value::arr(ALL_TREATIES.iter().map(|t| Value::str(t.key())).collect())),
        ("focus", Value::arr(ALL_FOCI.iter().map(|f| Value::str(f.key())).collect())),
        ("domain", Value::arr(vec![Value::str("land"), Value::str("sea")])),
        ("station_kind", Value::arr(vec![Value::str("region"), Value::str("sea")])),
        (
            "treaty_term_kind",
            Value::arr(vec![Value::str("sign"), Value::str("withdraw"), Value::str("cede"), Value::str("deliver")]),
        ),
        ("project_kind", Value::arr(vec![Value::str("infrastructure"), Value::str("port"), Value::str("repair"), Value::str("capacity")])),
    ])
}

/// Nested object shapes referenced from `actions_schema()` by name
/// (`field_object`/`field_array`'s `"object"`/`"items"`), matching
/// `station_from_value`, `project_from_value`, and `treaty_term_from_value`
/// respectively.
fn objects_schema() -> Value {
    Value::obj(vec![
        (
            "station",
            Value::obj(vec![(
                "fields",
                Value::arr(vec![field_enum("kind", "station_kind", true), field("id", "integer", true)]),
            )]),
        ),
        (
            "project",
            Value::obj(vec![
                (
                    "note",
                    Value::str("either a string naming one of enums.project_kind's non-\"capacity\" entries, or {\"capacity\":<good>}"),
                ),
                ("capacity_fields", Value::arr(vec![field_enum("capacity", "good", true)])),
            ]),
        ),
        (
            "treaty_term",
            Value::obj(vec![
                ("kind_field", field_enum("kind", "treaty_term_kind", true)),
                (
                    "variant_fields",
                    Value::obj(vec![
                        ("sign", Value::arr(vec![field_enum("treaty", "treaty", true)])),
                        ("withdraw", Value::arr(vec![field("from", "integer", true)])),
                        ("cede", Value::arr(vec![field("region", "integer", true)])),
                        (
                            "deliver",
                            Value::arr(vec![field_enum("good", "good", true), field("amount", "number", true)]),
                        ),
                    ]),
                ),
            ]),
        ),
    ])
}

/// The full action-vocabulary/enum/object schema `GET /schema`
/// (`server::handle_schema`) reports, minus the observation-vector and
/// scenario-id-range sections that live closer to their own source of
/// truth (`archipelago_sim::observation`, `archipelago_sim::scenario`) -
/// see `server::handle_schema` for where those get merged in.
pub fn schema() -> Value {
    Value::obj(vec![("actions", actions_schema()), ("enums", enums_schema()), ("objects", objects_schema())])
}

pub fn action_error_key(e: ActionError) -> &'static str {
    match e {
        ActionError::NotOwner => "not_owner",
        ActionError::UnitDead => "unit_dead",
        ActionError::NotAdjacent => "not_adjacent",
        ActionError::Pinned => "pinned",
        ActionError::RegionNotOwned => "region_not_owned",
        ActionError::RegionContested => "region_contested",
        ActionError::InsufficientManpower => "insufficient_manpower",
        ActionError::InsufficientEquipment => "insufficient_equipment",
        ActionError::InvalidValue => "invalid_value",
        ActionError::AlreadyBuilding => "already_building",
        ActionError::NoConstruction => "no_construction",
        ActionError::NoPort => "no_port",
    }
}

/// One rejected action's report: the index it arrived at in the request's
/// `actions[]` array (so a caller submitting 1000 actions can tell exactly
/// which ones failed without re-deriving it) and why.
pub fn rejected_value(index: usize, reason: &str) -> Value {
    Value::obj(vec![("index", Value::num(index as f64)), ("reason", Value::str(reason))])
}

pub fn event_to_value(event: &archipelago_sim::event::Event) -> Value {
    use archipelago_sim::event::Event;
    let kind = match event {
        Event::Battle { .. } => "battle",
        Event::NavalBattle { .. } => "naval_battle",
        Event::UnitDestroyed { .. } => "unit_destroyed",
        Event::RegionCaptured { .. } => "region_captured",
        Event::FactionEliminated { .. } => "faction_eliminated",
        Event::Strike { .. } => "strike",
        Event::Protest { .. } => "protest",
        Event::Mutiny { .. } => "mutiny",
        Event::CapitalFlight { .. } => "capital_flight",
        Event::RegimeChange { .. } => "regime_change",
        Event::Separatism { .. } => "separatism",
        Event::TreatyProposed { .. } => "treaty_proposed",
        Event::TreatySigned { .. } => "treaty_signed",
        Event::TreatyRejected { .. } => "treaty_rejected",
        Event::TreatyBroken { .. } => "treaty_broken",
        Event::WarDeclared { .. } => "war_declared",
        Event::AllianceDragIn { .. } => "alliance_drag_in",
        Event::NaturalLanguageProposed { .. } => "natural_language_proposed",
        Event::NaturalLanguageAccepted { .. } => "natural_language_accepted",
        Event::NaturalLanguageRejected { .. } => "natural_language_rejected",
        Event::NaturalLanguageTermsInvalid { .. } => "natural_language_terms_invalid",
    };
    Value::obj(vec![("kind", Value::str(kind)), ("text", Value::str(event.to_string()))])
}
