//! Daily production, the Stage 2A commodity chain, civilian consumption,
//! and conscription.
//!
//! This is also the labor/production trade-off from design.md §9: drafting
//! manpower reduces `region.labor_ratio`, which lowers every commodity's
//! output through `efficiency`.
//!
//! The chain (docs/phase2-spec.md "Stage 2A の解き方") is solved as one
//! national pool per faction, in a fixed stage order every tick. Civilian
//! demand for each good is served as soon as that good is available and
//! *before* any industrial recipe downstream of it draws on the same stock
//! - not after the whole industrial chain has already spent it - so a
//! faction only runs a shortage when its production genuinely can't cover
//! its population, not as an unavoidable side effect of solve order:
//! 1. `potential[g]` per faction = the capacity-weighted output every
//!    commodity *could* produce this tick, before any input constraint.
//! 1.5. **Production reads `Faction::stock` back** (`production_throttle_
//!    mult`, added once expanded reproduction (commit 52bb761) made
//!    industrial capacity actually compound: capacity that outgrows its own
//!    steady demand used to run flat out into a warehouse forever, since
//!    nothing upstream of this step ever read `Faction::stock` at all).
//!    `Food`/`Energy`/`Steel`/`Machinery`/`Munitions` each have their
//!    `pot[g]` eased down, continuously and without any cutoff, once their
//!    own national stock already covers more than `PRODUCTION_RESERVE_
//!    TARGET_DAYS` of that good's real structural demand - see that
//!    function's own doc for the exact curve, why it never reaches zero,
//!    and why a good with no demand yet is never punished for it. The
//!    Steel/Energy a throttled-down good would have drawn on is not
//!    redirected anywhere *this* tick (step 4's own doc: no
//!    redistribution mid-tick) - it simply stays unspent in stock, which
//!    raises every good's own budget share of it starting next tick. Five
//!    goods only: `Infantry`/`Armour`/`Artillery`/`Naval`/`Aircraft` have no
//!    comparable demand figure anywhere in this codebase and are not grown
//!    by investment either - see the throttle's own call site for why.
//!    Resolved in a fixed, dependency-respecting order - `Food`/`Machinery`/
//!    `Munitions` first (their own `needed` references nothing else in this
//!    group), then `Steel` (reads `Machinery`'s/`Munitions`' just-throttled
//!    `pot`), then `Energy` (reads `Steel`'s) - never an iterative solve;
//!    see the throttle call site's own doc for why one pass is exact here.
//! 2. `Food`/`Energy` have no inputs, so they're produced in full (net of
//!    step 1.5's throttle) and added straight to stock.
//! 3. Civilians draw `Food` and `Energy` from that stock (see
//!    `Faction::civilian_ration`); the worst-served of the two so far
//!    seeds `Faction::shortage`.
//! 4. `Steel`, `Machinery` and `Munitions` all draw on the same `Energy`
//!    stock left after civilians (a review flagged the earlier version of
//!    this stage, which gave `Steel` an unconditional first claim on
//!    `Energy` - the same hardcoded-precedence defect shape as #3 and the
//!    Phase 1 unrest accumulator - so a scarce `Energy` stock could starve
//!    `Machinery` and `Munitions` to zero no matter what a faction actually
//!    wanted): the `Energy` stock is split into a budget per good by
//!    `Faction::industry_priority[Steel]/[Machinery]/[Munitions]`, the same
//!    mechanism step 5 already used to split `Steel` between `Machinery`
//!    and `Munitions`. `Machinery` and `Munitions` also draw on `Steel`
//!    produced by this same step.
//! 5. `Machinery` and `Munitions` share `Steel` output; when it can't cover
//!    both at their full potential, it is split between them by
//!    `Faction::industry_priority`.
//! 6. Civilians draw `Machinery` from what industry left behind, folding
//!    into `Faction::shortage` the same way.
//! 7. `Infantry` equipment (Stage 11A's renamed `Arms`, `good::Good`'s own
//!    doc) is capped by what's left of the `Machinery` and `Steel` stock.
//! 8. `Armour`/`Artillery`/`Naval`/`Aircraft` are produced straight from
//!    capacity like `Food`/`Energy` - no recipe, no interaction with any
//!    step above. Stage 11B now wires real land branches and Sea/Air
//!    recruits to draw all four, but their *production* stays exactly the
//!    no-input shape Stage 11A gave Armour/Artillery, so none of them
//!    contends with `Infantry` for the shared Machinery/Steel budget above.

use crate::balance::{
    INFANTRY_INPUT_MACHINERY, INFANTRY_INPUT_STEEL, CAPITAL_FLIGHT_CONSTRUCTION_MULT, CAPITAL_FLIGHT_MACHINERY_MULT,
    CIVILIAN_ENERGY_DEMAND_PER_POP, CIVILIAN_FOOD_DEMAND_PER_POP, CIVILIAN_MACHINERY_DEMAND_PER_POP,
    CONSCRIPT_RATE, CONSTRUCTION_MACHINERY_PER_POINT, CONSTRUCTION_RATE, CONSTRUCTION_STEEL_PER_POINT,
    FOCUS_TECHNOCRACY_CONSTRUCTION_RATE_MULT, FOCUS_TECHNOCRACY_PRODUCTION_MULT, FOOD_EFFICIENCY_DAMPENING,
    FOOD_EFFICIENCY_FLOOR, INDUSTRIAL_STABILITY_FLOOR, MACHINERY_INPUT_ENERGY, MACHINERY_INPUT_STEEL,
    MANPOWER_DEMOBILIZATION_RATE, MUNITIONS_INPUT_ENERGY, MUNITIONS_INPUT_STEEL,
    PRODUCTION_RESERVE_TARGET_DAYS, REGIME_CHANGE_OUTPUT_MULT, STEEL_INPUT_ENERGY, STRIKE_OUTPUT_MULT,
};
use crate::construction;
use crate::focus::{self, NationalFocus};
use crate::good::{Good, ALL_GOODS, GOOD_COUNT};
use crate::logistics;
use crate::research::{self, ResearchAxis};
use crate::world::{Station, World};

/// `stock[good] -> min(stock[good], input_budget / coefficient)`, treating a
/// zero coefficient as "no input needed" (unbounded).
fn input_limit(budget: f32, coefficient: f32) -> f32 {
    if coefficient > 0.0 {
        budget / coefficient
    } else {
        f32::INFINITY
    }
}

