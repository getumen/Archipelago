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

use crate::{cannot_interpret_nl, default_heuristic_agent, HeuristicAgent, DEFAULT_CAUTION};

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

/// External code review fix B2 (docs/phase4-spec.md "Stage 4B — 自然言語外
/// 交": "受け手が HeuristicAgent なら..."): replaces the old
/// `natural_language_maps_to_terms`, which asserted that the old
/// `keyword_interpret` mapped design.md §12's worked example - "新潟方面か
/// ら撤兵する代わりに、港湾利用権を認めてほしい" ("withdraw from the Niigata
/// front in exchange for recognizing our port access") - to specific
/// `TreatyTerm`s. `keyword_interpret` is gone (docs/conventions.md §3: a
/// `HeuristicAgent` cannot actually read Japanese/English prose, so
/// approximating that via keyword matching was itself the fallback the
/// project owner rejected); `natural_language_maps_to_terms` described
/// exactly that removed behaviour and no longer describes anything real.
/// This test asserts the new, honest contract instead: `cannot_interpret_nl`
/// answers every proposal with no terms and a flat reject, *even* text that
/// would have hit every one of the old parser's keywords (the region name,
/// "撤兵", and "港湾利用" all appear below) - proving the replacement
/// doesn't quietly still do partial keyword matching under a new name.
#[test]
fn natural_language_proposal_is_never_interpreted_by_a_heuristic_agent() {
    let world = scenario::build_world();
    let proposer = FactionId(0);
    let recipient = FactionId(1);
    let obs = Observation { faction: recipient, world: &world };

    let text = "信越・北陸方面から撤兵する代わりに、港湾利用権を認めてほしい";
    let (terms, accept) = cannot_interpret_nl(&obs, proposer, text);

    assert!(terms.is_empty(), "a HeuristicAgent must extract no terms from any text, however keyword-rich");
    assert!(!accept, "a HeuristicAgent must decline every natural-language proposal outright");
}

/// `unparseable_proposal_is_rejected` (docs/phase4-spec.md "Stage 4B の受け
/// 入れ基準"): gibberish text yields no terms and a reject verdict under the
/// new `cannot_interpret_nl` contract exactly as it did under the old
/// keyword fallback (this was already the keyword parser's own behaviour
/// for unrecognized text - now it's *every* text's behaviour, not just
/// unrecognized text's), and running that verdict through the real action
/// pipeline leaves diplomacy untouched.
#[test]
fn unparseable_proposal_is_rejected() {
    let mut world = scenario::build_world();
    let proposer = FactionId(0);
    let recipient = FactionId(1);

    action::apply_action(&mut world, proposer, Action::ProposeInNaturalLanguage {
        to: recipient,
        text: "the weather today is quite pleasant, wouldn't you say".to_string(),
    })
    .unwrap();

    let (terms, accept) = {
        let obs = Observation { faction: recipient, world: &world };
        cannot_interpret_nl(&obs, proposer, "the weather today is quite pleasant, wouldn't you say")
    };
    assert!(terms.is_empty(), "gibberish text should yield no recognizable terms");
    assert!(!accept, "an unparseable proposal must be rejected outright, not accepted with zero terms");

    let stance_before = world.diplomacy.stance(proposer, recipient);
    let port_access_before = world.diplomacy.has_port_access(proposer, recipient);
    action::apply_action(&mut world, recipient, Action::RespondToNaturalLanguageProposal {
        from: proposer,
        terms,
        accept,
    })
    .unwrap();

    assert_eq!(world.diplomacy.stance(proposer, recipient), stance_before, "no treaty should have formed");
    assert_eq!(world.diplomacy.has_port_access(proposer, recipient), port_access_before);
}

