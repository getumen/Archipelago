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
//!
//! **Military delegation** (docs/design.md §14): `delegate_unit`/
//! `undelegate_unit`/`is_delegated` (per-unit) and `delegate_military`/
//! `undelegate_military`/`is_military_delegated` (the whole `Layer::
//! Military` decision domain, recruitment included - the operation
//! `--delegate-military` actually performs, see `main.rs`'s own doc) all
//! forward straight to the live `HumanAgent` controller (a no-op / `false`
//! for every other `Controller` variant, same shape as
//! `push_human_action`). Nothing here needs to know *how* a delegated unit
//! gets ordered or how large the delegated army grows - `HumanAgent::
//! decide` already folds those orders into the exact same `Vec<Action>` a
//! player's own queued orders come back in, so this file's own invariant
//! above (`tick` just applies whatever `decide()` returns) already covers
//! it, and so does `--record`/`--replay`: a delegated faction's AI-issued
//! orders (including its own `RecruitUnit`s) land in `last_human_actions`
//! like any other action, get written to the recording, and a `--replay`
//! run reproduces them from that recording via `ReplayAgent` with no
//! delegation-specific replay logic at all - see
//! `tests::delegated_play_replays_identically`.
//!
//! **Layer-scoped replay** generalizes that same idea to `--replay` itself:
//! a recording can declare, via `Replay::layers`, which `Layer`s it drives
//! for the played faction - a scripted economy/diplomacy with the AI
//! fighting the war, say - and `build_replay_controller` hands every layer
//! it leaves out to a fresh `archipelago_agents::default_heuristic_agent`,
//! composed through `archipelago_agents::CompositeAgent`. This is the exact
//! same routing primitive `HumanAgent`'s whole-military delegation already
//! uses (one caller passes `[Layer::Military]` to a live `HumanAgent`,
//! the other passes an arbitrary `Layer` subset to a `ReplayAgent`) - not a
//! second, unrelated mechanism invented alongside it. Because of that,
//! `--delegate-military` and `--replay` are no longer combined at all
//! (`main.rs`'s own doc): a replay's own declared scope is now how a
//! scripted faction hands `Layer::Military` to the AI. A plain, unscoped
//! recording (`ALL_LAYERS`, everything `--record` has ever produced) is
//! this mechanism's degenerate case, not a separate code path - see
//! `build_replay_controller`'s own doc for why that reproduces today's
//! full-scope `--replay` byte-for-byte.

use archipelago_agents::{CompositeAgent, HumanAgent};
use archipelago_sim::action::{Action, ActionError, Layer, ALL_LAYERS};
use archipelago_sim::agent::Agent;
use archipelago_sim::event::Event;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::World;

/// A `Vec<Action>` per day, replayed back in order - the in-memory shape of
/// a `--record` file once `crate::action_codec::read_record`/`read_replay`
/// has parsed it. `decide()` never invents anything beyond what's here:
/// running past the end of the list (a replay driven for more days than
/// were recorded) just returns an empty `Vec` for every subsequent day,
/// rather than panicking - harmless in practice since a replay of a
/// *complete* recording always stops on the same day the original run did
/// (same seed, same actions each day => byte-identical evolution =>
/// identical `Outcome`), so this only ever matters for a
/// deliberately-truncated recording.
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

/// A `--replay` recording plus which `Layer`s it claims for the played
/// faction (`crate::action_codec::read_replay`'s own doc for the two file
/// shapes this can come from). `layers == ALL_LAYERS` reproduces exactly
/// what `--replay` has always done - the whole faction scripted, nothing
/// left for an AI to decide; anything narrower hands every `Layer` *not*
/// listed here to a fresh `archipelago_agents::default_heuristic_agent` for
/// this same faction (`build_replay_controller`'s own doc), the same
/// `archipelago_agents::CompositeAgent` routing primitive
/// `archipelago_agents::human::HumanAgent`'s own military delegation is
/// built on - not a second, unrelated mechanism.
///
/// `layers` is never inferred from `days` - a day with no diplomacy action
/// in it must stay distinguishable from a replay that never claimed
/// `Layer::Diplomacy` at all, which is exactly the distinction inferring
/// from content could never draw (docs/conventions.md's no-fallback
/// principle).
#[derive(Clone)]
pub struct Replay {
    pub layers: Vec<Layer>,
    pub days: Vec<Vec<Action>>,
}

