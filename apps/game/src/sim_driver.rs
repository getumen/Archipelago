//! Drives an `archipelago_sim::sim::Simulation` one day at a time. No
//! `bevy` import anywhere in this file - this is the "sim-driving logic"
//! docs/phase7-spec.md's most important invariant (§0 "描画がシミュレーシ
//! ョンを変えてはならない") demands be separable from the rendering systems,
//! so `client_run_matches_headless` can call `SimDriver::tick` directly, in
//! a plain `#[test]`, with no window and no Bevy `App` at all.
//!
//! THE INVARIANT: one call to `tick` is always exactly one simulated day -
//! `Simulation::apply` for every living faction's `Agent::decide` output,
//! then exactly one `Simulation::step`. Nothing in this file ever reads a
//! frame's wall-clock delta or any other timing source; the Bevy side
//! (`crate::app::sim_control`) decides *how many times per frame* to call
//! `tick` (0 while paused, `Speed::ticks_per_frame()` otherwise) but never
//! changes what one call does. This is what keeps a run's outcome a pure
//! function of (scenario, seed, player actions) regardless of frame rate.

use archipelago_sim::agent::Agent;
use archipelago_sim::event::Event;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::World;

/// Owns the `Simulation` plus one `Agent` per faction. Stage 7A never
/// installs a `HumanAgent` (that's Stage 7B, docs/phase7-spec.md "Stage
/// 7B — 遊ぶ"): every faction here is `archipelago_agents::
/// default_heuristic_agent`, the same default `apps/headless --agent
/// heuristic` (no flags) uses - so a Stage 7A run with the player never
/// touching the keyboard/mouse is, faction for faction, the same AI-vs-AI
/// game headless would produce for the same seed and scenario.
pub struct SimDriver {
    pub sim: Simulation,
    agents: Vec<Box<dyn Agent + Send + Sync>>,
}

impl SimDriver {
    /// `world` is assumed already valid (built via `archipelago_sim::
    /// scenario::build_world`/`load_str`/`load_file`, which validate before
    /// building) - this never re-validates, matching every other
    /// `Simulation::with_world` caller in the workspace.
    pub fn new(world: World, seed: u64) -> Self {
        let sim = Simulation::with_world(world, seed);
        let agents: Vec<Box<dyn Agent + Send + Sync>> = (0..sim.world.factions.len())
            .map(|i| Box::new(archipelago_agents::default_heuristic_agent(i)) as Box<dyn Agent + Send + Sync>)
            .collect();
        SimDriver { sim, agents }
    }

    /// Advances the simulation by exactly one day - see this module's own
    /// doc for why this signature takes nothing timing-related at all.
    /// Mirrors `apps/headless/src/main.rs`'s per-day loop body exactly
    /// (decide+apply for every living faction in ascending `FactionId`
    /// order, then one `Simulation::step`).
    pub fn tick(&mut self) -> Vec<Event> {
        for f_idx in 0..self.sim.world.factions.len() {
            let faction = FactionId(f_idx as u32);
            if !self.sim.world.factions[f_idx].alive {
                continue;
            }
            let obs = Observation { faction, world: &self.sim.world };
            let actions = self.agents[f_idx].decide(&obs);
            self.sim.apply(faction, &actions);
        }
        self.sim.step()
    }

    pub fn world(&self) -> &World {
        &self.sim.world
    }

    pub fn outcome(&self, max_days: u32) -> Outcome {
        self.sim.outcome(max_days)
    }
}

/// How many simulated days one call to `advance` should run - "1 フレーム
/// あたり何 tick 進めるか" (docs/phase7-spec.md §0), never anything about
/// how *fast* those days play out. `Paused` is its own variant rather than
/// `X1` with a separate `bool` so "no ticks this frame" can never be
/// confused with "one tick, running at the slowest speed" by anything that
/// matches on this type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    Paused,
    X1,
    X5,
    X20,
}

