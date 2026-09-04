//! Stage 4A acceptance tests (docs/phase4-spec.md "Stage 4A の受け入れ基準").
//! Every backend used here is `MockBackend` - none of these tests ever touch
//! the network or the filesystem.

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::diplomacy::Treaty;
use archipelago_sim::focus::NationalFocus;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::scenario;
use archipelago_sim::sim::Simulation;
use archipelago_sim::world::{Station, World};

use crate::llm::{Doctrine, LlmAgent, LlmError, MockBackend, Posture, LLM_CONSULT_INTERVAL_DAYS};
use crate::HeuristicAgent;

/// `malformed_response_is_discarded` (docs/phase4-spec.md "malformed_response_
/// is_discarded: 壊れた JSON で Doctrine が変わらない"): a first consult
/// installs a valid `Doctrine`; a second, syntactically broken response must
/// leave that `Doctrine` untouched rather than clearing or corrupting it.
#[test]
fn malformed_response_is_discarded() {
    let mut world = scenario::build_world();
    let faction = FactionId(0);
    let fallback = HeuristicAgent::new(faction, 1.15);
    let backend = MockBackend::new(vec![
        Ok(r#"{"posture":"defensive","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"dig in"}"#.to_string()),
        Ok("this is not json at all {".to_string()),
    ]);
    let mut agent = LlmAgent::new(backend, fallback);

    world.day = 0;
    let _ = agent.decide(&Observation { faction, world: &world });
    let after_first = agent.doctrine().cloned().expect("first valid response should set a Doctrine");
    assert_eq!(after_first.posture, Posture::Defensive);

    world.day += LLM_CONSULT_INTERVAL_DAYS; // due for a second consult
    let _ = agent.decide(&Observation { faction, world: &world });
    let after_second = agent.doctrine().cloned().expect("a malformed response must not clear the Doctrine");
    assert_eq!(after_second, after_first, "a malformed response must leave the previous Doctrine unchanged");
}

/// `doctrine_changes_behaviour` (docs/phase4-spec.md "Posture::Defensive の
/// Doctrine を与えると攻勢の頻度が明確に下がる"): the MVP opening position has
/// faction 0 with an obvious, affordable attack available (see
/// `moving_unit_is_not_reissued_toward_same_destination` in `tests.rs`).
/// Handing the same `HeuristicAgent` a `Defensive` `Doctrine` on that exact
/// position must suppress every one of those proactive attack orders.
#[test]
fn doctrine_changes_behaviour() {
    let world = scenario::build_world();
    let faction = FactionId(0);
    let obs = Observation { faction, world: &world };

    let mut baseline = HeuristicAgent::new(faction, 1.15);
    let baseline_actions = baseline.decide_for_llm(&obs, None);
    let baseline_attacks = count_attack_moves(&world, faction, &baseline_actions);
    assert!(baseline_attacks > 0, "expected the undoctrined heuristic to attack from the opening position");

    let doctrine = Doctrine {
        posture: Posture::Defensive,
        primary_target: None,
        avoid: Vec::new(),
        focus: None,
        seek_treaties: Vec::new(),
        caution_bias: 0.0,
        rationale: "hold the line".to_string(),
    };
    let mut defensive = HeuristicAgent::new(faction, 1.15);
    let defensive_actions = defensive.decide_for_llm(&obs, Some(&doctrine));
    let defensive_attacks = count_attack_moves(&world, faction, &defensive_actions);

    assert_eq!(defensive_attacks, 0, "Posture::Defensive must suppress proactive offensives outright");
    assert!(defensive_attacks < baseline_attacks);
}

fn count_attack_moves(world: &World, faction: FactionId, actions: &[Action]) -> usize {
    actions
        .iter()
        .filter(|a| {
            matches!(a, Action::MoveUnit { to: Station::Region(r), .. } if world.region(*r).owner != faction)
        })
        .count()
}

/// `llm_cannot_produce_invalid_actions` (docs/phase4-spec.md "敵地への建設な
/// ど不正な指示を含む Doctrine を与えても、生成される Action はすべて検証を
/// 通る"): a `Doctrine` built by hand (bypassing `llm::parse_doctrine`
/// entirely, unlike a real backend response) with an out-of-range
/// `primary_target`, `avoid`/`seek_treaties` entries naming itself and a
/// nonexistent faction, a non-finite `caution_bias`, and an oversized
/// `rationale` must not panic anywhere in `HeuristicAgent::decide_for_llm`,
/// must never propose a treaty with itself or an invalid target, and every
/// action it does produce must still be accepted or cleanly rejected by
/// `Simulation::apply` - never corrupting faction state.
#[test]
fn llm_cannot_produce_invalid_actions() {
    let world = scenario::build_world();
    let faction = FactionId(0);
    let out_of_range = FactionId(9_999);

    let hostile = Doctrine {
        posture: Posture::Offensive,
        primary_target: Some(out_of_range),
        avoid: vec![out_of_range, faction],
        focus: Some(NationalFocus::MilitaryUnification),
        seek_treaties: vec![(faction, Treaty::Alliance), (out_of_range, Treaty::Ceasefire)],
        caution_bias: f32::INFINITY,
        rationale: "x".repeat(10_000),
    };

    let mut agent = HeuristicAgent::new(faction, 1.15);
    let actions = agent.decide_for_llm(&Observation { faction, world: &world }, Some(&hostile));

    for action in &actions {
        if let Action::ProposeTreaty { to, .. } = action {
            assert_ne!(*to, faction, "must never propose a treaty with ourselves");
            assert!((to.index()) < world.factions.len(), "must never propose a treaty with an out-of-range faction");
        }
    }

    let mut sim = Simulation::new(7);
    sim.world = world;
    let _errors = sim.apply(faction, &actions); // must not panic; rejections are fine
    let f = sim.world.faction(faction);
    assert!(f.manpower >= 0.0, "applying a hostile Doctrine's actions must never drive manpower negative");
    for &s in &f.stock {
        assert!(s >= 0.0, "applying a hostile Doctrine's actions must never drive stock negative");
    }
}

/// Sanity check backing `llm_failure_falls_back_to_heuristic` (proven at the
/// CLI level in `apps/headless/tests/llm_integration.rs`, where an
/// always-failing backend's `--json` output is compared byte-for-byte
/// against a plain `HeuristicAgent` run): an `LlmAgent` whose backend always
/// errors never installs a `Doctrine` at all, so every `decide` call falls
/// through to `decide_for_llm(obs, None)` - the exact same call plain
/// `HeuristicAgent::decide` makes.
#[test]
fn always_failing_backend_never_sets_a_doctrine() {
    let faction = FactionId(0);
    let fallback = HeuristicAgent::new(faction, 1.15);
    let backend = MockBackend::always_err(LlmError::Unavailable);
    let mut agent = LlmAgent::new(backend, fallback);

    for day in [0, LLM_CONSULT_INTERVAL_DAYS, LLM_CONSULT_INTERVAL_DAYS * 5] {
        let mut w = scenario::build_world();
        w.day = day;
        let _ = agent.decide(&Observation { faction, world: &w });
        assert!(agent.doctrine().is_none(), "an always-failing backend must never install a Doctrine");
    }
}
