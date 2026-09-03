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
//! 4. `Steel` is capped by what's left of the `Energy` stock.
//! 5. `Machinery` and `Munitions` both draw on `Steel` and what's left of
//!    `Energy` stock; when the shared input can't cover both at their full
//!    potential, it is split between them by `Faction::industry_priority`.
//! 6. Civilians draw `Machinery` from what industry left behind, folding
//!    into `Faction::shortage` the same way.
//! 7. `Arms` is capped by what's left of the `Machinery` and `Steel` stock.

use crate::balance::{
    ARMS_INPUT_MACHINERY, ARMS_INPUT_STEEL, CIVILIAN_ENERGY_DEMAND_PER_POP,
    CIVILIAN_FOOD_DEMAND_PER_POP, CIVILIAN_MACHINERY_DEMAND_PER_POP, CONSCRIPT_RATE,
    MACHINERY_INPUT_ENERGY, MACHINERY_INPUT_STEEL, MANPOWER_DEMOBILIZATION_RATE,
    MUNITIONS_INPUT_ENERGY, MUNITIONS_INPUT_STEEL, STEEL_INPUT_ENERGY,
};
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

pub fn tick_economy(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    // Step 0: per-region efficiency, shared across every commodity (Phase 1
    // §4.1's formula, now applied uniformly instead of Food having its own).
    let mut efficiency = vec![0.0f32; n_regions];
    for (i, region) in world.regions.iter().enumerate() {
        efficiency[i] = (region.infrastructure.max(0.2)
            * region.labor_ratio()
            * (1.0 - region.unrest / 150.0))
            .clamp(0.2, 1.0);
    }

    // Step 1: potential[f][g] = capacity-weighted output before any input
    // constraint, summed over each faction's owned regions in region-index
    // order (fixed accumulation order for determinism).
    let mut potential = vec![[0.0f32; GOOD_COUNT]; n_factions];
    let mut total_pop = vec![0.0f32; n_factions];
    for region in &world.regions {
        let f = region.owner.index();
        total_pop[f] += region.population;
        let e = efficiency[region.id.index()];
        for good in ALL_GOODS {
            potential[f][good.index()] += region.capacity[good.index()] * e;
        }
    }

    let mut draft = vec![0.0f32; n_factions];
    for faction in world.factions.iter_mut() {
        if !faction.alive {
            continue;
        }
        let f = faction.id.index();

        let stability_mult = 0.6 + 0.4 * (faction.stability / 100.0);
        let mut pot = potential[f];
        for v in pot.iter_mut() {
            *v *= stability_mult;
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

        // Step 3: Steel, capped by what's left of the Energy stock.
        let actual_steel = pot[Good::Steel.index()]
            .min(input_limit(stock[Good::Energy.index()], STEEL_INPUT_ENERGY))
            .max(0.0);
        stock[Good::Energy.index()] =
            (stock[Good::Energy.index()] - actual_steel * STEEL_INPUT_ENERGY).max(0.0);
        stock[Good::Steel.index()] += actual_steel;

        // Step 4: Machinery & Munitions share Steel and Energy. The shared
        // input is split into a per-good budget by `industry_priority`
        // (falling back to an even split if both weights are zero), then
        // each good is capped by its own potential and by what its share of
        // each input can fund. This isn't a full water-filling solver — a
        // good that can't use its whole budget (because its own potential
        // is the binding constraint) doesn't hand the rest to the other
        // good this tick — but it is deterministic, keeps the shared input
        // conserved, and makes `industry_priority`'s ratio directly control
        // how the contended input is split, which is all Stage 2A asks for.
        let w_machinery = faction.industry_priority[Good::Machinery.index()].max(0.0);
        let w_munitions = faction.industry_priority[Good::Munitions.index()].max(0.0);
        let weight_sum = w_machinery + w_munitions;
        let (share_machinery, share_munitions) = if weight_sum > 0.0 {
            (w_machinery / weight_sum, w_munitions / weight_sum)
        } else {
            (0.5, 0.5)
        };

        let steel_stock = stock[Good::Steel.index()];
        let energy_stock = stock[Good::Energy.index()];
        let steel_budget_machinery = steel_stock * share_machinery;
        let steel_budget_munitions = steel_stock * share_munitions;
        let energy_budget_machinery = energy_stock * share_machinery;
        let energy_budget_munitions = energy_stock * share_munitions;

        let actual_machinery = pot[Good::Machinery.index()]
            .min(input_limit(steel_budget_machinery, MACHINERY_INPUT_STEEL))
            .min(input_limit(energy_budget_machinery, MACHINERY_INPUT_ENERGY))
            .max(0.0);
        let actual_munitions = pot[Good::Munitions.index()]
            .min(input_limit(steel_budget_munitions, MUNITIONS_INPUT_STEEL))
            .min(input_limit(energy_budget_munitions, MUNITIONS_INPUT_ENERGY))
            .max(0.0);

        let steel_used = actual_machinery * MACHINERY_INPUT_STEEL + actual_munitions * MUNITIONS_INPUT_STEEL;
        let energy_used =
            actual_machinery * MACHINERY_INPUT_ENERGY + actual_munitions * MUNITIONS_INPUT_ENERGY;
        stock[Good::Steel.index()] = (steel_stock - steel_used).max(0.0);
        stock[Good::Energy.index()] = (energy_stock - energy_used).max(0.0);
        stock[Good::Machinery.index()] += actual_machinery;
        stock[Good::Munitions.index()] += actual_munitions;

        // Step 4.5: civilians draw Machinery from what industry just
        // produced, before Arms (the last, lowest-priority consumer in the
        // chain) gets to spend it.
        let machinery_shortage = consume(&mut stock[Good::Machinery.index()], machinery_need, ration);

        // Step 5: Arms, capped by what's left of the Machinery and Steel stock.
        let actual_arms = pot[Good::Arms.index()]
            .min(input_limit(stock[Good::Machinery.index()], ARMS_INPUT_MACHINERY))
            .min(input_limit(stock[Good::Steel.index()], ARMS_INPUT_STEEL))
            .max(0.0);
        stock[Good::Machinery.index()] =
            (stock[Good::Machinery.index()] - actual_arms * ARMS_INPUT_MACHINERY).max(0.0);
        stock[Good::Steel.index()] =
            (stock[Good::Steel.index()] - actual_arms * ARMS_INPUT_STEEL).max(0.0);
        stock[Good::Arms.index()] += actual_arms;

        faction.shortage = food_shortage.max(energy_shortage).max(machinery_shortage);

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
