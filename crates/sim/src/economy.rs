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
//! 2. `Food`/`Energy` have no inputs, so they're produced in full and
//!    added straight to stock.
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
//! 8. `Armour`/`Artillery` (Stage 11A, no unit type draws either yet) are
//!    produced straight from capacity like `Food`/`Energy` - no recipe, no
//!    interaction with any step above.

use crate::balance::{
    INFANTRY_INPUT_MACHINERY, INFANTRY_INPUT_STEEL, CAPITAL_FLIGHT_MACHINERY_MULT,
    CIVILIAN_ENERGY_DEMAND_PER_POP, CIVILIAN_FOOD_DEMAND_PER_POP, CIVILIAN_MACHINERY_DEMAND_PER_POP,
    CONSCRIPT_RATE, FOCUS_TECHNOCRACY_PRODUCTION_MULT, FOOD_EFFICIENCY_DAMPENING,
    FOOD_EFFICIENCY_FLOOR, INDUSTRIAL_STABILITY_FLOOR, MACHINERY_INPUT_ENERGY, MACHINERY_INPUT_STEEL,
    MANPOWER_DEMOBILIZATION_RATE, MUNITIONS_INPUT_ENERGY, MUNITIONS_INPUT_STEEL,
    REGIME_CHANGE_OUTPUT_MULT, STEEL_INPUT_ENERGY, STRIKE_OUTPUT_MULT,
};
use crate::focus::{self, NationalFocus};
use crate::good::{Good, ALL_GOODS, GOOD_COUNT};
use crate::world::World;

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
    for region in &world.regions {
        let f = region.owner.index();
        total_pop[f] += region.population;
        let e = efficiency[region.id.index()];
        let food_e = food_efficiency[region.id.index()];
        for good in ALL_GOODS {
            let mult = if good == Good::Food { food_e } else { e };
            potential[f][good.index()] += region.effective_capacity(good) * mult;
        }
    }

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

        let ration = faction.civilian_ration;
        let food_need = total_pop[f] * CIVILIAN_FOOD_DEMAND_PER_POP;
        let energy_need = total_pop[f] * CIVILIAN_ENERGY_DEMAND_PER_POP;
        let machinery_need = total_pop[f] * CIVILIAN_MACHINERY_DEMAND_PER_POP;

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
        let steel_weight_sum = w_machinery + w_munitions;
        let (share_machinery, share_munitions) = if steel_weight_sum > 0.0 {
            (w_machinery / steel_weight_sum, w_munitions / steel_weight_sum)
        } else {
            (0.5, 0.5)
        };

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
