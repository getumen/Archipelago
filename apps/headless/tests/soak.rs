//! Stage A of docs/soak-spec.md ("無人で長時間遊ばせ、成立してはいけない
//! ことが起きていないかを検査して、人が読める短い報告を出す"): runtime
//! invariant checking, `heuristic` mode only. `valid-fuzz`/`chaos-fuzz` are
//! a later stage (soak-spec.md §2) - this file only ever issues actions
//! `archipelago_agents::default_heuristic_agent` itself chooses.
//!
//! ## Why this exists, and why it isn't `scenario_acceptance.rs`
//!
//! `scenario_acceptance.rs` (this same crate's own `tests/` directory)
//! checks properties of a *finished* run (did a war happen, is anyone
//! permanently insolvent). Nothing in this repository checks whether the
//! world is internally consistent *mid-run*, tick by tick - the runtime
//! invariant checks that exist today are four `debug_assert!`s in
//! `observation.rs` (soak-spec.md §0). This file is that missing layer:
//! after every `Simulation::step()`, it asks a fixed list of "this must
//! never be true" questions - never "is the game balanced" or "did anyone
//! win" questions, which are `scenario_acceptance.rs`'s job.
//!
//! Every invariant below has a one-line reason it belongs on this list
//! (soak-spec.md §1: "理由の書けない検査は入れない") - most of them are
//! actual, already-documented constraints of the simulation (a type's own
//! `new()` bound, a value `docs/*-spec.md` or a doc comment states is
//! `0..1`, a field two other fields are documented to stay "in lockstep"
//! with) rather than something invented for this file.
//!
//! ## Why this lives in `apps/headless/tests/`, not `crates/sim`
//!
//! `crates/sim` stays dependency-free and this harness needs to drive real
//! `HeuristicAgent`s (`archipelago_agents`) against a real `Simulation`
//! (`archipelago_sim`) exactly the way `apps/headless` itself does - the
//! same reason `scenario_acceptance.rs` already lives here rather than in
//! `crates/sim/src/tests.rs`. Being a `tests/*.rs` integration test, it
//! never runs as part of an ordinary `cargo build`, and every multi-seed /
//! `japan_hex`-scale case below is `#[ignore]`d for the same runtime reason
//! `scenario_acceptance.rs`'s own module doc gives - see "Runtime" below.
//!
//! Loop-driving glue (`run_soak`'s per-day `decide`/`apply`/`step` body) is
//! reproduced from `apps/headless/src/main.rs` rather than imported, for
//! the same reason `scenario_acceptance.rs::run_trajectory` already gives:
//! `apps/headless` is a binary-only crate with no `lib.rs` to import from.
//!
//! ## Runtime / how to run the gated tests
//!
//! ```sh
//! cargo test -p archipelago-headless --test soak -- --ignored --nocapture
//! ```
//!
//! (add `--release` first for a couple of seconds instead of considerably
//! longer - `japan_hex` at 289 regions/8 factions/532 transport nodes,
//! checked at every one of 720 days across 3 seeds, is real work). The one
//! always-on test (`mvp`, seed 1) adds well under a second to ordinary
//! `cargo test --workspace`, matching `scenario_acceptance.rs`'s own bar
//! (soak-spec.md's own constraint: "通常の `cargo test` を重くしない").
//!
//! ## What this does NOT do (soak-spec.md §6)
//!
//! Not a balance judge (no assertion here reads `supply_ratio`'s *value* as
//! good or bad - only that it is a value a `0..1` ratio could actually be),
//! not an early-decision nudge (720-day stalemates are not flagged), and
//! not a substitute for a human looking at the rendered game.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use archipelago_agents::default_heuristic_agent;
use archipelago_sim::action::{self, Action};
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{
    CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN, FOCUS_MILITARY_ORG_CAP_MULT, IMPORT_PLAN_RATE_MAX, UNIT_EQUIPMENT,
    UNIT_MANPOWER, UNIT_ORG,
};
use archipelago_sim::event::Event;
use archipelago_sim::good::{ALL_GOODS, GOOD_COUNT};
use archipelago_sim::group::GROUP_COUNT;
use archipelago_sim::ids::FactionId;
use archipelago_sim::logistics;
use archipelago_sim::observation::Observation;
use archipelago_sim::research::RESEARCH_AXIS_COUNT;
use archipelago_sim::scenario;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::{Domain, World};

// ---------------------------------------------------------------------
// 1. Findings: a report is a deduplicated multiset, not a log.
//
// soak-spec.md §4: "同じ違反が 500 回出ても 1 行である。行数が報告の長さを
// 決めてはいけない" - so every violation is recorded under a `check_key`
// (which invariant fired), and only the *first* occurrence's detail string
// is kept; everything after that only increments a counter.
// ---------------------------------------------------------------------

#[derive(Default)]
struct Findings {
    by_check: BTreeMap<String, (String, u64)>,
}

impl Findings {
    fn record(&mut self, check_key: String, detail: String) {
        self.by_check
            .entry(check_key)
            .and_modify(|(_, count)| *count += 1)
            .or_insert((detail, 1));
    }

    fn merge(&mut self, other: Findings) {
        for (key, (detail, count)) in other.by_check {
            self.by_check.entry(key).and_modify(|(_, c)| *c += count).or_insert((detail, count));
        }
    }

    fn total(&self) -> u64 {
        self.by_check.values().map(|(_, c)| c).sum()
    }

