//! Spec §9 acceptance tests for the simulation core.

use crate::action::{self, Action, ActionError};
use crate::balance::{
    CIVILIAN_ENERGY_DEMAND_PER_POP, CONSTRUCTION_MACHINERY_PER_POINT, CONSTRUCTION_RATE,
    CONSTRUCTION_REQUIRED_CAPACITY, CONSTRUCTION_STEEL_PER_POINT, IMPORT_PER_PORT, OCCUPATION_RATE,
    UNIT_DEATH_MANPOWER, UNIT_EQUIPMENT,
};
use crate::construction::{self, Construction, Project};
use crate::economy;
use crate::good::{Good, GOOD_COUNT};
use crate::ids::{FactionId, RegionId, SeaZoneId};
use crate::logistics;
use crate::military;
use crate::naval;
use crate::politics;
use crate::rng::Rng;
use crate::scenario;
use crate::sim::Simulation;
use crate::trade;
use crate::world::{Domain, Station};

#[test]
fn supply_corridor_cut() {
    let mut world = scenario::build_world();
    logistics::recompute_supply(&mut world);
    let before = world.supply[RegionId(1).index()];

    // Region 2 is the only corridor between region 1 and the industrial
    // heartland at region 3; handing it to another faction should starve
    // region 1's relayed supply.
    world.region_mut(RegionId(2)).owner = FactionId(1);
    logistics::recompute_supply(&mut world);
    let after = world.supply[RegionId(1).index()];

    assert!(
        after < before * 0.7,
        "expected corridor cut to reduce supply: before={before}, after={after}"
    );
}

#[test]
fn combat_reduces_organization() {
    let mut world = scenario::build_world();
    let intruder = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(1))
        .unwrap();
    world.units[intruder].station = Station::Region(RegionId(3));
    world.units[intruder].movement = None;

    let org_before: Vec<f32> = world.units.iter().map(|u| u.organization).collect();

    let mut rng = Rng::new(1);
    let mut events = Vec::new();
    let report = military::tick_combat(&mut world, &mut rng, &mut events);

    assert!(report.fought[intruder]);
    assert!(world.units[intruder].organization < org_before[intruder]);

    let defender = world
        .units
        .iter()
        .find(|u| u.owner == FactionId(0) && u.station == Station::Region(RegionId(3)))
        .unwrap();
    assert!(defender.organization < org_before[defender.id.index()]);
}

#[test]
fn occupation_flips_owner() {
    let mut world = scenario::build_world();
    // Region 8 (Shikoku) starts undefended by faction 2; put a lone
    // faction-0 unit there and let occupation run.
    let mover = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(0))
        .unwrap();
    world.units[mover].station = Station::Region(RegionId(8));
    world.units[mover].movement = None;

    let mut events = Vec::new();
    for _ in 0..3 {
        military::tick_occupation(&mut world, &mut events);
    }
    assert_eq!(world.region(RegionId(8)).owner, FactionId(2));

    military::tick_occupation(&mut world, &mut events);
    assert_eq!(world.region(RegionId(8)).owner, FactionId(0));
}

#[test]
fn determinism() {
    let mut sim_a = Simulation::new(7);
    let mut sim_b = Simulation::new(7);
    for _ in 0..200 {
        sim_a.step();
        sim_b.step();
    }

    let owners_a: Vec<_> = sim_a.world.regions.iter().map(|r| r.owner).collect();
    let owners_b: Vec<_> = sim_b.world.regions.iter().map(|r| r.owner).collect();
    assert_eq!(owners_a, owners_b);

    for (fa, fb) in sim_a.world.factions.iter().zip(sim_b.world.factions.iter()) {
        assert_eq!(fa.manpower, fb.manpower);
        assert_eq!(fa.stock, fb.stock);
        assert_eq!(fa.war_support, fb.war_support);
        assert_eq!(fa.stability, fb.stability);
        assert_eq!(fa.alive, fb.alive);
    }
}

#[test]
fn invalid_action_rejected() {
    let mut sim = Simulation::new(1);
    let unit_id = sim
        .world
        .units
        .iter()
        .find(|u| u.owner == FactionId(0))
        .unwrap()
        .id;
    let before_station = sim.world.unit(unit_id).station;
    let before_movement = sim.world.unit(unit_id).movement;

    // Region 9 (Kyushu) is nowhere near faction 0's units.
    let errors = sim.apply(
        FactionId(0),
        &[Action::MoveUnit {
            unit: unit_id,
            to: Station::Region(RegionId(9)),
        }],
    );

    assert_eq!(errors, vec![ActionError::NotAdjacent]);
    assert_eq!(sim.world.unit(unit_id).station, before_station);
    assert_eq!(sim.world.unit(unit_id).movement, before_movement);
}

#[test]
fn unrest_recovers_after_shortage() {
    let mut world = scenario::build_world();
    let capital = world.faction(FactionId(0)).capital;
    // Simulate a region driven to maximum unrest by past shortage/supply
    // pressure. Faction 0's own `shortage`/`supply_ratio` are already at
    // their benign defaults (0.0 / 1.0), so once that pressure is gone the
    // region should relax back toward its (core, so zero) floor.
    world.region_mut(capital).unrest = 100.0;

    let casualties = vec![0.0; world.factions.len()];
    for _ in 0..80 {
        politics::tick_politics(&mut world, &casualties);
    }

    let unrest = world.region(capital).unrest;
    assert!(
        unrest < 5.0,
        "expected unrest to recover toward its floor once pressure ends, got {unrest}"
    );
}

#[test]
fn occupation_resets_on_occupier_change() {
    let mut world = scenario::build_world();
    // Region 8 (Shikoku) starts undefended by its owner, faction 2.
    let region_id = RegionId(8);

    let mover0 = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(0))
        .unwrap();
    world.units[mover0].station = Station::Region(region_id);
    world.units[mover0].movement = None;

    let mut events = Vec::new();
    military::tick_occupation(&mut world, &mut events);
    assert_eq!(world.region(region_id).occupier, Some(FactionId(0)));
    assert_eq!(world.region(region_id).occupation, OCCUPATION_RATE);

    // Faction 0's unit leaves and a faction-1 unit takes its place: the
    // occupier changes, so the progress faction 0 earned must be discarded
    // rather than letting faction 1 finish the capture with a head start.
    world.units[mover0].station = Station::Region(RegionId(0));
    let mover1 = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(1))
        .unwrap();
    world.units[mover1].station = Station::Region(region_id);
    world.units[mover1].movement = None;

    military::tick_occupation(&mut world, &mut events);
    assert_eq!(world.region(region_id).occupier, Some(FactionId(1)));
    assert_eq!(
        world.region(region_id).occupation,
        OCCUPATION_RATE,
        "occupation progress must reset to 0 before faction 1's tick of progress is added"
    );
}

#[test]
fn depleted_unit_is_destroyed_not_retreating() {
    let mut world = scenario::build_world();
    let region_id = world.faction(FactionId(0)).capital;

    let unit_idx = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(0))
        .unwrap();
    world.units[unit_idx].station = Station::Region(region_id);
    world.units[unit_idx].movement = None;
    world.units[unit_idx].organization = 0.0;
    world.units[unit_idx].manpower = UNIT_DEATH_MANPOWER;

    // An enemy unit shares the region, so - absent the fix - the old
    // "organization <= 0 && enemy present" branch would route this unit to
    // retreat instead of destroying it outright.
    let enemy_idx = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(1))
        .unwrap();
    world.units[enemy_idx].station = Station::Region(region_id);
    world.units[enemy_idx].movement = None;

    let fought = vec![false; world.units.len()];
    let mut events = Vec::new();
    military::tick_recovery(&mut world, &fought, &mut events);

    assert!(
        !world.units[unit_idx].alive,
        "a unit at/below the death threshold must be destroyed, not routed"
    );
    assert!(world.units[unit_idx].movement.is_none());
}

#[test]
fn mobilized_tracks_current_commitment() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    world.faction_mut(faction).manpower = 20.0;
    economy::tick_economy(&mut world);
    let mobilized_high = world.region(capital).mobilized;
    assert!(mobilized_high > 0.0);

    // Committed manpower falls (units lost, pool spent) - `mobilized` must
    // follow it back down instead of ratcheting upward forever.
    world.faction_mut(faction).manpower = 2.0;
    economy::tick_economy(&mut world);
    let mobilized_low = world.region(capital).mobilized;

    assert!(
        mobilized_low < mobilized_high,
        "expected mobilized to fall with commitment: before={mobilized_high}, after={mobilized_low}"
    );
}

