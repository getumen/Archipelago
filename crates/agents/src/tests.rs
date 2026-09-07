//! Regression tests for `HeuristicAgent` (see the A1 fix in offensive()).

use archipelago_sim::action::{self, Action};
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{
    AIR_OPERATING_RADIUS_KM, FOCUS_MARITIME_IMPORT_CAPACITY_MULT, IMPORT_COST_MACHINERY_PER_GOOD,
    UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG,
};
use archipelago_sim::diplomacy::{Stance, Treaty};
use archipelago_sim::focus::{self, NationalFocus};
use archipelago_sim::good::{Good, GOOD_COUNT};
use archipelago_sim::ids::{FactionId, RegionId, UnitId};
use archipelago_sim::military::{move_required, Movement, Unit};
use archipelago_sim::observation::Observation;
use archipelago_sim::scenario;
use archipelago_sim::trade;
use archipelago_sim::transport::Condition;
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
/// well past what its industry (`unit_cap`) can sustain, *and* whose
/// economy genuinely can't feed it (bad `supply_ratio` and an exhausted
/// Munitions stock - the 近畿府 case from `scenarios/japan_hex.json` day
/// 720: 36.5% `supply_ratio`, 0.0 Munitions), must stand the excess down on
/// its own, exactly like a human player would via the unit panel's new
/// button - this is the regression guard for `crate::disband_excess`
/// actually being wired into `decide_for_llm`.
///
/// Changed from the original head-count-only version of this test: the
/// trigger this guards is no longer "over `unit_cap` by a flat 25%" (that
/// margin is gone - see `disband_excess`'s own doc) but "over `unit_cap`
/// *and* insolvent", so this setup now explicitly drives both
/// `supply_ratio` and `Faction::stock[Munitions]` down to the insolvent
/// case, instead of relying on zeroed industry capacity alone to imply it.
/// Checked this fails when broken: temporarily reverted the guard to the
/// old `total <= cap * 1.25` head-count check - `disbands.len()` then comes
/// back `0` (9 units sits within `3.75`, the old margin) and the first
/// assertion fails.
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
    // `unit_cap`'s bare 3.0 floor.
    push_idle_land_units(&mut world, faction, capital, 6);
    let total_before: usize = world.units.iter().filter(|u| u.owner == faction && u.alive).count();
    assert_eq!(total_before, 9, "test setup: expected 3 starting + 6 fresh units");

    // Drive the new solvency signal into the insolvent case directly - an
    // empty national Munitions stockpile (so `munitions_buffer_days` reads
    // 0, however small the resulting daily demand is) and a `supply_ratio`
    // matching 近畿府's own measured 36.5%.
    world.faction_mut(faction).stock[Good::Munitions.index()] = 0.0;
    world.faction_mut(faction).supply_ratio = 0.365;

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

/// The specific regression this refinement fixes: a faction only *slightly*
/// past `unit_cap` must still shed its excess once the economy itself says
/// it can't keep up, even at an overshoot small enough that the old flat
/// head-count margin would have waved it through untouched. Uses mvp's
/// untouched starting industry (`unit_cap` == 9.2, `3.0 + 31/5.0`) rather
/// than zeroing it out, so the overshoot can be sized precisely: 10 units
/// clears `cap` but sits under the old `cap * 1.25` (== 11.5) margin -
/// exactly the 近畿府 shape, where the old trigger never fired because the
/// head count never got *that* far past the cap, even though the economy
/// was already failing to feed it.
/// Checked this fails when broken: reverting the guard to
/// `total <= cap * 1.25` (dropping the solvency check entirely) makes
/// `disbands.len()` come back `0`, since `10.0 <= 11.5`.
#[test]
fn heuristic_agent_disbands_when_insolvent_even_within_old_head_count_margin() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    // mvp's faction 0 starts with 3 units; + 7 fresh ones = 10. `unit_cap`
    // sits at 9.2 off mvp's own starting industry (untouched here), so
    // `10.0` clears `cap` but stays under the old `cap * 1.25` (== 11.5)
    // margin entirely.
    push_idle_land_units(&mut world, faction, capital, 7);
    let total_before: usize = world.units.iter().filter(|u| u.owner == faction && u.alive).count();
    assert_eq!(total_before, 10, "test setup: expected 3 starting + 7 fresh units");

    world.faction_mut(faction).stock[Good::Munitions.index()] = 0.0;
    world.faction_mut(faction).supply_ratio = 0.365;

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
        1,
        "an insolvent faction must shed its unit of overshoot even though it never crossed \
         the old 25% head-count margin: {actions:?}"
    );
}