/// Builds the `Agent` that drives `faction`'s `Controller::Replay` slot:
/// `replay.days` through a `ReplayAgent` claiming exactly `replay.layers`,
/// composed with a fresh `default_heuristic_agent` claiming every layer
/// `replay.layers` left out, via `CompositeAgent` - precisely the routing
/// primitive `HumanAgent`'s whole-military delegation already uses
/// (`archipelago_agents::human`'s own doc), applied here to an arbitrary
/// `Layer` subset instead of `Layer::Military` alone. When `replay.layers`
/// is `ALL_LAYERS` no second route is even added (there is nothing left for
/// it to claim) and the single `CompositeAgent` route around `ReplayAgent`
/// filters nothing out - byte-identical output to a bare `ReplayAgent`, so
/// today's full-scope `--replay` behavior is this function's degenerate
/// case, not a separate code path.
fn build_replay_controller(faction: FactionId, replay: Replay) -> Box<dyn Agent + Send + Sync> {
    let mut composite = CompositeAgent::new(faction).route(replay.layers.clone(), Box::new(ReplayAgent::new(replay.days)));
    let remaining: Vec<Layer> = ALL_LAYERS.into_iter().filter(|l| !replay.layers.contains(l)).collect();
    if !remaining.is_empty() {
        composite = composite.route(remaining, Box::new(archipelago_agents::default_heuristic_agent(faction.index())));
    }
    Box::new(composite)
}