/// The manpower pool must be a self-correcting reservoir, not a one-way
/// accumulator: a faction drafting continuously (`conscription = 1.0`) off
/// a fixed population, with nothing ever spending the pool (no units
/// recruited), should converge to a finite level instead of growing
/// forever. `economy::tick_economy` alone drives this - it's the
/// draft-in/demobilize-out balance, independent of anything military or
/// political - so calling it directly in a loop isolates exactly that.
#[test]
fn manpower_pool_does_not_grow_without_bound() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    world.faction_mut(faction).conscription = 1.0;

    for _ in 0..1000 {
        economy::tick_economy(&mut world);
    }
    let settled = world.faction(faction).manpower;

    for _ in 0..500 {
        economy::tick_economy(&mut world);
    }
    let later = world.faction(faction).manpower;

    assert!(
        settled < 500.0,
        "pool should have converged to a finite level, not ballooned: {settled}"
    );
    assert!(
        (later - settled).abs() < 0.01,
        "pool should have stopped growing by day 1000, not still be climbing: settled={settled}, later={later}"
    );
}

/// Once a faction stops feeding the pool, the idle conscripts in it must
/// drain back into the civilian workforce and be reflected in
/// `labor_ratio` - proving the pool isn't just capped but genuinely gives
/// labour back, per docs/mvp-spec.md §4.1's "動員解除・損耗で戻る".
#[test]
fn idle_conscripts_return_to_workforce() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    // A pool this large, relative to the capital's workforce, pins
    // labor_ratio at its floor the moment it's counted as mobilized.
    world.faction_mut(faction).manpower = 3000.0;
    world.faction_mut(faction).conscription = 0.0;
    economy::tick_economy(&mut world);
    let ratio_depressed = world.region(capital).labor_ratio();
    assert!(
        ratio_depressed <= 0.16,
        "expected the oversized pool to pin labor_ratio near its floor: {ratio_depressed}"
    );

    // With conscription at 0, nothing refills the pool, so demobilization
    // alone should drain it back out and let labor_ratio recover.
    for _ in 0..400 {
        economy::tick_economy(&mut world);
    }
    let ratio_recovered = world.region(capital).labor_ratio();

    assert!(
        ratio_recovered > ratio_depressed + 0.3,
        "expected labor_ratio to recover once the pool drained: before={ratio_depressed}, after={ratio_recovered}"
    );
}

#[test]
fn conscription_reduces_labor() {
    let mut sim = Simulation::new(3);
    let errors = sim.apply(FactionId(0), &[Action::SetConscription(1.0)]);
    assert!(errors.is_empty());

    let capital = sim.world.faction(FactionId(0)).capital;
    let ratio_before = sim.world.region(capital).labor_ratio();

    for _ in 0..30 {
        sim.step();
    }

    let ratio_after = sim.world.region(capital).labor_ratio();
    assert!(
        ratio_after < ratio_before,
        "expected labor ratio to fall: before={ratio_before}, after={ratio_after}"
    );
}

#[test]
fn casualties_accumulate_from_combat() {
    let mut sim = Simulation::new(1);

    // Force an overlap the same way `combat_reduces_organization` does:
    // park a faction-1 unit in faction 0's capital, which already holds a
    // faction-0 defender, so the two factions fight there every tick
    // without needing any AI-issued orders.
    let region_id = sim.world.faction(FactionId(0)).capital;
    let intruder = sim
        .world
        .units
        .iter()
        .position(|u| u.owner == FactionId(1))
        .unwrap();
    sim.world.units[intruder].station = Station::Region(region_id);
    sim.world.units[intruder].movement = None;

    assert_eq!(sim.world.faction(FactionId(0)).casualties, 0.0);
    assert_eq!(sim.world.faction(FactionId(1)).casualties, 0.0);

    let mut previous = [0.0f32; 2];
    for _ in 0..30 {
        sim.step();

        // `casualties` is a cumulative counter: it must never fall as the
        // simulation progresses.
        for f in 0..2 {
            let current = sim.world.faction(FactionId(f as u32)).casualties;
            assert!(
                current >= previous[f],
                "faction {f} casualties decreased: before={}, after={current}",
                previous[f]
            );
            previous[f] = current;
        }
    }

    // Both sides of the fight must have taken manpower losses (they were
    // stuck at 0.0 before the fix, despite the fighting above).
    assert!(
        sim.world.faction(FactionId(0)).casualties > 0.0,
        "expected faction 0 to have suffered casualties"
    );
    assert!(
        sim.world.faction(FactionId(1)).casualties > 0.0,
        "expected faction 1 to have suffered casualties"
    );
}

/// Stage 2A acceptance test: losing the nation's Machinery hub (Kanto, the
/// scenario's largest Machinery/Arms capacity by far) should leave Arms
/// production nearly stalled even with Steel and Energy in abundant supply,
/// because Arms production is capped by its own capacity-derived potential
/// and by the Machinery stock it consumes - not by Steel/Energy alone.
#[test]
fn losing_machinery_region_halts_arms() {
    let build = |strip_kanto: bool| {
        let mut world = scenario::build_world();
        if strip_kanto {
            // Region 3 (Kanto) is faction 0's own capital; handing it away
            // simulates losing the country's main Machinery/Arms base.
            world.region_mut(RegionId(3)).owner = FactionId(2);
        }
        let faction = FactionId(0);
        let f = world.faction_mut(faction);
        f.stock = [0.0; GOOD_COUNT];
        f.stock[Good::Steel.index()] = 1000.0;
        f.stock[Good::Energy.index()] = 1000.0;
        f.industry_priority[Good::Machinery.index()] = 0.5;
        f.industry_priority[Good::Munitions.index()] = 0.5;
        world
    };

    let mut with_kanto = build(false);
    economy::tick_economy(&mut with_kanto);
    let arms_with = with_kanto.faction(FactionId(0)).stock[Good::Arms.index()];

    let mut without_kanto = build(true);
    economy::tick_economy(&mut without_kanto);
    let arms_without = without_kanto.faction(FactionId(0)).stock[Good::Arms.index()];

    assert!(
        arms_with > 1.0,
        "expected meaningful Arms output while holding Kanto, got {arms_with}"
    );
    assert!(
        arms_without < arms_with * 0.15,
        "expected losing the Machinery hub to nearly halt Arms production despite \
         ample Steel/Energy: with={arms_with}, without={arms_without}"
    );
}

/// Stage 2A acceptance test: draining a faction's Energy-producing capacity
/// to zero should cascade downstream - Steel needs Energy, and Machinery /
/// Munitions / Arms all sit behind Steel - so every good past Energy stays
/// pinned at zero for the tick.
#[test]
fn input_shortage_limits_output() {
    let mut world = scenario::build_world();
    let faction = FactionId(1); // owns regions 4, 5, 6 with real Steel capacity
    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity[Good::Energy.index()] = 0.0;
        }
    }
    let f = world.faction_mut(faction);
    f.stock = [0.0; GOOD_COUNT];
    f.industry_priority[Good::Machinery.index()] = 0.5;
    f.industry_priority[Good::Munitions.index()] = 0.5;

    economy::tick_economy(&mut world);

    let stock = world.faction(faction).stock;
    assert_eq!(stock[Good::Energy.index()], 0.0, "no Energy capacity means no Energy output");
    assert_eq!(
        stock[Good::Steel.index()],
        0.0,
        "Steel needs Energy input and should stay at zero without it"
    );
    assert_eq!(stock[Good::Machinery.index()], 0.0);
    assert_eq!(stock[Good::Munitions.index()], 0.0);
    assert_eq!(stock[Good::Arms.index()], 0.0);
}

/// Stage 2A acceptance test: when Steel is too scarce to fund both
/// Machinery's and Munitions' full potential, the contended input is split
/// between them in the ratio of `Faction::industry_priority` - raising a
/// good's weight should raise its share of the scarce input (and therefore
/// its output) relative to the other.
#[test]
fn industry_priority_splits_shared_input() {
    let build = |machinery_weight: f32, munitions_weight: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                // Plenty of Machinery/Munitions potential and Energy so
                // Steel - deliberately kept scarce - is the only binding
                // constraint, and Arms consumes nothing (capacity zeroed)
                // so it can't eat into the Machinery this test measures.
                region.capacity[Good::Machinery.index()] = 100.0;
                region.capacity[Good::Munitions.index()] = 100.0;
                region.capacity[Good::Arms.index()] = 0.0;
                region.infrastructure = 1.0;
                region.unrest = 0.0;
            }
        }
        let f = world.faction_mut(faction);
        f.stock = [0.0; GOOD_COUNT];
        f.stock[Good::Steel.index()] = 5.0;
        f.stock[Good::Energy.index()] = 1000.0;
        f.stability = 100.0;
        f.industry_priority[Good::Machinery.index()] = machinery_weight;
        f.industry_priority[Good::Munitions.index()] = munitions_weight;

        economy::tick_economy(&mut world);
        let stock = world.faction(faction).stock;
        (stock[Good::Machinery.index()], stock[Good::Munitions.index()])
    };

    let (machinery_favored_m, machinery_favored_mu) = build(0.8, 0.2);
    let (munitions_favored_m, munitions_favored_mu) = build(0.2, 0.8);

    assert!(
        machinery_favored_m > machinery_favored_mu,
        "with Machinery weighted higher, it should out-produce Munitions: \
         machinery={machinery_favored_m}, munitions={machinery_favored_mu}"
    );
    assert!(
        munitions_favored_mu > munitions_favored_m,
        "with Munitions weighted higher, it should out-produce Machinery: \
         machinery={munitions_favored_m}, munitions={munitions_favored_mu}"
    );
    assert!(
        munitions_favored_m < machinery_favored_m,
        "shifting priority toward Munitions should reduce Machinery's output"
    );
    assert!(
        machinery_favored_mu < munitions_favored_mu,
        "shifting priority toward Machinery should reduce Munitions' output"
    );
}