    fn print(&self) {
        if self.by_check.is_empty() {
            println!("  (none)");
            return;
        }
        for (check, (detail, count)) in &self.by_check {
            println!("  [{check}] x{count} - first occurrence: {detail}");
        }
    }
}

/// Absolute float tolerance for every range check below - wide enough to
/// absorb ordinary float noise from a `clamp`/`min`/`max` chain, tight
/// enough that it would never mask a real defect (every bound checked here
/// is either a hard `clamp` target or a type's own `new()` bound, not a
/// value that drifts gradually).
const EPS: f32 = 1e-3;

/// `check_range` の上限を**含まない**版。`hi` ちょうどを違反として報告する。
///
/// `Region::occupation` のように「その値に達した瞬間に別の遷移が起きるので、
/// 保存された状態としては決して現れない」フィールド専用。EPS の緩衝も上側
/// には置かない——緩衝を置くと、まさに検出したい境界値が通り抜ける。
fn check_range_excl_hi(findings: &mut Findings, check_key: &str, val: f32, lo: f32, hi: f32, scenario: &str, seed: u64, day: u32, entity: &str) {
    if val.is_nan() || val.is_infinite() {
        findings.record(
            format!("nonfinite:{check_key}"),
            format!("{scenario} seed {seed} day {day}: {entity} {check_key} = {val} (not finite)"),
        );
        return;
    }
    if val < lo - EPS || val >= hi {
        findings.record(
            format!("out_of_range:{check_key}"),
            format!("{scenario} seed {seed} day {day}: {entity} {check_key} = {val}, expected [{lo}, {hi})"),
        );
    }
}

fn check_range(findings: &mut Findings, check_key: &str, val: f32, lo: f32, hi: f32, scenario: &str, seed: u64, day: u32, entity: &str) {
    if val.is_nan() || val.is_infinite() {
        findings.record(
            format!("nonfinite:{check_key}"),
            format!("{scenario} seed {seed} day {day}: {entity} {check_key} = {val} (not finite)"),
        );
        return;
    }
    if val < lo - EPS || val > hi + EPS {
        findings.record(
            format!("out_of_range:{check_key}"),
            format!("{scenario} seed {seed} day {day}: {entity} {check_key} = {val}, expected [{lo}, {hi}]"),
        );
    }
}

fn check_nonneg(findings: &mut Findings, check_key: &str, val: f32, scenario: &str, seed: u64, day: u32, entity: &str) {
    check_range(findings, check_key, val, 0.0, f32::INFINITY, scenario, seed, day, entity);
}

// ---------------------------------------------------------------------
// 2. The invariant list (soak-spec.md §1). Each `check_range`/`check_nonneg`
// call below, and each structural `if`, is one invariant - the comment
// immediately above it is the one-line reason it belongs here.
// ---------------------------------------------------------------------