/// The mirror image of the case above: a faction fielding a force well
/// past `unit_cap` in raw head-count terms - far past even the old 25%
/// margin - must not disband anything at all as long as its economy is
/// actually keeping up. This is 北海道方面軍's exact shape on
/// `scenarios/japan_hex.json`: a huge territory supports a healthy
/// Munitions stockpile and a comfortable `supply_ratio` even while fielding
/// several times its bare `unit_cap` floor in units, because the cap here
/// (an industry-derived *estimate* of sustainable size) is a poor proxy for
/// whether the force is actually being fed - the direct, measured signal
/// says it is. Checked this fails when broken: reverting the guard to the
/// old `total <= cap * DISBAND_UNIT_CAP_MARGIN` (1.25) check alone (with no
/// solvency condition) makes `disbands.len()` come back non-zero, since 9
/// units clears `3.0 * 1.25`.
#[test]
fn heuristic_agent_does_not_disband_when_solvent_despite_many_units_over_cap() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
        }
    }

    // Same 9-unit overshoot as `heuristic_agent_disbands_when_over_extended`
    // - the only difference is solvency, isolating that as the variable
    // that now decides the outcome.
    push_idle_land_units(&mut world, faction, capital, 6);

    // A deep Munitions reserve and a comfortable delivery ratio - the
    // 北海道方面軍 shape: `munitions_buffer_days` comes out far past
    // `DISBAND_SOLVENCY_BUFFER_DAYS`, and `supply_ratio` sits above
    // `DISBAND_SOLVENCY_SUPPLY_RATIO`, so the `insolvent` check in
    // `disband_excess` reads false on both counts.
    world.faction_mut(faction).stock[Good::Munitions.index()] = 1000.0;
    world.faction_mut(faction).supply_ratio = 0.9;

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    assert!(
        !actions.iter().any(|a| matches!(a, Action::DisbandUnit { .. })),
        "a solvent faction must not disband anything, no matter how far past unit_cap its raw \
         head count sits: {actions:?}"
    );
}

/// Distinguishes a *temporarily cut-off* front from genuine insolvency -
/// design goal explicitly called out for this refinement. A besieged
/// region's demand goes unserved (`logistics::distribute_supply`'s
/// `avail[r][f]` is zero for it), which drags `supply_ratio` down exactly
/// like real insolvency does, but the Munitions that would have gone to
/// that front is never drawn from the national `Faction::stock` in the
/// first place (nothing was delivered there), so the buffer behind the
/// rest of the force stays intact. This test drives exactly that
/// combination directly - low `supply_ratio`, healthy Munitions stock - and
/// checks `disband_excess` reads it as "logistics problem, not an economic
/// one" and leaves the force alone. Checked this fails when broken:
/// dropping the `munitions_buffer_days(...) < DISBAND_SOLVENCY_BUFFER_DAYS`
/// half of the `insolvent` check (leaving only the `supply_ratio`
/// condition) makes `disbands.len()` come back non-zero, since
/// `supply_ratio` alone already reads as bad here.
#[test]
fn heuristic_agent_does_not_gut_itself_when_temporarily_cut_off() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    for region in world.regions.iter_mut() {
        if region.owner == faction {
            region.capacity = [0.0; GOOD_COUNT];
        }
    }

    push_idle_land_units(&mut world, faction, capital, 6);

    // Bad delivery ratio (as if a front were cut off) but a deep national
    // reserve behind it - unlike the insolvent case above, the stockpile is
    // untouched.
    world.faction_mut(faction).stock[Good::Munitions.index()] = 1000.0;
    world.faction_mut(faction).supply_ratio = 0.2;

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    assert!(
        !actions.iter().any(|a| matches!(a, Action::DisbandUnit { .. })),
        "a faction with a healthy Munitions reserve must not gut its own army just because one \
         tick's delivery ratio looks bad: {actions:?}"
    );
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
///
/// Also drives the solvency signal into the insolvent case directly (as
/// `heuristic_agent_disbands_when_over_extended` above now does) - zeroed
/// industry capacity alone no longer implies insolvency under the new,
/// solvency-driven trigger, so this test's own "still over-extended enough
/// to disband" precondition needs it spelled out explicitly too.
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
    world.faction_mut(faction).stock[Good::Munitions.index()] = 0.0;
    world.faction_mut(faction).supply_ratio = 0.365;

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

