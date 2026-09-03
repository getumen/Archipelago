//! Regression tests for `HeuristicAgent` (see the A1 fix in offensive()).

use archipelago_sim::action::{self, Action};
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{
    FOCUS_MARITIME_IMPORT_CAPACITY_MULT, IMPORT_COST_MACHINERY_PER_GOOD, UNIT_EQUIPMENT,
    UNIT_MANPOWER, UNIT_ORG,
};
use archipelago_sim::diplomacy::{Stance, Treaty};
use archipelago_sim::focus::{self, NationalFocus};
use archipelago_sim::good::{Good, GOOD_COUNT};
use archipelago_sim::ids::{FactionId, RegionId, UnitId};
use archipelago_sim::military::{move_required, Movement, Unit};
use archipelago_sim::observation::Observation;
use archipelago_sim::scenario;
use archipelago_sim::trade;
use archipelago_sim::world::Station;

use crate::HeuristicAgent;

/// A unit already under way toward a destination must not be re-issued a
/// `MoveUnit` toward that same destination - doing so resets
/// `Movement::progress` to zero, and since the agent re-plans every 4 days
/// while a hostile strait/tunnel crossing can take longer than that, the
/// attack would never land. A second, previously-idle unit at the same
/// region should still be sent if the offensive's garrison quota leaves
/// room, and a third (freshly added) reserve should stay home once that
/// quota is used up - proving the already-en-route unit correctly consumes
/// part of the quota instead of being ignored entirely.
#[test]
fn moving_unit_is_not_reissued_toward_same_destination() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    // Faction 0's capital (region 3) starts with two units and borders two
    // enemy regions (4 and 5), both already garrisoned by faction 1's
    // starting units. Region 5 ("東海") is by far the higher-value target
    // (much more industry/population), so that's what the offensive
    // heuristic picks - a defended target, so the garrison rule keeps one
    // unit behind out of every two sent.
    let region = RegionId(3);
    let target = RegionId(5);
    let region3_units: Vec<UnitId> = world
        .units
        .iter()
        .filter(|u| u.owner == faction && u.station == Station::Region(region))
        .map(|u| u.id)
        .collect();
    assert_eq!(region3_units.len(), 2, "expected two faction-0 units at the capital");
    let moving_unit_id = region3_units[0];
    let already_idle_unit_id = region3_units[1];

    let link = world.link_between(region, target).unwrap();
    let required = move_required(link.kind, world.region(target).terrain, true);
    world.units[moving_unit_id.index()].movement = Some(Movement {
        from: Station::Region(region),
        to: Station::Region(target),
        progress: required * 0.5,
        required,
        retreat: false,
        strait_zone: None,
    });

    // Add a third faction-0 unit at region 3, idle, so there are three
    // units present: one already en route to `target`, one idle, one
    // freshly added idle. Garrison quota = 3 - 1 = 2; one slot is already
    // filled by the en-route unit, leaving exactly one fresh order to hand
    // out.
    let reserve_unit_id = UnitId(world.units.len() as u32);
    world.units.push(Unit {
        id: reserve_unit_id,
        owner: faction,
        name: "Test Reserve Corps".to_string(),
        station: Station::Region(region),
        movement: None,
        manpower: UNIT_MANPOWER,
        equipment: UNIT_EQUIPMENT,
        organization: UNIT_ORG,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(region),
        experience: 0.0,
        alive: true,
    });

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    let move_actions: Vec<(UnitId, Station)> = actions
        .iter()
        .filter_map(|a| match a {
            Action::MoveUnit { unit, to } => Some((*unit, *to)),
            _ => None,
        })
        .collect();
    let target = Station::Region(target);

    assert!(
        !move_actions.contains(&(moving_unit_id, target)),
        "a unit already moving toward its destination must not receive a fresh MoveUnit for it: {move_actions:?}"
    );
    assert!(
        move_actions.contains(&(already_idle_unit_id, target)),
        "the one remaining garrison slot should go to the unit that was already idle: {move_actions:?}"
    );
    assert!(
        !move_actions.contains(&(reserve_unit_id, target)),
        "with the quota already filled, the freshly added reserve unit should stay home: {move_actions:?}"
    );
}

