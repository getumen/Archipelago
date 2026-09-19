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
use archipelago_api::action_codec;
use archipelago_sim::action::{self, Action};
use archipelago_sim::agent::Agent;
use archipelago_sim::air;
use archipelago_sim::balance::{
    CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN, FOCUS_MILITARY_ORG_CAP_MULT, IMPORT_PLAN_RATE_MAX, UNIT_EQUIPMENT,
    UNIT_MANPOWER, UNIT_ORG,
};
use archipelago_sim::diplomacy::{Stance, Treaty, ALL_TREATIES};
use archipelago_sim::event::Event;
use archipelago_sim::good::{ALL_GOODS, GOOD_COUNT};
use archipelago_sim::group::GROUP_COUNT;
use archipelago_sim::ids::{FactionId, RegionId};
use archipelago_sim::json::Value;
use archipelago_sim::logistics;
use archipelago_sim::military::ALL_BRANCHES;
use archipelago_sim::observation::Observation;
use archipelago_sim::research::RESEARCH_AXIS_COUNT;
use archipelago_sim::rng::Rng;
use archipelago_sim::scenario;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::transport::TransportNodeKind;
use archipelago_sim::world::{Domain, Station, World};

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
fn check_range_excl_hi(findings: &mut Findings, check_key: &str, val: f32, lo: f32, hi: f32, scenario: &str, seed: u64, day: u32, mode: &str, entity: &str) {
    if val.is_nan() || val.is_infinite() {
        findings.record(
            format!("nonfinite:{check_key}"),
            format!("{scenario} seed {seed} day {day} mode {mode}: {entity} {check_key} = {val} (not finite)"),
        );
        return;
    }
    if val < lo - EPS || val >= hi {
        findings.record(
            format!("out_of_range:{check_key}"),
            format!("{scenario} seed {seed} day {day} mode {mode}: {entity} {check_key} = {val}, expected [{lo}, {hi})"),
        );
    }
}

fn check_range(findings: &mut Findings, check_key: &str, val: f32, lo: f32, hi: f32, scenario: &str, seed: u64, day: u32, mode: &str, entity: &str) {
    if val.is_nan() || val.is_infinite() {
        findings.record(
            format!("nonfinite:{check_key}"),
            format!("{scenario} seed {seed} day {day} mode {mode}: {entity} {check_key} = {val} (not finite)"),
        );
        return;
    }
    if val < lo - EPS || val > hi + EPS {
        findings.record(
            format!("out_of_range:{check_key}"),
            format!("{scenario} seed {seed} day {day} mode {mode}: {entity} {check_key} = {val}, expected [{lo}, {hi}]"),
        );
    }
}

fn check_nonneg(findings: &mut Findings, check_key: &str, val: f32, scenario: &str, seed: u64, day: u32, mode: &str, entity: &str) {
    check_range(findings, check_key, val, 0.0, f32::INFINITY, scenario, seed, day, mode, entity);
}

// ---------------------------------------------------------------------
// 2. The invariant list (soak-spec.md §1). Each `check_range`/`check_nonneg`
// call below, and each structural `if`, is one invariant - the comment
// immediately above it is the one-line reason it belongs here.
// ---------------------------------------------------------------------

