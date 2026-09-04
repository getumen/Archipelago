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
//!
//! Stage 7B (docs/phase7-spec.md "Stage 7B — 遊ぶ") adds `Controller`: each
//! faction is driven by exactly one of an AI (`archipelago_agents::
//! HeuristicAgent`), a live human (`archipelago_agents::HumanAgent`, fed by
//! `apps/game`'s input systems via `SimDriver::push_human_action`), or a
//! `ReplayAgent` (fed from a previously recorded action list, `--replay`).
//! At most one faction is ever `Human`/`Replay` - `--play <faction>` picks
//! which. Whichever it is, `tick` applies its `decide()` output through
//! `Simulation::apply` exactly like every AI faction's - no separate code
//! path, per docs/design.md §14's "人間も AI と同じ入口から世界に触る".

use archipelago_agents::HumanAgent;
use archipelago_sim::action::{Action, ActionError};
use archipelago_sim::agent::Agent;
use archipelago_sim::event::Event;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::World;

/// A `Vec<Action>` per day, replayed back in order - the in-memory shape of
/// a `--record` file once `crate::action_codec::read_record` has parsed it.
/// `decide()` never invents anything beyond what's here: running past the
/// end of the list (a replay driven for more days than were recorded) just
/// returns an empty `Vec` for every subsequent day, rather than panicking -
/// harmless in practice since a replay of a *complete* recording always
/// stops on the same day the original run did (same seed, same actions each
/// day => byte-identical evolution => identical `Outcome`), so this only
/// ever matters for a deliberately-truncated recording.
struct ReplayAgent {
    days: Vec<Vec<Action>>,
    cursor: usize,
}

impl ReplayAgent {
    fn new(days: Vec<Vec<Action>>) -> Self {
        ReplayAgent { days, cursor: 0 }
    }
}

impl Agent for ReplayAgent {
    fn name(&self) -> &str {
        "ReplayAgent"
    }

    fn decide(&mut self, _obs: &Observation) -> Vec<Action> {
        let actions = self.days.get(self.cursor).cloned().unwrap_or_default();
        self.cursor += 1;
        actions
    }
}

/// Which kind of `Agent` drives one faction's slot in `SimDriver::
/// controllers` - see this module's own doc for why exactly one of these
/// may ever be `Human`/`Replay`.
enum Controller {
    Ai(Box<dyn Agent + Send + Sync>),
    Human(HumanAgent),
    Replay(ReplayAgent),
}

/// Owns the `Simulation` plus one `Controller` per faction.
pub struct SimDriver {
    pub sim: Simulation,
    controllers: Vec<Controller>,
    /// `Some(i)` iff `controllers[i]` is `Human` or `Replay` - i.e. iff
    /// `--play` named a faction. At most one index, ever.
    human_index: Option<usize>,
    /// What the human/replay controller's own `decide()` returned on the
    /// most recent `tick()` - empty when there is no human/replay
    /// controller, or that faction wasn't alive this tick.
    /// `crate::app::sim_control::advance_simulation` reads this after every
    /// tick to grow the `--record` buffer with exactly what was actually
    /// applied that day, never a UI-side guess at it.
    last_human_actions: Vec<Action>,
    /// `Simulation::apply`'s rejections for the human/replay faction's
    /// actions on the most recent `tick()` - the UI reads this to surface
    /// *why* an order didn't happen (docs/phase7-spec.md "命令の可否を隠さ
    /// ない"). Not index-aligned with `last_human_actions` (`Simulation::
    /// apply` itself only ever returns the list of errors, not a per-action
    /// `Result`) - in practice the player almost always queues one action
    /// per paused day, so this is unambiguous where it matters.
    last_human_errors: Vec<ActionError>,
}

