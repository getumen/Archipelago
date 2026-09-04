//! JSON encoding of a `Simulation`'s board (docs/phase5-spec.md "GET
//! /state ... -> 盤面の完全な JSON") and of an `Observation::encode()`
//! vector. Adapted from `apps/headless/src/json.rs`'s hand-written
//! serializer - that file lives in a binary crate this one can't depend on,
//! so the same shape is reproduced here against this crate's `Value` type
//! instead of raw string concatenation.

use archipelago_sim::construction::{Construction, Project};
use archipelago_sim::diplomacy::ALL_TREATIES;
use archipelago_sim::focus;
use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::group::ALL_GROUPS;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::{Domain, Station, VictoryCondition, World};

use crate::json::Value;

fn good_object(values: &[f32]) -> Value {
    Value::Object(ALL_GOODS.iter().map(|g| (g.key().to_string(), Value::f32num(values[g.index()]))).collect())
}

fn group_object(values: &[f32]) -> Value {
    Value::Object(ALL_GROUPS.iter().map(|g| (g.key().to_string(), Value::f32num(values[g.index()]))).collect())
}

fn project_key(project: Project) -> String {
    match project {
        Project::Infrastructure => "infrastructure".to_string(),
        Project::Port => "port".to_string(),
        Project::Capacity(good) => format!("capacity:{}", good.key()),
        Project::Repair => "repair".to_string(),
    }
}

fn construction_value(construction: &Option<Construction>) -> Value {
    match construction {
        Some(c) => Value::obj(vec![
            ("project", Value::str(project_key(c.project))),
            ("invested", Value::f32num(c.invested)),
            ("required", Value::f32num(c.required)),
        ]),
        None => Value::Null,
    }
}

/// The declared-condition key `scenario::parse_victory_condition` reads
/// from `"type"` - reused verbatim so a client can trace a `Victory`
/// outcome back to the exact clause in the scenario's `victory` array.
fn victory_condition_key(condition: VictoryCondition) -> &'static str {
    match condition {
        VictoryCondition::Conquest => "conquest",
        VictoryCondition::Coalition => "coalition",
        VictoryCondition::Domination(_) => "domination",
    }
}