/// Stage 9D (docs/phase9-spec.md "4. AI"): `recruit` must stop growing the
/// land force the instant nationwide `Faction::supply_ratio` alone reads
/// below `DISBAND_SOLVENCY_SUPPLY_RATIO` (0.5) - *without* waiting for
/// `munitions_insolvent`'s stricter two-signal condition (a drained
/// Munitions buffer too) to also trip. This is the specific defect Stage 9B
/// exposed and the old code had no way to see: a faction can carry a
/// perfectly healthy Munitions stockpile (built up before the front thinned
/// out) while the *network* is already failing to carry today's demand
/// (`Faction::supply_ratio`, from `logistics::distribute_supply`'s
/// capacity-constrained flow) - measured on mvp seed 1, this used to let
/// `HeuristicAgent` recruit straight through a transport network that could
/// no longer feed the force it already had (10 units at old-model
/// `supply_ratio` 0.500 winning by day 265, regressed to 15 units at
/// Stage-9B `supply_ratio` 0.095 stalemating at day 720).
///
/// Confirmed this fails without the fix: temporarily removed the new
/// `f.supply_ratio < DISBAND_SOLVENCY_SUPPLY_RATIO` check from `recruit` and
/// re-ran - the first assertion below failed (`actions` contained a
/// `RecruitUnit` even at `supply_ratio` 0.2 with a full Munitions stock).
/// Reverted before committing.
#[test]
fn heuristic_agent_stops_recruiting_when_network_supply_ratio_is_bad() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    // Plenty of manpower/Arms/Munitions, and mvp's untouched starting
    // industry keeps this faction's 3 starting units well under `unit_cap`
    // - nothing *else* here should stop `recruit` from firing.
    world.faction_mut(faction).manpower = 50.0;
    world.faction_mut(faction).stock[Good::Arms.index()] = 100.0;
    world.faction_mut(faction).stock[Good::Munitions.index()] = 1000.0;
    world.faction_mut(faction).supply_ratio = 0.2; // well below DISBAND_SOLVENCY_SUPPLY_RATIO (0.5)

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);
    assert!(
        !actions.iter().any(|a| matches!(a, Action::RecruitUnit { .. })),
        "must not recruit while supply_ratio reads badly served, even with an ample Munitions stock: {actions:?}"
    );

    // Sanity: the identical setup with a healthy supply_ratio does recruit -
    // proving the gate above, not some unrelated precondition (mvp's own
    // manpower/Arms/unit_cap numbers), is what suppressed it.
    world.faction_mut(faction).supply_ratio = 1.0;
    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);
    assert!(
        actions.iter().any(|a| matches!(a, Action::RecruitUnit { .. })),
        "expected a healthy supply_ratio to let recruiting proceed: {actions:?}"
    );
}

/// Stage 9D AI: `transport_repair_ai` must fund restoring this faction's own
/// worst-damaged `TransportLine` via `Build`'s `Project::TransportLine`,
/// once it drops below `TRANSPORT_REPAIR_CONDITION_FLOOR` - "value acting on
/// the transport network" (docs/phase9-spec.md "4. AI"), the AI half of
/// Stage 9D's client-visible line rendering.
///
/// Confirmed this fails without the fix: temporarily removed the
/// `transport_repair_ai` call from `decide_for_llm` and re-ran - `actions`
/// contained no `Build` with a `Project::TransportLine` at all. Reverted
/// before committing.
#[test]
fn heuristic_agent_repairs_its_own_damaged_transport_line() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    let own_line = world
        .transport_lines
        .iter()
        .position(|l| {
            let ra = world.transport_node(l.from).region;
            let rb = world.transport_node(l.to).region;
            world.region(ra).owner == faction && world.region(rb).owner == faction
        })
        .expect("mvp's faction 0 must own at least one whole transport line");
    world.transport_lines[own_line].condition = Condition::new(0.2).unwrap();

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    let repairs: Vec<_> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Build { project: archipelago_sim::construction::Project::TransportLine(line), .. } => Some(*line),
            _ => None,
        })
        .collect();
    assert!(!repairs.is_empty(), "expected a Build order restoring the damaged line: {actions:?}");
    assert_eq!(repairs[0].index(), own_line, "must target the actual damaged line, not an arbitrary one");
}

/// `codex review` (P2): the worst-damaged own line is only a useful choice
/// if it can actually be invested in this tick. Selecting it first and then
/// checking host eligibility made the AI repair *nothing* whenever that one
/// line was blocked at both endpoints, even with other damaged lines sitting
/// repairable - the whole mechanism went idle waiting on a single line.
/// `transport_repair_target` now filters for a usable host while choosing,
/// so this pins "worst **repairable**", not "worst, if we get lucky".
///
/// **Confirmed this test can fail.** Restoring the old shape (fold to the
/// worst owned damaged line, then `find` a host and `?` out) made `repairs`
/// come back empty - the AI issued no `Build` at all - so both assertions
/// below tripped. Restored, and it passes.
#[test]
fn heuristic_agent_repairs_the_worst_line_it_can_actually_host() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    let own_lines: Vec<(usize, RegionId, RegionId)> = world
        .transport_lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let ra = world.transport_node(l.from).region;
            let rb = world.transport_node(l.to).region;
            (world.region(ra).owner == faction && world.region(rb).owner == faction).then_some((i, ra, rb))
        })
        .collect();
    assert!(own_lines.len() >= 2, "this test needs at least two wholly-owned lines, got {}", own_lines.len());

    // The worse of the two is deliberately made impossible to host: both of
    // its endpoint regions are already mid-construction, which is exactly
    // what `action::apply_build` refuses. `reachable` must share no
    // endpoint region with `blocked` (Stage 10A: a region can now own more
    // than one wholly-owned line of its own, e.g. its Depot<->Port and
    // Depot<->Airfield spurs both sit inside that one region - picking two
    // lines that happen to share `blocked`'s own region would put
    // `reachable` under construction too, the same way `blocked` itself
    // deliberately is).
    let (blocked, blocked_ra, blocked_rb) = own_lines[0];
    let (reachable, _, _) = *own_lines[1..]
        .iter()
        .find(|&&(_, ra, rb)| ra != blocked_ra && ra != blocked_rb && rb != blocked_ra && rb != blocked_rb)
        .expect("this test needs a second wholly-owned line with no endpoint region shared with the first");
    world.transport_lines[blocked].condition = Condition::new(0.1).unwrap();
    world.transport_lines[reachable].condition = Condition::new(0.3).unwrap();
    for endpoint in [world.transport_lines[blocked].from, world.transport_lines[blocked].to] {
        let region = world.transport_node(endpoint).region;
        world.regions[region.index()].construction = Some(archipelago_sim::construction::Construction {
            project: archipelago_sim::construction::Project::Infrastructure,
            invested: 0.0,
            required: 1e6,
        });
    }

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    let repairs: Vec<_> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Build { project: archipelago_sim::construction::Project::TransportLine(line), .. } => Some(*line),
            _ => None,
        })
        .collect();
    assert!(
        !repairs.is_empty(),
        "a damaged line with a free host must still be repaired even though a worse one is blocked: {actions:?}"
    );
    assert_eq!(
        repairs[0].index(),
        reachable,
        "must fall through to the worst line it can actually host, not stall on the unhostable one"
    );
}

