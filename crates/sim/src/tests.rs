//! Spec §9 acceptance tests for the simulation core.

use crate::action::{self, Action, ActionError, Layer, ALL_LAYERS};
use crate::balance::{
    CAPTURE_UNREST, CIVILIAN_ENERGY_DEMAND_PER_POP, CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN,
    CONSTRUCTION_MACHINERY_PER_POINT, CONSTRUCTION_RATE, CONSTRUCTION_REQUIRED_CAPACITY,
    CONSTRUCTION_STEEL_PER_POINT, DEVASTATION_ON_CAPTURE, FOCUS_SWITCH_DAYS, FOOD_EFFICIENCY_FLOOR,
    GROUP_SUPPORT_BASELINE, IMPORT_PER_PORT, INDUSTRIAL_STABILITY_FLOOR, LINE_INTERDICTION_DAMAGE,
    NL_PROPOSAL_COOLDOWN_DAYS, OCCUPATION_RATE, SEPARATISM_THRESHOLD, STRIKE_DAYS,
    STRIKE_OUTPUT_MULT, TRANSPORT_LINE_REPAIR_STEP, TREATY_ACCEPT_OPINION_BONUS,
    UNIT_DEATH_MANPOWER, UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG,
};
use crate::construction::{self, Construction, Project};
use crate::diplomacy::{self, Treaty, TreatyTerm};
use crate::economy;
use crate::event::Event;
use crate::focus::{self, NationalFocus};
use crate::good::{Good, GOOD_COUNT};
use crate::group::{Group, GROUP_COUNT};
use crate::ids::{FactionId, RegionId, SeaZoneId, TransportLineId, TransportNodeId, UnitId};
use crate::logistics;
use crate::military;
use crate::naval;
use crate::observation::{
    encoding_len, Observation, DIPLOMACY_FIELD_COUNT, ENCODING_LEN, FACTION_FIELD_COUNT,
    REGION_FIELD_COUNT, SEA_ZONE_FIELD_COUNT, TRANSPORT_LINE_FIELD_COUNT, TRANSPORT_NODE_FIELD_COUNT,
};
use crate::politics;
use crate::rng::Rng;
use crate::scenario;
use crate::sim::{Outcome, Simulation};
use crate::trade;
use crate::transport::{self, Capacity, Condition, TransportLineKind, TransportNodeKind};
use crate::world::{Domain, DominationShare, Station, VictoryCondition, VictoryDeclaration, World};

/// Stage 6C (docs/phase6-spec.md "Stage 6C" item 1): this test used to
/// "cut" the corridor by reassigning `RegionId(2)`'s `owner` to another
/// faction outright. But `recompute_supply` refuses to relay across *any*
/// faction boundary unconditionally (`if world.regions[j].owner !=
/// owner_i { continue; }`), regardless of link kind, throughput, or
/// whether the chokepoint mechanism the corridor is supposed to exercise
/// does anything at all — an owner change always "works", which means this
/// test passed for a reason unrelated to what it claimed to test and would
/// never have caught a broken `recompute_supply`. Rewritten the same way
/// `japan47_chokepoints_still_bind` was: cut the corridor through the real
/// mechanism (`contested[i]`, an enemy unit *contesting* the region without
/// capturing it) instead, on mvp's 10-region map, and prove below that the
/// rewritten assertion can in fact fail.
///
/// Region 2 (`minami_tohoku`) hosts the only transport-network corridor
/// (`minami_tohoku_depot -> kita_tohoku_depot`, a `Rail` `TransportLine`)
/// between region 1 (`kita_tohoku`) and touhou_rengou's industrial base at
/// region 3 (`kanto`) — region 1 has no other route there. `isolate_single_source`
/// zeroes every other touhou_rengou region's own capacity/port and
/// saturates `kanto`'s, so whatever reaches region 1 is provably
/// attributable to the corridor being tested rather than region 1's own
/// (unboosted, unzeroed) `supply_source` masking the cut, the same
/// reasoning `japan47_chokepoints_still_bind`'s own `isolate_single_source`
/// doc explains for a multi-region faction.
///
/// Stage 9B rewrite: `world.supply[region]` is now demand-bounded (this
/// module's own doc) rather than a pure network ceiling, so a region with
/// no units posted in it always reads `0.0` regardless of whether the
/// corridor mechanism works at all — mvp's default unit placement never
/// puts one at kita_tohoku (`scenario::build_world`'s `locations` cycle
/// only ever lands on `kanto`/`minami_tohoku`). A single unit is added at
/// kita_tohoku in both boards below so `before`/`after` measure the actual
/// corridor mechanism instead of trivially reading zero either way.
#[test]
fn supply_corridor_cut() {
    fn isolate_single_source(world: &mut World, source: RegionId) {
        let faction = world.region(source).owner;
        for i in 0..world.regions.len() {
            let r = RegionId(i as u32);
            if r != source && world.region(r).owner == faction {
                world.region_mut(r).capacity = [0.0; GOOD_COUNT];
                world.region_mut(r).port = 0.0;
            }
        }
        for good in crate::good::ALL_GOODS {
            world.region_mut(source).capacity[good.index()] = 1000.0;
        }
        world.region_mut(source).infrastructure = 1.0;
    }

    fn station_garrison(world: &mut World, region: RegionId) {
        let owner = world.region(region).owner;
        let id = crate::ids::UnitId(world.units.len() as u32);
        world.units.push(military::Unit {
            id,
            owner,
            name: "Garrison".to_string(),
            station: Station::Region(region),
            movement: None,
            manpower: 1.0,
            equipment: crate::balance::UNIT_EQUIPMENT,
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

    let kanto = RegionId(3);
    let kita_tohoku = RegionId(1);
    let minami_tohoku = RegionId(2);

    let mut world = scenario::build_world();
    isolate_single_source(&mut world, kanto);
    station_garrison(&mut world, kita_tohoku);
    logistics::recompute_supply(&mut world);
    let before = world.supply[kita_tohoku.index()];

    let mut cut = scenario::build_world();
    isolate_single_source(&mut cut, kanto);
    station_garrison(&mut cut, kita_tohoku);
    let owner = cut.region(minami_tohoku).owner;
    // A foreign, at-war unit merely *stationed* in region 2 - enough to
    // make `World::has_enemy_units`/`recompute_supply`'s `contested[i]`
    // true - without ever touching `Region::owner`.
    let enemy = FactionId((owner.0 + 1) % cut.factions.len() as u32);
    let raider_id = crate::ids::UnitId(cut.units.len() as u32);
    cut.units.push(military::Unit {
        id: raider_id,
        owner: enemy,
        name: "Enemy Raiding Force".to_string(),
        station: Station::Region(minami_tohoku),
        movement: None,
        manpower: 1.0,
        equipment: 1.0,
        organization: 100.0,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(minami_tohoku),
        experience: 0.0,
        alive: true,
    });
    logistics::recompute_supply(&mut cut);
    let after = cut.supply[kita_tohoku.index()];

    assert_eq!(
        cut.region(minami_tohoku).owner,
        owner,
        "test setup requires ownership to stay unchanged - the cut must come from contest, not conquest"
    );
    assert!(before > 0.0, "sanity: region 1 should receive relayed supply via region 2 when intact: {before}");
    assert!(
        after < before * 0.1,
        "an enemy force holding region 2 (contested, not captured) must starve region 1's relayed supply \
         without touching ownership: before={before}, after={after}"
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

/// The disband defect's most basic fix: `Action::DisbandUnit` removes the
/// unit and shrinks its owner's living force by exactly one - the mechanic
/// this codebase had no answer for at all until now. Also checks the
/// refund this design deliberately grants (see `action::apply_disband`'s
/// doc): the unit's *current* manpower/equipment land back in
/// `Faction::manpower`/`stock[Arms]`, not the nominal recruit cost.
#[test]
fn disbanded_unit_is_gone_and_force_shrinks() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let unit_id = world.units.iter().find(|u| u.owner == faction && u.alive).unwrap().id;
    // Give the unit non-nominal manpower/equipment so the refund can be
    // told apart from "always refunds the fresh-recruit constants".
    {
        let unit = world.unit_mut(unit_id);
        unit.manpower = 0.6;
        unit.equipment = 13.0;
    }

    let before_count = world.units.iter().filter(|u| u.owner == faction && u.alive).count();
    let before_manpower = world.faction(faction).manpower;
    let before_arms = world.faction(faction).stock[Good::Arms.index()];

    let result = action::apply_action(&mut world, faction, Action::DisbandUnit { unit: unit_id });
    assert_eq!(result, Ok(()));

    assert!(!world.unit(unit_id).alive, "a disbanded unit must no longer be alive");
    let after_count = world.units.iter().filter(|u| u.owner == faction && u.alive).count();
    assert_eq!(after_count, before_count - 1, "the owner's living force must shrink by exactly one");

    assert!(
        (world.faction(faction).manpower - (before_manpower + 0.6)).abs() < 1e-4,
        "expected the unit's current manpower (0.6), not UNIT_MANPOWER, to be refunded: got {}",
        world.faction(faction).manpower
    );
    assert!(
        (world.faction(faction).stock[Good::Arms.index()] - (before_arms + 13.0)).abs() < 1e-4,
        "expected the unit's current equipment (13.0) to be refunded to Arms stock: got {}",
        world.faction(faction).stock[Good::Arms.index()]
    );
}

/// `Action::DisbandUnit` is validated like every other unit action: it must
/// be rejected for a unit the acting faction doesn't own, and must leave
/// that unit completely untouched. Checked this fails when broken:
/// temporarily removed the `owner != faction` check from `owned_unit`
/// (shared by every unit action, including this one) - the assertions
/// below then fail (the foreign unit is disbanded).
#[test]
fn disband_rejected_for_unit_not_owned() {
    let mut world = scenario::build_world();
    let foreign_unit = world.units.iter().find(|u| u.owner == FactionId(1) && u.alive).unwrap().id;

    let result = action::apply_action(&mut world, FactionId(0), Action::DisbandUnit { unit: foreign_unit });

    assert_eq!(result, Err(ActionError::NotOwner));
    assert!(world.unit(foreign_unit).alive, "a rejected disband must never touch the unit");
    assert_eq!(world.unit(foreign_unit).owner, FactionId(1));
}

/// A unit sharing its station with the enemy cannot simply demobilise and
/// walk away - the same "Pinned" condition `MoveUnit`/`ReinforceUnit`
/// already refuse under. This also closes the exploit the refund above
/// would otherwise open: cashing out a doomed unit's full manpower/
/// equipment the instant before it would die in combat as an unrefunded
/// casualty (`military::tick_combat`'s `Outcome::Destroyed`). Checked this
/// fails when broken: temporarily removed the `pinned` check from
/// `apply_disband` - the assertions below then fail (the contested unit is
/// disbanded, refund and all).
#[test]
fn disband_rejected_under_enemy_contact() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let defender = world.units.iter().find(|u| u.owner == faction && u.alive).unwrap().id;
    let contested_region = world.unit(defender).station.region().unwrap();

    // An enemy unit merely stationed in the same region - enough to make
    // `World::has_enemy_units` true - the same setup `supply_corridor_cut`
    // uses to contest a region without capturing it.
    let enemy = FactionId(1);
    let raider_id = crate::ids::UnitId(world.units.len() as u32);
    world.units.push(military::Unit {
        id: raider_id,
        owner: enemy,
        name: "Enemy Raiding Force".to_string(),
        station: Station::Region(contested_region),
        movement: None,
        manpower: 1.0,
        equipment: 1.0,
        organization: 100.0,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(contested_region),
        experience: 0.0,
        alive: true,
    });

    let before_manpower = world.faction(faction).manpower;
    let result = action::apply_action(&mut world, faction, Action::DisbandUnit { unit: defender });

    assert_eq!(result, Err(ActionError::RegionContested));
    assert!(world.unit(defender).alive, "a contested unit must not be disbanded");
    assert_eq!(world.faction(faction).manpower, before_manpower, "a rejected disband must not refund anything");
}

/// Regression guard for the defect this whole action exists to fix
/// (docs/conventions.md §6's one-way accumulator shape, measured on
/// `scenarios/japan_hex.json`): a faction that over-recruited relative to
/// its industry had no path back to solvency at all before `DisbandUnit`
/// existed - Munitions demand from an oversized army permanently
/// outstripped what its industry could produce, pinning the stockpile at
/// zero forever no matter how many ticks passed. Standing enough of the
/// excess down must let production catch back up. Checked this fails when
/// broken: temporarily made `apply_disband` a no-op (`Ok(())` without
/// touching `unit.alive`) - `munitions_recovered` then stays pinned at
/// (approximately) `munitions_overextended`, and the final assertion fails.
#[test]
fn over_extended_faction_recovers_solvency_by_disbanding() {
    let mut sim = Simulation::new(1);
    let faction = FactionId(0);
    let capital = sim.world.faction(faction).capital;

    // Massively over-recruit: far more units than this faction's Munitions
    // production could ever feed, so demand permanently swamps supply -
    // the same shape as 近畿府 on japan_hex after losing territory its army
    // was sized for.
    let mut extra_units = Vec::new();
    for _ in 0..40 {
        let id = crate::ids::UnitId(sim.world.units.len() as u32);
        sim.world.units.push(military::Unit {
            id,
            owner: faction,
            name: "Overextension Test Corps".to_string(),
            station: Station::Region(capital),
            movement: None,
            manpower: UNIT_MANPOWER,
            equipment: UNIT_EQUIPMENT,
            organization: UNIT_ORG,
            morale: 1.0,
            supply: 1.0,
            arms_delivery: 1.0,
            arms_budget: 0.0,
            arms_delivery_station: Station::Region(capital),
            experience: 0.0,
            alive: true,
        });
        extra_units.push(id);
    }

    for _ in 0..60 {
        sim.step();
    }
    let munitions_overextended = sim.world.faction(faction).stock[Good::Munitions.index()];
    assert!(
        munitions_overextended < 0.01,
        "sanity: an army this oversized should pin Munitions at zero: {munitions_overextended}"
    );

    // Stand the entire oversized addition back down - back toward something
    // this faction's industry can actually feed - and let the economy run
    // on.
    for &unit in &extra_units {
        let errors = sim.apply(faction, &[Action::DisbandUnit { unit }]);
        assert!(errors.is_empty(), "disbanding an uncontested own unit must succeed: {errors:?}");
    }

    for _ in 0..90 {
        sim.step();
    }
    let munitions_recovered = sim.world.faction(faction).stock[Good::Munitions.index()];

    assert!(
        munitions_recovered > munitions_overextended + 1.0,
        "expected Munitions to recover once the oversized army was stood down: \
         overextended={munitions_overextended}, recovered={munitions_recovered}"
    );
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
    let region_delta = vec![0i32; world.factions.len()];
    let mut events = Vec::new();
    for _ in 0..80 {
        politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
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
///
/// Stage 9B rewrite: mvp's default unit placement never posts a unit at
/// region 1 (`supply_corridor_cut`'s own doc explains why), and
/// `world.supply[region]` is now demand-bounded, so without a unit there
/// this always read `0.0` regardless of devastation. A single garrison unit
/// is added at region 1 so `before` is provably nonzero and `after` reflects
/// the corridor's own devastation, not an empty demand sink. Stage 9B also
/// ties a `TransportLine`'s `effective_capacity` to *both* endpoint
/// regions' devastation (`INFRA_DAMAGE_SHARE`-scaled, `TransportLine::
/// effective_capacity`'s own doc) exactly the way the pre-Stage-9B relay
/// formula read `effective_infrastructure`, so this property survives the
/// model swap unchanged in spirit.
#[test]
fn devastation_reduces_supply_throughput() {
    fn build(devastate_corridor: bool) -> f32 {
        let mut world = scenario::build_world();
        let kanto = RegionId(3);
        let region1 = RegionId(1);

        // Boost the source, zero every other same-faction region's own
        // base (`supply_corridor_cut`'s own `isolate_single_source`),
        // and post a large garrison at region 1 so demand there is far
        // above what the (devastated) corridor could ever carry - without
        // this, a single unit's tiny demand is satisfiable even at a
        // heavily devastated corridor's reduced capacity, and the
        // devastation would never actually bind (`node_throughput_limits_supply`'s
        // own doc makes the same "saturate demand, not just the source"
        // point for exactly this reason).
        let faction = world.region(kanto).owner;
        for i in 0..world.regions.len() {
            let r = RegionId(i as u32);
            if r != kanto && world.region(r).owner == faction {
                world.region_mut(r).capacity = [0.0; GOOD_COUNT];
                world.region_mut(r).port = 0.0;
            }
        }
        for good in crate::good::ALL_GOODS {
            world.region_mut(kanto).capacity[good.index()] = 1000.0;
        }
        world.region_mut(kanto).infrastructure = 1.0;

        for i in 0..20 {
            let id = crate::ids::UnitId(world.units.len() as u32);
            world.units.push(military::Unit {
                id,
                owner: faction,
                name: format!("Garrison {i}"),
                station: Station::Region(region1),
                movement: None,
                manpower: 1.0,
                equipment: UNIT_EQUIPMENT,
                organization: 100.0,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: Station::Region(region1),
                experience: 0.0,
                alive: true,
            });
        }

        // Region 2 is the only corridor between region 1 and the
        // industrial heartland at region 3 (see `supply_corridor_cut`);
        // devastating it (without changing its owner) should still choke
        // what it relays onward.
        if devastate_corridor {
            world.region_mut(RegionId(2)).devastation = 0.9;
        }
        logistics::recompute_supply(&mut world);
        world.supply[region1.index()]
    }

    let before = build(false);
    let after = build(true);

    assert!(before > 0.0, "sanity: region 1 should receive relayed supply via region 2 when intact: {before}");
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

/// Stage 9D fix (docs/conventions.md §6's "状態には必ず回復経路を持たせる" -
/// "state must always have a recovery path, never one that's entered and
/// never left"): a unit stranded beyond every supply route
/// (`world.supply[region] == 0.0` for its own faction, forever) must
/// eventually stop being stuck. `military::tick_recovery`'s own
/// unsupplied-attrition (`ATTRITION_MANPOWER`, gated on `unit.supply`) is
/// the mechanism meant to shrink such a unit toward `UNIT_DEATH_MANPOWER`
/// and out of play - but `action::apply_reinforce` used to refill manpower
/// straight from the faction's national pool with no reference to the
/// network at all, which undid every tick's attrition loss the moment
/// anything called `ReinforceUnit` on it, forever. In real play this
/// happens every single tick: `HeuristicAgent::reinforce_weak_units` issues
/// `ReinforceUnit` for any unit under `REINFORCE_THRESHOLD` strength every
/// time the AI decides, without ever checking whether the network can
/// actually reach it.
///
/// Confirmed this can fail: temporarily removed the `network_reachable`
/// gate in `apply_reinforce` (letting `fill_manpower` refill unconditionally
/// from the national pool again, as before this fix) and re-ran with the
/// same 400-day cap below - the unit never died at all (a full refill every
/// day trivially outpaces `ATTRITION_MANPOWER`'s 0.006/day loss, so manpower
/// stayed pinned near full for the entire run). Reverted before committing.
#[test]
fn stranded_unit_eventually_dies_despite_being_reinforced_every_tick() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let unit_id = world.units.iter().find(|u| u.owner == faction).unwrap().id;
    let region = world.unit(unit_id).station.region().expect("test needs a land unit");
    assert_eq!(world.region(region).owner, faction, "test setup requires the unit's own region");

    // Sever the region from the transport network for good - nothing in
    // this test ever calls `logistics::recompute_supply`, so this stays
    // zero for every simulated day below, standing in for a front cut off
    // behind a severed line permanently rather than for one tick.
    world.supply[region.index()] = 0.0;
    {
        let unit = world.unit_mut(unit_id);
        unit.supply = 0.0; // already eased down to zero, as a real cut-off unit would be within a handful of ticks
        unit.manpower = UNIT_MANPOWER;
        unit.equipment = UNIT_EQUIPMENT; // no equipment gap - isolates the manpower question this fix is about
    }
    // An abundant national pool an unconstrained refill could draw from
    // forever, so nothing but the fix itself stops the unit being topped up.
    world.faction_mut(faction).manpower = 1_000_000.0;

    let worst_case_days = ((UNIT_MANPOWER - UNIT_DEATH_MANPOWER) / crate::balance::ATTRITION_MANPOWER).ceil() as u32;
    let max_days = worst_case_days + 50; // headroom past the theoretical worst case, not a tight bound
    let mut day = 0u32;
    loop {
        // Simulates `HeuristicAgent::reinforce_weak_units` calling
        // `ReinforceUnit` on this unit every single day - the worst case
        // for the trap this fix closes.
        let _ = action::apply_action(&mut world, faction, Action::ReinforceUnit { unit: unit_id });
        let fought = vec![false; world.units.len()];
        let mut events = Vec::new();
        military::tick_recovery(&mut world, &fought, &mut events);
        day += 1;
        if !world.unit(unit_id).alive || day >= max_days {
            break;
        }
    }

    assert!(
        !world.unit(unit_id).alive,
        "a unit cut off from every supply route must eventually stop being stuck (die to attrition) even when \
         something calls ReinforceUnit on it every single day - it survived past day {max_days} (worst case \
         {worst_case_days}), which means manpower reinforcement is undoing the network's own attrition again"
    );
}

/// The third sibling of the occupier-supply defect `logistics`'s own
/// module doc already documents two fixes for (`distribute_supply`'s and
/// `land_unit_supply_avail`'s non-owner branches): `naval::
/// best_facing_port_supply` used to read `world.supply[region]`, which
/// since Stage 9B means "delivered to this region's own *land* demand" -
/// `0.0` whenever no land unit happens to be garrisoned there, regardless
/// of how healthy and fully connected the port actually is (`logistics::
/// compute_transport_flow` never creates a demand candidate for a region
/// with none). A damaged fleet facing exactly such a port could then never
/// reinforce its manpower even though the network was fully intact - fixed
/// by `world.port_capacity` (`World`'s own doc), a structural
/// capacity/source figure independent of local demand.
///
/// Confirmed this can fail: reverted `naval::best_facing_port_supply` to
/// read `world.supply[r.index()]` again (the pre-fix formula) and reran -
/// `world.supply[port_region]` reads `0.0` with no land garrison (asserted
/// below as the demand-bounded reading the test setup relies on), so
/// `naval::fleet_unit_supply_avail` also reads `0.0`, `network_reachable`
/// is `false`, and the fleet's manpower does not move at all (`after ==
/// before`), failing the final assertion. Reverted before committing.
#[test]
fn damaged_fleet_at_undegarrisoned_port_can_reinforce() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let port_region = world.faction(faction).capital;
    assert!(world.has_port_node(port_region), "test setup requires the capital to have a port");
    let zone = naval::home_zone(&world, port_region).expect("test setup requires a facing sea zone");

    // `naval::best_facing_port_supply` takes the *max* over every one of the
    // zone's coastal regions this faction owns - so it isn't enough to clear
    // the port region alone; every same-faction coastal region sharing this
    // zone must also lose its land garrison, or that neighbor's own nonzero
    // `world.supply` would mask exactly the defect this test exists to
    // catch. `elsewhere` is deliberately outside this zone's own coast.
    let coastal_regions: Vec<RegionId> = world.sea_zone(zone).coast.clone();
    let elsewhere = world
        .regions
        .iter()
        .find(|r| r.owner == faction && !coastal_regions.contains(&r.id))
        .expect("test setup requires the faction to own a region off this zone's coast")
        .id;
    for unit in world.units.iter_mut() {
        if unit.owner == faction
            && matches!(unit.station, Station::Region(r) if coastal_regions.contains(&r))
        {
            unit.station = Station::Region(elsewhere);
        }
    }

    world.faction_mut(faction).manpower = 1_000.0;
    world.faction_mut(faction).stock[Good::Arms.index()] = 1_000.0;
    action::apply_action(&mut world, faction, Action::RecruitUnit { region: port_region, domain: Domain::Sea })
        .unwrap();
    let fleet_id = world.units.last().unwrap().id;
    assert_eq!(world.unit(fleet_id).station.domain(), Domain::Sea);

    logistics::recompute_supply(&mut world);
    assert_eq!(
        world.supply[port_region.index()], 0.0,
        "test setup requires the demand-bounded region reading to be zero with no land garrison present"
    );
    assert!(
        world.port_capacity[port_region.index()][faction.index()] > 0.0,
        "test setup requires the port to be structurally reachable"
    );

    world.unit_mut(fleet_id).manpower = UNIT_MANPOWER * 0.5;
    let before = world.unit(fleet_id).manpower;

    let result = action::apply_action(&mut world, faction, Action::ReinforceUnit { unit: fleet_id });
    assert_eq!(result, Ok(()));
    let after = world.unit(fleet_id).manpower;

    assert!(
        after > before,
        "a damaged fleet facing a healthy, fully-connected port with no land garrison must still be able to \
         reinforce manpower: before={before}, after={after}"
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

/// Stage 2D acceptance test: a real sea crossing's throughput must fall
/// once the sea zone it crosses is dominated by an enemy faction — the
/// contrast case for `kanmon_tunnel_survives_blockade`.
///
/// Stage 9B rewrite: the mechanism is now `naval::sea_line_factor`
/// throttling the `hokkaido_port -> kita_tohoku_port` `Sea` `TransportLine`
/// (`logistics::compute_transport_flow`'s own doc), not a region `Link`'s
/// `strait_zone`. `world.supply[region]` is also demand-bounded now - mvp's
/// default unit placement never posts one at region 1
/// (`supply_corridor_cut`'s own doc) - so a garrison unit is added there.
#[test]
fn sea_control_throttles_strait() {
    let build = |enemy_control: f32| {
        let mut world = scenario::build_world();
        // 北東北 (region 1) also reaches faction 0's industrial heartland
        // via 南東北/関東 (region 2/3, Rail) - zero that route out so the
        // 北海道—北東北 Sea line (crossing 北方海域, zone 0) is the *only*
        // high-value path into region 1, isolating the crossing's own
        // throttle instead of measuring a route that bypasses it entirely.
        for &r in &[RegionId(1), RegionId(2), RegionId(3)] {
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

        // A garrison well above the Sea line's own 6.0 capacity, so demand
        // - not the tiny appetite of a single unit - is what the crossing's
        // own throttle actually has to hold back (`region 1`'s own base is
        // also zeroed above, so none of this can come from local production
        // either - `japan47_chokepoints_still_bind`'s `station_garrison`
        // doc makes the same point).
        for i in 0..10 {
            let id = crate::ids::UnitId(world.units.len() as u32);
            world.units.push(military::Unit {
                id,
                owner: world.region(RegionId(1)).owner,
                name: format!("Garrison {i}"),
                station: Station::Region(RegionId(1)),
                movement: None,
                manpower: 1.0,
                equipment: UNIT_EQUIPMENT,
                organization: 100.0,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: Station::Region(RegionId(1)),
                experience: 0.0,
                alive: true,
            });
        }

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

// ---------------------------------------------------------------------------
// Stage 3A — 国内政治勢力 (docs/phase3-spec.md "Stage 3A")
// ---------------------------------------------------------------------------

/// design.md §2's central claim, and the regression guard for this whole
/// stage: a faction that keeps winning the war outright (net territorial
/// gain, never a loss) can still see its government collapse if it keeps
/// tightening conscription and rationing on its own population. Military
/// and Government support both get a boost from every captured region, but
/// Labor, Citizens and (via sustained unrest/shortage) Government and
/// LocalGovernment support all suffer - and those five groups outweigh
/// Military+Government's combined 0.40 influence share.
#[test]
fn winning_war_can_still_topple_government() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let start_regions = world.region_count(faction);

    let mut targets: Vec<RegionId> = world
        .regions
        .iter()
        .filter(|r| r.owner != faction)
        .map(|r| r.id)
        .collect();
    targets.sort_by_key(|r| r.0);

    let mut events = Vec::new();
    let mut regime_changed = false;
    let mut day = 0u32;
    while day < 800 && !regime_changed {
        // Every 40 days, capture one more region outright and never give
        // any back - a faction that is unambiguously winning the war.
        let mut region_delta = vec![0i32; world.factions.len()];
        if day % 40 == 0 {
            if let Some(target) = targets.pop() {
                world.region_mut(target).owner = faction;
                region_delta[faction.index()] = 1;
            }
        }

        // Reapplied every tick: a regime change resets policy to its
        // comfortable default, and the point of this test is that the
        // government keeps squeezing anyway (and keeps paying for it).
        {
            let f = world.faction_mut(faction);
            f.conscription = 1.0;
            f.civilian_ration = CIVILIAN_RATION_MIN;
            f.shortage = 0.9;
        }
        for r in world.regions_of(faction) {
            world.region_mut(r).unrest = 95.0;
        }

        // Sustained daily war deaths despite winning - a real war still
        // costs lives even while the front line only moves one way.
        let casualties = vec![0.5f32, 0.0, 0.0];
        politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
        if events
            .iter()
            .any(|e| matches!(e, Event::RegimeChange { faction: f } if *f == faction))
        {
            regime_changed = true;
        }
        day += 1;
    }

    assert!(
        regime_changed,
        "expected sustained conscription and rationing to eventually topple the government \
         even while the faction keeps winning territory"
    );
    assert!(
        world.region_count(faction) > start_regions,
        "expected the faction to have gained territory, not lost it, before the government fell: \
         start={start_regions}, end={}",
        world.region_count(faction)
    );
}

/// Support must move toward a target, not accumulate - so it recovers once
/// the pressure driving it down eases, the same guarantee `unrest` already
/// has. Regression guard for the failure this stage was warned about
/// explicitly: an accumulating quantity that only ever falls would pin at a
/// floor it can never leave.
#[test]
fn support_recovers_after_policy_relaxed() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let casualties = vec![0.0f32; world.factions.len()];
    let region_delta = vec![0i32; world.factions.len()];
    let mut events = Vec::new();

    {
        let f = world.faction_mut(faction);
        f.conscription = 1.0;
        f.civilian_ration = CIVILIAN_RATION_MIN;
        f.shortage = 0.9;
    }
    for _ in 0..150 {
        politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
    }
    let squeezed_labor = world.faction(faction).group_support[Group::Labor.index()];
    let squeezed_citizens = world.faction(faction).group_support[Group::Citizens.index()];
    assert!(
        squeezed_labor < 40.0 && squeezed_citizens < 40.0,
        "sanity: sustained conscription+rationing should depress Labor/Citizens support: \
         labor={squeezed_labor}, citizens={squeezed_citizens}"
    );

    {
        let f = world.faction_mut(faction);
        f.conscription = 0.2;
        f.civilian_ration = 1.0;
        f.shortage = 0.0;
    }
    for _ in 0..300 {
        politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
    }
    let recovered_labor = world.faction(faction).group_support[Group::Labor.index()];
    let recovered_citizens = world.faction(faction).group_support[Group::Citizens.index()];

    assert!(
        recovered_labor > squeezed_labor + 15.0,
        "expected Labor support to recover once conscription/rationing eased, not sit pinned: \
         squeezed={squeezed_labor}, recovered={recovered_labor}"
    );
    assert!(
        recovered_citizens > squeezed_citizens + 15.0,
        "expected Citizens support to recover once conscription/rationing eased, not sit pinned: \
         squeezed={squeezed_citizens}, recovered={recovered_citizens}"
    );
    assert!(
        recovered_labor > 45.0 && recovered_citizens > 45.0,
        "expected support to recover close to its unpressured baseline, not merely off its floor: \
         labor={recovered_labor}, citizens={recovered_citizens}"
    );
}

/// docs/phase3-spec.md "政権交代の扱い": a regime change resets policy
/// (`conscription`/`civilian_ration`/`industry_priority`/
/// `logistics_priority`/`import_plan`) and `war_support` to their scenario
/// defaults, but must never touch territory, units, or stock - it's a
/// penalty for the policy stance a faction built up, not a board-destroying
/// one.
#[test]
fn regime_change_resets_policy_not_territory() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let default_faction = scenario::build_world().factions[faction.index()].clone();

    let regions_before: Vec<FactionId> = world.regions.iter().map(|r| r.owner).collect();
    let units_before: Vec<(f32, f32)> = world
        .units
        .iter()
        .map(|u| (u.manpower, u.equipment))
        .collect();
    let stock_before = world.faction(faction).stock;

    {
        let f = world.faction_mut(faction);
        f.conscription = 0.95;
        f.civilian_ration = CIVILIAN_RATION_MIN;
        f.industry_priority[Good::Munitions.index()] = 0.9;
        f.industry_priority[Good::Machinery.index()] = 0.1;
        f.logistics_priority[Good::Arms.index()] = 0.9;
        f.logistics_priority[Good::Munitions.index()] = 0.1;
        f.import_plan[Good::Food.index()] = 12.0;
        f.war_support = 90.0;
    }

    let mut events = Vec::new();
    let region_delta = vec![0i32; world.factions.len()];
    let casualties = vec![0.5f32, 0.0, 0.0];
    let mut regime_changed = false;
    for _ in 0..300 {
        {
            let f = world.faction_mut(faction);
            f.shortage = 0.95;
        }
        for r in world.regions_of(faction) {
            world.region_mut(r).unrest = 95.0;
        }
        politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
        if events
            .iter()
            .any(|e| matches!(e, Event::RegimeChange { faction: f } if *f == faction))
        {
            regime_changed = true;
            break;
        }
    }
    assert!(
        regime_changed,
        "expected sustained hardship to trigger a regime change within 300 days"
    );

    let f = world.faction(faction);
    assert_eq!(f.conscription, default_faction.conscription, "conscription must reset to default");
    assert_eq!(
        f.civilian_ration, default_faction.civilian_ration,
        "civilian_ration must reset to default"
    );
    assert_eq!(
        f.industry_priority, default_faction.industry_priority,
        "industry_priority must reset to default"
    );
    assert_eq!(
        f.logistics_priority, default_faction.logistics_priority,
        "logistics_priority must reset to default"
    );
    assert_eq!(f.import_plan, default_faction.import_plan, "import_plan must reset to default");
    assert_eq!(f.war_support, 50.0, "war_support must reset to 50");

    let regions_after: Vec<FactionId> = world.regions.iter().map(|r| r.owner).collect();
    assert_eq!(regions_after, regions_before, "regime change must not move any territory");
    let units_after: Vec<(f32, f32)> = world
        .units
        .iter()
        .map(|u| (u.manpower, u.equipment))
        .collect();
    assert_eq!(units_after, units_before, "regime change must not touch any unit");
    assert_eq!(world.faction(faction).stock, stock_before, "regime change must not touch stock");
}

/// External code review fix (Stage 3A, Fix 1): `protest_active`/
/// `mutiny_active`/`capital_flight_active` are computed earlier in the same
/// `apply_political_events` pass, from *this tick's* freshly-updated
/// `group_support` - if a regime change then resets every group's support
/// to the 50 baseline (above every one of those events' thresholds) without
/// also clearing the flags, they'd keep applying their effects
/// (`military::tick_recovery`'s mutiny org-regen penalty,
/// `economy::tick_economy`/`construction::tick_construction`'s capital-flight
/// penalties) for one extra day against a condition that's no longer true.
#[test]
fn regime_change_clears_live_event_flags() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    // Support deep enough below every live-condition threshold (and low
    // enough, weighted, to put `stability` under `REGIME_CHANGE_THRESHOLD`)
    // that a single `GROUP_ADAPT_RATE`-sized step this tick can't lift it
    // back out - so protest/mutiny/capital-flight are all still live at the
    // moment regime change fires in this same tick.
    world.faction_mut(faction).group_support = [5.0; GROUP_COUNT];
    world.faction_mut(faction).regime_change_days = 0;

    let mut events = Vec::new();
    let region_delta = vec![0i32; world.factions.len()];
    let casualties = vec![0.0f32; world.factions.len()];
    politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::RegimeChange { faction: f } if *f == faction)),
        "sanity: expected the deeply unpopular starting state to trigger a regime change this tick"
    );

    let f = world.faction(faction);
    assert_eq!(f.group_support, [GROUP_SUPPORT_BASELINE; GROUP_COUNT]);
    assert_eq!(f.stability, GROUP_SUPPORT_BASELINE);
    assert!(!f.protest_active, "protest_active must not survive a regime change's support reset");
    assert!(!f.mutiny_active, "mutiny_active must not survive a regime change's support reset");
    assert!(
        !f.capital_flight_active,
        "capital_flight_active must not survive a regime change's support reset"
    );
}

/// docs/phase3-spec.md "政治イベント": `Event::Strike` (Labor support below
/// `STRIKE_THRESHOLD`) depresses non-Food industrial output for
/// `STRIKE_DAYS`.
#[test]
fn strike_reduces_industrial_output() {
    let build = |striking: bool| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                region.capacity = [0.0; GOOD_COUNT];
                // Energy is deliberately abundant so it never binds Steel's
                // output - only the strike multiplier (and efficiency,
                // identical in both runs) should move the result.
                region.capacity[Good::Energy.index()] = 1000.0;
                region.capacity[Good::Steel.index()] = 10.0;
                region.infrastructure = 1.0;
                region.unrest = 0.0;
            }
        }
        {
            let f = world.faction_mut(faction);
            f.stability = 100.0;
            f.stock = [0.0; GOOD_COUNT];
            // Ample so civilian Food/Machinery demand never competes with
            // (or distorts) the Steel measurement below.
            f.stock[Good::Food.index()] = 1_000_000.0;
            f.stock[Good::Machinery.index()] = 1_000_000.0;
            if striking {
                f.strike_days = STRIKE_DAYS;
            }
        }
        // Machinery/Munitions/Arms capacity stays zero, so nothing consumes
        // the Steel this produces - the final stock is exactly this tick's
        // Steel output.
        economy::tick_economy(&mut world);
        world.faction(faction).stock[Good::Steel.index()]
    };

    let steel_normal = build(false);
    let steel_striking = build(true);

    assert!(steel_normal > 0.0, "sanity: normal operation should produce some Steel: {steel_normal}");
    assert!(
        steel_striking < steel_normal * (STRIKE_OUTPUT_MULT + 0.05),
        "expected an active strike to depress non-Food industrial output by roughly \
         STRIKE_OUTPUT_MULT: normal={steel_normal}, striking={steel_striking}"
    );
}

