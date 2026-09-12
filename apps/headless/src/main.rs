//! Headless CLI runner (mvp-spec.md §8): drives three `HeuristicAgent`s
//! against `archipelago-sim` with no rendering, so the acceptance case -
//! "leave three AIs alone and history happens" - can be checked from a
//! terminal.

mod bench;
mod cli;
mod json;
mod report;

use archipelago_agents::llm::{LlmAgent, LlmBackend, LlmError, MockBackend, ScriptedBackend};
use archipelago_agents::newspaper::{self, NEWSPAPER_INTERVAL_DAYS};
use archipelago_sim::agent::Agent;
use archipelago_sim::event::Event;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::World;

use cli::{AgentKind, Args, BackendKind};

/// Loads the map this run uses: the embedded default scenario, or whatever
/// `--scenario <path>` names (Stage 6A, docs/phase6-spec.md "Stage 6A").
/// Per "壊れたデータで暗黙に既定値へ落ちないこと" - a scenario file that
/// fails to load is a hard error, never a silent fall-back to the default.
fn load_world(args: &Args) -> World {
    match &args.scenario {
        None => archipelago_sim::scenario::build_world(),
        Some(path) => archipelago_sim::scenario::load_file(path).unwrap_or_else(|e| {
            eprintln!("error: could not load --scenario {path}: {e}");
            std::process::exit(1);
        }),
    }
}

/// Stage 4A `--backend mock` (docs/phase4-spec.md "Stage 4A"): a small,
/// fixed, deterministic rotation of valid canned `Doctrine` responses -
/// varied enough to demonstrate `Doctrine`-driven behaviour actually
/// changing over a long run, and, being purely a function of call count
/// (`MockBackend`'s doc), identical every time the CLI is run with the same
/// `--seed` (`mock_backend_run_is_deterministic`).
fn mock_doctrine_backend() -> MockBackend {
    MockBackend::new(vec![
        Ok(r#"{"posture":"consolidate","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"stabilize the home front before any new venture"}"#
            .to_string()),
        Ok(r#"{"posture":"offensive","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":-0.2,"rationale":"press the advantage while it lasts"}"#
            .to_string()),
        Ok(r#"{"posture":"defensive","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.4,"rationale":"hold what we have and rebuild"}"#
            .to_string()),
    ])
}

/// Stage 4C `--newspaper` (docs/phase4-spec.md "Stage 4C — 新聞・報道生成"):
/// the backend `newspaper::generate_issue` tries before falling back to its
/// own event-driven Japanese template (`archipelago_agents::newspaper::
/// template_summary`).
///
/// External code review fix: the previous implementation
/// (`newspaper_mock_backend`, removed) unconditionally wired `--newspaper`
/// to a small fixed rotation of fabricated English flavor text, so every
/// run's articles described nothing about the actual game, regardless of
/// `--agent`/`--backend` - the real, event-driven template sat unused. Now:
/// with no LLM configured at all (`--agent heuristic`, the default, or
/// `--agent llm --backend fail`), this always fails, so `generate_issue`
/// always falls through to the template - which is what `--seed 1 --days
/// 200 --newspaper` (no `--agent`/`--backend` flags) actually exercises.
/// Only when the run has genuinely opted into an LLM backend
/// (`--agent llm --backend mock` or `--backend scripted:<path>`) does the
/// LLM path get a real backend to try, with the template still standing by
/// as its own fallback (`generate_article`'s doc) - a fresh, independent
/// instance from whatever backend `build_agents` wired up for `Doctrine`
/// consults, so the two never share a response-cycling call counter.
fn newspaper_backend(args: &Args) -> Box<dyn LlmBackend> {
    if args.agent != AgentKind::Llm {
        return Box::new(MockBackend::always_err(LlmError::Unavailable));
    }
    match &args.backend {
        BackendKind::Mock => Box::new(mock_doctrine_backend()),
        BackendKind::Fail => Box::new(MockBackend::always_err(LlmError::Unavailable)),
        BackendKind::Scripted(path) => match ScriptedBackend::from_file(path) {
            Ok(scripted) => Box::new(scripted),
            Err(_) => Box::new(MockBackend::always_err(LlmError::Unavailable)),
        },
    }
}