/// External code review fix B2: replaces the old
/// `natural_language_worked_example_end_to_end`, which drove design.md §12's
/// worked example through the *old* `keyword_interpret`-backed pipeline and
/// asserted the deal was understood and accepted. Under the new contract a
/// `HeuristicAgent` recipient cannot interpret the proposal at all - not
/// even this friendly, textbook-clean one - so the honest end-to-end
/// outcome is a flat rejection with the world left exactly as it was, not a
/// negotiated `Withdraw`+`PortAccess` deal. `natural_language_maps_to_terms`
/// covers the interpreter's own return value in isolation; this test proves
/// that same "no" carries all the way through `HeuristicAgent::decide` and
/// `Simulation::apply` without anything downstream quietly still granting
/// the deal.
#[test]
fn natural_language_proposal_is_rejected_end_to_end() {
    let mut world = scenario::build_world();
    let proposer = FactionId(0); // 東方連合
    let recipient = FactionId(1); // 中央同盟 - region 4's core owner
    let region = RegionId(4); // 信越・北陸

    // Faction 0 captured region 4 from faction 1 earlier in the war, and the
    // two have since settled into a ceasefire - the same friendly, feasible
    // setup the old keyword-fallback test used, so this test isolates "a
    // HeuristicAgent can't interpret language" as the *only* reason the
    // deal doesn't go through, not some unrelated reason it would have
    // failed anyway (a live war, an infeasible withdrawal, ...).
    world.region_mut(region).owner = proposer;
    assert_eq!(world.region(region).core, recipient, "region 4 must still read as faction 1's own soil");

    action::apply_action(&mut world, proposer, Action::ProposeTreaty { to: recipient, treaty: Treaty::Ceasefire })
        .unwrap();
    action::apply_action(&mut world, recipient, Action::AcceptTreaty { from: proposer, treaty: Treaty::Ceasefire })
        .unwrap();

    let text = "信越・北陸方面から撤兵する代わりに、港湾利用権を認めてほしい";
    action::apply_action(&mut world, proposer, Action::ProposeInNaturalLanguage {
        to: recipient,
        text: text.to_string(),
    })
    .unwrap();

    // Faction 1's own HeuristicAgent has no LLM to reach for, so it answers
    // with `cannot_interpret_nl`'s honest "no" (`HeuristicAgent::decide`'s
    // own doc) rather than design.md §12's "AI勢力が条件を評価して返答す
    // る" - which describes an *LLM*-backed recipient's behaviour, not a
    // bare heuristic one's.
    let mut agent = HeuristicAgent::new(recipient, 1.15);
    let respond_action = {
        let obs = Observation { faction: recipient, world: &world };
        agent
            .decide(&obs)
            .into_iter()
            .find(|a| matches!(a, Action::RespondToNaturalLanguageProposal { from, .. } if *from == proposer))
            .expect("expected faction 1 to answer the natural-language proposal")
    };

    let Action::RespondToNaturalLanguageProposal { terms, accept, .. } = respond_action.clone() else {
        unreachable!()
    };
    assert!(terms.is_empty(), "a HeuristicAgent must extract no terms, even from this friendly, feasible deal");
    assert!(!accept, "a HeuristicAgent must decline the proposal outright, not accept it with zero terms");

    let stance_before = world.diplomacy.stance(proposer, recipient);
    let port_access_before = world.diplomacy.has_port_access(proposer, recipient);
    action::apply_action(&mut world, recipient, respond_action).unwrap();

    assert_eq!(
        world.region(region).owner, proposer,
        "with nothing accepted, region 4 must stay exactly where it was - no silent withdrawal"
    );
    assert_eq!(world.diplomacy.stance(proposer, recipient), stance_before, "no treaty should have formed");
    assert_eq!(
        world.diplomacy.has_port_access(proposer, recipient), port_access_before,
        "the never-extracted Sign(PortAccess) term must never take effect"
    );
}

/// External code review fix B5: `default_heuristic_agent` used to hand any
/// faction index past `DEFAULT_CAUTION`/`DEFAULT_PEACE_DISPOSITION`'s 8
/// entries a made-up `(1.25, 1.0)` personality instead of a real one. A
/// scenario with more factions than the table covers must fail loudly
/// instead - checked here at exactly the boundary (index 7 still works,
/// index 8 - one past the table - panics with a message naming the mismatch,
/// not an out-of-bounds index panic from plain indexing).
#[test]
fn default_heuristic_agent_covers_every_table_entry() {
    for i in 0..DEFAULT_CAUTION.len() {
        let agent = default_heuristic_agent(i);
        assert_eq!(agent.faction(), FactionId(i as u32));
    }
}

#[test]
#[should_panic(expected = "no default AI personality")]
fn default_heuristic_agent_beyond_the_table_fails_loudly() {
    let _ = default_heuristic_agent(DEFAULT_CAUTION.len());
}