fn check_world_invariants(scenario: &str, seed: u64, day: u32, mode: &str, world: &World, findings: &mut Findings) {
    let n_factions = world.factions.len();

    // --- Regions ---
    for (ridx, region) in world.regions.iter().enumerate() {
        let entity = format!("region[{ridx}]");
        // A physical stockpile/headcount/capacity/import volume can never
        // be negative - there is no such thing as owing the world stock.
        check_nonneg(findings, "region.population", region.population, scenario, seed, day, mode, &entity);
        check_nonneg(findings, "region.infrastructure", region.infrastructure, scenario, seed, day, mode, &entity);
        check_nonneg(findings, "region.port", region.port, scenario, seed, day, mode, &entity);
        check_nonneg(findings, "region.mobilized", region.mobilized, scenario, seed, day, mode, &entity);
        check_nonneg(findings, "region.import_flow", region.import_flow, scenario, seed, day, mode, &entity);
        for good in ALL_GOODS {
            check_nonneg(findings, "region.capacity", region.capacity[good.index()], scenario, seed, day, mode, &entity);
        }
        // `Region::unrest`'s own writer (`politics::tick_politics`) clamps
        // every write to `0..100` - it is read elsewhere as a percentage.
        check_range(findings, "region.unrest", region.unrest, 0.0, 100.0, scenario, seed, day, mode, &entity);
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
        check_range_excl_hi(findings, "region.occupation", region.occupation, 0.0, 100.0, scenario, seed, day, mode, &entity);
        // `Region::devastation`'s own doc: "War damage, 0..1".
        check_range(findings, "region.devastation", region.devastation, 0.0, 1.0, scenario, seed, day, mode, &entity);
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
                mode,
                &entity,
            );
        }
        // A faction cannot occupy its own territory - "occupier" only means
        // anything as "someone other than the owner holding this against
        // them".
        if region.occupier == Some(region.owner) {
            findings.record(
                "region_occupies_itself".to_string(),
                format!("{scenario} seed {seed} day {day} mode {mode}: {entity} owner={:?} occupier==owner", region.owner),
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
                    "{scenario} seed {seed} day {day} mode {mode}: {entity} occupier={:?} occupation_kind={:?}",
                    region.occupier, region.occupation_kind
                ),
            );
        }
    }

    // --- Factions ---
    for (fidx, faction) in world.factions.iter().enumerate() {
        let entity = format!("faction[{fidx}]");
        check_nonneg(findings, "faction.manpower", faction.manpower, scenario, seed, day, mode, &entity);
        for good in ALL_GOODS {
            // A stockpile going negative would let a faction spend matériel
            // it never had.
            check_nonneg(findings, "faction.stock", faction.stock[good.index()], scenario, seed, day, mode, &entity);
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
                mode,
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
                mode,
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
                mode,
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
                mode,
                &entity,
            );
        }
        // `action::apply_set_conscription` rejects anything outside `0..=1`.
        check_range(findings, "faction.conscription", faction.conscription, 0.0, 1.0, scenario, seed, day, mode, &entity);
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
            mode,
            &entity,
        );
        // `politics::tick_politics` clamps every write to `0..100` - the
        // whole political model reads these as percentages.
        check_range(findings, "faction.war_support", faction.war_support, 0.0, 100.0, scenario, seed, day, mode, &entity);
        check_range(findings, "faction.stability", faction.stability, 0.0, 100.0, scenario, seed, day, mode, &entity);
        // `faction.shortage` is a `max()` of three already-clamped `0..1`
        // values (`economy::tick_economy`).
        check_range(findings, "faction.shortage", faction.shortage, 0.0, 1.0, scenario, seed, day, mode, &entity);
        // `logistics::distribute_supply` computes this as `served/demand`
        // then `.min(1.0)`, with both operands non-negative.
        check_range(findings, "faction.supply_ratio", faction.supply_ratio, 0.0, 1.0, scenario, seed, day, mode, &entity);
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
            mode,
            &entity,
        );
        for g in 0..GROUP_COUNT {
            // Stage 3A: `politics::tick_politics` clamps every write to
            // `0..100`.
            check_range(findings, "faction.group_support", faction.group_support[g], 0.0, 100.0, scenario, seed, day, mode, &entity);
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
                mode,
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
        check_range(findings, "unit.manpower", unit.manpower, 0.0, UNIT_MANPOWER, scenario, seed, day, mode, &entity);
        check_range(findings, "unit.equipment", unit.equipment, 0.0, UNIT_EQUIPMENT, scenario, seed, day, mode, &entity);
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
            mode,
            &entity,
        );
        // `military::tick_recovery` clamps every write to `0..1` -
        // `combat_power`'s `(0.50 + 0.50 * morale)` term assumes a fraction.
        check_range(findings, "unit.morale", unit.morale, 0.0, 1.0, scenario, seed, day, mode, &entity);
        // `logistics::distribute_supply` clamps every write to `0..1` -
        // `combat_power`'s `(0.35 + 0.65 * supply)` term assumes a fraction.
        check_range(findings, "unit.supply", unit.supply, 0.0, 1.0, scenario, seed, day, mode, &entity);
        // Documented as "the fraction of this unit's equipment gap the
        // supply network can currently deliver" - a fraction.
        check_range(findings, "unit.arms_delivery", unit.arms_delivery, 0.0, 1.0, scenario, seed, day, mode, &entity);
        // `arms_budget = equipment_gap * arms_delivery`, and
        // `equipment_gap <= UNIT_EQUIPMENT` by construction.
        check_range(findings, "unit.arms_budget", unit.arms_budget, 0.0, UNIT_EQUIPMENT, scenario, seed, day, mode, &entity);
        // `military::tick_combat`'s own `.min(1.0)` - `combat_power`'s
        // `(1.00 + 0.35 * experience)` term assumes a fraction.
        check_range(findings, "unit.experience", unit.experience, 0.0, 1.0, scenario, seed, day, mode, &entity);
        // `Unit::branch`'s own doc: "Some(branch) for every land unit
        // (station.domain() == Domain::Land)... None for every fleet and
        // air unit."
        let is_land = unit.station.domain() == Domain::Land;
        if unit.branch.is_some() != is_land {
            findings.record(
                "unit_branch_domain_disagree".to_string(),
                format!(
                    "{scenario} seed {seed} day {day} mode {mode}: {entity} station={:?} branch={:?}",
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
                format!("{scenario} seed {seed} day {day} mode {mode}: {entity} is alive but its faction is not"),
            );
        }
    }

    // --- Sea zones ---
    for (zidx, zone) in world.sea_zones.iter().enumerate() {
        let entity = format!("sea_zone[{zidx}]");
        for f in 0..n_factions {
            // `SeaZone::control`'s own doc: "power[f] / sum(power[*])" - a
            // share, `0..1`.
            check_range(findings, "sea_zone.control", zone.control[f], 0.0, 1.0, scenario, seed, day, mode, &entity);
        }
    }

    // --- Transport network ---
    for (nidx, node) in world.transport_nodes.iter().enumerate() {
        let entity = format!("transport_node[{nidx}]");
        // `transport::Condition::new` can only ever hold `0.0..=1.0`.
        check_range(findings, "transport_node.condition", node.condition.get(), 0.0, 1.0, scenario, seed, day, mode, &entity);
    }
    let line_flows = logistics::transport_line_flows(world);
    for (lidx, line) in world.transport_lines.iter().enumerate() {
        let entity = format!("transport_line[{lidx}]");
        check_range(findings, "transport_line.condition", line.condition.get(), 0.0, 1.0, scenario, seed, day, mode, &entity);
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
                format!("{scenario} seed {seed} day {day} mode {mode}: {entity} flow={flow} > effective_capacity={cap}"),
            );
        } else if !flow.is_finite() {
            findings.record(
                "transport_line_flow_nonfinite".to_string(),
                format!("{scenario} seed {seed} day {day} mode {mode}: {entity} flow={flow}"),
            );
        }
    }

    // --- Supply arrays (World-level, per (region|zone|node, faction)) ---
    for r in 0..world.regions.len() {
        check_nonneg(findings, "world.supply", world.supply[r], scenario, seed, day, mode, &format!("region[{r}]"));
        for f in 0..n_factions {
            check_nonneg(
                findings,
                "world.supply_by_faction",
                world.supply_by_faction[r][f],
                scenario,
                seed,
                day,
                mode,
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
                mode,
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
                mode,
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
///
/// Returns `(accepted, rejected)` counts so fuzz callers (stage B) can track
/// how many of the actions they threw at the engine actually landed, without
/// this function's own callers needing to change - the two existing
/// `heuristic`-mode call sites in `run_soak` simply don't bind the return
/// value.
fn apply_and_check_atomicity(world: &mut World, faction: FactionId, actions: Vec<Action>, scenario: &str, seed: u64, day: u32, mode: &str, findings: &mut Findings, actions_seen: &mut BTreeSet<&'static str>) -> (u64, u64) {
    let mut accepted = 0u64;
    let mut rejected = 0u64;
    for act in actions {
        actions_seen.insert(action_tag(&act));
        let act_debug = format!("{act:?}");
        // Stage B addition: `shadow_precondition_holds` must be evaluated
        // against the world *as it stood when the engine itself made this
        // decision* - i.e. before `apply_action` below runs, never after.
        // `apply_strike_node`'s own losses (`air::apply_strike_losses`) can
        // destroy or displace the very squadron that made the strike legal,
        // so re-deriving `air::units_reaching` on the post-mutation world
        // would flag every *legitimate* strike whose attacker took losses
        // it couldn't survive - a false positive this exact ordering exists
        // to rule out (`Action` is `Clone`, cloned once here for the finding
        // message built after `apply_action` has consumed the original).
        let act_for_shadow = act.clone();
        let shadow_pre = shadow_precondition_holds(world, faction, &act);
        let before = fingerprint(world);
        let result = action::apply_action(world, faction, act);
        match &result {
            Ok(()) => accepted += 1,
            Err(_) => rejected += 1,
        }
        if result.is_ok() && shadow_pre == Some(false) {
            findings.record(
                shadow_check_key(&act_for_shadow).to_string(),
                format!(
                    "{scenario} seed {seed} day {day} mode {mode}: faction {faction:?} action {act_for_shadow:?} was accepted \
                     but its own documented reach precondition (checked against the world just before this action \
                     applied) did not hold"
                ),
            );
        }
        if let Err(err) = result {
            let after = fingerprint(world);
            if before != after {
                findings.record(
                    "rejected_action_mutated_world".to_string(),
                    format!(
                        "{scenario} seed {seed} day {day} mode {mode}: faction {faction:?} action {act_debug} rejected as \
                         {err:?} but the world's fingerprint changed ({before} -> {after})"
                    ),
                );
            }
        }
    }
    (accepted, rejected)
}

/// Stage B addition (docs/soak-spec.md §5's flagship case, restated in the
/// task brief that commissioned this stage: "the `StrikeNode` one is the
/// specific case stage A predicted only fuzz could reach"). `action::
/// apply_strike_node`'s own doc states the attacker must have aircraft
/// reaching the target region *before* anything else runs
/// (`air::units_reaching`); `action::apply_interdict_line`'s own doc states
/// the same precondition for `action::any_force_reaches`. Both functions are
/// already `pub` - this reuses them as the ground truth for "was this action
/// actually eligible", rather than re-deriving the rule (docs/conventions.md
/// "エクストリームプログラミング禁止" is about inventing a new business
/// abstraction, not about calling an existing public function a second
/// time from a test to check the engine's own decision agrees with it).
///
/// **Must be called against the world exactly as it stood before
/// `apply_action` runs** - see `apply_and_check_atomicity`'s own call site
/// for why. `apply_strike_node` itself reads `air::units_reaching` at that
/// same pre-mutation instant (its own doc: "collected before anything is
/// mutated"), and its `air::apply_strike_losses` can then destroy or
/// displace the very squadron that made the strike legal; a check made
/// against the world *after* `apply_action` returns would flag every
/// legitimate strike whose attacker didn't survive its own sortie as if the
/// precondition had never held - this file's own `mvp_valid_fuzz_zero_
/// invariant_violations` caught exactly that false positive in itself
/// during development (recorded in this stage's own report), which is why
/// the ordering is called out this explicitly.
///
/// Returns `None` when `act` is neither `StrikeNode` nor `InterdictLine`
/// (nothing to check) or when the id it names doesn't resolve to a real
/// node/line (can't have been accepted anyway, since `apply_strike_node`/
/// `apply_interdict_line` themselves reject that first); `Some(true)`/
/// `Some(false)` otherwise.
///
/// Every per-field range check in this file (`check_world_invariants`) is
/// blind to this defect class by construction: a struck node's `condition`
/// stays inside its own legal `0..1` range whether or not the striker had
/// any business striking it at all (soak-spec.md's own account of why
/// stage A's reintroduction of this exact historical bug went undetected).
/// This is the one check in this file that is not a value-range check - it
/// exists specifically to give `valid-fuzz` (soak-spec.md §0: `HeuristicAgent`
/// never issues either action without confirming reach first, so this can
/// never fire under `heuristic` mode - the self-shielding problem this whole
/// stage exists to close) something to actually catch, not merely reach.
fn shadow_precondition_holds(world: &World, faction: FactionId, act: &Action) -> Option<bool> {
    match act {
        Action::StrikeNode { node } => {
            let n = world.transport_nodes.get(node.index())?;
            Some(!air::units_reaching(world, n.region, faction).is_empty())
        }
        Action::InterdictLine { line } => {
            let l = world.transport_lines.get(line.index())?;
            let region_a = world.transport_node(l.from).region;
            let region_b = world.transport_node(l.to).region;
            Some(action::any_force_reaches(world, region_a, faction) || action::any_force_reaches(world, region_b, faction))
        }
        _ => None,
    }
}

/// `Findings` key for a `shadow_precondition_holds` failure - kept
/// per-action-kind (rather than one shared key) so `Findings::print`'s own
/// "first occurrence, deduplicated by check" report (soak-spec.md §4) never
/// merges a `StrikeNode` violation's detail under an `InterdictLine` one or
/// vice versa. Panics for anything `shadow_precondition_holds` itself
/// returns `None`/never `Some(false)` for - this is only ever called
/// immediately after a `Some(false)` from that function on the very same
/// `act`, never independently.
fn shadow_check_key(act: &Action) -> &'static str {
    match act {
        Action::StrikeNode { .. } => "strike_node_accepted_without_aircraft_in_range",
        Action::InterdictLine { .. } => "interdict_line_accepted_without_force_in_range",
        other => panic!("shadow_check_key called for {other:?}, which shadow_precondition_holds never flags"),
    }
}

// ---------------------------------------------------------------------
// 3b. Stage B (docs/soak-spec.md §2): `valid-fuzz` and `chaos-fuzz`.
//
// Both modes build every action's JSON payload from the same source an
// external RL/HTTP client would (`archipelago_api::action_codec::schema()`
// for the vocabulary, `action_codec::action_from_value` for decoding it back
// into a real `Action`) rather than a second, hand-maintained list of
// `Action` variants - soak-spec.md §2: "同じ源から引く... 行動を足したのに
// fuzz が知らない、という状態を作らない". `FuzzSource::from_schema` reads
// every enum vocabulary (`good`/`treaty`/`focus`/`research_axis`/`domain`/
// `station_kind`/`treaty_term_kind`/`project_kind`) and the full action-type
// list straight out of that schema at runtime, so a new `Action` variant
// shipped correctly (schema entry + decoder arm, `schema_matches_decoder`'s
// own job in `crates/api`) needs no matching update here to be *sampled* -
// only `sample_action_value`'s `match` needs a new arm to know how to build
// one, and that `match` panics loudly (not silently skips) if it doesn't -
// see `fuzz_generator_covers_every_schema_action_type` below, which turns
// that panic into an ordinary, always-on `cargo test` failure rather than
// something only the `#[ignore]`d fuzz suites would ever hit.
//
// `chaos: bool` threads through every sampler below: `false` (valid-fuzz)
// always draws a real id/enum key/in-range number so the payload is
// well-typed and *legal by construction* (soak-spec.md §2: "合法な行動を
// 無作為に選んで出す") even when the specific combination is one no
// `HeuristicAgent` would ever choose; `true` (chaos-fuzz) sometimes swaps a
// field for an out-of-domain value instead (`garbage_id`/`garbage_weight`/
// `sample_key`'s own "garbage" menus) - soak-spec.md §2: "非合法を含む行動
// を出す". Neither mode ever hand-picks which action *kind* is legal for
// the current world state (e.g. only building where a faction owns
// something) - that discrimination is `Simulation::apply`'s own job, and
// pre-filtering it away here would just be a second, weaker copy of the
// validation this file exists to exercise from the outside.
// ---------------------------------------------------------------------

/// Which of soak-spec.md §2's three modes is driving one run. `Heuristic`
/// reproduces stage A exactly (no fuzz action is ever issued); the other
/// two additionally layer fuzz actions on top of whatever `HeuristicAgent`
/// itself decided each day (see `run_soak`'s own doc for why "on top of",
/// not "instead of").
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Heuristic,
    ValidFuzz,
    ChaosFuzz,
}

impl Mode {
    /// Human-readable label matching docs/soak-spec.md §2's own vocabulary
    /// ("heuristic"/"valid-fuzz"/"chaos-fuzz") - `codex review` (P2): every
    /// recorded `Findings` message must carry scenario/seed/day *and* mode
    /// (soak-spec.md §3), since the same scenario/seed/day now names up to
    /// three different runs (one per mode) rather than one.
    fn label(self) -> &'static str {
        match self {
            Mode::Heuristic => "heuristic",
            Mode::ValidFuzz => "valid-fuzz",
            Mode::ChaosFuzz => "chaos-fuzz",
        }
    }
}