/// Stage 3C playtest fix (regression guard for the ninth §0-shaped defect:
/// seeds 3 and 5 both froze two surviving factions at maximum `shortage` for
/// 220+ days). `balance::FOOD_EFFICIENCY_FLOOR`'s doc has the full account -
/// at the worst political state a region can be in (maximum unrest, minimum
/// stability), Food must still produce a meaningful fraction of its
/// capacity, unlike every other commodity, which is allowed to collapse
/// toward the shared `efficiency * stability_mult` floor. Steel is measured
/// alongside Food, at the same infrastructure/labor/unrest/stability, as the
/// contrast: this is Food being specifically exempted, not every commodity
/// getting gentler.
#[test]
fn food_output_survives_political_collapse() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let food_capacity = 10.0;
    let steel_capacity = 10.0;
    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
            region.capacity[Good::Food.index()] = food_capacity;
            region.capacity[Good::Steel.index()] = steel_capacity;
            region.infrastructure = 1.0;
            region.unrest = 100.0; // maximum unrest
            region.devastation = 0.0; // owned and undevastated - the property under test
            region.population = 1.0; // negligible civilian demand, isolates production
        }
    }
    {
        let f = world.faction_mut(faction);
        f.stability = 0.0; // minimum stability
        f.stock = [0.0; GOOD_COUNT];
    }
    let total_food_capacity: f32 = world
        .regions
        .iter()
        .filter(|r| r.owner == faction)
        .map(|r| r.capacity[Good::Food.index()])
        .sum();

    economy::tick_economy(&mut world);

    let f = world.faction(faction);
    let food_output = f.stock[Good::Food.index()];
    let steel_output = f.stock[Good::Steel.index()];

    assert!(
        food_output >= total_food_capacity * FOOD_EFFICIENCY_FLOOR - 0.01,
        "expected Food output to respect FOOD_EFFICIENCY_FLOOR even at maximum unrest and \
         minimum stability: output={food_output}, capacity={total_food_capacity}, \
         floor={FOOD_EFFICIENCY_FLOOR}"
    );
    assert!(
        food_output > total_food_capacity * 0.5,
        "expected an owned, undevastated region to still produce a meaningful fraction of its \
         Food capacity under maximum political collapse: output={food_output}, \
         capacity={total_food_capacity}"
    );
    assert!(
        steel_output < food_output * 0.4,
        "expected industrial output (Steel) to collapse far harder than Food under the exact \
         same political conditions - Food's weaker sensitivity is the fix, not a general \
         loosening: steel={steel_output}, food={food_output}"
    );
}

/// Companion to `food_output_survives_political_collapse`: the floor that
/// protects Food from unrest/stability must NOT protect it from physical
/// destruction of the land. `Region::effective_capacity`'s `* (1 -
/// devastation)` term is untouched by the Stage 3C fix, so a devastated
/// region's Food output must still fall far below an otherwise-identical
/// intact region's, even though both sit at the same (calm) political
/// state - war damage stays a real, locally-caused loss.
#[test]
fn devastation_still_destroys_food() {
    let build = |devastation: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                region.capacity = [0.0; GOOD_COUNT];
                region.capacity[Good::Food.index()] = 10.0;
                region.infrastructure = 1.0;
                region.unrest = 0.0;
                region.devastation = devastation;
                region.population = 1.0; // negligible civilian demand
            }
        }
        world.faction_mut(faction).stability = 100.0;
        world.faction_mut(faction).stock = [0.0; GOOD_COUNT];

        economy::tick_economy(&mut world);
        world.faction(faction).stock[Good::Food.index()]
    };

    let food_intact = build(0.0);
    let food_devastated = build(0.9);

    assert!(food_intact > 0.0, "sanity: an intact, calm region should produce Food: {food_intact}");
    assert!(
        food_devastated < food_intact * 0.2,
        "expected heavy devastation to still gut Food output despite calm political conditions: \
         intact={food_intact}, devastated={food_devastated}"
    );
}

/// Stage 3C playtest fix, the regression guard for the whole defect: a
/// faction driven to maximum `shortage` by political collapse must be able
/// to climb back out on its own once the fighting that caused it stops and
/// unrest/stability are free to recover - seeds 3 and 5's playtest run
/// instead froze two surviving factions at `shortage == 1.0` for 220+ days,
/// a state neither could ever leave (docs/phase3-spec.md §0's absorbing-
/// state rule). `FOOD_EFFICIENCY_FLOOR` is exactly what breaks the
/// chicken-and-egg deadlock: without it, production can't recover until
/// unrest decays, and unrest can't decay until production (and therefore
/// shortage) recovers - a loop with no floor to climb out from. War damage
/// (`Region::devastation`) is included here, not just unrest/stability,
/// because that is how the actual playtested collapse compounds: devastated
/// land produces little, the shortfall keeps unrest pinned high, and
/// `construction::tick_devastation_recovery`'s own recovery rate is itself
/// gated on unrest being low - so this test also exercises devastation
/// healing once the political floor gives the faction enough Food to let
/// unrest start easing.
#[test]
fn collapsed_faction_can_recover() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
            region.infrastructure = 1.0;
            region.unrest = 100.0;
            if region.id == capital {
                region.capacity[Good::Food.index()] = 5.0;
                region.capacity[Good::Energy.index()] = 1000.0;
                // Machinery output is an input-constrained *output* of the
                // Stage 2A chain (economy.rs), not a direct read of its own
                // capacity - it needs Steel (and Energy) to actually turn
                // into product, so Steel capacity is set abundant here too,
                // otherwise `machinery_shortage` would stay pinned at 1.0
                // forever regardless of how much Food/political conditions
                // recover, which isn't the property this test is about.
                region.capacity[Good::Steel.index()] = 1000.0;
                region.capacity[Good::Machinery.index()] = 1000.0;
                region.devastation = 0.97;
                region.population = 500.0;
            } else {
                // Negligible population so the rest of the faction's
                // regions don't add demand this test isn't measuring.
                region.population = 1.0;
            }
        }
    }
    {
        let f = world.faction_mut(faction);
        f.stock = [0.0; GOOD_COUNT];
        f.group_support = [0.0; GROUP_COUNT];
        f.stability = 0.0;
        f.conscription = 0.0;
        f.civilian_ration = CIVILIAN_RATION_MAX;
    }

    let n = world.factions.len();
    let casualties = vec![0.0f32; n];
    let region_delta = vec![0i32; n];
    let mut events = Vec::new();

    // Day 0: still fully collapsed - devastated land, maximum unrest,
    // minimum stability, zero stock.
    economy::tick_economy(&mut world);
    politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
    let shortage_start = world.faction(faction).shortage;
    assert!(
        shortage_start > 0.8,
        "sanity: devastated land at maximum unrest, minimum stability and zero stock should \
         start this faction in severe shortage: {shortage_start}"
    );

    // No more fighting: casualties and region deltas stay zero for the rest
    // of the run (the same "pressure eases" setup
    // `unrest_recovers_after_shortage`/`support_recovers_after_policy_relaxed`
    // already use), letting unrest, stability and devastation all recover on
    // their own.
    for _ in 0..500 {
        economy::tick_economy(&mut world);
        politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);
        construction::tick_devastation_recovery(&mut world);
    }

    let shortage_end = world.faction(faction).shortage;
    assert!(
        shortage_end < shortage_start - 0.5,
        "expected a collapsed faction to climb back out of maximum shortage once fighting \
         stopped, not stay pinned near it - the absorbing state this whole fix exists to break: \
         start={shortage_start}, end={shortage_end}"
    );
    assert!(
        shortage_end < 0.3,
        "expected shortage to recover to a comfortable level, not just drift off the ceiling: \
         {shortage_end}"
    );
}

/// docs/phase8-spec.md Fix 2's follow-up (`balance::INDUSTRIAL_STABILITY_FLOOR`'s
/// doc): companion to `food_output_survives_political_collapse` for every
/// commodity Food's own Stage 3C fix didn't touch. At the true worst case
/// (`efficiency`'s own 0.2 floor - needs `infrastructure == 0.0`, not just
/// `unrest == 100.0`, to actually bottom out; `region.labor_ratio()` stays
/// at its own ceiling of `1.0` with no conscription - and `stability == 0`,
/// `stability_output_mult`'s own 0.6 floor) a non-Food good's compound
/// multiplier used to be pinned at exactly `0.2 * 0.6 = 0.12`x capacity,
/// the same value `FOOD_EFFICIENCY_FLOOR`'s doc found insufficient for
/// Food. Energy is the cleanest commodity to measure this precisely on -
/// like Food, it has no production inputs (`economy.rs`'s Step 2), so
/// giving the region *only* Energy capacity means nothing else can ever
/// draw on the stock this test reads. This confirms Energy now clears
/// `INDUSTRIAL_STABILITY_FLOOR`'s higher `0.2 * 0.7 = 0.14`x instead -
/// strictly above the old bound, so this test fails outright if
/// `INDUSTRIAL_STABILITY_FLOOR` regresses to `stability_output_mult`'s own
/// `0.6` (`industrial_output_outpaces_fixed_demand_only_with_new_floor`
/// below covers Steel/Munitions, whose shared Energy/Steel inputs make an
/// equally precise single-good formula impractical to isolate the same
/// way).
#[test]
fn industrial_output_respects_higher_stability_floor_than_food_used_to_get() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let energy_capacity = 1000.0;
    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
            region.capacity[Good::Energy.index()] = energy_capacity;
            region.infrastructure = 0.0; // together with unrest, drives `efficiency` to its true 0.2 floor
            region.unrest = 100.0;
            region.devastation = 0.0; // isolates political disorder from war damage
            region.population = 1.0; // negligible civilian demand, isolates production
        }
    }
    {
        let f = world.faction_mut(faction);
        f.stability = 0.0; // stability_output_mult's own 0.6 floor
        f.stock = [0.0; GOOD_COUNT];
    }
    let total_energy_capacity: f32 = world
        .regions
        .iter()
        .filter(|r| r.owner == faction)
        .map(|r| r.capacity[Good::Energy.index()])
        .sum();

    economy::tick_economy(&mut world);

    let energy_output = world.faction(faction).stock[Good::Energy.index()];

    let old_floor_mult = 0.2 * 0.6; // pre-fix compound: efficiency's floor times stability_output_mult's own floor
    let new_floor_mult = 0.2 * INDUSTRIAL_STABILITY_FLOOR;
    assert!(
        new_floor_mult > old_floor_mult,
        "sanity: INDUSTRIAL_STABILITY_FLOOR must actually raise the compound floor"
    );

    assert!(
        energy_output >= total_energy_capacity * new_floor_mult - 0.5,
        "expected Energy output to respect INDUSTRIAL_STABILITY_FLOOR at minimum stability and \
         maximum unrest: output={energy_output}, capacity={total_energy_capacity}, \
         expected_mult={new_floor_mult}"
    );
    assert!(
        energy_output > total_energy_capacity * old_floor_mult + 0.5,
        "expected the new stability floor to lift Energy output strictly above the old, \
         unprotected 0.6-stability-floor compound (0.12x capacity) - fails if \
         INDUSTRIAL_STABILITY_FLOOR regresses to stability_output_mult's own floor: \
         output={energy_output}, old_floor_output={}",
        total_energy_capacity * old_floor_mult
    );
}

/// Companion to `industrial_output_respects_higher_stability_floor_than_food_used_to_get`:
/// the whole point of `docs/conventions.md` §6 ("状態には必ず回復経路を持たせる")
/// is that a faction stuck at the *old* 0.12x compound floor could never
/// outrun even a small, fixed daily draw - production never crossed the
/// threshold needed to net positive, so stock stayed pinned at exactly zero
/// no matter how long the faction waited. This pins `unrest`/`stability` at
/// their absolute worst values for the entire run (never calling
/// `politics::tick_politics` - `unrest_recovers_after_shortage` and
/// `collapsed_faction_can_recover` already cover unrest/stability actually
/// easing over time; this isolates the production side) and applies a
/// fixed daily draw to Steel and Munitions calibrated to sit strictly
/// between the old floor's output (12/day off a capacity of 100) and the
/// new floor's (14/day) - so this test fails outright if
/// `INDUSTRIAL_STABILITY_FLOOR` regresses to 0.6: at the old floor the
/// faction could never accumulate a single unit of stock over any number of
/// days, let alone the 50 here.
#[test]
fn industrial_output_outpaces_fixed_demand_only_with_new_floor() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    // `daily_draw`'s calibration below is against a *national* Steel/
    // Munitions capacity of 100 each - split evenly across however many
    // regions faction 0 actually owns, so the total stays 100 regardless of
    // scenario detail.
    let n_owned = world.regions.iter().filter(|r| r.owner == faction).count() as f32;
    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
            region.capacity[Good::Energy.index()] = 1000.0 / n_owned;
            region.capacity[Good::Steel.index()] = 100.0 / n_owned;
            region.capacity[Good::Munitions.index()] = 100.0 / n_owned;
            region.infrastructure = 0.0;
            region.unrest = 100.0;
            region.devastation = 0.0;
            region.population = 1.0;
        }
    }
    {
        let f = world.faction_mut(faction);
        f.stability = 0.0;
        f.stock = [0.0; GOOD_COUNT];
        f.industry_priority = [0.0; GOOD_COUNT];
        f.industry_priority[Good::Steel.index()] = 1.0;
        f.industry_priority[Good::Munitions.index()] = 1.0;
    }

    // Munitions' own gross production is 12/day at the old floor (100 * 0.2
    // * 0.6) and 14/day at the new one (100 * 0.2 *
    // INDUSTRIAL_STABILITY_FLOOR) - nothing else draws on it within
    // `economy::tick_economy`, so 13/day (strictly between) is calibrated
    // directly against that. Steel is also spent as Munitions' own input
    // (`MUNITIONS_INPUT_STEEL`) before this test's draw ever sees it, so
    // what's left over nets to 8.4/day (old) vs 9.8/day (new) - 9.1/day is
    // calibrated against *that* net figure instead of Steel's own gross
    // production.
    let munitions_draw = 13.0;
    let steel_draw = 9.1;
    for _ in 0..50 {
        economy::tick_economy(&mut world);
        let f = world.faction_mut(faction);
        f.stock[Good::Steel.index()] = (f.stock[Good::Steel.index()] - steel_draw).max(0.0);
        f.stock[Good::Munitions.index()] = (f.stock[Good::Munitions.index()] - munitions_draw).max(0.0);
    }

    let f = world.faction(faction);
    assert!(
        f.stock[Good::Steel.index()] > 10.0,
        "expected Steel production to outpace a fixed daily draw calibrated to sit above the old \
         (pre-fix) floor's output - if this is ~0.0 the faction is still trapped exactly the way \
         Food used to be: steel_stock={}",
        f.stock[Good::Steel.index()]
    );
    assert!(
        f.stock[Good::Munitions.index()] > 10.0,
        "expected Munitions production to outpace the same fixed daily draw - Munitions is the \
         commodity docs/phase8-spec.md Fix 2's japan_hex report was actually about: \
         munitions_stock={}",
        f.stock[Good::Munitions.index()]
    );
}

/// Companion to the two tests above: the new stability floor must not blunt
/// war damage. `Region::effective_capacity`'s `(1 - devastation)` term is
/// completely untouched by `INDUSTRIAL_STABILITY_FLOOR` - it multiplies raw
/// `capacity` before any of `efficiency`/`stability_output_mult` ever runs
/// - so a devastated region's non-Food output must still fall far below an
/// otherwise-identical intact region's, even under calm political
/// conditions where the new floor plays no role at all (mirrors
/// `devastation_still_destroys_food` for the commodities that test doesn't
/// cover).
#[test]
fn devastation_still_suppresses_industrial_output() {
    let build = |devastation: f32| {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        for region in world.regions.iter_mut() {
            if region.owner == faction {
                region.capacity = [0.0; GOOD_COUNT];
                region.capacity[Good::Energy.index()] = 10.0;
                region.infrastructure = 1.0;
                region.unrest = 0.0;
                region.devastation = devastation;
                region.population = 1.0; // negligible civilian demand
            }
        }
        world.faction_mut(faction).stability = 100.0;
        world.faction_mut(faction).stock = [0.0; GOOD_COUNT];

        economy::tick_economy(&mut world);
        world.faction(faction).stock[Good::Energy.index()]
    };

    let energy_intact = build(0.0);
    let energy_devastated = build(0.9);

    assert!(
        energy_intact > 0.0,
        "sanity: an intact, calm region should produce Energy: {energy_intact}"
    );
    assert!(
        energy_devastated < energy_intact * 0.2,
        "expected heavy devastation to still gut Energy output despite calm political conditions \
         and INDUSTRIAL_STABILITY_FLOOR: intact={energy_intact}, devastated={energy_devastated}"
    );
}

/// docs/phase3-spec.md "地方独立運動": an occupied region (`core != owner`)
/// left with neglected LocalGovernment support - and no units defending it
/// - drifts back to its core faction on its own.
#[test]
fn separatism_returns_occupied_region() {
    let mut world = scenario::build_world();
    let occupier = FactionId(1);
    let core_faction = FactionId(0);
    let region_id = RegionId(0);

    world.region_mut(region_id).owner = occupier;
    assert_eq!(
        world.region(region_id).core,
        core_faction,
        "sanity: region 0's core stays faction 0 even though its owner just changed"
    );

    // Guarantee no unit of any faction sits in the occupied region -
    // separatism only ever acts on a region nobody is physically contesting.
    let capitals: Vec<RegionId> = world.factions.iter().map(|f| f.capital).collect();
    for unit in world.units.iter_mut() {
        if unit.station == Station::Region(region_id) {
            unit.station = Station::Region(capitals[unit.owner.index()]);
            unit.movement = None;
        }
    }

    world.faction_mut(occupier).group_support[Group::LocalGovernment.index()] =
        SEPARATISM_THRESHOLD - 5.0;

    let mut events = Vec::new();
    let mut reverted = false;
    for _ in 0..100 {
        politics::tick_separatism(&mut world, &mut events);
        if world.region(region_id).owner == core_faction {
            reverted = true;
            break;
        }
    }

    assert!(
        reverted,
        "expected a neglected occupied region to revert to its core faction via separatism"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Separatism { region, from, to }
                if *region == region_id && *from == occupier && *to == core_faction
        )),
        "expected an Event::Separatism naming the reverted region to be logged"
    );
}

/// External code review fix (Stage 3A, Fix 2/3): the owner's own garrison
/// stationed in a region under active separatism must slow the drift, never
/// veto it outright - the old code skipped `tick_separatism` entirely for
/// any region with so much as one unit present, which let a garrison freeze
/// the meter for free (docs/phase3-spec.md §0's absorbing-state rule). A
/// garrison should cost something (the supply/manpower it ties down) rather
/// than being a free, permanent political shield.
#[test]
fn garrison_slows_but_does_not_stop_separatism() {
    let occupier = FactionId(1);
    let region_id = RegionId(0);

    let build = |garrison: bool| {
        let mut world = scenario::build_world();
        world.region_mut(region_id).owner = occupier;

        // Clear out whatever units the scenario started in this region so
        // the only unit present (if any) is the garrison this test adds.
        let capitals: Vec<RegionId> = world.factions.iter().map(|f| f.capital).collect();
        for unit in world.units.iter_mut() {
            if unit.station == Station::Region(region_id) {
                unit.station = Station::Region(capitals[unit.owner.index()]);
                unit.movement = None;
            }
        }
        if garrison {
            let id = crate::ids::UnitId(world.units.len() as u32);
            world.units.push(military::Unit {
                id,
                owner: occupier,
                name: "Test Garrison".to_string(),
                station: Station::Region(region_id),
                movement: None,
                manpower: 1.0,
                equipment: UNIT_EQUIPMENT,
                organization: 100.0,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: Station::Region(region_id),
                experience: 0.0,
                alive: true,
            });
        }

        world.faction_mut(occupier).group_support[Group::LocalGovernment.index()] =
            SEPARATISM_THRESHOLD - 5.0;

        let mut events = Vec::new();
        for _ in 0..10 {
            politics::tick_separatism(&mut world, &mut events);
        }
        world.region(region_id).occupation
    };

    let no_garrison = build(false);
    let garrisoned = build(true);

    assert!(
        no_garrison > 0.0,
        "sanity: separatism should advance in an undefended region: {no_garrison}"
    );
    assert!(garrisoned > 0.0, "expected a garrison to slow separatism, not stop it: {garrisoned}");
    assert!(
        garrisoned < no_garrison,
        "expected separatism to advance slower with a garrison present than with none: \
         garrisoned={garrisoned} no_garrison={no_garrison}"
    );
}

/// External code review fix (Stage 3A, Fix 3): regression guard for a
/// playtested absorbing state - a faction that has taken on more territory
/// (and the population that comes with it) than it can feed has, pre-Fix-2/
/// 3, no way out: regime change only resets policy (it doesn't create
/// food), and a garrisoned occupier used to veto separatism outright. With
/// Fix 2/3 (a garrison slows separatism instead of stopping it,
/// docs/phase3-spec.md §0's escape-hatch rule), occupied territory an
/// over-extended faction can't hold drifts back to its still-alive `core`
/// even while garrisoned, which is the population's actual way out - and
/// that way out must show up as `Faction::shortage_by_good` actually
/// recovering, not merely as the territory changing hands (a weaker guard
/// that dropped units from every handed-over region entirely would never
/// exercise `garrison_slows_but_does_not_stop_separatism`'s fix at all: it's
/// only meaningful if a garrison is still standing there when the territory
/// finally sheds).
///
/// Constructed directly - handing 西方同盟 (`FactionId(2)`) all three of
/// 中央同盟's (`FactionId(1)`) regions, stationing a 西方同盟 garrison in one
/// of them, and forcing LocalGovernment support below `SEPARATISM_THRESHOLD`,
/// the same pattern `separatism_returns_occupied_region`/
/// `garrison_slows_but_does_not_stop_separatism` above already use - rather
/// than through a full, AI-driven 720-day run: Stage 3B's diplomacy AI
/// (`archipelago-agents`) makes a full run's emergent history too sensitive
/// to reliably reproduce this specific crisis on demand (western 西方同盟's
/// only land neighbor is 中央同盟, so a perfectly reasonable `Ceasefire`
/// between them - which docs/phase3-spec.md explicitly wants seeds to
/// produce - removes the only front it could ever over-extend across in the
/// first place).
#[test]
fn overextended_faction_sheds_unaffordable_territory_via_separatism() {
    let mut world = scenario::build_world();
    let watched = FactionId(2);
    let core = FactionId(1);
    let handed_over = [RegionId(4), RegionId(5), RegionId(6)];
    // 信越・北陸: the least populous of the three, so a garrison here
    // suppresses `separatist_rate` the hardest (`SEPARATISM_GARRISON_MAX_
    // SUPPRESSION`'s cap) - the slowest-shedding case this guard can put in
    // front of a garrison, still not a veto.
    let garrisoned_region = RegionId(4);

    for &r in &handed_over {
        world.region_mut(r).owner = watched;
        // A real conquest would leave exactly this behind
        // (`military::tick_occupation`'s `DEVASTATION_ON_CAPTURE`/
        // `CAPTURE_UNREST` on the region that just changed hands) - this
        // test's direct, costless `owner` reassignment skips the fighting
        // that would normally produce it, so it's set explicitly instead.
        world.region_mut(r).devastation = DEVASTATION_ON_CAPTURE;
        world.region_mut(r).unrest = CAPTURE_UNREST;
        // Stage 3C playtest fix (`balance::FOOD_EFFICIENCY_FLOOR`'s doc):
        // Food no longer takes the same unrest/stability-driven hit every
        // other commodity does, so `DEVASTATION_ON_CAPTURE`/`CAPTURE_UNREST`
        // alone (the realistic amount a single capture leaves) are no
        // longer enough to manufacture a famine here - 西方同盟's own three
        // regions are close enough to self-sufficient that even a modest
        // Food contribution from the other three covers the gap. This test
        // is about separatism shedding unaffordable territory, not about
        // Food's political sensitivity, so the "can't feed it" premise is
        // now built the same structural way `imports_feed_food_poor_faction`/
        // `blockaded_faction_starves` already do: these regions' farmland
        // itself, not their political state, can't support the population
        // that comes with them.
        world.region_mut(r).capacity[Good::Food.index()] = 0.0;
    }
    // Strips the starting `scenario::FACTION_STOCK` Food reserve, which
    // would otherwise cushion a single day's shortfall and mask exactly the
    // property under test (today's production against today's need) behind
    // a buffer that has nothing to do with whether the territory itself is
    // affordable.
    world.faction_mut(watched).stock[Good::Food.index()] = 0.0;
    // `core`'s original units, still sitting in what are now `watched`-owned
    // regions, would otherwise be a foreign, at-war presence there and hand
    // each region's meter to `military::tick_occupation` instead of
    // `politics::tick_separatism` (see `tick_separatism`'s `foreign_present`
    // check). Killed rather than relocated: `handed_over` is *all three* of
    // `core`'s starting regions (`scenario::FACTION_SPECS`'s
    // `中央同盟: regions: &[4, 5, 6]`) - including its own capital
    // (region 6) - so every one of `core`'s units is already inside
    // `handed_over` and there is no safe own region left to move any of
    // them to (unlike `separatism_returns_occupied_region`'s single-region
    // handover, where the owner's capital survives untouched). `core`
    // itself stays untouched otherwise (still `alive` - `Faction::alive` is
    // only ever set by `Simulation::step`'s elimination check, which this
    // test never calls).
    for unit in world.units.iter_mut() {
        if unit.owner == core {
            unit.alive = false;
        }
    }

    // A 西方同盟 garrison, left standing in `garrisoned_region` for the
    // entire run - this is the fix under test: the garrison must slow that
    // region's separatism (`garrison_slows_but_does_not_stop_separatism`)
    // but never prevent it from eventually shedding along with the other
    // two, ungarrisoned regions.
    let garrison_id = crate::ids::UnitId(world.units.len() as u32);
    world.units.push(military::Unit {
        id: garrison_id,
        owner: watched,
        name: "Test Garrison".to_string(),
        station: Station::Region(garrisoned_region),
        movement: None,
        manpower: 1.0,
        equipment: UNIT_EQUIPMENT,
        organization: 100.0,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(garrisoned_region),
        experience: 0.0,
        alive: true,
    });

    world.faction_mut(watched).group_support[Group::LocalGovernment.index()] =
        SEPARATISM_THRESHOLD - 5.0;

    // Baseline: while still over-extended (six regions, three of them
    // freshly conquered and devastated/unrested, its Food reserve already
    // spent), a single `economy::tick_economy` pass should show a real,
    // measurable Food shortage - the starving state this guard needs
    // shedding to recover from.
    economy::tick_economy(&mut world);
    let shortage_before = world.faction(watched).shortage_by_good[Good::Food.index()];
    assert!(
        shortage_before > 0.01,
        "sanity: an over-extended faction holding territory it can't feed should show a \
         measurable Food shortage before it sheds anything: shortage_by_good[Food]={shortage_before}"
    );

    let mut events = Vec::new();
    let mut shed_territory = false;
    for _ in 0..600 {
        politics::tick_separatism(&mut world, &mut events);
        if handed_over.iter().all(|&r| world.region(r).owner == core) {
            shed_territory = true;
            break;
        }
    }

    assert!(
        shed_territory,
        "expected every handed-over region to eventually drift back to its still-alive core \
         faction via separatism, even the one 西方同盟 garrisons"
    );
    assert_eq!(
        world.region_count(watched),
        3,
        "shedding the unaffordable territory should return the faction to exactly its \
         original three regions"
    );
    assert!(
        events.iter().any(
            |e| matches!(e, Event::Separatism { region, from, to } if *region == garrisoned_region && *from == watched && *to == core)
        ),
        "expected the garrisoned region specifically to still revert via Event::Separatism, \
         proving the garrison slowed it rather than vetoing it"
    );

    // Recovery, not just the territory loss: back at its sustainable
    // footprint (its original three regions and starting stock), a single
    // `economy::tick_economy` pass should serve civilian Food demand in
    // full - the same baseline `starting_factions_are_not_in_shortage`
    // already establishes for every faction at day 0, now reached again
    // after shedding what it couldn't afford to hold. The comparison
    // against `shortage_before` is the property that actually matters here:
    // not just "some small number", but a real recovery from real distress.
    economy::tick_economy(&mut world);
    let shortage_after = world.faction(watched).shortage_by_good[Good::Food.index()];
    assert!(
        shortage_after < shortage_before * 0.1,
        "expected shortage to recover substantially once the unaffordable territory (and the \
         population it carried) was shed: before={shortage_before}, after={shortage_after}"
    );
    assert!(
        shortage_after < 0.01,
        "expected shortage to recover once the unaffordable territory (and the population it \
         carried) was shed: shortage_by_good[Food]={shortage_after}"
    );
}