impl SimDriver {
    /// `world` is assumed already valid (built via `archipelago_sim::
    /// scenario::build_world`/`load_str`/`load_file`, which validate before
    /// building) - this never re-validates, matching every other
    /// `Simulation::with_world` caller in the workspace.
    ///
    /// Every faction is `archipelago_agents::default_heuristic_agent` -
    /// observing-only, unchanged from Stage 7A. Use `new_with_player` for a
    /// human-controlled faction.
    pub fn new(world: World, seed: u64) -> Self {
        Self::new_with_player(world, seed, None, None)
    }

    /// As `new`, but `player`, if given, is driven by a `HumanAgent` (fed
    /// via `push_human_action`) - or, if `replay` is also given, by a
    /// `ReplayAgent` fed from that pre-parsed `--replay` recording instead,
    /// with no live input accepted for that faction at all. Every other
    /// faction is `HeuristicAgent`, exactly as `new`.
    pub fn new_with_player(world: World, seed: u64, player: Option<FactionId>, replay: Option<Vec<Vec<Action>>>) -> Self {
        let sim = Simulation::with_world(world, seed);
        let n = sim.world.factions.len();
        let mut controllers = Vec::with_capacity(n);
        let mut human_index = None;
        for i in 0..n {
            let faction = FactionId(i as u32);
            let controller = if Some(faction) == player {
                human_index = Some(i);
                match &replay {
                    Some(days) => Controller::Replay(ReplayAgent::new(days.clone())),
                    None => Controller::Human(HumanAgent::new(faction)),
                }
            } else {
                Controller::Ai(Box::new(archipelago_agents::default_heuristic_agent(i)) as Box<dyn Agent + Send + Sync>)
            };
            controllers.push(controller);
        }
        SimDriver { sim, controllers, human_index, last_human_actions: Vec::new(), last_human_errors: Vec::new() }
    }

    /// The `--play`ed faction, if any.
    pub fn human_faction(&self) -> Option<FactionId> {
        self.human_index.map(|i| FactionId(i as u32))
    }

    /// Queues one action for the human player's next `decide()` call - a
    /// no-op when there's no live `HumanAgent` controller (observing-only
    /// mode, or a `--replay` run, where nothing the UI does can reach the
    /// simulation - by construction, not by a check anything could get
    /// wrong: there is simply no `Action` sink to write into).
    pub fn push_human_action(&mut self, action: Action) {
        if let Some(i) = self.human_index
            && let Controller::Human(agent) = &mut self.controllers[i]
        {
            agent.push(action);
        }
    }

    /// See this struct's own field doc.
    pub fn last_human_actions(&self) -> &[Action] {
        &self.last_human_actions
    }

    /// See this struct's own field doc.
    pub fn last_human_errors(&self) -> &[ActionError] {
        &self.last_human_errors
    }