/// Stage 9D AI: `transport_interdict_ai` must strike the enemy's own
/// transport network once at war and `offensive`'s own `allow_offense` gate
/// is open - the other half of "value acting on the transport network".
///
/// Confirmed this fails without the fix: temporarily removed the
/// `transport_interdict_ai` call from `decide_for_llm` and re-ran - `actions`
/// contained no `InterdictLine` at all even though mvp's factions start at
/// war by default. Reverted before committing.
#[test]
fn heuristic_agent_interdicts_an_enemy_transport_line() {
    let world = scenario::build_world();
    let faction = FactionId(0);
    assert!(
        world.diplomacy.is_at_war(faction, FactionId(1)) || world.diplomacy.is_at_war(faction, FactionId(2)),
        "test setup: mvp's factions must start at war for there to be a hostile line to strike"
    );

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    assert!(
        actions.iter().any(|a| matches!(a, Action::InterdictLine { .. })),
        "expected an InterdictLine order against a hostile faction's own transport network: {actions:?}"
    );
}

/// Stage 10D AI (docs/phase10-spec.md "Stage 10D": "recruit squadrons when
/// it makes sense"): a fresh faction with no air units yet, and comfortably
/// affordable manpower/Arms/Machinery (mvp's own starting stock -
/// `scenario::FACTION_STOCK`), must recruit toward `AIR_MIN_SQUADRONS`.
///
/// Confirmed this fails without the fix: temporarily removed the
/// `air_recruit` call from `decide_for_llm` and re-ran - `actions` contained
/// no `RecruitUnit { domain: Domain::Air, .. }` at all even though mvp's
/// faction 0 starts able to afford one. Reverted before committing.
#[test]
fn heuristic_agent_recruits_air_when_it_can_afford_it() {
    let world = scenario::build_world();
    let faction = FactionId(0);

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::RecruitUnit { domain: archipelago_sim::world::Domain::Air, .. })),
        "expected a Domain::Air RecruitUnit order from a faction with no air units yet and money to spend: {actions:?}"
    );
}

