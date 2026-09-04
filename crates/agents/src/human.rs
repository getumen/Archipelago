//! `HumanAgent` (docs/phase7-spec.md "Stage 7B — 遊ぶ", docs/design.md §14):
//! the `Agent` implementation a human player sits behind. Every player
//! subject - human included - goes through the exact same `Agent`
//! interface, so this is deliberately the thinnest possible adapter: a
//! queue that `apps/game`'s input systems push `Action`s onto, and
//! `decide()` drains.
//!
//! **No privileged path.** This struct never sees a `World` or an
//! `Observation` beyond what `decide`'s signature hands it, and never
//! touches one at all - it has no way to check whether a queued action is
//! legal, so it cannot pre-filter anything even by accident. Whatever the
//! UI pushes comes back out of `decide()` unchanged and goes through
//! `Simulation::apply` exactly like any `HeuristicAgent`/`LlmAgent` output -
//! see `tests::human_agent_actions_go_through_validation` for the regression
//! guard on that property.
//!
//! **Bevy-free.** This module (and this whole crate - see
//! `tests::human_agent_is_bevy_free`) imports nothing from `bevy`. The
//! `apps/game` client owns the mouse/keyboard/UI side entirely; it hands
//! this struct finished `Action` values and nothing else, which is what
//! keeps `HumanAgent` usable from a headless/RL/scripted context too (a
//! test harness, a bug-repro script, or a training-data capture tool can
//! drive one exactly like `apps/game` does, with no Bevy `App` anywhere).

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;

pub struct HumanAgent {
    faction: FactionId,
    /// Actions queued since the last `decide()` call, in the order `push`
    /// was called - `decide()` hands them to `Simulation::apply` in that
    /// same order, so a player who (say) sets conscription and then moves a
    /// unit in the same paused day has both applied in that order, matching
    /// what they clicked/typed.
    queue: Vec<Action>,
}

impl HumanAgent {
    pub fn new(faction: FactionId) -> Self {
        HumanAgent { faction, queue: Vec::new() }
    }

    pub fn faction(&self) -> FactionId {
        self.faction
    }

    /// Queues one action for the next `decide()` call. Called only from
    /// `apps/game`'s input systems (map clicks, the order menu, keyboard
    /// policy changes, the diplomacy panel) - this is the only way an
    /// `Action` ever enters the queue, and it is never validated here (see
    /// this module's own doc for why).
    pub fn push(&mut self, action: Action) {
        self.queue.push(action);
    }

    /// What's queued right now, for UI code that wants to show "N 部隊選択
    /// 中" style feedback about orders not yet committed to a `decide()`
    /// call. Never mutated through this - `push`/`decide` are the only
    /// writers.
    pub fn pending(&self) -> &[Action] {
        &self.queue
    }
}

impl Agent for HumanAgent {
    fn name(&self) -> &str {
        "HumanAgent"
    }