    /// Advances the simulation by exactly one day - see this module's own
    /// doc for why this signature takes nothing timing-related at all.
    /// Mirrors `apps/headless/src/main.rs`'s per-day loop body exactly
    /// (decide+apply for every living faction in ascending `FactionId`
    /// order, then one `Simulation::step`).
    pub fn tick(&mut self) -> Vec<Event> {
        self.last_human_actions.clear();
        self.last_human_errors.clear();
        for f_idx in 0..self.sim.world.factions.len() {
            let faction = FactionId(f_idx as u32);
            if !self.sim.world.factions[f_idx].alive {
                continue;
            }
            let obs = Observation { faction, world: &self.sim.world };
            let actions = match &mut self.controllers[f_idx] {
                Controller::Ai(agent) => agent.decide(&obs),
                Controller::Human(agent) => agent.decide(&obs),
                Controller::Replay(agent) => agent.decide(&obs),
            };
            let is_human_slot = Some(f_idx) == self.human_index;
            let errors = self.sim.apply(faction, &actions);
            if is_human_slot {
                self.last_human_actions = actions;
                self.last_human_errors = errors;
            }
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

    /// Stage 7B's determinism regression guard (docs/phase7-spec.md "Stage
    /// 7B の受け入れ基準": "記録した行動列の再生がバイト単位で同じ最終状態
    /// になる"). Plays a short, scripted "human" session against faction 0
    /// (a legal move, a legal policy change, and a deliberately invalid
    /// policy value - so a *rejected* order is exercised too, since
    /// `--record` must capture exactly what was asked for, not just what
    /// succeeded), round-trips the recording through the actual
    /// `crate::action_codec` file format (not just the in-memory
    /// `Vec<Vec<Action>>`, so a serialization bug would fail this test
    /// too), then replays it into a fresh `SimDriver` with no live input at
    /// all and checks the final `World` matches byte-for-byte.
    ///
    /// Confirmed this can actually fail: temporarily made `ReplayAgent::
    /// decide` return `Vec::new()` unconditionally (i.e. "replay" that
    /// silently drops every recorded action) and re-ran - the final
    /// `World` `Debug` snapshot no longer matched (the day-3 `HoldUnit` and
    /// day-10 `SetConscription` never took effect, so the two runs'
    /// `Faction::conscription` fields alone already differed). Reverted
    /// before committing.
    #[test]
    fn recorded_play_replays_identically() {
        const SEED: u64 = 1;
        const DAYS: u32 = 60;
        let player = FactionId(0);

        let mut driver = SimDriver::new_with_player(scenario::build_world(), SEED, Some(player), None);
        let mut recorded: Vec<Vec<Action>> = Vec::new();
        for day in 0..DAYS {
            if driver.outcome(DAYS) != Outcome::Ongoing {
                break;
            }
            if day == 3 {
                let unit = driver
                    .sim
                    .world
                    .units
                    .iter()
                    .find(|u| u.owner == player && u.alive)
                    .map(|u| u.id)
                    .expect("faction 0 starts with at least one living unit");
                driver.push_human_action(Action::HoldUnit { unit });
            }
            if day == 10 {
                driver.push_human_action(Action::SetConscription(0.4));
            }
            if day == 20 {
                // Deliberately invalid regardless of scenario data:
                // conscription must be in 0.0..=1.0 - proves a *rejected*
                // order still gets recorded and replayed identically, not
                // silently dropped before it ever reaches the recording.
                driver.push_human_action(Action::SetConscription(5.0));
            }
            driver.tick();
            recorded.push(driver.last_human_actions().to_vec());
        }
        let final_day = driver.sim.world.day;
        let final_state = format!("{:?}", driver.sim.world);
        assert_ne!(final_day, 0, "the scripted run must have actually played");
        assert!(recorded.iter().any(|day| !day.is_empty()), "the scripted run must have actually queued at least one action");

        // Round-trip through the real `--record`/`--replay` file format,
        // not just the in-memory `Vec<Vec<Action>>`.
        let path = std::env::temp_dir().join(format!("archipelago-game-record-test-{}.json", std::process::id()));
        crate::action_codec::write_record(&path, &recorded).expect("write recording");
        let replayed_days = crate::action_codec::read_record(&path).expect("read recording");
        let _ = std::fs::remove_file(&path);
        assert_eq!(replayed_days, recorded, "round-tripping through the record file must reproduce the exact same actions");

        // A fresh SimDriver, same seed and scenario, with no live input at
        // all - every action comes from `replayed_days`.
        let mut replay_driver = SimDriver::new_with_player(scenario::build_world(), SEED, Some(player), Some(replayed_days));
        for _ in 0..DAYS {
            if replay_driver.outcome(DAYS) != Outcome::Ongoing {
                break;
            }
            replay_driver.tick();
        }

        assert_eq!(replay_driver.sim.world.day, final_day, "the replay must stop on the exact same day as the original run");
        assert_eq!(
            format!("{:?}", replay_driver.sim.world),
            final_state,
            "the replay must reach byte-identical final state to the original recorded run"
        );
    }
}