/// Fixed XOR mask that seeds the fuzz generator's own `Rng` from the same
/// `seed` a run was given, while keeping its output sequence distinct from
/// `Simulation::with_world`'s own internal `Rng` (also seeded from `seed`,
/// but never shared - `Simulation`'s own doc). Nothing about this file's
/// determinism guarantee (soak-spec.md §3: "無作為の行動選択も seed から
/// 決まること") depends on the specific constant, only on both `Rng`s being
/// deterministic functions of `seed`.
const FUZZ_RNG_MASK: u64 = 0x9E37_79B9_7F4A_7C15;

/// How many fuzz actions each living faction is offered per day, uniformly
/// `0..=MAX_FUZZ_ACTIONS_PER_FACTION_PER_DAY` - `0` included on purpose, so
/// some (faction, day) pairs look exactly like `heuristic` mode even inside
/// a fuzz run (soak-spec.md §2's "legal-but-unusual combinations and
/// orderings" includes the combination of "nothing extra today").
const MAX_FUZZ_ACTIONS_PER_FACTION_PER_DAY: u32 = 3;

/// chaos-fuzz only: the chance that `garbage_envelope` replaces an entire
/// generated action with a structurally-broken top-level JSON value (missing
/// `type`, an unknown `type`, or not even an object) instead of perturbing
/// one field of an otherwise well-typed action - soak-spec.md §2's "非合法
/// を含む行動" includes malformed envelopes, not just malformed field values.
const CHAOS_GARBAGE_ENVELOPE_PROB: f32 = 0.1;

/// chaos-fuzz only: the chance any single id/number/enum field a sampler
/// below builds is replaced by an out-of-domain "garbage" value instead of a
/// real one. `0.5` rather than `1.0` so a typical chaos action still has
/// *some* well-formed fields - the fully-nonsensical end of the spectrum is
/// already covered by `CHAOS_GARBAGE_ENVELOPE_PROB` and by whichever action
/// *kind* got picked (uniform over every schema type, unfiltered by whether
/// it makes sense for the current world state).
const CHAOS_FIELD_CORRUPTION_PROB: f32 = 0.5;

/// Picks a uniform index into a length-`len` sequence. `len == 0` is
/// possible (e.g. a scenario with no transport lines) and must not panic
/// (`% 0`) - it degenerates to always returning `0`, which every caller
/// below only ever feeds into a JSON id field a decoder validates against
/// the real length anyway (an id of `0` into an empty collection is simply
/// invalid, exactly like every other out-of-range id this file generates).
fn pick(rng: &mut Rng, len: usize) -> u32 {
    if len == 0 { 0 } else { rng.next_u32() % len as u32 }
}

fn sample_bool(rng: &mut Rng) -> bool {
    rng.unit() < 0.5
}

/// chaos-fuzz id corruption menu: every one of `Value::as_u32`'s own
/// rejection reasons (non-finite, negative, non-integral, `> u32::MAX`) plus
/// one value that parses as a perfectly good `u32` but addresses nothing in
/// any real world (`u32::MAX` itself) - so roughly half of this menu tests
/// `action_from_value`'s own decode-time rejection and the other half tests
/// `Simulation::apply`'s `.get()`-guarded out-of-range lookups.
fn garbage_id(rng: &mut Rng) -> f64 {
    const MENU: [f64; 7] = [f64::NAN, f64::INFINITY, -1.0, -1.0e9, 0.5, 4_294_967_296.0, u32::MAX as f64];
    MENU[pick(rng, MENU.len()) as usize]
}

/// chaos-fuzz numeric-field corruption menu: non-finite (rejected at decode,
/// `Value::as_f32` requires finite), and finite-but-out-of-every-field's-own
/// legal range (rejected by whichever `apply_set_*` clamp/range-check owns
/// that field) - never a value that happens to be legal for every field this
/// menu is used on, since the point is to land outside each field's own
/// documented range at least some of the time.
fn garbage_weight(rng: &mut Rng) -> f64 {
    const MENU: [f64; 6] = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0e6, 1.0e6, -0.0001];
    MENU[pick(rng, MENU.len()) as usize]
}

/// `chaos` id sampler shared by every id-typed field below: a real, in-range
/// index most of the time, `garbage_id`'s menu with probability
/// `CHAOS_FIELD_CORRUPTION_PROB` when `chaos` is set. Always a real index
/// when `chaos` is false (valid-fuzz's own "legal by construction" contract).
fn sample_id(rng: &mut Rng, len: usize, chaos: bool) -> f64 {
    if chaos && rng.unit() < CHAOS_FIELD_CORRUPTION_PROB {
        garbage_id(rng)
    } else {
        pick(rng, len) as f64
    }
}

/// `chaos` numeric sampler shared by every weight/rate/value field below -
/// `garbage_weight`'s twin of `sample_id` immediately above.
fn sample_weight(rng: &mut Rng, lo: f32, hi: f32, chaos: bool) -> f64 {
    if chaos && rng.unit() < CHAOS_FIELD_CORRUPTION_PROB {
        garbage_weight(rng)
    } else {
        rng.range(lo, hi) as f64
    }
}