/// Serves `need * ration` from `*stock` (never oversubscribing it, and
/// never trying to deliver more than `ration` of `need` even when stock is
/// abundant - the undelivered share is left in stock for industry to use)
/// and returns the unmet fraction of the *full* `need` in `0..=1`. At
/// `ration == 1.0` this is plain "serve as much of `need` as `stock` covers."
fn consume(stock: &mut f32, need: f32, ration: f32) -> f32 {
    if need <= 0.0 {
        return 0.0;
    }
    let target = need * ration;
    let served = stock.min(target).max(0.0);
    *stock -= served;
    ((need - served) / need).clamp(0.0, 1.0)
}

/// The industrial output multiplier from a faction's political `stability`
/// (`0..100`, docs/phase3-spec.md "安定度の再定義"): floors at 0.6, the same
/// way `efficiency` floors at 0.2 - neither term can zero a faction's output
/// on its own. Factored out so Step 0's Food-specific dampening
/// (`balance::FOOD_EFFICIENCY_FLOOR`'s doc) and Step 1's per-faction
/// commodity scaling read the exact same formula instead of two copies that
/// could drift apart.
fn stability_output_mult(stability: f32) -> f32 {
    0.6 + 0.4 * (stability / 100.0)
}

/// National Munitions demand this tick: every alive unit's own upkeep draw
/// (`logistics::unit_supply_demand`'s own formula - reused rather than
/// reimplemented, exactly the per-unit figure `logistics::distribute_
/// supply` will draw `Faction::stock[Munitions]` down by later this same
/// tick), summed by owner. Computed once, before the per-faction loop below
/// takes `world.factions.iter_mut()`, the same "read `world.units` up
/// front" shape this function's own `committed` pass (bottom of this file)
/// already uses.
///
/// `in_combat` is read the same live "is an enemy actually present at this
/// unit's station" check `logistics::region_demand`/`naval::sea_demand`/
/// `air::air_demand` already run for their own domain, generalized across
/// all three `Station` variants the way `crates/agents`' own
/// `unit_contested` already does for AI decision-making - not a cached
/// flag, so it can never go stale. This is a coarser, national total than
/// the per-region figure `distribute_supply` computes later (Stage 2A's
/// production runs before combat resolves for the day, so it can only see
/// today's *opening* positions) - fine for a throttle that only needs
/// "roughly how hungry is the standing army today," not the exact per-unit
/// delivery `distribute_supply` separately computes and applies.
///
/// **A fifth codex review pass named a real gap here, reported rather than
/// fixed.** `logistics::distribute_supply` only ever debits `Faction::
/// stock[Munitions]` by `total_served` - what the transport network could
/// actually *deliver* - so a unit cut off by a severed or saturated network
/// contributes its full nominal upkeep to the sum below while drawing
/// nothing from national stock for real, keeping this throttle's reserve
/// target higher than the true realized draw. Closing this exactly would
/// need this tick's real transport-flow result
/// (`logistics::compute_transport_flow`) - but that runs *after* `economy`
/// in `Simulation::step_timed`'s own fixed order, on top of `transport::
/// tick_transport_condition`'s own today's-condition update, neither of
/// which has run yet at this point in the tick. Reaching for it here would
/// mean either reordering the tick (every other system's own doc already
/// explains why its slot is where it is) or running the flow model a
/// second time per tick - a real architectural change past a single
/// function's scope, not something to reach for without raising it first
/// (docs/conventions.md §1: propose new structure, don't just add it).
fn national_munitions_demand(world: &World, n_factions: usize) -> Vec<f32> {
    let mut demand = vec![0.0f32; n_factions];
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let in_combat = match unit.station {
            Station::Region(r) => world.has_enemy_units(r, unit.owner),
            Station::Sea(z) => world.has_enemy_fleets(z, unit.owner),
            Station::Airfield(node) => world.has_enemy_units(world.transport_node(node).region, unit.owner),
        };
        let (munitions, _arms) = logistics::unit_supply_demand(unit, in_combat);
        demand[unit.owner.index()] += munitions;
    }
    demand
}

