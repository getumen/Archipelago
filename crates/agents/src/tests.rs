//! Regression tests for `HeuristicAgent` (see the A1 fix in offensive()).

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG};
use archipelago_sim::ids::{FactionId, RegionId, UnitId};
use archipelago_sim::military::{move_required, Movement, Unit};
use archipelago_sim::observation::Observation;
use archipelago_sim::scenario;

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
        .filter(|u| u.owner == faction && u.location == region)
        .map(|u| u.id)
        .collect();
    assert_eq!(region3_units.len(), 2, "expected two faction-0 units at the capital");
    let moving_unit_id = region3_units[0];
    let already_idle_unit_id = region3_units[1];

    let link = world.link_between(region, target).unwrap();
    let required = move_required(link.kind, world.region(target).terrain, true);
    world.units[moving_unit_id.index()].movement = Some(Movement {
        from: region,
        to: target,
        progress: required * 0.5,
        required,
        retreat: false,
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
        location: region,
        movement: None,
        manpower: UNIT_MANPOWER,
        equipment: UNIT_EQUIPMENT,
        organization: UNIT_ORG,
        morale: 1.0,
        supply: 1.0,
        experience: 0.0,
        alive: true,
    });

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    let move_actions: Vec<(UnitId, RegionId)> = actions
        .iter()
        .filter_map(|a| match a {
            Action::MoveUnit { unit, to } => Some((*unit, *to)),
            _ => None,
        })
        .collect();

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