/// External code review fix (Stage 2C): `set_trade_policy` used to scale
/// *both* the Food and the Energy import request off the aggregate
/// `Faction::shortage` (the worst of Food/Energy/Machinery), so a faction
/// that was only short on Food would still request a full-scale Energy
/// import too - the two plans then compete for the same port capacity and
/// the same Machinery payment, crowding out the import that's actually
/// needed. With Food short and Energy fully stocked, the requested Energy
/// rate must be ~0 and Food must get a real request.
#[test]
fn import_plan_targets_the_deficient_commodity() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    {
        let f = world.faction_mut(faction);
        // Aggregate shortage stays nonzero (as it would from Food alone),
        // but only Food is actually short - Energy is fully served.
        f.shortage = 0.6;
        f.shortage_by_good = [0.0; GOOD_COUNT];
        f.shortage_by_good[Good::Food.index()] = 0.6;
        f.shortage_by_good[Good::Energy.index()] = 0.0;
        // Plenty of Machinery on hand so the low-Machinery throttle
        // (`IMPORT_MACHINERY_LOW_DAYS`) doesn't suppress the request and
        // mask the effect under test.
        f.stock[Good::Machinery.index()] = 10_000.0;
    }

    let obs = Observation { faction, world: &world };
    let mut actions = Vec::new();
    crate::set_trade_policy(faction, &obs, &mut actions);

    let food_rate = actions
        .iter()
        .find_map(|a| match a {
            Action::SetImportPlan { good: Good::Food, rate } => Some(*rate),
            _ => None,
        })
        .expect("expected a Food import plan action");
    let energy_rate = actions
        .iter()
        .find_map(|a| match a {
            Action::SetImportPlan { good: Good::Energy, rate } => Some(*rate),
            _ => None,
        })
        .expect("expected an Energy import plan action");

    assert!(
        energy_rate < 0.01,
        "Energy is fully stocked and should not be requested just because Food is short: {energy_rate}"
    );
    assert!(
        food_rate > 1.0,
        "Food is short and should get a real import request in place of the crowded-out Energy demand: {food_rate}"
    );
}

/// External code review fix P1 (`major_change_focus`): with both reactive
/// triggers (a permanent territorial collapse *and* a permanent alliance)
/// true for the whole run, the pre-fix level-triggered checks made the
/// faction perpetually alternate between `DefensivePosture` and
/// `AllianceNetwork` - every ~`FOCUS_SWITCH_DAYS` cycle producing another
/// switch, so the faction spent almost the entire run stuck in
/// `focus::active() == None`'s transition blackout rather than benefiting
/// from either focus. Drives `national_focus_ai` directly (bypassing
/// `HeuristicAgent::decide`'s 4-day cadence so a long run covers many
/// `FOCUS_SWITCH_DAYS` transitions quickly) across 400 days with both
/// conditions wired to stay true throughout, and asserts the agent settles
/// after at most the opening pick plus one reactive switch, spending the
/// large majority of the run with a focus actually `active()`.
#[test]
fn focus_does_not_oscillate_under_persistent_conditions() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let other = FactionId(1);

    // Territory collapse, permanently: faction 0 starts with 4 core regions
    // (0, 1, 2, 3, per `scenario::FACTION_SPECS`) - hand two of them to
    // faction 1 so owned (2) stays <= core (4) * MAJOR_TERRITORY_LOSS_FRACTION
    // (0.5) for the rest of the run (`Region::core` itself never changes).
    world.regions[2].owner = other;
    world.regions[3].owner = other;

    // Alliance, permanently: sign it directly through the action layer (no
    // stance precondition on `Treaty::Alliance` beyond "not already active,
    // no cooldown" - see `action::apply_propose_treaty`) so
    // `Stance::Alliance` holds between faction 0 and 1 for the whole run.
    action::apply_action(&mut world, faction, Action::ProposeTreaty { to: other, treaty: Treaty::Alliance })
        .unwrap();
    action::apply_action(&mut world, other, Action::AcceptTreaty { from: faction, treaty: Treaty::Alliance })
        .unwrap();
    assert_eq!(world.diplomacy.stance(faction, other), Stance::Alliance, "sanity: alliance must actually be active");

    let mut initialized = false;
    let mut switches = 0u32;
    let mut days_active = 0u32;
    const TOTAL_DAYS: u32 = 400; // 20 FOCUS_SWITCH_DAYS(20)-cycles' worth.

    for day in 0..TOTAL_DAYS {
        world.day = day;
        let mut actions = Vec::new();
        {
            let obs = Observation { faction, world: &world };
            crate::national_focus_ai(faction, &mut initialized, &obs, &mut actions);
        }
        for a in &actions {
            if let Action::SetNationalFocus(focus) = *a {
                switches += 1;
                action::apply_action(&mut world, faction, Action::SetNationalFocus(focus))
                    .expect("major_change_focus must never emit a switch the simulation rejects");
            }
        }
        focus::tick_national_focus(&mut world);
        if focus::active(world.faction(faction)).is_some() {
            days_active += 1;
        }
    }

    assert!(
        switches <= 2,
        "expected the opening pick plus at most one reactive switch under permanently-true \
         conditions, got {switches} SetNationalFocus actions - the reactive triggers are still \
         oscillating"
    );
    assert!(
        days_active * 2 > TOTAL_DAYS,
        "expected the agent to spend most of the run with a focus actually active, not stuck \
         oscillating in transition: {days_active}/{TOTAL_DAYS} days active"
    );
}

