//! A hand-written JSON serializer for the final simulation state
//! (mvp-spec.md §8's `--json` flag). No external crate: the workspace is
//! dependency-free, so this covers only what `World`/`Outcome` need
//! (strings, finite numbers, bools, arrays, objects, null).

use archipelago_sim::construction::{Construction, Project};
use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::World;

/// Renders a `[f32; GOOD_COUNT]`-shaped array as a JSON object keyed by
/// `Good::key()`, e.g. `{"food":1.0,"energy":2.0,...}`.
fn good_object(values: &[f32]) -> String {
    let items: Vec<String> = ALL_GOODS
        .iter()
        .map(|g| format!("{}:{}", string(g.key()), number(values[g.index()])))
        .collect();
    format!("{{{}}}", items.join(","))
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn string(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

/// JSON key for a `Project`, e.g. `"infrastructure"`, `"port"`, `"repair"`,
/// or `"capacity:steel"` for `Project::Capacity(Good::Steel)`.
fn project_key(project: Project) -> String {
    match project {
        Project::Infrastructure => "infrastructure".to_string(),
        Project::Port => "port".to_string(),
        Project::Capacity(good) => format!("capacity:{}", good.key()),
        Project::Repair => "repair".to_string(),
    }
}

/// Renders `Region::construction` as `null` or
/// `{"project":...,"invested":...,"required":...}`.
fn construction_object(construction: &Option<Construction>) -> String {
    match construction {
        Some(c) => format!(
            "{{\"project\":{},\"invested\":{},\"required\":{}}}",
            string(&project_key(c.project)),
            number(c.invested),
            number(c.required),
        ),
        None => "null".to_string(),
    }
}

/// Renders as a plain JSON number; the simulation clamps every field this
/// serializer touches, so NaN/Infinity never reach here.
fn number(v: f32) -> String {
    if v.is_finite() { format!("{v}") } else { "0".to_string() }
}

pub fn serialize_state(world: &World, seed: u64, outcome: Outcome) -> String {
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!("\"seed\":{seed},"));
    out.push_str(&format!("\"day\":{},", world.day));
    out.push_str(&format!("\"outcome\":{},", serialize_outcome(world, outcome)));
    out.push_str(&format!("\"factions\":{},", serialize_factions(world)));
    out.push_str(&format!("\"regions\":{}", serialize_regions(world)));
    out.push('}');
    out
}

fn serialize_outcome(world: &World, outcome: Outcome) -> String {
    match outcome {
        Outcome::Victory(faction) => format!(
            "{{\"type\":\"victory\",\"faction\":{},\"faction_name\":{}}}",
            faction.0,
            string(&world.faction(faction).name)
        ),
        Outcome::Stalemate => "{\"type\":\"stalemate\"}".to_string(),
        Outcome::Ongoing => "{\"type\":\"ongoing\"}".to_string(),
    }
}

fn serialize_factions(world: &World) -> String {
    let items: Vec<String> = world
        .factions
        .iter()
        .map(|f| {
            let units = world.units.iter().filter(|u| u.alive && u.owner == f.id).count();
            format!(
                "{{\"id\":{},\"name\":{},\"alive\":{},\"regions\":{},\"units\":{},\"manpower\":{},\"stock\":{},\"conscription\":{},\"industry_priority\":{},\"civilian_ration\":{},\"war_support\":{},\"stability\":{},\"shortage\":{},\"casualties\":{},\"supply_ratio\":{},\"import_plan\":{},\"logistics_priority\":{}}}",
                f.id.0,
                string(&f.name),
                f.alive,
                world.region_count(f.id),
                units,
                number(f.manpower),
                good_object(&f.stock),
                number(f.conscription),
                good_object(&f.industry_priority),
                number(f.civilian_ration),
                number(f.war_support),
                number(f.stability),
                number(f.shortage),
                number(f.casualties),
                number(f.supply_ratio),
                good_object(&f.import_plan),
                good_object(&f.logistics_priority),
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn serialize_regions(world: &World) -> String {
    let items: Vec<String> = world
        .regions
        .iter()
        .map(|r| {
            let occupier = match r.occupier {
                Some(f) => f.0.to_string(),
                None => "null".to_string(),
            };
            format!(
                "{{\"id\":{},\"name\":{},\"owner\":{},\"owner_name\":{},\"core\":{},\"capacity\":{},\"unrest\":{},\"occupation\":{},\"occupier\":{},\"supply\":{},\"devastation\":{},\"construction\":{},\"import_flow\":{},\"node_throughput\":{}}}",
                r.id.0,
                string(&r.name),
                r.owner.0,
                string(&world.faction(r.owner).name),
                r.core.0,
                good_object(&r.capacity),
                number(r.unrest),
                number(r.occupation),
                occupier,
                number(world.supply[r.id.index()]),
                number(r.devastation),
                construction_object(&r.construction),
                number(r.import_flow),
                number(r.node_throughput()),
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}
