//! Headless CLI runner (mvp-spec.md §8): drives three `HeuristicAgent`s
//! against `archipelago-sim` with no rendering, so the acceptance case -
//! "leave three AIs alone and history happens" - can be checked from a
//! terminal.

mod cli;
mod json;
mod report;

use archipelago_agents::HeuristicAgent;
use archipelago_sim::agent::Agent;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation};

use cli::Args;

/// Per-faction caution (the force-ratio margin required before attacking;
/// higher = more cautious, ~1.0 = attack at parity) so the three AIs don't
/// converge on identical play (mvp-spec.md §7 suggests this spread).
const CAUTION: [f32; 3] = [1.15, 1.30, 1.45];

fn main() {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    };

    let mut sim = Simulation::new(args.seed);
    let mut agents: Vec<HeuristicAgent> = (0..sim.world.factions.len())
        .map(|i| {
            let caution = CAUTION.get(i).copied().unwrap_or(1.25);
            HeuristicAgent::new(FactionId(i as u32), caution)
        })
        .collect();

    // `--json` keeps stdout a single parseable blob; `--quiet` only trims
    // the day-by-day log, the final board and outcome still print (§8).
    let log = !args.quiet && !args.json;
    let show_summary = !args.json;
    if log {
        report::print_header(args.seed, args.days, args.report);
    }

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
    };

    if show_summary {
        report::print_final_board(&sim.world);
        report::print_outcome(&sim.world, outcome);
    }

    if args.json {
        println!("{}", json::serialize_state(&sim.world, args.seed, outcome));
    }
}