fn check_world_invariants(scenario: &str, seed: u64, day: u32, world: &World, findings: &mut Findings) {
    let n_factions = world.factions.len();

    // --- Regions ---
    for (ridx, region) in world.regions.iter().enumerate() {
        let entity = format!("region[{ridx}]");
        // A physical stockpile/headcount/capacity/import volume can never
        // be negative - there is no such thing as owing the world stock.
        check_nonneg(findings, "region.population", region.population, scenario, seed, day, &entity);
        check_nonneg(findings, "region.infrastructure", region.infrastructure, scenario, seed, day, &entity);
        check_nonneg(findings, "region.port", region.port, scenario, seed, day, &entity);
        check_nonneg(findings, "region.mobilized", region.mobilized, scenario, seed, day, &entity);
        check_nonneg(findings, "region.import_flow", region.import_flow, scenario, seed, day, &entity);
        for good in ALL_GOODS {
            check_nonneg(findings, "region.capacity", region.capacity[good.index()], scenario, seed, day, &entity);
        }
        // `Region::unrest`'s own writer (`politics::tick_politics`) clamps
        // every write to `0..100` - it is read elsewhere as a percentage.
        check_range(findings, "region.unrest", region.unrest, 0.0, 100.0, scenario, seed, day, &entity);
        // `military::tick_occupation` flips ownership and resets this to 0
        // the instant it would reach 100 - a stored value at/above 100
        // means a region changed hands without that flip happening.
        //
        // **上限は含まない。** ここは当初 `check_range(.., 0.0, 100.0, ..)`
        // だったが、`check_range` は `val > hi + EPS` で弾くので**ちょうど
        // 100.0 を通していた**——上のコメントが「at/above 100」と書いている
        // 当の境界値である。`codex review` の指摘。**検出器が、自分の
        // コメントが述べている不変条件を実装していなかった。**
        // CLAUDE.md が「空のテスト」として 6 件記録している形が、検査する側
        // に出た例なので、直したうえでここに残す。
        check_range_excl_hi(findings, "region.occupation", region.occupation, 0.0, 100.0, scenario, seed, day, &entity);
        // `Region::devastation`'s own doc: "War damage, 0..1".
        check_range(findings, "region.devastation", region.devastation, 0.0, 1.0, scenario, seed, day, &entity);
        // `Region::air_superiority`'s own type (`AirSuperiority::new`) can
        // only ever hold `0.0..=1.0` - checked here too as a defense against
        // a future refactor that swaps the bounded type for a raw `f32`
        // without preserving the bound.
        for f in 0..n_factions {
            check_range(
                findings,
                "region.air_superiority",
                region.air_superiority[f].get(),
                0.0,
                1.0,
                scenario,
                seed,
                day,
                &entity,
            );
        }
        // A faction cannot occupy its own territory - "occupier" only means
        // anything as "someone other than the owner holding this against
        // them".
        if region.occupier == Some(region.owner) {
            findings.record(
                "region_occupies_itself".to_string(),
                format!("{scenario} seed {seed} day {day}: {entity} owner={:?} occupier==owner", region.owner),
            );
        }
        // `Region::occupation_kind`'s own doc: "Kept in lockstep with
        // occupier (Some iff occupier.is_some())" - letting them drift is
        // exactly the military-vs-separatist ownership bug this field was
        // introduced to stop (see its own doc's "External code review fix").
        if region.occupier.is_some() != region.occupation_kind.is_some() {
            findings.record(
                "occupier_occupation_kind_disagree".to_string(),
                format!(
                    "{scenario} seed {seed} day {day}: {entity} occupier={:?} occupation_kind={:?}",
                    region.occupier, region.occupation_kind
                ),
            );
        }
    }

    // --- Factions ---
    for (fidx, faction) in world.factions.iter().enumerate() {
        let entity = format!("faction[{fidx}]");
        check_nonneg(findings, "faction.manpower", faction.manpower, scenario, seed, day, &entity);
        for good in ALL_GOODS {
            // A stockpile going negative would let a faction spend matériel
            // it never had.
            check_nonneg(findings, "faction.stock", faction.stock[good.index()], scenario, seed, day, &entity);
            // `action::apply_set_import_plan` clamps to `0..=IMPORT_PLAN_RATE_MAX`.
            check_range(
                findings,
                "faction.import_plan",
                faction.import_plan[good.index()],
                0.0,
                IMPORT_PLAN_RATE_MAX,
                scenario,
                seed,
                day,
                &entity,
            );
            // `action::apply_set_logistics_priority` rejects anything
            // outside `0..=1`.
            check_range(
                findings,
                "faction.logistics_priority",
                faction.logistics_priority[good.index()],
                0.0,
                1.0,
                scenario,
                seed,
                day,
                &entity,
            );
            // `action::apply_set_industry_priority` rejects anything
            // outside `0..=1`.
            check_range(
                findings,
                "faction.industry_priority",
                faction.industry_priority[good.index()],
                0.0,
                1.0,
                scenario,
                seed,
                day,
                &entity,
            );
            // `economy::consume`'s own `.clamp(0.0, 1.0)`.
            check_range(
                findings,
                "faction.shortage_by_good",
                faction.shortage_by_good[good.index()],
                0.0,
                1.0,
                scenario,
                seed,
                day,
                &entity,
            );
        }
        // `action::apply_set_conscription` rejects anything outside `0..=1`.
        check_range(findings, "faction.conscription", faction.conscription, 0.0, 1.0, scenario, seed, day, &entity);
        // `action::apply_set_civilian_ration` rejects anything outside
        // `CIVILIAN_RATION_MIN..=CIVILIAN_RATION_MAX`.
        check_range(
            findings,
            "faction.civilian_ration",
            faction.civilian_ration,
            CIVILIAN_RATION_MIN,
            CIVILIAN_RATION_MAX,
            scenario,
            seed,
            day,
            &entity,
        );
        // `politics::tick_politics` clamps every write to `0..100` - the
        // whole political model reads these as percentages.
        check_range(findings, "faction.war_support", faction.war_support, 0.0, 100.0, scenario, seed, day, &entity);
        check_range(findings, "faction.stability", faction.stability, 0.0, 100.0, scenario, seed, day, &entity);
        // `faction.shortage` is a `max()` of three already-clamped `0..1`
        // values (`economy::tick_economy`).
        check_range(findings, "faction.shortage", faction.shortage, 0.0, 1.0, scenario, seed, day, &entity);
        // `logistics::distribute_supply` computes this as `served/demand`
        // then `.min(1.0)`, with both operands non-negative.
        check_range(findings, "faction.supply_ratio", faction.supply_ratio, 0.0, 1.0, scenario, seed, day, &entity);
        // `economy::tick_economy`'s own doc: "Today's Machinery output over
        // its input-unconstrained potential, 0..1".
        check_range(
            findings,
            "faction.machinery_output_ratio",
            faction.machinery_output_ratio,
            0.0,
            1.0,
            scenario,
            seed,
            day,
            &entity,
        );
        for g in 0..GROUP_COUNT {
            // Stage 3A: `politics::tick_politics` clamps every write to
            // `0..100`.
            check_range(findings, "faction.group_support", faction.group_support[g], 0.0, 100.0, scenario, seed, day, &entity);
        }
        for axis in 0..RESEARCH_AXIS_COUNT {
            // `research::ResearchWeight::new` can only ever hold
            // `0.0..=1.0` - defense against a future refactor unwrapping it.
            check_range(
                findings,
                "faction.research_allocation",
                faction.research_allocation[axis].get(),
                0.0,
                1.0,
                scenario,
                seed,
                day,
                &entity,
            );
        }
    }

    // --- Units ---
    for (uidx, unit) in world.units.iter().enumerate() {
        if !unit.alive {
            continue;
        }
        let entity = format!("unit[{uidx}] owner={:?}", unit.owner);
        // `action::apply_recruit`/`apply_reinforce` only ever fill a unit up
        // to its own gap against `UNIT_MANPOWER`/`UNIT_EQUIPMENT` - leaving
        // this range means a fill computation broke its own gap accounting.
        check_range(findings, "unit.manpower", unit.manpower, 0.0, UNIT_MANPOWER, scenario, seed, day, &entity);
        check_range(findings, "unit.equipment", unit.equipment, 0.0, UNIT_EQUIPMENT, scenario, seed, day, &entity);
        // `military::tick_recovery` clamps every write to
        // `0..UNIT_ORG*FOCUS_MILITARY_ORG_CAP_MULT` (the focus-boosted
        // ceiling, the widest this can legally get).
        check_range(
            findings,
            "unit.organization",
            unit.organization,
            0.0,
            UNIT_ORG * FOCUS_MILITARY_ORG_CAP_MULT,
            scenario,
            seed,
            day,
            &entity,
        );
        // `military::tick_recovery` clamps every write to `0..1` -
        // `combat_power`'s `(0.50 + 0.50 * morale)` term assumes a fraction.
        check_range(findings, "unit.morale", unit.morale, 0.0, 1.0, scenario, seed, day, &entity);
        // `logistics::distribute_supply` clamps every write to `0..1` -
        // `combat_power`'s `(0.35 + 0.65 * supply)` term assumes a fraction.
        check_range(findings, "unit.supply", unit.supply, 0.0, 1.0, scenario, seed, day, &entity);
        // Documented as "the fraction of this unit's equipment gap the
        // supply network can currently deliver" - a fraction.
        check_range(findings, "unit.arms_delivery", unit.arms_delivery, 0.0, 1.0, scenario, seed, day, &entity);
        // `arms_budget = equipment_gap * arms_delivery`, and
        // `equipment_gap <= UNIT_EQUIPMENT` by construction.
        check_range(findings, "unit.arms_budget", unit.arms_budget, 0.0, UNIT_EQUIPMENT, scenario, seed, day, &entity);
        // `military::tick_combat`'s own `.min(1.0)` - `combat_power`'s
        // `(1.00 + 0.35 * experience)` term assumes a fraction.
        check_range(findings, "unit.experience", unit.experience, 0.0, 1.0, scenario, seed, day, &entity);
        // `Unit::branch`'s own doc: "Some(branch) for every land unit
        // (station.domain() == Domain::Land)... None for every fleet and
        // air unit."
        let is_land = unit.station.domain() == Domain::Land;
        if unit.branch.is_some() != is_land {
            findings.record(
                "unit_branch_domain_disagree".to_string(),
                format!(
                    "{scenario} seed {seed} day {day}: {entity} station={:?} branch={:?}",
                    unit.station, unit.branch
                ),
            );
        }
        // `Simulation::step_timed` kills every unit belonging to a faction
        // the instant it is eliminated (`Event::FactionEliminated`) - an
        // alive unit under a dead faction means that bookkeeping was
        // skipped.
        if !world.factions[unit.owner.index()].alive {
            findings.record(
                "alive_unit_of_dead_faction".to_string(),
                format!("{scenario} seed {seed} day {day}: {entity} is alive but its faction is not"),
            );
        }
    }

    // --- Sea zones ---
    for (zidx, zone) in world.sea_zones.iter().enumerate() {
        let entity = format!("sea_zone[{zidx}]");
        for f in 0..n_factions {
            // `SeaZone::control`'s own doc: "power[f] / sum(power[*])" - a
            // share, `0..1`.
            check_range(findings, "sea_zone.control", zone.control[f], 0.0, 1.0, scenario, seed, day, &entity);
        }
    }

    // --- Transport network ---
    for (nidx, node) in world.transport_nodes.iter().enumerate() {
        let entity = format!("transport_node[{nidx}]");
        // `transport::Condition::new` can only ever hold `0.0..=1.0`.
        check_range(findings, "transport_node.condition", node.condition.get(), 0.0, 1.0, scenario, seed, day, &entity);
    }
    let line_flows = logistics::transport_line_flows(world);
    for (lidx, line) in world.transport_lines.iter().enumerate() {
        let entity = format!("transport_line[{lidx}]");
        check_range(findings, "transport_line.condition", line.condition.get(), 0.0, 1.0, scenario, seed, day, &entity);
        let cap = line.effective_capacity(world);
        let flow = line_flows[lidx];
        // Phase 9's whole reason for existing (docs/phase9-spec.md "2. 補給
        // を有限流量にする"): a route's combined forward+backward flow this
        // tick must never exceed `capacity * condition` (further scaled by
        // endpoint devastation) - an unlimited-flow regression here is
        // exactly the defect class Stage 9B was built to close.
        let tol = cap.abs() * 1e-4 + 1e-2;
        if flow.is_finite() && flow > cap + tol {
            findings.record(
                "transport_line_flow_exceeds_capacity".to_string(),
                format!("{scenario} seed {seed} day {day}: {entity} flow={flow} > effective_capacity={cap}"),
            );
        } else if !flow.is_finite() {
            findings.record(
                "transport_line_flow_nonfinite".to_string(),
                format!("{scenario} seed {seed} day {day}: {entity} flow={flow}"),
            );
        }
    }

    // --- Supply arrays (World-level, per (region|zone|node, faction)) ---
    for r in 0..world.regions.len() {
        check_nonneg(findings, "world.supply", world.supply[r], scenario, seed, day, &format!("region[{r}]"));
        for f in 0..n_factions {
            check_nonneg(
                findings,
                "world.supply_by_faction",
                world.supply_by_faction[r][f],
                scenario,
                seed,
                day,
                &format!("region[{r}] faction[{f}]"),
            );
        }
    }
    for z in 0..world.sea_zones.len() {
        for f in 0..n_factions {
            check_nonneg(
                findings,
                "world.supply_sea",
                world.supply_sea[z][f],
                scenario,
                seed,
                day,
                &format!("sea_zone[{z}] faction[{f}]"),
            );
        }
    }
    for node in 0..world.transport_nodes.len() {
        for f in 0..n_factions {
            check_nonneg(
                findings,
                "world.supply_air",
                world.supply_air[node][f],
                scenario,
                seed,
                day,
                &format!("transport_node[{node}] faction[{f}]"),
            );
        }
    }
}