/// A good's own production feedback from inventory - this file's module
/// doc has the defect this closes (capacity that exceeds steady demand
/// ran flat out into a warehouse forever, because nothing read `Faction::
/// stock`). Reads `1.0` (no throttle at all) while `stock` covers no more
/// than `PRODUCTION_RESERVE_TARGET_DAYS` of `needed`; beyond that horizon,
/// output eases down in inverse proportion to how many multiples of the
/// horizon the stock already covers - the same reciprocal-of-buffer-days
/// shape `crates/agents`' `apportion_growth_goods` already uses to turn a
/// buffer-days reading into a weight, here inverted into a `0..=1`
/// multiplier bounded above by `1.0` instead of an unbounded scarcity
/// weight.
///
/// **No hard cutoff.** At `buffer_days` = 1000x the target the multiplier
/// reads `0.001`, not `0.0` - production only asymptotes toward zero, it
/// never reaches it at any finite stock. A good that stops being
/// overstocked (because `needed` rose, or something else drew `stock` back
/// down) recovers to full output the very next tick this function runs
/// with the new numbers - a pure function of today's `stock`/`needed`, not
/// a persisted state, so there is no separate "unstick" step to forget
/// (`docs/conventions.md` §6: 状態には必ず回復経路を持たせる).
///
/// **`needed` is floored at `potential / PRODUCTION_RESERVE_TARGET_DAYS`,
/// never read as literally `0.0`.** A first version of this function read
/// `needed <= 0.0` as "no throttle at all" (`1.0` forever) so a good
/// nothing currently draws on - a faction with no units has no Munitions
/// `needed`, and every faction in every shipped scenario starts this way -
/// could still stockpile. A second `codex review` pass caught that this
/// reopened the exact defect this mechanism exists to close, just for that
/// one case: `1.0` forever means *no* feedback from inventory ever reaches
/// that good, so its stock would grow completely unbounded at full
/// capacity for as long as real demand stays at zero - indistinguishable
/// from the pre-fix behaviour in that state.
///
/// The floor closes this without inventing a fresh tunable constant or a
/// second vocabulary: `potential / PRODUCTION_RESERVE_TARGET_DAYS` is
/// exactly the daily draw that would make *today's own full-capacity
/// output* (`potential`, one day's worth) the comfortable reserve - so with
/// zero real demand, a good can still freely stockpile up to one day of
/// its own potential before this same curve starts easing it down, exactly
/// the way any other good eases down beyond its own reserve horizon.
///
/// **The floor only ever replaces a `needed` that is exactly `0.0` - it
/// never blends with, or overrides, a `needed` that is merely small.** This
/// is not a stylistic choice: an earlier version used `needed.max(floor)`,
/// which reads identically at `needed == 0.0` but silently inflates *every*
/// small-but-real `needed` too - and `floor` scales with `potential`, this
/// good's own raw capacity, which is exactly what can be arbitrarily large
/// relative to a genuinely small downstream draw (the very shape this
/// mechanism exists to correct). Measured directly: with `Machinery` itself
/// heavily throttled down to a near-zero *real* draw on `Steel`,
/// `steel_needed_by_chain` read a small but genuine positive number - and
/// `needed.max(floor)` replaced it with `Steel`'s own multi-hundred-unit
/// potential/30, undoing the P1 fix this same file's call site just made
/// (Steel went right back to producing at full, un-throttled output). The
/// exact-zero branch below reads a real, however-small, computed demand
/// completely unmodified - only a `needed` that is *entirely absent* (a
/// faction with no units at all summing to a literal `0.0`, never a
/// continuously-hovering epsilon: unit counts are discrete, and every
/// loaded scenario's `population > 0.0` is enforced at `Scenario::validate`)
/// falls through to the floor.
///
/// **A third codex review pass named a real, structural limit of this
/// floor - recorded here rather than left implicit.** Once `stock` exceeds
/// `potential` (the free-to-produce zone), output decays as `potential² /
/// stock` - the same reciprocal shape this whole function uses everywhere
/// else - which, integrated over time with nothing ever consuming a truly
/// zero-demand good, makes `stock` grow roughly as `sqrt(time)`: slow, but
/// not a *bounded* equilibrium. Measured directly (a faction with zero
/// units, forever, `Good::Munitions` potential pinned at 40/day since
/// nothing invests in a good nobody needs - `crates/agents::growth_buffer_
/// days` scores its own buffer as infinite, giving it zero investment
/// weight): stock reaches 2,899 by day 2,880 (the horizon this whole change
/// was measured against - about 72x potential, nowhere near the 16,000-
/// 112,000 the *compounding-capacity* defect this change fixes actually
/// produced) and 17,561 even out to day 100,000 (roughly matching the
/// `sqrt(time)` prediction, ~439x potential).
///
/// **This is not fixable within this function's own constraints, and is
/// reported rather than silently resolved.** A genuine bounded equilibrium
/// needs `mult` to fall away *faster than any reciprocal power* of `stock`
/// (an exponential-decay shape, an actual cutoff, or a real consumption/
/// decay mechanic on `Faction::stock` itself) - every one of which is
/// either a hard cutoff (this task's own "no cliffs, they oscillate" rule)
/// or a genuinely new mechanic/vocabulary this codebase does not have
/// today and this change was not asked to add
/// (docs/conventions.md §1: propose new business logic, do not just add
/// it). What this floor *does* guarantee - the actual defect this whole
/// mechanism exists to close - is that a good's own **capacity** no longer
/// compounds into an ever-faster flood the way `crates/agents`'
/// `apportion_growth_goods` measured before this change (`022ddaa`/
/// `52bb761`'s own reports): a zero-demand good's `potential` stays fixed
/// (nothing invests in it), so the residual growth here is bounded by a
/// slow, non-compounding tail on a *constant* capacity, not the compounding
/// one the defect this file's module doc names was actually about.
fn production_throttle_mult(stock: f32, needed: f32, potential: f32) -> f32 {
    let effective_needed = if needed > 0.0 { needed } else { potential.max(0.0) / PRODUCTION_RESERVE_TARGET_DAYS };
    if effective_needed <= 0.0 {
        return 1.0;
    }
    let buffer_days = stock.max(0.0) / effective_needed;
    (PRODUCTION_RESERVE_TARGET_DAYS / buffer_days.max(PRODUCTION_RESERVE_TARGET_DAYS)).clamp(0.0, 1.0)
}