/// `chaos` enum-key sampler shared by every enum-typed field below: a real
/// key from `keys` (itself read off the live schema by `FuzzSource::
/// from_schema`, never a second hand-typed vocabulary) most of the time, one
/// of a handful of definitely-not-a-real-key strings with probability
/// `CHAOS_FIELD_CORRUPTION_PROB` when `chaos` is set.
fn sample_key(rng: &mut Rng, keys: &[String], chaos: bool) -> String {
    if chaos && rng.unit() < CHAOS_FIELD_CORRUPTION_PROB {
        const GARBAGE: [&str; 4] = ["", "not_a_real_key", "🎲not_json_safe🎲", "null"];
        GARBAGE[pick(rng, GARBAGE.len()) as usize].to_string()
    } else if keys.is_empty() {
        // Defensive only: every enum this file samples from is non-empty on
        // every shipped scenario (each has at least one Good/Treaty/Focus/
        // ResearchAxis/domain/... key) - if a future schema ever advertised
        // an empty enum, garbage is the only thing left to offer.
        "not_a_real_key".to_string()
    } else {
        keys[pick(rng, keys.len()) as usize].clone()
    }
}

/// Free-text sampler for `propose_in_natural_language`'s `text` field -
/// `action::apply_propose_nl` enforces `balance::NL_PROPOSAL_TEXT_MAX_CHARS`,
/// so the chaos menu specifically includes something well past that bound.
fn sample_text(rng: &mut Rng, chaos: bool) -> String {
    if chaos && rng.unit() < CHAOS_FIELD_CORRUPTION_PROB {
        match pick(rng, 3) {
            0 => String::new(),
            1 => "x".repeat(5_000),
            _ => "\u{0}\u{1}\u{7}control-bytes".to_string(),
        }
    } else {
        const WORDS: [&str; 6] = ["和平を求める", "同盟を提案する", "撤退せよ", "trade?", "help", "no"];
        WORDS[pick(rng, WORDS.len()) as usize].to_string()
    }
}

/// chaos-fuzz only: replaces an entire action with a structurally-broken
/// top-level value instead of a well-formed-but-corrupted one - see this
/// section's own header doc and `CHAOS_GARBAGE_ENVELOPE_PROB`.
fn garbage_envelope(rng: &mut Rng) -> Value {
    match pick(rng, 4) {
        0 => Value::obj(vec![]),
        1 => Value::obj(vec![("type", Value::str("not_a_real_action"))]),
        2 => Value::arr(vec![]),
        _ => Value::Null,
    }
}

/// Every enum vocabulary and the full action-type list, read once from
/// `archipelago_api::action_codec::schema()` - the "same source the RL
/// action table uses" this stage's own brief requires. `branch` is the one
/// exception: Stage 11B's `RecruitUnit.branch` has no schema/enum entry at
/// all yet (`crates/api/src/action_codec.rs::actions_schema`'s own
/// `recruit_unit` entry only lists `region`/`domain`) even though the
/// decoder already reads it (defaulting to `Infantry` when absent) - so this
/// is read off `archipelago_sim::military::ALL_BRANCHES` instead, the same
/// enum-array-plus-`key()` convention every other vocabulary here already
/// follows, just without a schema layer in between. Exercising it anyway
/// (rather than only ever sending the decoder's own default) is strictly
/// more coverage, not less.
struct FuzzSource {
    action_types: Vec<String>,
    good: Vec<String>,
    treaty: Vec<String>,
    focus: Vec<String>,
    research_axis: Vec<String>,
    domain: Vec<String>,
    station_kind: Vec<String>,
    treaty_term_kind: Vec<String>,
    project_kind: Vec<String>,
    branch: Vec<String>,
}

fn schema_enum(schema: &Value, name: &str) -> Vec<String> {
    schema
        .get("enums")
        .and_then(|e| e.get(name))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("schema()[\"enums\"][{name:?}] is missing or not an array"))
        .iter()
        .map(|v| v.as_str().expect("schema enum entries are always strings").to_string())
        .collect()
}

impl FuzzSource {
    fn from_schema() -> Self {
        let schema = action_codec::schema();
        let action_types: Vec<String> = schema
            .get("actions")
            .and_then(Value::as_array)
            .expect("schema()[\"actions\"] is missing or not an array")
            .iter()
            .map(|entry| {
                entry
                    .get("type")
                    .and_then(Value::as_str)
                    .expect("every schema action entry has a string `type`")
                    .to_string()
            })
            .collect();
        FuzzSource {
            good: schema_enum(&schema, "good"),
            treaty: schema_enum(&schema, "treaty"),
            focus: schema_enum(&schema, "focus"),
            research_axis: schema_enum(&schema, "research_axis"),
            domain: schema_enum(&schema, "domain"),
            station_kind: schema_enum(&schema, "station_kind"),
            treaty_term_kind: schema_enum(&schema, "treaty_term_kind"),
            project_kind: schema_enum(&schema, "project_kind"),
            branch: ALL_BRANCHES.iter().map(|b| b.key().to_string()).collect(),
            action_types,
        }
    }
}

// ---------------------------------------------------------------------
// 3c. `codex review` P1: `valid-fuzz` sampling ids from global collections
// (any region/unit/faction, not ones `faction` actually owns or can
// legally target) made most of its own generated actions bounce off
// `Simulation::apply`'s validation - exploring rejection, which is
// `chaos-fuzz`'s job, not the "legal-but-unusual combinations" soak-spec.md
// §2 asks `valid-fuzz` for. Every candidate list below mirrors one specific
// `apply_*` precondition in `crates/sim/src/action.rs`, re-checked by hand
// against that function so this doesn't silently drift into a second,
// weaker copy of it - the same discipline `shadow_precondition_holds`
// already follows.
//
// **Deliberately NOT included: attacker-capability preconditions**
// (`air::units_reaching`/`action::any_force_reaches` for `StrikeNode`/
// `InterdictLine`) or economic sufficiency (manpower/stock/machinery for
// `RecruitUnit`/`ReinforceUnit`/`Build`, cooldowns for `ProposeTreaty`).
// The capability omission is the important one: filtering `StrikeNode`'s
// candidates down to targets `faction` can actually reach would silently
// rebuild `HeuristicAgent`'s own self-shielding *inside* the fuzzer meant
// to break it (soak-spec.md §0, `shadow_precondition_holds`'s own doc) -
// this file's own report has the acceptance-rate accounting for exactly
// which fields stay unfiltered and why. The economic ones are just
// genuinely expensive to precompute per candidate for a middling payoff
// (`Simulation::apply` still validates them regardless) and are reported,
// not silently forced up.
// ---------------------------------------------------------------------

fn own_unit_ids(world: &World, faction: FactionId) -> Vec<u32> {
    world.units.iter().enumerate().filter(|(_, u)| u.alive && u.owner == faction).map(|(i, _)| i as u32).collect()
}

fn own_region_ids(world: &World, faction: FactionId) -> Vec<u32> {
    world.regions.iter().enumerate().filter(|(_, r)| r.owner == faction).map(|(i, _)| i as u32).collect()
}

/// `RecruitUnit`/`Build`'s shared "own and not currently fought over"
/// precondition (`apply_recruit`/`apply_build`'s own `World::
/// has_enemy_units` check, `ActionError::RegionContested`).
fn own_uncontested_region_ids(world: &World, faction: FactionId) -> Vec<u32> {
    own_region_ids(world, faction).into_iter().filter(|&r| !world.has_enemy_units(RegionId(r), faction)).collect()
}

/// `Build`'s further precondition: no construction already running there
/// (`apply_build`: `ActionError::AlreadyBuilding` otherwise).
fn own_buildable_region_ids(world: &World, faction: FactionId) -> Vec<u32> {
    own_uncontested_region_ids(world, faction).into_iter().filter(|&r| world.region(RegionId(r)).construction.is_none()).collect()
}

/// `CancelBuild`'s actual precondition is the *opposite* of `Build`'s: a
/// construction must already be running there (`apply_cancel_build`:
/// `ActionError::NoConstruction` otherwise).
fn own_region_with_construction_ids(world: &World, faction: FactionId) -> Vec<u32> {
    own_region_ids(world, faction).into_iter().filter(|&r| world.region(RegionId(r)).construction.is_some()).collect()
}

fn other_alive_faction_ids(world: &World, faction: FactionId) -> Vec<u32> {
    world.factions.iter().enumerate().filter(|(i, f)| f.alive && *i as u32 != faction.0).map(|(i, _)| i as u32).collect()
}

/// `DeclareWar`'s actual precondition: only valid from `Stance::Ceasefire`
/// (`apply_declare_war`; already-`War` or any other stance is rejected).
fn ceasefire_faction_ids(world: &World, faction: FactionId) -> Vec<u32> {
    other_alive_faction_ids(world, faction)
        .into_iter()
        .filter(|&i| world.diplomacy.stance(faction, FactionId(i)) == Stance::Ceasefire)
        .collect()
}

