//! A hand-written JSON serializer for the final simulation state
//! (mvp-spec.md §8's `--json` flag). No external crate: the workspace is
//! dependency-free, so this covers only what `World`/`Outcome` need
//! (strings, finite numbers, bools, arrays, objects, null).

use archipelago_sim::construction::{Construction, Project};
use archipelago_sim::diplomacy::ALL_TREATIES;
use archipelago_sim::focus;
use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::group::ALL_GROUPS;
use archipelago_sim::ids::FactionId;
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::{Domain, Station, VictoryCondition, World};

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

pub fn serialize_state(world: &World, seed: u64, outcome: &Outcome) -> String {
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

/// The declared-condition key `outcome`'s `"condition"` field prints - the
/// scenario-file `type` string `scenario::parse_victory_condition` reads,
/// reused here so a reader can trace a run's `--json` outcome straight back
/// to the exact clause in the scenario's `victory` array that produced it.
fn victory_condition_key(condition: VictoryCondition) -> &'static str {
    match condition {
        VictoryCondition::Conquest => "conquest",
        VictoryCondition::Coalition => "coalition",
        VictoryCondition::Domination(_) => "domination",
    }
}

/// A `Victory` outcome names every winner honestly (`Outcome::Victory`'s
/// doc) - never a single `"faction"` field that would silently pick one
/// representative out of a `Coalition`/`Domination` group and lie about the
/// rest.
///
/// `VictoryCondition::Conquest` with a single winner is the one exception,
/// and deliberately keeps the exact pre-existing
/// `{"type":"victory","faction":...,"faction_name":...}` shape (no
/// `"condition"`, no `"winners"` array) rather than wrapping it in the new
/// multi-winner shape too: `scenarios/mvp.json` declares only `Conquest`,
/// which can never produce more than one winner, so this is the *only*
/// outcome shape mvp's `--json` output can ever actually emit - preserving
/// it byte-for-byte is what keeps seed 1's 720-day hash unchanged
/// (docs/conventions.md §5). This isn't a lie either way: a lone winner
/// reported alone is exactly as honest as a lone winner reported inside a
/// one-element array, so there's nothing to weigh against the hash
/// guarantee here.
///
/// The exception is keyed on the *condition*, not on the winner count:
/// external review found the original `if let [only] = winners.as_slice()`
/// guard fired for *any* single-winner outcome, so a `Coalition` or
/// `Domination` victory that happened to leave only one faction standing
/// also lost its `"condition"` field, and a consumer had no way to tell
/// which declared rule actually fired. A `Coalition`/`Domination` win -
/// single winner or not - always gets the full `"condition"`/`"winners"`
/// treatment below; only `Conquest` (single-winner by construction) ever
/// takes the legacy branch.
fn serialize_outcome(world: &World, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Victory { condition: VictoryCondition::Conquest, winners } => {
            let [only] = winners.as_slice() else {
                unreachable!("Conquest names exactly one winner - evaluate_victory's Conquest arm");
            };
            format!("{{\"type\":\"victory\",\"faction\":{},\"faction_name\":{}}}", only.0, string(&world.faction(*only).name))
        }
        Outcome::Victory { condition, winners } => {
            let winners_json: Vec<String> = winners
                .iter()
                .map(|&f| format!("{{\"faction\":{},\"faction_name\":{}}}", f.0, string(&world.faction(f).name)))
                .collect();
            format!(
                "{{\"type\":\"victory\",\"condition\":{},\"winners\":[{}]}}",
                string(victory_condition_key(*condition)),
                winners_json.join(",")
            )
        }
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
                "{{\"id\":{},\"name\":{},\"alive\":{},\"regions\":{},\"units\":{},\"fleets\":{},\"manpower\":{},\"stock\":{},\"conscription\":{},\"industry_priority\":{},\"civilian_ration\":{},\"war_support\":{},\"stability\":{},\"shortage\":{},\"casualties\":{},\"supply_ratio\":{},\"import_plan\":{},\"logistics_priority\":{},\"group_support\":{},\"group_influence\":{},\"strike_days\":{},\"regime_change_days\":{},\"protest_active\":{},\"mutiny_active\":{},\"capital_flight_active\":{},\"national_focus\":{},\"focus_transition_days\":{},\"focus_active\":{}}}",
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
                string(f.national_focus.key()),
                f.focus_transition_days,
                focus::active(f).is_some(),
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
#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::scenario;
    use archipelago_sim::world::DominationShare;

    /// External review fix (P2): `serialize_outcome`'s legacy
    /// `{"faction":...}` shape used to fire for *any* single-winner
    /// `Outcome::Victory`, keyed on `winners.len() == 1` alone - so a
    /// `Coalition`/`Domination` win that happened to leave one faction
    /// standing silently lost its `"condition"` field, and a consumer had
    /// no way to tell which declared rule actually fired. These tests pin
    /// the fix directly against `serialize_outcome` with a hand-built
    /// `Outcome`, rather than depending on some scenario/agent pairing
    /// actually reaching a single-winner Coalition or Domination outcome
    /// over a real run (both are reachable only through
    /// `scenarios/japan47.json`, and not reliably at exactly one winner).
    ///
    /// Confirmed each can fail: reverted `serialize_outcome`'s Conquest-only
    /// match arm back to the pre-fix `if let [only] = winners.as_slice()`
    /// guard (keyed on winner count, not condition) and re-ran - both tests
    /// below failed, since that guard swallowed the `"condition"` field for
    /// these non-Conquest single-winner outcomes too. Reverted before
    /// committing.
    #[test]
    fn single_winner_domination_victory_reports_its_condition() {
        let world = scenario::build_world();
        let share = DominationShare::new(0.6).expect("0.6 is a valid domination share");
        let outcome = Outcome::Victory { condition: VictoryCondition::Domination(share), winners: vec![FactionId(0)] };

        let json = serialize_outcome(&world, &outcome);

        assert!(
            json.contains("\"condition\":\"domination\""),
            "a single-winner Domination victory must still report its condition, got: {json}"
        );
        assert!(
            json.contains("\"winners\":[{\"faction\":0,\"faction_name\":"),
            "a single-winner Domination victory must use the winners-array shape, got: {json}"
        );
        assert!(
            !json.contains("\"type\":\"victory\",\"faction\":"),
            "a single-winner Domination victory must not take the legacy Conquest shape, got: {json}"
        );
    }

    #[test]
    fn single_winner_coalition_victory_reports_its_condition() {
        let world = scenario::build_world();
        let outcome = Outcome::Victory { condition: VictoryCondition::Coalition, winners: vec![FactionId(0)] };

        let json = serialize_outcome(&world, &outcome);

        assert!(
            json.contains("\"condition\":\"coalition\""),
            "a single-winner Coalition victory must still report its condition, got: {json}"
        );
        assert!(
            json.contains("\"winners\":[{\"faction\":0,\"faction_name\":"),
            "a single-winner Coalition victory must use the winners-array shape, got: {json}"
        );
        assert!(
            !json.contains("\"type\":\"victory\",\"faction\":"),
            "a single-winner Coalition victory must not take the legacy Conquest shape, got: {json}"
        );
    }

    /// Sanity companion: single-winner `Conquest` (the only shape
    /// `scenarios/mvp.json` can ever produce) must still take the legacy
    /// shape - the fix narrows the branch to `Conquest`, it doesn't remove
    /// it, and this is what keeps mvp's `--json` hash unchanged.
    #[test]
    fn single_winner_conquest_victory_keeps_the_legacy_shape() {
        let world = scenario::build_world();
        let outcome = Outcome::Victory { condition: VictoryCondition::Conquest, winners: vec![FactionId(0)] };

        let json = serialize_outcome(&world, &outcome);

        assert!(
            json.contains("\"type\":\"victory\",\"faction\":0,\"faction_name\":"),
            "a single-winner Conquest victory must keep the legacy `faction` shape, got: {json}"
        );
        assert!(!json.contains("\"condition\""), "the legacy Conquest shape must not gain a `condition` field, got: {json}");
    }
}

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