/// Stage 10D AI: `air_strike_ai` must issue `Action::StrikeNode` against a
/// hostile `Airfield`/`Port` node once this faction actually has air power
/// of its own *and that air power can reach the target's airspace* - the
/// "use them for what Phase 10 built them for" half of Stage 10D, distinct
/// from `heuristic_agent_interdicts_an_enemy_transport_line` (lines, not
/// nodes) above.
///
/// The reach half of this rule is new since `Action::StrikeNode` started
/// gating and pricing itself on `Region::air_superiority`
/// (`action::apply_strike_node`): `air_strike_ai` now only considers a
/// target whose own region this faction's air already projects some share
/// onto (`air::tick_air_superiority`'s reach test - operational airfield,
/// within `air::AIR_OPERATING_RADIUS_KM`), so the two hostile regions are
/// placed on top of the relocated air unit's own base (distance `0.0`,
/// trivially inside the radius) rather than trusted to fall inside it by
/// mvp's own incidental geometry.
///
/// Confirmed this fails without the fix: temporarily removed the
/// `air_strike_ai` call from `decide_for_llm` and re-ran with the same
/// fixture - `actions` contained no `StrikeNode` at all even with an air
/// unit in play and a hostile airfield to strike. Reverted before
/// committing.
#[test]
fn heuristic_agent_strikes_an_enemy_node_once_it_has_air_power() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    assert!(
        world.diplomacy.is_at_war(faction, FactionId(1)),
        "test setup: mvp's factions must start at war for there to be a hostile node to strike"
    );

    // Give faction 0 an air unit by moving an existing one of its own onto
    // its own airfield - `Station::domain()` derives purely from the
    // station variant, so this alone makes `air_strike_ai`'s own
    // `own_unit_count(obs, Domain::Air) > 0` gate true, without needing to
    // hand-build a fresh `Unit`.
    let own_node = world
        .transport_nodes
        .iter()
        .find(|n| n.kind == archipelago_sim::transport::TransportNodeKind::Airfield && world.region(n.region).owner == faction)
        .map(|n| n.id)
        .expect("mvp gives every faction's own territory an airfield");
    let own_region = world.transport_node(own_node).region;
    let unit_id = UnitId(0);
    assert_eq!(world.unit(unit_id).owner, faction, "scenario::build_world assigns unit 0 to faction 0");
    world.unit_mut(unit_id).station = Station::Airfield(own_node);

    // Collapse every hostile region onto this faction's own base so reach
    // is not what this test is measuring - `air_superiority_derives_from_
    // both_sides_strength_by_ratio_and_is_order_independent`'s own
    // convention of placing regions by hand rather than relying on mvp's
    // incidental map layout.
    let hostile_regions: Vec<RegionId> = (0..world.regions.len())
        .map(|i| RegionId(i as u32))
        .filter(|&r| {
            let owner = world.region(r).owner;
            owner != faction && world.diplomacy.is_at_war(faction, owner)
        })
        .collect();
    assert!(!hostile_regions.is_empty(), "test setup: mvp must have at least one hostile region");
    let own_position = world.region(own_region).position;
    for &r in &hostile_regions {
        world.region_mut(r).position = own_position;
    }
    archipelago_sim::air::tick_air_superiority(&mut world);
    assert!(
        world.region(hostile_regions[0]).air_superiority[faction.index()].get() > 0.0,
        "test setup: the relocated air unit must actually project power onto a hostile region"
    );

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &world };
    let actions = agent.decide(&obs);

    assert!(
        actions.iter().any(|a| matches!(a, Action::StrikeNode { .. })),
        "expected a StrikeNode order against a hostile faction's own airfield/port once air power exists and can reach it: {actions:?}"
    );
}

/// Regression guard for the mvp land-army collapse traced to `disband_
/// excess_air`/`air_recruit`: Stage 10D copied `disband_excess_naval`'s
/// `land_is_gone` chronic-insolvency gate verbatim for air, which put
/// `AIR_MIN_SQUADRONS` in the *same* protected tier as `NAVY_MIN_FLEETS`
/// (both released only once land alone was gone) instead of one tier
/// further down, the way `AIR_MIN_SQUADRONS`'s own doc already frames air -
/// "one domain further" than the navy, smaller and cheaper still. Two
/// floors sharing one tier meant neither ever gave ground until land was
/// completely gone, so both drew on the same national Munitions pool at
/// full, fixed cost for the entire time land was doing all the adjusting.
/// Measured on `scenarios/mvp.json` seeds 1-8 with this bug present: land
/// collapsed to 0-2 units per surviving faction (down from 7-11
/// pre-Phase-10) and only 1/8 seeds still reached `Outcome::Victory` (down
/// from 8/8) - nobody left alive with enough of an army to take ground.
///
/// The fix (`disband_excess_air`'s own doc) makes air the tier *below* the
/// navy, not its sibling: air's own chronic branch now waits for the navy
/// to be gone too, not land alone.
///
/// Confirmed this fails without the fix: reverted `disband_excess_air`'s
/// `land_and_navy_gone` back to checking only `own_unit_count(obs,
/// Domain::Land) == 0` (the pre-fix shape) and re-ran - the first
/// assertion below failed with a `DisbandUnit` for the air squadron
/// present even though the navy's own floor was still fully intact.
/// Reverted before committing.
#[test]
fn air_floor_is_not_shed_while_the_navy_still_stands() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let capital = world.faction(faction).capital;

    let own_node = world
        .transport_nodes
        .iter()
        .find(|n| {
            n.kind == archipelago_sim::transport::TransportNodeKind::Airfield && world.region(n.region).owner == faction
        })
        .map(|n| n.id)
        .expect("mvp gives every faction's own territory an airfield");
    let own_zone = world.zones_touching(capital).into_iter().next().expect("mvp's capital touches a sea zone");

    // Land must be entirely gone - this test is specifically about whether
    // the navy alone is enough to keep protecting the air floor once land
    // no longer can.
    for unit in world.units.iter_mut().filter(|u| u.owner == faction) {
        unit.alive = false;
    }

    let push = |world: &mut archipelago_sim::world::World, station: Station, count: usize| -> Vec<UnitId> {
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            let id = UnitId(world.units.len() as u32);
            world.units.push(Unit {
                id,
                owner: faction,
                name: "Test Unit".to_string(),
                station,
                movement: None,
                manpower: UNIT_MANPOWER,
                equipment: UNIT_EQUIPMENT,
                organization: UNIT_ORG,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: station,
                experience: 0.0,
                alive: true,
            });
            ids.push(id);
        }
        ids
    };
    let sea = push(&mut world, Station::Sea(own_zone), 2);
    let air = push(&mut world, Station::Airfield(own_node), 2);

    // A chronic Munitions drought, well past `CHRONIC_INSOLVENCY_TICKS_FOR_
    // FLOOR_TRIM` (15) - `disband_excess_air` takes the streak length as a
    // plain argument, so this drives its chronic branch directly without
    // needing to replay 60+ in-game days through a live `HeuristicAgent`.
    world.faction_mut(faction).stock[Good::Munitions.index()] = 0.0;

    let obs = Observation { faction, world: &world };
    let mut actions = Vec::new();
    crate::disband_excess_air(faction, 20, &obs, &mut actions);
    assert!(
        actions.is_empty(),
        "air's floor must not be cut while the navy's own floor is still standing (only land is gone): {actions:?}"
    );

    // Sanity companion: once the navy is gone too, the same chronic streak
    // does finally reach the air floor - the recovery path this whole
    // mechanism exists for is not itself broken by the fix.
    for &u in &sea {
        world.unit_mut(u).alive = false;
    }
    let obs = Observation { faction, world: &world };
    let mut actions = Vec::new();
    crate::disband_excess_air(faction, 20, &obs, &mut actions);
    assert_eq!(
        actions.len(),
        1,
        "once land and navy are both gone, air's own chronic branch must shed a unit: {actions:?}"
    );
    match actions[0] {
        Action::DisbandUnit { unit } => {
            assert!(air.contains(&unit), "must disband one of the air units, not something else: {actions:?}")
        }
        ref other => panic!("expected a DisbandUnit, got {other:?}"),
    }
}