/// Regression guard for the Stage 2A "solve-order defect": with civilian
/// demand deducted before industry draws its inputs (economy.rs's fixed
/// stage order) and the scenario's default policy (`civilian_ration ==
/// 1.0`), every faction should be able to feed its population from a few
/// days of its own production - shortage must not be structurally pinned
/// high from day one regardless of any decision, the way it was when
/// industry drained Energy/Machinery before civilians ever got a turn.
#[test]
fn starting_factions_are_not_in_shortage() {
    let mut world = scenario::build_world();
    for _ in 0..10 {
        economy::tick_economy(&mut world);
    }

    for faction in &world.factions {
        assert!(
            faction.shortage < 0.2,
            "{} shortage {} after 10 ticks at scenario start with default policy",
            faction.name,
            faction.shortage
        );
    }
}

/// Stage 2A acceptance test: `civilian_ration` is design.md §9's civilian/
/// war trade-off made explicit - rationing civilians harder must raise
/// `shortage` (the unrest pressure it feeds) while leaving more of the
/// rationed goods' stock for industry to consume. Food and Machinery are
/// pre-loaded with an abundant stock buffer so the comparison isolates the
/// Energy path: Energy capacity is deliberately scarce and Steel capacity
/// deliberately abundant, so Steel output is purely bound by whatever
/// Energy civilians left behind.
#[test]
fn rationing_trades_unrest_for_output() {
    let build = |ration: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        {
            let f = world.faction_mut(faction);
            f.stability = 100.0;
            f.civilian_ration = ration;
            f.stock = [0.0; GOOD_COUNT];
            f.stock[Good::Food.index()] = 1000.0;
            f.stock[Good::Machinery.index()] = 1000.0;
        }
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                region.infrastructure = 1.0;
                region.unrest = 0.0;
                region.population = 100.0;
                region.capacity = [0.0; GOOD_COUNT];
                region.capacity[Good::Energy.index()] = 0.5; // 4 regions -> 2.0 total
                region.capacity[Good::Steel.index()] = 100.0; // ample: Energy-input-bound, not potential-bound
            }
        }

        economy::tick_economy(&mut world);
        let f = world.faction(faction);
        (f.shortage, f.stock[Good::Steel.index()])
    };

    let (shortage_full, steel_full) = build(1.0);
    let (shortage_low, steel_low) = build(0.7);

    assert!(
        shortage_low > shortage_full,
        "lowering civilian_ration should raise shortage: full={shortage_full}, low={shortage_low}"
    );
    assert!(
        steel_low > steel_full,
        "lowering civilian_ration should free up more Energy for industry: \
         full={steel_full}, low={steel_low}"
    );
}

/// Stage 2A acceptance test for the structural half of the fix: when
/// produced Energy is barely enough to cover civilian demand and nothing
/// more, civilians must be served in full and it's industry - not
/// civilians - that gets cut to nothing. This is the reverse of the
/// original defect, where industry always spent Energy first and civilians
/// were left with an unsatisfiable remainder.
#[test]
fn civilian_demand_is_served_before_industry() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    {
        let f = world.faction_mut(faction);
        f.stability = 100.0;
        f.civilian_ration = 1.0;
        f.stock = [0.0; GOOD_COUNT];
        // Food and Machinery are pre-funded so only the Energy path is under test.
        f.stock[Good::Food.index()] = 1000.0;
        f.stock[Good::Machinery.index()] = 1000.0;
    }

    let owned: Vec<usize> = world
        .regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.owner == faction)
        .map(|(i, _)| i)
        .collect();
    for &i in &owned {
        world.regions[i].infrastructure = 1.0;
        world.regions[i].unrest = 0.0;
        world.regions[i].population = 100.0;
    }
    let total_pop = owned.len() as f32 * 100.0;
    let energy_need = total_pop * CIVILIAN_ENERGY_DEMAND_PER_POP;
    let per_region_energy = energy_need / owned.len() as f32;
    for &i in &owned {
        world.regions[i].capacity = [0.0; GOOD_COUNT];
        world.regions[i].capacity[Good::Energy.index()] = per_region_energy;
        world.regions[i].capacity[Good::Steel.index()] = 100.0; // would produce plenty, if it had Energy left
    }

    economy::tick_economy(&mut world);

    let f = world.faction(faction);
    assert!(
        f.shortage < 0.02,
        "civilian demand should be met when production barely covers it: shortage={}",
        f.shortage
    );
    assert!(
        f.stock[Good::Steel.index()] < 0.02,
        "with no Energy left after civilians, Steel output should be cut to ~zero: steel={}",
        f.stock[Good::Steel.index()]
    );
}

/// Stage 2B acceptance test: a region that saw combat should end up with
/// higher `devastation`, and that in turn should pull its effective
/// production capacity below the nominal figure.
#[test]
fn combat_devastates_region() {
    let mut world = scenario::build_world();
    let intruder = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(1))
        .unwrap();
    world.units[intruder].station = Station::Region(RegionId(3));
    world.units[intruder].movement = None;

    let devastation_before = world.region(RegionId(3)).devastation;
    assert_eq!(devastation_before, 0.0);

    let mut rng = Rng::new(1);
    let mut events = Vec::new();
    military::tick_combat(&mut world, &mut rng, &mut events);

    let region = world.region(RegionId(3));
    assert!(
        region.devastation > devastation_before,
        "expected combat to raise devastation, got {}",
        region.devastation
    );

    let nominal = region.capacity[Good::Steel.index()];
    let effective = region.effective_capacity(Good::Steel);
    assert!(
        effective < nominal,
        "expected devastation to reduce effective capacity below nominal: \
         nominal={nominal}, effective={effective}"
    );
}

/// Stage 2B acceptance test: the tick a region's ownership flips, its
/// effective production capacity should sit far below its nominal capacity
/// — occupation does not hand over a usable economy immediately.
#[test]
fn captured_region_produces_less() {
    let mut world = scenario::build_world();
    // Region 8 (Shikoku) starts undefended by its owner, faction 2 (same
    // setup as `occupation_flips_owner`).
    let region_id = RegionId(8);
    let mover = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(0))
        .unwrap();
    world.units[mover].station = Station::Region(region_id);
    world.units[mover].movement = None;

    let mut events = Vec::new();
    // `OCCUPATION_RATE` (30) needs 4 ticks to clear the 100-point capture
    // threshold (see `occupation_flips_owner`).
    for _ in 0..4 {
        military::tick_occupation(&mut world, &mut events);
    }
    assert_eq!(world.region(region_id).owner, FactionId(0));

    let region = world.region(region_id);
    assert!(
        region.devastation > 0.0,
        "expected capture to spike devastation"
    );

    let nominal: f32 = crate::good::ALL_GOODS
        .iter()
        .filter(|&&g| g != Good::Food)
        .map(|&g| region.capacity[g.index()])
        .sum();
    let effective = region.industry_total();
    assert!(
        effective < nominal * 0.75,
        "expected freshly captured region's effective industry to sit far below \
         nominal: nominal={nominal}, effective={effective}"
    );
}

/// Stage 2B acceptance test: recovery from `devastation` (docs/phase2-spec.md
/// Stage 2B's recovery formula) must be markedly slower in a high-unrest
/// region than in a calm one, at equal devastation and stability.
#[test]
fn unrest_slows_reconstruction() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    world.faction_mut(faction).stability = 100.0;

    // Regions 0 and 1 both belong to faction 0 (see FACTION_SPECS).
    let calm = RegionId(0);
    let unruly = RegionId(1);
    world.region_mut(calm).devastation = 0.5;
    world.region_mut(calm).unrest = 0.0;
    world.region_mut(unruly).devastation = 0.5;
    world.region_mut(unruly).unrest = 90.0;

    construction::tick_devastation_recovery(&mut world);

    let calm_recovery = 0.5 - world.region(calm).devastation;
    let unruly_recovery = 0.5 - world.region(unruly).devastation;

    assert!(
        calm_recovery > unruly_recovery,
        "expected the calm region to recover more: calm={calm_recovery}, unruly={unruly_recovery}"
    );
    assert!(
        unruly_recovery < calm_recovery * 0.3,
        "expected high unrest to recover markedly slower, not just slightly: \
         calm={calm_recovery}, unruly={unruly_recovery}"
    );
}