/// docs/phase3-spec.md "安定度の再定義": `stability` must always equal the
/// influence-weighted average of `group_support`, not an independently
/// tracked variable.
#[test]
fn stability_is_weighted_group_support() {
    let mut world = scenario::build_world();
    let casualties = vec![0.0f32; world.factions.len()];
    let region_delta = vec![0i32; world.factions.len()];
    let mut events = Vec::new();
    politics::tick_politics(&mut world, &casualties, &region_delta, &mut events);

    for faction in &world.factions {
        let expected: f32 = (0..GROUP_COUNT)
            .map(|g| faction.group_influence[g] * faction.group_support[g])
            .sum();
        assert!(
            (faction.stability - expected).abs() < 1e-3,
            "stability should equal the influence-weighted average of group_support for \
             faction {}: stability={}, expected={expected}",
            faction.id.0,
            faction.stability
        );
    }
}

// ---------------------------------------------------------------------------
// Stage 3B — 外交関係と条約 (docs/phase3-spec.md "Stage 3B"). `Simulation`
// isn't needed for most of these - `action::apply_action`/`diplomacy::
// tick_diplomacy`/`military::tick_combat`/`military::tick_occupation`/
// `trade::tick_imports` are called directly, the same style every earlier
// Stage 2/3A test in this file already uses.
// ---------------------------------------------------------------------------

/// `Observation::encode()`'s only length check in the whole codebase - it's
/// never called from `apps/headless` or the AI, only by external RL/LLM
/// consumers (design.md §15/§18), so nothing else exercises the
/// `debug_assert_eq!` inside `encode()` itself. Stage 3B extended
/// `ENCODING_LEN`'s formula with the new per-relation diplomacy block - this
/// confirms the two stay in sync for every faction's own view, not just one.
#[test]
fn observation_encoding_matches_declared_length() {
    let world = scenario::build_world();
    for faction in &world.factions {
        let obs = Observation { faction: faction.id, world: &world };
        assert_eq!(obs.encode().len(), ENCODING_LEN);
    }
}

/// `Stance::Ceasefire` (docs/phase3-spec.md "Ceasefire": "戦闘・占領が発生し
/// ない"): two factions sharing a region must not fight once a Ceasefire is
/// signed between them, even though the default scenario would otherwise
/// have them fight every tick (every other combat test in this file
/// exercises exactly that default).
#[test]
fn ceasefire_stops_combat() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);

    action::apply_action(&mut world, a, Action::ProposeTreaty { to: b, treaty: Treaty::Ceasefire })
        .unwrap();
    action::apply_action(&mut world, b, Action::AcceptTreaty { from: a, treaty: Treaty::Ceasefire })
        .unwrap();
    assert_eq!(world.diplomacy.stance(a, b), crate::diplomacy::Stance::Ceasefire);

    // Same setup `combat_reduces_organization` uses: put a faction-1 unit
    // into region 3, which already has faction-0 units.
    let intruder = world.units.iter().position(|u| u.owner == b).unwrap();
    world.units[intruder].station = Station::Region(RegionId(3));
    world.units[intruder].movement = None;
    let org_before: Vec<f32> = world.units.iter().map(|u| u.organization).collect();
    let devastation_before = world.region(RegionId(3)).devastation;

    let mut rng = Rng::new(1);
    let mut events = Vec::new();
    let report = military::tick_combat(&mut world, &mut rng, &mut events);

    assert!(!report.fought[intruder], "a Ceasefire partner's unit must not fight");
    assert_eq!(world.units[intruder].organization, org_before[intruder]);
    assert_eq!(world.region(RegionId(3)).devastation, devastation_before);
    assert!(
        !events.iter().any(|e| matches!(e, Event::Battle { region, .. } if *region == RegionId(3))),
        "no Battle event should be logged for a region with no warring pair present"
    );

    // Occupation must not advance either - the intruder physically present
    // in foreign territory, at peace, is not an invasion.
    for _ in 0..5 {
        military::tick_occupation(&mut world, &mut events);
    }
    assert_eq!(world.region(RegionId(3)).owner, FactionId(0));
    assert_eq!(world.region(RegionId(3)).occupation, 0.0);
}

/// `Stance::Alliance` (docs/phase3-spec.md: "同盟国が攻撃されたら自動参戦す
/// る"): when faction `a` (allied with `c`) goes back to war with `b`,
/// `c` - previously at `Ceasefire` with `b`, not at war at all - is dragged
/// into that war too.
#[test]
fn alliance_drags_into_war() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);
    let c = FactionId(2);

    for (x, y) in [(a, c), (b, c)] {
        action::apply_action(&mut world, x, Action::ProposeTreaty { to: y, treaty: Treaty::Ceasefire })
            .unwrap();
        action::apply_action(&mut world, y, Action::AcceptTreaty { from: x, treaty: Treaty::Ceasefire })
            .unwrap();
    }
    action::apply_action(&mut world, a, Action::ProposeTreaty { to: b, treaty: Treaty::Alliance })
        .unwrap();
    action::apply_action(&mut world, b, Action::AcceptTreaty { from: a, treaty: Treaty::Alliance })
        .unwrap();
    assert_eq!(world.diplomacy.stance(a, b), crate::diplomacy::Stance::Alliance);
    assert!(!world.diplomacy.is_at_war(b, c), "sanity: b and c start at Ceasefire, not War");

    action::apply_action(&mut world, a, Action::DeclareWar { to: c }).unwrap();

    assert!(world.diplomacy.is_at_war(a, c), "a declared war on c directly");
    assert!(
        world.diplomacy.is_at_war(b, c),
        "b, allied with a, should be dragged into a's war with c even though b and c were at \
         Ceasefire"
    );
    assert!(
        world
            .diplomacy
            .log
            .iter()
            .any(|e| matches!(e, Event::AllianceDragIn { faction, into_war_with } if *faction == b && *into_war_with == c)),
        "expected an Event::AllianceDragIn naming b's forced entry into the war with c"
    );
}

/// `Treaty::MilitaryAccess` (docs/phase3-spec.md: "相手領を通過できる（占領は
/// 発生しない）"): a faction can sit in a granted region indefinitely without
/// that presence ever starting an occupation, even while the two remain at
/// `Stance::War` - the default, permanent-war scenario every other
/// occupation test in this file relies on stays valid without this grant
/// (`occupation_flips_owner` uses the exact same region-8 setup and *does*
/// flip ownership).
#[test]
fn military_access_allows_transit_without_occupation() {
    let mut world = scenario::build_world();
    let owner = FactionId(2);
    let visitor = FactionId(0);

    action::apply_action(
        &mut world,
        owner,
        Action::ProposeTreaty { to: visitor, treaty: Treaty::MilitaryAccess },
    )
    .unwrap();
    action::apply_action(
        &mut world,
        visitor,
        Action::AcceptTreaty { from: owner, treaty: Treaty::MilitaryAccess },
    )
    .unwrap();
    assert!(world.diplomacy.is_at_war(owner, visitor), "sanity: still at War by default");

    // Region 8 (Shikoku) starts undefended by faction 2 - same setup
    // `occupation_flips_owner` uses.
    let mover = world.units.iter().position(|u| u.owner == visitor).unwrap();
    world.units[mover].station = Station::Region(RegionId(8));
    world.units[mover].movement = None;

    let mut events = Vec::new();
    for _ in 0..10 {
        military::tick_occupation(&mut world, &mut events);
    }

    assert_eq!(
        world.region(RegionId(8)).owner,
        owner,
        "MilitaryAccess should let the visitor sit in region 8 indefinitely without capturing it"
    );
    assert_eq!(world.region(RegionId(8)).occupation, 0.0);
}

/// `Treaty::PortAccess` (docs/phase3-spec.md: "相手の港を自国の輸入容量とし
/// て使える"): granting it measurably raises the grantee's usable import
/// capacity - the same request, against the same grantee, actually lands
/// more Food with the grant than without it.
#[test]
fn port_access_adds_import_capacity() {
    let grantor = FactionId(0);
    let grantee = FactionId(2);

    let build = |grant: bool| {
        let mut world = scenario::build_world();
        world.faction_mut(grantee).import_plan[Good::Food.index()] = 1000.0;
        world.faction_mut(grantee).stock[Good::Machinery.index()] = 100_000.0;
        if grant {
            action::apply_action(
                &mut world,
                grantor,
                Action::ProposeTreaty { to: grantee, treaty: Treaty::PortAccess },
            )
            .unwrap();
            action::apply_action(
                &mut world,
                grantee,
                Action::AcceptTreaty { from: grantor, treaty: Treaty::PortAccess },
            )
            .unwrap();
        }
        trade::tick_imports(&mut world);
        world.faction(grantee).stock[Good::Food.index()]
    };

    let without_grant = build(false);
    let with_grant = build(true);

    assert!(
        with_grant > without_grant,
        "PortAccess should let the grantee import more than its own ports alone allow: \
         without={without_grant}, with={with_grant}"
    );
}

/// `Treaty::TradeAgreement` (docs/phase3-spec.md: "余剰のある側から不足のあ
/// る側へ、港湾容量の範囲で流れる"): a surplus faction's stock actually moves
/// to a deficit partner once the agreement is signed.
#[test]
fn trade_agreement_moves_surplus_to_deficit() {
    let mut world = scenario::build_world();
    let surplus = FactionId(0);
    let deficit = FactionId(1);

    world.faction_mut(surplus).stock[Good::Food.index()] = 1000.0;
    world.faction_mut(deficit).shortage_by_good[Good::Food.index()] = 1.0;
    let surplus_food_before = world.faction(surplus).stock[Good::Food.index()];
    let deficit_food_before = world.faction(deficit).stock[Good::Food.index()];

    action::apply_action(
        &mut world,
        surplus,
        Action::ProposeTreaty { to: deficit, treaty: Treaty::TradeAgreement },
    )
    .unwrap();
    action::apply_action(
        &mut world,
        deficit,
        Action::AcceptTreaty { from: surplus, treaty: Treaty::TradeAgreement },
    )
    .unwrap();

    trade::tick_imports(&mut world);

    let surplus_food_after = world.faction(surplus).stock[Good::Food.index()];
    let deficit_food_after = world.faction(deficit).stock[Good::Food.index()];

    assert!(
        deficit_food_after > deficit_food_before,
        "the deficit faction should receive Food: before={deficit_food_before}, \
         after={deficit_food_after}"
    );
    assert!(
        surplus_food_after < surplus_food_before,
        "the surplus faction should give up exactly what it sent: before={surplus_food_before}, \
         after={surplus_food_after}"
    );
    let sent = surplus_food_before - surplus_food_after;
    let received = deficit_food_after - deficit_food_before;
    assert!(
        (sent - received).abs() < 1e-3,
        "the flow must conserve the good exactly: sent={sent}, received={received}"
    );
}

/// `opinion` (docs/phase3-spec.md "関係": "-100..100 の非対称な感情"):
/// `Action::BreakTreaty` must damage it, never improve or leave it
/// unchanged - the opposite of what accepting a treaty does.
#[test]
fn breaking_treaty_damages_opinion() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);

    action::apply_action(
        &mut world,
        a,
        Action::ProposeTreaty { to: b, treaty: Treaty::MilitaryAccess },
    )
    .unwrap();
    action::apply_action(
        &mut world,
        b,
        Action::AcceptTreaty { from: a, treaty: Treaty::MilitaryAccess },
    )
    .unwrap();
    assert!(world.diplomacy.has_treaty(a, b, Treaty::MilitaryAccess));
    let opinion_a_before = world.diplomacy.opinion(a, b);
    let opinion_b_before = world.diplomacy.opinion(b, a);

    action::apply_action(&mut world, a, Action::BreakTreaty { with: b, treaty: Treaty::MilitaryAccess })
        .unwrap();

    assert!(!world.diplomacy.has_treaty(a, b, Treaty::MilitaryAccess));
    assert!(
        world.diplomacy.opinion(a, b) < opinion_a_before,
        "breaker's opinion of the other side should drop: before={opinion_a_before}, \
         after={}",
        world.diplomacy.opinion(a, b)
    );
    assert!(
        world.diplomacy.opinion(b, a) < opinion_b_before,
        "the other side's opinion of the breaker should drop: before={opinion_b_before}, \
         after={}",
        world.diplomacy.opinion(b, a)
    );
    assert!(
        world
            .diplomacy
            .log
            .iter()
            .any(|e| matches!(e, Event::TreatyBroken { a: x, b: y, treaty: Treaty::MilitaryAccess } if *x == a && *y == b)),
        "expected an Event::TreatyBroken to be logged"
    );
}

/// docs/phase3-spec.md "条約": "一方的な提案だけでは条約が成立しない" - a
/// proposal with no matching `AcceptTreaty` never becomes an active treaty,
/// and expires on its own after `PROPOSAL_TTL_DAYS` (docs/phase3-spec.md:
/// "提案は 1 tick 保留され、相手の応答を待つ").
#[test]
fn proposal_requires_acceptance() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);

    action::apply_action(&mut world, a, Action::ProposeTreaty { to: b, treaty: Treaty::Ceasefire })
        .unwrap();

    assert!(
        !world.diplomacy.has_treaty(a, b, Treaty::Ceasefire),
        "a lone proposal must not itself establish the treaty"
    );
    assert!(world.diplomacy.find_pending(a, b).is_some(), "the proposal should be pending");

    // Nobody ever answers it - `tick_diplomacy` (`Simulation::step`'s
    // once-a-day diplomacy maintenance) should expire it on its own.
    let mut events = Vec::new();
    for _ in 0..5 {
        diplomacy::tick_diplomacy(&mut world, &mut events);
    }

    assert!(
        !world.diplomacy.has_treaty(a, b, Treaty::Ceasefire),
        "still no treaty after the unanswered proposal expired"
    );
    assert!(
        world.diplomacy.find_pending(a, b).is_none(),
        "an unanswered proposal should expire rather than stay pending forever"
    );
}

/// External code review fix A1: `Stance::Ceasefire` must stop naval combat
/// exactly the way `ceasefire_stops_combat` already proves it stops land
/// combat - before this fix `tick_naval_combat` had no stance awareness at
/// all, so fleets of factions under Ceasefire/NonAggression/Alliance kept
/// fighting every tick regardless of what treaty they'd signed.
#[test]
fn ceasefire_stops_naval_combat() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);
    let zone = SeaZoneId(0);

    action::apply_action(&mut world, a, Action::ProposeTreaty { to: b, treaty: Treaty::Ceasefire })
        .unwrap();
    action::apply_action(&mut world, b, Action::AcceptTreaty { from: a, treaty: Treaty::Ceasefire })
        .unwrap();
    assert_eq!(world.diplomacy.stance(a, b), crate::diplomacy::Stance::Ceasefire);

    // Any two units, repurposed as fleets sharing a zone - same technique
    // `naval_combat_sinks_fleet` uses.
    let fleet_a = world.units.iter().find(|u| u.owner == a).unwrap().id;
    let fleet_b = world.units.iter().find(|u| u.owner == b).unwrap().id;
    world.unit_mut(fleet_a).station = Station::Sea(zone);
    world.unit_mut(fleet_a).movement = None;
    world.unit_mut(fleet_b).station = Station::Sea(zone);
    world.unit_mut(fleet_b).movement = None;

    assert!(
        !world.has_enemy_fleets(zone, a),
        "a Ceasefire partner's fleet must no longer count as an enemy fleet"
    );

    let org_before = world.unit(fleet_a).organization;
    let mut rng = Rng::new(1);
    let mut events = Vec::new();
    let report = naval::tick_naval_combat(&mut world, &mut rng, &mut events);

    assert!(!report.fought[fleet_a.index()], "a Ceasefire partner's fleet must not fight");
    assert!(!report.fought[fleet_b.index()], "a Ceasefire partner's fleet must not fight");
    assert_eq!(world.unit(fleet_a).organization, org_before);
    assert!(
        !events.iter().any(|e| matches!(e, Event::NavalBattle { zone: z, .. } if *z == zone)),
        "no NavalBattle event should be logged for a zone with no warring pair present"
    );
}

/// External code review fix A1: a faction at peace with a port's owner must
/// never blockade that port, no matter how completely it dominates the sea
/// zones the port faces - the contrast case for `blockade_stops_import`
/// (identical setup, still at War, still blockades).
#[test]
fn peace_partner_port_is_not_blockaded() {
    let mut world = scenario::build_world();
    let owner = FactionId(1); // owns 信越・北陸(4), 東海(5), 近畿(6)
    let peace_partner = FactionId(0);
    let still_at_war = FactionId(2);

    action::apply_action(
        &mut world,
        owner,
        Action::ProposeTreaty { to: peace_partner, treaty: Treaty::Ceasefire },
    )
    .unwrap();
    action::apply_action(
        &mut world,
        peace_partner,
        Action::AcceptTreaty { from: owner, treaty: Treaty::Ceasefire },
    )
    .unwrap();
    assert_eq!(world.diplomacy.stance(owner, peace_partner), crate::diplomacy::Stance::Ceasefire);
    assert!(world.diplomacy.is_at_war(owner, still_at_war), "sanity: still at War by default");

    // peace_partner (faction 0's slot) holds full, uncontested control of
    // every sea zone touching 東海 (region 5) - well past
    // BLOCKADE_CONTROL_THRESHOLD.
    for zone in world.zones_touching(RegionId(5)) {
        world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];
    }
    assert!(
        !naval::is_port_blockaded(&world, RegionId(5)),
        "a Ceasefire partner's dominant sea control must not blockade the port"
    );

    // The identical control, attributed to a faction still at War instead,
    // must still blockade - confirming the difference above is the Stance,
    // not something else this test changed.
    for zone in world.zones_touching(RegionId(5)) {
        world.sea_zone_mut(zone).control = vec![0.0, 0.0, 1.0];
    }
    assert!(
        naval::is_port_blockaded(&world, RegionId(5)),
        "sanity: the same dominant control from a faction still at War must still blockade"
    );
}

/// External code review fix A2: crossed bilateral proposals (`a` proposes a
/// treaty to `b` while `b` independently proposes the same treaty to `a`,
/// before either has answered) must not let the signing bonus be paid
/// twice - accepting one direction activates the treaty and must silently
/// retire the now-redundant reverse proposal, and even if it somehow
/// survived, accepting an already-active treaty a second time must be
/// rejected outright.
#[test]
fn crossed_proposals_pay_signing_bonus_once() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);

    action::apply_action(
        &mut world,
        a,
        Action::ProposeTreaty { to: b, treaty: Treaty::NonAggression },
    )
    .unwrap();
    action::apply_action(
        &mut world,
        b,
        Action::ProposeTreaty { to: a, treaty: Treaty::NonAggression },
    )
    .unwrap();
    assert!(world.diplomacy.find_pending(a, b).is_some(), "sanity: a's proposal to b is pending");
    assert!(world.diplomacy.find_pending(b, a).is_some(), "sanity: b's proposal to a is pending");

    let opinion_a_before = world.diplomacy.opinion(a, b);
    let opinion_b_before = world.diplomacy.opinion(b, a);

    // a accepts b's proposal first, activating the treaty and paying the
    // bonus exactly once.
    action::apply_action(
        &mut world,
        a,
        Action::AcceptTreaty { from: b, treaty: Treaty::NonAggression },
    )
    .unwrap();
    assert_eq!(world.diplomacy.stance(a, b), crate::diplomacy::Stance::NonAggression);
    let opinion_a_after = world.diplomacy.opinion(a, b);
    let opinion_b_after = world.diplomacy.opinion(b, a);
    assert!(opinion_a_after > opinion_a_before, "the signing bonus should have been paid once");
    assert!(opinion_b_after > opinion_b_before, "the signing bonus should have been paid once");

    assert!(
        world.diplomacy.find_pending(a, b).is_none(),
        "the now-obsolete reverse-direction proposal must be cleared once the treaty activates"
    );
    let result = action::apply_action(
        &mut world,
        b,
        Action::AcceptTreaty { from: a, treaty: Treaty::NonAggression },
    );
    assert!(
        result.is_err(),
        "accepting an already-active treaty a second time must be rejected, not silently succeed"
    );

    assert_eq!(
        world.diplomacy.opinion(a, b), opinion_a_after,
        "the signing bonus must not be paid a second time for one treaty"
    );
    assert_eq!(
        world.diplomacy.opinion(b, a), opinion_b_after,
        "the signing bonus must not be paid a second time for one treaty"
    );
}

/// External code review fix A3: re-proposing the exact same treaty that's
/// already pending in the same direction - the shape a machine agent
/// spamming `ProposeTreaty` every tick would produce - must be a true no-op:
/// no TTL refresh (which would keep the proposal alive forever) and no
/// second `Event::TreatyProposed` (which would flood the event log).
#[test]
fn repeated_proposal_does_not_refresh_or_relog() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);

    action::apply_action(&mut world, a, Action::ProposeTreaty { to: b, treaty: Treaty::Ceasefire })
        .unwrap();
    assert_eq!(
        world.diplomacy.log.iter().filter(|e| matches!(e, Event::TreatyProposed { .. })).count(),
        1,
        "sanity: the first proposal logs exactly one TreatyProposed event"
    );

    // Let a day pass so the ttl actually counts down from its initial value
    // - otherwise a refreshed proposal and an untouched one would look
    // identical.
    let mut events = Vec::new();
    diplomacy::tick_diplomacy(&mut world, &mut events);
    let ttl_after_one_day = world.diplomacy.pending[world.diplomacy.find_pending(a, b).unwrap()].ttl;

    // Re-propose the identical (from, to, treaty) repeatedly, the way a
    // machine agent hammering the same action every tick would.
    for _ in 0..5 {
        action::apply_action(&mut world, a, Action::ProposeTreaty { to: b, treaty: Treaty::Ceasefire })
            .unwrap();
    }

    assert!(
        world.diplomacy.log.is_empty(),
        "repeated proposals for an already-pending treaty must not log a second \
         Event::TreatyProposed: {:?}",
        world.diplomacy.log
    );
    let idx = world.diplomacy.find_pending(a, b).expect("the proposal should still be pending");
    assert_eq!(
        world.diplomacy.pending[idx].ttl, ttl_after_one_day,
        "repeated proposals for an already-pending treaty must not refresh its ttl"
    );
}

// ---------------------------------------------------------------------------
// Stage 3C — 国家方針 (docs/phase3-spec.md "Stage 3C — 国家方針")
// ---------------------------------------------------------------------------

/// `Action::SetNationalFocus` (docs/phase3-spec.md: "変更には FOCUS_SWITCH_
/// DAYS の移行期間があり、その間は効果が出ない"): the new focus must not be
/// `focus::active` immediately, must stay inactive for the whole transition,
/// and must become active only once `FOCUS_SWITCH_DAYS` have actually
/// elapsed.
#[test]
fn focus_switch_has_transition_period() {
    let mut world = scenario::build_world();
    let f = FactionId(0);

    action::apply_action(&mut world, f, Action::SetNationalFocus(NationalFocus::MaritimeTrade))
        .unwrap();
    assert_eq!(world.faction(f).national_focus, NationalFocus::MaritimeTrade);
    assert!(
        focus::active(world.faction(f)).is_none(),
        "the new focus must not take effect the instant it's set"
    );

    for day in 0..(FOCUS_SWITCH_DAYS - 1) {
        focus::tick_national_focus(&mut world);
        assert!(
            focus::active(world.faction(f)).is_none(),
            "day {day}: should still be mid-transition"
        );
    }
    focus::tick_national_focus(&mut world);
    assert_eq!(
        focus::active(world.faction(f)),
        Some(NationalFocus::MaritimeTrade),
        "the focus should be active once FOCUS_SWITCH_DAYS have fully elapsed"
    );
}

/// `NationalFocus::MilitaryUnification` (docs/phase3-spec.md: "Military 支持
/// ＋、Citizens 支持 −"): once active, `politics::tick_politics` should push
/// Military support up and Citizens support down relative to a faction with
/// no support-affecting focus active.
#[test]
fn focus_affects_group_support() {
    let run = |focus: NationalFocus| {
        let mut world = scenario::build_world();
        let f = FactionId(0);
        world.faction_mut(f).national_focus = focus;
        world.faction_mut(f).focus_transition_days = 0;
        let n = world.factions.len();
        let mut events = Vec::new();
        politics::tick_politics(&mut world, &vec![0.0; n], &vec![0i32; n], &mut events);
        world.faction(f).group_support
    };

    // `AllianceNetwork` has no group-support line at all (see
    // `scenario::FACTION_NATIONAL_FOCUS_DEFAULT`'s doc) - a clean control
    // that isolates MilitaryUnification's specific contribution.
    let control = run(NationalFocus::AllianceNetwork);
    let militarized = run(NationalFocus::MilitaryUnification);

    assert!(
        militarized[Group::Military.index()] > control[Group::Military.index()],
        "MilitaryUnification should raise Military support: control={}, militarized={}",
        control[Group::Military.index()],
        militarized[Group::Military.index()]
    );
    assert!(
        militarized[Group::Citizens.index()] < control[Group::Citizens.index()],
        "MilitaryUnification should lower Citizens support: control={}, militarized={}",
        control[Group::Citizens.index()],
        militarized[Group::Citizens.index()]
    );
}

/// `NationalFocus::MaritimeTrade` (docs/phase3-spec.md: "港湾の輸入容量＋"):
/// once active, a faction should be able to import more through its own
/// ports than an otherwise-identical faction without the focus.
#[test]
fn maritime_trade_increases_import_capacity() {
    let run = |focus: Option<NationalFocus>| {
        let mut world = scenario::build_world();
        let f = FactionId(0);
        world.faction_mut(f).import_plan[Good::Food.index()] = 1000.0;
        world.faction_mut(f).stock[Good::Machinery.index()] = 100_000.0;
        if let Some(focus) = focus {
            world.faction_mut(f).national_focus = focus;
            world.faction_mut(f).focus_transition_days = 0;
        }
        trade::tick_imports(&mut world);
        world.faction(f).stock[Good::Food.index()]
    };

    let baseline = run(None);
    let maritime = run(Some(NationalFocus::MaritimeTrade));

    assert!(
        maritime > baseline,
        "MaritimeTrade should increase how much Food actually lands through this faction's own \
         ports: baseline={baseline}, maritime={maritime}"
    );
}

/// `NationalFocus::DefensivePosture` (docs/phase3-spec.md: "自領での防御補正
/// ＋"): an intruder attacking this faction on its own `core` soil should
/// take more damage than the same attack against an otherwise-identical
/// defender with no focus-driven combat bonus.
#[test]
fn defensive_posture_improves_home_defense() {
    let run = |focus: Option<NationalFocus>| {
        let mut world = scenario::build_world();
        let defender = FactionId(0);
        if let Some(focus) = focus {
            world.faction_mut(defender).national_focus = focus;
            world.faction_mut(defender).focus_transition_days = 0;
        }

        // Region 3 (関東) is faction 0's capital and `core` territory - put
        // a lone faction-1 intruder there alongside faction 0's own
        // defenders, mirroring `combat_reduces_organization`'s setup.
        let intruder = world.units.iter().position(|u| u.owner == FactionId(1)).unwrap();
        world.units[intruder].station = Station::Region(RegionId(3));
        world.units[intruder].movement = None;

        let mut rng = Rng::new(1);
        let mut events = Vec::new();
        military::tick_combat(&mut world, &mut rng, &mut events);
        world.units[intruder].organization
    };

    let baseline_org = run(None);
    let defended_org = run(Some(NationalFocus::DefensivePosture));

    assert!(
        defended_org < baseline_org,
        "an intruder attacking a DefensivePosture faction's home soil should take more damage \
         (lose more organization) than against an otherwise-identical defender: \
         baseline={baseline_org}, defended={defended_org}"
    );
}