// ---------------------------------------------------------------------
// 3. Action-application atomicity: a rejected `Action` must not mutate the
// world (docs/conventions.md's fail-fast `ActionError` contract - an
// `ActionError` is documented API behaviour, not a fallback, but it is
// only meaningful if a rejected action is genuinely a no-op).
//
// Checked with a cheap fingerprint rather than a full `World` clone/Debug
// dump per action (perf: this runs once per action, of which there can be
// thousands per run) - `Diplomacy` (small: O(factions^2)) is fingerprinted
// via its own `#[derive(Debug)]` (exhaustive, including its private
// cooldown bookkeeping); every other field an `Action` can plausibly touch
// (stock, manpower, unit state, transport/node condition, construction,
// national focus, research allocation) is folded in by hand as bit-exact
// `f32::to_bits()`/discriminant hashing - see `fingerprint`'s own body for
// exactly which fields.
// ---------------------------------------------------------------------

fn fingerprint(world: &World) -> u64 {
    let mut h = DefaultHasher::new();

    for region in &world.regions {
        h.write_u32(region.owner.0);
        h.write_u32(region.core.0);
        for good in ALL_GOODS {
            h.write_u32(region.capacity[good.index()].to_bits());
        }
        h.write_u32(region.infrastructure.to_bits());
        h.write_u32(region.port.to_bits());
        h.write_u32(region.mobilized.to_bits());
        h.write_u32(region.unrest.to_bits());
        h.write_u32(region.occupation.to_bits());
        h.write_u32(region.occupier.map(|f| f.0).unwrap_or(u32::MAX));
        h.write(format!("{:?}", region.occupation_kind).as_bytes());
        h.write_u32(region.devastation.to_bits());
        h.write(format!("{:?}", region.construction).as_bytes());
        h.write_u32(region.import_flow.to_bits());
    }

    for faction in &world.factions {
        h.write_u32(faction.manpower.to_bits());
        for good in ALL_GOODS {
            h.write_u32(faction.stock[good.index()].to_bits());
            h.write_u32(faction.industry_priority[good.index()].to_bits());
            h.write_u32(faction.import_plan[good.index()].to_bits());
            h.write_u32(faction.logistics_priority[good.index()].to_bits());
        }
        h.write_u32(faction.conscription.to_bits());
        h.write_u32(faction.civilian_ration.to_bits());
        h.write_u32(faction.war_support.to_bits());
        h.write_u32(faction.stability.to_bits());
        for g in 0..GROUP_COUNT {
            h.write_u32(faction.group_support[g].to_bits());
        }
        h.write_u32(faction.strike_days);
        h.write_u32(faction.regime_change_days);
        h.write_u8(faction.protest_active as u8);
        h.write_u8(faction.mutiny_active as u8);
        h.write_u8(faction.capital_flight_active as u8);
        h.write_u32(faction.national_focus.index() as u32);
        h.write_u32(faction.focus_transition_days);
        for axis in 0..RESEARCH_AXIS_COUNT {
            h.write_u32(faction.research_allocation[axis].get().to_bits());
            h.write_u32(faction.research_progress[axis].to_bits());
        }
        h.write_u8(faction.alive as u8);
    }

    for unit in &world.units {
        h.write_u32(unit.owner.0);
        h.write(format!("{:?}", unit.station).as_bytes());
        h.write(format!("{:?}", unit.movement).as_bytes());
        h.write_u32(unit.manpower.to_bits());
        h.write_u32(unit.equipment.to_bits());
        h.write_u32(unit.organization.to_bits());
        h.write_u32(unit.morale.to_bits());
        h.write_u32(unit.supply.to_bits());
        h.write_u32(unit.arms_delivery.to_bits());
        h.write_u32(unit.arms_budget.to_bits());
        h.write(format!("{:?}", unit.arms_delivery_station).as_bytes());
        h.write_u32(unit.experience.to_bits());
        h.write_u8(unit.alive as u8);
        h.write(format!("{:?}", unit.branch).as_bytes());
    }

    for node in &world.transport_nodes {
        h.write_u32(node.condition.get().to_bits());
    }
    for line in &world.transport_lines {
        h.write_u32(line.condition.get().to_bits());
    }

    // Exhaustive by construction (derives `Debug` over every field,
    // including the private cooldown/log bookkeeping `pub` accessors don't
    // expose) rather than hand-enumerated like the rest of this function -
    // `Diplomacy` is small (O(factions^2)), so formatting it is cheap.
    h.write(format!("{:?}", world.diplomacy).as_bytes());

    h.finish()
}