    /// Drains and returns whatever `push` accumulated since the last call -
    /// `obs` is intentionally unused (see this module's own "no privileged
    /// path" doc: this struct has no way to consult it even if it wanted
    /// to).
    fn decide(&mut self, _obs: &Observation) -> Vec<Action> {
        std::mem::take(&mut self.queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::action::ActionError;
    use archipelago_sim::scenario;
    use archipelago_sim::sim::Simulation;
    use archipelago_sim::world::Station;

    #[test]
    fn queue_drains_in_push_order_and_empties() {
        let mut agent = HumanAgent::new(FactionId(0));
        assert!(agent.pending().is_empty());
        agent.push(Action::SetConscription(0.3));
        agent.push(Action::SetCivilianRation(0.9));
        assert_eq!(agent.pending().len(), 2);

        let world = scenario::build_world();
        let obs = Observation { faction: FactionId(0), world: &world };
        let actions = agent.decide(&obs);
        assert_eq!(actions, vec![Action::SetConscription(0.3), Action::SetCivilianRation(0.9)]);
        // Draining must actually drain - a second `decide()` with nothing
        // newly pushed returns nothing, not the same batch again.
        assert!(agent.decide(&obs).is_empty());
        assert!(agent.pending().is_empty());
    }

    /// Stage 7B's acceptance criterion (docs/phase7-spec.md "Stage 7B の受け
    /// 入れ基準"): an invalid action queued through `HumanAgent` must come
    /// back from `Simulation::apply` as an `ActionError`, and the world must
    /// be left completely unchanged - exactly what happens to an AI agent's
    /// invalid action, since both go through the identical `Simulation::
    /// apply` call with no special case for either.
    ///
    /// Confirmed this can actually fail: temporarily changed the pushed
    /// action's target to an adjacent region (a legal move) and re-ran -
    /// `errors` came back empty and the `World` `Debug` snapshot no longer
    /// matched `before` (the unit's `movement` field was now `Some`), so
    /// this test fails the instant the rejected-move assumption stops
    /// holding. Reverted before committing.
    #[test]
    fn human_agent_actions_go_through_validation() {
        let mut sim = Simulation::with_world(scenario::build_world(), 1);
        let before = format!("{:?}", sim.world);

        let unit_id = sim
            .world
            .units
            .iter()
            .find(|u| u.owner == FactionId(0) && u.alive)
            .map(|u| u.id)
            .expect("faction 0 starts with at least one living unit in the embedded scenario");
        let current_station = sim.world.units[unit_id.index()].station;

        // Any region that is neither the unit's current station nor linked
        // to it - `Simulation::apply` must reject a move there as
        // `ActionError::NotAdjacent`, the same check an AI's `MoveUnit`
        // action goes through.
        let far_region = sim
            .world
            .regions
            .iter()
            .map(|r| r.id)
            .find(|&r| match current_station {
                Station::Region(cur) => cur != r && sim.world.link_between(cur, r).is_none(),
                Station::Sea(_) => true, // a fleet ordered onto land is always rejected, regardless of which region.
            })
            .expect("the embedded scenario has at least one non-adjacent region");

        let mut human = HumanAgent::new(FactionId(0));
        human.push(Action::MoveUnit { unit: unit_id, to: Station::Region(far_region) });

        let obs = Observation { faction: FactionId(0), world: &sim.world };
        let actions = human.decide(&obs);
        assert_eq!(actions.len(), 1, "decide() must hand back exactly what was pushed - HumanAgent invents nothing and drops nothing");

        let errors = sim.apply(FactionId(0), &actions);
        assert_eq!(errors, vec![ActionError::NotAdjacent], "an invalid HumanAgent action must be rejected exactly like an AI's would be");
        assert_eq!(
            format!("{:?}", sim.world),
            before,
            "a rejected action must leave the world byte-for-byte unchanged - HumanAgent has no privileged path around validation"
        );
    }

    /// Stage 7B's Bevy-free guard (docs/phase7-spec.md "Stage 7B の受け入れ
    /// 基準": "crates/agents が Bevy に依存しないこと"). Reading the
    /// manifest text and asserting it never names `bevy` is a stronger,
    /// in-suite check than running `cargo tree -p archipelago-agents` out of
    /// band (the spec explicitly allows either) - it fails this very test
    /// run the moment a `bevy` dependency line is added, with no separate
    /// invocation required.
    ///
    /// Confirmed this can actually fail: temporarily appended
    /// `bevy = "0.19"` to `crates/agents/Cargo.toml` and re-ran this test -
    /// it failed immediately on the `contains("bevy")` assertion. Reverted
    /// the manifest before committing.
    #[test]
    fn human_agent_is_bevy_free() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.to_lowercase().contains("bevy"),
            "crates/agents/Cargo.toml must never depend on bevy - HumanAgent must stay reachable from a \
             headless/RL/scripted context with no Bevy App anywhere, not just from apps/game"
        );
    }
}