/// docs/phase3-spec.md §0's abuse-resistance rule, applied to `Action::
/// SetNationalFocus` specifically: rapid repeated calls - retargeting mid-
/// transition, re-affirming the same target over and over - must never
/// produce a different `national_focus`/`focus_transition_days` outcome, nor
/// a different day the focus actually becomes active, than a single call.
#[test]
fn rapid_focus_switching_gains_no_advantage() {
    let f = FactionId(0);

    let mut world_single = scenario::build_world();
    action::apply_action(&mut world_single, f, Action::SetNationalFocus(NationalFocus::Technocracy))
        .unwrap();

    let mut world_spam = scenario::build_world();
    action::apply_action(&mut world_spam, f, Action::SetNationalFocus(NationalFocus::Technocracy))
        .unwrap();
    for _ in 0..10 {
        // Retargeting mid-transition must be rejected outright...
        assert!(
            action::apply_action(&mut world_spam, f, Action::SetNationalFocus(NationalFocus::MaritimeTrade))
                .is_err(),
            "retargeting an in-progress focus switch should be rejected"
        );
        // ...and re-affirming the same target must be a harmless no-op, not
        // a timer reset.
        action::apply_action(&mut world_spam, f, Action::SetNationalFocus(NationalFocus::Technocracy))
            .unwrap();
    }

    assert_eq!(world_single.faction(f).national_focus, world_spam.faction(f).national_focus);
    assert_eq!(
        world_single.faction(f).focus_transition_days,
        world_spam.faction(f).focus_transition_days,
        "spamming SetNationalFocus must not shorten (or lengthen) the transition already in progress"
    );

    for day in 0..(FOCUS_SWITCH_DAYS + 2) {
        focus::tick_national_focus(&mut world_single);
        focus::tick_national_focus(&mut world_spam);
        assert_eq!(
            focus::active(world_single.faction(f)).is_some(),
            focus::active(world_spam.faction(f)).is_some(),
            "day {day}: spamming SetNationalFocus must activate the focus on exactly the same \
             day as a single call"
        );
    }
    assert_eq!(focus::active(world_single.faction(f)), Some(NationalFocus::Technocracy));
}


/// Stage 4B (docs/phase4-spec.md "Stage 4B の受け入れ基準":
/// "llm_cannot_bypass_treaty_validation"): an interpretation that says
/// "accept" is not the same thing as the deal taking effect - every
/// `TreatyTerm` is re-validated against the *current* board regardless of
/// what the recipient's agent (LLM or otherwise) decided, exactly the way
/// `apply_accept_treaty` already revalidates a treaty proposal. Covers both
/// of the spec's own example shapes: a region that doesn't exist at all, and
/// one that exists but the proposer doesn't own.
#[test]
fn llm_cannot_bypass_treaty_validation() {
    // Shape 1: cede a region id that is out of range entirely.
    {
        let mut world = scenario::build_world();
        let a = FactionId(0);
        let b = FactionId(1);
        action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
            to: b,
            text: "give me your land".to_string(),
        })
        .unwrap();

        let owners_before: Vec<FactionId> = world.regions.iter().map(|r| r.owner).collect();
        let result = action::apply_action(
            &mut world,
            b,
            Action::RespondToNaturalLanguageProposal {
                from: a,
                terms: vec![TreatyTerm::Cede { region: RegionId(9_999) }],
                accept: true,
            },
        );
        assert_eq!(result, Ok(()), "answering a proposal is itself always a well-formed action");
        let owners_after: Vec<FactionId> = world.regions.iter().map(|r| r.owner).collect();
        assert_eq!(
            owners_before, owners_after,
            "a term naming a nonexistent region must never change the board, even though the \
             recipient's interpretation said \"accept\""
        );
        assert!(
            world.diplomacy.find_pending_nl(a, b).is_none(),
            "the proposal must still be consumed even though the deal didn't take effect"
        );
    }

    // Shape 2: cede a region that exists, but the proposer doesn't own.
    {
        let mut world = scenario::build_world();
        let a = FactionId(0); // owns regions 0..=3
        let b = FactionId(1);
        let not_owned_by_a = RegionId(4); // owned by faction 1, not faction 0
        assert_ne!(world.region(not_owned_by_a).owner, a);

        action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
            to: b,
            text: "I'll cede land I don't actually hold".to_string(),
        })
        .unwrap();

        let owner_before = world.region(not_owned_by_a).owner;
        action::apply_action(
            &mut world,
            b,
            Action::RespondToNaturalLanguageProposal {
                from: a,
                terms: vec![TreatyTerm::Cede { region: not_owned_by_a }],
                accept: true,
            },
        )
        .unwrap();
        assert_eq!(
            world.region(not_owned_by_a).owner, owner_before,
            "a term ceding a region the proposer doesn't own must never change its owner"
        );
    }
}

/// External code review fix (`apply_treaty_terms`'s doc: batch validation
/// must be cumulative, not "validate every term against the unchanged world,
/// then apply them all"): two `Deliver` terms that each individually fit
/// inside the proposer's stock, but together overdraw it, must be rejected
/// as a whole - never partially applied, and never allowed to drive the
/// stock negative.
#[test]
fn duplicate_deliver_terms_cannot_overdraw_stock() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);
    world.faction_mut(a).stock[Good::Steel.index()] = 100.0;
    let stock_before_a = world.faction(a).stock[Good::Steel.index()];
    let stock_before_b = world.faction(b).stock[Good::Steel.index()];

    action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
        to: b,
        text: "here, take some steel".to_string(),
    })
    .unwrap();

    let result = action::apply_action(
        &mut world,
        b,
        Action::RespondToNaturalLanguageProposal {
            from: a,
            terms: vec![
                TreatyTerm::Deliver { good: Good::Steel, amount: 75.0 },
                TreatyTerm::Deliver { good: Good::Steel, amount: 75.0 },
            ],
            accept: true,
        },
    );
    assert_eq!(result, Ok(()), "answering a proposal is itself always a well-formed action");
    assert_eq!(
        world.faction(a).stock[Good::Steel.index()], stock_before_a,
        "two Delivers that each individually fit but together overdraw the proposer's stock must \
         leave the stock completely unchanged, never negative and never partially spent"
    );
    assert_eq!(
        world.faction(b).stock[Good::Steel.index()], stock_before_b,
        "the recipient must not receive anything from a batch that was rejected as a whole"
    );
}

/// External code review fix: an exact duplicate `Sign(treaty)` term in one
/// response must not pay `TREATY_ACCEPT_OPINION_BONUS` twice for a single
/// signing - see `apply_treaty_terms`'s doc on why `Sign` is deduplicated
/// (idempotent, like re-proposing an already-pending treaty) rather than
/// treated as a second concession.
#[test]
fn duplicate_sign_terms_pay_bonus_once() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);
    action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
        to: b,
        text: "let's have peace".to_string(),
    })
    .unwrap();

    let opinion_before_ab = world.diplomacy.opinion(a, b);
    let opinion_before_ba = world.diplomacy.opinion(b, a);
    action::apply_action(&mut world, b, Action::RespondToNaturalLanguageProposal {
        from: a,
        terms: vec![TreatyTerm::Sign(Treaty::Ceasefire), TreatyTerm::Sign(Treaty::Ceasefire)],
        accept: true,
    })
    .unwrap();

    assert_eq!(
        world.diplomacy.stance(a, b),
        crate::diplomacy::Stance::Ceasefire,
        "the treaty should still take effect exactly once"
    );
    assert_eq!(
        world.diplomacy.opinion(a, b), opinion_before_ab + TREATY_ACCEPT_OPINION_BONUS,
        "a duplicated Sign term in one response must pay the acceptance bonus exactly once, not twice"
    );
    assert_eq!(
        world.diplomacy.opinion(b, a), opinion_before_ba + TREATY_ACCEPT_OPINION_BONUS,
        "the bonus must be paid exactly once on the other side too"
    );
}

/// External code review fix: a batch mixing one otherwise-valid term with
/// one genuinely infeasible term must change nothing at all - not even the
/// valid term applies. The same all-or-nothing guarantee
/// `llm_cannot_bypass_treaty_validation` checks for a single bad term,
/// extended to a mixed batch to confirm cumulative validation doesn't let a
/// later failure leave an earlier term's effects in place.
#[test]
fn conflicting_terms_are_rejected_atomically() {
    let mut world = scenario::build_world();
    let a = FactionId(0); // owns regions 0..=3
    let b = FactionId(1);
    let not_owned_by_a = RegionId(4); // owned by faction 1, not faction 0
    assert_ne!(world.region(not_owned_by_a).owner, a);

    action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
        to: b,
        text: "peace, plus land I don't actually own".to_string(),
    })
    .unwrap();

    let stance_before = world.diplomacy.stance(a, b);
    let opinion_before_ab = world.diplomacy.opinion(a, b);
    let opinion_before_ba = world.diplomacy.opinion(b, a);
    let owner_before = world.region(not_owned_by_a).owner;

    action::apply_action(&mut world, b, Action::RespondToNaturalLanguageProposal {
        from: a,
        terms: vec![TreatyTerm::Sign(Treaty::Ceasefire), TreatyTerm::Cede { region: not_owned_by_a }],
        accept: true,
    })
    .unwrap();

    assert_eq!(
        world.diplomacy.stance(a, b), stance_before,
        "the otherwise-valid Sign term must not apply when a later term in the same batch is invalid"
    );
    assert_eq!(world.diplomacy.opinion(a, b), opinion_before_ab, "no bonus must be paid when the batch is rejected");
    assert_eq!(world.diplomacy.opinion(b, a), opinion_before_ba);
    assert_eq!(
        world.region(not_owned_by_a).owner, owner_before,
        "the invalid Cede term must not change ownership either"
    );
}

/// One more abuse-resistance guard beyond the spec's three named Stage 4B
/// tests (docs/phase3-spec.md §0, and the same shapes already fixed for
/// Stage 3B's structured `ProposeTreaty`): repeated natural-language
/// proposals must not be spammable to keep a proposal alive, flood the event
/// log, or re-harvest `TREATY_ACCEPT_OPINION_BONUS` by resubmitting the same
/// deal.
#[test]
fn repeated_natural_language_proposals_cannot_farm_opinion_or_flood_events() {
    let mut world = scenario::build_world();
    let a = FactionId(0);
    let b = FactionId(1);

    action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
        to: b,
        text: "let's talk".to_string(),
    })
    .unwrap();

    // Spamming more proposals to the same target while one is outstanding
    // must all be rejected outright - no stacking, no TTL refresh.
    for _ in 0..10 {
        assert!(
            action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
                to: b,
                text: "let's talk again".to_string(),
            })
            .is_err(),
            "a repeat ProposeInNaturalLanguage while one is already pending must be rejected"
        );
    }
    assert_eq!(
        world.diplomacy.pending_nl.len(), 1,
        "spamming ProposeInNaturalLanguage must never queue more than one outstanding proposal"
    );

    let mut events = Vec::new();
    diplomacy::tick_diplomacy(&mut world, &mut events);
    let proposed_count = events.iter().filter(|e| matches!(e, Event::NaturalLanguageProposed { .. })).count();
    assert_eq!(proposed_count, 1, "only the first proposal should ever have reached the event log");

    // Accept a Sign(Ceasefire) term - the opinion-bearing path.
    let opinion_before = world.diplomacy.opinion(a, b);
    action::apply_action(&mut world, b, Action::RespondToNaturalLanguageProposal {
        from: a,
        terms: vec![TreatyTerm::Sign(Treaty::Ceasefire)],
        accept: true,
    })
    .unwrap();
    let opinion_after_first = world.diplomacy.opinion(a, b);
    assert!(opinion_after_first > opinion_before, "a genuinely accepted Sign term should raise opinion once");

    // Immediately trying to re-propose (to farm the bonus again) must be
    // blocked by the cooldown `respond_nl` just spent.
    assert!(
        action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
            to: b,
            text: "let's sign peace again".to_string(),
        })
        .is_err(),
        "re-proposing right after a response must be blocked by NL_PROPOSAL_COOLDOWN_DAYS"
    );

    // Once the cooldown has fully elapsed, a fresh proposal is allowed again,
    // but the treaty is already active - a second Sign(Ceasefire) attempt
    // must fail validation and must not pay the bonus a second time.
    let mut events2 = Vec::new();
    for _ in 0..NL_PROPOSAL_COOLDOWN_DAYS {
        diplomacy::tick_diplomacy(&mut world, &mut events2);
    }
    action::apply_action(&mut world, a, Action::ProposeInNaturalLanguage {
        to: b,
        text: "let's sign peace once more".to_string(),
    })
    .unwrap();
    let opinion_before_second_response = world.diplomacy.opinion(a, b);
    action::apply_action(&mut world, b, Action::RespondToNaturalLanguageProposal {
        from: a,
        terms: vec![TreatyTerm::Sign(Treaty::Ceasefire)],
        accept: true,
    })
    .unwrap();
    let opinion_after_second_response = world.diplomacy.opinion(a, b);
    assert_eq!(
        opinion_before_second_response, opinion_after_second_response,
        "signing an already-active treaty a second time via natural language must not grant the \
         acceptance bonus again"
    );
}

// Stage 6A (docs/phase6-spec.md "Stage 6A の受け入れ基準"): the scenario
// data-driven-ization regression guards.

/// `scenario_roundtrip`: serialising the built-in scenario and reading it
/// back must reproduce it exactly - both as a `Scenario` (structural
/// equality) and as the `World` it builds (`{:?}` equality, since `World`
/// itself doesn't derive `PartialEq` - see `Region`'s `#[derive(Debug)]`).
#[test]
fn scenario_roundtrip() {
    let original = scenario::Scenario::parse(scenario::embedded_mvp_json()).expect("embedded mvp.json parses");
    original.validate().expect("embedded mvp.json passes validation");

    let json = original.to_json();
    let reparsed = scenario::Scenario::parse(&json).expect("round-tripped JSON parses");
    assert_eq!(original, reparsed, "writing out the built-in scenario and reading it back must reproduce it exactly");

    let world_a = original.build_world();
    let world_b = reparsed.build_world();
    assert_eq!(
        format!("{world_a:?}"),
        format!("{world_b:?}"),
        "the round-tripped scenario must build a `World` identical to the original"
    );
}