/// Stage 2B acceptance test: a completed `Capacity(Steel)` project raises
/// the region's Steel capacity by `CAPACITY_STEP` and clears the project.
#[test]
fn construction_raises_capacity() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let region_id = world.faction(faction).capital;

    world.faction_mut(faction).stock[Good::Machinery.index()] = 1000.0;
    world.faction_mut(faction).stock[Good::Steel.index()] = 1000.0;

    let steel_before = world.region(region_id).capacity[Good::Steel.index()];

    world.region_mut(region_id).construction = Some(Construction {
        project: Project::Capacity(Good::Steel),
        // One tick's worth of fully-funded progress away from completion.
        invested: CONSTRUCTION_REQUIRED_CAPACITY - CONSTRUCTION_RATE,
        required: CONSTRUCTION_REQUIRED_CAPACITY,
    });

    construction::tick_construction(&mut world);

    let steel_after = world.region(region_id).capacity[Good::Steel.index()];
    assert!(
        steel_after > steel_before,
        "expected Capacity(Steel) completion to raise Steel capacity: \
         before={steel_before}, after={steel_after}"
    );
    assert!(
        world.region(region_id).construction.is_none(),
        "expected the completed project to clear from the region"
    );
}

/// Regression test for the P2 construction-overshoot bug: the tick that
/// completes a project must only ever be charged for the progress it
/// actually credits (`required - invested`), not a full `CONSTRUCTION_RATE`
/// with the excess silently discarded. Runs a project to completion across
/// ticks where a Steel shortage forces fractional progress, so `invested`
/// never lands on a clean multiple of `CONSTRUCTION_RATE` and the final tick
/// has a partial remainder — exactly the case that used to overcharge.
#[test]
fn construction_total_cost_matches_required_progress() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let region_id = world.faction(faction).capital;

    // A `required` that isn't a multiple of `CONSTRUCTION_RATE`, so even the
    // very last tick can't land on a whole `CONSTRUCTION_RATE` step.
    let required = 5.0_f32;
    world.region_mut(region_id).construction = Some(Construction {
        project: Project::Repair,
        invested: 0.0,
        required,
    });

    let mut machinery_spent = 0.0_f32;
    let mut steel_spent = 0.0_f32;

    let tick = |world: &mut crate::world::World,
                    machinery_stock: f32,
                    steel_stock: f32,
                    machinery_spent: &mut f32,
                    steel_spent: &mut f32| {
        world.faction_mut(faction).stock[Good::Machinery.index()] = machinery_stock;
        world.faction_mut(faction).stock[Good::Steel.index()] = steel_stock;
        construction::tick_construction(world);
        *machinery_spent += machinery_stock - world.faction(faction).stock[Good::Machinery.index()];
        *steel_spent += steel_stock - world.faction(faction).stock[Good::Steel.index()];
    };

    // Plenty of Machinery, but a Steel shortage (0.7 against a fully-funded
    // per-tick cost of `CONSTRUCTION_RATE * CONSTRUCTION_STEEL_PER_POINT` =
    // 2.0) caps each of these ticks' progress at a fraction of
    // `CONSTRUCTION_RATE`, driving `invested` to a non-multiple of it.
    for _ in 0..3 {
        tick(&mut world, 1000.0, 0.7, &mut machinery_spent, &mut steel_spent);
    }
    assert!(
        world.region(region_id).construction.is_some(),
        "project should still be in progress after the shortage ticks"
    );

    // Fully fund the rest; the project should complete within a few ticks,
    // with the final tick's remaining progress short of a full
    // `CONSTRUCTION_RATE`.
    for _ in 0..10 {
        if world.region(region_id).construction.is_none() {
            break;
        }
        tick(&mut world, 1000.0, 1000.0, &mut machinery_spent, &mut steel_spent);
    }

    assert!(
        world.region(region_id).construction.is_none(),
        "expected the project to complete once fully funded"
    );

    let expected_machinery = required * CONSTRUCTION_MACHINERY_PER_POINT;
    let expected_steel = required * CONSTRUCTION_STEEL_PER_POINT;

    assert!(
        (machinery_spent - expected_machinery).abs() < 1e-3,
        "expected total Machinery spent to equal required * per-point rate exactly, not more: \
         spent={machinery_spent}, expected={expected_machinery}"
    );
    assert!(
        (steel_spent - expected_steel).abs() < 1e-3,
        "expected total Steel spent to equal required * per-point rate exactly, not more: \
         spent={steel_spent}, expected={expected_steel}"
    );
}

/// Stage 2B acceptance test: `Action::Build` against a region with enemy
/// units present must be rejected, not silently accepted.
#[test]
fn build_rejected_when_contested() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    let intruder = world
        .units
        .iter()
        .position(|u| u.owner == FactionId(1))
        .unwrap();
    world.units[intruder].station = Station::Region(capital);
    world.units[intruder].movement = None;

    let result = action::apply_action(
        &mut world,
        faction,
        Action::Build { region: capital, project: Project::Infrastructure },
    );

    assert_eq!(result, Err(ActionError::RegionContested));
    assert!(world.region(capital).construction.is_none());
}

/// Stage 2B acceptance test: devastating a corridor region that supply
/// relays through should reduce what reaches the region behind it, the same
/// way a contested/owner-changed corridor does in `supply_corridor_cut`.
#[test]
fn devastation_reduces_supply_throughput() {
    let mut world = scenario::build_world();
    logistics::recompute_supply(&mut world);
    let before = world.supply[RegionId(1).index()];

    // Region 2 is the only corridor between region 1 and the industrial
    // heartland at region 3 (see `supply_corridor_cut`); devastating it
    // (without changing its owner) should still choke what it relays onward.
    world.region_mut(RegionId(2)).devastation = 0.9;
    logistics::recompute_supply(&mut world);
    let after = world.supply[RegionId(1).index()];

    assert!(
        after < before * 0.9,
        "expected devastating the relay corridor to reduce downstream supply: \
         before={before}, after={after}"
    );
}

/// Regression test for the code-review finding that Steel had an
/// unconditional first claim on Energy: with Energy scarce and Steel
/// already sitting on a large stockpile, Machinery and Munitions must still
/// get a share of Energy and produce something this tick - not be starved
/// to zero just because Steel's own capacity could burn the whole scarce
/// stock by itself.
#[test]
fn energy_is_shared_not_monopolised_by_steel() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    {
        let f = world.faction_mut(faction);
        f.stability = 100.0;
        f.stock = [0.0; GOOD_COUNT];
        // Already sitting on a large Steel stockpile - the review's
        // scenario - so this isn't a case of Steel merely being needed too.
        f.stock[Good::Steel.index()] = 500.0;
    }
    let owned: Vec<usize> = world
        .regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.owner == faction)
        .map(|(i, _)| i)
        .collect();
    for &i in &owned {
        world.regions[i].infrastructure = 1.0;
        world.regions[i].unrest = 0.0;
        world.regions[i].population = 100.0;
        world.regions[i].capacity = [0.0; GOOD_COUNT];
        // Energy is scarce; Steel, Machinery and Munitions all want far
        // more of it than is available, so whichever one gets an
        // unconditional first claim can starve the other two to zero.
        world.regions[i].capacity[Good::Energy.index()] = 0.3;
        world.regions[i].capacity[Good::Steel.index()] = 100.0;
        world.regions[i].capacity[Good::Machinery.index()] = 100.0;
        world.regions[i].capacity[Good::Munitions.index()] = 100.0;
    }

    economy::tick_economy(&mut world);

    let f = world.faction(faction);
    assert!(
        f.stock[Good::Machinery.index()] > 0.0,
        "Machinery should still get a share of scarce Energy despite Steel's \
         stockpile and appetite: {}",
        f.stock[Good::Machinery.index()]
    );
    assert!(
        f.stock[Good::Munitions.index()] > 0.0,
        "Munitions should still get a share of scarce Energy despite Steel's \
         stockpile and appetite: {}",
        f.stock[Good::Munitions.index()]
    );
}

/// Stage 2A acceptance test for the fix: `industry_priority` doesn't just
/// split Steel between Machinery and Munitions, it also splits a scarce
/// Energy stock across Steel, Machinery and Munitions - raising a good's
/// weight should raise its Energy-bound output relative to a competing
/// consumer's.
#[test]
fn industry_priority_controls_energy_split() {
    let build = |steel_weight: f32, machinery_weight: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                region.capacity = [0.0; GOOD_COUNT];
                // Steel and Machinery both want far more Energy than is
                // available; Munitions is silenced (zero capacity and zero
                // weight) so it doesn't dilute the two-way comparison.
                region.capacity[Good::Energy.index()] = 0.3;
                region.capacity[Good::Steel.index()] = 100.0;
                region.capacity[Good::Machinery.index()] = 100.0;
                region.infrastructure = 1.0;
                region.unrest = 0.0;
                region.population = 100.0;
            }
        }
        let f = world.faction_mut(faction);
        f.stability = 100.0;
        // Start Steel at zero so Machinery's own Steel-input consumption
        // (drawn from whatever Steel was produced this tick) doesn't hide
        // behind a large pre-existing stockpile - the final stock is
        // exactly what each good produced, net of what the other consumed.
        f.stock = [0.0; GOOD_COUNT];
        f.industry_priority[Good::Steel.index()] = steel_weight;
        f.industry_priority[Good::Machinery.index()] = machinery_weight;
        f.industry_priority[Good::Munitions.index()] = 0.0;

        economy::tick_economy(&mut world);
        let stock = world.faction(faction).stock;
        (stock[Good::Steel.index()], stock[Good::Machinery.index()])
    };

    let (steel_favored_steel, steel_favored_machinery) = build(0.8, 0.2);
    let (machinery_favored_steel, machinery_favored_machinery) = build(0.2, 0.8);

    assert!(
        steel_favored_steel > steel_favored_machinery,
        "weighting Steel higher should out-produce Machinery: \
         steel={steel_favored_steel}, machinery={steel_favored_machinery}"
    );
    assert!(
        machinery_favored_machinery > machinery_favored_steel,
        "weighting Machinery higher should out-produce Steel: \
         steel={machinery_favored_steel}, machinery={machinery_favored_machinery}"
    );
}