/// External code review fix P2 (`agents::own_port_capacity`): Stage 3C added
/// `NationalFocus::MaritimeTrade`'s `FOCUS_MARITIME_IMPORT_CAPACITY_MULT` to
/// `trade::tick_imports`'s own per-port capacity figure, but the agent-side
/// mirror wasn't updated alongside it. Sets up a faction with `MaritimeTrade`
/// active and a Food import request far beyond what its ports can carry (so
/// the import is capacity-bound, not demand- or Machinery-bound), runs
/// `trade::tick_imports` for real, and asserts the Food actually delivered
/// equals exactly what `own_port_capacity` reports - proving the agent's
/// estimate and the simulation's own grant agree, multiplier included.
#[test]
fn agent_port_capacity_matches_simulation() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    // Stage 3C `NationalFocus::MaritimeTrade`, already active (no transition
    // in progress) - matches `focus::active`'s contract directly rather than
    // waiting out `FOCUS_SWITCH_DAYS` through the action layer.
    {
        let f = world.faction_mut(faction);
        f.national_focus = NationalFocus::MaritimeTrade;
        f.focus_transition_days = 0;
    }
    assert_eq!(focus::active(world.faction(faction)), Some(NationalFocus::MaritimeTrade), "sanity");

    let expected_capacity = crate::own_port_capacity(faction, &world);
    assert!(expected_capacity > 0.0, "sanity: faction 0 must hold at least one usable port");
    // Sanity: the multiplier must actually be doing something here, or this
    // test can't tell a fixed mirror from a still-broken one.
    let capacity_without_focus = {
        let mut plain = world.clone();
        plain.faction_mut(faction).national_focus = NationalFocus::AllianceNetwork;
        crate::own_port_capacity(faction, &plain)
    };
    assert!(
        (expected_capacity - capacity_without_focus * FOCUS_MARITIME_IMPORT_CAPACITY_MULT).abs() < 1e-4,
        "expected_capacity should reflect FOCUS_MARITIME_IMPORT_CAPACITY_MULT over the unfocused figure"
    );

    // A Food request far beyond any plausible port capacity, and enough
    // Machinery on hand that the world-market Machinery-affordability cap
    // (`IMPORT_COST_MACHINERY_PER_GOOD`) never binds instead - so the run is
    // unambiguously capacity-bound, the only case that isolates the port
    // figure itself.
    {
        let f = world.faction_mut(faction);
        f.import_plan[Good::Food.index()] = 1_000_000.0;
        f.import_plan[Good::Energy.index()] = 0.0;
        f.stock[Good::Machinery.index()] = 1_000_000.0 * IMPORT_COST_MACHINERY_PER_GOOD;
        f.shortage_by_good = [0.0; GOOD_COUNT]; // no TradeAgreement flows to add noise.
    }
    let food_before = world.faction(faction).stock[Good::Food.index()];

    trade::tick_imports(&mut world);

    let food_after = world.faction(faction).stock[Good::Food.index()];
    let actual_import = food_after - food_before;

    assert!(
        (actual_import - expected_capacity).abs() < 1e-3,
        "trade::tick_imports granted {actual_import} but agents::own_port_capacity estimated \
         {expected_capacity} - the agent-side mirror has drifted from the simulation"
    );
}