/// Applies every action a faction issued this day one at a time (mirroring
/// `Simulation::apply`'s own loop exactly - see `action::apply_action`'s
/// doc), fingerprinting the world immediately before and after each one and
/// recording a finding if a *rejected* action's fingerprint moved anyway.
fn apply_and_check_atomicity(world: &mut World, faction: FactionId, actions: Vec<Action>, scenario: &str, seed: u64, day: u32, findings: &mut Findings, actions_seen: &mut BTreeSet<&'static str>) {
    for act in actions {
        actions_seen.insert(action_tag(&act));
        let act_debug = format!("{act:?}");
        let before = fingerprint(world);
        let result = action::apply_action(world, faction, act);
        if let Err(err) = result {
            let after = fingerprint(world);
            if before != after {
                findings.record(
                    "rejected_action_mutated_world".to_string(),
                    format!(
                        "{scenario} seed {seed} day {day}: faction {faction:?} action {act_debug} rejected as \
                         {err:?} but the world's fingerprint changed ({before} -> {after})"
                    ),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------
// 4. Anomaly signals (soak-spec.md §1 "異常の兆候") - reported, never
// failing. Never turned into an `assert!` anywhere in this file.
// ---------------------------------------------------------------------

/// Every `Action` variant, by name - the "did the AI ever even try this"
/// universe for the `actions_seen` anomaly below. Intentionally an
/// exhaustive `match` with no wildcard arm, so adding an `Action` variant
/// without updating this list is a compile error, not a silent gap - the
/// same shape of mistake CLAUDE.md records happening twice already
/// ("行動を足したのに fuzz が知らない", Phase 11 and Stage 12C).
fn action_tag(action: &Action) -> &'static str {
    match action {
        Action::MoveUnit { .. } => "MoveUnit",
        Action::HoldUnit { .. } => "HoldUnit",
        Action::DisbandUnit { .. } => "DisbandUnit",
        Action::RecruitUnit { .. } => "RecruitUnit",
        Action::ReinforceUnit { .. } => "ReinforceUnit",
        Action::SetConscription(_) => "SetConscription",
        Action::SetIndustryPriority { .. } => "SetIndustryPriority",
        Action::SetCivilianRation(_) => "SetCivilianRation",
        Action::Build { .. } => "Build",
        Action::CancelBuild { .. } => "CancelBuild",
        Action::SetImportPlan { .. } => "SetImportPlan",
        Action::SetLogisticsPriority { .. } => "SetLogisticsPriority",
        Action::SetResearchAllocation { .. } => "SetResearchAllocation",
        Action::ProposeTreaty { .. } => "ProposeTreaty",
        Action::AcceptTreaty { .. } => "AcceptTreaty",
        Action::RejectTreaty { .. } => "RejectTreaty",
        Action::DeclareWar { .. } => "DeclareWar",
        Action::BreakTreaty { .. } => "BreakTreaty",
        Action::SetNationalFocus(_) => "SetNationalFocus",
        Action::ProposeInNaturalLanguage { .. } => "ProposeInNaturalLanguage",
        Action::RespondToNaturalLanguageProposal { .. } => "RespondToNaturalLanguageProposal",
        Action::InterdictLine { .. } => "InterdictLine",
        Action::StrikeNode { .. } => "StrikeNode",
    }
}

const ALL_ACTION_TAGS: &[&str] = &[
    "MoveUnit",
    "HoldUnit",
    "DisbandUnit",
    "RecruitUnit",
    "ReinforceUnit",
    "SetConscription",
    "SetIndustryPriority",
    "SetCivilianRation",
    "Build",
    "CancelBuild",
    "SetImportPlan",
    "SetLogisticsPriority",
    "SetResearchAllocation",
    "ProposeTreaty",
    "AcceptTreaty",
    "RejectTreaty",
    "DeclareWar",
    "BreakTreaty",
    "SetNationalFocus",
    "ProposeInNaturalLanguage",
    "RespondToNaturalLanguageProposal",
    "InterdictLine",
    "StrikeNode",
];

/// Every `Event` variant, by name - same exhaustive-match-no-wildcard shape
/// as `action_tag`, for the "which event types never fired" anomaly.
fn event_tag(event: &Event) -> &'static str {
    match event {
        Event::Battle { .. } => "Battle",
        Event::NavalBattle { .. } => "NavalBattle",
        Event::UnitDestroyed { .. } => "UnitDestroyed",
        Event::RegionCaptured { .. } => "RegionCaptured",
        Event::FactionEliminated { .. } => "FactionEliminated",
        Event::Strike { .. } => "Strike",
        Event::Protest { .. } => "Protest",
        Event::Mutiny { .. } => "Mutiny",
        Event::CapitalFlight { .. } => "CapitalFlight",
        Event::RegimeChange { .. } => "RegimeChange",
        Event::Separatism { .. } => "Separatism",
        Event::TreatyProposed { .. } => "TreatyProposed",
        Event::TreatySigned { .. } => "TreatySigned",
        Event::TreatyRejected { .. } => "TreatyRejected",
        Event::TreatyBroken { .. } => "TreatyBroken",
        Event::WarDeclared { .. } => "WarDeclared",
        Event::AllianceDragIn { .. } => "AllianceDragIn",
        Event::NaturalLanguageProposed { .. } => "NaturalLanguageProposed",
        Event::NaturalLanguageAccepted { .. } => "NaturalLanguageAccepted",
        Event::NaturalLanguageRejected { .. } => "NaturalLanguageRejected",
        Event::NaturalLanguageTermsInvalid { .. } => "NaturalLanguageTermsInvalid",
        Event::NodeStruck { .. } => "NodeStruck",
        Event::LineInterdicted { .. } => "LineInterdicted",
    }
}

const ALL_EVENT_TAGS: &[&str] = &[
    "Battle",
    "NavalBattle",
    "UnitDestroyed",
    "RegionCaptured",
    "FactionEliminated",
    "Strike",
    "Protest",
    "Mutiny",
    "CapitalFlight",
    "RegimeChange",
    "Separatism",
    "TreatyProposed",
    "TreatySigned",
    "TreatyRejected",
    "TreatyBroken",
    "WarDeclared",
    "AllianceDragIn",
    "NaturalLanguageProposed",
    "NaturalLanguageAccepted",
    "NaturalLanguageRejected",
    "NaturalLanguageTermsInvalid",
    "NodeStruck",
    "LineInterdicted",
];

/// `(unit counts, region counts)` per faction - the same frozen-world
/// signature `scenario_acceptance.rs::unit_and_region_signature` already
/// uses, reproduced here for the same "no shared lib to import from"
/// reason the rest of this file's glue is reproduced.
fn unit_and_region_signature(world: &World) -> (Vec<usize>, Vec<usize>) {
    let n = world.factions.len();
    let units: Vec<usize> = (0..n).map(|i| world.units.iter().filter(|u| u.alive && u.owner == FactionId(i as u32)).count()).collect();
    let regions: Vec<usize> = (0..n).map(|i| world.region_count(FactionId(i as u32))).collect();
    (units, regions)
}

// ---------------------------------------------------------------------
// 5. Driving one run.
// ---------------------------------------------------------------------

struct SoakOutcome {
    findings: Findings,
    actions_seen: BTreeSet<&'static str>,
    events_seen: BTreeSet<&'static str>,
    /// Whether any faction's stock of each good was ever above zero, at any
    /// point in the run - `good.index()`-indexed.
    good_ever_nonzero: [bool; GOOD_COUNT],
    max_frozen_streak_days: u32,
    final_day: u32,
    ticks_checked: u32,
}

/// Runs `world` to completion (or `days`, whichever comes first) with a
/// default `HeuristicAgent` per faction - `apps/headless/src/main.rs`'s own
/// per-day loop, reproduced (not imported - see this file's module doc),
/// with every action applied one at a time through `apply_and_check_atomicity`
/// instead of the batched `Simulation::apply`, and `check_world_invariants`
/// run once up front and again after every tick.
fn run_soak(scenario_label: &str, world: World, seed: u64, days: u32) -> SoakOutcome {
    let n = world.factions.len();
    let mut sim = Simulation::with_world(world, seed);
    let mut agents: Vec<Box<dyn Agent>> = (0..n).map(|i| Box::new(default_heuristic_agent(i)) as Box<dyn Agent>).collect();

    let mut findings = Findings::default();
    let mut actions_seen = BTreeSet::new();
    let mut events_seen = BTreeSet::new();
    let mut good_ever_nonzero = [false; GOOD_COUNT];
    let mut frozen_streak = 0u32;
    let mut max_frozen_streak_days = 0u32;
    let mut ticks_checked = 0u32;

    check_world_invariants(scenario_label, seed, sim.world.day, &sim.world, &mut findings);
    ticks_checked += 1;
    // codex review P2: the pre-loop stock/signature sample below must
    // happen here too, not only after the first `sim.step()` - otherwise a
    // good that starts with positive stock and is exhausted during day 1
    // (or a run whose unit/region counts never move at all) reads as
    // "chronically zero"/"frozen from day one" purely because nothing ever
    // sampled the *initial* state to compare against.
    for faction in &sim.world.factions {
        for good in ALL_GOODS {
            if faction.stock[good.index()] > 0.0 {
                good_ever_nonzero[good.index()] = true;
            }
        }
    }
    let mut last_signature: Option<(Vec<usize>, Vec<usize>)> = Some(unit_and_region_signature(&sim.world));

    let outcome = loop {
        let outcome = sim.outcome(days);
        if outcome != Outcome::Ongoing {
            break outcome;
        }
        let day = sim.world.day;
        for f_idx in 0..sim.world.factions.len() {
            let faction = FactionId(f_idx as u32);
            if !sim.world.factions[f_idx].alive {
                continue;
            }
            let obs = Observation { faction, world: &sim.world };
            let actions = agents[f_idx].decide(&obs);
            apply_and_check_atomicity(&mut sim.world, faction, actions, scenario_label, seed, day, &mut findings, &mut actions_seen);
        }
        for event in sim.step() {
            events_seen.insert(event_tag(&event));
        }

        check_world_invariants(scenario_label, seed, sim.world.day, &sim.world, &mut findings);
        ticks_checked += 1;

        for faction in &sim.world.factions {
            for good in ALL_GOODS {
                if faction.stock[good.index()] > 0.0 {
                    good_ever_nonzero[good.index()] = true;
                }
            }
        }

        // codex review P2: `frozen_streak` counts consecutive days that
        // equal the day *immediately before* them, so a changed signature
        // resets it to `0` (this day itself did not freeze), not `1` (which
        // both counted the changed day as a one-day "streak" of its own and
        // overstated every genuine streak that followed by one day).
        let signature = unit_and_region_signature(&sim.world);
        if last_signature.as_ref() == Some(&signature) {
            frozen_streak += 1;
        } else {
            frozen_streak = 0;
        }
        max_frozen_streak_days = max_frozen_streak_days.max(frozen_streak);
        last_signature = Some(signature);
    };
    let _ = outcome; // outcome itself is not this file's business (soak-spec.md §6)

    SoakOutcome { findings, actions_seen, events_seen, good_ever_nonzero, max_frozen_streak_days, final_day: sim.world.day, ticks_checked }
}

// ---------------------------------------------------------------------
// 6. The report (soak-spec.md §4).
// ---------------------------------------------------------------------

fn print_report(label: &str, runs: &[(&str, u64, SoakOutcome)]) {
    println!("\n=== soak report: {label} ===");
    let mut total_findings = Findings::default();
    let mut total_ticks = 0u32;
    for (scenario, seed, outcome) in runs {
        println!(
            "- {scenario} seed {seed}: {} ticks checked, ran to day {} ({} invariant violations)",
            outcome.ticks_checked,
            outcome.final_day,
            outcome.findings.total()
        );
        total_ticks += outcome.ticks_checked;
        total_findings.merge(Findings { by_check: outcome.findings.by_check.clone() });
    }
    println!("\n不変条件違反 (invariant violations), deduplicated by check, {total_ticks} ticks checked total:");
    total_findings.print();

    let mut actions_seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut events_seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut good_ever_nonzero = [false; GOOD_COUNT];
    let mut max_frozen = 0u32;
    for (_, _, outcome) in runs {
        actions_seen.extend(outcome.actions_seen.iter().copied());
        events_seen.extend(outcome.events_seen.iter().copied());
        for good in ALL_GOODS {
            good_ever_nonzero[good.index()] |= outcome.good_ever_nonzero[good.index()];
        }
        max_frozen = max_frozen.max(outcome.max_frozen_streak_days);
    }

    println!("\n異常の兆候 (anomalies - candidates for improvement, not failures):");
    let never_issued: Vec<&str> = ALL_ACTION_TAGS.iter().copied().filter(|a| !actions_seen.contains(a)).collect();
    if never_issued.is_empty() {
        println!("  - every Action variant was issued at least once across this run");
    } else {
        println!("  - Action variants no faction ever issued: {never_issued:?}");
    }
    let never_fired: Vec<&str> = ALL_EVENT_TAGS.iter().copied().filter(|e| !events_seen.contains(e)).collect();
    if never_fired.is_empty() {
        println!("  - every Event variant fired at least once across this run");
    } else {
        println!("  - Event variants that never fired: {never_fired:?}");
    }
    let chronic_zero: Vec<&str> = ALL_GOODS.iter().filter(|g| !good_ever_nonzero[g.index()]).map(|g| g.key()).collect();
    if chronic_zero.is_empty() {
        println!("  - every Good was held in positive stock by some faction at some point");
    } else {
        println!("  - Goods held at zero stock by every faction for the entire run: {chronic_zero:?}");
    }
    println!("  - longest run of consecutive days with every faction's unit/region counts frozen: {max_frozen}");

    println!("=== end soak report: {label} ===\n");
}

// ---------------------------------------------------------------------
// 7. Tests.
// ---------------------------------------------------------------------

/// Always-on (soak-spec.md §5's "通常の cargo test を重くしない"): `mvp` is
/// small (10 regions, 3 factions) so a full 720-day check-every-tick run
/// stays fast, mirroring `scenario_acceptance.rs`'s own always-on `mvp`
/// tests. Per soak-spec.md §5 ("既定の対戦（heuristic）で不変条件違反が
/// ゼロである"): any failure here is a live defect, not a fuzzer artifact -
/// this run issues only actions `HeuristicAgent` itself chooses.
#[test]
fn mvp_heuristic_zero_invariant_violations() {
    let outcome = run_soak("mvp", scenario::build_world(), 1, 720);
    if outcome.findings.total() > 0 {
        outcome.findings.print();
    }
    assert_eq!(outcome.findings.total(), 0, "mvp seed 1: runtime invariant violations found - see printed detail above");
}

/// `japan47`-scale, multiple seeds - gated behind `#[ignore]` for runtime
/// only, same as `scenario_acceptance.rs`'s own `japan_hex` tests.
#[test]
#[ignore]
fn japan47_heuristic_zero_invariant_violations() {
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        let outcome = run_soak("japan47", world, seed, 720);
        if outcome.findings.total() > 0 {
            outcome.findings.print();
        }
        assert_eq!(outcome.findings.total(), 0, "japan47 seed {seed}: runtime invariant violations found - see printed detail above");
    }
}

/// `japan_hex`-scale (289 regions, 8 factions, 532 transport nodes) -
/// gated behind `#[ignore]` for runtime.
#[test]
#[ignore]
fn japan_hex_heuristic_zero_invariant_violations() {
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        let outcome = run_soak("japan_hex", world, seed, 720);
        if outcome.findings.total() > 0 {
            outcome.findings.print();
        }
        assert_eq!(outcome.findings.total(), 0, "japan_hex seed {seed}: runtime invariant violations found - see printed detail above");
    }
}

/// The actual deliverable (soak-spec.md §4): one human-readable report
/// covering all three shipped scenarios, several seeds each, `heuristic`
/// mode. Run with `--nocapture` to see it. Also asserts zero invariant
/// violations (so a regression here still fails CI loudly) - the anomaly
/// section never does (soak-spec.md §1: anomalies are reported, not
/// failed).
#[test]
#[ignore]
fn heuristic_soak_report() {
    let mut runs: Vec<(&str, u64, SoakOutcome)> = Vec::new();

    runs.push(("mvp", 1, run_soak("mvp", scenario::build_world(), 1, 720)));

    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        runs.push(("japan47", seed, run_soak("japan47", world, seed, 720)));
    }
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        runs.push(("japan_hex", seed, run_soak("japan_hex", world, seed, 720)));
    }

    print_report("heuristic, 7 runs (mvp x1, japan47 x3, japan_hex x3), 720 days each", &runs);

    let total: u64 = runs.iter().map(|(_, _, o)| o.findings.total()).sum();
    assert_eq!(total, 0, "heuristic soak report found runtime invariant violations - see the printed report above");
}