/// `BreakTreaty`'s actual precondition: `with` must currently hold a real,
/// breakable (non-`Ceasefire` - `apply_break_treaty` rejects that kind
/// outright, `DeclareWar` is the ceasefire-breaking action instead) treaty
/// with `faction`.
fn breakable_treaty_partners(world: &World, faction: FactionId) -> Vec<(u32, Treaty)> {
    let mut out = Vec::new();
    for other in other_alive_faction_ids(world, faction) {
        for &treaty in ALL_TREATIES.iter() {
            if treaty != Treaty::Ceasefire && world.diplomacy.has_treaty(faction, FactionId(other), treaty) {
                out.push((other, treaty));
            }
        }
    }
    out
}

/// `AcceptTreaty`/`RejectTreaty`'s actual precondition: a real pending
/// proposal *to* `faction` (`apply_accept_treaty`/`apply_reject_treaty`'s
/// own `world.diplomacy.pending` lookup) naming exactly this `(from,
/// treaty)` pair - picking `from` and `treaty` independently would still
/// mismatch most of the time even with both individually "real".
fn pending_treaty_proposals_to(world: &World, faction: FactionId) -> Vec<(u32, Treaty)> {
    world.diplomacy.pending.iter().filter(|p| p.to == faction).map(|p| (p.from.0, p.treaty)).collect()
}

/// `RespondToNaturalLanguageProposal`'s actual precondition: a real pending
/// NL proposal *to* `faction` (`apply_respond_nl`'s own `find_pending_nl`).
fn pending_nl_proposals_to(world: &World, faction: FactionId) -> Vec<u32> {
    world.diplomacy.pending_nl.iter().filter(|p| p.to == faction).map(|p| p.from.0).collect()
}

/// `InterdictLine`'s target-side precondition (`apply_interdict_line`):
/// both endpoints share one owner, that owner isn't `faction`, and `faction`
/// is at war with them. Not filtered further by `action::any_force_reaches`
/// - see this section's own header doc.
fn hostile_line_ids(world: &World, faction: FactionId) -> Vec<u32> {
    world
        .transport_lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let owner_a = world.region(world.transport_node(l.from).region).owner;
            let owner_b = world.region(world.transport_node(l.to).region).owner;
            owner_a == owner_b && owner_a != faction && world.diplomacy.is_at_war(faction, owner_a)
        })
        .map(|(i, _)| i as u32)
        .collect()
}

/// `StrikeNode`'s target-side precondition (`apply_strike_node`): an
/// `Airfield`/`Port` node whose region is hostile. Not filtered further by
/// `air::units_reaching` - see this section's own header doc and
/// `shadow_precondition_holds`'s own doc for why that omission is
/// deliberate (soak-spec.md §0's self-shielding gap).
fn hostile_strikeable_node_ids(world: &World, faction: FactionId) -> Vec<u32> {
    world
        .transport_nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            (n.kind == TransportNodeKind::Airfield || n.kind == TransportNodeKind::Port) && {
                let owner = world.region(n.region).owner;
                owner != faction && world.diplomacy.is_at_war(faction, owner)
            }
        })
        .map(|(i, _)| i as u32)
        .collect()
}

/// `Build`'s `Project::TransportLine` precondition: both endpoints must be
/// this faction's own (`apply_build`'s own check, `ActionError::
/// LineNotOwned` otherwise).
fn own_transport_line_ids(world: &World, faction: FactionId) -> Vec<u32> {
    world
        .transport_lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            world.region(world.transport_node(l.from).region).owner == faction
                && world.region(world.transport_node(l.to).region).owner == faction
        })
        .map(|(i, _)| i as u32)
        .collect()
}

/// `chaos`-aware id sampler for a field whose *legal* value is scoped to
/// `candidates` (one of the lists just above), not "any real entity of this
/// kind" - `sample_id`'s counterpart for fields `apply_action` validates
/// against something narrower than mere existence. Picks from `candidates`
/// whenever there is at least one (in every mode - see this section's own
/// header doc for why chaos-fuzz benefits from this too, not just
/// valid-fuzz); corrupts to `garbage_id`'s menu with probability
/// `CHAOS_FIELD_CORRUPTION_PROB` when `chaos` is set, exactly as before this
/// stage's `candidate_or_any` fix; falls back to `any_len`'s full range only
/// when `candidates` is genuinely empty (this faction owns/can-target none
/// of this kind right now - reported, not hidden).
fn candidate_or_any(rng: &mut Rng, candidates: &[u32], any_len: usize, chaos: bool) -> f64 {
    if chaos && rng.unit() < CHAOS_FIELD_CORRUPTION_PROB {
        return garbage_id(rng);
    }
    if !candidates.is_empty() {
        candidates[pick(rng, candidates.len()) as usize] as f64
    } else {
        pick(rng, any_len) as f64
    }
}

/// `AcceptTreaty`/`RejectTreaty`/`BreakTreaty`'s shared shape: the target
/// field (`from`/`with`) and the `treaty` field must name the *same* real
/// pair, not just independently-real values (`pending_treaty_proposals_to`/
/// `breakable_treaty_partners`'s own doc). Falls back to independently
/// sampling both (`sample_id`/`sample_key`) when `pairs` is empty or a
/// chaos-corruption roll fires - the same fallback shape every other
/// candidate list here uses.
fn sample_correlated_faction_treaty(rng: &mut Rng, pairs: &[(u32, Treaty)], treaty_keys: &[String], n_factions: usize, chaos: bool) -> (f64, String) {
    if chaos && rng.unit() < CHAOS_FIELD_CORRUPTION_PROB {
        return (garbage_id(rng), sample_key(rng, treaty_keys, chaos));
    }
    if !pairs.is_empty() {
        let (id, treaty) = pairs[pick(rng, pairs.len()) as usize];
        return (id as f64, treaty.key().to_string());
    }
    (pick(rng, n_factions) as f64, sample_key(rng, treaty_keys, chaos))
}

fn sample_station(src: &FuzzSource, world: &World, rng: &mut Rng, chaos: bool) -> Value {
    let kind = sample_key(rng, &src.station_kind, chaos);
    let len = match kind.as_str() {
        "region" => world.regions.len(),
        "sea" => world.sea_zones.len(),
        "airfield" => world.transport_nodes.len(),
        // Garbage kind (chaos only - `station_from_value` rejects it before
        // `id` is even looked at): the id domain doesn't matter here.
        _ => world.regions.len(),
    };
    Value::obj(vec![("kind", Value::str(kind)), ("id", Value::num(sample_id(rng, len, chaos)))])
}

/// `MoveUnit`'s own precondition (`action_codec::station_from_value`'s doc:
/// "a land unit's `to` must be `Station::Region`; a fleet's must be
/// `Station::Sea`; a squadron's must be `Station::Airfield`") - reads the
/// picked unit's own current station and builds a `to` of the matching kind
/// for a *real* candidate: `World::neighbors` for land (an actual adjacent
/// region - `apply_move`'s own `ActionError::NotAdjacent` otherwise) and
/// this faction's own operational airfields for air.
///
/// **Sea is left unfiltered, and so is `NotAdjacent` in general beyond plain
/// land adjacency**: this test harness has no equally simple "real adjacent
/// sea zone" query to reuse (`apply_move`'s own sea-zone adjacency lives
/// behind more machinery than a single `World` method), and `Domain::Air`'s
/// own `balance::AIR_OPERATING_RADIUS_KM` reach check is a geographic
/// distance, not a graph the way land adjacency is - both are reported as
/// genuinely-hard-to-satisfy-at-random in this stage's own report rather
/// than forced.
fn sample_move_destination(src: &FuzzSource, world: &World, faction: FactionId, rng: &mut Rng, chaos: bool, unit_id: u32) -> Value {
    if !chaos {
        if let Some(unit) = world.units.get(unit_id as usize) {
            match unit.station {
                Station::Region(r) => {
                    let neighbor_ids: Vec<u32> = world.neighbors(r).map(|n| n.0).collect();
                    if !neighbor_ids.is_empty() {
                        let to = neighbor_ids[pick(rng, neighbor_ids.len()) as usize];
                        return Value::obj(vec![("kind", Value::str("region")), ("id", Value::num(to as f64))]);
                    }
                }
                Station::Airfield(_) => {
                    let candidates: Vec<u32> = world
                        .transport_nodes
                        .iter()
                        .enumerate()
                        .filter(|(_, n)| {
                            n.kind == TransportNodeKind::Airfield && n.operational() && world.region(n.region).owner == faction
                        })
                        .map(|(i, _)| i as u32)
                        .collect();
                    if !candidates.is_empty() {
                        let to = candidates[pick(rng, candidates.len()) as usize];
                        return Value::obj(vec![("kind", Value::str("airfield")), ("id", Value::num(to as f64))]);
                    }
                }
                Station::Sea(_) => {}
            }
        }
    }
    sample_station(src, world, rng, chaos)
}