/// A minimal 3-region, 2-faction, sea-zone-free scenario, valid as written -
/// every `invalid_scenario_is_rejected` case below starts from this text
/// and breaks exactly one rule, so each failure is unambiguously
/// attributable to the one thing that test changed.
const MINI_VALID_SCENARIO: &str = r#"
{
  "regions": [
    { "id": "a", "name": "A", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [0.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" } ] },
    { "id": "b", "name": "B", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [1.0, 0.0],
      "links": [ { "to": "a", "kind": "rail" }, { "to": "c", "kind": "rail" } ] },
    { "id": "c", "name": "C", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [2.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" } ] }
  ],
  "sea_zones": [],
  "transport": {
    "nodes": [
      { "id": "a_depot", "name": "A Depot", "kind": "depot", "region": "a" },
      { "id": "b_depot", "name": "B Depot", "kind": "depot", "region": "b" },
      { "id": "c_depot", "name": "C Depot", "kind": "depot", "region": "c" }
    ],
    "lines": [
      { "from": "a_depot", "to": "b_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot", "to": "c_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 }
    ]
  },
  "factions": [
    { "id": "f1", "name": "F1", "capital": "a", "regions": ["a", "b"] },
    { "id": "f2", "name": "F2", "capital": "c", "regions": ["c"] }
  ],
  "diplomacy": { "blocs": [] },
  "victory": [ { "type": "conquest" } ]
}
"#;

#[test]
fn invalid_scenario_is_rejected() {
    // Sanity check: the shared base text is actually valid, so every
    // failure below is caused by the one edit each variant makes.
    scenario::load_str(MINI_VALID_SCENARIO).expect("MINI_VALID_SCENARIO must itself be valid");

    // A one-way link: `a` lists a link to `b`, but `b`'s own link list no
    // longer lists one back to `a` (only to `c`).
    let one_way = MINI_VALID_SCENARIO.replacen(
        r#"{ "to": "a", "kind": "rail" }, { "to": "c", "kind": "rail" }"#,
        r#"{ "to": "c", "kind": "rail" }"#,
        1,
    );
    match scenario::load_str(&one_way) {
        Err(scenario::ScenarioError::OneWayLink { from, to }) => {
            assert_eq!((from.as_str(), to.as_str()), ("a", "b"));
        }
        other => panic!("expected a distinct OneWayLink error, got {other:?}"),
    }

    // A dangling id: `a` gains a second link to a region that doesn't exist.
    let dangling = MINI_VALID_SCENARIO.replacen(
        r#"{ "to": "b", "kind": "rail" } ] },
    { "id": "b""#,
        r#"{ "to": "b", "kind": "rail" }, { "to": "nowhere", "kind": "rail" } ] },
    { "id": "b""#,
        1,
    );
    match scenario::load_str(&dangling) {
        Err(scenario::ScenarioError::UnknownId { id, .. }) => assert_eq!(id, "nowhere"),
        other => panic!("expected a distinct UnknownId error, got {other:?}"),
    }

    // A disconnected region: a fourth region `d`, with no links at all,
    // claimed by `f2` alongside `c` so ownership itself stays valid.
    let disconnected = MINI_VALID_SCENARIO
        .replacen(
            r#"{ "id": "c", "name": "C""#,
            r#"{ "id": "d", "name": "D", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [3.0, 0.0], "links": [] },
    { "id": "c", "name": "C""#,
            1,
        )
        .replacen(r#""regions": ["c"]"#, r#""regions": ["c", "d"]"#, 1);
    match scenario::load_str(&disconnected) {
        Err(scenario::ScenarioError::Disconnected { unreachable }) => assert_eq!(unreachable, vec!["d".to_string()]),
        other => panic!("expected a distinct Disconnected error, got {other:?}"),
    }

    // A faction with no territory: `f2`'s `regions` becomes empty, without
    // reassigning `c` to anyone else.
    let no_territory = MINI_VALID_SCENARIO.replacen(r#""regions": ["c"]"#, r#""regions": []"#, 1);
    match scenario::load_str(&no_territory) {
        Err(scenario::ScenarioError::FactionWithoutTerritory { faction }) => assert_eq!(faction, "f2"),
        other => panic!("expected a distinct FactionWithoutTerritory error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Scenario-scoped starting diplomacy (docs/future-work.md "japan47 が 720 日
// で決着しない"): every scenario must declare its own starting `diplomacy`
// state (a required `{ "blocs": [...] }` field - see `scenario::DiplomacyDef`'s
// doc), rather than an implied "all factions start at war" default no
// scenario could opt out of. `scenarios/mvp.json`'s `"blocs": []` spells out
// exactly that same all-war baseline explicitly, which is why its `--json`
// hash is unchanged (`client_run_matches_headless`, `scenario_flag_matches_
// builtin_scenario`).
// ---------------------------------------------------------------------------

/// A `diplomacy` field naming no bloc other than an empty `[]` - required by
/// every one of the tests above via `MINI_VALID_SCENARIO` - is not the same
/// thing as the field being *absent*: a scenario that omits `diplomacy`
/// entirely must be rejected outright, naming exactly what's missing, per
/// docs/conventions.md §3 "壊れたデータで暗黙に既定値へ落ちないこと" applied
/// to this field like every other required one (`missing_scenario_position_
/// is_rejected`'s doc makes the same point for `position`).
///
/// Confirmed this can fail: temporarily changed `Scenario::parse` to treat a
/// missing `diplomacy` field as `DiplomacyDef { blocs: Vec::new() }` instead
/// of propagating `require_object_field`'s error, and re-ran - `load_str`
/// stopped returning `Err` at all for this input. Reverted before
/// committing.
#[test]
fn missing_diplomacy_declaration_is_rejected() {
    let no_diplomacy = MINI_VALID_SCENARIO.replacen(
        "  ],\n  \"diplomacy\": { \"blocs\": [] },\n  \"victory\"",
        "  ],\n  \"victory\"",
        1,
    );
    match scenario::load_str(&no_diplomacy) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("diplomacy"), "expected the error to name `diplomacy`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a missing `diplomacy` field, got {other:?}"),
    }
}

/// The `victory` sibling of `missing_diplomacy_declaration_is_rejected`:
/// a scenario that omits the required `victory` declaration entirely must
/// be rejected outright, naming exactly what's missing - per
/// docs/conventions.md §3 applied to this field the same way it already
/// applies to `diplomacy`/`position`. There is no implied default (e.g.
/// "fall back to `Conquest`") a scenario could rely on by leaving this out.
///
/// Confirmed this can fail: temporarily changed `Scenario::parse` to treat
/// a missing `victory` field as `Vec::new()` instead of propagating
/// `require_object_field`'s error, and re-ran - `load_str` stopped
/// returning `Err` at all for this input (and produced a scenario that can
/// never end in `Outcome::Victory` at all, silently). Reverted before
/// committing.
#[test]
fn missing_victory_declaration_is_rejected() {
    let no_victory = MINI_VALID_SCENARIO.replacen(
        "  \"diplomacy\": { \"blocs\": [] },\n  \"victory\": [ { \"type\": \"conquest\" } ]\n}",
        "  \"diplomacy\": { \"blocs\": [] }\n}",
        1,
    );
    match scenario::load_str(&no_victory) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("victory"), "expected the error to name `victory`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a missing `victory` field, got {other:?}"),
    }
}

/// A minimal 3-faction scenario naming one starting bloc (`f1`+`f2`) - `f3`
/// is in no bloc, so it stays at war with both by `Diplomacy::new_with_
/// blocs`'s default. Reused by `scenario_declared_alliance_behaves_like_
/// signed_treaty` below.
const BLOC_SCENARIO: &str = r#"
{
  "regions": [
    { "id": "a", "name": "A", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [0.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" } ] },
    { "id": "b", "name": "B", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [1.0, 0.0],
      "links": [ { "to": "a", "kind": "rail" }, { "to": "c", "kind": "rail" } ] },
    { "id": "c", "name": "C", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [2.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" } ] }
  ],
  "sea_zones": [],
  "transport": {
    "nodes": [
      { "id": "a_depot", "name": "A Depot", "kind": "depot", "region": "a" },
      { "id": "b_depot", "name": "B Depot", "kind": "depot", "region": "b" },
      { "id": "c_depot", "name": "C Depot", "kind": "depot", "region": "c" }
    ],
    "lines": [
      { "from": "a_depot", "to": "b_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot", "to": "c_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 }
    ]
  },
  "factions": [
    { "id": "f1", "name": "F1", "capital": "a", "regions": ["a"] },
    { "id": "f2", "name": "F2", "capital": "b", "regions": ["b"] },
    { "id": "f3", "name": "F3", "capital": "c", "regions": ["c"] }
  ],
  "diplomacy": {
    "blocs": [
      { "id": "bloc1", "name": "Bloc1", "factions": ["f1", "f2"] }
    ]
  },
  "victory": [ { "type": "conquest" }, { "type": "coalition" } ]
}
"#;

/// A bloc declared in the scenario file must produce a `Stance::Alliance`
/// indistinguishable from one signed in play - same alliance-drag-in
/// behaviour and all, not a separate, parallel notion of "starting ally"
/// (the task this fixes: reuse the Stage 3B `Stance`/`Diplomacy` model,
/// don't invent a second one). Deliberately mirrors `alliance_drags_into_
/// war` step for step; the only difference is *how* f1 and f2 became
/// allied - declared in `BLOC_SCENARIO`'s `diplomacy` field here, versus
/// `ProposeTreaty`/`AcceptTreaty` there - which is exactly what proves the
/// two paths converge on the same state.
///
/// Confirmed this can fail: temporarily made `Scenario::build_world` call
/// `Diplomacy::new` instead of `Diplomacy::new_with_blocs` (silently
/// dropping the declared bloc) and re-ran - the first assertion below
/// failed immediately (f1/f2 stayed at `Stance::War`). Reverted before
/// committing.
#[test]
fn scenario_declared_alliance_behaves_like_signed_treaty() {
    let mut world = scenario::load_str(BLOC_SCENARIO).expect("BLOC_SCENARIO must be a valid scenario");
    let f1 = FactionId(0);
    let f2 = FactionId(1);
    let f3 = FactionId(2);

    assert_eq!(
        world.diplomacy.stance(f1, f2),
        diplomacy::Stance::Alliance,
        "the scenario's declared bloc must produce Stance::Alliance without any in-play treaty"
    );
    assert!(world.diplomacy.is_at_war(f1, f3), "f3 is in no bloc, so it must still start at war with f1");
    assert!(world.diplomacy.is_at_war(f2, f3), "f3 is in no bloc, so it must still start at war with f2");

    // Same setup `alliance_drags_into_war` uses for an in-play alliance:
    // both allies sign a Ceasefire with the common rival first, so
    // `Action::DeclareWar` (only ever usable to break an active Ceasefire)
    // has something to break.
    for (x, y) in [(f1, f3), (f2, f3)] {
        action::apply_action(&mut world, x, Action::ProposeTreaty { to: y, treaty: Treaty::Ceasefire }).unwrap();
        action::apply_action(&mut world, y, Action::AcceptTreaty { from: x, treaty: Treaty::Ceasefire }).unwrap();
    }
    assert!(!world.diplomacy.is_at_war(f2, f3), "sanity: f2 and f3 now hold a Ceasefire, not War");

    action::apply_action(&mut world, f1, Action::DeclareWar { to: f3 }).unwrap();

    assert!(world.diplomacy.is_at_war(f1, f3), "f1 declared war on f3 directly");
    assert!(
        world.diplomacy.is_at_war(f2, f3),
        "f2, allied with f1 purely through the scenario's declared bloc, must be dragged into f1's war \
         with f3 exactly like an in-play alliance would - even though f2 and f3 were at Ceasefire"
    );
    assert!(
        world
            .diplomacy
            .log
            .iter()
            .any(|e| matches!(e, Event::AllianceDragIn { faction, into_war_with } if *faction == f2 && *into_war_with == f3)),
        "expected an Event::AllianceDragIn naming f2's forced entry into the war with f3"
    );
}

/// External code review fix (Stage 6A, P2 #1): an empty `regions` or
/// `factions` array makes every validation loop below a no-op, so the file
/// used to "pass" `validate()` and only blow up later - a `--bench` run on
/// such a scenario panics inside `Observation::encode()` for `FactionId(0)`,
/// which doesn't exist. Both collections must be rejected, as two separate
/// cases so neither one masks the other.
#[test]
fn empty_scenario_is_rejected() {
    let empty_regions = {
        let start = MINI_VALID_SCENARIO.find(r#""regions": ["#).expect("regions array present");
        let region_array_start = start + r#""regions": ["#.len();
        let region_array_end = MINI_VALID_SCENARIO[region_array_start..].find("],\n  \"sea_zones\"").expect("end of regions array") + region_array_start;
        format!("{}{}{}", &MINI_VALID_SCENARIO[..region_array_start], "", &MINI_VALID_SCENARIO[region_array_end..])
    };
    match scenario::load_str(&empty_regions) {
        Err(scenario::ScenarioError::Empty { what }) => assert_eq!(what, "regions"),
        other => panic!("expected a distinct Empty(\"regions\") error, got {other:?}"),
    }

    let empty_factions = {
        let start = MINI_VALID_SCENARIO.find(r#""factions": ["#).expect("factions array present");
        let faction_array_start = start + r#""factions": ["#.len();
        // The factions array's own closing `]` sits on its own line ("  ],"),
        // immediately before the `"diplomacy"` field - find that line and
        // step past its 2-space indent to land on the `]` itself.
        let marker_pos = MINI_VALID_SCENARIO[faction_array_start..]
            .find("  ],\n  \"diplomacy\"")
            .expect("end of factions array")
            + faction_array_start;
        let faction_array_end = marker_pos + 2;
        format!("{}{}{}", &MINI_VALID_SCENARIO[..faction_array_start], "", &MINI_VALID_SCENARIO[faction_array_end..])
    };
    match scenario::load_str(&empty_factions) {
        Err(scenario::ScenarioError::Empty { what }) => assert_eq!(what, "factions"),
        other => panic!("expected a distinct Empty(\"factions\") error, got {other:?}"),
    }
}

/// External code review fix (Stage 6A, P2 #2): a `strait` link that omits
/// `strait_zone` silently becomes immune to blockade (`logistics`/
/// `military` only throttle a link when `strait_zone.is_some()`), with no
/// error and no visible symptom until someone notices a strait behaves like
/// a land route.
#[test]
fn strait_without_zone_is_rejected() {
    // `a`'s link to `b` becomes a `strait` but keeps no `strait_zone` -
    // exactly the silent-mechanics-change case from the review.
    let broken = MINI_VALID_SCENARIO.replacen(
        r#""links": [ { "to": "b", "kind": "rail" } ] },
    { "id": "b""#,
        r#""links": [ { "to": "b", "kind": "strait" } ] },
    { "id": "b""#,
        1,
    );
    match scenario::load_str(&broken) {
        Err(scenario::ScenarioError::InvalidStraitZone { from, to, .. }) => {
            assert_eq!((from.as_str(), to.as_str()), ("a", "b"));
        }
        other => panic!("expected a distinct InvalidStraitZone error, got {other:?}"),
    }
}

/// External code review fix (Stage 6A, P2 #2): a rail/road/tunnel link that
/// names a `strait_zone` silently becomes subject to blockade throttling it
/// was never meant to have - the exact opposite mistake from
/// `strait_without_zone_is_rejected`, and the one the Kanmon tunnel (a land
/// route deliberately immune to blockade) exists to guard against.
#[test]
fn land_link_with_zone_is_rejected() {
    // Give the scenario a real sea zone to reference, so the failure is
    // unambiguously "rail link must not name a strait_zone" and not
    // "unknown sea zone id".
    let with_zone = MINI_VALID_SCENARIO.replacen(
        r#""sea_zones": [],"#,
        r#""sea_zones": [ { "id": "z", "name": "Z", "coast": ["a", "b"], "adjacent": [] } ],"#,
        1,
    );
    let broken = with_zone.replacen(
        r#""links": [ { "to": "b", "kind": "rail" } ] },
    { "id": "b""#,
        r#""links": [ { "to": "b", "kind": "rail", "strait_zone": "z" } ] },
    { "id": "b""#,
        1,
    );
    match scenario::load_str(&broken) {
        Err(scenario::ScenarioError::InvalidStraitZone { from, to, .. }) => {
            assert_eq!((from.as_str(), to.as_str()), ("a", "b"));
        }
        other => panic!("expected a distinct InvalidStraitZone error, got {other:?}"),
    }
}

/// External code review fix (Stage 6A, P2 #3): `Value::as_f32` used to check
/// finiteness on the `f64` *before* narrowing to `f32`, so a finite-but-huge
/// value like `1e100` silently became `f32::INFINITY` and passed. Covers
/// both a scalar field (`population`) and a per-good `capacity` field, since
/// `parse_capacity` calls `as_f32` independently of `require_f32`.
#[test]
fn out_of_range_number_is_rejected() {
    let huge_population = MINI_VALID_SCENARIO.replacen(r#""population": 10.0,"#, r#""population": 1e100,"#, 1);
    match scenario::load_str(&huge_population) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("population"), "expected the error to name `population`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error, got {other:?}"),
    }

    let huge_capacity = MINI_VALID_SCENARIO.replacen(r#""food":1.0,"#, r#""food":1e100,"#, 1);
    match scenario::load_str(&huge_capacity) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("capacity"), "expected the error to name `capacity`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error, got {other:?}"),
    }
}

/// Guards `scenario::REGION_COUNT`/`SEA_ZONE_COUNT`/`FACTION_COUNT` (kept as
/// compile-time constants purely so `observation::ENCODING_LEN` can be one)
/// against ever silently drifting from what `scenarios/mvp.json` actually
/// contains.
#[test]
fn default_scenario_dimensions_match_embedded_json() {
    let world = scenario::build_world();
    assert_eq!(world.regions.len(), scenario::REGION_COUNT);
    assert_eq!(world.sea_zones.len(), scenario::SEA_ZONE_COUNT);
    assert_eq!(world.factions.len(), scenario::FACTION_COUNT);
    assert_eq!(world.transport_nodes.len(), scenario::TRANSPORT_NODE_COUNT);
    assert_eq!(world.transport_lines.len(), scenario::TRANSPORT_LINE_COUNT);
}

// ---------------------------------------------------------------------------
// Stage 7A (docs/phase7-spec.md "Stage 7A — 観る", "地域の座標"): the required
// per-region `position` field `apps/game` uses to place regions on the map.
// External code review fix B4: this field used to be optional, with
// `apps/game::layout` computing a deterministic graph-derived fallback
// layout for any region a scenario left unplaced - the project owner
// rejected that fallback (docs/conventions.md §3), so `position` is now
// required for every region and a scenario missing one is rejected at load
// time (`malformed_scenario_position_is_rejected` below covers the
// malformed case; the "missing entirely" case is exercised by every other
// test in this module, since `MINI_VALID_SCENARIO` itself now names one for
// every region and would fail to load otherwise).
// ---------------------------------------------------------------------------

/// External code review fix B4: a region naming no `position` field at all
/// must be rejected exactly like any other missing required field
/// (`require_object_field`'s ordinary behaviour), not defaulted to `None` -
/// the fallback layout that behaviour used to feed is gone
/// (`apps/game::layout::region_positions` no longer computes one).
#[test]
fn missing_scenario_position_is_rejected() {
    let no_position = MINI_VALID_SCENARIO.replacen(
        r#""infrastructure": 0.5, "port": 0.0, "position": [0.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" } ] },"#,
        r#""infrastructure": 0.5, "port": 0.0,
      "links": [ { "to": "b", "kind": "rail" } ] },"#,
        1,
    );
    match scenario::load_str(&no_position) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("position"), "expected the error to name `position`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a missing position, got {other:?}"),
    }
}

/// A `position` field that *is* present but malformed - the wrong number of
/// elements, or a non-numeric element - is a hard `ScenarioError::Schema`,
/// never a silent `None` (this module's own "壊れたデータで暗黙に既定値へ落
/// ちないこと" discipline, applied to Stage 7A's own new field exactly like
/// every other field `parse_region` reads).
#[test]
fn malformed_scenario_position_is_rejected() {
    let one_element = MINI_VALID_SCENARIO.replacen(
        r#""links": [ { "to": "b", "kind": "rail" } ] },"#,
        r#""links": [ { "to": "b", "kind": "rail" } ], "position": [1.0] },"#,
        1,
    );
    match scenario::load_str(&one_element) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("position"), "expected the error to name `position`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a 1-element position, got {other:?}"),
    }

    let non_numeric = MINI_VALID_SCENARIO.replacen(
        r#""links": [ { "to": "b", "kind": "rail" } ] },"#,
        r#""links": [ { "to": "b", "kind": "rail" } ], "position": ["x", 1.0] },"#,
        1,
    );
    match scenario::load_str(&non_numeric) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("position"), "expected the error to name `position`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a non-numeric position, got {other:?}"),
    }
}

/// Both shipped scenarios were given real coordinates by Stage 7A
/// (docs/phase7-spec.md "`scenarios/mvp.json` と `scenarios/japan47.json` の
/// 両方に座標を入れる") - every region in each file must actually carry a
/// finite `position` (`Region::position` is required since External code
/// review fix B4, so both files loading at all already proves every region
/// names one; this additionally guards against a placeholder like `NaN`
/// slipping through, which `parse_position`'s own `as_f32` already rejects
/// at load time but is still worth pinning here against a future change to
/// either file).
#[test]
fn shipped_scenarios_have_positions_everywhere() {
    let mvp = scenario::build_world();
    for region in &mvp.regions {
        let [x, y] = region.position;
        assert!(x.is_finite() && y.is_finite(), "scenarios/mvp.json region `{}` has a non-finite `position`", region.name);
    }

    let japan47 = scenario::load_str(&load_japan47_str()).expect("scenarios/japan47.json must load");
    for region in &japan47.regions {
        let [x, y] = region.position;
        assert!(x.is_finite() && y.is_finite(), "scenarios/japan47.json region `{}` has a non-finite `position`", region.name);
    }
}

// ---------------------------------------------------------------------------
// Stage 6B (docs/phase6-spec.md "Stage 6B — 47 都道府県マップ"):
// `scenarios/japan47.json`, the 47-prefecture scale-up of the 10-region MVP
// map. `cargo test` runs with this crate's own directory as the working
// directory, so the path below (not `scenarios/mvp.json`'s `include_str!`)
// is relative to `crates/sim/`, exactly like `apps/headless/tests/
// scenario_integration.rs`'s own `--scenario` argument.
// ---------------------------------------------------------------------------

const JAPAN47_PATH: &str = "../../scenarios/japan47.json";

fn load_japan47_str() -> String {
    std::fs::read_to_string(JAPAN47_PATH).expect("scenarios/japan47.json must exist and be readable")
}

/// Stage 6B acceptance test: `scenarios/japan47.json` passes every rule
/// `Scenario::validate` enforces (non-empty, ids resolve, links are
/// bidirectional and internally consistent, every faction owns territory,
/// the region graph is connected, sea zone coasts name real regions) - the
/// same "検証を通る" bullet Stage 6A's own `invalid_scenario_is_rejected`
/// guards the *rules* for; this test guards that the authored *data*
/// actually satisfies them at 47-region scale.
#[test]
fn japan47_is_valid() {
    let scenario = scenario::Scenario::parse(&load_japan47_str()).expect("scenarios/japan47.json must be valid JSON matching the scenario schema");
    scenario.validate().expect("scenarios/japan47.json must pass every Scenario::validate rule");
    assert_eq!(scenario.regions.len(), 47, "expected exactly 47 prefectures");
    assert!(
        scenario.sea_zones.len() >= 6 && scenario.sea_zones.len() <= 8,
        "docs/phase6-spec.md Stage 6B: 海域は6〜8程度, got {}",
        scenario.sea_zones.len()
    );
    assert!(
        scenario.factions.len() >= 6 && scenario.factions.len() <= 8,
        "docs/phase6-spec.md Stage 6B: 勢力数を増やして6〜8勢力とする, got {}",
        scenario.factions.len()
    );
}

/// docs/future-work.md "japan47 が 720 日で決着しない": japan47's own fix -
/// two regional blocs (東日本/西日本) rather than a six-way free-for-all.
/// This is the acceptance test for what actually *loads*: every faction
/// inside a bloc must start allied with its bloc-mates, and every faction
/// must still start at war with every faction outside its own bloc - the
/// exact shape `scenarios/japan47.json`'s `diplomacy.blocs` declares.
///
/// Confirmed this can fail: temporarily edited a local copy of
/// `scenarios/japan47.json` to declare `"blocs": []` (mvp's all-war
/// baseline) instead of the two real blocs and pointed this test at it -
/// every cross-bloc `is_at_war` assertion below still passed (everyone is
/// at war with everyone under an empty declaration too), but every
/// within-bloc `Stance::Alliance` assertion failed. Reverted before
/// committing.
#[test]
fn japan47_declares_two_blocs() {
    let world = scenario::load_str(&load_japan47_str()).expect("scenarios/japan47.json must build a valid World");
    let scenario = scenario::Scenario::parse(&load_japan47_str()).unwrap();
    let faction_ids: Vec<String> = scenario.factions.iter().map(|f| f.id.clone()).collect();
    let index_of = |id: &str| {
        FactionId(faction_ids.iter().position(|x| x == id).expect("faction id must exist in scenarios/japan47.json") as u32)
    };

    let east = ["hokuto_rengou", "kanto_fu", "chubu_domei"].map(index_of);
    let west = ["kinki_fu", "seinihon_domei", "shikoku_rengou"].map(index_of);

    for &a in &east {
        for &b in &east {
            if a != b {
                assert_eq!(
                    world.diplomacy.stance(a, b),
                    diplomacy::Stance::Alliance,
                    "every pair inside the declared 東日本 bloc must start allied"
                );
            }
        }
    }
    for &a in &west {
        for &b in &west {
            if a != b {
                assert_eq!(
                    world.diplomacy.stance(a, b),
                    diplomacy::Stance::Alliance,
                    "every pair inside the declared 西日本 bloc must start allied"
                );
            }
        }
    }
    for &a in &east {
        for &b in &west {
            assert!(world.diplomacy.is_at_war(a, b), "factions in different blocs must still start at war");
        }
    }
}

/// Stage 6B acceptance test: 720 simulated days must run to completion on
/// the 47-region map without panicking - the same bare `Simulation::step`
/// loop `determinism` above already exercises on the embedded 10-region
/// scenario, just for the full MVP run length instead of 200 days. No
/// agent actions are applied (`crates/sim` cannot depend on
/// `archipelago-agents`) - `apps/headless`'s own `--scenario
/// scenarios/japan47.json --days 720` run (with real `HeuristicAgent`
/// decisions) is this test's integration-level companion.
#[test]
fn japan47_completes_720_days() {
    let world = scenario::load_str(&load_japan47_str()).expect("scenarios/japan47.json must build a valid World");
    let mut sim = Simulation::with_world(world, 1);
    for _ in 0..720 {
        sim.step();
    }
    assert_eq!(sim.world.day, 720);
}

/// Stage 6B's key regression guard (docs/phase6-spec.md "japan47_chokepoints_
/// still_bind"): Phase 2's chokepoint design ("回廊を断つと奥が枯れる") must
/// still hold at 47-region scale for all three deliberately-placed
/// chokepoints - Kanmon (山口—福岡), Seikan (青森—北海道), and the central
/// highlands (長野・岐阜).
///
/// External code review fix (P2): the original version of this test flipped
/// the target region's *owner* to "cut" every corridor - that always zeroed
/// supply regardless of the mechanism under test, so the test could not fail
/// even if the chokepoint mechanic were completely broken. This version
/// keeps ownership unchanged throughout and drives the *actual* mechanism:
///   - Seikan (a `TransportLineKind::Sea` line): enemy sea control over
///     北方海域, via `naval::sea_line_factor` - exactly how a real blockade
///     throttles a sea crossing (docs/phase2-spec.md "1. 海峡リンクの遮断"'s
///     transport-network counterpart).
///   - Kanmon (a low-capacity `Rail` line, deliberately blockade-immune):
///     its own low capacity as the binding cap, an enemy landing force
///     holding 福岡 (contested, not captured) as the sever, and an explicit
///     check that sea control leaves it untouched.
///   - Central highlands (`Road` lines, no sea zone involved at all): an
///     enemy force holding both 長野 and 岐阜 (again contested, not
///     captured), which blocks them from relaying onward exactly like a
///     real siege would, with no ownership change anywhere.
///
/// Stage 9B rewrite: supply now flows over the transport network
/// (`world.transport_nodes`/`transport_lines`), not `Region::links`, so
/// every corridor above is now driven through the corresponding
/// `TransportLine` rather than a region `Link`'s `LinkKind`/`strait_zone` -
/// `transport_line_capacity` below looks up a line's own
/// `effective_capacity` directly instead of reading a `LinkKind` constant.
/// `world.supply[region]` is also now demand-bounded (this module's own
/// doc): mvp/japan47's default unit placement doesn't necessarily post a
/// unit at every target region measured here, so `station_garrison` adds
/// one explicitly at each sink under test - without it, a target region
/// with no units would trivially read `0.0` regardless of whether the
/// corridor mechanism works at all (`supply_corridor_cut`'s own doc found
/// the same gap for mvp). `isolate_single_source` (below) still boosts one
/// region into a saturated source and zeroes every other same-faction
/// region's own base, so whatever the target region receives remains
/// provably attributable to relay across the corridor under test, not some
/// other region's own idle production.
#[test]
fn japan47_chokepoints_still_bind() {
    let base_world = || scenario::load_str(&load_japan47_str()).expect("scenarios/japan47.json must build a valid World");

    fn id_of(ids: &[String], target: &str) -> RegionId {
        let i = ids.iter().position(|id| id == target).expect("region id must exist in scenarios/japan47.json");
        RegionId(i as u32)
    }

    fn zone_id_of(ids: &[String], target: &str) -> SeaZoneId {
        let i = ids.iter().position(|id| id == target).expect("sea zone id must exist in scenarios/japan47.json");
        SeaZoneId(i as u32)
    }

    // Zeroes every region owned by `source`'s faction *except* `source`
    // itself, then boosts `source` into a saturated supply base. Every
    // faction here spans several prefectures - unlike mvp's 10-region map,
    // where a chokepoint's two sides were each a single region - so without
    // this, a downstream region's own (unboosted, unzeroed) local
    // `supply_source` would keep contributing something after the corridor
    // is cut, masking whether the cut actually mattered. With it, `source`
    // is provably the *only* thing feeding the rest of the faction, so
    // whatever the target region receives is entirely attributable to
    // relay across the corridor being tested.
    fn isolate_single_source(world: &mut World, source: RegionId) {
        let faction = world.region(source).owner;
        for i in 0..world.regions.len() {
            let r = RegionId(i as u32);
            if r != source && world.region(r).owner == faction {
                world.region_mut(r).capacity = [0.0; GOOD_COUNT];
                world.region_mut(r).port = 0.0;
            }
        }
        for good in crate::good::ALL_GOODS {
            world.region_mut(source).capacity[good.index()] = 1000.0;
        }
        world.region_mut(source).infrastructure = 1.0;
    }

    // Posts a garrison of `count` units of `region`'s own owner in `region`
    // - real, non-trivial Munitions demand for the Stage 9B demand-bounded
    // flow model to actually deliver against (see this test's own doc).
    fn station_garrison(world: &mut World, region: RegionId, count: usize) {
        let owner = world.region(region).owner;
        for i in 0..count {
            let id = crate::ids::UnitId(world.units.len() as u32);
            world.units.push(military::Unit {
                id,
                owner,
                name: format!("Garrison {i}"),
                station: Station::Region(region),
                movement: None,
                manpower: 1.0,
                equipment: UNIT_EQUIPMENT,
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
    }

    // Stations a single foreign, at-war unit in `region` - enough to make
    // `World::has_enemy_units`/`recompute_supply`'s `contested[i]` true -
    // without touching `Region::owner`. This is what actually stops a
    // region from relaying supply onward; it does not stop the region from
    // itself still receiving whatever a non-contested neighbor relays into
    // it, which is exactly the "corridor besieged, not captured" case this
    // test wants for Kanmon/central-highlands (as opposed to Seikan, where
    // the real mechanism under test is sea control, not land contest).
    //
    // `enemy` must genuinely be at war with `owner` - `has_enemy_units` (and
    // therefore `contested[i]`) is gated on `Diplomacy::is_at_war`, not mere
    // faction identity, so search for a faction actually at war instead of
    // assuming one.
    fn contest_with_enemy(world: &mut World, region: RegionId) {
        let owner = world.region(region).owner;
        let enemy = (0..world.factions.len())
            .map(|i| FactionId(i as u32))
            .find(|&candidate| world.diplomacy.is_at_war(owner, candidate))
            .expect("test setup requires at least one faction actually at war with the region's owner");
        let id = crate::ids::UnitId(world.units.len() as u32);
        world.units.push(military::Unit {
            id,
            owner: enemy,
            name: "Enemy Raiding Force".to_string(),
            station: Station::Region(region),
            movement: None,
            manpower: 1.0,
            equipment: 1.0,
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

    // Gives `enemy` total (1.0) control of `zone`, every other faction 0.0 -
    // `naval::sea_line_factor`/`SeaZone::enemy_control_max` then read this
    // directly, the same shape `tick_sea_control` itself would produce if
    // `enemy`'s fleet were the only power present.
    fn dominate_zone(world: &mut World, zone: SeaZoneId, enemy: FactionId) {
        let mut control = vec![0.0f32; world.factions.len()];
        control[enemy.index()] = 1.0;
        world.sea_zone_mut(zone).control = control;
    }

    // Stage 9B: the actual per-tick ceiling of the (unique) `TransportLine`
    // directly connecting `a` and `b`'s regions - the transport-network
    // replacement for reading a `LinkKind` constant off a region `Link`.
    fn transport_line_capacity(world: &World, a: RegionId, b: RegionId) -> f32 {
        world
            .transport_lines
            .iter()
            .find(|l| {
                let ra = world.transport_node(l.from).region;
                let rb = world.transport_node(l.to).region;
                (ra == a && rb == b) || (ra == b && rb == a)
            })
            .expect("test setup requires a direct TransportLine between these two regions")
            .effective_capacity(world)
    }

    // Region/sea-zone ids in file order = RegionId/SeaZoneId assignment
    // order (`Scenario::build_world`'s doc), so recover both id->index
    // tables once from the raw scenario, the same way `Scenario::
    // build_world` itself does.
    let scenario = scenario::Scenario::parse(&load_japan47_str()).unwrap();
    let ids: Vec<String> = scenario.regions.iter().map(|r| r.id.clone()).collect();
    let zone_ids: Vec<String> = scenario.sea_zones.iter().map(|z| z.id.clone()).collect();

    // 1. Seikan (青森—北海道, a `Sea` TransportLine, zone 北方海域/`hoppou`):
    //    青森 is the sole source for all of 北方連合. 北海道's only route is
    //    this sea crossing, so enemy sea control there (`naval::
    //    sea_line_factor` -> 0) must starve it - ownership of 北海道 never
    //    changes.
    {
        let aomori = id_of(&ids, "aomori");
        let hokkaido = id_of(&ids, "hokkaido");
        let hoppou = zone_id_of(&zone_ids, "hoppou");

        let mut world = base_world();
        isolate_single_source(&mut world, aomori);
        station_garrison(&mut world, hokkaido, 1);
        logistics::recompute_supply(&mut world);
        let open = world.supply[hokkaido.index()];

        let mut cut = base_world();
        isolate_single_source(&mut cut, aomori);
        station_garrison(&mut cut, hokkaido, 1);
        let owner = cut.region(aomori).owner;
        let enemy = FactionId((owner.0 + 1) % cut.factions.len() as u32);
        dominate_zone(&mut cut, hoppou, enemy);
        logistics::recompute_supply(&mut cut);
        let severed = cut.supply[hokkaido.index()];

        assert_eq!(cut.region(hokkaido).owner, owner, "test setup requires ownership to stay unchanged");
        assert!(open > 0.0, "sanity: Seikan should relay something when intact: {open}");
        assert!(
            severed < open * 0.1,
            "enemy sea control over 北方海域 must starve 北海道's relayed supply without touching ownership: open={open}, severed={severed}"
        );
    }

    // 2. Kanmon (山口—福岡, a low-capacity `Rail` TransportLine, deliberately
    //    blockade-immune): 山口 is the sole source for all of 西日本同盟
    //    (spanning both 中国 and 九州).
    {
        let yamaguchi = id_of(&ids, "yamaguchi");
        let fukuoka = id_of(&ids, "fukuoka");
        let kagoshima = id_of(&ids, "kagoshima");
        // Sea zones actually touching the corridor's two ends, used only for
        // the immunity check below - `setouchi` faces 山口, `toshina` faces
        // 福岡/鹿児島.
        let setouchi = zone_id_of(&zone_ids, "setouchi");
        let toshina = zone_id_of(&zone_ids, "toshina");

        // 福岡 and 鹿児島 both draw through the *same* single 8.0-capacity
        // corridor (山口->福岡->熊本->鹿児島) - loading heavy demand at both
        // at once would make them contend with *each other* for that shared
        // corridor (correctly, per Stage 9B's whole reason to exist - see
        // `supply_behind_shared_line_is_demand_proportional` below, which
        // pins exactly that), which would confound 2a's "does the corridor's
        // own ceiling bind" question with "how is it split between two
        // sinks". So 2a's heavy demand goes at 福岡 alone, and 2b/2c's own
        // baseline/comparisons use a separate board with heavy demand at
        // 鹿児島 alone instead.
        let mut world = base_world();
        isolate_single_source(&mut world, yamaguchi);
        // A large garrison at 福岡 so demand there sits well above the
        // tunnel's own low capacity - otherwise a small demand is trivially
        // satisfiable even at a heavily-throttled corridor, and 2a below
        // would never actually observe the tunnel's ceiling binding
        // (`node_throughput_limits_supply`'s own doc makes the same point).
        station_garrison(&mut world, fukuoka, 20);
        logistics::recompute_supply(&mut world);
        let open_fukuoka = world.supply[fukuoka.index()];

        let mut world_kagoshima = base_world();
        isolate_single_source(&mut world_kagoshima, yamaguchi);
        station_garrison(&mut world_kagoshima, kagoshima, 20);
        logistics::recompute_supply(&mut world_kagoshima);
        let open_kagoshima = world_kagoshima.supply[kagoshima.index()];

        assert!(open_kagoshima > 0.0, "sanity: Kanmon should relay something into Kyushu when intact: {open_kagoshima}");

        // 2a. Its own low capacity (8.0, versus a Rail line's ordinary
        //     25.0) is what actually binds - even with 山口 saturated and
        //     福岡's own demand pushed well past it, 福岡's inbound supply
        //     cannot exceed the tunnel line's own ceiling.
        let tunnel_cap = transport_line_capacity(&world, yamaguchi, fukuoka);
        assert!(
            open_fukuoka <= tunnel_cap + 0.05,
            "Kanmon's own capacity ({tunnel_cap}) must cap what reaches 福岡 even from a saturated source and heavy \
             local demand: open_fukuoka={open_fukuoka}"
        );
        assert!(
            (open_fukuoka - tunnel_cap).abs() < 0.5,
            "with 山口 saturated and demand pushed past it, 福岡's inbound supply should sit at (not far below) \
             Kanmon's own capacity ceiling of {tunnel_cap}, proving the tunnel - not downstream Kyushu rail (25.0) - \
             is what binds: open_fukuoka={open_fukuoka}"
        );

        // 2b. An enemy force holding 福岡 (contested, not captured) severs
        //     the rest of Kyushu from 山口 - ownership never changes.
        let mut cut = base_world();
        isolate_single_source(&mut cut, yamaguchi);
        station_garrison(&mut cut, kagoshima, 20);
        let owner = cut.region(yamaguchi).owner;
        contest_with_enemy(&mut cut, fukuoka);
        logistics::recompute_supply(&mut cut);
        let severed = cut.supply[kagoshima.index()];

        assert_eq!(cut.region(fukuoka).owner, owner, "test setup requires ownership to stay unchanged");
        assert!(
            severed < open_kagoshima * 0.1,
            "an enemy force holding 福岡 must starve the rest of Kyushu without capturing it: \
             open={open_kagoshima}, severed={severed}"
        );

        // 2c. Immunity, the deliberate flip side of 2b (docs/phase2-spec.md
        //     "関門トンネルが封鎖の影響を受けないのは意図的である"): total
        //     enemy sea control over *both* zones the corridor touches must
        //     leave Kyushu's supply untouched, because the tunnel is
        //     modeled as a `Rail` line, never a `Sea` one
        //     (`naval::sea_line_factor` is only ever applied to `Sea` lines
        //     - `logistics::compute_transport_flow`'s own doc).
        let mut blockaded = base_world();
        isolate_single_source(&mut blockaded, yamaguchi);
        station_garrison(&mut blockaded, kagoshima, 20);
        let enemy = FactionId((owner.0 + 1) % blockaded.factions.len() as u32);
        dominate_zone(&mut blockaded, setouchi, enemy);
        dominate_zone(&mut blockaded, toshina, enemy);
        logistics::recompute_supply(&mut blockaded);
        let under_blockade = blockaded.supply[kagoshima.index()];

        assert!(
            under_blockade > open_kagoshima * 0.9,
            "Kanmon must stay immune to sea control by design - total enemy control of the zones either end \
             faces must not throttle it: open={open_kagoshima}, under_blockade={under_blockade}"
        );
    }

    // 3. Central highlands (長野・岐阜, `Road` TransportLines - no sea zone
    //    involved): 愛知 is the sole source for all of 中部同盟. 愛知 has no
    //    direct link to 石川's Hokuriku cluster (新潟/富山/石川/福井) at all -
    //    every route runs through 岐阜 directly, or through 静岡->長野. An
    //    enemy force holding both 長野 and 岐阜 (contested, not captured)
    //    blocks both from relaying onward, leaving Hokuriku with no
    //    alternate route and no ownership change anywhere.
    {
        let aichi = id_of(&ids, "aichi");
        let nagano = id_of(&ids, "nagano");
        let gifu = id_of(&ids, "gifu");
        let ishikawa = id_of(&ids, "ishikawa");

        let mut world = base_world();
        isolate_single_source(&mut world, aichi);
        station_garrison(&mut world, ishikawa, 1);
        logistics::recompute_supply(&mut world);
        let open = world.supply[ishikawa.index()];

        let mut cut = base_world();
        isolate_single_source(&mut cut, aichi);
        station_garrison(&mut cut, ishikawa, 1);
        let owner = cut.region(aichi).owner;
        contest_with_enemy(&mut cut, nagano);
        contest_with_enemy(&mut cut, gifu);
        logistics::recompute_supply(&mut cut);
        let severed = cut.supply[ishikawa.index()];

        assert_eq!(cut.region(nagano).owner, owner, "test setup requires ownership to stay unchanged");
        assert_eq!(cut.region(gifu).owner, owner, "test setup requires ownership to stay unchanged");
        assert!(open > 0.0, "sanity: 石川 should receive relayed supply via 長野/岐阜 when intact: {open}");
        assert!(
            severed < open * 0.1,
            "an enemy force holding both 長野 and 岐阜 must starve Hokuriku's relayed supply, \
             with no alternate route quietly carrying it: open={open}, severed={severed}"
        );
    }
}

// ---------------------------------------------------------------------------
// Stage 9B (docs/phase9-spec.md "2. 補給を有限流量にする", §7 "Stage 9B"):
// the flow-model acceptance tests proper. `BRANCH_SCENARIO` is a hand-built,
// single-faction map shaped exactly like the property each test needs: one
// source region, a hub, and two sibling sinks (`left`/`right`) that only
// ever reach the source through the hub - the hub's own inbound line
// (`hub_depot`'s `source_depot -> hub_depot` `TransportLine`, capacity
// `BRANCH_BOTTLENECK_CAPACITY`) is the single shared bottleneck every test
// below drives. `left`/`right`'s own `hub_depot -> *_depot` lines are given
// generous capacity so they never bind - only the shared corridor should.
// ---------------------------------------------------------------------------

const BRANCH_BOTTLENECK_CAPACITY: f32 = 10.0;

const BRANCH_SCENARIO: &str = r#"
{
  "regions": [
    { "id": "source", "name": "Source", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1000.0,"machinery":1000.0,"munitions":1000.0,"arms":1000.0},
      "infrastructure": 1.0, "port": 0.0, "position": [0.0, 0.0],
      "links": [ { "to": "hub", "kind": "rail" } ] },
    { "id": "hub", "name": "Hub", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 1.0, "port": 0.0, "position": [1.0, 0.0],
      "links": [ { "to": "source", "kind": "rail" }, { "to": "left", "kind": "rail" }, { "to": "right", "kind": "rail" }, { "to": "enemy_home", "kind": "rail" } ] },
    { "id": "left", "name": "Left", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 1.0, "port": 0.0, "position": [2.0, 1.0],
      "links": [ { "to": "hub", "kind": "rail" } ] },
    { "id": "right", "name": "Right", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 1.0, "port": 0.0, "position": [2.0, -1.0],
      "links": [ { "to": "hub", "kind": "rail" } ] },
    { "id": "enemy_home", "name": "Enemy Home", "terrain": "plain", "population": 1.0,
      "capacity": {"food":0.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 0.0, "port": 0.0, "position": [1.0, 2.0],
      "links": [ { "to": "hub", "kind": "rail" } ] }
  ],
  "sea_zones": [],
  "transport": {
    "nodes": [
      { "id": "source_depot", "name": "Source Depot", "kind": "depot", "region": "source" },
      { "id": "hub_depot", "name": "Hub Depot", "kind": "depot", "region": "hub" },
      { "id": "left_depot", "name": "Left Depot", "kind": "depot", "region": "left" },
      { "id": "right_depot", "name": "Right Depot", "kind": "depot", "region": "right" },
      { "id": "enemy_home_depot", "name": "Enemy Home Depot", "kind": "depot", "region": "enemy_home" }
    ],
    "lines": [
      { "from": "source_depot", "to": "hub_depot", "kind": "rail", "capacity": 10.0, "condition": 1.0 },
      { "from": "hub_depot", "to": "left_depot", "kind": "rail", "capacity": 100.0, "condition": 1.0 },
      { "from": "hub_depot", "to": "right_depot", "kind": "rail", "capacity": 100.0, "condition": 1.0 }
    ]
  },
  "factions": [
    { "id": "f1", "name": "F1", "capital": "source", "regions": ["source", "hub", "left", "right"] },
    { "id": "f2", "name": "F2", "capital": "enemy_home", "regions": ["enemy_home"] }
  ],
  "diplomacy": { "blocs": [] },
  "victory": [ { "type": "conquest" } ]
}
"#;

/// Stations `count` garrison units (each `SUPPLY_NEED_PER_MANPOWER`-worth of
/// Munitions demand) of `region`'s own owner in `region`, for the
/// `BRANCH_SCENARIO`-based tests below.
fn station_units(world: &mut World, region: RegionId, count: usize) {
    let owner = world.region(region).owner;
    for i in 0..count {
        let id = crate::ids::UnitId(world.units.len() as u32);
        world.units.push(military::Unit {
            id,
            owner,
            name: format!("Unit {i}"),
            station: Station::Region(region),
            movement: None,
            manpower: 1.0,
            equipment: UNIT_EQUIPMENT,
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
}

fn branch_region(ids: &[String], name: &str) -> RegionId {
    RegionId(ids.iter().position(|id| id == name).expect("region must exist in BRANCH_SCENARIO") as u32)
}

fn branch_ids() -> Vec<String> {
    scenario::Scenario::parse(BRANCH_SCENARIO).unwrap().regions.iter().map(|r| r.id.clone()).collect()
}

/// A fresh `BRANCH_SCENARIO` `World`, with `Scenario::build_world`'s
/// auto-placed starting units (three per faction, at capital + owned
/// neighbors - the same placement `scenario::build_world`'s own doc
/// describes for the embedded mvp scenario, applied to *any* loaded
/// scenario) removed - the tests below need to control every unit's
/// demand exactly, not have `source`/`hub`/`enemy_home` carry incidental
/// demand from that default placement.
fn branch_world() -> World {
    let mut world = scenario::load_str(BRANCH_SCENARIO).expect("BRANCH_SCENARIO must be valid");
    world.units.clear();
    world
}

/// **The central Stage 9B property** (docs/phase9-spec.md §7 Stage 9B,
/// criterion 1; CLAUDE.md: "Phase 9 の存在理由"): adding units *behind the
/// same line* - even in a sibling region whose own unit count never
/// changes - lowers that sibling's per-unit supply, because the two
/// regions are now genuinely contending for the same finite corridor
/// capacity rather than each independently drawing a best-path ceiling
/// that ignores the other entirely.
///
/// `left` keeps exactly 2 units throughout. With `right` empty, `left`'s
/// tiny demand (2.0) sits far under the shared corridor's 10.0 capacity, so
/// it is served in full. Adding a large garrison at `right` (sharing the
/// *same* `source_depot -> hub_depot` line, not `left`'s own downstream
/// line) pushes combined demand past the corridor's capacity, and `left`'s
/// per-unit supply drops sharply *without a single unit of its own ever
/// moving, arriving, or leaving* - proof this is real capacity contention
/// across regions, not merely each region's own demand capping its own
/// ceiling (which a region could exhibit entirely on its own, and would
/// prove nothing about the network).
///
/// Confirmed this can fail: temporarily hardcoded the round's per-resource
/// `scale` closures (`scale_vertex`/`scale_line`) in `compute_transport_flow`
/// to always return `1.0` (i.e. every desired amount granted in full,
/// exactly the pre-Stage-9B "capacity never actually consumed" defect this
/// stage exists to fix) and re-ran - `left`'s per-unit supply came back
/// identical (`1.0`) in both the empty-`right` and loaded-`right` cases, and
/// the assertion below failed immediately. Reverted before committing.
#[test]
fn supply_behind_shared_line_is_demand_proportional() {
    let ids = branch_ids();
    let left = branch_region(&ids, "left");
    let right = branch_region(&ids, "right");

    let mut idle_right = branch_world();
    station_units(&mut idle_right, left, 2);
    logistics::recompute_supply(&mut idle_right);
    let per_unit_idle = idle_right.supply[left.index()] / 2.0;

    let mut loaded_right = branch_world();
    station_units(&mut loaded_right, left, 2);
    station_units(&mut loaded_right, right, 40);
    logistics::recompute_supply(&mut loaded_right);
    let per_unit_loaded = loaded_right.supply[left.index()] / 2.0;

    assert!(
        per_unit_idle > 0.9,
        "sanity: with the corridor otherwise idle, left's tiny demand should be served in full: {per_unit_idle}"
    );
    assert!(
        per_unit_loaded < per_unit_idle * 0.7,
        "adding a large garrison behind the SAME corridor at a sibling region (right) must lower left's own \
         per-unit supply, even though left's own unit count never changed: idle={per_unit_idle}, loaded={per_unit_loaded}"
    );
}

/// docs/phase9-spec.md §7 Stage 9B, criterion 2: cutting one line drops
/// supply beyond it; with a detour present, it falls to the detour's own
/// capacity rather than to zero. Extends `BRANCH_SCENARIO`'s topology (via
/// direct `World` edits rather than a second scenario file) with a second,
/// lower-capacity `source_depot -> hub_depot` line standing in for a
/// detour - `World`'s `transport_lines: Vec<TransportLine>` has no
/// uniqueness constraint on endpoints, so two parallel lines between the
/// same two nodes is exactly "two routes between the same two places", the
/// simplest possible detour shape.
///
/// Confirmed this can fail: temporarily left the detour line's capacity
/// unclamped at the *primary* line's own 10.0 (instead of a distinctly
/// lower 3.0) - the "falls to the detour's capacity, not to the primary's"
/// half of the assertion below is what actually distinguishes this from a
/// test that would pass even if the detour were silently ignored;
/// re-running with the detour line simply deleted confirms the *other*
/// half - `severed` was `0.0` with no detour present, versus positive once
/// it exists - so both halves of "falls to the detour's capacity rather
/// than to zero" are independently exercised.
#[test]
fn cutting_a_line_falls_to_the_detour_capacity_not_zero() {
    const DETOUR_CAPACITY: f32 = 3.0;

    let ids = branch_ids();
    let source = branch_region(&ids, "source");
    let hub = branch_region(&ids, "hub");
    let left = branch_region(&ids, "left");

    fn primary_line_index(world: &World, source: RegionId, hub: RegionId) -> usize {
        world
            .transport_lines
            .iter()
            .position(|l| world.transport_node(l.from).region == source && world.transport_node(l.to).region == hub)
            .expect("BRANCH_SCENARIO must declare a source -> hub line")
    }

    // No detour at all: severing the one corridor must starve `left` to
    // exactly zero.
    let mut no_detour = branch_world();
    station_units(&mut no_detour, left, 2);
    let primary = primary_line_index(&no_detour, source, hub);
    no_detour.transport_lines[primary].condition = Condition::new(0.0).unwrap();
    logistics::recompute_supply(&mut no_detour);
    let severed_no_detour = no_detour.supply[left.index()];

    // A detour present: severing the *primary* line (condition -> 0) still
    // leaves the parallel, lower-capacity detour line intact.
    let mut with_detour = branch_world();
    station_units(&mut with_detour, left, 2);
    let hub_node = with_detour.transport_nodes.iter().find(|n| n.region == hub).unwrap().id;
    let source_node = with_detour.transport_nodes.iter().find(|n| n.region == source).unwrap().id;
    let new_line_id = crate::ids::TransportLineId(with_detour.transport_lines.len() as u32);
    with_detour.transport_lines.push(crate::transport::TransportLine {
        id: new_line_id,
        from: source_node,
        to: hub_node,
        kind: TransportLineKind::Rail,
        capacity: Capacity::new(DETOUR_CAPACITY).unwrap(),
        condition: Condition::new(1.0).unwrap(),
    });
    logistics::recompute_supply(&mut with_detour);
    let open_with_detour = with_detour.supply[left.index()];

    let primary = primary_line_index(&with_detour, source, hub);
    with_detour.transport_lines[primary].condition = Condition::new(0.0).unwrap();
    logistics::recompute_supply(&mut with_detour);
    let severed_with_detour = with_detour.supply[left.index()];

    assert_eq!(severed_no_detour, 0.0, "with no detour at all, cutting the sole corridor must starve left to exactly zero");
    assert!(open_with_detour > 1.8, "sanity: with both lines intact, left should draw near its full 2.0 demand: {open_with_detour}");
    assert!(
        severed_with_detour > 0.0,
        "with a detour present, cutting the primary line must not starve left to zero: {severed_with_detour}"
    );
    assert!(
        severed_with_detour <= DETOUR_CAPACITY + 0.05,
        "with a detour present, cutting the primary line must cap left's supply at the detour's own capacity \
         ({DETOUR_CAPACITY}), not leave it at the primary's higher ceiling: severed_with_detour={severed_with_detour}"
    );
}

/// docs/phase9-spec.md §7 Stage 9B, criterion 3: oversubscribed allocation
/// is proportional to demand and independent of iteration order - "which
/// front gets supply must never depend on iteration order or on an id"
/// (CLAUDE.md「繰り返し踏んだ欠陥」). `left`/`right` share the same 10.0
/// corridor with *different* demand (6 units vs. 2 units, a 3:1 ratio);
/// both boards below have the exact same topology and demand, differing
/// only in which region id (`RegionId(2)` vs `RegionId(3)`, the order
/// `BRANCH_SCENARIO`'s own region list assigns them) carries the *larger*
/// demand - `compute_transport_flow` iterates regions in ascending
/// `RegionId` order (`logistics.rs`'s own doc), so this directly probes
/// whether being visited first/last changes the *ratio* each side receives.
///
/// Confirmed this can fail: temporarily changed the commit loop in
/// `compute_transport_flow` to grant candidates their full `desired` amount
/// in ascending-`RegionId` order until the line's residual capacity ran out
/// (a sequential first-come-first-served allocator, the exact "fixed
/// priority decided by iteration order" shape CLAUDE.md lists first) and
/// re-ran - the low-id region always drained the corridor first regardless
/// of which side actually carried the larger demand, and the proportionality
/// assertion below failed. Reverted before committing.
#[test]
fn oversubscribed_allocation_is_proportional_and_order_independent() {
    fn measure(heavy_is_left: bool) -> (f32, f32) {
        let mut world = branch_world();
        let ids = branch_ids();
        let left = branch_region(&ids, "left");
        let right = branch_region(&ids, "right");
        let (heavy, light) = if heavy_is_left { (left, right) } else { (right, left) };
        // 30:10 (a 3:1 ratio, same as `6:2`) but large enough combined
        // (40.0) to genuinely oversubscribe the 10.0 corridor - `6:2`'s
        // combined demand (8.0) fit under the corridor with room to spare,
        // so both sides were served in full regardless of allocation order,
        // and this test could not actually have caught a broken allocator.
        station_units(&mut world, heavy, 30);
        station_units(&mut world, light, 10);
        logistics::recompute_supply(&mut world);
        (world.supply[heavy.index()], world.supply[light.index()])
    }

    let (heavy_left, light_left) = measure(true);
    let (heavy_right, light_right) = measure(false);

    assert!(heavy_left + light_left <= BRANCH_BOTTLENECK_CAPACITY + 0.05, "sanity: total delivered must not exceed the corridor's own capacity");
    assert!(
        heavy_left > light_left * 2.0,
        "the 30-unit side must receive noticeably more than the 10-unit side (roughly a 3:1 split of the shared \
         corridor): heavy={heavy_left}, light={light_left}"
    );
    assert!(
        (heavy_left - heavy_right).abs() < 0.05 && (light_left - light_right).abs() < 0.05,
        "swapping which region id (left, the lower id, vs right, the higher id) carries the heavier demand must \
         not change who gets how much - only demand should: (heavy_left={heavy_left}, light_left={light_left}) \
         vs (heavy_right={heavy_right}, light_right={light_right})"
    );
}

// ---------------------------------------------------------------------
// P1 fix (external code review, post-Stage-9B): each `TransportLine`'s
// residual budget was initialized as `[c, c]` - a full `capacity *
// condition` *per direction* - instead of one shared budget for the whole
// (undirected) physical route. Two genuinely independent streams routing
// in opposite directions over the same line in the same tick could
// therefore together carry up to `2 * capacity * condition`: the exact
// "capacity is never consumed" defect Stage 9B exists to fix, reintroduced
// in a smaller, two-way-traffic-shaped form. `CROSS_SCENARIO` below is
// built specifically to make both directions of one line route genuinely
// (not merely as a hypothetical): a `west`/`east` pair straddle the line
// under test, each fed primarily from its own adjacent producer
// (`west_producer`/`east_producer`) - but each producer's own adjacent
// relay line is shared: whatever that relay line doesn't grant its "own"
// side (because the *other* side's demand crossing the test line also
// contends for it, pooled the same way any two candidates sharing a
// resource are) crosses the test line into the opposite side. This yields
// two real, simultaneous cross-line flows, not a contrived one.
// ---------------------------------------------------------------------
const CROSS_SCENARIO: &str = r#"
{
  "regions": [
    { "id": "east", "name": "East", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 1.0, "port": 0.0, "position": [0.0, 0.0],
      "links": [ {"to":"west","kind":"rail"}, {"to":"filler","kind":"rail"} ] },
    { "id": "west", "name": "West", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 1.0, "port": 0.0, "position": [1.0, 0.0],
      "links": [ {"to":"east","kind":"rail"}, {"to":"west_producer","kind":"rail"} ] },
    { "id": "west_producer", "name": "WestProducer", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":500.0,"machinery":500.0,"munitions":500.0,"arms":500.0},
      "infrastructure": 1.0, "port": 0.0, "position": [2.0, 0.0],
      "links": [ {"to":"west","kind":"rail"}, {"to":"east_producer","kind":"rail"} ] },
    { "id": "east_producer", "name": "EastProducer", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":500.0,"machinery":500.0,"munitions":500.0,"arms":500.0},
      "infrastructure": 1.0, "port": 0.0, "position": [3.0, 0.0],
      "links": [ {"to":"west_producer","kind":"rail"}, {"to":"filler","kind":"rail"} ] },
    { "id": "filler", "name": "Filler", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":0.0,"steel":0.0,"machinery":0.0,"munitions":0.0,"arms":0.0},
      "infrastructure": 1.0, "port": 0.0, "position": [1.5, 1.0],
      "links": [ {"to":"east_producer","kind":"rail"}, {"to":"east","kind":"rail"} ] }
  ],
  "sea_zones": [],
  "transport": {
    "nodes": [
      { "id": "east_d", "name": "East Depot", "kind": "depot", "region": "east" },
      { "id": "west_d", "name": "West Depot", "kind": "depot", "region": "west" },
      { "id": "wp_d", "name": "WestProducer Depot", "kind": "depot", "region": "west_producer" },
      { "id": "ep_d", "name": "EastProducer Depot", "kind": "depot", "region": "east_producer" },
      { "id": "filler_d", "name": "Filler Depot", "kind": "depot", "region": "filler" }
    ],
    "lines": [
      { "from": "east_d", "to": "west_d", "kind": "rail", "capacity": 8.0, "condition": 1.0 },
      { "from": "west_d", "to": "wp_d", "kind": "rail", "capacity": 12.0, "condition": 1.0 },
      { "from": "wp_d", "to": "ep_d", "kind": "rail", "capacity": 12.0, "condition": 1.0 },
      { "from": "ep_d", "to": "filler_d", "kind": "rail", "capacity": 12.0, "condition": 1.0 },
      { "from": "filler_d", "to": "east_d", "kind": "rail", "capacity": 12.0, "condition": 1.0 }
    ]
  },
  "factions": [
    { "id": "f1", "name": "F1", "capital": "east", "regions": ["east","west","west_producer","east_producer","filler"] }
  ],
  "diplomacy": { "blocs": [] },
  "victory": [ { "type": "conquest" } ]
}
"#;

const CROSS_LINE_CAPACITY: f32 = 8.0;

/// **The central P1-fix invariant**: no `TransportLine`'s total flow in a
/// tick, summed over *both* directions, may exceed its own
/// `capacity * condition` - the property `[c, c]` violated. `east`/`west`
/// each draw mostly from their own adjacent producer, but every producer's
/// relay line is shared with the *other* side's crossing demand (see this
/// section's own doc above), so real traffic genuinely routes both ways
/// over the `east`-`west` line in the same tick - this is not a
/// hypothetical, order-dependent, or exhaustion-timing artifact; `east`
/// and `west` both carry heavy, identical demand from the start.
///
/// Confirmed this fails against the pre-fix code: with `residual_line`
/// initialized as `[c, c]` (a full 8.0 budget *per direction* instead of
/// one shared 8.0 for the line), this scenario produced `east->west =
/// 5.0` and `west->east = 6.0` - a combined 11.0 over a line whose own
/// `capacity * condition` is 8.0, i.e. the line carried 137% of what it
/// physically allows. Fixed (`residual_line: Vec<f32>`, one shared budget
/// per line, opposing directions pooled into the same `total_desired_line`
/// and arbitrated by the same proportional-scale mechanism any other
/// contended resource uses), this same scenario instead produces
/// `east->west = 2.0` and `west->east = 6.0` - a combined 8.0, exactly the
/// line's own `capacity * condition` and no more - and this assertion
/// holds.
#[test]
fn line_flow_never_doubles_under_two_way_traffic() {
    let ids = scenario::Scenario::parse(CROSS_SCENARIO).unwrap().regions.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    let idx = |name: &str| RegionId(ids.iter().position(|i| i == name).unwrap() as u32);
    let east = idx("east");
    let west = idx("west");

    let mut world = scenario::load_str(CROSS_SCENARIO).expect("CROSS_SCENARIO must be valid");
    world.units.clear();
    station_units(&mut world, east, 30);
    station_units(&mut world, west, 30);
    logistics::recompute_supply(&mut world);

    let flows = logistics::supply_link_flows(&world);
    let mut forward = 0.0f32;
    let mut backward = 0.0f32;
    for f in &flows {
        if f.from == east && f.to == west {
            forward = f.throughput.flow();
        }
        if f.from == west && f.to == east {
            backward = f.throughput.flow();
        }
    }

    assert!(forward > 0.5, "sanity: east must genuinely draw some of its supply across the line from west's producer: {forward}");
    assert!(backward > 0.5, "sanity: west must genuinely draw some of its supply across the line from east's producer: {backward}");
    assert!(
        forward + backward <= CROSS_LINE_CAPACITY + 0.05,
        "a single physical line's combined forward+backward flow this tick must never exceed its own \
         capacity * condition ({CROSS_LINE_CAPACITY}): forward={forward}, backward={backward}, \
         total={} - two-way traffic must not double the line's effective capacity",
        forward + backward
    );
}

/// docs/phase9-spec.md §7 Stage 9B, criterion 4: `condition` recovers.
/// CLAUDE.md's「繰り返し踏んだ欠陥」: "状態には必ず回復経路を持たせる" - a
/// line driven down by war damage must have a real path back up, not just a
/// one-way accumulator.
///
/// Confirmed this can fail: temporarily deleted the `else` branch of
/// `transport::tick_transport_condition`'s `damaged` check (so an
/// uncontested line's `condition` was simply left unchanged forever instead
/// of stepped toward `Condition::FULL`) and re-ran - `condition` stayed
/// pinned at its damaged floor for the entire post-enemy-departure window,
/// and the final assertion below failed. Reverted before committing.
#[test]
fn transport_line_condition_recovers_after_damage_stops() {
    let mut world = branch_world();
    let ids = branch_ids();
    let hub = branch_region(&ids, "hub");
    let source = branch_region(&ids, "source");

    let line_index = world
        .transport_lines
        .iter()
        .position(|l| world.transport_node(l.from).region == source && world.transport_node(l.to).region == hub)
        .expect("BRANCH_SCENARIO must declare a source -> hub line");

    // An at-war enemy unit contests `hub`, damaging every line touching it
    // (`transport::tick_transport_condition`'s doc) - `BRANCH_SCENARIO`'s
    // `f2` (home region `enemy_home`, otherwise untouched by this test)
    // starts at war with `f1` by default (`Diplomacy::new`'s unconditional
    // baseline, since `BRANCH_SCENARIO` declares no blocs).
    let owner = world.region(hub).owner;
    let enemy = world.faction(FactionId(1)).id;
    let raider = crate::ids::UnitId(world.units.len() as u32);
    world.units.push(military::Unit {
        id: raider,
        owner: enemy,
        name: "Enemy Raider".to_string(),
        station: Station::Region(hub),
        movement: None,
        manpower: 1.0,
        equipment: 1.0,
        organization: 100.0,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(hub),
        experience: 0.0,
        alive: true,
    });
    assert!(world.diplomacy.is_at_war(owner, enemy), "test setup requires the raider's faction to be at war with hub's owner");

    for _ in 0..30 {
        transport::tick_transport_condition(&mut world);
    }
    let damaged = world.transport_lines[line_index].condition.get();
    assert!(damaged < 0.5, "30 days of contest should have driven condition well below its starting 1.0: {damaged}");

    // The enemy withdraws - `hub` is no longer contested.
    world.units[raider.index()].alive = false;
    for _ in 0..30 {
        transport::tick_transport_condition(&mut world);
    }
    let recovered = world.transport_lines[line_index].condition.get();

    assert!(
        recovered > damaged + 0.1,
        "condition must recover once the line is no longer contested, not stay pinned at its damaged floor: \
         damaged={damaged}, recovered={recovered}"
    );
}

// ---------------------------------------------------------------------
// Stage 7C (docs/phase7-spec.md "Stage 7C の受け入れ基準"): the client's
// supply-overlay/blockade display reads `logistics::supply_routes`/
// `logistics::supply_link_flows`/`naval::blockaded_ports` - all read-only,
// all re-derived from `world.supply`/`SeaZone::control` as already computed
// by the real tick systems, never a second source of truth. These three
// tests guard that reconstruction against silently drifting from what
// actually happened.
// ---------------------------------------------------------------------

/// `supply_route_reconstruction_matches_logistics`: on a board with a real,
/// single-corridor dependency (kita_tohoku's only route to the sole source,
/// kanto, runs through minami_tohoku - `supply_corridor_cut`'s own setup),
/// `logistics::supply_routes` names minami_tohoku as kita_tohoku's source,
/// and `logistics::supply_link_flows`'s independently-computed flow for
/// that exact link matches `world.supply[kita_tohoku]` exactly - two
/// separately-written functions over the same board, cross-checked against
/// each other and against the real `recompute_supply` output they both
/// re-derive from.
///
/// Confirmed this can fail: temporarily dropped the `Inbound` node-side cap
/// from `compute_transport_flow` for `supply_link_flows`'s own re-run
/// (leaving `supply_routes`'s independent re-run unchanged) and re-ran - the
/// final flow-vs-cap assertion failed (`supply_link_flows` reported a larger
/// number than `world.supply` actually held). Reverted before committing.
///
/// Stage 9B rewrite: `world.supply[region]` is demand-bounded now (this
/// module's own doc) - mvp's default unit placement never posts one at
/// kita_tohoku (`supply_corridor_cut`'s own doc), so a garrison unit is
/// added there to make the sanity check meaningful.
#[test]
fn supply_route_reconstruction_matches_logistics() {
    fn isolate_single_source(world: &mut World, source: RegionId) {
        let faction = world.region(source).owner;
        for i in 0..world.regions.len() {
            let r = RegionId(i as u32);
            if r != source && world.region(r).owner == faction {
                world.region_mut(r).capacity = [0.0; GOOD_COUNT];
                world.region_mut(r).port = 0.0;
            }
        }
        for good in crate::good::ALL_GOODS {
            world.region_mut(source).capacity[good.index()] = 1000.0;
        }
        world.region_mut(source).infrastructure = 1.0;
    }

    let kanto = RegionId(3);
    let minami_tohoku = RegionId(2);
    let kita_tohoku = RegionId(1);

    let mut world = scenario::build_world();
    isolate_single_source(&mut world, kanto);
    let id = crate::ids::UnitId(world.units.len() as u32);
    world.units.push(military::Unit {
        id,
        owner: world.region(kita_tohoku).owner,
        name: "Garrison".to_string(),
        station: Station::Region(kita_tohoku),
        movement: None,
        manpower: 1.0,
        equipment: UNIT_EQUIPMENT,
        organization: 100.0,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(kita_tohoku),
        experience: 0.0,
        alive: true,
    });
    logistics::recompute_supply(&mut world);

    let routes = logistics::supply_routes(&world);
    let kita_route = routes.iter().find(|r| r.region == kita_tohoku).expect("kita_tohoku must have a route entry");

    assert!(world.supply[kita_tohoku.index()] > 0.0, "sanity: kita_tohoku must actually receive some supply");
    assert_eq!(
        kita_route.source,
        logistics::SupplySource::Relay(minami_tohoku),
        "kita_tohoku's only route to the sole source (kanto) runs through minami_tohoku, not its own base"
    );

    let flows = logistics::supply_link_flows(&world);
    let corridor_flow = flows
        .iter()
        .find(|f| f.from == minami_tohoku && f.to == kita_tohoku)
        .expect("the minami_tohoku -> kita_tohoku link must appear in supply_link_flows");
    assert!(
        (corridor_flow.throughput.flow() - kita_route.cap).abs() < 0.01,
        "the reconstructed corridor's own flow ({}) must match kita_tohoku's actual world.supply entry ({})",
        corridor_flow.throughput.flow(),
        kita_route.cap
    );
}

/// `chokepoint_is_flagged_when_saturated`: the 中国—九州 Kanmon corridor
/// (transport-network `TransportLine` capacity 8.0, the lowest in the mvp
/// map), once 中国 is boosted into a saturated source exactly as
/// `kanmon_tunnel_survives_blockade` sets up *and* 九州's own demand is
/// pushed well past that 8.0 ceiling, is flagged `is_saturated`; the same
/// link on an ordinary, unmodified board is not.
///
/// Confirmed this can fail: temporarily hardcoded
/// `LinkThroughput::is_saturated` to always return `false` and re-ran - the
/// positive assertion below failed immediately. Reverted before committing.
///
/// Stage 9B rewrite: `world.supply[region]` (and therefore this line's own
/// committed flow) is demand-bounded now, so a small demand never pushes a
/// line to its own capacity ceiling regardless of how saturated the source
/// is - a heavy garrison is added at 九州 so demand there genuinely exceeds
/// the corridor's own 8.0 capacity (`japan47_chokepoints_still_bind`'s
/// `station_garrison` makes the same point for its own Kanmon sub-test).
#[test]
fn chokepoint_is_flagged_when_saturated() {
    let chugoku = RegionId(7);
    let kyushu = RegionId(9);

    let mut world = scenario::build_world();
    for good in crate::good::ALL_GOODS {
        world.region_mut(chugoku).capacity[good.index()] = 1000.0;
    }
    world.region_mut(chugoku).infrastructure = 1.0;
    world.region_mut(kyushu).port = 0.0;
    world.region_mut(kyushu).capacity = [0.0; GOOD_COUNT];
    for i in 0..20 {
        let id = crate::ids::UnitId(world.units.len() as u32);
        world.units.push(military::Unit {
            id,
            owner: world.region(kyushu).owner,
            name: format!("Garrison {i}"),
            station: Station::Region(kyushu),
            movement: None,
            manpower: 1.0,
            equipment: UNIT_EQUIPMENT,
            organization: 100.0,
            morale: 1.0,
            supply: 1.0,
            arms_delivery: 1.0,
            arms_budget: 0.0,
            arms_delivery_station: Station::Region(kyushu),
            experience: 0.0,
            alive: true,
        });
    }

    logistics::recompute_supply(&mut world);

    let flows = logistics::supply_link_flows(&world);
    let kanmon = flows
        .iter()
        .find(|f| f.from == chugoku && f.to == kyushu)
        .expect("the Kanmon tunnel link must appear in supply_link_flows");

    assert!(kanmon.throughput.flow() > 0.0, "sanity: the tunnel must actually be relaying something");
    assert!(
        kanmon.throughput.is_saturated(),
        "a link whose flow ({}) has reached its own max_throughput ({}) must be flagged as a chokepoint",
        kanmon.throughput.flow(),
        kanmon.throughput.capacity()
    );

    let mut idle_world = scenario::build_world();
    logistics::recompute_supply(&mut idle_world);
    let idle_flows = logistics::supply_link_flows(&idle_world);
    let idle_kanmon = idle_flows
        .iter()
        .find(|f| f.from == chugoku && f.to == kyushu)
        .expect("the Kanmon tunnel link must appear in supply_link_flows on the unmodified board too");
    assert!(
        !idle_kanmon.throughput.is_saturated(),
        "the tunnel must not be flagged as a chokepoint on an ordinary, unsaturated board: flow={}, capacity={}",
        idle_kanmon.throughput.flow(),
        idle_kanmon.throughput.capacity()
    );
}

/// `blockaded_port_is_flagged`: a port under full enemy sea control (exactly
/// `blockade_stops_import`'s own setup) shows up in `naval::blockaded_ports`
/// together with the sea zone(s) actually responsible for it; an
/// unblockaded port does not appear at all.
///
/// Confirmed this can fail: temporarily changed `blockaded_ports`'s filter
/// from `is_port_blockaded(world, r.id)` to a constant `false` and re-ran -
/// the positive assertion below failed immediately (the blockaded region no
/// longer appeared in the returned list at all). Reverted before
/// committing.
#[test]
fn blockaded_port_is_flagged() {
    let mut world = scenario::build_world();
    let tokai = RegionId(5); // faction 1's port region, per `blockade_stops_import`

    for zone in world.zones_touching(tokai) {
        world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];
    }
    assert!(naval::is_port_blockaded(&world, tokai), "sanity: this setup must actually blockade the port");

    let blockades = naval::blockaded_ports(&world);
    let entry = blockades.iter().find(|b| b.region == tokai).expect("the blockaded port must appear in naval::blockaded_ports");

    assert!(!entry.causes.is_empty(), "a blockaded port must name at least one responsible sea zone");
    for &zone in &entry.causes {
        assert!(
            world.zones_touching(tokai).contains(&zone),
            "every named cause must actually be a sea zone touching the blockaded port"
        );
    }

    // Negative control: an unblockaded port (信越・北陸, a disjoint sea zone
    // per `blockade_is_per_port`) must not appear at all.
    let hokuriku = RegionId(4);
    assert!(
        blockades.iter().all(|b| b.region != hokuriku),
        "an unblockaded port must not appear in naval::blockaded_ports"
    );
}

/// Code review fix: `Observation::encode()`'s per-transport-node
/// `blockaded` field is documented (`TRANSPORT_NODE_FIELD_COUNT`'s own doc)
/// as `false` for every non-`Port` node, but used to call
/// `naval::is_port_blockaded` for *every* node regardless of `kind` - that
/// function only ever answers a *region's* question, so a blockaded
/// region's `Depot`/`Junction` nodes read blockaded too. Wrong on every
/// shipped scenario: every `mvp.json` region carries both a depot and a
/// port node (CLAUDE.md's own FIX 1 note), so this was live on the
/// reference scenario, not a hypothetical.
///
/// Confirmed this can fail: reverted the `node.kind == TransportNodeKind::
/// Port` gate in `observation.rs` (calling `is_port_blockaded` for every
/// node again) and re-ran - the negative assertion below failed immediately
/// (東海's depot read `1.0` blockaded). Reverted before committing.
#[test]
fn blockade_flag_is_port_only_in_observation() {
    let mut world = scenario::build_world();
    let faction = FactionId(1); // owns 東海 (region 5), same setup as `blockade_stops_import`
    let tokai = RegionId(5);

    for zone in world.zones_touching(tokai) {
        world.sea_zone_mut(zone).control = vec![1.0, 0.0, 0.0];
    }
    assert!(naval::is_port_blockaded(&world, tokai), "sanity: this setup must actually blockade the port");

    let depot = world
        .transport_nodes
        .iter()
        .position(|n| n.region == tokai && n.kind == TransportNodeKind::Depot)
        .expect("東海 must have its own depot node, per CLAUDE.md's own FIX 1 note");
    let port = world
        .transport_nodes
        .iter()
        .position(|n| n.region == tokai && n.kind == TransportNodeKind::Port)
        .expect("東海 must have its own port node");

    let obs = Observation { faction, world: &world };
    let encoded = obs.encode();

    let nodes_offset = encoded.len()
        - world.transport_nodes.len() * TRANSPORT_NODE_FIELD_COUNT;
    let blockaded_field = |node_index: usize| encoded[nodes_offset + node_index * TRANSPORT_NODE_FIELD_COUNT + 1];

    assert_eq!(
        blockaded_field(depot),
        0.0,
        "a blockaded region's own Depot node must not read blockaded - only its Port node is under blockade"
    );
    assert_eq!(blockaded_field(port), 1.0, "the blockaded region's own Port node must read blockaded");
}

// ---------------------------------------------------------------------------
// Scenario-declared victory conditions (design.md §5: "勝利条件は一つに限定
// しない"; docs/future-work.md "japan47 が 720 日で決着しない"). Every
// scenario must declare which of `VictoryCondition::{Conquest, Coalition,
// Domination}` are active - `Simulation::outcome` checks only what
// `world.victory` names, in that order. These tests build on the embedded
// `scenarios/mvp.json` world (`Simulation::new`, 3 factions / 10 regions,
// declaring `Conquest` alone) and override `world.victory` directly to
// exercise each condition in isolation, the same way other tests here
// override `world.diplomacy`/`world.regions` directly rather than needing a
// bespoke scenario file per case.
// ---------------------------------------------------------------------------

/// `missing_victory_declaration_is_rejected` (above, alongside
/// `missing_diplomacy_declaration_is_rejected`) already covers an absent
/// `victory` field. This covers the other half of "malformed": a present
/// field naming an unrecognised condition `type`, or a `domination` entry
/// whose `share` falls outside `DominationShare`'s guaranteed `(0.0, 1.0]`
/// range - both must be rejected at load time, never silently coerced or
/// defaulted.
///
/// Confirmed this can fail: temporarily made `parse_victory_condition`'s
/// `"domination"` arm skip the `DominationShare::new` validation and store
/// the raw `f32` via a hypothetical unchecked constructor instead, and
/// re-ran - the out-of-range-share case below stopped returning `Err` at
/// all. Reverted before committing.
#[test]
fn malformed_victory_declaration_is_rejected() {
    let unknown_type = MINI_VALID_SCENARIO.replacen(
        r#""victory": [ { "type": "conquest" } ]"#,
        r#""victory": [ { "type": "world_domination" } ]"#,
        1,
    );
    match scenario::load_str(&unknown_type) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("world_domination"), "expected the error to name the unknown type, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for an unknown victory condition type, got {other:?}"),
    }

    for bad_share in ["0.0", "-0.5", "1.5"] {
        let bad_domination = MINI_VALID_SCENARIO.replacen(
            r#""victory": [ { "type": "conquest" } ]"#,
            &format!(r#""victory": [ {{ "type": "domination", "share": {bad_share} }} ]"#),
            1,
        );
        match scenario::load_str(&bad_domination) {
            Err(scenario::ScenarioError::Schema(msg)) => {
                assert!(msg.contains("share"), "expected the error to name `share` for share={bad_share}, got {msg:?}");
            }
            other => panic!("expected a distinct Schema error for domination share={bad_share}, got {other:?}"),
        }
    }
}

/// An empty `"victory": []` is not the same failure as an unrecognised
/// condition `type` (`malformed_victory_declaration_is_rejected`, above) -
/// it's syntactically a valid array of zero elements, so it must be caught
/// on its own. Left unrejected, it would parse and validate successfully
/// while leaving `Simulation::outcome`'s `evaluate_victory` nothing to ever
/// check - the exact permanently-unreachable-victory state a required
/// `victory` declaration exists to prevent, reachable even with a single
/// faction left `alive`. `world::VictoryDeclaration::new` makes this state
/// unrepresentable at all (`VictoryDeclaration`'s doc): there is no way to
/// build one from an empty `Vec`, so `parse_victory` must fail here rather
/// than at a `world.victory.is_empty()` check some caller could forget.
///
/// Confirmed this can fail: temporarily changed `parse_victory` to return
/// `Ok(VictoryDeclaration(Vec::new()))` directly (bypassing `new`'s check)
/// for an empty array instead of propagating an error, and re-ran - `load_str`
/// stopped returning `Err` at all for this input. Reverted before committing.
#[test]
fn empty_victory_declaration_is_rejected() {
    let empty_victory = MINI_VALID_SCENARIO.replacen(r#""victory": [ { "type": "conquest" } ]"#, r#""victory": []"#, 1);
    match scenario::load_str(&empty_victory) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("victory"), "expected the error to name `victory`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for an empty `victory` array, got {other:?}"),
    }
}

/// `VictoryCondition::Conquest` fires exactly as `Outcome::Victory` always
/// did before scenario-declared conditions existed: the instant only one
/// faction remains `alive`, naming that faction and no one else.
/// `scenarios/mvp.json` declares only this condition, which is exactly what
/// keeps its `--json` hash for seed 1 / 720 days unchanged.
///
/// Confirmed this can fail: temporarily changed the `Conquest` arm of
/// `victory_winners` to `alive.len() == 0` (an impossible bar, since an
/// empty `alive` is Stalemate's business per `outcome`'s own check that
/// runs after `evaluate_victory`) and re-ran - the assertion below failed
/// with `Ongoing` instead of `Victory`. Reverted before committing.
#[test]
fn conquest_victory_fires_when_one_faction_survives() {
    let mut sim = Simulation::new(1);
    assert_eq!(
        sim.world.victory,
        VictoryDeclaration::new(vec![VictoryCondition::Conquest]).unwrap(),
        "sanity: mvp.json must declare Conquest alone"
    );

    let survivor = FactionId(0);
    sim.world.factions[1].alive = false;
    sim.world.factions[2].alive = false;

    match sim.outcome(1_000) {
        Outcome::Victory { condition: VictoryCondition::Conquest, winners } => {
            assert_eq!(winners, vec![survivor], "Conquest must name exactly the one surviving faction");
        }
        other => panic!("expected a Conquest victory for the lone survivor, got {other:?}"),
    }
}

/// `VictoryCondition::Coalition`: fires once every surviving faction
/// belongs to one mutually-allied group, naming every member of that group
/// - not just while a lone survivor remains outside it.
///
/// Confirmed this can fail: temporarily changed the `Coalition` arm of
/// `victory_winners` to compare `group.len() >= 1` instead of `group.len()
/// == alive.len()` and re-ran - the negative assertion below (f2 still
/// outside the pair) failed, firing a spurious victory the instant f0/f1
/// signed their alliance while f2 was still alive and at war. Reverted
/// before committing.
#[test]
fn coalition_victory_requires_every_survivor_in_the_group() {
    let mut sim = Simulation::new(1);
    sim.world.victory = VictoryDeclaration::new(vec![VictoryCondition::Coalition]).unwrap();
    let f0 = FactionId(0);
    let f1 = FactionId(1);
    let f2 = FactionId(2);

    action::apply_action(&mut sim.world, f0, Action::ProposeTreaty { to: f1, treaty: Treaty::Alliance }).unwrap();
    action::apply_action(&mut sim.world, f1, Action::AcceptTreaty { from: f0, treaty: Treaty::Alliance }).unwrap();
    assert_eq!(
        sim.world.diplomacy.stance(f0, f1),
        diplomacy::Stance::Alliance,
        "sanity: f0/f1 must actually be allied now"
    );

    assert_eq!(
        sim.outcome(1_000),
        Outcome::Ongoing,
        "f2 is alive and outside the f0/f1 alliance, so Coalition must not fire yet"
    );

    // f2 is eliminated; the remaining survivors (f0, f1) are now one
    // mutually-allied group covering every survivor.
    sim.world.factions[f2.index()].alive = false;
    match sim.outcome(1_000) {
        Outcome::Victory { condition: VictoryCondition::Coalition, winners } => {
            assert_eq!(winners, vec![f0, f1], "Coalition must name every member of the winning group");
        }
        other => panic!("expected a Coalition victory once every survivor is mutually allied, got {other:?}"),
    }
}

/// `VictoryCondition::Domination`: fires the instant a faction's holdings
/// clear the declared `DominationShare` of the map's regions, and not
/// before - `scenarios/mvp.json`'s embedded world has exactly 10 regions,
/// so a 0.6 threshold is a clean 6/10 boundary.
///
/// Confirmed this can fail: temporarily changed the `Domination` arm of
/// `victory_winners` to compare `held as f32 > share.get() * total_regions`
/// (strictly greater) instead of `>=` and re-ran - the exact-boundary
/// assertion below (6/10 == 0.6) failed, staying `Ongoing` instead of
/// firing. Reverted before committing.
#[test]
fn domination_victory_fires_at_threshold_not_below() {
    let mut sim = Simulation::new(1);
    let share = DominationShare::new(0.6).expect("0.6 is a valid domination share");
    sim.world.victory = VictoryDeclaration::new(vec![VictoryCondition::Domination(share)]).unwrap();
    let f0 = FactionId(0);
    let f1 = FactionId(1);
    assert_eq!(sim.world.regions.len(), 10, "sanity: mvp.json's embedded world has 10 regions");

    for (i, region) in sim.world.regions.iter_mut().enumerate() {
        region.owner = if i < 5 { f0 } else { f1 };
    }
    assert_eq!(
        sim.outcome(1_000),
        Outcome::Ongoing,
        "5/10 regions (50%) must not clear a 60% domination threshold"
    );

    sim.world.regions[5].owner = f0;
    match sim.outcome(1_000) {
        Outcome::Victory { condition: VictoryCondition::Domination(fired_share), winners } => {
            assert_eq!(winners, vec![f0]);
            assert_eq!(fired_share, share);
        }
        other => panic!("expected a Domination victory at exactly the 60% threshold (6/10 regions), got {other:?}"),
    }
}

/// `VictoryCondition::Domination` reads the same "allied group" `Coalition`
/// uses: an allied group's *combined* holdings count toward the threshold,
/// so a bloc can dominate the map together even though no single member
/// holds the share alone.
///
/// Confirmed this can fail: temporarily changed the `Domination` arm of
/// `victory_winners` to compute `held` from `world.region_count(f)` alone
/// (the lone faction being checked, ignoring `group`) instead of summing
/// over the whole `group`, and re-ran - the final assertion below failed,
/// staying `Ongoing` forever since no single faction here ever holds 60%
/// alone. Reverted before committing.
#[test]
fn allied_groups_combined_holdings_count_toward_domination() {
    let mut sim = Simulation::new(1);
    let share = DominationShare::new(0.6).expect("0.6 is a valid domination share");
    sim.world.victory = VictoryDeclaration::new(vec![VictoryCondition::Domination(share)]).unwrap();
    let f0 = FactionId(0);
    let f1 = FactionId(1);
    let f2 = FactionId(2);
    assert_eq!(sim.world.regions.len(), 10, "sanity: mvp.json's embedded world has 10 regions");

    // f0: 3 regions, f1: 3 regions, f2: 4 regions - no single faction
    // reaches 6/10 (60%) alone.
    for (i, region) in sim.world.regions.iter_mut().enumerate() {
        region.owner = if i < 3 {
            f0
        } else if i < 6 {
            f1
        } else {
            f2
        };
    }
    assert_eq!(sim.outcome(1_000), Outcome::Ongoing, "no single faction holds 60% of the map alone");

    // f0 and f1 ally; their combined 6/10 regions now clears the threshold.
    action::apply_action(&mut sim.world, f0, Action::ProposeTreaty { to: f1, treaty: Treaty::Alliance }).unwrap();
    action::apply_action(&mut sim.world, f1, Action::AcceptTreaty { from: f0, treaty: Treaty::Alliance }).unwrap();

    match sim.outcome(1_000) {
        Outcome::Victory { condition: VictoryCondition::Domination(_), winners } => {
            assert_eq!(winners, vec![f0, f1], "the allied group's combined holdings must be named together");
        }
        other => panic!("expected the allied group's combined 6/10 regions to trigger Domination, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Layer classification (docs task "make agents pluggable per decision
// layer"): `Action::layer`/`Action::target_unit` are exhaustive matches
// enforced by the compiler (no wildcard arm - see both methods' own docs),
// so the *existence* of a classification for every variant can never
// regress silently. What the compiler cannot check is that a variant sits
// in the *right* bucket - that is what these tests pin down, one concrete
// sample of every one of `Action`'s 20 variants at a time.
// ---------------------------------------------------------------------

/// One concrete, arbitrary-but-valid sample of every `Action` variant,
/// paired with the `Layer` it must classify into. Exhaustively listing
/// every variant here (rather than looping over some smaller
/// representative subset) is what makes `every_action_variant_has_the_expected_layer`
/// actually cover the whole enum - a variant added to `Action` without a
/// matching entry added here is caught by `layer_classification_is_exhaustive_over_all_samples`
/// below, not silently skipped.
fn action_layer_samples() -> Vec<(Action, Layer)> {
    let unit = UnitId(0);
    let region = RegionId(0);
    let other_region = RegionId(1);
    let faction = FactionId(1);
    vec![
        (Action::MoveUnit { unit, to: Station::Region(other_region) }, Layer::Military),
        (Action::HoldUnit { unit }, Layer::Military),
        (Action::DisbandUnit { unit }, Layer::Military),
        (Action::ReinforceUnit { unit }, Layer::Military),
        (Action::RecruitUnit { region, domain: Domain::Land }, Layer::Military),
        (Action::Build { region, project: Project::Infrastructure }, Layer::Economy),
        (Action::CancelBuild { region }, Layer::Economy),
        (Action::SetConscription(0.2), Layer::Economy),
        (Action::SetCivilianRation(0.8), Layer::Economy),
        (Action::SetIndustryPriority { good: Good::Steel, weight: 0.5 }, Layer::Economy),
        (Action::SetLogisticsPriority { good: Good::Munitions, weight: 0.5 }, Layer::Economy),
        (Action::SetImportPlan { good: Good::Food, rate: 1.0 }, Layer::Economy),
        (Action::SetNationalFocus(NationalFocus::EconomicSphere), Layer::GrandStrategy),
        (Action::ProposeTreaty { to: faction, treaty: Treaty::NonAggression }, Layer::Diplomacy),
        (Action::AcceptTreaty { from: faction, treaty: Treaty::NonAggression }, Layer::Diplomacy),
        (Action::RejectTreaty { from: faction, treaty: Treaty::NonAggression }, Layer::Diplomacy),
        (Action::DeclareWar { to: faction }, Layer::Diplomacy),
        (Action::BreakTreaty { with: faction, treaty: Treaty::Alliance }, Layer::Diplomacy),
        (Action::ProposeInNaturalLanguage { to: faction, text: "hello".to_string() }, Layer::Diplomacy),
        (
            Action::RespondToNaturalLanguageProposal { from: faction, terms: vec![], accept: true },
            Layer::Diplomacy,
        ),
        (Action::InterdictLine { line: crate::ids::TransportLineId(0) }, Layer::Military),
    ]
}

/// Every `Action` variant must classify into exactly the layer this
/// codebase's design intends - see `Layer`'s own doc in `action.rs` for the
/// reasoning behind each boundary (`RecruitUnit` into `Military` despite
/// spending economic resources, `Build`/`CancelBuild` into `Economy` rather
/// than a separate infrastructure layer, `SetNationalFocus` alone in
/// `GrandStrategy`).
///
/// Confirmed this can actually fail: temporarily changed `Action::layer`'s
/// `RecruitUnit` arm to return `Layer::Economy` and re-ran - the assertion
/// for that sample failed with the mismatch spelled out. Reverted before
/// committing.
#[test]
fn every_action_variant_has_the_expected_layer() {
    for (action, expected) in action_layer_samples() {
        assert_eq!(action.layer(), expected, "{action:?} should classify as {expected:?}");
    }
}

/// `action_layer_samples` must itself list exactly one sample per `Action`
/// variant - 21 entries, matching the count in this module's own doc and in
/// `crates/api/src/action_codec.rs`'s decoder. This is what stands in for
/// the compiler's own exhaustiveness check (which `Action::layer`'s
/// wildcard-free `match` already enforces at the type level) at the level
/// of *this test suite*: if a future variant is added to `Action` without a
/// corresponding sample added above, this count assertion catches the gap
/// even though the crate itself still compiles fine (the new variant would
/// simply never be exercised by `every_action_variant_has_the_expected_layer`
/// otherwise).
#[test]
fn layer_classification_is_exhaustive_over_all_samples() {
    assert_eq!(action_layer_samples().len(), 21, "one sample per Action variant - update this alongside any new variant");
}

/// `ALL_LAYERS` must list every `Layer` variant exactly once, in the fixed
/// order `Layer::index` agrees with - `agents::CompositeAgent` and the API
/// schema both iterate this array rather than the enum itself.
#[test]
fn all_layers_is_complete_and_indexed_consistently() {
    assert_eq!(ALL_LAYERS.len(), crate::action::LAYER_COUNT);
    for (i, layer) in ALL_LAYERS.iter().enumerate() {
        assert_eq!(layer.index(), i, "ALL_LAYERS's position must match Layer::index for {layer:?}");
    }
    let mut seen: Vec<Layer> = Vec::new();
    for layer in ALL_LAYERS {
        assert!(!seen.contains(&layer), "{layer:?} appears more than once in ALL_LAYERS");
        seen.push(layer);
    }
}

/// `Action::target_unit` must return the exact unit a `Military`-layer
/// order names, and `None` for everything else - including `RecruitUnit`,
/// which is `Military` but names no *existing* unit (see that method's own
/// doc for why). This is `agents::HumanAgent`'s per-unit delegation filter,
/// so a regression here silently changes which orders a delegated unit
/// receives.
///
/// Confirmed this can actually fail: temporarily made `target_unit` return
/// `None` for `Action::ReinforceUnit` and re-ran - the assertion for that
/// sample failed. Reverted before committing.
#[test]
fn target_unit_identifies_exactly_the_unit_orders() {
    let ordered_unit = UnitId(7);
    let region = RegionId(0);
    let faction = FactionId(1);
    let cases: Vec<(Action, Option<UnitId>)> = vec![
        (Action::MoveUnit { unit: ordered_unit, to: Station::Region(RegionId(1)) }, Some(ordered_unit)),
        (Action::HoldUnit { unit: ordered_unit }, Some(ordered_unit)),
        (Action::DisbandUnit { unit: ordered_unit }, Some(ordered_unit)),
        (Action::ReinforceUnit { unit: ordered_unit }, Some(ordered_unit)),
        (Action::RecruitUnit { region, domain: Domain::Land }, None),
        (Action::SetConscription(0.5), None),
        (Action::SetNationalFocus(NationalFocus::Technocracy), None),
        (Action::ProposeTreaty { to: faction, treaty: Treaty::Ceasefire }, None),
        (Action::InterdictLine { line: crate::ids::TransportLineId(0) }, None),
    ];
    for (action, expected) in cases {
        assert_eq!(action.target_unit(), expected, "{action:?} should target {expected:?}");
    }
}

// ---------------------------------------------------------------------------
// Stage 9A (docs/phase9-spec.md "1. 層の分離", "3. データ"): the transport
// network's types, scenario schema and validation. Supply computation is
// unchanged - `logistics::recompute_supply` still propagates over
// `Region::links` exactly as it did before this stage, which is why every
// pre-existing test above still passes unmodified once `MINI_VALID_SCENARIO`/
// `BLOC_SCENARIO` gained a minimal, valid `transport` block of their own.
// ---------------------------------------------------------------------------

/// `MINI_VALID_SCENARIO`'s `transport` block must build into the exact
/// `TransportNode`/`TransportLine` values it describes: three `Depot` nodes
/// (none of `a`/`b`/`c` has a port, so no `Port` node), each resolved to its
/// own region, and two `Rail` lines resolved to the right node ids in
/// declaration order (`TransportNodeId` assignment mirrors `RegionId`'s own
/// "file order fixes id" convention - `Scenario::transport_nodes`'s doc).
///
/// Confirmed this can fail: temporarily swapped `TransportNode { region:
/// RegionId(index_of[def.region.as_str()]), .. }` in `Scenario::build_world`
/// for a hardcoded `RegionId(0)` and re-ran - the `b_depot`/`c_depot`
/// assertions below failed (both reported region `a`). Reverted before
/// committing.
#[test]
fn transport_network_builds_correctly() {
    let world = scenario::load_str(MINI_VALID_SCENARIO).expect("MINI_VALID_SCENARIO must be valid");

    assert_eq!(world.transport_nodes.len(), 3);
    for (i, region) in [(0, RegionId(0)), (1, RegionId(1)), (2, RegionId(2))] {
        let node = &world.transport_nodes[i];
        assert_eq!(node.id, TransportNodeId(i as u32));
        assert_eq!(node.kind, TransportNodeKind::Depot);
        assert_eq!(node.region, region, "node {i} should belong to region {region:?}");
    }

    assert_eq!(world.transport_lines.len(), 2);
    let line0 = world.transport_lines[0];
    assert_eq!((line0.from, line0.to), (TransportNodeId(0), TransportNodeId(1)), "a_depot -> b_depot");
    assert_eq!(line0.kind, TransportLineKind::Rail);
    assert_eq!(line0.capacity, Capacity::new(25.0).unwrap());
    assert_eq!(line0.condition, Condition::new(1.0).unwrap());

    let line1 = world.transport_lines[1];
    assert_eq!((line1.from, line1.to), (TransportNodeId(1), TransportNodeId(2)), "b_depot -> c_depot");
}

/// docs/phase9-spec.md §7 Stage 9A: "輸送網が欠けたシナリオ... 明確なエラー
/// になる" - a scenario that omits the required `transport` field entirely
/// must be rejected, naming exactly what's missing, per docs/conventions.md
/// §3's "フォールバック原則禁止" applied here exactly as it already is to
/// `diplomacy`/`victory`/`position` (there is no implied "derive it from
/// `Region::links`" fallback - docs/phase9-spec.md §3's own explicit ban).
///
/// Confirmed this can fail: temporarily changed `Scenario::parse` to treat a
/// missing `transport` field as `(Vec::new(), Vec::new())` instead of
/// propagating `require_object_field`'s error, and re-ran - `load_str`
/// stopped returning `Err` for this input (and built a `World` with an
/// empty transport network no scenario ever declared). Reverted before
/// committing.
#[test]
fn missing_transport_declaration_is_rejected() {
    let no_transport = MINI_VALID_SCENARIO.replacen(
        "  \"sea_zones\": [],\n  \"transport\": {\n    \"nodes\": [\n      { \"id\": \"a_depot\", \"name\": \"A Depot\", \"kind\": \"depot\", \"region\": \"a\" },\n      { \"id\": \"b_depot\", \"name\": \"B Depot\", \"kind\": \"depot\", \"region\": \"b\" },\n      { \"id\": \"c_depot\", \"name\": \"C Depot\", \"kind\": \"depot\", \"region\": \"c\" }\n    ],\n    \"lines\": [\n      { \"from\": \"a_depot\", \"to\": \"b_depot\", \"kind\": \"rail\", \"capacity\": 25.0, \"condition\": 1.0 },\n      { \"from\": \"b_depot\", \"to\": \"c_depot\", \"kind\": \"rail\", \"capacity\": 25.0, \"condition\": 1.0 }\n    ]\n  },\n",
        "  \"sea_zones\": [],\n",
        1,
    );
    match scenario::load_str(&no_transport) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("transport"), "expected the error to name `transport`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a missing `transport` field, got {other:?}"),
    }
}

/// docs/phase9-spec.md §7 Stage 9A: "存在しないノードを指す路線... 明確な
/// エラーになる" - a line naming a node id nothing declares must be
/// rejected, distinctly from every other error.
///
/// Confirmed this can fail: temporarily removed the `transport_node_ids.
/// contains(l.to.as_str())` check from `Scenario::validate` (kept only the
/// `l.from` check) and re-ran - `load_str` no longer returned `Err` at all
/// for this input, and `Scenario::build_world`'s `transport_node_index_of[..]`
/// lookup for the dangling `to` id would have panicked instead. Reverted
/// before committing.
#[test]
fn transport_line_with_unknown_node_is_rejected() {
    let dangling_line = MINI_VALID_SCENARIO.replacen(
        r#"{ "from": "b_depot", "to": "c_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 }"#,
        r#"{ "from": "b_depot", "to": "c_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "c_depot", "to": "nowhere_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 }"#,
        1,
    );
    match scenario::load_str(&dangling_line) {
        Err(scenario::ScenarioError::UnknownId { id, .. }) => assert_eq!(id, "nowhere_depot"),
        other => panic!("expected a distinct UnknownId error, got {other:?}"),
    }
}

/// docs/phase9-spec.md §7 Stage 9A: "所属地域のないノード... 明確なエラーに
/// なる" - a `TransportNode` whose `region` names an id nothing declares
/// must be rejected, distinctly from every other error (`transport::
/// TransportNode::region`'s doc: every node belongs to exactly one region).
///
/// Confirmed this can fail: temporarily removed the `region_ids.contains(n.
/// region.as_str())` check for transport nodes from `Scenario::validate`
/// and re-ran - `load_str` no longer returned `Err` for this input, and
/// `Scenario::build_world`'s `index_of[def.region.as_str()]` lookup for the
/// dangling region id would have panicked instead. Reverted before
/// committing.
#[test]
fn transport_node_with_unknown_region_is_rejected() {
    let dangling_region = MINI_VALID_SCENARIO.replacen(
        r#"{ "id": "c_depot", "name": "C Depot", "kind": "depot", "region": "c" }"#,
        r#"{ "id": "c_depot", "name": "C Depot", "kind": "depot", "region": "nowhere" }"#,
        1,
    );
    match scenario::load_str(&dangling_region) {
        Err(scenario::ScenarioError::UnknownId { id, .. }) => assert_eq!(id, "nowhere"),
        other => panic!("expected a distinct UnknownId error, got {other:?}"),
    }
}

/// Stage 9A's one consistency rule between `Region::port` and the new
/// node-based representation (docs/phase9-spec.md §1 "港の扱い": "同じ事実
/// が2か所に別々に存在する状態は作らない", `scenario::ScenarioError::
/// PortNodeWithoutRegionPort`'s doc): a `Port` node cannot exist for a
/// region that itself reports no port. `MINI_VALID_SCENARIO`'s region `a`
/// has `"port": 0.0`, so giving it a `Port` node must be rejected.
///
/// Confirmed this can fail: temporarily removed the `region.port <= 0.0`
/// check (kept construction unconditional) from `Scenario::validate` and
/// re-ran - `load_str` stopped returning `Err` for this input, silently
/// accepting a `Port` node for a landlocked region. Reverted before
/// committing.
#[test]
fn port_node_without_region_port_is_rejected() {
    let phantom_port = MINI_VALID_SCENARIO.replacen(
        r#"{ "id": "a_depot", "name": "A Depot", "kind": "depot", "region": "a" },"#,
        r#"{ "id": "a_depot", "name": "A Depot", "kind": "depot", "region": "a" },
      { "id": "a_port", "name": "A Port", "kind": "port", "region": "a" },"#,
        1,
    );
    match scenario::load_str(&phantom_port) {
        Err(scenario::ScenarioError::PortNodeWithoutRegionPort { node, region }) => {
            assert_eq!((node.as_str(), region.as_str()), ("a_port", "a"));
        }
        other => panic!("expected a distinct PortNodeWithoutRegionPort error, got {other:?}"),
    }
}

/// The converse of `port_node_without_region_port_is_rejected`
/// (`scenario::ScenarioError::RegionPortWithoutPortNode`'s doc): a region
/// that reports `port > 0.0` but declares no `Port` node at all must also be
/// rejected, distinctly - otherwise naval logic (still keyed off
/// `Region::port` in Stage 9A) and the transport layer could disagree about
/// whether a region has a port. `MINI_VALID_SCENARIO`'s region `a` starts
/// with `"port": 0.0` and no `Port` node; giving it a nonzero port without
/// adding one must fail.
///
/// Confirmed this can fail: temporarily removed the new
/// `regions_with_port_node` loop (the "converse check" block) from
/// `Scenario::validate` and re-ran - `load_str` stopped returning `Err` for
/// this input, silently accepting a region whose `port` field and transport
/// nodes disagree. Reverted before committing.
#[test]
fn region_port_without_port_node_is_rejected() {
    let phantom_region_port = MINI_VALID_SCENARIO.replacen(
        r#"{ "id": "a", "name": "A", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [0.0, 0.0],"#,
        r#"{ "id": "a", "name": "A", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 5.0, "position": [0.0, 0.0],"#,
        1,
    );
    match scenario::load_str(&phantom_region_port) {
        Err(scenario::ScenarioError::RegionPortWithoutPortNode { region }) => {
            assert_eq!(region.as_str(), "a");
        }
        other => panic!("expected a distinct RegionPortWithoutPortNode error, got {other:?}"),
    }
}

/// Every id-keyed collection in this schema rejects a duplicate id
/// (`region`/`sea zone`/`faction` - see the loops at the top of
/// `Scenario::validate`); transport nodes must be no exception, since
/// `Scenario::build_world`'s `transport_node_index_of` map would otherwise
/// silently collapse two distinct authored nodes onto one `TransportNodeId`.
///
/// Confirmed this can fail: temporarily removed the transport-node
/// duplicate-id loop from `Scenario::validate` and re-ran - `load_str` no
/// longer returned `Err` for this input. Reverted before committing.
#[test]
fn duplicate_transport_node_id_is_rejected() {
    let duplicate = MINI_VALID_SCENARIO.replacen(
        r#"{ "id": "b_depot", "name": "B Depot", "kind": "depot", "region": "b" },"#,
        r#"{ "id": "b_depot", "name": "B Depot", "kind": "depot", "region": "b" },
      { "id": "b_depot", "name": "B Depot Again", "kind": "depot", "region": "b" },"#,
        1,
    );
    match scenario::load_str(&duplicate) {
        Err(scenario::ScenarioError::DuplicateId { kind, id }) => {
            assert_eq!((kind, id.as_str()), ("transport node", "b_depot"));
        }
        other => panic!("expected a distinct DuplicateId error, got {other:?}"),
    }
}

/// `transport::Condition::new` (docs/conventions.md §1, "不正な値をそもそも
/// 構築できなくする") is applied at scenario parse time, the same place
/// `world::DominationShare::new` already is for `victory`'s `share` field
/// (`parse_victory_condition`) - a `condition` outside `0.0..=1.0` is a hard
/// `ScenarioError::Schema`, never silently clamped.
///
/// Confirmed this can fail: temporarily replaced `Condition::new(condition_
/// raw).ok_or_else(..)?` in `scenario::parse_transport_line` with
/// `Condition::new(condition_raw.clamp(0.0, 1.0)).unwrap()` and re-ran -
/// both assertions below failed (`load_str` stopped returning `Err` for
/// either value). Reverted before committing.
#[test]
fn transport_line_condition_out_of_range_is_rejected() {
    let too_high = MINI_VALID_SCENARIO.replacen(
        r#""kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot""#,
        r#""kind": "rail", "capacity": 25.0, "condition": 1.5 },
      { "from": "b_depot""#,
        1,
    );
    match scenario::load_str(&too_high) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("condition"), "expected the error to name `condition`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for condition > 1.0, got {other:?}"),
    }

    let negative = MINI_VALID_SCENARIO.replacen(
        r#""kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot""#,
        r#""kind": "rail", "capacity": 25.0, "condition": -0.1 },
      { "from": "b_depot""#,
        1,
    );
    match scenario::load_str(&negative) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("condition"), "expected the error to name `condition`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a negative condition, got {other:?}"),
    }
}

/// `transport::Capacity::new` (docs/conventions.md §1, applied here exactly
/// as `Condition` already is on the same struct): a negative `capacity` must
/// be rejected at parse time rather than reaching `TransportLine::capacity`
/// as a bare negative `f32` - Stage 9B's planned `capacity * condition` flow
/// limit would otherwise silently corrupt allocation on a negative value.
///
/// Confirmed this can fail: temporarily replaced `Capacity::new(capacity_
/// raw).ok_or_else(..)?` in `scenario::parse_transport_line` with
/// `Capacity::new(capacity_raw.max(0.0)).unwrap()` and re-ran - the
/// assertion below failed (`load_str` stopped returning `Err` for a negative
/// capacity). Reverted before committing.
#[test]
fn transport_line_capacity_negative_is_rejected() {
    let negative = MINI_VALID_SCENARIO.replacen(
        r#""kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot""#,
        r#""kind": "rail", "capacity": -1.0, "condition": 1.0 },
      { "from": "b_depot""#,
        1,
    );
    match scenario::load_str(&negative) {
        Err(scenario::ScenarioError::Schema(msg)) => {
            assert!(msg.contains("capacity"), "expected the error to name `capacity`, got {msg:?}");
        }
        other => panic!("expected a distinct Schema error for a negative capacity, got {other:?}"),
    }
}

/// docs/phase9-spec.md §1 "輸送路線": `Sea` connects a port to a port via a
/// sea zone. A `sea` line whose endpoints are ordinary `Depot` nodes (as in
/// `MINI_VALID_SCENARIO`, which has no `Port` node at all) must be rejected,
/// distinctly from every other error - Stage 9B/9C's blockade and import
/// logic relies on every `Sea` line actually terminating at a `Port`.
///
/// Confirmed this can fail: temporarily removed the `l.kind ==
/// TransportLineKind::Sea` check block from `Scenario::validate` and re-ran
/// - `load_str` stopped returning `Err` for this input, silently accepting
/// a sea line between two depots.
#[test]
fn sea_line_between_non_port_nodes_is_rejected() {
    let sea_between_depots = MINI_VALID_SCENARIO.replacen(
        r#"{ "from": "a_depot", "to": "b_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },"#,
        r#"{ "from": "a_depot", "to": "b_depot", "kind": "sea", "capacity": 25.0, "condition": 1.0 },"#,
        1,
    );
    match scenario::load_str(&sea_between_depots) {
        Err(scenario::ScenarioError::SeaLineNotBetweenPorts { from, to, offending }) => {
            assert_eq!((from.as_str(), to.as_str(), offending.as_str()), ("a_depot", "b_depot", "a_depot"));
        }
        other => panic!("expected a distinct SeaLineNotBetweenPorts error, got {other:?}"),
    }
}

/// The three shipped scenarios must all declare (and pass validation with)
/// a non-empty transport network (docs/phase9-spec.md §3: "すべてのシナリオ
/// が輸送網を宣言する... フォールバック禁止"). `japan_hex.json`'s network is
/// now Stage 9C's real-data-derived one (`tools/hexmap/transport_real.py`),
/// not Stage 9A's mechanical placeholder - its `note` must cite the real
/// MLIT datasets and must no longer claim to be provisional (the literal
/// string this test used to require before Stage 9C landed).
///
/// Confirmed this can fail: temporarily reverted `tools/hexmap/build_
/// scenario.py`'s `TRANSPORT_NOTE` to the old Stage 9A wording (still
/// containing "PROVISIONAL") without touching the actual node/line data,
/// and the "must no longer say PROVISIONAL" assertion below failed as
/// expected. Reverted before committing.
#[test]
fn shipped_scenarios_all_declare_transport_networks() {
    let mvp = scenario::Scenario::parse(scenario::embedded_mvp_json()).expect("mvp.json parses");
    mvp.validate().expect("mvp.json's transport network is valid");
    assert!(!mvp.transport_nodes.is_empty());
    assert!(!mvp.transport_lines.is_empty());

    for path in ["../../scenarios/japan47.json", "../../scenarios/japan_hex.json"] {
        let json_text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        let scenario = scenario::Scenario::parse(&json_text).unwrap_or_else(|e| panic!("{path} must parse: {e}"));
        scenario.validate().unwrap_or_else(|e| panic!("{path}'s transport network must validate: {e}"));
        assert!(!scenario.transport_nodes.is_empty(), "{path} must declare transport nodes");
        assert!(!scenario.transport_lines.is_empty(), "{path} must declare transport lines");
    }

    let hex_text = std::fs::read_to_string("../../scenarios/japan_hex.json").expect("japan_hex.json readable");
    assert!(
        !hex_text.contains("PROVISIONAL"),
        "japan_hex.json's transport network is Stage 9C's real-data-derived one now - it must not still claim to be a placeholder"
    );
    for cite in ["N02", "C02", "Kokudo Suuchi Jouhou"] {
        assert!(
            hex_text.contains(cite),
            "japan_hex.json's transport `note` must cite the real MLIT dataset ({cite}) it was derived from"
        );
    }
}

/// Stage 9C's own §3 verification list (docs/phase9-spec.md), pinned as a
/// regression guard so a future regeneration (a new MLIT data year, a
/// changed matching radius) can't silently drift away from it without a
/// test noticing:
///   - the three Phase 2 chokepoints keep their pre-existing structural
///     kind (関門 a blockade-immune `Rail` line, 青函/瀬戸内 blockade-
///     vulnerable `Sea` lines between `Port` nodes - `crates/sim/src/
///     transport.rs`'s module doc; Stage 9C does not revisit this design
///     choice) and 関門 in particular carries real evidence of being a
///     genuine trunk crossing (capacity well above an ordinary `Road`
///     line's, since the real Sanyo Shinkansen/Kagoshima Main Line both
///     cross there through the Shin-Kanmon/Kanmon tunnels)
///   - the five named major real ports (横浜/名古屋/神戸/北九州/苫小牧) each
///     have a `Port` node whose name identifies that real port
///
/// Confirmed this can fail: temporarily pointed the 関門 lookup at a
/// different (non-tunnel) region pair - the "must be a Rail line" assertion
/// failed as expected. Reverted before committing.
#[test]
fn japan_hex_transport_matches_stage_9c_verification() {
    let json_text = std::fs::read_to_string("../../scenarios/japan_hex.json").expect("japan_hex.json readable");
    let world = scenario::load_str(&json_text).expect("scenarios/japan_hex.json must build a valid World");

    // `TransportNode` doesn't carry its own scenario-file string id (only a
    // numeric `TransportNodeId` and a `region: RegionId`) - resolve by
    // region string id instead, the same `Scenario::parse` file-order
    // convention `japan47_chokepoints_still_bind`'s own `id_of` helper
    // relies on.
    let scenario = scenario::Scenario::parse(&json_text).unwrap();
    let region_ids: Vec<String> = scenario.regions.iter().map(|r| r.id.clone()).collect();
    let region_index = |id: &str| -> RegionId {
        RegionId(region_ids.iter().position(|r| r == id).unwrap_or_else(|| panic!("region {id} must exist")) as u32)
    };

    let find_line = |ra: RegionId, rb: RegionId| -> &crate::transport::TransportLine {
        world
            .transport_lines
            .iter()
            .find(|l| {
                let (na, nb) = (world.transport_node(l.from).region, world.transport_node(l.to).region);
                (na == ra && nb == rb) || (na == rb && nb == ra)
            })
            .unwrap_or_else(|| panic!("no TransportLine directly between {ra:?} and {rb:?}"))
    };

    // 関門 (Kanmon): h4_-4 (福岡, Kyushu side) <-> h4_-3 (山口, Honshu side).
    let kanmon = find_line(region_index("h4_-4"), region_index("h4_-3"));
    assert_eq!(kanmon.kind, TransportLineKind::Rail, "関門 must stay a Rail line (blockade-immune)");
    assert!(
        kanmon.capacity.get() > 20.0,
        "関門's capacity ({}) should show real evidence of a trunk-tier crossing (Sanyo Shinkansen + Kagoshima \
         Main Line both cross there), well above an ordinary Road line",
        kanmon.capacity.get()
    );

    // 津軽海峡 (Seikan): h13_21 (本州側) <-> h14_20 (北海道側).
    let seikan = find_line(region_index("h13_21"), region_index("h14_20"));
    assert_eq!(seikan.kind, TransportLineKind::Sea, "青函 must stay a Sea line (blockade-vulnerable)");

    // 瀬戸内 (Setouchi): h11_-1 (本州側) <-> h12_-2 (四国側).
    let setouchi = find_line(region_index("h11_-1"), region_index("h12_-2"));
    assert_eq!(setouchi.kind, TransportLineKind::Sea, "瀬戸内 must stay a Sea line (blockade-vulnerable)");

    // The five named major real ports: each must have a `Port` node whose
    // name identifies it (`transport_real.py`'s "hex名 港（実在の港名港）"
    // labelling - checked as a substring so this doesn't pin the whole
    // label format).
    for (region_id, real_port_name) in [
        ("h22_1", "横浜"),
        ("h16_0", "名古屋"),
        ("h12_-1", "神戸"),
        ("h4_-4", "北九州"),
        ("h14_25", "苫小牧"),
    ] {
        let rid = region_index(region_id);
        let port_node = world
            .transport_nodes
            .iter()
            .find(|n| n.region == rid && n.kind == TransportNodeKind::Port)
            .unwrap_or_else(|| panic!("region {region_id} must have a Port node"));
        assert!(
            port_node.name.contains(real_port_name),
            "region {region_id}'s Port node name ({:?}) must identify the real {real_port_name} port",
            port_node.name
        );
    }
}


// ---------------------------------------------------------------------------
// Stage 9D (docs/phase9-spec.md "4. 観測・行動・AI"): the transport network's
// own observation fields, `Action::InterdictLine`, and `Build`'s new
// `Project::TransportLine`.
// ---------------------------------------------------------------------------

/// docs/phase9-spec.md §7 Stage 9D: "観測長が... 取れ" - and, more than just
/// the *length* growing by the right count, the new per-line/per-node
/// fields must actually carry the facts the spec asks for (capacity/
/// condition/flow/usable per line, owned/blockaded per node).
///
/// Confirmed this can fail: temporarily left the new per-line/per-node
/// loops in `Observation::encode()` unimplemented while leaving
/// `encoding_len`'s formula updated - the trailing `debug_assert_eq!`
/// inside `encode()` itself failed immediately (length mismatch). Separately,
/// temporarily made `usable_by_self` always `true` regardless of ownership -
/// this test's own `b_depot<->c_depot` assertion below failed. Reverted both
/// before committing.
#[test]
fn observation_encodes_transport_network() {
    let world = scenario::load_str(MINI_VALID_SCENARIO).expect("MINI_VALID_SCENARIO must be valid");
    let f1 = world.regions[0].owner; // "a" is f1's own region
    let obs = Observation { faction: f1, world: &world };
    let encoded = obs.encode();

    let expected_len = encoding_len(
        world.regions.len(),
        world.sea_zones.len(),
        world.factions.len(),
        world.transport_lines.len(),
        world.transport_nodes.len(),
    );
    assert_eq!(encoded.len(), expected_len, "encode() must actually grow by the transport section's own length");

    let lines_offset = world.regions.len() * REGION_FIELD_COUNT
        + world.sea_zones.len() * SEA_ZONE_FIELD_COUNT
        + FACTION_FIELD_COUNT
        + world.factions.len() * DIPLOMACY_FIELD_COUNT;

    // Line 0 (declaration order - `transport_network_builds_correctly`'s own
    // doc): a_depot -> b_depot, both f1's own territory.
    let line0 = world.transport_lines[0];
    let base0 = lines_offset;
    assert_eq!(encoded[base0], line0.capacity.get(), "capacity field");
    assert_eq!(encoded[base0 + 1], line0.condition.get(), "condition field");
    assert_eq!(encoded[base0 + 3], 1.0, "a_depot<->b_depot is entirely f1's own network, so f1 must see it usable");

    // Line 1: b_depot -> c_depot, straddling f1 (b) and f2 (c) - unusable
    // for either side, per `logistics::compute_transport_flow`'s own
    // same-owner rule.
    let base1 = lines_offset + TRANSPORT_LINE_FIELD_COUNT;
    assert_eq!(encoded[base1 + 3], 0.0, "a line straddling two owners must not read usable");

    let nodes_offset = lines_offset + world.transport_lines.len() * TRANSPORT_LINE_FIELD_COUNT;
    // Node 0: a_depot, in f1's own region "a".
    assert_eq!(encoded[nodes_offset], 1.0, "a_depot's region belongs to f1");
    // Node 2: c_depot, in f2's region "c".
    assert_eq!(encoded[nodes_offset + 2 * TRANSPORT_NODE_FIELD_COUNT], 0.0, "c_depot's region does not belong to f1");
}

/// `MINI_VALID_SCENARIO` extended with a fourth region `d`, owned by `f2`
/// alongside `c` and connected only to `c` - so `c_depot<->d_depot` is a
/// transport line owned entirely by `f2`, the shape `Action::InterdictLine`
/// needs a legitimate target to test against (`b_depot<->c_depot` alone,
/// straddling both owners, can't exercise the "line is hostile but valid"
/// path at all).
fn scenario_with_enemy_owned_line() -> String {
    MINI_VALID_SCENARIO
        .replacen(
            r#"{ "id": "c", "name": "C", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [2.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" } ] }
  ],"#,
            r#"{ "id": "c", "name": "C", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [2.0, 0.0],
      "links": [ { "to": "b", "kind": "rail" }, { "to": "d", "kind": "rail" } ] },
    { "id": "d", "name": "D", "terrain": "plain", "population": 10.0,
      "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
      "infrastructure": 0.5, "port": 0.0, "position": [3.0, 0.0],
      "links": [ { "to": "c", "kind": "rail" } ] }
  ],"#,
            1,
        )
        .replacen(
            r#""nodes": [
      { "id": "a_depot", "name": "A Depot", "kind": "depot", "region": "a" },
      { "id": "b_depot", "name": "B Depot", "kind": "depot", "region": "b" },
      { "id": "c_depot", "name": "C Depot", "kind": "depot", "region": "c" }
    ],"#,
            r#""nodes": [
      { "id": "a_depot", "name": "A Depot", "kind": "depot", "region": "a" },
      { "id": "b_depot", "name": "B Depot", "kind": "depot", "region": "b" },
      { "id": "c_depot", "name": "C Depot", "kind": "depot", "region": "c" },
      { "id": "d_depot", "name": "D Depot", "kind": "depot", "region": "d" }
    ],"#,
            1,
        )
        .replacen(
            r#""lines": [
      { "from": "a_depot", "to": "b_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot", "to": "c_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 }
    ]"#,
            r#""lines": [
      { "from": "a_depot", "to": "b_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "b_depot", "to": "c_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 },
      { "from": "c_depot", "to": "d_depot", "kind": "rail", "capacity": 25.0, "condition": 1.0 }
    ]"#,
            1,
        )
        .replacen(r#""regions": ["c"]"#, r#""regions": ["c", "d"]"#, 1)
}

/// `Action::InterdictLine` (docs/phase9-spec.md "4. 行動"): rejected against
/// a line this faction has no legitimate reason to strike - its own line, a
/// line straddling two different owners, or an out-of-range id - and
/// otherwise lowers the targeted (single-owner, hostile) line's `Condition`
/// by exactly `LINE_INTERDICTION_DAMAGE`.
///
/// Confirmed this can fail: temporarily dropped the `owner_a == faction`
/// check from `apply_interdict_line` and re-ran - the "own line" assertion
/// below failed (f1 was able to interdict its own a_depot<->b_depot line).
/// Reverted before committing.
#[test]
fn interdict_line_validates_target_and_damages_condition() {
    let text = scenario_with_enemy_owned_line();
    let mut world = scenario::load_str(&text).expect("scenario_with_enemy_owned_line must be valid");
    let f1 = FactionId(0); // owns a, b
    fn find_line(world: &World, ra_name: &str, rb_name: &str) -> TransportLineId {
        let region_named =
            |name: &str| -> RegionId { RegionId(["a", "b", "c", "d"].iter().position(|n| *n == name).unwrap() as u32) };
        let (ra, rb) = (region_named(ra_name), region_named(rb_name));
        let idx = world
            .transport_lines
            .iter()
            .position(|l| {
                let (na, nb) = (world.transport_node(l.from).region, world.transport_node(l.to).region);
                (na == ra && nb == rb) || (na == rb && nb == ra)
            })
            .unwrap_or_else(|| panic!("no line between {ra_name} and {rb_name}"));
        TransportLineId(idx as u32)
    }

    // Own line (a<->b, both f1): rejected, condition untouched.
    let own_line = find_line(&world, "a", "b");
    let before = world.transport_line(own_line).condition.get();
    assert_eq!(
        action::apply_action(&mut world, f1, Action::InterdictLine { line: own_line }),
        Err(ActionError::LineNotHostile)
    );
    assert_eq!(world.transport_line(own_line).condition.get(), before);

    // Mixed-ownership line (b:f1 <-> c:f2): rejected.
    let mixed_line = find_line(&world, "b", "c");
    assert_eq!(
        action::apply_action(&mut world, f1, Action::InterdictLine { line: mixed_line }),
        Err(ActionError::LineNotHostile)
    );

    // Out-of-range line id: rejected as InvalidLine, never a panic.
    let bogus = TransportLineId(world.transport_lines.len() as u32 + 3);
    assert_eq!(
        action::apply_action(&mut world, f1, Action::InterdictLine { line: bogus }),
        Err(ActionError::InvalidLine)
    );

    // Enemy-owned line (c<->d, both f2, and f1 is at war with f2 by
    // MINI_VALID_SCENARIO's default `"blocs": []` unconditional War):
    // accepted, damages condition by exactly LINE_INTERDICTION_DAMAGE.
    let enemy_line = find_line(&world, "c", "d");
    let before_enemy = world.transport_line(enemy_line).condition.get();
    action::apply_action(&mut world, f1, Action::InterdictLine { line: enemy_line })
        .expect("a single-owner hostile line must be a valid InterdictLine target");
    let after_enemy = world.transport_line(enemy_line).condition.get();
    assert!(
        (before_enemy - after_enemy - LINE_INTERDICTION_DAMAGE).abs() < 1e-6,
        "expected condition to drop by exactly LINE_INTERDICTION_DAMAGE ({LINE_INTERDICTION_DAMAGE}), \
         went from {before_enemy} to {after_enemy}"
    );
}

/// `Action::Build`'s `Project::TransportLine` (docs/phase9-spec.md "4. 行動":
/// "`Build` の `Project` に輸送網に対するものを追加する"): rejected when the
/// hosting region isn't actually one of the line's own two endpoints, or
/// when the line isn't entirely this faction's own network - and, once
/// validly queued and fully funded to completion, raises the line's
/// `Condition` by exactly `TRANSPORT_LINE_REPAIR_STEP`.
///
/// Confirmed this can fail: temporarily dropped the endpoint-ownership check
/// from `apply_build`'s `Project::TransportLine` branch and re-ran - the
/// "mixed-ownership line" assertion below failed (f1 was able to queue an
/// investment in a line half-owned by f2). Reverted before committing.
#[test]
fn build_transport_line_project_validates_and_repairs_on_completion() {
    let mut world = scenario::load_str(MINI_VALID_SCENARIO).expect("MINI_VALID_SCENARIO must be valid");
    let f1 = FactionId(0);
    let a = RegionId(0);
    let b = RegionId(1);

    let own_line = TransportLineId(0); // a_depot -> b_depot, declared first
    let mixed_line = TransportLineId(1); // b_depot -> c_depot, declared second

    // Wrong host: `a` is f1's own region (so this isn't merely the ordinary
    // "not your region" rejection), but it isn't an endpoint of the b<->c
    // line at all.
    assert_eq!(
        action::apply_action(&mut world, f1, Action::Build { region: a, project: Project::TransportLine(mixed_line) }),
        Err(ActionError::InvalidValue)
    );

    // Mixed-ownership line: `b` *is* an endpoint, but the line's other end
    // (c) belongs to f2, so this must still be rejected.
    assert_eq!(
        action::apply_action(&mut world, f1, Action::Build { region: b, project: Project::TransportLine(mixed_line) }),
        Err(ActionError::LineNotOwned)
    );

    // Out-of-range line id.
    let bogus = TransportLineId(world.transport_lines.len() as u32 + 3);
    assert_eq!(
        action::apply_action(&mut world, f1, Action::Build { region: a, project: Project::TransportLine(bogus) }),
        Err(ActionError::InvalidLine)
    );

    // Valid: damage the line, queue the repair project at `a`, fund it to
    // completion, and check the completion effect.
    world.transport_lines[own_line.index()].condition = Condition::new(0.4).unwrap();
    action::apply_action(&mut world, f1, Action::Build { region: a, project: Project::TransportLine(own_line) })
        .expect("a's own line, hosted at a's own endpoint, must be a valid Build target");
    assert_eq!(world.region(a).construction.map(|c| c.project), Some(Project::TransportLine(own_line)));

    world.faction_mut(f1).stock[Good::Machinery.index()] = 1e6;
    world.faction_mut(f1).stock[Good::Steel.index()] = 1e6;
    for _ in 0..200 {
        if world.region(a).construction.is_none() {
            break;
        }
        construction::tick_construction(&mut world);
    }
    assert!(world.region(a).construction.is_none(), "a fully-funded project must complete within 200 ticks");
    let after = world.transport_lines[own_line.index()].condition.get();
    assert!(
        (after - (0.4 + TRANSPORT_LINE_REPAIR_STEP)).abs() < 1e-4,
        "completion must raise condition by exactly TRANSPORT_LINE_REPAIR_STEP (0.4 + {TRANSPORT_LINE_REPAIR_STEP} = {}), got {after}",
        0.4 + TRANSPORT_LINE_REPAIR_STEP
    );
}

/// A transport-line repair validated at order time must not still be applied
/// once the line stopped qualifying (`codex review`, P2). `apply_build`
/// checks that both of the line's endpoint regions belong to the ordering
/// faction, but that check ran when the order was issued; the front can move
/// underneath a project that takes many ticks to fund. Without revalidation
/// at completion, capturing the host region hands the captor a free repair of
/// a line it does not own - and more generally it is CLAUDE.md's 「発令時点の
/// 値を焼き込まない」: a condition that must hold *now* was sampled once.
///
/// Cancelling (rather than stalling) is what keeps this from creating a
/// different listed defect: a project that could neither complete nor be
/// cleared would be a state with no exit, so the region's `construction`
/// slot is released and it is free to order something legal.
///
/// **Confirmed this test can fail.** Removing `tick_construction`'s
/// `transport_line_still_owned` guard makes it repair the now-foreign line
/// anyway: `condition` went 0.4 -> 0.55 and `construction` reported
/// `None` by completion rather than by cancellation, tripping both
/// assertions below. Restored, and it passes.
#[test]
fn transport_line_repair_is_abandoned_when_the_line_stops_qualifying() {
    let mut world = scenario::load_str(MINI_VALID_SCENARIO).expect("MINI_VALID_SCENARIO must be valid");
    let f1 = FactionId(0);
    let f2 = FactionId(1);
    let a = RegionId(0);
    let b = RegionId(1);
    let own_line = TransportLineId(0); // a_depot <-> b_depot, both f1's at load time

    world.transport_lines[own_line.index()].condition = Condition::new(0.4).unwrap();
    action::apply_action(&mut world, f1, Action::Build { region: a, project: Project::TransportLine(own_line) })
        .expect("both endpoints are f1's at order time, so the order is legal");
    assert_eq!(world.region(a).construction.map(|c| c.project), Some(Project::TransportLine(own_line)));

    // The far endpoint changes hands while the project is still in progress.
    world.regions[b.index()].owner = f2;

    world.faction_mut(f1).stock[Good::Machinery.index()] = 1e6;
    world.faction_mut(f1).stock[Good::Steel.index()] = 1e6;
    for _ in 0..200 {
        if world.region(a).construction.is_none() {
            break;
        }
        construction::tick_construction(&mut world);
    }

    assert!(
        world.region(a).construction.is_none(),
        "the stale project must be cleared, not left occupying the region's construction slot forever"
    );
    let after = world.transport_lines[own_line.index()].condition.get();
    assert!(
        (after - 0.4).abs() < 1e-4,
        "a line whose far endpoint was captured mid-construction must not be repaired by the order that \
         was legal only before the capture: condition should still be 0.4, got {after}"
    );
}

/// Defect fix (post-Stage-9B): `distribute_supply`'s/`land_unit_supply_avail`'s
/// "non-owner" branch used to guess at an occupier's supply with
/// `0.4 * world.supply[some same-owner neighbor]`, a formula that reads
/// near-zero whenever that neighbor happens to have no units of its own to
/// draw the figure up - regardless of how much the network could actually
/// carry through it (this module's own top-of-file doc has the full
/// account). Fixed by routing an occupier's demand into
/// `compute_transport_flow` itself, through its own faction's network to
/// the frontier and across into wherever it actually stands, so it is a
/// real flow candidate rather than a downstream guess.
///
/// mvp's `kanto` (region 3, touhou_rengou's own, highly industrial) shares a
/// direct `TransportLine` (`kanto_depot -> shinetsu_hokuriku_depot`) with
/// `shinetsu_hokuriku` (region 4, owned by chuo_domei) - the only two
/// regions in the whole map connected by a line crossing a faction
/// boundary. Standing a lone touhou_rengou unit in `shinetsu_hokuriku`
/// (occupying, not owning, it) is exactly the scenario the defect
/// description names: "adjacent to a healthy, well-supplied friendly
/// network."
fn station_foreign_unit(world: &mut World, owner: FactionId, region: RegionId) -> UnitId {
    let id = UnitId(world.units.len() as u32);
    world.units.push(military::Unit {
        id,
        owner,
        name: "Occupier".to_string(),
        station: Station::Region(region),
        movement: None,
        manpower: 1.0,
        equipment: UNIT_EQUIPMENT,
        organization: 100.0,
        morale: 1.0,
        supply: 0.0,
        arms_delivery: 0.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(region),
        experience: 0.0,
        alive: true,
    });
    id
}

/// **Confirmed this can fail**: reverting `distribute_supply`'s non-owner
/// `avail` branch to the pre-fix `world.regions[r].links.iter()...find a
/// same-owner, uncontested neighbor... * PROJECTED_SUPPLY_FACTOR` formula
/// (and `land_unit_supply_avail`'s identical branch) while leaving
/// everything else in place reproduces the exact failure this test is
/// built to catch: `shinetsu_hokuriku` has no `Region::links` neighbor
/// touhou_rengou owns at all (mvp's region graph, unlike the transport
/// graph, has no edge between `shinetsu_hokuriku` and `kanto`), so the old
/// formula's `.fold(0.0f32, f32::max)` over an empty iterator returns
/// `0.0` - `avail_occupier` came back exactly `0.0` and this test's first
/// assertion failed with "an occupier standing next to a healthy friendly
/// network must actually receive supply, got 0". Restored before
/// committing.
/// `codex review` (P2): `usable_by_self` in the observation must follow the
/// *same* rule the flow model does (`TransportGraph::line_eligible_for`,
/// which keys off `controlled` — ownership **or** live units present), not a
/// second ownership-only rule that drifts from it. A faction occupying both
/// endpoints really does haul supply over the line after the Stage 9D
/// occupier fix, so reporting it unusable would teach an RL or API consumer
/// the opposite of what the simulator does.
///
/// **Confirmed this test can fail.** Reverting the flag to the plain
/// `owner == self.faction` test on both endpoints makes the occupier read
/// `0.0` for a line it is actively hauling over, tripping the second
/// assertion. Restored, and it passes.
#[test]
fn transport_line_usable_flag_follows_the_flow_models_own_rule() {
    let mut world = scenario::load_str(MINI_VALID_SCENARIO).expect("MINI_VALID_SCENARIO must be valid");
    let f2 = FactionId(1);
    let line = &world.transport_lines[0];
    let ra = world.transport_node(line.from).region;
    let rb = world.transport_node(line.to).region;
    assert_eq!(world.region(ra).owner, world.region(rb).owner, "line 0 must start wholly owned by one faction");
    assert_ne!(world.region(ra).owner, f2, "and that faction must not already be f2");

    let usable_idx = world.regions.len() * REGION_FIELD_COUNT
        + world.sea_zones.len() * SEA_ZONE_FIELD_COUNT
        + FACTION_FIELD_COUNT
        + world.factions.len() * DIPLOMACY_FIELD_COUNT
        + 3; // line 0's `usable_by_self` field

    let before = Observation { faction: f2, world: &world }.encode();
    assert_eq!(before[usable_idx], 0.0, "f2 neither owns nor occupies either endpoint yet");

    // f2 physically occupies both endpoints without owning them - exactly
    // the case `compute_transport_flow`'s `controlled` matrix admits.
    station_foreign_unit(&mut world, f2, ra);
    station_foreign_unit(&mut world, f2, rb);

    let after = Observation { faction: f2, world: &world }.encode();
    assert_eq!(
        after[usable_idx], 1.0,
        "a faction occupying both endpoints hauls supply over this line, so the observation must say so"
    );
}

#[test]
fn occupier_adjacent_to_healthy_network_gets_supplied() {
    let kanto = RegionId(3);
    let shinetsu_hokuriku = RegionId(4);
    let touhou_rengou = FactionId(0);
    let chuo_domei = FactionId(1);
    assert_eq!(scenario::build_world().region(shinetsu_hokuriku).owner, chuo_domei, "test setup: shinetsu_hokuriku must be chuo_domei's own territory");
    assert_eq!(scenario::build_world().region(kanto).owner, touhou_rengou, "test setup: kanto must be touhou_rengou's own territory");

    let mut world = scenario::build_world();
    // Give chuo_domei's own units nothing to draw shinetsu_hokuriku's line
    // capacity down with, isolating "does the occupier get served at all"
    // from "how does it split against the owner's own demand" (a separate,
    // already-covered property - `oversubscribed_allocation_is_proportional_
    // and_order_independent`).
    world.units.retain(|u| u.owner != chuo_domei);
    let occupier = station_foreign_unit(&mut world, touhou_rengou, shinetsu_hokuriku);

    logistics::recompute_supply(&mut world);
    let avail_occupier = world.supply_by_faction[shinetsu_hokuriku.index()][touhou_rengou.index()];
    // `compute_transport_flow`'s `served` is demand-*bounded* (Stage 9B's
    // whole point - a delivered amount, never a ceiling exceeding what the
    // unit could use), so a lone unit's own demand (~1.0, `SUPPLY_NEED_PER_
    // MANPOWER * UNIT_MANPOWER`) is also this candidate's own ceiling here.
    // The property under test is that kanto's ample production actually
    // clears essentially all of that demand through the direct line, not
    // the pre-fix formula's near-zero regardless of capacity.
    assert!(
        avail_occupier > 0.9,
        "an occupier standing next to a healthy friendly network must actually receive nearly all its own demand, \
         got {avail_occupier} (kanto's own production should easily clear a lone unit's demand through the direct \
         kanto<->shinetsu_hokuriku line)"
    );
    assert_eq!(
        logistics::land_unit_supply_avail(&world, occupier),
        avail_occupier,
        "land_unit_supply_avail must read the exact same (region, faction) figure distribute_supply's own avail array does"
    );

    // The full pass actually raises the occupier's own `unit.supply`, not
    // just the region-level `avail` figure - the property that ultimately
    // matters for combat power (docs/mvp-spec.md's own supply->combat_power
    // chain).
    for _ in 0..30 {
        logistics::distribute_supply(&mut world);
    }
    assert!(
        world.unit(occupier).supply > 0.5,
        "30 ticks of a healthy, uncontested supply line should ease the occupier's own supply ratio well above \
         zero, got {}",
        world.unit(occupier).supply
    );
}

/// The other half of the same property: a unit genuinely beyond every
/// route its own faction's network could ever reach must read `0.0`,
/// because the flow search never finds a path there - not because of a
/// projection formula guessing wrong. `kyushu` (region 9, seihou_domei's
/// own) shares no `TransportLine` with anything touhou_rengou owns or
/// could ever reach without crossing chuo_domei's or seihou_domei's own
/// territory (which `compute_transport_flow`'s eligibility rule forbids -
/// a line is only usable by a hauling faction when *both* its ends are
/// that faction's own or physically occupied by it).
///
/// **Confirmed this can fail**: with the pre-fix formula restored (as
/// above), this assertion is *not* what catches the defect - the old
/// formula also reads `0.0` here (there being no `Region::links` neighbor
/// either) for the wrong reason, which is exactly why this test also pins
/// the healthy case above: a fix that made this one pass by, say, defaulting
/// unreachable demand to `0.0` while leaving the healthy case broken would
/// slip through this test alone.
#[test]
fn occupier_with_no_route_home_gets_nothing() {
    let kyushu = RegionId(9);
    let touhou_rengou = FactionId(0);
    let seihou_domei = FactionId(2);
    assert_eq!(scenario::build_world().region(kyushu).owner, seihou_domei, "test setup: kyushu must be seihou_domei's own territory");

    let mut world = scenario::build_world();
    let occupier = station_foreign_unit(&mut world, touhou_rengou, kyushu);

    logistics::recompute_supply(&mut world);
    let avail_occupier = world.supply_by_faction[kyushu.index()][touhou_rengou.index()];
    assert_eq!(
        avail_occupier, 0.0,
        "a unit with no possible route back to its own faction's network must read exactly 0.0, not a nonzero \
         guess: got {avail_occupier}"
    );
    assert_eq!(logistics::land_unit_supply_avail(&world, occupier), 0.0);
}
