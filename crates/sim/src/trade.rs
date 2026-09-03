//! Stage 2C sea imports (docs/phase2-spec.md "Stage 2C — 品目別物流・港湾容量・
//! 海上輸入", "1. 海上輸入"): the Stage 2A playtest found that a faction
//! holding the nation's industrial heartland cannot feed its own population
//! at any efficiency — the islands' urban core is not food self-sufficient,
//! and until now there was no way to bring food in from outside the map.
//!
//! Runs before `economy::tick_economy` in `Simulation::step`, so imported
//! Food/Energy is in stock in time for that same day's civilian ration —
//! the whole point is that imports feed civilians, not that they arrive a
//! day late. Paid for out of *yesterday's* Machinery stock (whatever
//! `tick_economy`/`construction` left behind last tick), the same way every
//! other consumer in the pipeline draws on stock as it stood when its turn
//! came up.
//!
//! Per the spec's five-step procedure:
//! 1. Compute each owned, uncontested port's capacity and this faction's total.
//! 2. Cap the faction's total desired Food+Energy import volume at that total.
//! 3. Cap it again by what the faction's Machinery stock can pay for.
//! 4. Add the actual volume (split across Food/Energy in the plan's ratio)
//!    to stock, and deduct the Machinery cost.
//! 5. Record each port's `import_flow` as the actual volume apportioned by
//!    that port's share of the faction's total capacity.
//!
//! Capacity is kept per port region throughout, never collapsed into one
//! national number — Stage 2D blockades individual ports, and only a
//! per-port figure can express that a blockade of one port doesn't touch
//! another.

use crate::balance::{IMPORT_COST_MACHINERY_PER_GOOD, IMPORT_PER_PORT};
use crate::good::Good;
use crate::naval;
use crate::world::World;

pub fn tick_imports(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let contested: Vec<bool> = (0..n_regions)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();

    for region in world.regions.iter_mut() {
        region.import_flow = 0.0;
    }

    // Step 1: per-port capacity (own, uncontested regions only), and each
    // faction's total across its own ports.
    let mut port_capacity = vec![0.0f32; n_regions];
    let mut total_capacity = vec![0.0f32; n_factions];
    for i in 0..n_regions {
        // Stage 2D (docs/phase2-spec.md "2. 港の封鎖"): a blockaded port
        // imports nothing, independent of (and in addition to) land contest
        // — judged per port, so a blockade of one port never touches
        // another's `import_flow`.
        if contested[i] || naval::is_port_blockaded(world, world.regions[i].id) {
            continue;
        }
        let region = &world.regions[i];
        let cap = region.port * IMPORT_PER_PORT * (1.0 - region.devastation);
        if cap > 0.0 {
            port_capacity[i] = cap;
            total_capacity[region.owner.index()] += cap;
        }
    }

    for f in 0..n_factions {
        if !world.factions[f].alive || total_capacity[f] <= 0.0 {
            continue;
        }

        let desired_food = world.factions[f].import_plan[Good::Food.index()].max(0.0);
        let desired_energy = world.factions[f].import_plan[Good::Energy.index()].max(0.0);
        let desired_total = desired_food + desired_energy;
        if desired_total <= 0.0 {
            continue;
        }

        // Step 2: cap the combined request at total port capacity.
        let capacity_limited = desired_total.min(total_capacity[f]);

        // Step 3: cap further by what Machinery can pay for.
        let machinery_stock = world.factions[f].stock[Good::Machinery.index()];
        let affordable = if IMPORT_COST_MACHINERY_PER_GOOD > 0.0 {
            machinery_stock / IMPORT_COST_MACHINERY_PER_GOOD
        } else {
            f32::INFINITY
        };
        let actual_total = capacity_limited.min(affordable).max(0.0);
        if actual_total <= 0.0 {
            continue;
        }

        // Step 4: split the actual volume across goods in the plan's ratio,
        // credit stock, and pay for it.
        let scale = actual_total / desired_total;
        let actual_food = desired_food * scale;
        let actual_energy = desired_energy * scale;

        let faction = &mut world.factions[f];
        faction.stock[Good::Food.index()] += actual_food;
        faction.stock[Good::Energy.index()] += actual_energy;
        faction.stock[Good::Machinery.index()] =
            (machinery_stock - actual_total * IMPORT_COST_MACHINERY_PER_GOOD).max(0.0);

        // Step 5: apportion the flow across this faction's ports by each
        // port's share of the faction's total capacity.
        for i in 0..n_regions {
            if world.regions[i].owner.index() == f && port_capacity[i] > 0.0 {
                world.regions[i].import_flow = actual_total * (port_capacity[i] / total_capacity[f]);
            }
        }
    }
}
