//! A hand-written JSON serializer for the final simulation state
//! (mvp-spec.md §8's `--json` flag). No external crate: the workspace is
//! dependency-free, so this covers only what `World`/`Outcome` need
//! (strings, finite numbers, bools, arrays, objects, null).

use archipelago_sim::construction::{Construction, Project};
use archipelago_sim::diplomacy::ALL_TREATIES;
use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::group::ALL_GROUPS;
use archipelago_sim::ids::FactionId;
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::{Domain, Station, World};

/// Renders a `[f32; GOOD_COUNT]`-shaped array as a JSON object keyed by
/// `Good::key()`, e.g. `{"food":1.0,"energy":2.0,...}`.
fn good_object(values: &[f32]) -> String {
    let items: Vec<String> = ALL_GOODS
        .iter()
        .map(|g| format!("{}:{}", string(g.key()), number(values[g.index()])))
        .collect();
    format!("{{{}}}", items.join(","))
}

/// Renders a `[f32; GROUP_COUNT]`-shaped array as a JSON object keyed by
/// `Group::key()` (Stage 3A), e.g. `{"government":60.0,...}`.
fn group_object(values: &[f32]) -> String {
    let items: Vec<String> = ALL_GROUPS
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

/// Stage 3B (docs/phase3-spec.md "Stage 3B"): every faction pair's stance,
/// opinion (both directions) and active grant treaties, plus the pending
/// proposal queue.
fn serialize_diplomacy(world: &World) -> String {
    let n = world.factions.len();
    let mut pairs = Vec::new();
    for a_idx in 0..n {
        for b_idx in (a_idx + 1)..n {
            let a = FactionId(a_idx as u32);
            let b = FactionId(b_idx as u32);
            let grants: Vec<String> = ALL_TREATIES
                .iter()
                .copied()
                .filter(|t| !t.is_stance() && world.diplomacy.has_treaty(a, b, *t))
                .map(|t| string(t.key()))
                .collect();
            pairs.push(format!(
                "{{\"a\":{},\"b\":{},\"stance\":{},\"opinion_a_of_b\":{},\"opinion_b_of_a\":{},\"grants\":[{}]}}",
                a.0,
                b.0,
                string(world.diplomacy.stance(a, b).key()),
                number(world.diplomacy.opinion(a, b)),
                number(world.diplomacy.opinion(b, a)),
                grants.join(","),
            ));
        }
    }
    let pending: Vec<String> = world
        .diplomacy
        .pending
        .iter()
        .map(|p| {
            format!(
                "{{\"from\":{},\"to\":{},\"treaty\":{}}}",
                p.from.0,
                p.to.0,
                string(p.treaty.key()),
            )
        })
        .collect();
    format!("{{\"pairs\":[{}],\"pending\":[{}]}}", pairs.join(","), pending.join(","))
}

pub fn serialize_state(world: &World, seed: u64, outcome: Outcome) -> String {
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!("\"seed\":{seed},"));
    out.push_str(&format!("\"day\":{},", world.day));
    out.push_str(&format!("\"outcome\":{},", serialize_outcome(world, outcome)));
    out.push_str(&format!("\"factions\":{},", serialize_factions(world)));
    out.push_str(&format!("\"regions\":{},", serialize_regions(world)));
    out.push_str(&format!("\"sea_zones\":{},", serialize_sea_zones(world)));
    out.push_str(&format!("\"diplomacy\":{}", serialize_diplomacy(world)));
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
            format!(
                "{{\"id\":{},\"name\":{},\"alive\":{},\"regions\":{},\"units\":{},\"fleets\":{},\"manpower\":{},\"stock\":{},\"conscription\":{},\"industry_priority\":{},\"civilian_ration\":{},\"war_support\":{},\"stability\":{},\"shortage\":{},\"casualties\":{},\"supply_ratio\":{},\"import_plan\":{},\"logistics_priority\":{},\"group_support\":{},\"group_influence\":{},\"strike_days\":{},\"regime_change_days\":{},\"protest_active\":{},\"mutiny_active\":{},\"capital_flight_active\":{}}}",
                f.id.0,
                string(&f.name),
                f.alive,
                world.region_count(f.id),
                units,
                fleets,
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
                group_object(&f.group_support),
                group_object(&f.group_influence),
                f.strike_days,
                f.regime_change_days,
                f.protest_active,
                f.mutiny_active,
                f.capital_flight_active,
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

/// Stage 2D (docs/phase2-spec.md "海域の表(制海権と艦隊数)"): sea control per
/// faction and the fleet count present, per sea zone.
fn serialize_sea_zones(world: &World) -> String {
    let items: Vec<String> = world
        .sea_zones
        .iter()
        .map(|z| {
            let control: Vec<String> = z.control.iter().map(|&c| number(c)).collect();
            let fleets = world
                .units
                .iter()
                .filter(|u| u.alive && u.station == Station::Sea(z.id))
                .count();
            format!(
                "{{\"id\":{},\"name\":{},\"coast\":{},\"adjacent\":{},\"control\":[{}],\"fleets\":{}}}",
                z.id.0,
                string(&z.name),
                format!("[{}]", z.coast.iter().map(|r| r.0.to_string()).collect::<Vec<_>>().join(",")),
                format!("[{}]", z.adjacent.iter().map(|zz| zz.0.to_string()).collect::<Vec<_>>().join(",")),
                control.join(","),
                fleets,
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}