fn sample_project(src: &FuzzSource, world: &World, faction: FactionId, rng: &mut Rng, chaos: bool) -> Value {
    let kind = sample_key(rng, &src.project_kind, chaos);
    match kind.as_str() {
        "capacity" => Value::obj(vec![("capacity", Value::str(sample_key(rng, &src.good, chaos)))]),
        "transport_line" => {
            let candidates = own_transport_line_ids(world, faction);
            Value::obj(vec![(
                "transport_line",
                Value::num(candidate_or_any(rng, &candidates, world.transport_lines.len(), chaos)),
            )])
        }
        // "infrastructure" | "port" | "repair", or (chaos) a garbage kind
        // string - `project_from_value` treats any bare string it doesn't
        // recognize as an unknown project, never a panic.
        other => Value::str(other),
    }
}

/// `sample_action_value`'s own `"respond_to_natural_language_proposal"`
/// arm's `terms` builder. `cede`'s `region` is scoped to `faction`'s own
/// regions (a faction can only cede territory it holds); `sign`/`deliver`/
/// `withdraw` are left unscoped - `diplomacy::apply_treaty_terms`'s own
/// interpretation of a natural-language response is a secondary, rare path
/// this stage doesn't attempt to fully model (see this stage's own report).
fn sample_treaty_term(src: &FuzzSource, world: &World, faction: FactionId, rng: &mut Rng, chaos: bool) -> Value {
    let kind = sample_key(rng, &src.treaty_term_kind, chaos);
    match kind.as_str() {
        "sign" => Value::obj(vec![("kind", Value::str("sign")), ("treaty", Value::str(sample_key(rng, &src.treaty, chaos)))]),
        "withdraw" => {
            Value::obj(vec![("kind", Value::str("withdraw")), ("from", Value::num(sample_id(rng, world.regions.len(), chaos)))])
        }
        "cede" => {
            let candidates = own_region_ids(world, faction);
            Value::obj(vec![("kind", Value::str("cede")), ("region", Value::num(candidate_or_any(rng, &candidates, world.regions.len(), chaos)))])
        }
        "deliver" => Value::obj(vec![
            ("kind", Value::str("deliver")),
            ("good", Value::str(sample_key(rng, &src.good, chaos))),
            ("amount", Value::num(sample_weight(rng, 0.0, 1000.0, chaos))),
        ]),
        // Garbage kind (chaos only): no other fields - `treaty_term_from_value`
        // rejects an unknown `kind` before looking for them.
        other => Value::obj(vec![("kind", Value::str(other))]),
    }
}

/// Builds one JSON action payload for schema action type `kind`, scoped to
/// the acting faction's own real preconditions when one applies (this
/// section's own header doc: `codex review` P1) and otherwise (still)
/// sourced from `world`'s own live entity counts rather than a fixed slot
/// count the way `python/env/schema.py`'s RL table does - this harness
/// drives one already-running `World`, not a fresh episode, so "a real id"
/// means "real right now", not "real at any point an episode could reach".
/// Panics if `kind` isn't one this function knows how to build - see this
/// section's own header doc for why that is the correct, loud failure mode
/// rather than silently producing nothing.
fn sample_action_value(src: &FuzzSource, kind: &str, world: &World, faction: FactionId, rng: &mut Rng, chaos: bool) -> Value {
    let n_regions = world.regions.len();
    let n_units = world.units.len();
    let n_factions = world.factions.len();
    let n_lines = world.transport_lines.len();
    let n_nodes = world.transport_nodes.len();

    match kind {
        "move_unit" => {
            let candidates = own_unit_ids(world, faction);
            let unit_val = candidate_or_any(rng, &candidates, n_units, chaos);
            let to = sample_move_destination(src, world, faction, rng, chaos, unit_val as u32);
            Value::obj(vec![("type", Value::str("move_unit")), ("unit", Value::num(unit_val)), ("to", to)])
        }
        "hold_unit" => {
            let candidates = own_unit_ids(world, faction);
            Value::obj(vec![("type", Value::str("hold_unit")), ("unit", Value::num(candidate_or_any(rng, &candidates, n_units, chaos)))])
        }
        "disband_unit" => {
            let candidates = own_unit_ids(world, faction);
            Value::obj(vec![("type", Value::str("disband_unit")), ("unit", Value::num(candidate_or_any(rng, &candidates, n_units, chaos)))])
        }
        "recruit_unit" => {
            let domain_key = sample_key(rng, &src.domain, chaos);
            let base = own_uncontested_region_ids(world, faction);
            let region_candidates: Vec<u32> = match domain_key.as_str() {
                "land" => base,
                "sea" => base.into_iter().filter(|&r| world.port_node_operational(RegionId(r))).collect(),
                "air" => base.into_iter().filter(|&r| world.airfield_node_operational(RegionId(r))).collect(),
                // Garbage domain (chaos only): no meaningful filter left.
                _ => Vec::new(),
            };
            Value::obj(vec![
                ("type", Value::str("recruit_unit")),
                ("region", Value::num(candidate_or_any(rng, &region_candidates, n_regions, chaos))),
                ("domain", Value::str(domain_key)),
                ("branch", Value::str(sample_key(rng, &src.branch, chaos))),
            ])
        }
        "reinforce_unit" => {
            let candidates = own_unit_ids(world, faction);
            Value::obj(vec![("type", Value::str("reinforce_unit")), ("unit", Value::num(candidate_or_any(rng, &candidates, n_units, chaos)))])
        }
        "set_conscription" => {
            Value::obj(vec![("type", Value::str("set_conscription")), ("value", Value::num(sample_weight(rng, 0.0, 1.0, chaos)))])
        }
        "set_industry_priority" => Value::obj(vec![
            ("type", Value::str("set_industry_priority")),
            ("good", Value::str(sample_key(rng, &src.good, chaos))),
            ("weight", Value::num(sample_weight(rng, 0.0, 1.0, chaos))),
        ]),
        "set_civilian_ration" => Value::obj(vec![
            ("type", Value::str("set_civilian_ration")),
            ("value", Value::num(sample_weight(rng, CIVILIAN_RATION_MIN, CIVILIAN_RATION_MAX, chaos))),
        ]),
        "build" => {
            let candidates = own_buildable_region_ids(world, faction);
            Value::obj(vec![
                ("type", Value::str("build")),
                ("region", Value::num(candidate_or_any(rng, &candidates, n_regions, chaos))),
                ("project", sample_project(src, world, faction, rng, chaos)),
            ])
        }
        "cancel_build" => {
            let candidates = own_region_with_construction_ids(world, faction);
            Value::obj(vec![("type", Value::str("cancel_build")), ("region", Value::num(candidate_or_any(rng, &candidates, n_regions, chaos)))])
        }
        "set_import_plan" => {
            // `apply_set_import_plan`'s actual precondition: only Food/
            // Energy are importable at all (`ActionError::InvalidValue`
            // otherwise). Restricted here for valid-fuzz; chaos-fuzz keeps
            // sampling the full `good` vocabulary (`sample_key`'s own
            // corruption) - a real good that isn't Food/Energy is itself a
            // legal-but-wrong-domain value worth exercising, not something
            // to filter away.
            let good = if chaos { sample_key(rng, &src.good, chaos) } else { ["food", "energy"][pick(rng, 2) as usize].to_string() };
            Value::obj(vec![
                ("type", Value::str("set_import_plan")),
                ("good", Value::str(good)),
                ("rate", Value::num(sample_weight(rng, 0.0, IMPORT_PLAN_RATE_MAX, chaos))),
            ])
        }
        "set_logistics_priority" => Value::obj(vec![
            ("type", Value::str("set_logistics_priority")),
            ("good", Value::str(sample_key(rng, &src.good, chaos))),
            ("weight", Value::num(sample_weight(rng, 0.0, 1.0, chaos))),
        ]),
        "set_research_allocation" => Value::obj(vec![
            ("type", Value::str("set_research_allocation")),
            ("axis", Value::str(sample_key(rng, &src.research_axis, chaos))),
            ("weight", Value::num(sample_weight(rng, 0.0, 1.0, chaos))),
        ]),
        "propose_treaty" => {
            let candidates = other_alive_faction_ids(world, faction);
            Value::obj(vec![
                ("type", Value::str("propose_treaty")),
                ("to", Value::num(candidate_or_any(rng, &candidates, n_factions, chaos))),
                ("treaty", Value::str(sample_key(rng, &src.treaty, chaos))),
            ])
        }
        "accept_treaty" => {
            let pairs = pending_treaty_proposals_to(world, faction);
            let (from, treaty) = sample_correlated_faction_treaty(rng, &pairs, &src.treaty, n_factions, chaos);
            Value::obj(vec![("type", Value::str("accept_treaty")), ("from", Value::num(from)), ("treaty", Value::str(treaty))])
        }
        "reject_treaty" => {
            let pairs = pending_treaty_proposals_to(world, faction);
            let (from, treaty) = sample_correlated_faction_treaty(rng, &pairs, &src.treaty, n_factions, chaos);
            Value::obj(vec![("type", Value::str("reject_treaty")), ("from", Value::num(from)), ("treaty", Value::str(treaty))])
        }
        "declare_war" => {
            let candidates = ceasefire_faction_ids(world, faction);
            Value::obj(vec![("type", Value::str("declare_war")), ("to", Value::num(candidate_or_any(rng, &candidates, n_factions, chaos)))])
        }
        "break_treaty" => {
            let pairs = breakable_treaty_partners(world, faction);
            let (with, treaty) = sample_correlated_faction_treaty(rng, &pairs, &src.treaty, n_factions, chaos);
            Value::obj(vec![("type", Value::str("break_treaty")), ("with", Value::num(with)), ("treaty", Value::str(treaty))])
        }
        "set_national_focus" => {
            Value::obj(vec![("type", Value::str("set_national_focus")), ("focus", Value::str(sample_key(rng, &src.focus, chaos)))])
        }
        "propose_in_natural_language" => {
            let candidates = other_alive_faction_ids(world, faction);
            Value::obj(vec![
                ("type", Value::str("propose_in_natural_language")),
                ("to", Value::num(candidate_or_any(rng, &candidates, n_factions, chaos))),
                ("text", Value::str(sample_text(rng, chaos))),
            ])
        }
        "respond_to_natural_language_proposal" => {
            let candidates = pending_nl_proposals_to(world, faction);
            let from = candidate_or_any(rng, &candidates, n_factions, chaos);
            let n_terms = pick(rng, 3) as usize;
            let terms = (0..n_terms).map(|_| sample_treaty_term(src, world, faction, rng, chaos)).collect();
            Value::obj(vec![
                ("type", Value::str("respond_to_natural_language_proposal")),
                ("from", Value::num(from)),
                ("accept", Value::Bool(sample_bool(rng))),
                ("terms", Value::arr(terms)),
            ])
        }
        "interdict_line" => {
            let candidates = hostile_line_ids(world, faction);
            Value::obj(vec![("type", Value::str("interdict_line")), ("line", Value::num(candidate_or_any(rng, &candidates, n_lines, chaos)))])
        }
        "strike_node" => {
            let candidates = hostile_strikeable_node_ids(world, faction);
            Value::obj(vec![("type", Value::str("strike_node")), ("node", Value::num(candidate_or_any(rng, &candidates, n_nodes, chaos)))])
        }
        other => panic!(
            "fuzz generator has no builder for schema action type {other:?} - GET /schema advertises it but \
             soak.rs's sample_action_value doesn't know how to build one yet; add an arm here \
             (docs/soak-spec.md §2: fuzz actions must come from the same schema the RL action table does, and \
             this workspace has already shipped an action that existed in the schema/decoder but was silently \
             unreachable from a hand-maintained action list twice - Phase 11 and Stage 12C, per CLAUDE.md)"
        ),
    }
}