/// A `Victory` outcome names every winner honestly (`Outcome::Victory`'s
/// doc): `"winners"` is always a non-empty array, one entry per faction
/// that actually won - never a single `"faction"` field that would pick an
/// arbitrary representative out of a `Coalition`/`Domination` group and
/// silently drop the rest.
fn outcome_value(world: &World, outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Victory { condition, winners } => Value::obj(vec![
            ("type", Value::str("victory")),
            ("condition", Value::str(victory_condition_key(*condition))),
            (
                "winners",
                Value::arr(
                    winners
                        .iter()
                        .map(|&f| {
                            Value::obj(vec![
                                ("faction", Value::num(f.0 as f64)),
                                ("faction_name", Value::str(world.faction(f).name.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]),
        Outcome::Stalemate => Value::obj(vec![("type", Value::str("stalemate"))]),
        Outcome::Ongoing => Value::obj(vec![("type", Value::str("ongoing"))]),
    }
}

fn diplomacy_value(world: &World) -> Value {
    let n = world.factions.len();
    let mut pairs = Vec::new();
    for a_idx in 0..n {
        for b_idx in (a_idx + 1)..n {
            let a = FactionId(a_idx as u32);
            let b = FactionId(b_idx as u32);
            let grants: Vec<Value> = ALL_TREATIES
                .iter()
                .copied()
                .filter(|t| !t.is_stance() && world.diplomacy.has_treaty(a, b, *t))
                .map(|t| Value::str(t.key()))
                .collect();
            pairs.push(Value::obj(vec![
                ("a", Value::num(a.0 as f64)),
                ("b", Value::num(b.0 as f64)),
                ("stance", Value::str(world.diplomacy.stance(a, b).key())),
                ("opinion_a_of_b", Value::f32num(world.diplomacy.opinion(a, b))),
                ("opinion_b_of_a", Value::f32num(world.diplomacy.opinion(b, a))),
                ("grants", Value::arr(grants)),
            ]));
        }
    }
    let pending: Vec<Value> = world
        .diplomacy
        .pending
        .iter()
        .map(|p| {
            Value::obj(vec![
                ("from", Value::num(p.from.0 as f64)),
                ("to", Value::num(p.to.0 as f64)),
                ("treaty", Value::str(p.treaty.key())),
            ])
        })
        .collect();
    Value::obj(vec![("pairs", Value::arr(pairs)), ("pending", Value::arr(pending))])
}

fn factions_value(world: &World) -> Value {
    let items: Vec<Value> = world
        .factions
        .iter()
        .map(|f| {
            let units = world
                .units
                .iter()
                .filter(|u| u.alive && u.owner == f.id && u.station.domain() == Domain::Land)
                .count();
            let fleets = world
                .units
                .iter()
                .filter(|u| u.alive && u.owner == f.id && u.station.domain() == Domain::Sea)
                .count();
            Value::obj(vec![
                ("id", Value::num(f.id.0 as f64)),
                ("name", Value::str(f.name.clone())),
                ("alive", Value::Bool(f.alive)),
                ("regions", Value::num(world.region_count(f.id) as f64)),
                ("units", Value::num(units as f64)),
                ("fleets", Value::num(fleets as f64)),
                ("manpower", Value::f32num(f.manpower)),
                ("stock", good_object(&f.stock)),
                ("conscription", Value::f32num(f.conscription)),
                ("industry_priority", good_object(&f.industry_priority)),
                ("civilian_ration", Value::f32num(f.civilian_ration)),
                ("war_support", Value::f32num(f.war_support)),
                ("stability", Value::f32num(f.stability)),
                ("shortage", Value::f32num(f.shortage)),
                ("casualties", Value::f32num(f.casualties)),
                ("supply_ratio", Value::f32num(f.supply_ratio)),
                ("import_plan", good_object(&f.import_plan)),
                ("logistics_priority", good_object(&f.logistics_priority)),
                ("group_support", group_object(&f.group_support)),
                ("group_influence", group_object(&f.group_influence)),
                ("strike_days", Value::num(f.strike_days as f64)),
                ("regime_change_days", Value::num(f.regime_change_days as f64)),
                ("protest_active", Value::Bool(f.protest_active)),
                ("mutiny_active", Value::Bool(f.mutiny_active)),
                ("capital_flight_active", Value::Bool(f.capital_flight_active)),
                ("national_focus", Value::str(f.national_focus.key())),
                ("focus_transition_days", Value::num(f.focus_transition_days as f64)),
                ("focus_active", Value::Bool(focus::active(f).is_some())),
            ])
        })
        .collect();
    Value::arr(items)
}

fn regions_value(world: &World) -> Value {
    let items: Vec<Value> = world
        .regions
        .iter()
        .map(|r| {
            Value::obj(vec![
                ("id", Value::num(r.id.0 as f64)),
                ("name", Value::str(r.name.clone())),
                ("owner", Value::num(r.owner.0 as f64)),
                ("owner_name", Value::str(world.faction(r.owner).name.clone())),
                ("core", Value::num(r.core.0 as f64)),
                ("capacity", good_object(&r.capacity)),
                ("population", Value::f32num(r.population)),
                ("infrastructure", Value::f32num(r.infrastructure)),
                ("port", Value::f32num(r.port)),
                ("unrest", Value::f32num(r.unrest)),
                ("occupation", Value::f32num(r.occupation)),
                ("occupier", r.occupier.map(|f| Value::num(f.0 as f64)).unwrap_or(Value::Null)),
                ("supply", Value::f32num(world.supply[r.id.index()])),
                ("devastation", Value::f32num(r.devastation)),
                ("construction", construction_value(&r.construction)),
                ("import_flow", Value::f32num(r.import_flow)),
                ("node_throughput", Value::f32num(r.node_throughput())),
            ])
        })
        .collect();
    Value::arr(items)
}

fn sea_zones_value(world: &World) -> Value {
    let items: Vec<Value> = world
        .sea_zones
        .iter()
        .map(|z| {
            let fleets = world.units.iter().filter(|u| u.alive && u.station == Station::Sea(z.id)).count();
            Value::obj(vec![
                ("id", Value::num(z.id.0 as f64)),
                ("name", Value::str(z.name.clone())),
                ("coast", Value::arr(z.coast.iter().map(|r| Value::num(r.0 as f64)).collect())),
                ("adjacent", Value::arr(z.adjacent.iter().map(|zz| Value::num(zz.0 as f64)).collect())),
                ("control", Value::arr(z.control.iter().map(|&c| Value::f32num(c)).collect())),
                ("fleets", Value::num(fleets as f64)),
            ])
        })
        .collect();
    Value::arr(items)
}

fn units_value(world: &World) -> Value {
    let items: Vec<Value> = world
        .units
        .iter()
        .map(|u| {
            let station = match u.station {
                Station::Region(r) => Value::obj(vec![("kind", Value::str("region")), ("id", Value::num(r.0 as f64))]),
                Station::Sea(z) => Value::obj(vec![("kind", Value::str("sea")), ("id", Value::num(z.0 as f64))]),
            };
            Value::obj(vec![
                ("id", Value::num(u.id.0 as f64)),
                ("owner", Value::num(u.owner.0 as f64)),
                ("name", Value::str(u.name.clone())),
                ("alive", Value::Bool(u.alive)),
                ("domain", Value::str(if u.station.domain() == Domain::Land { "land" } else { "sea" })),
                ("station", station),
                ("manpower", Value::f32num(u.manpower)),
                ("equipment", Value::f32num(u.equipment)),
                ("organization", Value::f32num(u.organization)),
                ("morale", Value::f32num(u.morale)),
                ("supply", Value::f32num(u.supply)),
            ])
        })
        .collect();
    Value::arr(items)
}

/// The full board (docs/phase5-spec.md's `GET /state`), independent of any
/// one faction's point of view - every field the simulation tracks is
/// already visible to every player in this game (no fog of war), so unlike
/// `observation_value` below this doesn't take a `faction` at all.
pub fn state_value(world: &World, seed: u64, outcome: &Outcome) -> Value {
    Value::obj(vec![
        ("seed", Value::num(seed as f64)),
        ("day", Value::num(world.day as f64)),
        ("outcome", outcome_value(world, outcome)),
        ("factions", factions_value(world)),
        ("regions", regions_value(world)),
        ("sea_zones", sea_zones_value(world)),
        ("units", units_value(world)),
        ("diplomacy", diplomacy_value(world)),
    ])
}

/// `Observation::encode()`'s fixed-length float vector, as a JSON array -
/// the RL-facing counterpart of `state_value`'s full board.
pub fn observation_value(world: &World, faction: FactionId) -> Value {
    let obs = Observation { faction, world };
    Value::arr(obs.encode().into_iter().map(Value::f32num).collect())
}