/// Stage 10 follow-up AI: `air_redeploy` must reposition a squadron whose
/// current airfield reaches no front region at all, toward a better own
/// airfield that does - the AI half of the gap this stage closes now that
/// `Domain::Air` `MoveUnit` support is real (`action::apply_move`'s
/// `Station::Airfield` arm). Before this, `best_own_airfield_region`'s own
/// doc noted recruitment placement was the *only* lever the AI had over
/// which airspace its air power projected over, because there was no way to
/// reposition a squadron once the front moved past it.
///
/// This pins the **outcome**, not the emission: `air_redeploy`'s first shape
/// emitted a `MoveUnit` toward whichever own airfield was closest to the
/// front *anywhere on the map*, with no check that the squadron could
/// actually reach it in one order. That action was real, but
/// `action::apply_move`'s `Station::Airfield` arm rejects anything past
/// `AIR_OPERATING_RADIUS_KM` as `ActionError::NotAdjacent` - so a test that
/// only asserted an action was generated (the P1 this replaces) stayed green
/// while the squadron never actually moved, the same shape
/// docs/conventions.md §2 and CLAUDE.md's "検証についての教訓" already
/// record for two other regression guards. This one instead runs the
/// action through the real `action::apply_action` and `sim::Simulation`
/// path and asserts the squadron's `Station` actually changes.
///
/// mvp's faction 0 (touhou_rengou) owns four regions
/// (hokkaido/kita_tohoku/minami_tohoku/kanto); only kanto borders foreign
/// territory (chuo_domei's shinetsu_hokuriku/tokai), so it is faction 0's
/// sole front region. Positions are hand-set (the same convention `air_
/// superiority_derives_from_both_sides_strength_by_ratio_and_is_order_
/// independent` and `heuristic_agent_strikes_an_enemy_node_once_it_has_air_
/// power` already use), laid out on one line through the front so reach -
/// not mvp's own incidental map layout - is what this measures:
///
/// - kanto (front) at distance `0`
/// - kita_tohoku (reachable stepping stone) at `0.9 * AIR_OPERATING_RADIUS_KM`
///   from kanto - within one hop of kanto, and, critically, also within one
///   hop of hokkaido below, so it is a legal intermediate stop
/// - hokkaido (stranded) at `1.8 * AIR_OPERATING_RADIUS_KM` from kanto -
///   beyond one hop of kanto directly, but exactly `0.9 * AIR_OPERATING_
///   RADIUS_KM` (one hop) from kita_tohoku
/// - minami_tohoku pushed far off this line so it never competes as a
///   candidate
///
/// A squadron at hokkaido therefore cannot legally jump straight to kanto -
/// only to kita_tohoku. The old, reachability-blind rule picked kanto
/// anyway (distance `0` beats kita_tohoku's `0.9`); this test asserts the
/// squadron ends up at kita_tohoku, which only a reachability-aware rule
/// can produce.
///
/// Confirmed this fails without the fix: temporarily reverted
/// `air_redeploy`'s target selection to plain `best_own_airfield_region`
/// (ignoring reachability, as it read before this fix) and re-ran - `sim.
/// apply` returned `ActionError::NotAdjacent` for the emitted `MoveUnit`
/// (toward kanto, `1.8 * AIR_OPERATING_RADIUS_KM` away), and the squadron's
/// `Station` never left hokkaido even after stepping the simulation
/// forward, failing the final assertion. Reverted before committing.
#[test]
fn heuristic_agent_redeploys_a_stranded_squadron_toward_the_front() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    let front_base = RegionId(3); // kanto - faction 0's only front region
    let stepping_stone = RegionId(1); // kita_tohoku - one hop from both ends
    let stranded_base = RegionId(0); // hokkaido - not a front region
    let out_of_the_way = RegionId(2); // minami_tohoku - kept clear of the line

    world.region_mut(front_base).position = [0.0, 0.0];
    world.region_mut(stepping_stone).position = [AIR_OPERATING_RADIUS_KM * 0.9, 0.0];
    world.region_mut(stranded_base).position = [AIR_OPERATING_RADIUS_KM * 1.8, 0.0];
    world.region_mut(out_of_the_way).position = [0.0, AIR_OPERATING_RADIUS_KM * 100.0];

    // The stepping stone gets a *second* airfield, and its first is wrecked.
    // `codex review` (P2): the region filter accepts a region as long as any
    // of its airfields works, but taking the region's lowest-id node there
    // hands back the wrecked one and `apply_move` rejects the order every
    // tick. This shape is the actual destination the AI must find.
    let wrecked_first = world.airfield_node(stepping_stone).expect("mvp regions all carry an airfield node").id;
    let stepping_stone_airfield = archipelago_sim::ids::TransportNodeId(world.transport_nodes.len() as u32);
    world.transport_nodes.push(archipelago_sim::transport::TransportNode {
        id: stepping_stone_airfield,
        name: "kita_tohoku spare airfield".to_string(),
        kind: archipelago_sim::transport::TransportNodeKind::Airfield,
        region: stepping_stone,
        condition: archipelago_sim::transport::Condition::FULL,
    });
    world.transport_nodes[wrecked_first.index()].condition =
        archipelago_sim::transport::Condition::new(0.0).expect("0.0 is a valid condition");
    let stranded_airfield = world.airfield_node(stranded_base).expect("mvp regions all carry an airfield node").id;

    // Give faction 0 a squadron stuck at hokkaido by moving an existing unit
    // of its own onto that airfield - `Station::domain()` derives purely
    // from the station variant, the same trick `heuristic_agent_strikes_an_
    // enemy_node_once_it_has_air_power` uses to avoid hand-building a fresh
    // `Unit`.
    let unit_id = UnitId(0);
    assert_eq!(world.unit(unit_id).owner, faction, "scenario::build_world assigns unit 0 to faction 0");
    world.unit_mut(unit_id).station = Station::Airfield(stranded_airfield);
    world.unit_mut(unit_id).movement = None;

    {
        let obs = Observation { faction, world: &world };
        assert_eq!(
            obs.front_regions(),
            vec![front_base],
            "test setup: faction 0's own map only puts kanto on the front"
        );
    }

    let mut sim = archipelago_sim::sim::Simulation::with_world(world, 1);
    let mut agent = HeuristicAgent::new(faction, 1.15);
    let obs = Observation { faction, world: &sim.world };
    let actions = agent.decide(&obs);

    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::MoveUnit { unit, to: Station::Airfield(node) }
                if *unit == unit_id && *node == stepping_stone_airfield
        )),
        "expected the stranded squadron to redeploy toward kita_tohoku, the only reachable stepping stone: {actions:?}"
    );

    let errors = sim.apply(faction, &actions);
    assert!(errors.is_empty(), "the AI must never emit an action the simulator rejects: {errors:?}");

    // `AIR_MOVE_DAYS` (1.0) at the slowest possible daily step (0.5, at zero
    // supply) still arrives within a handful of days - `military::
    // tick_movement`'s own per-day progress.
    for _ in 0..10 {
        if sim.world.unit(unit_id).station == Station::Airfield(stepping_stone_airfield) {
            break;
        }
        sim.step();
    }
    assert_eq!(
        sim.world.unit(unit_id).station,
        Station::Airfield(stepping_stone_airfield),
        "the stranded squadron must actually arrive at kita_tohoku, not merely be ordered there"
    );
}