fn choose_fuzz_action(src: &FuzzSource, world: &World, faction: FactionId, rng: &mut Rng, chaos: bool) -> Value {
    if chaos && rng.unit() < CHAOS_GARBAGE_ENVELOPE_PROB {
        return garbage_envelope(rng);
    }
    let kind = &src.action_types[pick(rng, src.action_types.len()) as usize];
    sample_action_value(src, kind, world, faction, rng, chaos)
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
    /// Stage B fuzz stats - all zero under `Mode::Heuristic` (no fuzz action
    /// is ever issued there). `issued == accepted + apply_rejected +
    /// decode_rejected`.
    fuzz_accepted: u64,
    fuzz_apply_rejected: u64,
    fuzz_decode_rejected: u64,
}

/// Runs `world` to completion (or `days`, whichever comes first) with a
/// default `HeuristicAgent` per faction - `apps/headless/src/main.rs`'s own
/// per-day loop, reproduced (not imported - see this file's module doc),
/// with every action applied one at a time through `apply_and_check_atomicity`
/// instead of the batched `Simulation::apply`, and `check_world_invariants`
/// run once up front and again after every tick.
///
/// `mode` (soak-spec.md §2) never changes what `HeuristicAgent` itself
/// decides - `Mode::ValidFuzz`/`Mode::ChaosFuzz` only *add*
/// `0..=MAX_FUZZ_ACTIONS_PER_FACTION_PER_DAY` fuzz actions per living
/// faction per day, on top of its normal orders, before `sim.step()` runs.
/// "On top of", not "instead of", is a deliberate choice: soak-spec.md §2
/// asks for "legal-but-unusual combinations", and a world where every
/// faction stops playing sensibly at all would decay into stalemate before
/// exercising anything interesting - the same reason `heuristic` mode is
/// the baseline every fuzz mode still runs on. The fuzz `Rng` is seeded from
/// `seed` (`FUZZ_RNG_MASK`'s own doc) so this is deterministic end to end
/// even in fuzz modes.
fn run_soak(scenario_label: &str, world: World, seed: u64, days: u32, mode: Mode) -> SoakOutcome {
    let n = world.factions.len();
    let mut sim = Simulation::with_world(world, seed);
    let mut agents: Vec<Box<dyn Agent>> = (0..n).map(|i| Box::new(default_heuristic_agent(i)) as Box<dyn Agent>).collect();
    let fuzz_source = if mode == Mode::Heuristic { None } else { Some(FuzzSource::from_schema()) };
    let mut fuzz_rng = Rng::new(seed ^ FUZZ_RNG_MASK);
    let chaos = mode == Mode::ChaosFuzz;
    // codex review P2: threaded into every `Findings` message below
    // (`check_world_invariants`/`apply_and_check_atomicity`) so the same
    // scenario/seed/day names one run, not up to three.
    let mode_label = mode.label();

    let mut findings = Findings::default();
    let mut actions_seen = BTreeSet::new();
    let mut events_seen = BTreeSet::new();
    let mut good_ever_nonzero = [false; GOOD_COUNT];
    let mut frozen_streak = 0u32;
    let mut max_frozen_streak_days = 0u32;
    let mut ticks_checked = 0u32;
    let mut fuzz_accepted = 0u64;
    let mut fuzz_apply_rejected = 0u64;
    let mut fuzz_decode_rejected = 0u64;

    check_world_invariants(scenario_label, seed, sim.world.day, mode_label, &sim.world, &mut findings);
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
            apply_and_check_atomicity(&mut sim.world, faction, actions, scenario_label, seed, day, mode_label, &mut findings, &mut actions_seen);

            if let Some(src) = &fuzz_source {
                let n_fuzz = pick(&mut fuzz_rng, (MAX_FUZZ_ACTIONS_PER_FACTION_PER_DAY + 1) as usize);
                for _ in 0..n_fuzz {
                    let value = choose_fuzz_action(src, &sim.world, faction, &mut fuzz_rng, chaos);
                    match action_codec::action_from_value(&value) {
                        Ok(act) => {
                            let (ok, err) = apply_and_check_atomicity(
                                &mut sim.world,
                                faction,
                                vec![act],
                                scenario_label,
                                seed,
                                day,
                                mode_label,
                                &mut findings,
                                &mut actions_seen,
                            );
                            fuzz_accepted += ok;
                            fuzz_apply_rejected += err;
                        }
                        Err(reason) => {
                            // valid-fuzz's own contract: every sampler above
                            // always produces a well-typed payload when
                            // `chaos` is false, so a decode failure here is
                            // this generator's own bug, not a finding about
                            // the engine - fail loudly rather than silently
                            // undercounting how much of the schema valid-fuzz
                            // actually exercised.
                            assert!(
                                chaos,
                                "valid-fuzz generator produced JSON action_from_value rejected \
                                 ({scenario_label} seed {seed} day {day} faction {faction:?}): {reason} \
                                 for payload {value:?}"
                            );
                            fuzz_decode_rejected += 1;
                        }
                    }
                }
            }
        }
        for event in sim.step() {
            events_seen.insert(event_tag(&event));
        }

        check_world_invariants(scenario_label, seed, sim.world.day, mode_label, &sim.world, &mut findings);
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

    SoakOutcome {
        findings,
        actions_seen,
        events_seen,
        good_ever_nonzero,
        max_frozen_streak_days,
        final_day: sim.world.day,
        ticks_checked,
        fuzz_accepted,
        fuzz_apply_rejected,
        fuzz_decode_rejected,
    }
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

    let (mut fuzz_accepted, mut fuzz_apply_rejected, mut fuzz_decode_rejected) = (0u64, 0u64, 0u64);
    for (_, _, outcome) in runs {
        fuzz_accepted += outcome.fuzz_accepted;
        fuzz_apply_rejected += outcome.fuzz_apply_rejected;
        fuzz_decode_rejected += outcome.fuzz_decode_rejected;
    }
    let fuzz_issued = fuzz_accepted + fuzz_apply_rejected + fuzz_decode_rejected;
    if fuzz_issued > 0 {
        println!(
            "\nfuzz actions issued: {fuzz_issued} (accepted {fuzz_accepted}, rejected by Simulation::apply \
             {fuzz_apply_rejected}, rejected at decode {fuzz_decode_rejected})"
        );
    }

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
    let outcome = run_soak("mvp", scenario::build_world(), 1, 720, Mode::Heuristic);
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
        let outcome = run_soak("japan47", world, seed, 720, Mode::Heuristic);
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
        let outcome = run_soak("japan_hex", world, seed, 720, Mode::Heuristic);
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

    runs.push(("mvp", 1, run_soak("mvp", scenario::build_world(), 1, 720, Mode::Heuristic)));

    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        runs.push(("japan47", seed, run_soak("japan47", world, seed, 720, Mode::Heuristic)));
    }
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        runs.push(("japan_hex", seed, run_soak("japan_hex", world, seed, 720, Mode::Heuristic)));
    }

    print_report("heuristic, 7 runs (mvp x1, japan47 x3, japan_hex x3), 720 days each", &runs);

    let total: u64 = runs.iter().map(|(_, _, o)| o.findings.total()).sum();
    assert_eq!(total, 0, "heuristic soak report found runtime invariant violations - see the printed report above");
}