/// Which kind of `Agent` drives one faction's slot in `SimDriver::
/// controllers` - see this module's own doc for why exactly one of these
/// may ever be `Human`/`Replay`. `Replay` is a boxed trait object rather
/// than a bare `ReplayAgent` because a layer-scoped replay is actually a
/// `CompositeAgent` wrapping one (`build_replay_controller`) - `tick`'s own
/// dispatch only ever needs `Agent::decide`, so nothing here has to know
/// which of the two it actually is.
enum Controller {
    Ai(Box<dyn Agent + Send + Sync>),
    Human(HumanAgent),
    Replay(Box<dyn Agent + Send + Sync>),
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
    /// The human/replay faction's own rejected actions from the most recent
    /// `tick()`, each paired with *which* `Action` it was - the panel UI
    /// (Stage 8B, owner ask "パネルUI") reads this to attach a rejection's
    /// reason to the specific panel the order was issued from (region/unit/
    /// policy/diplomacy), not only a single undifferentiated corner
    /// (docs/phase7-spec.md "命令の可否を隠さない"). Built by replaying this
    /// faction's actions one at a time through `action::apply_action`
    /// directly (`tick`'s own doc) instead of the single batched
    /// `Simulation::apply(faction, &actions)` every other faction still
    /// uses - `Simulation::apply` itself only ever returns the flat list of
    /// errors with no per-action correlation, and `crates/sim` stays
    /// untouched (docs/conventions.md's constraint for this task), so the
    /// pairing has to happen here instead. Calling `action::apply_action` in
    /// the same order for the same faction produces bit-identical `World`
    /// mutations to what `Simulation::apply`'s own internal loop would have
    /// done (it's the same function, called the same number of times in the
    /// same order) - this changes nothing about *what* happens, only what
    /// this struct remembers about it afterward.
    last_human_action_errors: Vec<(Action, ActionError)>,
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
    /// via `push_human_action`) - or, if `replay` is also given, by
    /// `build_replay_controller(player, replay)` instead: `replay.days`
    /// fed back for exactly `replay.layers`, every other layer decided by a
    /// fresh AI, with no live input accepted for this faction at all (see
    /// `Replay`'s own doc). Every other faction is `HeuristicAgent`,
    /// exactly as `new`.
    pub fn new_with_player(world: World, seed: u64, player: Option<FactionId>, replay: Option<Replay>) -> Self {
        let sim = Simulation::with_world(world, seed);
        let n = sim.world.factions.len();
        let mut controllers = Vec::with_capacity(n);
        let mut human_index = None;
        let mut replay = replay;
        for i in 0..n {
            let faction = FactionId(i as u32);
            let controller = if Some(faction) == player {
                human_index = Some(i);
                match replay.take() {
                    Some(r) => Controller::Replay(build_replay_controller(faction, r)),
                    None => Controller::Human(HumanAgent::new(faction)),
                }
            } else {
                Controller::Ai(Box::new(archipelago_agents::default_heuristic_agent(i)) as Box<dyn Agent + Send + Sync>)
            };
            controllers.push(controller);
        }
        SimDriver { sim, controllers, human_index, last_human_actions: Vec::new(), last_human_action_errors: Vec::new() }
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

    /// Hands `unit`'s day-to-day orders to the same `HeuristicAgent` logic
    /// the AI factions run (`archipelago_agents::HumanAgent::delegate`'s own
    /// doc) - a no-op with no live `HumanAgent` controller, mirroring
    /// `push_human_action`'s own doc for why (observing-only mode, or a
    /// `--replay` run with a `ReplayAgent` in this slot instead).
    pub fn delegate_unit(&mut self, unit: archipelago_sim::ids::UnitId) {
        if let Some(i) = self.human_index
            && let Controller::Human(agent) = &mut self.controllers[i]
        {
            agent.delegate(unit);
        }
    }

    /// Takes `unit` back under direct player control - see `delegate_unit`
    /// and `archipelago_agents::HumanAgent::undelegate`.
    pub fn undelegate_unit(&mut self, unit: archipelago_sim::ids::UnitId) {
        if let Some(i) = self.human_index
            && let Controller::Human(agent) = &mut self.controllers[i]
        {
            agent.undelegate(unit);
        }
    }

    /// Hands the entire `Layer::Military` decision domain - every unit's
    /// orders *and* recruitment - to the same `HeuristicAgent` logic the AI
    /// factions run (`archipelago_agents::HumanAgent::delegate_military`'s
    /// own doc: this, not a per-unit loop, is what "the AI runs the war"
    /// actually means, since recruitment has no existing unit a per-unit
    /// call could ever name). A no-op with no live `HumanAgent` controller,
    /// mirroring `push_human_action`'s own doc for why.
    pub fn delegate_military(&mut self) {
        if let Some(i) = self.human_index
            && let Controller::Human(agent) = &mut self.controllers[i]
        {
            agent.delegate_military();
        }
    }

    /// Takes the whole military back under direct player control - see
    /// `delegate_military` and `archipelago_agents::HumanAgent::undelegate_military`.
    pub fn undelegate_military(&mut self) {
        if let Some(i) = self.human_index
            && let Controller::Human(agent) = &mut self.controllers[i]
        {
            agent.undelegate_military();
        }
    }

    /// Whether the entire military is currently delegated - `false` with no
    /// live `HumanAgent` controller. Read by `apps/game`'s policy panel to
    /// show "AI runs the war" state distinctly from individual delegated
    /// units.
    pub fn is_military_delegated(&self) -> bool {
        match self.human_index.map(|i| &self.controllers[i]) {
            Some(Controller::Human(agent)) => agent.is_military_delegated(),
            _ => false,
        }
    }

    /// Whether `unit` is currently delegated - `false` with no live
    /// `HumanAgent` controller (nothing can be delegated at all in that
    /// case). Read by `apps/game`'s unit panel/map visuals to mark
    /// AI-controlled units.
    pub fn is_delegated(&self, unit: archipelago_sim::ids::UnitId) -> bool {
        match self.human_index.map(|i| &self.controllers[i]) {
            Some(Controller::Human(agent)) => agent.is_delegated(unit),
            _ => false,
        }
    }

    /// See this struct's own field doc.
    pub fn last_human_actions(&self) -> &[Action] {
        &self.last_human_actions
    }

    /// See `last_human_action_errors`'s own field doc.
    pub fn last_human_action_errors(&self) -> &[(Action, ActionError)] {
        &self.last_human_action_errors
    }

    /// Advances the simulation by exactly one day - see this module's own
    /// doc for why this signature takes nothing timing-related at all.
    /// Mirrors `apps/headless/src/main.rs`'s per-day loop body exactly
    /// (decide+apply for every living faction in ascending `FactionId`
    /// order, then one `Simulation::step`).
    pub fn tick(&mut self) -> Vec<Event> {
        self.last_human_actions.clear();
        self.last_human_action_errors.clear();
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
            if is_human_slot {
                // Applied one action at a time (via the same `action::
                // apply_action` `Simulation::apply` calls internally,
                // `last_human_action_errors`'s own doc) instead of through
                // `Simulation::apply`'s batch form, purely to capture which
                // action produced which error - identical `World` mutations
                // either way.
                self.last_human_action_errors = actions
                    .iter()
                    .cloned()
                    .filter_map(|act| archipelago_sim::action::apply_action(&mut self.sim.world, faction, act.clone()).err().map(|e| (act, e)))
                    .collect();
                self.last_human_actions = actions;
            } else {
                self.sim.apply(faction, &actions);
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
        let mut replay_driver = SimDriver::new_with_player(scenario::build_world(), SEED, Some(player), Some(Replay { layers: ALL_LAYERS.to_vec(), days: replayed_days }));
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

    /// Military delegation's own determinism guard, alongside
    /// `recorded_play_replays_identically` above: hands the player
    /// faction's entire military over to `HeuristicAgent` logic via a
    /// single `delegate_military()` call at day 0 - the real player-facing
    /// operation (`--delegate-military`'s own doc in `main.rs`), not a
    /// per-tick re-delegation loop standing in for it - and no other player
    /// input at all (the point of delegation is to let the AI run the whole
    /// army, recruitment included). Records the resulting play and checks a
    /// fresh `SimDriver` replaying that recording (with **no** delegation
    /// call made against it at all - `delegate_military`'s own doc says the
    /// recorded `Action`s are all a replay ever needs) reaches byte-identical
    /// final state.
    ///
    /// Confirmed this can actually fail: temporarily made `HumanAgent::
    /// decide` skip appending the filtered `military` actions (returning
    /// only the drained queue, as it did before delegation existed) and
    /// re-ran - `recorded` came back with every day empty (nothing was
    /// ever delegated *and* pushed by hand in this test), so the driver
    /// never issued a single order for its own units all game and the
    /// final `World` differed sharply from a real delegated run (far fewer
    /// regions/units owned by the player faction at the end). Reverted
    /// before committing.
    #[test]
    fn delegated_play_replays_identically() {
        const SEED: u64 = 1;
        const DAYS: u32 = 120;
        let player = FactionId(0);

        let mut driver = SimDriver::new_with_player(scenario::build_world(), SEED, Some(player), None);
        let starting_units: Vec<archipelago_sim::ids::UnitId> =
            driver.sim.world.units.iter().filter(|u| u.owner == player && u.alive).map(|u| u.id).collect();
        assert!(!starting_units.is_empty(), "faction 0 must start with at least one living unit to delegate");

        driver.delegate_military();
        assert!(driver.is_military_delegated(), "delegate_military must be reflected by is_military_delegated immediately");
        for &unit in &starting_units {
            assert!(driver.is_delegated(unit), "delegate_military must delegate every existing unit, not just future ones");
        }

        let mut recorded: Vec<Vec<Action>> = Vec::new();
        for _ in 0..DAYS {
            if driver.outcome(DAYS) != Outcome::Ongoing {
                break;
            }
            // No further delegation calls of any kind below - a freshly
            // recruited unit must already be covered by the single
            // `delegate_military()` call above, or this test can't tell
            // the whole-layer fix apart from the old per-unit-only
            // mechanism it replaced.
            driver.tick();
            recorded.push(driver.last_human_actions().to_vec());
        }
        let final_day = driver.sim.world.day;
        let final_state = format!("{:?}", driver.sim.world);
        assert_ne!(final_day, 0, "the delegated run must have actually played");
        assert!(
            recorded.iter().any(|day| !day.is_empty()),
            "a fully-delegated faction must actually receive unit orders over 120 days with no player input at all"
        );
        assert!(
            recorded.iter().any(|day| day.iter().any(|a| matches!(a, Action::RecruitUnit { .. }))),
            "a whole-military-delegated faction must actually recruit new units over 120 days, not just reorder its starting force"
        );

        let path = std::env::temp_dir().join(format!("archipelago-game-delegated-record-test-{}.json", std::process::id()));
        crate::action_codec::write_record(&path, &recorded).expect("write recording");
        let replayed_days = crate::action_codec::read_record(&path).expect("read recording");
        let _ = std::fs::remove_file(&path);
        assert_eq!(replayed_days, recorded, "round-tripping the delegated recording through the record file must reproduce the exact same actions");

        // No `delegate_unit` call anywhere against this driver - replay
        // must reproduce the delegated AI's orders purely from the
        // recorded `Action`s, exactly like any other player action.
        let mut replay_driver = SimDriver::new_with_player(scenario::build_world(), SEED, Some(player), Some(Replay { layers: ALL_LAYERS.to_vec(), days: replayed_days }));
        for _ in 0..DAYS {
            if replay_driver.outcome(DAYS) != Outcome::Ongoing {
                break;
            }
            replay_driver.tick();
        }

        assert_eq!(replay_driver.sim.world.day, final_day, "the delegated replay must stop on the exact same day as the original run");
        assert_eq!(
            format!("{:?}", replay_driver.sim.world),
            final_state,
            "the delegated replay must reach byte-identical final state to the original delegated run"
        );
    }

    /// The per-unit carve-out through `SimDriver` itself - the exact API
    /// surface `apps/game`'s unit panel calls (`app::panels`'s own doc) -
    /// on top of whole-military delegation (`--delegate-military`,
    /// `main.rs`'s own doc): delegating everything and then taking one unit
    /// back must stop that unit's AI orders while every other unit keeps
    /// receiving them, on the exact same tick.
    #[test]
    fn delegate_military_then_taking_one_unit_back_only_stops_that_unit() {
        let player = FactionId(0);
        let mut driver = SimDriver::new_with_player(scenario::build_world(), 1, Some(player), None);

        driver.delegate_military();
        assert!(driver.is_military_delegated());

        let units: Vec<archipelago_sim::ids::UnitId> =
            driver.sim.world.units.iter().filter(|u| u.owner == player && u.alive).map(|u| u.id).collect();
        assert!(units.len() >= 2, "faction 0 must start with at least two living units for this test to distinguish carve-out from the rest");
        let (carved_out, rest) = (units[0], units[1..].to_vec());
        for &u in &units {
            assert!(driver.is_delegated(u), "everything must start delegated once the whole military is");
        }

        driver.undelegate_unit(carved_out);
        assert!(!driver.is_delegated(carved_out), "the carved-out unit must stop being delegated");
        assert!(driver.is_military_delegated(), "carving out one unit must not turn off whole-layer delegation itself");
        for &u in &rest {
            assert!(driver.is_delegated(u), "every other unit must remain delegated");
        }

        driver.tick();
        let orders = driver.last_human_actions();
        assert!(
            orders.iter().all(|a| a.target_unit() != Some(carved_out)),
            "the carved-out unit must receive no AI order this tick: {orders:?}"
        );
        assert!(
            rest.iter().any(|&u| orders.iter().any(|a| a.target_unit() == Some(u))),
            "at least one non-carved-out unit must still receive an AI order this tick: {orders:?}"
        );
    }

    // -------------------------------------------------------------------
    // Layer-scoped replay
    // -------------------------------------------------------------------

    /// The headline property: a `Replay` that claims only `Layer::Economy`
    /// must drive economic policy exactly as scripted, while every other
    /// layer - visibly, not merely by absence of anything from the replay
    /// itself - comes from a fresh AI (`build_replay_controller`'s own
    /// doc). This is the exact CLI-level capability the task exists to add:
    /// a scripted economy with the AI fighting the war.
    ///
    /// Confirmed this can actually fail: temporarily changed
    /// `build_replay_controller` to skip adding the AI route for
    /// `remaining` layers whenever it was non-empty (i.e. a layer-scoped
    /// replay claimed its own layers and left everything else *unrouted*,
    /// `CompositeAgent`'s own "absent layer produces nothing" rule) and
    /// re-ran - the "AI must still act outside the replay's own layer"
    /// assertion below failed immediately, since 30 days produced no
    /// Military/GrandStrategy/Diplomacy action at all. Reverted before
    /// committing.
    #[test]
    fn layer_scoped_replay_drives_only_its_layers() {
        const SEED: u64 = 1;
        const DAYS: u32 = 30;
        let player = FactionId(0);

        let scripted_day0 =
            vec![Action::SetConscription(0.05), Action::SetIndustryPriority { good: archipelago_sim::good::Good::Munitions, weight: 1.0 }];
        let replay = Replay { layers: vec![Layer::Economy], days: vec![scripted_day0.clone()] };

        let mut driver = SimDriver::new_with_player(scenario::build_world(), SEED, Some(player), Some(replay));

        let mut all_recorded: Vec<Vec<Action>> = Vec::new();
        for day in 0..DAYS {
            if driver.outcome(DAYS) != Outcome::Ongoing {
                break;
            }
            driver.tick();
            if day == 0 {
                assert_eq!(
                    driver.last_human_actions().iter().filter(|a| a.layer() == Layer::Economy).cloned().collect::<Vec<_>>(),
                    scripted_day0,
                    "day 0's Economy-layer output must be exactly what the replay scripted, in the order it was scripted"
                );
            }
            all_recorded.push(driver.last_human_actions().to_vec());
        }

        let economy_actions: Vec<&Action> = all_recorded.iter().flatten().filter(|a| a.layer() == Layer::Economy).collect();
        assert_eq!(
            economy_actions,
            scripted_day0.iter().collect::<Vec<_>>(),
            "no Economy-layer action beyond exactly what the replay scripted may ever appear - a layer the replay claims must never also \
             receive AI-produced orders"
        );
        assert!(
            all_recorded.iter().flatten().any(|a| a.layer() != Layer::Economy),
            "a layer the replay never claimed must still visibly receive AI-produced orders over {DAYS} days, not sit empty: {all_recorded:?}"
        );
    }

    /// "No regression": a `Replay` claiming `ALL_LAYERS` - `--replay`'s
    /// default, always-full-scope case before layer scoping existed - must
    /// keep *every* recorded action, across every `Layer`, exactly as bare
    /// `--replay` always has. Deliberately scripts one action per `Layer`
    /// on day 0 so a bug that dropped even a single layer from the
    /// "full scope" wrapping is caught here directly, rather than only
    /// surfacing as a subtler divergence many simulated days later.
    ///
    /// Confirmed this can actually fail: temporarily hardcoded
    /// `build_replay_controller` to route the `ReplayAgent` to
    /// `[Layer::Military, Layer::Economy]` regardless of `replay.layers`
    /// (leaving `Diplomacy`/`GrandStrategy` to the "remaining" AI route
    /// instead of the replay) and re-ran - the final assertion failed
    /// because `last_human_actions` no longer contained the scripted
    /// `SetNationalFocus`/`DeclareWar`. Reverted before committing.
    #[test]
    fn full_scope_replay_keeps_every_layers_actions() {
        let player = FactionId(0);
        let world = scenario::build_world();
        let unit = world.units.iter().find(|u| u.owner == player && u.alive).map(|u| u.id).expect("faction 0 has a living unit");

        let scripted_day0 = vec![
            Action::HoldUnit { unit },
            Action::SetConscription(0.3),
            Action::SetNationalFocus(archipelago_sim::focus::NationalFocus::DefensivePosture),
            Action::DeclareWar { to: FactionId(1) },
        ];
        assert_eq!(
            scripted_day0.iter().map(Action::layer).collect::<std::collections::BTreeSet<_>>(),
            ALL_LAYERS.into_iter().collect::<std::collections::BTreeSet<_>>(),
            "test setup: the scripted day-0 actions must cover every Layer, or this test can't prove full scope keeps all of them"
        );

        let replay = Replay { layers: ALL_LAYERS.to_vec(), days: vec![scripted_day0.clone()] };
        let mut driver = SimDriver::new_with_player(world, 1, Some(player), Some(replay));
        driver.tick();
        assert_eq!(
            driver.last_human_actions(),
            &scripted_day0[..],
            "a full-scope (ALL_LAYERS) replay must keep every recorded action across every layer, unchanged from before layer scoping existed"
        );
    }

    /// A layer-scoped recording, round-tripped through the real
    /// `crate::action_codec::write_scoped_record`/`read_replay` file format
    /// (not just an in-memory `Replay`), must replay to byte-identical
    /// final state - the same determinism guarantee `--replay` has always
    /// made (`recorded_play_replays_identically`'s own doc), now checked
    /// for the layer-scoped file shape.
    ///
    /// Confirmed this can actually fail: temporarily replayed the second
    /// run against `SEED + 1` instead of `SEED` and re-ran - the final
    /// `World` `Debug` snapshot assertion failed immediately (and the
    /// `day_a == day_b` assertion above it failed too), confirming these
    /// aren't vacuously true. Reverted before committing.
    #[test]
    fn layer_scoped_recording_replays_identically() {
        const SEED: u64 = 1;
        const DAYS: u32 = 60;
        let player = FactionId(0);

        let layers = vec![Layer::Economy, Layer::Diplomacy];
        let days: Vec<Vec<Action>> = vec![
            vec![Action::SetConscription(0.2), Action::SetCivilianRation(0.8)],
            vec![],
            vec![Action::SetIndustryPriority { good: archipelago_sim::good::Good::Munitions, weight: 0.7 }],
        ];

        let path = std::env::temp_dir().join(format!("archipelago-game-scoped-record-test-{}.json", std::process::id()));
        crate::action_codec::write_scoped_record(&path, &layers, &days).expect("write scoped recording");
        let (read_layers, read_days) = crate::action_codec::read_replay(&path).expect("read scoped recording");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_layers, layers, "round-tripping a scoped recording through the real file format must preserve its declared layers exactly");
        assert_eq!(read_days, days, "round-tripping a scoped recording through the real file format must preserve its recorded actions exactly");

        fn run(seed: u64, player: FactionId, layers: Vec<Layer>, days: Vec<Vec<Action>>, days_cap: u32) -> (u32, String) {
            let mut driver = SimDriver::new_with_player(scenario::build_world(), seed, Some(player), Some(Replay { layers, days }));
            for _ in 0..days_cap {
                if driver.outcome(days_cap) != Outcome::Ongoing {
                    break;
                }
                driver.tick();
            }
            (driver.sim.world.day, format!("{:?}", driver.sim.world))
        }

        let (day_a, state_a) = run(SEED, player, read_layers.clone(), read_days.clone(), DAYS);
        let (day_b, state_b) = run(SEED, player, read_layers, read_days, DAYS);
        assert_ne!(day_a, 0, "the scoped replay must have actually played");
        assert_eq!(day_a, day_b, "the same seed and the same layer-scoped recording must stop on the same day");
        assert_eq!(state_a, state_b, "the same seed and the same layer-scoped recording must reach byte-identical final World state");
    }
}