/// `codex review` (P2): during insolvency, `disband_excess_air` can pick a
/// squadron for `DisbandUnit` in the very same tick `air_redeploy` would
/// otherwise pick that same squadron for a `MoveUnit` - both read the same
/// pre-tick `Observation`, with no way for either to see what the other
/// decided. Actions apply in the order `decide_for_llm` pushed them
/// (`sim::Simulation::apply`), so the `MoveUnit` that used to follow would
/// be rejected outright as `ActionError::UnitDead` once the `DisbandUnit`
/// ahead of it landed - a rejection an API/RL caller would see for an order
/// the AI should never have issued at all.
///
/// Reuses the exact stranded-squadron geometry `heuristic_agent_redeploys_
/// a_stranded_squadron_toward_the_front` already established: kanto (front,
/// `x=0`) is faction 0's only front region once its land and navy are gone
/// too, kita_tohoku (`x = 0.9 * AIR_OPERATING_RADIUS_KM`) is a legal
/// one-hop stepping stone, and hokkaido (`x = 1.8 * AIR_OPERATING_RADIUS_KM`)
/// is out of the front's reach directly - the shape that makes `air_
/// redeploy` want to move the squadron based there at all. A single
/// squadron at hokkaido, with land and navy both wiped out and a chronic
/// Munitions drought, is simultaneously `disband_excess_air`'s only
/// disband candidate and `air_redeploy`'s only redeploy candidate - the
/// exact overlap the fix must close.
///
/// **Confirmed this can fail.** Temporarily dropped the `if disbanding.
/// contains(&unit_id) { continue; }` guard from `air_redeploy`. Re-ran:
/// `actions` after both calls held both a `DisbandUnit` and a `MoveUnit`
/// for the same unit id, the first assertion below failed, and feeding
/// that exact two-action list through `action::apply_action` in order (the
/// counterfactual block below) reproduced the reported symptom directly -
/// `Ok(())` for the disband, then `Err(ActionError::UnitDead)` for the
/// move right behind it. Reverted before committing.
#[test]
fn heuristic_agent_never_moves_a_unit_it_is_also_disbanding() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);

    let front_base = RegionId(3); // kanto - faction 0's only front region
    let stepping_stone = RegionId(1); // kita_tohoku - one hop from both ends
    let stranded_base = RegionId(0); // hokkaido - not a front region
    let out_of_the_way = RegionId(2); // minami_tohoku - kept clear of the line

    world.region_mut(front_base).position = [0.0, 0.0];
    world.region_mut(stepping_stone).position = [AIR_OPERATING_RADIUS_KM * 0.9, 0.0];
    world.region_mut(stranded_base).position = [AIR_OPERATING_RADIUS_KM * 1.8, 0.0];
    world.region_mut(out_of_the_way).position = [0.0, AIR_OPERATING_RADIUS_KM * 100.0];

    // Land and navy both gone - `disband_excess_air`'s chronic branch only
    // reaches air once neither tier above it can absorb the cut any more
    // (its own doc).
    for unit in world.units.iter_mut().filter(|u| u.owner == faction) {
        unit.alive = false;
    }

    let stranded_airfield = world.airfield_node(stranded_base).expect("mvp regions all carry an airfield node").id;
    let unit_id = UnitId(world.units.len() as u32);
    world.units.push(Unit {
        id: unit_id,
        owner: faction,
        name: "Test Squadron".to_string(),
        station: Station::Airfield(stranded_airfield),
        movement: None,
        manpower: UNIT_MANPOWER,
        equipment: UNIT_EQUIPMENT,
        organization: UNIT_ORG,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: Station::Airfield(stranded_airfield),
        experience: 0.0,
        alive: true,
    });

    let obs = Observation { faction, world: &world };
    assert_eq!(obs.front_regions(), vec![front_base], "test setup: faction 0's own map only puts kanto on the front");

    // A chronic Munitions drought well past `CHRONIC_INSOLVENCY_TICKS_FOR_
    // FLOOR_TRIM` (15) - passed directly, the same way `air_floor_is_not_
    // shed_while_the_navy_still_stands` drives `disband_excess_air`'s
    // chronic branch without replaying dozens of in-game days.
    let mut actions = Vec::new();
    crate::disband_excess_air(faction, 20, &obs, &mut actions);
    assert_eq!(actions.len(), 1, "test setup: the lone stranded squadron must be the one chronic-insolvency candidate: {actions:?}");
    assert!(
        matches!(actions[0], Action::DisbandUnit { unit } if unit == unit_id),
        "test setup: the disbanded unit must be the stranded squadron: {actions:?}"
    );

    let disbanding: std::collections::BTreeSet<UnitId> = actions
        .iter()
        .filter_map(|a| match a {
            Action::DisbandUnit { unit } => Some(*unit),
            _ => None,
        })
        .collect();

    crate::air_redeploy(faction, &obs, &disbanding, &mut actions);
    assert_eq!(
        actions.len(),
        1,
        "a unit already selected for disbanding must never also receive a MoveUnit in the same batch: {actions:?}"
    );

    // Counterfactual: reproduce the reported symptom directly - the
    // pre-fix action list (a `DisbandUnit` followed by a `MoveUnit` for the
    // same unit) really does get the second order rejected as `UnitDead`
    // once applied in order, which is exactly why the exclusion above
    // matters and is not merely cosmetic.
    let stepping_stone_airfield =
        world.airfield_node(stepping_stone).expect("mvp regions all carry an airfield node").id;
    let mut counterfactual = world.clone();
    action::apply_action(&mut counterfactual, faction, Action::DisbandUnit { unit: unit_id })
        .expect("disbanding an uncontested unit must succeed");
    let result = action::apply_action(
        &mut counterfactual,
        faction,
        Action::MoveUnit { unit: unit_id, to: Station::Airfield(stepping_stone_airfield) },
    );
    assert_eq!(
        result,
        Err(action::ActionError::UnitDead),
        "sanity: a MoveUnit for an already-disbanded unit is exactly the rejection this fix prevents the AI from ever issuing"
    );
}