pub fn tick_economy(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    // Step 0: per-region efficiency, shared across every commodity except
    // Food (Phase 1 §4.1's formula). Food used to read the exact same
    // `efficiency[i]` (and the same faction-wide `stability_mult` below) as
    // every other commodity - the Stage 3C playtest found that equality
    // closes unrest/shortage into a loop with no floor
    // (`balance::FOOD_EFFICIENCY_FLOOR`'s doc has the full account), so Food
    // now gets its own, separately dampened and floored multiplier,
    // `food_efficiency[i]`, computed from the same underlying signal
    // (`efficiency[i]` here times this region owner's `stability_mult`, read
    // fresh from `faction.stability` since the per-faction loop below
    // hasn't computed its own copy yet).
    let mut efficiency = vec![0.0f32; n_regions];
    let mut food_efficiency = vec![0.0f32; n_regions];
    for (i, region) in world.regions.iter().enumerate() {
        efficiency[i] = (region.effective_infrastructure().max(0.2)
            * region.labor_ratio()
            * (1.0 - region.unrest / 150.0))
            .clamp(0.2, 1.0);

        let owner_stability_mult =
            stability_output_mult(world.factions[region.owner.index()].stability);
        let disorder_free = (efficiency[i] * owner_stability_mult).clamp(0.0, 1.0);
        food_efficiency[i] = FOOD_EFFICIENCY_FLOOR
            + (1.0 - FOOD_EFFICIENCY_FLOOR) * disorder_free.powf(FOOD_EFFICIENCY_DAMPENING);
    }

    // Step 1: potential[f][g] = capacity-weighted output before any input
    // constraint, summed over each faction's owned regions in region-index
    // order (fixed accumulation order for determinism). Food is weighted by
    // `food_efficiency` instead of the shared `efficiency` every other good
    // uses - see Step 0's comment.
    let mut potential = vec![[0.0f32; GOOD_COUNT]; n_factions];
    let mut total_pop = vec![0.0f32; n_factions];
    // Step 1.5's own Steel/Machinery `needed` (below) has to count today's
    // active construction projects too: `construction::tick_construction`
    // draws both goods from this same national stock immediately after this
    // function runs every tick - a direct same-tick consumer the throttle
    // would otherwise not know about (codex review: a faction with a full
    // stockpile but several in-progress projects would get throttled by
    // civilian/recipe demand alone and stall those projects until the
    // stockpile fell below an unrelated reserve target).
    //
    // This mirrors `tick_construction`'s own per-region cost exactly, not
    // just its project count - a second codex review pass caught that a
    // flat `count * CONSTRUCTION_RATE` (this function's first cut, and the
    // same simplification `crates/agents`' own `active_construction_draw`
    // already makes for its own, different purpose) over- or under-states
    // real draw whenever a faction is under Capital Flight/Technocracy
    // (`tick_construction`'s own `rate` multipliers) or a project is on its
    // last, partial tick (`tick_construction`'s own `rate.min(required -
    // invested)` cap) - both read here from the exact same fields
    // `tick_construction` itself reads, before this tick's own mutable
    // per-faction loop below needs `world.factions` mutably. Deliberately
    // *not* also reproducing `tick_construction`'s `funded_ratio` clamp:
    // that depends on stock *availability*, which is what this throttle is
    // for - every other `needed` term in this function is an appetite
    // figure, not a pre-clamped one, and construction's should read the
    // same way.
    let mut construction_machinery_demand = vec![0.0f32; n_factions];
    let mut construction_steel_demand = vec![0.0f32; n_factions];
    for region in &world.regions {
        let f = region.owner.index();
        total_pop[f] += region.population;
        let e = efficiency[region.id.index()];
        let food_e = food_efficiency[region.id.index()];
        for good in ALL_GOODS {
            let mult = if good == Good::Food { food_e } else { e };
            potential[f][good.index()] += region.effective_capacity(good) * mult;
        }
        // A `Project::TransportLine` whose far endpoint has since changed
        // hands is cancelled for free, no cost charged, the moment
        // `construction::tick_construction` reaches it - this same tick,
        // immediately after `tick_economy` returns (`Simulation::step_timed`'s
        // own fixed order). Skipping it here too (`construction::
        // transport_line_still_owned`, the exact check `tick_construction`
        // itself uses) keeps this demand estimate from counting a project
        // that will not actually draw on either stock today.
        let stale_transport_line = matches!(
            region.construction.map(|c| c.project),
            Some(construction::Project::TransportLine(line_id))
                if !construction::transport_line_still_owned(world, region.id.index(), line_id)
        );
        if let Some(constr) = region.construction.filter(|_| !stale_transport_line) {
            let owner_faction = &world.factions[f];
            let mut rate = CONSTRUCTION_RATE;
            if owner_faction.capital_flight_active {
                rate *= CAPITAL_FLIGHT_CONSTRUCTION_MULT;
            }
            if focus::active(owner_faction) == Some(NationalFocus::Technocracy) {
                rate *= FOCUS_TECHNOCRACY_CONSTRUCTION_RATE_MULT;
            }
            let attempted = rate.min(constr.required - constr.invested).max(0.0);
            construction_machinery_demand[f] += attempted * CONSTRUCTION_MACHINERY_PER_POINT;
            construction_steel_demand[f] += attempted * CONSTRUCTION_STEEL_PER_POINT;
        }
    }

    // Step 1.5's own demand signal for Munitions (see `national_munitions_
    // demand`'s doc) has to be read before the loop below takes
    // `world.factions.iter_mut()` - the same "read `world.units` up front"
    // shape this function's own `committed` pass (bottom of this file)
    // already follows.
    let munitions_demand = national_munitions_demand(world, n_factions);

    let mut draft = vec![0.0f32; n_factions];
    for faction in world.factions.iter_mut() {
        if !faction.alive {
            continue;
        }
        let f = faction.id.index();

        let stability_mult = stability_output_mult(faction.stability);
        // docs/phase8-spec.md Fix 2's follow-up (the "shortage → unrest →
        // lower efficiency → worse shortage" loop for every commodity but
        // Food - see `balance::INDUSTRIAL_STABILITY_FLOOR`'s doc): every
        // non-Food good is about to be scaled by `stability_mult` below,
        // and `stability_mult` on its own already floors at 0.6
        // (`stability_output_mult`'s doc) - but combined with `efficiency`'s
        // own independent 0.2 floor (Step 0), the compound worst case a
        // fully collapsed faction's non-Food output can fall to is 0.12x
        // capacity, the same value `FOOD_EFFICIENCY_FLOOR`'s doc found
        // insufficient for Food. `industrial_stability_mult` raises just the
        // `stability_mult` half of that product for non-Food goods - not
        // `efficiency` itself, which stays exactly as sensitive to a
        // region's own war damage/unrest as before (`devastation_still_...`-
        // style tests for the other commodities must keep holding).
        let industrial_stability_mult = stability_mult.max(INDUSTRIAL_STABILITY_FLOOR);
        // Stage 3A political events (docs/phase3-spec.md "政治イベント"):
        // `Event::Strike` depresses every non-Food commodity's potential
        // (a labor strike, not a farming one); `Event::RegimeChange`
        // depresses every commodity including Food (the whole economy is
        // disrupted, not just industry) for its own fixed duration. Both are
        // read as a live "is the event's timer still running" check, not
        // recomputed from group support here - `politics::tick_politics`
        // owns triggering and counting them down.
        let strike_mult = if faction.strike_days > 0 { STRIKE_OUTPUT_MULT } else { 1.0 };
        let regime_change_mult = if faction.regime_change_days > 0 {
            REGIME_CHANGE_OUTPUT_MULT
        } else {
            1.0
        };
        // Stage 3C `NationalFocus::Technocracy` (docs/phase3-spec.md: "生産
        // 効率＋"): applies to every commodity, including Food, alongside
        // `stability_mult`/`regime_change_mult`.
        let focus_production_mult = if focus::active(faction) == Some(NationalFocus::Technocracy) {
            FOCUS_TECHNOCRACY_PRODUCTION_MULT
        } else {
            1.0
        };
        // `stability_mult` is exempted for Food: `food_efficiency` (Step 0)
        // already folded a dampened, floored copy of it in, so applying the
        // shared, un-dampened multiplier again here would undo that floor.
        // Every other commodity reads `industrial_stability_mult` instead of
        // the raw `stability_mult` (see that binding's doc just above).
        // `regime_change_mult`/`focus_production_mult` still apply to every
        // commodity including Food - see their own doc comments for why
        // that's fine (a fixed-duration, self-resetting event and a pure
        // bonus respectively, neither one part of the feedback loop this
        // exemption exists to break).
        let mut pot = potential[f];
        for (idx, v) in pot.iter_mut().enumerate() {
            *v *= regime_change_mult * focus_production_mult;
            if idx != Good::Food.index() {
                *v *= industrial_stability_mult * strike_mult;
            }
        }
        // `Event::CapitalFlight` narrows further: only Machinery output is
        // hit (docs/phase3-spec.md: "建設速度と Machinery 生産に係数" -
        // construction's own share is applied in `construction.rs`).
        if faction.capital_flight_active {
            pot[Good::Machinery.index()] *= CAPITAL_FLIGHT_MACHINERY_MULT;
        }

        // Stage 12B (docs/phase12-spec.md §0's table): the two
        // production-side research axes, each a straight multiplier on
        // `pot` exactly like every other factor in this block
        // (`research::coefficient`'s own doc has the diminishing-returns
        // shape). Civilian covers the good's own "食料・エネルギー・機械"
        // wording; Munitions covers "軍需品と装備" - the Munitions good
        // itself plus every one of the five per-branch/domain equipment
        // goods (`good::Good`'s own "○○装備" naming already groups these
        // as one family). Read fresh from `faction.research_progress`
        // every tick - never sampled once and cached - so this always
        // reflects the faction's current standing, including on a tick
        // where it just lost the territory that was funding it.
        let civilian_mult = research::coefficient(faction.research_progress[ResearchAxis::Civilian.index()]);
        for good in [Good::Food, Good::Energy, Good::Machinery] {
            pot[good.index()] *= civilian_mult;
        }
        let munitions_mult = research::coefficient(faction.research_progress[ResearchAxis::Munitions.index()]);
        for good in [Good::Munitions, Good::Infantry, Good::Armour, Good::Artillery, Good::Naval, Good::Aircraft] {
            pot[good.index()] *= munitions_mult;
        }

        let food_need = total_pop[f] * CIVILIAN_FOOD_DEMAND_PER_POP;
        let energy_need = total_pop[f] * CIVILIAN_ENERGY_DEMAND_PER_POP;
        let machinery_need = total_pop[f] * CIVILIAN_MACHINERY_DEMAND_PER_POP;
        // Read here (moved up from just before Step 2 below) so Step 1.5's
        // own throttle can use it too - see `food_need_rationed`'s own
        // comment for why.
        let ration = faction.civilian_ration;
        // Step 1.5's own throttle reads the *rationed* civilian draw, not
        // the full `food_need`/`energy_need`/`machinery_need` `consume`
        // below still uses for shortage accounting - a sixth codex review
        // pass caught that using the full figure overstates real
        // consumption whenever `civilian_ration < 1.0`: `consume` itself
        // only ever withdraws `need * ration` (this file's own `consume`
        // doc), so a faction rationing civilians to, say, 25% draws stock
        // four times slower than `food_need` alone implies - reading the
        // un-rationed figure would let a deliberately-rationed stockpile
        // read as a much shorter "reserve" than it actually is, keeping
        // production open (and consuming Steel/Energy inputs) for far
        // longer than the intended horizon. `consume`'s own unrationed
        // `food_need`/`energy_need`/`machinery_need` calls below are
        // unaffected - shortage accounting is about the *full* population's
        // need regardless of policy, a different question from how fast
        // policy is actually letting stock drain.
        let food_need_rationed = food_need * ration;
        let energy_need_rationed = energy_need * ration;
        let machinery_need_rationed = machinery_need * ration;

        // Step 1.5: give production a feedback from inventory (this file's
        // module doc has the defect - capacity exceeding steady demand ran
        // flat out into a warehouse forever). Each of these five goods
        // already has a real structural "how much does the chain actually
        // want today" figure available at this point in the tick: `Food`/
        // `Machinery`'s civilian draw just computed above, `Energy`/`Steel`'s
        // own recipe draw from downstream `pot`, and `Munitions` from the
        // standing army `national_munitions_demand` computed once above.
        // `Steel`/`Machinery` also add today's active `construction::
        // tick_construction` draw (`construction_machinery_demand[f]`/
        // `construction_steel_demand[f]`, computed above from each active
        // project's own real rate and remaining-progress cap) - a direct
        // same-tick consumer of both goods this throttle would otherwise
        // not know about, which a codex review caught: without it, a
        // faction mid-build with a full stockpile could have its own
        // construction throttled by civilian/recipe demand alone, stalling
        // in-progress projects until the stockpile fell below a reserve
        // target that has nothing to do with what construction is actually
        // drawing. `Infantry`/`Armour`/`Artillery`/`Naval`/
        // `Aircraft` are deliberately left out: none of them has a
        // comparable flow-demand figure anywhere in this codebase (their
        // stock is only ever drawn down in lumps by `action::apply_recruit`/
        // `apply_reinforce`, at whatever rate a player or AI happens to be
        // recruiting - `crates/agents`' own `growth_buffer_days` doc records
        // the same gap), and inventing one here would be exactly the
        // "pick a number, tune it until the outcome looks right" mistake
        // CLAUDE.md's own "繰り返し踏んだ欠陥" record warns against. Their
        // capacity is also never grown by investment (`crates/agents`'
        // `apportion_growth_goods` only ever targets Energy/Steel/Machinery/
        // Munitions), so they are not the compounding-capacity defect this
        // change exists to close in the first place.
        //
        // **Order is load-bearing here - a second codex review (P1) caught
        // that the first version read every good's downstream `pot` fully
        // un-throttled**, i.e. each good's *structural potential* rather than
        // what its own throttle actually leaves it drawing. That let a
        // downstream good sitting on a saturated stockpile of its own (and
        // therefore barely drawing on its inputs any more) keep reporting
        // its *full, un-throttled* appetite upstream - so Steel's own
        // `buffer_days` never rose even once Machinery/Munitions had already
        // eased off, and Steel just kept piling up behind them (measured:
        // Machinery's peak fell 112,148 -> 10,539 on japan_hex seed 3 while
        // Steel's barely moved, 36,242 -> 22,756 - the good whose `needed`
        // was computed correctly improved, the one computed from stale
        // upstream demand did not).
        //
        // The fix is *not* an iterative solve - `docs/conventions.md` bans
        // float-convergence loops, and this group has no cycle to converge
        // in the first place. The four goods form a strict one-way chain:
        // `Machinery`'s and `Munitions`' own `needed` never reference `Steel`
        // or `Energy` at all (civilian population / `Infantry`'s potential,
        // which is never throttled / construction draw for `Machinery`; the
        // standing army's upkeep for `Munitions`), so both can be resolved
        // first, independent of everything else in this group and of each
        // other. `Steel`'s `needed` sums exactly those two goods' (and
        // `Infantry`'s) appetite for `Steel`, so it is resolved next, reading
        // `pot[Machinery]`/`pot[Munitions]` *after* they were just throttled
        // above - their real, post-throttle draw, not their un-throttled
        // potential. `Energy`'s `needed` sums `Steel`'s own appetite (plus
        // `Machinery`'s/`Munitions`'), so it is resolved last, reading
        // `pot[Steel]` after *it* was just throttled. `Food` depends on
        // nothing in this group and can be resolved anywhere. One fixed pass
        // in this order is exact - not an approximation that would need
        // iterating toward a fixed point, the way `SUPPLY_FLOW_ROUNDS` in
        // `logistics.rs` iterates a genuine mutual-contention loop.
        // `industry_priority`'s own share weights (Step 3/4's own doc has
        // the full account of the split itself) moved up from where they
        // used to be computed, so `machinery_feasible`/`munitions_feasible`
        // just below can use the *same* shares - a seventh codex review
        // pass (P2) caught that the feasibility cap was treating a
        // faction's *entire* Steel/Energy stock as available to Machinery/
        // Munitions, when Steps 3/4 below only ever hand each good its own
        // `industry_priority`-weighted slice: a good with a near-zero
        // priority weight can be starved by policy just as thoroughly as by
        // genuine physical scarcity, and the feasibility cap needs to see
        // that the same way. Pure weight ratios, independent of any stock
        // value, so computing them here (before `stock[Energy]`/`stock[
        // Steel]` have even received this tick's own production) changes
        // nothing about what they mean - only *which* stock they get
        // multiplied against differs between here and Step 3/4's own use.
        let w_steel = faction.industry_priority[Good::Steel.index()].max(0.0);
        let w_machinery = faction.industry_priority[Good::Machinery.index()].max(0.0);
        let w_munitions = faction.industry_priority[Good::Munitions.index()].max(0.0);

        let energy_weight_sum = w_steel + w_machinery + w_munitions;
        let (energy_share_steel, energy_share_machinery, energy_share_munitions) =
            if energy_weight_sum > 0.0 {
                (
                    w_steel / energy_weight_sum,
                    w_machinery / energy_weight_sum,
                    w_munitions / energy_weight_sum,
                )
            } else {
                (1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0)
            };

        let steel_weight_sum = w_machinery + w_munitions;
        let (share_machinery, share_munitions) = if steel_weight_sum > 0.0 {
            (w_machinery / steel_weight_sum, w_munitions / steel_weight_sum)
        } else {
            (0.5, 0.5)
        };

        let constr_machinery = construction_machinery_demand[f];
        let constr_steel = construction_steel_demand[f];

        pot[Good::Food.index()] *= production_throttle_mult(
            faction.stock[Good::Food.index()],
            food_need_rationed,
            pot[Good::Food.index()],
        );

        let machinery_needed_by_chain =
            machinery_need_rationed + pot[Good::Infantry.index()] * INFANTRY_INPUT_MACHINERY + constr_machinery;
        pot[Good::Machinery.index()] *= production_throttle_mult(
            faction.stock[Good::Machinery.index()],
            machinery_needed_by_chain,
            pot[Good::Machinery.index()],
        );

        pot[Good::Munitions.index()] *= production_throttle_mult(
            faction.stock[Good::Munitions.index()],
            munitions_demand[f],
            pot[Good::Munitions.index()],
        );

        // Reads `pot[Machinery]`/`pot[Munitions]` *after* the two throttles
        // just above - see this block's own doc for why that ordering,
        // not iteration, is what closes the P1.
        //
        // **Feasibility-capped, not raw `pot` - a fourth codex review pass
        // (P1).** `pot[Machinery]`/`pot[Munitions]` at this point are each
        // already throttled against *their own* inventory, but neither is
        // yet capped by whatever a *third* input actually leaves them able
        // to produce: a Machinery chain genuinely starved of Steel (a real
        // capacity shortfall, not an inventory throttle - `supply_ratio`
        // running under 1.0 is this game's normal state, `docs/future-
        // work.md`'s "需要が生産を上回る理由") still reports its full,
        // un-starved potential as Energy's downstream appetite, so Energy
        // never learns that draw will not actually materialize and keeps
        // producing for a demand that structurally cannot show up - the
        // warehouse-growth shape persisting specifically for input-starved
        // chains. `machinery_feasible`/`munitions_feasible` cap each by
        // *today's starting* `faction.stock[Steel]`/`[Energy]` (untouched -
        // Step 2 below hasn't run yet, so this is a real figure already
        // known, not a second good's not-yet-computed output): if Steel has
        // been chronically scarce, today's starting Steel stock already
        // reflects that scarcity. This is deliberately a cheap, single-pass
        // *estimate*, not the exact `input_limit`/budget-split cascade
        // Steps 3-4 below compute for real (that would need a second full
        // pass through this tick's own production to know, which is
        // circular - Steel's real output depends on Energy's, and Energy's
        // throttle is exactly what this estimate feeds) - conservative in
        // the sense that a good already unthrottled by its own inventory is
        // never capped *below* what it could actually fund from today's
        // stock, only prevented from reporting appetite the stock plainly
        // cannot back.
        //
        // Each good's slice of that stock is its own `industry_priority`
        // share (`share_machinery`/`share_munitions` for Steel,
        // `energy_share_machinery`/`energy_share_munitions` for Energy -
        // computed just above, the exact shares Steps 3/4 below apply to
        // the *real* stock at that later point in the tick), not the whole
        // stock - a seventh codex review pass (P2): a good with a near-zero
        // priority weight can be starved by policy as thoroughly as by
        // genuine scarcity, and this estimate needs to see that too, or it
        // just relocates the same "phantom demand" defect from physical
        // scarcity to policy allocation.
        //
        // **Starting stock only - deliberately *not* also crediting this
        // tick's own raw potential output, after a real back-and-forth
        // across two more codex review passes.** An eighth pass (P1) first
        // caught that starting stock alone under-counts: Step 3 adds this
        // tick's own `actual_steel` to `stock[Steel]` *before* Step 4 ever
        // splits it among Machinery/Munitions, so a faction with low
        // starting Steel but real Steel *capacity* can still fund a real
        // Machinery/Munitions draw this same tick - reading only the
        // opening stock made that draw look infeasible and suppressed
        // `Energy`'s own `needed` for it, throttling Energy against a
        // shortage that was never real. Crediting `pot[Steel]`/`pot[Energy]`
        // (their raw, pre-throttle Step 1 values - the only figures
        // available here without a genuinely circular second pass, since
        // Steel's own throttle is computed *from* `steel_needed_by_chain`,
        // which these two `_feasible` values feed) fixed that specific
        // case - but a ninth pass (P1) then caught what the eighth pass's
        // own "the two failure modes don't overlap" reasoning missed: raw,
        // *un-throttled* potential is exactly what makes a good look
        // "available" regardless of how comfortably oversupplied it already
        // is - a large-capacity Steel/Energy stockpile now justified its
        // own continued full-tilt production by crediting Machinery with
        // an appetite for Steel that was never going to survive Steel's
        // *own* throttle a few lines below, self-justifying in exactly the
        // compounding-capacity, already-overstocked case this whole
        // mechanism exists to close.
        //
        // Between the two, the un-credited (starting-stock-only) version is
        // kept: it can read a transiently-low Steel stock as "Machinery
        // can't fund this" when Steel capacity would in fact cover it this
        // same tick - a real but bounded imprecision (found but not fixed;
        // it only ever makes the throttle *more* conservative, never
        // reopens unbounded growth) - while the credited version can be
        // gamed by the *exact* shape (already-large capacity, low stock
        // relative to it) this task exists to fix. A correct fix needs
        // Steel's real, *post-throttle* output, which only exists after
        // Steel's own throttle runs - a genuine two-pass dependency, not
        // something a single-pass estimate can resolve either way; closing
        // it properly is future work, not a call to make silently by
        // picking whichever single-pass estimate happens to read better on
        // one measured scenario.
        let steel_available_for_downstream = faction.stock[Good::Steel.index()];
        // Net of civilians' own rationed claim (`energy_need_rationed`,
        // already computed above): a tenth codex review pass (P2) caught
        // that Step 2.5 withdraws civilians' share of `stock[Energy]`
        // *before* Step 3 ever hands industry its own budget from what's
        // left - a known, already-computed quantity, not a second good's
        // not-yet-decided output, so crediting it here carries none of the
        // circularity risk the Steel/Machinery credit above does. Without
        // this, an Energy stock already mostly earmarked for civilians
        // still read as "available" to Machinery/Munitions, overstating
        // their real feasible draw the same direction (if more mildly) as
        // the raw-potential credit just above was reverted for.
        let energy_available_for_downstream = (faction.stock[Good::Energy.index()] - energy_need_rationed).max(0.0);
        let machinery_feasible = pot[Good::Machinery.index()]
            .min(input_limit(steel_available_for_downstream * share_machinery, MACHINERY_INPUT_STEEL))
            .min(input_limit(energy_available_for_downstream * energy_share_machinery, MACHINERY_INPUT_ENERGY));
        let munitions_feasible = pot[Good::Munitions.index()]
            .min(input_limit(steel_available_for_downstream * share_munitions, MUNITIONS_INPUT_STEEL))
            .min(input_limit(energy_available_for_downstream * energy_share_munitions, MUNITIONS_INPUT_ENERGY));

        let steel_needed_by_chain = machinery_feasible * MACHINERY_INPUT_STEEL
            + munitions_feasible * MUNITIONS_INPUT_STEEL
            + pot[Good::Infantry.index()] * INFANTRY_INPUT_STEEL
            + constr_steel;
        pot[Good::Steel.index()] *= production_throttle_mult(
            faction.stock[Good::Steel.index()],
            steel_needed_by_chain,
            pot[Good::Steel.index()],
        );

        // Reads `pot[Steel]` after *its* throttle just above, for the same
        // reason. Steel has no input but Energy, but its raw (already
        // inventory-throttled) `pot` is still not its real appetite for
        // Energy - an eleventh codex review pass (P2), the same shape as
        // `machinery_feasible`/`munitions_feasible` above but for Steel's
        // own claim: a faction with `industry_priority[Steel]` at or near
        // `0.0` gets little or none of `energy_available_for_downstream`
        // in Step 3 below (`energy_share_steel`, computed with `share_
        // machinery`/`share_munitions` above), so `actual_steel` stays
        // small regardless of how large `pot[Steel]` reads - reporting the
        // whole un-capped `pot[Steel]` as Energy demand here would be
        // exactly the policy-allocation phantom demand `energy_share_
        // machinery`/`energy_share_munitions` already exist to prevent for
        // Machinery/Munitions, just left open for Steel's own claim.
        let steel_feasible = pot[Good::Steel.index()]
            .min(input_limit(energy_available_for_downstream * energy_share_steel, STEEL_INPUT_ENERGY));
        let energy_needed_by_chain = energy_need_rationed
            + steel_feasible * STEEL_INPUT_ENERGY
            + machinery_feasible * MACHINERY_INPUT_ENERGY
            + munitions_feasible * MUNITIONS_INPUT_ENERGY;
        pot[Good::Energy.index()] *= production_throttle_mult(
            faction.stock[Good::Energy.index()],
            energy_needed_by_chain,
            pot[Good::Energy.index()],
        );

        let stock = &mut faction.stock;

        // Step 2: Food & Energy have no inputs.
        stock[Good::Food.index()] += pot[Good::Food.index()];
        stock[Good::Energy.index()] += pot[Good::Energy.index()];

        // Step 2.5: civilians draw Food and Energy *before* any industrial
        // recipe gets to spend the same Energy stock - this is the
        // structural fix for the Stage 2A defect where industry always
        // drained Energy first and civilian demand was left with an
        // unsatisfiable remainder every tick, regardless of policy.
        let food_shortage = consume(&mut stock[Good::Food.index()], food_need, ration);
        let energy_shortage = consume(&mut stock[Good::Energy.index()], energy_need, ration);

        // Step 3: Steel, Machinery and Munitions all contend for the same
        // Energy stock, so it is split into a per-good budget by
        // `industry_priority` up front (falling back to an even three-way
        // split if every weight is zero) - exactly the mechanism step 4
        // below already used to split Steel between Machinery and
        // Munitions, now reused instead of letting Steel take an
        // unconditional first claim. Each good's budget is a fixed share of
        // the Energy stock as it stood before any of the three produced
        // anything this tick, so one good leaving its budget unspent
        // (because its own potential is the binding constraint) doesn't
        // hand the rest to another good this tick — the same
        // no-redistribution behaviour step 4 already relies on for Steel.
        // (`w_steel`/`w_machinery`/`w_munitions`/`energy_share_*`/
        // `share_machinery`/`share_munitions` are computed once, above
        // Step 1.5, so its own feasibility estimate can use the identical
        // shares - see that computation's own doc.)
        let energy_stock = stock[Good::Energy.index()];
        let energy_budget_steel = energy_stock * energy_share_steel;
        let energy_budget_machinery = energy_stock * energy_share_machinery;
        let energy_budget_munitions = energy_stock * energy_share_munitions;

        let actual_steel = pot[Good::Steel.index()]
            .min(input_limit(energy_budget_steel, STEEL_INPUT_ENERGY))
            .max(0.0);
        stock[Good::Steel.index()] += actual_steel;

        // Step 4: Machinery & Munitions share Steel output (produced just
        // above) and their own Energy budget from step 3. The shared Steel
        // input is split into a per-good budget by `industry_priority`
        // (falling back to an even split if both weights are zero), then
        // each good is capped by its own potential and by what its share of
        // each input can fund. This isn't a full water-filling solver — a
        // good that can't use its whole budget (because its own potential
        // is the binding constraint) doesn't hand the rest to the other
        // good this tick — but it is deterministic, keeps the shared input
        // conserved, and makes `industry_priority`'s ratio directly control
        // how the contended input is split, which is all Stage 2A asks for.
        let steel_stock = stock[Good::Steel.index()];
        let steel_budget_machinery = steel_stock * share_machinery;
        let steel_budget_munitions = steel_stock * share_munitions;

        let actual_machinery = pot[Good::Machinery.index()]
            .min(input_limit(steel_budget_machinery, MACHINERY_INPUT_STEEL))
            .min(input_limit(energy_budget_machinery, MACHINERY_INPUT_ENERGY))
            .max(0.0);
        // Stage 3A (docs/phase3-spec.md "生産（Machinery）が好調"): how much
        // of what was actually achievable this tick (`pot[Machinery]`,
        // already net of the stability/strike/regime-change/capital-flight
        // multipliers above) the input-constrained chain actually delivered
        // - `politics::tick_politics` reads this for Business's group-support
        // target. `0.0` (not "no signal") when there's no potential to speak
        // of, since a faction producing nothing has nothing to feel good about.
        faction.machinery_output_ratio = if pot[Good::Machinery.index()] > 0.0 {
            (actual_machinery / pot[Good::Machinery.index()]).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // Phase 12 (`Faction::machinery_output`'s own doc): the same
        // `actual_machinery` figure, kept in absolute form for
        // `research::tick_research` to read - see that field's doc for why
        // the ratio above can't stand in for it.
        faction.machinery_output = actual_machinery;
        let actual_munitions = pot[Good::Munitions.index()]
            .min(input_limit(steel_budget_munitions, MUNITIONS_INPUT_STEEL))
            .min(input_limit(energy_budget_munitions, MUNITIONS_INPUT_ENERGY))
            .max(0.0);

        let steel_used = actual_machinery * MACHINERY_INPUT_STEEL + actual_munitions * MUNITIONS_INPUT_STEEL;
        let energy_used = actual_steel * STEEL_INPUT_ENERGY
            + actual_machinery * MACHINERY_INPUT_ENERGY
            + actual_munitions * MUNITIONS_INPUT_ENERGY;
        stock[Good::Steel.index()] = (steel_stock - steel_used).max(0.0);
        stock[Good::Energy.index()] = (energy_stock - energy_used).max(0.0);
        stock[Good::Machinery.index()] += actual_machinery;
        stock[Good::Munitions.index()] += actual_munitions;

        // Step 4.5: civilians draw Machinery from what industry just
        // produced, before Infantry equipment (the last, lowest-priority
        // consumer in the chain) gets to spend it.
        let machinery_shortage = consume(&mut stock[Good::Machinery.index()], machinery_need, ration);

        // Step 5: Infantry equipment (Stage 11A's renamed `Arms` - see
        // `good::Good`'s own doc), capped by what's left of the Machinery
        // and Steel stock. Unchanged from the pre-Stage-11A `Arms` formula:
        // same inputs, same order, same values - `Good::Infantry` occupies
        // the exact slot `Good::Arms` used to.
        let actual_infantry = pot[Good::Infantry.index()]
            .min(input_limit(stock[Good::Machinery.index()], INFANTRY_INPUT_MACHINERY))
            .min(input_limit(stock[Good::Steel.index()], INFANTRY_INPUT_STEEL))
            .max(0.0);
        stock[Good::Machinery.index()] =
            (stock[Good::Machinery.index()] - actual_infantry * INFANTRY_INPUT_MACHINERY).max(0.0);
        stock[Good::Steel.index()] =
            (stock[Good::Steel.index()] - actual_infantry * INFANTRY_INPUT_STEEL).max(0.0);
        stock[Good::Infantry.index()] += actual_infantry;

        // Step 5.5 (Stage 11A, docs/phase11-spec.md §3): `Armour` and
        // `Artillery` are genuinely new commodities with region-varying
        // capacity, but no unit type draws on either yet ("部隊種別は入れ
        // ない" - Stage 11B adds that). Produced straight from capacity, the
        // same way `Food`/`Energy` are (Step 2 above) - no Machinery/Steel
        // input recipe - specifically so their introduction cannot change
        // what `Infantry` (or anything upstream of it) computes above: a
        // recipe sharing Steel/Machinery with `Infantry` would silently
        // shrink Infantry's own draw the moment these goods got any nonzero
        // capacity, which is exactly the kind of one-name-two-behaviors leak
        // Stage 11A's own acceptance bar ("3 シナリオの結果が変わらない")
        // exists to catch. A realistic recipe belongs in Stage 11B, once a
        // real unit type creates real demand to balance it against.
        stock[Good::Armour.index()] += pot[Good::Armour.index()];
        stock[Good::Artillery.index()] += pot[Good::Artillery.index()];

        // Stage 11B (docs/phase11-spec.md §2 "海軍と航空の装備"): `Naval`/
        // `Aircraft` get the exact same no-recipe treatment as `Armour`/
        // `Artillery` just above - produced straight from region capacity,
        // with no Machinery/Steel input to contend with `Infantry` over.
        // Sea/Air recruits now draw these instead of `Good::Infantry`
        // (`action::apply_recruit`), so unlike Armour/Artillery's Stage 11A
        // introduction this *does* change existing scenarios' outcomes -
        // that's this stage's entire point (the single-pool measurement
        // `docs/future-work.md` records), not a regression to guard against.
        stock[Good::Naval.index()] += pot[Good::Naval.index()];
        stock[Good::Aircraft.index()] += pot[Good::Aircraft.index()];

        faction.shortage = food_shortage.max(energy_shortage).max(machinery_shortage);
        // External code review fix (Stage 2C): keep each commodity's own
        // shortage alongside the collapsed worst-of-three scalar above, so
        // a consumer that cares which good is actually short (Stage 2C's
        // import planner) doesn't have to guess from the aggregate.
        faction.shortage_by_good = [0.0; GOOD_COUNT];
        faction.shortage_by_good[Good::Food.index()] = food_shortage;
        faction.shortage_by_good[Good::Energy.index()] = energy_shortage;
        faction.shortage_by_good[Good::Machinery.index()] = machinery_shortage;

        // Demobilization: conscripts sitting idle in the pool (drafted but
        // not assigned to a unit) trickle back to the civilian workforce
        // daily, proportional to the pool's size. `conscription` only
        // throttles inflow, so without this the pool is a one-way
        // accumulator - this is the outflow that lets it settle at a
        // finite level instead. This never touches population or unit
        // manpower; it only shrinks the pool that `region.mobilized` counts
        // below, so the labour is back in `labor_ratio` next tick.
        faction.manpower -= faction.manpower * MANPOWER_DEMOBILIZATION_RATE;

        draft[f] = total_pop[f] * CONSCRIPT_RATE * faction.conscription;
        faction.manpower += draft[f];
    }

    // `region.mobilized` reflects the faction's manpower CURRENTLY under
    // arms (the barracks pool plus every living unit's manpower), not a
    // ratchet of everything ever drafted - casualties and disbanded units
    // must give labour capacity back.
    let mut committed = vec![0.0f32; n_factions];
    for faction in &world.factions {
        committed[faction.id.index()] += faction.manpower;
    }
    for unit in &world.units {
        if unit.alive {
            committed[unit.owner.index()] += unit.manpower;
        }
    }
    for region in world.regions.iter_mut() {
        let f = region.owner.index();
        region.mobilized = if total_pop[f] > 0.0 {
            committed[f] * (region.population / total_pop[f])
        } else {
            0.0
        };
    }
}
