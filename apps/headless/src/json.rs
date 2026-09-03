//! A hand-written JSON serializer for the final simulation state
//! (mvp-spec.md §8's `--json` flag). No external crate: the workspace is
//! dependency-free, so this covers only what `World`/`Outcome` need
//! (strings, finite numbers, bools, arrays, objects, null).

use archipelago_sim::sim::Outcome;
use archipelago_sim::world::World;

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
                "{{\"id\":{},\"name\":{},\"alive\":{},\"regions\":{},\"units\":{},\"manpower\":{},\"supplies\":{},\"equipment\":{},\"conscription\":{},\"production_mix\":{},\"war_support\":{},\"stability\":{},\"shortage\":{},\"casualties\":{},\"supply_ratio\":{}}}",
                f.id.0,
                string(&f.name),
                f.alive,
                world.region_count(f.id),
                units,
                number(f.manpower),
                number(f.supplies),
                number(f.equipment),
                number(f.conscription),
                number(f.production_mix),
                number(f.war_support),
                number(f.stability),
                number(f.shortage),
                number(f.casualties),
                number(f.supply_ratio),
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
                "{{\"id\":{},\"name\":{},\"owner\":{},\"owner_name\":{},\"core\":{},\"unrest\":{},\"occupation\":{},\"occupier\":{},\"supply\":{}}}",
                r.id.0,
                string(&r.name),
                r.owner.0,
                string(&world.faction(r.owner).name),
                r.core.0,
                number(r.unrest),
                number(r.occupation),
                occupier,
                number(world.supply[r.id.index()]),
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}