// ---------------------------------------------------------------------
// 8. Stage B tests (docs/soak-spec.md §2): `valid-fuzz` and `chaos-fuzz`.
//
// Every test below shares stage A's own contract: a `Findings` violation is
// always a live defect, in every mode - fuzz mode changes which actions get
// tried, never what counts as a violation (`check_world_invariants`'s own
// checks don't read `Mode` at all, and `shadow_check_reach_precondition`
// only ever *adds* a violation kind heuristic play could never trigger in
// the first place). So every test here keeps stage A's `assert_eq!(...,
// 0, ...)` shape unchanged rather than loosening it for fuzz modes - a
// looser bar here would be exactly the "loosen it once, ratchet it twice"
// shape CLAUDE.md's own retrospective warns against.
// ---------------------------------------------------------------------

/// Proves `sample_action_value` can build *every* action type `GET /schema`
/// advertises - the actual answer to "how do you know the generator covers
/// every schema action" (see this file's "3b." section header for why this,
/// not a second list compared against the first, is the proof). Always-on:
/// one schema call plus one decode call per action type, not a soak run.
#[test]
fn fuzz_generator_covers_every_schema_action_type() {
    let world = scenario::build_world();
    let src = FuzzSource::from_schema();
    assert!(!src.action_types.is_empty(), "GET /schema reported no action types at all");
    let mut rng = Rng::new(1);
    for kind in &src.action_types {
        let value = sample_action_value(&src, kind, &world, FactionId(0), &mut rng, false);
        let decoded = action_codec::action_from_value(&value);
        assert!(
            decoded.is_ok(),
            "fuzz generator's own payload for schema action type {kind:?} failed to decode: {decoded:?} \
             (payload: {value:?})"
        );
    }
}

/// Always-on, small (`mvp`): `valid-fuzz` layers `0..=3` schema-legal but
/// otherwise random actions per faction per day on top of `HeuristicAgent`'s
/// own choices. This is the mode docs/soak-spec.md §0 says is the only way
/// to reach `HeuristicAgent`'s own self-shielding gaps (`StrikeNode` without
/// aircraft, `InterdictLine` without any reachable force) - see
/// `shadow_check_reach_precondition`'s own doc for the two checks that exist
/// specifically to catch them here.
#[test]
fn mvp_valid_fuzz_zero_invariant_violations() {
    let outcome = run_soak("mvp", scenario::build_world(), 1, 720, Mode::ValidFuzz);
    if outcome.findings.total() > 0 {
        outcome.findings.print();
    }
    assert_eq!(outcome.findings.total(), 0, "mvp valid-fuzz seed 1: runtime invariant violations found - see printed detail above");
}

/// Always-on, small (`mvp`): `chaos-fuzz` additionally sends malformed/
/// illegal actions (soak-spec.md §2's "非合法を含む行動"). The bar is
/// fail-fast (reject without panicking, and a rejected action must not
/// mutate the world - `apply_and_check_atomicity`'s own atomicity check,
/// reused unchanged), not tolerance: a panic anywhere under
/// `action_codec::action_from_value`/`Simulation::apply` aborts this test
/// process outright (nothing to assert - the process exit code says it
/// all), and an atomicity/invariant violation fails the assertion below.
#[test]
fn mvp_chaos_fuzz_no_panic_and_atomicity() {
    let outcome = run_soak("mvp", scenario::build_world(), 1, 720, Mode::ChaosFuzz);
    if outcome.findings.total() > 0 {
        outcome.findings.print();
    }
    assert_eq!(outcome.findings.total(), 0, "mvp chaos-fuzz seed 1: runtime invariant violations found - see printed detail above");
}

/// `japan47`-scale valid-fuzz, multiple seeds - `#[ignore]`d for runtime
/// only, same as this file's own heuristic equivalents.
#[test]
#[ignore]
fn japan47_valid_fuzz_zero_invariant_violations() {
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        let outcome = run_soak("japan47", world, seed, 720, Mode::ValidFuzz);
        if outcome.findings.total() > 0 {
            outcome.findings.print();
        }
        assert_eq!(outcome.findings.total(), 0, "japan47 valid-fuzz seed {seed}: runtime invariant violations found - see printed detail above");
    }
}

#[test]
#[ignore]
fn japan47_chaos_fuzz_no_panic_and_atomicity() {
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        let outcome = run_soak("japan47", world, seed, 720, Mode::ChaosFuzz);
        if outcome.findings.total() > 0 {
            outcome.findings.print();
        }
        assert_eq!(outcome.findings.total(), 0, "japan47 chaos-fuzz seed {seed}: runtime invariant violations found - see printed detail above");
    }
}

/// `japan_hex`-scale valid-fuzz (289 regions, 8 factions, 532 transport
/// nodes) - `#[ignore]`d for runtime.
#[test]
#[ignore]
fn japan_hex_valid_fuzz_zero_invariant_violations() {
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        let outcome = run_soak("japan_hex", world, seed, 720, Mode::ValidFuzz);
        if outcome.findings.total() > 0 {
            outcome.findings.print();
        }
        assert_eq!(outcome.findings.total(), 0, "japan_hex valid-fuzz seed {seed}: runtime invariant violations found - see printed detail above");
    }
}

#[test]
#[ignore]
fn japan_hex_chaos_fuzz_no_panic_and_atomicity() {
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        let outcome = run_soak("japan_hex", world, seed, 720, Mode::ChaosFuzz);
        if outcome.findings.total() > 0 {
            outcome.findings.print();
        }
        assert_eq!(outcome.findings.total(), 0, "japan_hex chaos-fuzz seed {seed}: runtime invariant violations found - see printed detail above");
    }
}

/// The `valid-fuzz` counterpart of `heuristic_soak_report` - same 7 runs,
/// same report shape, `Mode::ValidFuzz` instead.
#[test]
#[ignore]
fn valid_fuzz_soak_report() {
    let mut runs: Vec<(&str, u64, SoakOutcome)> = Vec::new();

    runs.push(("mvp", 1, run_soak("mvp", scenario::build_world(), 1, 720, Mode::ValidFuzz)));

    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        runs.push(("japan47", seed, run_soak("japan47", world, seed, 720, Mode::ValidFuzz)));
    }
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        runs.push(("japan_hex", seed, run_soak("japan_hex", world, seed, 720, Mode::ValidFuzz)));
    }

    print_report("valid-fuzz, 7 runs (mvp x1, japan47 x3, japan_hex x3), 720 days each", &runs);

    let total: u64 = runs.iter().map(|(_, _, o)| o.findings.total()).sum();
    assert_eq!(total, 0, "valid-fuzz soak report found runtime invariant violations - see the printed report above");
}

/// The `chaos-fuzz` counterpart of `heuristic_soak_report`.
#[test]
#[ignore]
fn chaos_fuzz_soak_report() {
    let mut runs: Vec<(&str, u64, SoakOutcome)> = Vec::new();

    runs.push(("mvp", 1, run_soak("mvp", scenario::build_world(), 1, 720, Mode::ChaosFuzz)));

    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan47.json").expect("scenarios/japan47.json must load");
        runs.push(("japan47", seed, run_soak("japan47", world, seed, 720, Mode::ChaosFuzz)));
    }
    for seed in [1u64, 2, 3] {
        let world = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        runs.push(("japan_hex", seed, run_soak("japan_hex", world, seed, 720, Mode::ChaosFuzz)));
    }

    print_report("chaos-fuzz, 7 runs (mvp x1, japan47 x3, japan_hex x3), 720 days each", &runs);

    let total: u64 = runs.iter().map(|(_, _, o)| o.findings.total()).sum();
    assert_eq!(total, 0, "chaos-fuzz soak report found runtime invariant violations - see the printed report above");
}