/// Builds this run's per-faction `Agent`s (docs/phase4-spec.md "headless
/// defaults to HeuristicAgent; LLM is opt-in via `--agent llm --backend
/// mock`"). Every faction gets the same `AgentKind`/`BackendKind` - a mixed
/// run isn't part of Stage 4A's scope - but each still gets its own
/// `HeuristicAgent` fallback (`archipelago_agents::default_heuristic_agent`'s
/// `DEFAULT_CAUTION`/`DEFAULT_PEACE_DISPOSITION` spread), and (for
/// `AgentKind::Llm`) its own independent backend instance.
fn build_agents(args: &Args, faction_count: usize) -> Vec<Box<dyn Agent>> {
    (0..faction_count)
        .map(|i| {
            let fallback = archipelago_agents::default_heuristic_agent(i);

            match args.agent {
                AgentKind::Heuristic => Box::new(fallback) as Box<dyn Agent>,
                AgentKind::Llm => {
                    let backend: Box<dyn LlmBackend> = match &args.backend {
                        BackendKind::Mock => Box::new(mock_doctrine_backend()),
                        BackendKind::Fail => Box::new(MockBackend::always_err(LlmError::Unavailable)),
                        BackendKind::Scripted(path) => match ScriptedBackend::from_file(path) {
                            Ok(scripted) => Box::new(scripted),
                            Err(e) => {
                                eprintln!(
                                    "warning: could not read --backend scripted:{path} ({e}); \
                                     this faction's LlmAgent will fall back to HeuristicAgent behaviour"
                                );
                                Box::new(ScriptedBackend::new(Vec::new()))
                            }
                        },
                    };
                    Box::new(LlmAgent::new(backend, fallback)) as Box<dyn Agent>
                }
            }
        })
        .collect()
}

fn main() {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    };

    if args.bench {
        bench::run(&args, load_world(&args));
        return;
    }

    let mut sim = Simulation::with_world(load_world(&args), args.seed);
    let mut agents: Vec<Box<dyn Agent>> = build_agents(&args, sim.world.factions.len());

    // `--json` keeps stdout a single parseable blob; `--quiet` only trims
    // the day-by-day log, the final board and outcome still print (§8).
    let log = !args.quiet && !args.json;
    let show_summary = !args.json;
    if log {
        report::print_header(args.seed, args.days, args.report);
    }

    // Stage 4C `--newspaper` (docs/phase4-spec.md "Stage 4C"): gated behind
    // `!args.json` the same way `log`/`show_summary` are, so this flag can
    // never add a single byte to `--json` output - see
    // `newspaper_does_not_affect_simulation`. `newspaper_backend`/`period_events`/
    // `period_start_world` are only ever *read from* `sim.world` and
    // `events`, never fed back into `sim` - see `newspaper.rs`'s module doc
    // for the boundary this enforces.
    //
    // docs/newspaper-spec.md §1: the article is driven by the *diff* between
    // the period's start and end board state, so (only when newspaper output
    // is actually wanted) a full `World` snapshot is kept from the moment
    // each period begins - `World` derives `Clone` for exactly this kind of
    // occasional, infrequent (`NEWSPAPER_INTERVAL_DAYS` apart) snapshot.
    let print_newspaper = args.newspaper && !args.json;
    let newspaper_backend = newspaper_backend(&args);
    let mut period_events: Vec<Event> = Vec::new();
    let mut period_start_world: Option<World> = print_newspaper.then(|| sim.world.clone());

    let outcome = loop {
        let outcome = sim.outcome(args.days);
        if outcome != Outcome::Ongoing {
            break outcome;
        }

        for f_idx in 0..sim.world.factions.len() {
            let faction = FactionId(f_idx as u32);
            if !sim.world.factions[f_idx].alive {
                continue;
            }
            let obs = Observation { faction, world: &sim.world };
            let actions = agents[f_idx].decide(&obs);
            sim.apply(faction, &actions);
        }

        let events = sim.step();
        if log {
            for event in &events {
                report::print_event(&sim.world, sim.world.day, event);
            }
            if sim.world.day % args.report == 0 {
                report::print_faction_table(&sim.world);
                report::print_sea_zone_table(&sim.world);
            }
        }

        if print_newspaper {
            period_events.extend(events.iter().cloned());
            if sim.world.day % NEWSPAPER_INTERVAL_DAYS == 0 {
                let start_world = period_start_world.as_ref().expect("print_newspaper implies Some");
                let issue = newspaper::generate_issue(&newspaper_backend, start_world, &sim.world, &period_events);
                report::print_newspaper_issue(&sim.world, &issue);
                period_events.clear();
                period_start_world = Some(sim.world.clone());
            }
        }
    };

    if show_summary {
        report::print_final_board(&sim.world);
        report::print_outcome(&sim.world, &outcome);
    }

    if args.json {
        println!("{}", json::serialize_state(&sim.world, args.seed, &outcome));
    }
}