impl Speed {
    pub fn ticks_per_frame(self) -> u32 {
        match self {
            Speed::Paused => 0,
            Speed::X1 => 1,
            Speed::X5 => 5,
            Speed::X20 => 20,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Speed::Paused => "stopped",
            Speed::X1 => "1x",
            Speed::X5 => "5x",
            Speed::X20 => "20x",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::scenario;

    /// `client_run_matches_headless` (docs/phase7-spec.md "Stage 7A の受け
    /// 入れ基準"): driving `SimDriver` for seed 1 with a 720-day cap and no
    /// player input must reach the exact same final `World` (and stop on
    /// the exact same day) as the textbook headless loop
    /// (`apps/headless/src/main.rs`'s own per-day body,
    /// reproduced verbatim below rather than imported - `apps/headless` is
    /// a binary-only crate with no library target, and duplicating its ~10
    /// line loop here is far cheaper than adding one). This is the
    /// regression guard for the project's central invariant: nothing about
    /// *how* `apps/game` drives the simulation (frame pacing, speed
    /// control, event handling) may change *what* a tick does.
    ///
    /// Confirmed this can actually fail: temporarily changed `SimDriver::
    /// tick` to call `self.sim.step()` before applying actions (agents
    /// would then be reacting to tomorrow's board with today's date) and
    /// re-ran this test - it failed immediately with a `World` `Debug`
    /// mismatch. Reverted before committing.
    ///
    /// Independently cross-checked against the actual `archipelago-headless`
    /// binary outside this test (not shelled out to here, to keep this test
    /// hermetic): `cargo run --release -p archipelago-headless -- --seed 1
    /// --days 720 --json --agent heuristic | sha256sum` reproduces
    /// docs/phase7-spec.md's documented
    /// `0138bf5d537417128e737d3fb68ff55591b8c5dc96e7f382cedaec3b63ffb714`
    /// unchanged by every edit this stage made (the `position` field
    /// included), which is exactly the state this test's `reference` loop
    /// below is built from the same public API to reproduce in-process.
    #[test]
    fn client_run_matches_headless() {
        const SEED: u64 = 1;
        const DAYS: u32 = 720;

        // The reference: `apps/headless/src/main.rs`'s loop body, faithfully
        // reproduced - build the embedded default scenario, one default
        // `HeuristicAgent` per faction, decide+apply+step until `outcome`
        // stops being `Ongoing`.
        let mut reference_sim = Simulation::with_world(scenario::build_world(), SEED);
        let mut reference_agents: Vec<Box<dyn Agent + Send + Sync>> = (0..reference_sim.world.factions.len())
            .map(|i| Box::new(archipelago_agents::default_heuristic_agent(i)) as Box<dyn Agent + Send + Sync>)
            .collect();
        loop {
            if reference_sim.outcome(DAYS) != Outcome::Ongoing {
                break;
            }
            for f_idx in 0..reference_sim.world.factions.len() {
                let faction = FactionId(f_idx as u32);
                if !reference_sim.world.factions[f_idx].alive {
                    continue;
                }
                let obs = Observation { faction, world: &reference_sim.world };
                let actions = reference_agents[f_idx].decide(&obs);
                reference_sim.apply(faction, &actions);
            }
            reference_sim.step();
        }

        // The client's own sim-driving logic - no window, no Bevy `App`,
        // just `SimDriver::tick` called directly, exactly as
        // `crate::app::sim_control::advance_simulation` will call it once
        // per unpaused frame.
        let mut driver = SimDriver::new(scenario::build_world(), SEED);
        while driver.outcome(DAYS) == Outcome::Ongoing {
            driver.tick();
        }

        // seed 1's mvp run actually ends in an early `Outcome::Victory`
        // (day 366, faction 0) rather than running the full 720 days to a
        // `Stalemate` - confirmed independently via `cargo run --release -p
        // archipelago-headless -- --seed 1 --days 720 --json --agent
        // heuristic`, whose `"day"` field reads `366`. Both loops above stop
        // the instant `outcome(DAYS)` leaves `Outcome::Ongoing`, so the
        // right assertion is "both loops stopped on the same day", not "both
        // reached day 720" - a hardcoded `DAYS` here would have made this
        // test pass vacuously true only by coincidence if the outcome had
        // instead been a day-720 `Stalemate`.
        assert_ne!(reference_sim.world.day, 0, "the reference loop must have actually run");
        assert_eq!(
            reference_sim.world.day, driver.sim.world.day,
            "the client driver must stop on the exact same day as the reference loop"
        );
        assert_eq!(
            format!("{:?}", reference_sim.world),
            format!("{:?}", driver.sim.world),
            "the client's sim-driving loop must reach byte-identical final state to headless's own loop"
        );
    }

    #[test]
    fn speed_never_changes_tick_count_only_pace() {
        // `Speed` only ever describes ticks-per-frame; it must never be
        // capable of expressing "run for N seconds" or anything else that
        // would let wall-clock time leak into the simulation.
        assert_eq!(Speed::Paused.ticks_per_frame(), 0);
        assert_eq!(Speed::X1.ticks_per_frame(), 1);
        assert_eq!(Speed::X5.ticks_per_frame(), 5);
        assert_eq!(Speed::X20.ticks_per_frame(), 20);
    }
}