/// Stage 2B acceptance test for the fix: `military::tick_recovery`'s
/// organization-regeneration term must read `effective_infrastructure`
/// (devastation-adjusted), not the raw `infrastructure` field - a unit
/// sitting in a region flattened by fighting should recover organization
/// more slowly than one in an intact region with the same nominal
/// infrastructure.
#[test]
fn devastation_slows_organisation_recovery() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    let unit_ids: Vec<_> = world
        .units
        .iter()
        .filter(|u| u.owner == faction)
        .map(|u| u.id)
        .collect();
    assert!(
        unit_ids.len() >= 2,
        "need two of faction 0's units for this comparison"
    );
    let intact_unit = unit_ids[0];
    let devastated_unit = unit_ids[1];

    // Regions 0 and 1 both belong to faction 0 (see FACTION_SPECS); give
    // them identical nominal infrastructure and differ only in devastation.
    let intact_region = RegionId(0);
    let devastated_region = RegionId(1);
    world.region_mut(intact_region).infrastructure = 1.0;
    world.region_mut(intact_region).devastation = 0.0;
    world.region_mut(devastated_region).infrastructure = 1.0;
    world.region_mut(devastated_region).devastation = 0.9;

    for &(id, region) in &[(intact_unit, intact_region), (devastated_unit, devastated_region)] {
        let unit = world.unit_mut(id);
        unit.station = Station::Region(region);
        unit.movement = None;
        unit.organization = 0.0;
        unit.supply = 1.0;
    }

    let fought = vec![false; world.units.len()];
    let mut events = Vec::new();
    military::tick_recovery(&mut world, &fought, &mut events);

    let intact_org = world.unit(intact_unit).organization;
    let devastated_org = world.unit(devastated_unit).organization;

    assert!(
        devastated_org < intact_org,
        "expected a devastated region to regenerate organization more slowly: \
         intact={intact_org}, devastated={devastated_org}"
    );
    assert!(
        intact_org > 0.0,
        "sanity check: the intact region should still regenerate some organization"
    );
}

/// Stage 2C acceptance test — the regression guard for the structural
/// famine found in the Stage 2A playtest (docs/phase2-spec.md "Stage 2C の
/// 2A で判明した必須要件：輸入": a faction holding the industrial heartland
/// cannot feed its population "at any efficiency" from domestic Food
/// capacity alone). Faction 1 (信越・北陸/東海/近畿) is reshaped here into
/// exactly that case - urban, Machinery-rich, Food capacity nowhere near
/// its population's need even at full efficiency - using the same direct
/// world-construction style `losing_machinery_region_halts_arms` and
/// `input_shortage_limits_output` already use, rather than depending on
/// hundreds of ticks of the full pipeline (conscription, unrest, combat)
/// to eventually reach that state on its own. Without imports the deficit
/// must show up in `shortage` at (very near) the 1.0 ceiling the spec
/// calls out; with a real import plan funded by its own Machinery surplus
/// (ports 東海/近畿/信越・北陸 stay uncontested and undevastated), it must
/// recover to nowhere near that ceiling.
#[test]
fn imports_feed_food_poor_faction() {
    let build = |with_imports: bool| {
        let mut world = scenario::build_world();
        let faction = FactionId(1);
        {
            let f = world.faction_mut(faction);
            f.stock = [0.0; GOOD_COUNT];
            f.stock[Good::Machinery.index()] = 500.0; // ample surplus to pay for imports with
            f.stability = 100.0;
            if with_imports {
                f.import_plan[Good::Food.index()] = 20.0; // more than port capacity can serve
            }
        }
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                // The Stage 2A finding, reproduced directly: an urban
                // industrial region whose own Food capacity cannot cover
                // its population's demand even at 100% efficiency.
                region.infrastructure = 1.0;
                region.unrest = 0.0;
                region.population = 2000.0;
                region.capacity[Good::Food.index()] = 0.3;
            }
        }

        let mut shortage = 0.0;
        for _ in 0..30 {
            trade::tick_imports(&mut world);
            economy::tick_economy(&mut world);
            shortage = world.faction(faction).shortage;
        }
        shortage
    };

    let shortage_without_imports = build(false);
    let shortage_with_imports = build(true);

    assert!(
        shortage_without_imports > 0.9,
        "expected the Stage 2A structural famine (Food capacity that can't cover \
         population demand at any efficiency) to show up as shortage pinned near 1.0 \
         without imports: {shortage_without_imports}"
    );
    assert!(
        shortage_with_imports < 0.3,
        "expected imports funded by Machinery exports to lift the faction out of \
         structural famine: {shortage_with_imports}"
    );
}

/// Stage 2C acceptance test: with no Machinery to pay for it, an import plan
/// with real port capacity and real demand must still land nothing.
#[test]
fn import_requires_payment() {
    let mut world = scenario::build_world();
    let faction = FactionId(1);
    {
        let f = world.faction_mut(faction);
        f.stock[Good::Machinery.index()] = 0.0;
        f.import_plan[Good::Food.index()] = 10.0;
    }
    let food_before = world.faction(faction).stock[Good::Food.index()];

    trade::tick_imports(&mut world);

    let food_after = world.faction(faction).stock[Good::Food.index()];
    assert_eq!(
        food_after, food_before,
        "no Machinery to pay with should mean zero imports land: before={food_before}, after={food_after}"
    );
    let landed: f32 = world
        .regions
        .iter()
        .filter(|r| r.owner == faction)
        .map(|r| r.import_flow)
        .sum();
    assert_eq!(landed, 0.0, "no port should record any import_flow either: {landed}");
}

/// Stage 2C acceptance test: import capacity is accounted per port node, not
/// as one summed national figure — losing a port region should reduce total
/// import volume by exactly that port's own capacity contribution, no more
/// and no less.
#[test]
fn import_capacity_is_per_port() {
    // 東海 (region 5)'s port value from the scenario table, fetched from a
    // fresh world rather than hardcoded so this test tracks the table.
    let tokai_port = scenario::build_world().region(RegionId(5)).port;

    let build = |strip_tokai: bool| {
        let mut world = scenario::build_world();
        let faction = FactionId(1);
        if strip_tokai {
            world.region_mut(RegionId(5)).owner = FactionId(0);
        }
        let f = world.faction_mut(faction);
        f.stock[Good::Machinery.index()] = 1_000_000.0;
        f.import_plan[Good::Food.index()] = 1_000.0; // saturate capacity

        trade::tick_imports(&mut world);
        world
            .regions
            .iter()
            .filter(|r| r.owner == faction)
            .map(|r| r.import_flow)
            .sum::<f32>()
    };

    let with_tokai = build(false);
    let without_tokai = build(true);
    let drop = with_tokai - without_tokai;
    let expected_drop = tokai_port * IMPORT_PER_PORT;

    assert!(
        without_tokai < with_tokai,
        "losing a port region should reduce total import volume: with={with_tokai}, without={without_tokai}"
    );
    assert!(
        (drop - expected_drop).abs() < 0.01,
        "the drop should equal exactly the lost port's own capacity, not a shared/summed \
         figure: drop={drop}, expected={expected_drop}"
    );
}

/// Stage 2C acceptance test: a devastated port's capacity — and therefore
/// its actual import_flow — falls with it.
#[test]
fn devastated_port_imports_less() {
    let build = |devastation: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(1);
        world.region_mut(RegionId(5)).devastation = devastation;
        let f = world.faction_mut(faction);
        f.stock[Good::Machinery.index()] = 1_000_000.0;
        f.import_plan[Good::Food.index()] = 1_000.0; // saturate capacity

        trade::tick_imports(&mut world);
        world.region(RegionId(5)).import_flow
    };

    let intact = build(0.0);
    let devastated = build(0.8);

    assert!(intact > 0.0, "sanity check: the intact port should import something: {intact}");
    assert!(
        devastated < intact * 0.3,
        "a heavily devastated port should import much less: intact={intact}, devastated={devastated}"
    );
}

