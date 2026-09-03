//! `--bench` (docs/phase6-spec.md "Stage 6A — ベンチマーク"): measures the
//! things Stage 6B/6C's scaling decisions get made from, so they're real
//! numbers recorded against the 10-region baseline rather than estimates -
//! per-system tick timings, total wall time for a full run, `Observation::
//! encode()`'s length and generation time, and an approximate peak memory
//! figure. Runs instead of the normal day-by-day loop and exits.

use std::time::{Duration, Instant};

use archipelago_sim::agent::Agent;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation, StepTimings};
use archipelago_sim::world::World;

use crate::cli::Args;

/// How many extra `Observation::encode()` calls to time, after the main
/// run, to get a stable average - one call is far too fast (sub-microsecond
/// for the 10-region map) to time meaningfully on its own.
const ENCODE_SAMPLES: u32 = 2_000;

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 {
        format!("{secs:.3} s")
    } else if secs >= 0.001 {
        format!("{:.3} ms", secs * 1_000.0)
    } else {
        format!("{:.3} µs", secs * 1_000_000.0)
    }
}

/// Approximate peak resident set size for this process, in bytes - Linux
/// only (`/proc/self/status`'s `VmHWM`, "high water mark"), read directly
/// with `std::fs` rather than any external crate. `None` on any other
/// platform, or if `/proc` isn't readable for some other reason - the
/// report prints "unavailable" rather than a fabricated number.
fn peak_memory_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kib * 1024);
        }
    }
    None
}

/// Runs `args.days` simulated days on `world` with a plain `HeuristicAgent`
/// per faction (the same default `apps/headless` itself uses), printing a
/// timing/memory report and returning without ever writing `--json` or the
/// normal console log - `--bench` is its own mode, not a modifier on the
/// regular run.
pub fn run(args: &Args, world: World) {
    let region_count = world.regions.len();
    let sea_zone_count = world.sea_zones.len();
    let faction_count = world.factions.len();

    let mut sim = Simulation::with_world(world, args.seed);
    let mut agents: Vec<Box<dyn Agent>> =
        (0..faction_count).map(|i| Box::new(archipelago_agents::default_heuristic_agent(i)) as Box<dyn Agent>).collect();

    let mut totals = StepTimings::default();
    let mut ticks: u32 = 0;
    let wall_start = Instant::now();

    loop {
        if sim.outcome(args.days) != Outcome::Ongoing {
            break;
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
        let (_events, timings) = sim.step_timed();
        totals.economy += timings.economy;
        totals.logistics += timings.logistics;
        totals.military += timings.military;
        totals.politics += timings.politics;
        ticks += 1;
    }

    let wall_total = wall_start.elapsed();

    // `Observation::encode()`: length is exact (it's the same call the RL
    // API and the day-by-day loop both use); generation time is averaged
    // over `ENCODE_SAMPLES` calls on the post-run state so a single call's
    // sub-microsecond duration doesn't get lost in timer-resolution noise.
    let obs = Observation { faction: FactionId(0), world: &sim.world };
    let encoded_len = obs.encode().len();
    let encode_start = Instant::now();
    for _ in 0..ENCODE_SAMPLES {
        std::hint::black_box(obs.encode());
    }
    let encode_avg = encode_start.elapsed() / ENCODE_SAMPLES;

    println!("=== archipelago-headless --bench ===");
    println!("scenario: regions={region_count} sea_zones={sea_zone_count} factions={faction_count}");
    println!("seed={} days_requested={} days_run={ticks}", args.seed, args.days);
    println!();
    println!("per-system average time per tick (over {ticks} ticks):");
    let per_tick = |d: Duration| if ticks == 0 { Duration::ZERO } else { d / ticks };
    println!("  economy:   {}", fmt_duration(per_tick(totals.economy)));
    println!("  logistics: {}", fmt_duration(per_tick(totals.logistics)));
    println!("  military:  {}", fmt_duration(per_tick(totals.military)));
    println!("  politics:  {}", fmt_duration(per_tick(totals.politics)));
    println!("  sum:       {}", fmt_duration(per_tick(totals.total())));
    println!();
    println!("total wall time ({ticks} days, agent decisions + Simulation::apply + Simulation::step): {}", fmt_duration(wall_total));
    if ticks > 0 {
        println!("  average per day: {}", fmt_duration(wall_total / ticks));
    }
    println!();
    println!("Observation::encode(): length={encoded_len} floats, average generation time over {ENCODE_SAMPLES} calls = {}", fmt_duration(encode_avg));
    println!();
    match peak_memory_bytes() {
        Some(bytes) => println!("peak memory (VmHWM, /proc/self/status, approximate): {:.1} MB", bytes as f64 / (1024.0 * 1024.0)),
        None => println!("peak memory: unavailable (no /proc/self/status on this platform)"),
    }
}
