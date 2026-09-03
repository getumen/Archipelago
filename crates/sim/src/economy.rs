//! Daily production, civilian/food consumption, and conscription.
//!
//! This is the labor/production trade-off from design.md §9: drafting
//! manpower reduces `region.labor_ratio`, which lowers industrial output.

use crate::balance::{
    CIVILIAN_DEMAND_PER_POP, CONSCRIPT_RATE, FOOD_DEMAND_PER_POP, FOOD_OUTPUT_PER_POINT,
    INDUSTRY_OUTPUT_PER_POINT,
};
use crate::world::World;

pub fn tick_economy(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let mut industry_output = vec![0.0f32; n_regions];
    let mut food_output = vec![0.0f32; n_regions];
    for (i, region) in world.regions.iter().enumerate() {
        let efficiency = (region.infrastructure.max(0.2)
            * region.labor_ratio()
            * (1.0 - region.unrest / 150.0))
            .clamp(0.2, 1.0);
        industry_output[i] = region.industry * INDUSTRY_OUTPUT_PER_POINT * efficiency;
        food_output[i] =
            region.food * FOOD_OUTPUT_PER_POINT * (0.5 + 0.5 * region.labor_ratio());
    }

    let mut total_output = vec![0.0f32; n_factions];
    let mut total_food = vec![0.0f32; n_factions];
    let mut total_pop = vec![0.0f32; n_factions];
    for (i, region) in world.regions.iter().enumerate() {
        let f = region.owner.index();
        total_output[f] += industry_output[i];
        total_food[f] += food_output[i];
        total_pop[f] += region.population;
    }

    let mut draft = vec![0.0f32; n_factions];
    for faction in world.factions.iter_mut() {
        if !faction.alive {
            continue;
        }
        let f = faction.id.index();
        let stability_mult = 0.6 + 0.4 * (faction.stability / 100.0);
        let out = total_output[f] * stability_mult;
        let civ_need = total_pop[f] * CIVILIAN_DEMAND_PER_POP;
        let civ = out.min(civ_need);
        let military = out - civ;
        faction.equipment += military * faction.production_mix;
        faction.supplies += military * (1.0 - faction.production_mix);

        let civ_shortage = if civ_need > 0.0 {
            ((civ_need - civ) / civ_need).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let food_need = total_pop[f] * FOOD_DEMAND_PER_POP;
        let food_shortage = if food_need > 0.0 {
            ((food_need - total_food[f]) / food_need).clamp(0.0, 1.0)
        } else {
            0.0
        };
        faction.shortage = civ_shortage.max(food_shortage);

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