/// Stage 2C acceptance test: a port with enemy units present must not
/// import at all, even with ample capacity/Machinery/demand elsewhere.
#[test]
fn contested_port_does_not_import() {
    let mut world = scenario::build_world();
    let faction = FactionId(1);
    {
        let f = world.faction_mut(faction);
        f.stock[Good::Machinery.index()] = 1_000_000.0;
        f.import_plan[Good::Food.index()] = 1_000.0;
    }
    // Put an enemy (faction 0) unit at 東海 (region 5), one of faction 1's
    // own ports, contesting it without taking it.
    let intruder = world.units.iter().position(|u| u.owner == FactionId(0)).unwrap();
    world.units[intruder].station = Station::Region(RegionId(5));
    world.units[intruder].movement = None;

    trade::tick_imports(&mut world);

    assert_eq!(
        world.region(RegionId(5)).import_flow,
        0.0,
        "a contested port should not import"
    );
    let total: f32 = world
        .regions
        .iter()
        .filter(|r| r.owner == faction)
        .map(|r| r.import_flow)
        .sum();
    assert!(
        total > 0.0,
        "faction 1's other, uncontested ports should still import: {total}"
    );
}

/// Stage 2C acceptance test: `logistics::recompute_supply`'s node-side cap
/// (`Region::node_throughput`) binds even when the link into the node is
/// otherwise unconstrained — a saturated upstream source and a fully
/// retained, max-infra link must still not push a node's throughput past
/// its own `node_throughput()`, which for an underdeveloped node sits well
/// under a Rail link's 25.0 `max_throughput`.
#[test]
fn node_throughput_limits_supply() {
    let mut world = scenario::build_world();

    // Region 3 (関東, source): saturate its own supply base so nothing
    // upstream is the binding constraint.
    for good in [Good::Steel, Good::Machinery, Good::Munitions, Good::Arms] {
        world.region_mut(RegionId(3)).capacity[good.index()] = 100_000.0;
    }
    world.region_mut(RegionId(3)).infrastructure = 1.0;
    world.region_mut(RegionId(3)).devastation = 0.0;

    // Region 2 (南東北, relay node): full infra, so the link retention
    // formula's own infra factor is maxed out and not what's limiting
    // anything here - only node_throughput's own NODE_BASE/NODE_INFRA
    // terms (no port) are left to cap it.
    world.region_mut(RegionId(2)).infrastructure = 1.0;
    world.region_mut(RegionId(2)).devastation = 0.0;
    world.region_mut(RegionId(2)).port = 0.0;

    logistics::recompute_supply(&mut world);

    let cap2 = world.supply[RegionId(2).index()];
    let node_cap = world.region(RegionId(2)).node_throughput();

    assert!(
        cap2 <= node_cap + 0.01,
        "throughput at the node must not exceed its own node_throughput even with a \
         saturated upstream and a fully retained link: cap2={cap2}, node_cap={node_cap}"
    );
    assert!(
        cap2 < 20.0,
        "node_throughput should keep this region's supply well under Rail's own 25.0 \
         max_throughput ceiling: cap2={cap2}"
    );
}

/// Stage 2C acceptance test: raising `logistics_priority[Arms]` relative to
/// `[Munitions]` (or vice versa) must flip which of the two goods has the
/// higher delivery rate at a region where both are genuinely contended for
/// the same scarce throughput.
#[test]
fn logistics_priority_splits_delivery() {
    let build = |munitions_weight: f32, arms_weight: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        let region = world.faction(faction).capital;

        // Replace faction 0's units with two controlled units, both at the
        // capital (faction 0's own, uncontested), so demand is exactly what
        // this test sets up: real Munitions upkeep demand (manpower) and
        // real Arms delivery demand (a large equipment gap) at once.
        world.units.retain(|u| u.owner != faction);
        for i in 0..2 {
            let id = crate::ids::UnitId(world.units.len() as u32);
            world.units.push(military::Unit {
                id,
                owner: faction,
                name: format!("Test Corps {i}"),
                station: Station::Region(region),
                movement: None,
                manpower: 1.0,
                equipment: 5.0, // large gap vs UNIT_EQUIPMENT (20.0)
                organization: 100.0,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: Station::Region(region),
                experience: 0.0,
                alive: true,
            });
        }
        // Deliberately scarce throughput relative to combined demand, so
        // the two goods are genuinely contending for it.
        world.supply[region.index()] = 2.0;

        let f = world.faction_mut(faction);
        f.logistics_priority[Good::Munitions.index()] = munitions_weight;
        f.logistics_priority[Good::Arms.index()] = arms_weight;

        for _ in 0..30 {
            logistics::distribute_supply(&mut world);
        }

        let units: Vec<_> = world.units.iter().filter(|u| u.owner == faction).collect();
        let n = units.len() as f32;
        let avg_supply = units.iter().map(|u| u.supply).sum::<f32>() / n;
        let avg_arms = units.iter().map(|u| u.arms_delivery).sum::<f32>() / n;
        (avg_supply, avg_arms)
    };

    let (supply_munitions_favored, arms_munitions_favored) = build(0.8, 0.2);
    let (supply_arms_favored, arms_arms_favored) = build(0.2, 0.8);

    assert!(
        supply_munitions_favored > arms_munitions_favored,
        "favoring Munitions should give it the higher delivery rate: \
         supply={supply_munitions_favored}, arms={arms_munitions_favored}"
    );
    assert!(
        arms_arms_favored > supply_arms_favored,
        "favoring Arms should give it the higher delivery rate: \
         supply={supply_arms_favored}, arms={arms_arms_favored}"
    );
}

/// Stage 2C acceptance test: even with an abundant national Arms stockpile,
/// `ReinforceUnit` can't restore more of a unit's equipment gap than the
/// unit's current `arms_delivery` ratio allows.
#[test]
fn arms_delivery_limits_reinforcement() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let unit_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    {
        let unit = world.unit_mut(unit_id);
        unit.equipment = 5.0; // gap of 15.0 against UNIT_EQUIPMENT (20.0)
        unit.arms_delivery = 0.1;
        // External code review fix (Stage 2C): `apply_reinforce` now spends
        // down a real per-tick `arms_budget` (`gap * arms_delivery`, as
        // `logistics::distribute_supply` would have just set it) instead of
        // re-applying `arms_delivery` to the gap at call time - stamp both
        // fields the way a real tick would, matching `arms_delivery_station`
        // (still the unit's own, unchanged, `station`) to `station` so
        // `apply_reinforce` trusts the stamped budget rather than treating
        // it as stale and recomputing from `world.supply` instead.
        unit.arms_budget = (UNIT_EQUIPMENT - unit.equipment) * unit.arms_delivery;
        unit.arms_delivery_station = unit.station;
    }
    world.faction_mut(faction).stock[Good::Arms.index()] = 1_000_000.0;

    let before = world.unit(unit_id).equipment;
    let result = action::apply_action(&mut world, faction, Action::ReinforceUnit { unit: unit_id });
    assert_eq!(result, Ok(()));

    let after = world.unit(unit_id).equipment;
    let gap = UNIT_EQUIPMENT - before;
    let filled = after - before;

    assert!(
        filled < gap * 0.5,
        "a low arms_delivery ratio should stop the unit from being reinforced to full \
         despite an abundant national Arms stock: gap={gap}, filled={filled}"
    );
    assert!(
        (filled - gap * 0.1).abs() < 0.01,
        "the filled amount should match need_equipment * arms_delivery exactly: \
         expected={}, got={filled}",
        gap * 0.1
    );
}

/// External code review fix (Stage 2C, P1): `ReinforceUnit` used to
/// re-apply `arms_delivery` (a ratio) to the unit's *remaining* equipment
/// gap on every call, so N actions against the same unit in one batch
/// compounded past a single tick's delivery allowance - at
/// `arms_delivery == 0.1`, ten actions filled roughly `1 - 0.9^10 ≈ 65%`
/// of the original gap instead of 10%. `apply_reinforce` now spends down a
/// real per-tick `arms_budget` instead, so no number of actions in one
/// batch can together deliver more than one tick's allowance.
#[test]
fn repeated_reinforce_cannot_exceed_daily_delivery() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let unit_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    {
        let unit = world.unit_mut(unit_id);
        unit.equipment = 5.0; // gap of 15.0 against UNIT_EQUIPMENT (20.0)
        unit.arms_delivery = 0.1;
        unit.arms_budget = (UNIT_EQUIPMENT - unit.equipment) * unit.arms_delivery; // 1.5
        unit.arms_delivery_station = unit.station;
    }
    world.faction_mut(faction).stock[Good::Arms.index()] = 1_000_000.0;

    let before = world.unit(unit_id).equipment;
    let gap = UNIT_EQUIPMENT - before;
    let one_tick_allowance = gap * 0.1;

    // N large enough that the pre-fix "reapply ratio to remainder" shape
    // would obviously fail this: 1 - 0.9^30 ≈ 96% of the gap, versus the
    // ~10% one tick should actually allow.
    for _ in 0..30 {
        let result = action::apply_action(&mut world, faction, Action::ReinforceUnit { unit: unit_id });
        assert_eq!(result, Ok(()));
    }

    let after = world.unit(unit_id).equipment;
    let filled = after - before;

    assert!(
        (filled - one_tick_allowance).abs() < 0.01,
        "30 ReinforceUnit actions in a single batch must not deliver more \
         than one tick's allowance ({one_tick_allowance}) no matter how many \
         times the action is resubmitted: filled={filled}, gap={gap}"
    );
}