/// Pushes `count` freshly-built, full-strength land units for `faction`,
/// idle at `region` - the same shape `moving_unit_is_not_reissued_toward_
/// same_destination`'s own "reserve" unit above already uses, factored out
/// since the disband-policy tests below need several at once.
fn push_idle_land_units(world: &mut archipelago_sim::world::World, faction: FactionId, region: RegionId, count: usize) -> Vec<UnitId> {
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = UnitId(world.units.len() as u32);
        world.units.push(Unit {
            id,
            owner: faction,
            name: format!("Test Corps {i}"),
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
        ids.push(id);
    }
    ids
}

/// The disband-defect fix's AI half: a faction whose land force has grown
/// well past what its industry (`unit_cap`) can sustain must stand the
/// excess down on its own, exactly like a human player would via the unit
/// panel's new button - this is the regression guard for
/// `crate::disband_excess` actually being wired into `decide_for_llm`.
/// Checked this fails when broken: temporarily removed the
/// `disband_excess(...)` call from `decide_for_llm` - `disbands.len()`
/// then comes back `0` and the first assertion fails.
#[test]
fn heuristic_agent_disbands_when_over_extended() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    // Zero this faction's industry so `unit_cap` sits at its bare 3.0
    // floor, regardless of mvp's own starting capacity numbers - the exact
    // shape of 近畿府 on japan_hex: territory (and industry) lost after an
    // army was already raised for a bigger economy.
    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
        }
    }

    // mvp's faction 0 starts with 3 units; + 6 fresh ones = 9, well past
    // `unit_cap` (3.0) * `DISBAND_UNIT_CAP_MARGIN` (1.25) = 3.75.
    push_idle_land_units(&mut world, faction, capital, 6);
    let total_before: usize = world.units.iter().filter(|u| u.owner == faction && u.alive).count();
    assert_eq!(total_before, 9, "test setup: expected 3 starting + 6 fresh units");

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    let disbands: Vec<UnitId> = actions
        .iter()
        .filter_map(|a| match a {
            Action::DisbandUnit { unit } => Some(*unit),
            _ => None,
        })
        .collect();

    assert_eq!(
        disbands.len(),
        6,
        "expected the force trimmed from 9 down to unit_cap's floor of 3 (6 disbands): {actions:?}"
    );
    let unique: std::collections::BTreeSet<UnitId> = disbands.iter().copied().collect();
    assert_eq!(unique.len(), disbands.len(), "must never disband the same unit twice: {disbands:?}");
    for &u in &disbands {
        assert_eq!(world.unit(u).owner, faction, "must only disband this faction's own units");
    }
}

/// A faction whose force is within (or only marginally past) `unit_cap`
/// must never disband anything - this is not a policy that fires on every
/// tick, only on a genuine, sustained overshoot. Uses mvp's untouched
/// starting state, where faction 0's industry comfortably covers its 2
/// starting units.
#[test]
fn heuristic_agent_does_not_disband_within_cap() {
    let world = scenario::build_world();
    let faction = FactionId(0);

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    assert!(
        !actions.iter().any(|a| matches!(a, Action::DisbandUnit { .. })),
        "a faction well within its unit cap must not disband anything: {actions:?}"
    );
}

/// Even a faction badly over-extended must never stand down a unit that is
/// currently under enemy contact - mirrors `action::apply_disband`'s own
/// refusal (`ActionError::RegionContested`), and is exactly what stops this
/// policy from ever disarming a faction mid-battle: the weakest unit in a
/// contested region must be skipped in favour of a weak *uncontested* one,
/// never queued at all. Checked this fails when broken: temporarily removed
/// the `unit_contested` filter from `disband_excess` - the assertion below
/// then fails (the contested unit appears in `disbands`).
#[test]
fn heuristic_agent_never_disbands_a_contested_unit() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
        }
    }

    // One of the two starting units is moved into contact with an enemy at
    // its own capital - the weakest possible target (lowest manpower) so a
    // policy that ignores contested status would pick it first.
    let starting_units: Vec<UnitId> =
        world.units.iter().filter(|u| u.owner == faction && u.alive).map(|u| u.id).collect();
    let contested_unit = starting_units[0];
    world.unit_mut(contested_unit).manpower = 0.01;
    let enemy = FactionId(1);
    let raider_id = UnitId(world.units.len() as u32);
    world.units.push(Unit {
        id: raider_id,
        owner: enemy,
        name: "Enemy Raiding Force".to_string(),
        station: Station::Region(capital),
        movement: None,
        manpower: 1.0,
        equipment: 1.0,
        organization: 100.0,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Region(capital),
        experience: 0.0,
        alive: true,
    });

    push_idle_land_units(&mut world, faction, capital, 6);

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    let disbands: Vec<UnitId> = actions
        .iter()
        .filter_map(|a| match a {
            Action::DisbandUnit { unit } => Some(*unit),
            _ => None,
        })
        .collect();

    assert!(!disbands.is_empty(), "test setup: this faction should still be over-extended enough to disband");
    assert!(
        !disbands.contains(&contested_unit),
        "a unit under enemy contact must never be disbanded, even as the weakest candidate: {disbands:?}"
    );
}
