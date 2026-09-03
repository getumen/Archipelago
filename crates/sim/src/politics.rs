//! Unrest, national stability, and war support — the political feedback
//! loop that eventually throttles a faction's economy (see economy.rs).

use crate::balance::{
    OCCUPIED_UNREST_FLOOR, STABILITY_ADAPT_RATE, UNREST_ADAPT_RATE, UNREST_SHORTAGE_PRESSURE,
    UNREST_SUPPLY_PRESSURE, WAR_SUPPORT_CASUALTY_MULT, WAR_SUPPORT_DRIFT,
};
use crate::world::World;

/// `day_casualties` is indexed like `World::factions`: manpower lost this tick.
pub fn tick_politics(world: &mut World, day_casualties: &[f32]) {
    let shortage: Vec<f32> = world.factions.iter().map(|f| f.shortage).collect();
    let supply_ratio: Vec<f32> = world.factions.iter().map(|f| f.supply_ratio).collect();

    for region in world.regions.iter_mut() {
        let floor = if region.core == region.owner {
            0.0
        } else {
            OCCUPIED_UNREST_FLOOR
        };
        let f = region.owner.index();
        let pressure = UNREST_SHORTAGE_PRESSURE * shortage[f]
            + UNREST_SUPPLY_PRESSURE * (1.0 - supply_ratio[f].clamp(0.0, 1.0));
        let target = (floor + pressure).min(100.0);
        region.unrest += (target - region.unrest) * UNREST_ADAPT_RATE;
        region.unrest = region.unrest.clamp(0.0, 100.0);
    }

    let n = world.factions.len();
    let mut avg_unrest = vec![0.0f32; n];
    let mut region_count = vec![0u32; n];
    for region in &world.regions {
        let f = region.owner.index();
        avg_unrest[f] += region.unrest;
        region_count[f] += 1;
    }
    for f in 0..n {
        if region_count[f] > 0 {
            avg_unrest[f] /= region_count[f] as f32;
        }
    }

    for faction in world.factions.iter_mut() {
        let f = faction.id.index();

        let target = 100.0 - avg_unrest[f] * 0.6 - faction.shortage * 40.0;
        faction.stability =
            (faction.stability + (target - faction.stability) * STABILITY_ADAPT_RATE)
                .clamp(0.0, 100.0);

        faction.war_support -= day_casualties[f] * WAR_SUPPORT_CASUALTY_MULT;
        if faction.war_support > 50.0 {
            faction.war_support = (faction.war_support - WAR_SUPPORT_DRIFT).max(50.0);
        } else if faction.war_support < 50.0 {
            faction.war_support = (faction.war_support + WAR_SUPPORT_DRIFT).min(50.0);
        }
        faction.war_support = faction.war_support.clamp(0.0, 100.0);
    }
}