/// External code review fix (Stage 2C, P1): `distribute_supply` computes
/// `arms_delivery` once a tick, *before* `military::tick_movement` runs, so
/// a unit that finishes moving into a newly cut-off friendly region still
/// carries the ratio/budget stamped from the well-supplied region it just
/// left - `apply_reinforce` must not trust that stale value for a region
/// that is, right now, starved.
#[test]
fn reinforcement_uses_current_region_supply() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let unit_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    let old_region = world.unit(unit_id).station.region().unwrap();
    let dest = RegionId(1);
    assert_eq!(world.region(dest).owner, faction, "test setup requires an owned, uncontested destination");

    // `world.supply` is recomputed once a tick by `recompute_supply` from
    // the map's link topology alone - independent of which units are
    // where - so directly driving it down to zero here is a faithful stand
    // in for "region 1's relay chain is currently cut" without this test
    // depending on the MVP map's specific link layout (already exercised
    // by `supply_corridor_cut` above).
    world.supply[dest.index()] = 0.0;

    // Simulate the unit having just finished a same-day move into the
    // now-cut-off region 1, before `distribute_supply` has run again for
    // its new station: `arms_delivery`/`arms_budget`/`arms_delivery_station`
    // are left exactly as they were at `old_region` (healthy, well
    // connected) - precisely the state `military::tick_movement` would
    // leave a freshly-arrived unit in.
    {
        let unit = world.unit_mut(unit_id);
        unit.station = Station::Region(dest);
        unit.movement = None;
        unit.equipment = 5.0; // gap of 15.0 against UNIT_EQUIPMENT (20.0)
        unit.arms_delivery = 1.0;
        unit.arms_budget = UNIT_EQUIPMENT - unit.equipment; // as if fully deliverable back at old_region
        unit.arms_delivery_station = Station::Region(old_region);
    }
    assert_ne!(old_region, dest, "test setup requires an actual region change");
    world.faction_mut(faction).stock[Good::Arms.index()] = 1_000_000.0;

    let before = world.unit(unit_id).equipment;
    let gap = UNIT_EQUIPMENT - before;
    let result = action::apply_action(&mut world, faction, Action::ReinforceUnit { unit: unit_id });
    assert_eq!(result, Ok(()));
    let after = world.unit(unit_id).equipment;
    let filled = after - before;

    assert!(
        filled < gap * 0.1,
        "a unit that moved into a cut-off region must not reinforce as if it \
         were still at its old, well-supplied region: filled={filled}, gap={gap}"
    );
}


// ===== Stage 2D — 海軍・制海権・海上封鎖 (docs/phase2-spec.md "Stage 2D") =====

/// Stage 2D acceptance test: a port whose facing sea zone is dominated by an
/// enemy faction (>= `BLOCKADE_CONTROL_THRESHOLD`) must import nothing, even
/// with a real import plan, ample Machinery to pay for it, and the port
/// itself neither devastated nor land-contested.
#[test]
fn blockade_stops_import() {
    let mut world = scenario::build_world();
    let faction = FactionId(1); // owns 信越・北陸(4), 東海(5), 近畿(6)
    {
        let f = world.faction_mut(faction);
        f.stock[Good::Machinery.index()] = 1_000_000.0;
        f.import_plan[Good::Food.index()] = 1_000.0; // saturate capacity
    }

    // Faction 0 holds full control of every sea zone touching 東海 (region
    // 5) - well past the blockade threshold - without a single enemy land
    // unit ever setting foot there.
    for zone in world.zones_touching(RegionId(5)) {
        world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];
    }
    assert!(naval::is_port_blockaded(&world, RegionId(5)));

    trade::tick_imports(&mut world);

    assert_eq!(
        world.region(RegionId(5)).import_flow,
        0.0,
        "a blockaded port must not import, despite plan/Machinery/no land contest"
    );
}

/// Stage 2D acceptance test: blockade is judged per port - blockading one of
/// a faction's ports must not touch another, unblockaded port's imports.
#[test]
fn blockade_is_per_port() {
    let mut world = scenario::build_world();
    let faction = FactionId(1); // owns 信越・北陸(4), 東海(5), 近畿(6)
    {
        let f = world.faction_mut(faction);
        f.stock[Good::Machinery.index()] = 1_000_000.0;
        f.import_plan[Good::Food.index()] = 1_000.0;
    }

    // Blockade 東海 (region 5) only - 信越・北陸 (region 4, facing 日本海,
    // zone 3) shares no sea zone with 東海 (太平洋南, zone 2), so it must
    // stay untouched.
    assert!(
        world.zones_touching(RegionId(5)).iter().all(|z| !world.zones_touching(RegionId(4)).contains(z)),
        "test setup requires 東海 and 信越・北陸 to face disjoint sea zones"
    );
    for zone in world.zones_touching(RegionId(5)) {
        world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];
    }

    trade::tick_imports(&mut world);

    assert_eq!(world.region(RegionId(5)).import_flow, 0.0, "東海 is blockaded and must not import");
    assert!(
        world.region(RegionId(4)).import_flow > 0.0,
        "信越・北陸 shares no sea zone with the blockaded port and must keep importing: {}",
        world.region(RegionId(4)).import_flow
    );
}

/// Stage 2D acceptance test: the 中国—九州 `Tunnel` link is deliberately
/// exempt from sea control - it must relay full throughput even when every
/// sea zone touching either end is fully enemy-controlled, unlike a real
/// `Strait` link which would be throttled to nothing under the same
/// control (see `sea_control_throttles_strait`).
#[test]
fn kanmon_tunnel_survives_blockade() {
    let build = |blockade: bool| {
        let mut world = scenario::build_world();
        // 中国 (region 7) is boosted into a saturated source so the Tunnel
        // link to 九州 (region 9) is the binding constraint, not upstream
        // production; 九州's own base is zeroed (capacity and port) so
        // `recompute_supply`'s cap there is driven purely by what the
        // Tunnel relays from region 7, not by its own (also blockade-
        // sensitive) `supply_source`.
        for good in crate::good::ALL_GOODS {
            world.region_mut(RegionId(7)).capacity[good.index()] = 1000.0;
        }
        world.region_mut(RegionId(7)).infrastructure = 1.0;
        world.region_mut(RegionId(9)).port = 0.0;
        world.region_mut(RegionId(9)).capacity = [0.0; GOOD_COUNT];

        if blockade {
            // 中国(7)/九州(9) are 西方同盟's (faction 2); faction 0 (東方連合)
            // is the enemy holding full control here.
            for zone in world
                .zones_touching(RegionId(7))
                .into_iter()
                .chain(world.zones_touching(RegionId(9)))
            {
                world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];
            }
        }
        logistics::recompute_supply(&mut world);
        world.supply[RegionId(9).index()]
    };

    let normal = build(false);
    let blockaded = build(true);

    assert!(normal > 0.0, "sanity: the tunnel route should relay something: {normal}");
    assert!(
        (blockaded - normal).abs() < 0.01,
        "the 中国—九州 Tunnel must be unaffected by sea control: normal={normal}, blockaded={blockaded}"
    );
}

/// Stage 2D acceptance test: a real `Strait` link's throughput must fall
/// once the sea zone it crosses is dominated by an enemy faction — the
/// contrast case for `kanmon_tunnel_survives_blockade`.
#[test]
fn sea_control_throttles_strait() {
    let build = |enemy_control: f32| {
        let mut world = scenario::build_world();
        // 北東北 (region 1) also reaches faction 0's industrial heartland
        // via 南東北/関東 (region 2/3, Rail) - zero that route out so the
        // 北海道—北東北 Strait link (crossing 北方海域, zone 0) is the *only*
        // high-value path into region 1, isolating the strait's own
        // throttle instead of measuring a route that bypasses it entirely.
        for &r in &[RegionId(2), RegionId(3)] {
            world.region_mut(r).capacity = [0.0; GOOD_COUNT];
            world.region_mut(r).port = 0.0;
        }
        for good in crate::good::ALL_GOODS {
            world.region_mut(RegionId(0)).capacity[good.index()] = 1000.0;
        }
        world.region_mut(RegionId(0)).infrastructure = 1.0;
        world.region_mut(RegionId(1)).infrastructure = 1.0;
        // Region 0/1 are both faction 0's; the enemy here is faction 1.
        world.sea_zone_mut(SeaZoneId(0)).control = vec![0.0, enemy_control, 0.0];

        logistics::recompute_supply(&mut world);
        world.supply[RegionId(1).index()]
    };

    let open = build(0.0);
    let contested = build(0.9);

    assert!(open > 0.0, "sanity: the strait route should relay something when uncontested: {open}");
    assert!(
        contested < open * 0.3,
        "heavy enemy sea control should throttle the strait's throughput sharply: \
         open={open}, contested={contested}"
    );
}

/// External code review fix (Stage 2D): a `Strait` crossing must track
/// *current* sea control, not whatever the zone looked like the instant the
/// order was issued. A unit part-way across an open strait should make
/// visibly slower per-tick progress once the enemy establishes a blockade
/// there - the pre-fix code baked the control factor into `Movement::required`
/// only once, at order time, so a blockade raised afterward had no effect at
/// all on a crossing already under way.
#[test]
fn strait_crossing_slows_when_blockade_established() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let zone = SeaZoneId(0); // 北方海域, the 北海道—北東北 Strait link's zone

    // Region 0/1 are both faction 0's, so this crossing is never
    // hostile-destination-slowed - any slowdown measured below can only
    // come from sea control.
    world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];

    let unit_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    world.unit_mut(unit_id).station = Station::Region(RegionId(0));
    world.unit_mut(unit_id).movement = None;

    action::apply_action(
        &mut world,
        faction,
        Action::MoveUnit { unit: unit_id, to: Station::Region(RegionId(1)) },
    )
    .unwrap();

    // One tick while the strait is open.
    military::tick_movement(&mut world);
    let open_step = world.unit(unit_id).movement.unwrap().progress;
    assert!(open_step > 0.0, "sanity: an open crossing should make progress");

    // Faction 1 now holds the zone almost completely.
    world.sea_zone_mut(zone).control = vec![0.05, 0.95, 0.0];

    let progress_before = world.unit(unit_id).movement.unwrap().progress;
    military::tick_movement(&mut world);
    let blockaded_step = world.unit(unit_id).movement.unwrap().progress - progress_before;

    assert!(
        blockaded_step < open_step * 0.3,
        "a crossing under a freshly-established blockade should make much \
         slower progress than the same crossing did while open: \
         open_step={open_step}, blockaded_step={blockaded_step}"
    );
}

/// External code review fix (Stage 2D): a `Strait` crossing ordered while
/// the enemy holds near-total sea control must still complete in a
/// reasonable number of ticks once that control is lost - not be stuck
/// forever. The pre-fix code divided `Movement::required` by the control
/// factor sampled at order time, so an order placed under a near-total
/// blockade produced an effectively infinite `required` that no later
/// change in sea control could ever undo.
#[test]
fn strait_crossing_recovers_when_blockade_lifted() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let zone = SeaZoneId(0);

    // Order the crossing while faction 1 holds the zone almost completely.
    world.sea_zone_mut(zone).control = vec![0.02, 0.98, 0.0];

    let unit_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    world.unit_mut(unit_id).station = Station::Region(RegionId(0));
    world.unit_mut(unit_id).movement = None;

    action::apply_action(
        &mut world,
        faction,
        Action::MoveUnit { unit: unit_id, to: Station::Region(RegionId(1)) },
    )
    .unwrap();

    let required = world.unit(unit_id).movement.unwrap().required;
    assert!(
        required < 20.0,
        "required must stay the control-independent travel cost, not balloon \
         toward infinity under a near-total blockade sampled at order time: \
         required={required}"
    );

    // The blockade lifts.
    world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];

    let mut arrived = false;
    for _ in 0..30 {
        military::tick_movement(&mut world);
        if world.unit(unit_id).movement.is_none() {
            arrived = true;
            break;
        }
    }

    assert!(
        arrived,
        "a crossing ordered under a heavy blockade must complete in a \
         reasonable number of ticks once control is lost, not stay stuck forever"
    );
    assert_eq!(world.unit(unit_id).station, Station::Region(RegionId(1)));
}

/// Stage 2D acceptance test: a broken fleet with no adjacent sea zone free
/// of enemy fleets to fall back into is sunk outright - the sea-domain
/// analogue of `depleted_unit_is_destroyed_not_retreating`.
#[test]
fn naval_combat_sinks_fleet() {
    let mut world = scenario::build_world();
    let faction = FactionId(2); // 西方同盟
    let zone = SeaZoneId(0); // 北方海域, adjacent to zones 1 and 3

    let fleet_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    {
        let fleet = world.unit_mut(fleet_id);
        fleet.station = Station::Sea(zone);
        fleet.movement = None;
        fleet.organization = 0.0;
        fleet.manpower = 1.0; // well above UNIT_DEATH_MANPOWER - this must go through the org/retreat path, not the manpower-death one
        fleet.supply = 1.0;
    }

    // Enemy (faction 0) fleets in the target zone *and* in both zones it
    // could otherwise retreat into, so nowhere is safe.
    let enemy_ids: Vec<_> = world
        .units
        .iter()
        .filter(|u| u.owner == FactionId(0))
        .map(|u| u.id)
        .take(3)
        .collect();
    assert_eq!(enemy_ids.len(), 3, "test setup requires 3 of faction 0's starting units");
    for (&id, z) in enemy_ids.iter().zip([SeaZoneId(0), SeaZoneId(1), SeaZoneId(3)]) {
        let unit = world.unit_mut(id);
        unit.station = Station::Sea(z);
        unit.movement = None;
    }

    // The fleet "fought" this tick (organization already driven to 0 by
    // naval combat just before `tick_recovery` runs in the real pipeline) -
    // without this, the regen pass below would raise organization back
    // above 0 before the retreat/destroy check ever sees it.
    let mut fought = vec![false; world.units.len()];
    fought[fleet_id.index()] = true;
    let mut events = Vec::new();
    military::tick_recovery(&mut world, &fought, &mut events);

    assert!(
        !world.unit(fleet_id).alive,
        "a broken fleet with no safe sea zone to retreat into must be sunk"
    );
}

/// Stage 2D acceptance test: `Action::MoveUnit` must reject a land unit
/// ordered into a sea zone, and a fleet ordered into a region.
#[test]
fn fleet_cannot_enter_land() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    let land_unit = world
        .units
        .iter()
        .find(|u| u.owner == faction && u.station.domain() == Domain::Land)
        .unwrap()
        .id;
    let result = action::apply_action(
        &mut world,
        faction,
        Action::MoveUnit { unit: land_unit, to: Station::Sea(SeaZoneId(0)) },
    );
    assert_eq!(result, Err(ActionError::NotAdjacent));

    let port_region = world.faction(faction).capital; // 関東 (region 3), port > 0
    assert!(world.region(port_region).port > 0.0, "test setup requires a port at the capital");
    world.faction_mut(faction).manpower = 100.0;
    world.faction_mut(faction).stock[Good::Arms.index()] = 1_000.0;
    action::apply_action(
        &mut world,
        faction,
        Action::RecruitUnit { region: port_region, domain: Domain::Sea },
    )
    .unwrap();
    let fleet_id = world.units.last().unwrap().id;
    assert_eq!(world.unit(fleet_id).station.domain(), Domain::Sea);

    let result = action::apply_action(
        &mut world,
        faction,
        Action::MoveUnit { unit: fleet_id, to: Station::Region(RegionId(0)) },
    );
    assert_eq!(result, Err(ActionError::NotAdjacent));
}

/// Stage 2D acceptance test - the regression guard for design.md §2's core
/// causal claim (docs/phase2-spec.md Stage 2D's own framing: "企画書 §2 の
/// 「港湾を封鎖することで物資輸入が停止する」を成立させる"): a faction whose
/// food supply structurally depends on imports must see its `shortage`
/// worsen once its ports are blockaded, even with an identical import plan
/// and identical Machinery to pay for it. Reshapes faction 1 into the same
/// Stage 2A structural-famine case `imports_feed_food_poor_faction` uses.
#[test]
fn blockaded_faction_starves() {
    let build = |blockaded: bool| {
        let mut world = scenario::build_world();
        let faction = FactionId(1);
        {
            let f = world.faction_mut(faction);
            f.stock = [0.0; GOOD_COUNT];
            f.stock[Good::Machinery.index()] = 500.0; // ample surplus to pay for imports with
            f.stability = 100.0;
            f.import_plan[Good::Food.index()] = 20.0; // more than port capacity can serve
        }
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                region.infrastructure = 1.0;
                region.unrest = 0.0;
                region.population = 2000.0;
                region.capacity[Good::Food.index()] = 0.3;
            }
        }

        let mut shortage = 0.0;
        for _ in 0..30 {
            if blockaded {
                // Faction 0 holds full control of every sea zone in the map
                // - nothing here calls `naval::tick_sea_control` to keep a
                // fleet-derived value fresh, so it's reasserted every tick
                // the same way `trade`'s own `contested` snapshot is
                // recomputed every tick from current unit positions.
                for zone in world.sea_zones.iter_mut() {
                    zone.control = vec![1.0, 0.0, 0.0];
                }
            }
            trade::tick_imports(&mut world);
            economy::tick_economy(&mut world);
            shortage = world.faction(faction).shortage;
        }
        shortage
    };

    let shortage_open = build(false);
    let shortage_blockaded = build(true);

    assert!(
        shortage_open < 0.3,
        "sanity: an unblockaded import plan should lift the faction out of famine: {shortage_open}"
    );
    assert!(
        shortage_blockaded > shortage_open + 0.3,
        "blockading every one of faction 1's ports must worsen its shortage despite an \
         identical import plan and identical Machinery to pay for it: \
         open={shortage_open}, blockaded={shortage_blockaded}"
    );
}
