//! `HumanAgent` (docs/phase7-spec.md "Stage 7B — 遊ぶ", docs/design.md §14):
//! the `Agent` implementation a human player sits behind. Every player
//! subject - human included - goes through the exact same `Agent`
//! interface, so this is deliberately the thinnest possible adapter: a
//! queue that `apps/game`'s input systems push `Action`s onto, and
//! `decide()` drains.
//!
//! **No privileged path.** Whatever the UI pushes comes back out of
//! `decide()` unchanged and goes through `Simulation::apply` exactly like
//! any `HeuristicAgent`/`LlmAgent` output - see
//! `tests::human_agent_actions_go_through_validation` for the regression
//! guard on that property. This struct has no way to check whether a
//! queued action is legal, so it cannot pre-filter anything even by
//! accident.
//!
//! **Military delegation** (docs/design.md §14): a player can hand a unit's
//! day-to-day orders to the same `HeuristicAgent` logic the AI factions run,
//! at per-unit granularity - `delegate`/`undelegate` add/remove a `UnitId`
//! from `delegated`, and `decide()` folds in whatever `military` (an
//! internal `HeuristicAgent` for this same faction, composed - not
//! inherited from, per docs/conventions.md §1) would have ordered for
//! exactly those units. This is why `decide()` now reads `obs`: it did not
//! before, since there was nothing here that needed it. That is still not a
//! privileged path - `military.decide_for_llm` is the identical call
//! `LlmAgent::decide` makes on its own wrapped `HeuristicAgent`
//! (`crate::llm`), and every action it produces still goes through
//! `Simulation::apply` unchanged, whether it targets a delegated unit or
//! came from the player's own queue. Delegation is deliberately per-unit
//! rather than per-army or per-front: `docs/design.md §14`'s promise is
//! that the player always keeps *some* units under direct control while
//! the rest fight themselves, and a coarser knob (all-or-nothing, or "this
//! front only" with no map-level notion of fronts in `crates/sim`) can't
//! express "hold these three elite corps back, delegate everyone else" -
//! exactly the workflow `apps/game`'s unit panel's own "select all, then
//! carve out exceptions" flow is built around.
//!
//! `delegated` is never pruned when a unit dies or changes hands - a stale
//! `UnitId` left in the set is inert (`Observation::own_units` never
//! produces it again, `Action`s are only ever emitted for ids currently
//! owned by this faction, and `UnitId`s are never reused - see
//! `military::Unit` construction - so a stale entry can never later
//! resolve to some other unit) and this is player intent, not `crates/sim`
//! world state, so it survives exactly as long as this `HumanAgent` does.
//!
//! **Bevy-free.** This module (and this whole crate - see
//! `tests::human_agent_is_bevy_free`) imports nothing from `bevy`. The
//! `apps/game` client owns the mouse/keyboard/UI side entirely; it hands
//! this struct finished `Action` values and nothing else, which is what
//! keeps `HumanAgent` usable from a headless/RL/scripted context too (a
//! test harness, a bug-repro script, or a training-data capture tool can
//! drive one exactly like `apps/game` does, with no Bevy `App` anywhere).

use std::collections::BTreeSet;

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::ids::{FactionId, UnitId};
use archipelago_sim::observation::Observation;

use crate::HeuristicAgent;

pub struct HumanAgent {
    faction: FactionId,
    /// Actions queued since the last `decide()` call, in the order `push`
    /// was called - `decide()` hands them to `Simulation::apply` in that
    /// same order, so a player who (say) sets conscription and then moves a
    /// unit in the same paused day has both applied in that order, matching
    /// what they clicked/typed.
    queue: Vec<Action>,
    /// Units currently delegated to `military` - see this module's own doc
    /// under "Military delegation". A plain `BTreeSet` (not a `HashSet`):
    /// iteration order never actually matters for correctness here (lookups
    /// are all by `contains`), but this crate follows docs/conventions.md
    /// §5's "never iterate a HashMap/HashSet" rule structurally rather than
    /// re-litigating it per call site.
    delegated: BTreeSet<UnitId>,
    /// The same `HeuristicAgent` logic every AI-controlled faction runs
    /// (`crate::default_heuristic_agent`), composed here rather than
    /// reimplemented, so a delegated unit is ordered by the literal same
    /// code path - see `decide`'s own doc.
    military: HeuristicAgent,
}

impl HumanAgent {
    pub fn new(faction: FactionId) -> Self {
        HumanAgent {
            faction,
            queue: Vec::new(),
            delegated: BTreeSet::new(),
            military: crate::default_heuristic_agent(faction.index()),
        }
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

    /// Hands `unit`'s orders to `military` from the next `decide()` call
    /// onward. Idempotent - delegating an already-delegated unit changes
    /// nothing.
    pub fn delegate(&mut self, unit: UnitId) {
        self.delegated.insert(unit);
    }

    /// Takes `unit` back under direct player control. Idempotent - taking
    /// back a unit that isn't delegated changes nothing. `military` simply
    /// stops being asked to order this unit; any move already under way
    /// (`Unit::movement`) is untouched, exactly as taking direct control of
    /// an AI faction's unit via `Simulation` never resets its progress.
    pub fn undelegate(&mut self, unit: UnitId) {
        self.delegated.remove(&unit);
    }

    /// Whether `unit` is currently delegated - read by `apps/game`'s unit
    /// panel/map visuals to mark AI-controlled units.
    pub fn is_delegated(&self, unit: UnitId) -> bool {
        self.delegated.contains(&unit)
    }

    /// Every currently-delegated unit, for UI code that wants to list or
    /// count them without a unit-by-unit `is_delegated` scan.
    pub fn delegated_units(&self) -> &BTreeSet<UnitId> {
        &self.delegated
    }
}

/// Whether `action` orders an existing unit (as opposed to a whole-faction
/// policy/diplomacy/construction/recruitment decision) - `HumanAgent::
/// decide` keeps only these from `military`'s output, and only for units in
/// `delegated`. Every other `Action` variant `HeuristicAgent::decide_for_llm`
/// can produce (`SetConscription`, `RecruitUnit`, `Build`, `ProposeTreaty`,
/// ...) stays the player's own call per docs/design.md §14 - delegation is
/// unit orders only, never economy, diplomacy, or force composition.
fn ordered_unit(action: &Action) -> Option<UnitId> {
    match *action {
        Action::MoveUnit { unit, .. }
        | Action::HoldUnit { unit }
        | Action::DisbandUnit { unit }
        | Action::ReinforceUnit { unit } => Some(unit),
        _ => None,
    }
}

impl Agent for HumanAgent {
    fn name(&self) -> &str {
        "HumanAgent"
    }

    /// Drains whatever `push` accumulated since the last call, then - only
    /// while at least one unit is delegated - asks `military` (the same
    /// `HeuristicAgent::decide_for_llm` call `LlmAgent::decide` makes on its
    /// own wrapped agent) what it would order this tick, and appends
    /// whichever of those orders target a delegated unit
    /// (`ordered_unit`/`delegated`). Every other action `military` produces
    /// (policy, diplomacy, recruitment, construction) is discarded - it was
    /// computed (the same monolithic `decide_for_llm` runs all of it in one
    /// call, precisely so no piece of the AI's unit-ordering logic is
    /// forked out into a copy) but never applied, since none of it is a
    /// `unit` order and `docs/design.md §14` leaves all of that to the
    /// player. `military` still only actually decides once every
    /// `HeuristicAgent`-internal `period` days - identical cadence to an
    /// AI-controlled faction of the same index - so most calls here cost
    /// nothing beyond the early return inside `decide_for_llm` itself.
    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = std::mem::take(&mut self.queue);
        if !self.delegated.is_empty() {
            let ai_actions = self.military.decide_for_llm(obs, None);
            actions.extend(ai_actions.into_iter().filter(|a| ordered_unit(a).is_some_and(|u| self.delegated.contains(&u))));
        }
        actions
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

    /// Which unit id(s) a *fresh* default heuristic agent for `faction`
    /// would order on `obs`'s exact tick. `HumanAgent::military` starts in
    /// the identical state (`crate::default_heuristic_agent`, never
    /// previously called) as this standalone peek agent, so for the very
    /// first `decide()` call against the same `obs` the two are guaranteed
    /// to produce byte-identical output - this predicts a fresh
    /// `HumanAgent`'s first decision without ever calling `decide_for_llm`
    /// on the `HumanAgent` under test itself (which would advance its
    /// internal counters and make the test's own peek interfere with what
    /// it's trying to observe).
    fn units_a_fresh_heuristic_would_order(faction: FactionId, obs: &Observation) -> Vec<UnitId> {
        let mut peek = crate::default_heuristic_agent(faction.index());
        peek.decide_for_llm(obs, None).iter().filter_map(ordered_unit).collect()
    }

    /// A delegated unit must receive an order from `decide()` even though
    /// the player never called `push` at all - the central promise of
    /// military delegation (docs/design.md §14).
    ///
    /// Confirmed this can actually fail: temporarily made `HumanAgent::
    /// decide` return only `std::mem::take(&mut self.queue)` (i.e. ignore
    /// `delegated`/`military` entirely, the pre-delegation behaviour) and
    /// re-ran - the assertion below failed immediately since `actions` came
    /// back empty. Reverted before committing.
    #[test]
    fn delegated_unit_receives_orders_without_player_issuing_any() {
        let world = scenario::build_world();
        let obs = Observation { faction: FactionId(0), world: &world };
        let would_order = units_a_fresh_heuristic_would_order(FactionId(0), &obs);
        assert!(
            !would_order.is_empty(),
            "the embedded mvp scenario's faction 0 must actually get at least one unit order on its first \
             decide() call, or this test can't demonstrate delegation doing anything"
        );

        let mut agent = HumanAgent::new(FactionId(0));
        for &unit in &would_order {
            agent.delegate(unit);
        }
        // No `push` call anywhere - every action below must come from
        // `military`.
        let actions = agent.decide(&obs);
        assert!(
            would_order.iter().any(|&u| actions.iter().any(|a| ordered_unit(a) == Some(u))),
            "a delegated unit must be ordered by decide() with no player action queued: got {actions:?}"
        );
    }

    /// A unit that was never delegated must never receive an order, even
    /// while `military` is actively running (because some *other* unit is
    /// delegated) and would have ordered this one too if it had been handed
    /// over.
    ///
    /// Confirmed this can actually fail: temporarily changed `HumanAgent::
    /// decide` to append every one of `military`'s unit-ordering actions
    /// unconditionally (dropping the `self.delegated.contains(&u)` filter)
    /// and re-ran - the assertion failed because `target` (deliberately left
    /// undelegated) showed up in `actions` anyway. Reverted before
    /// committing.
    #[test]
    fn undelegated_unit_receives_no_orders() {
        let world = scenario::build_world();
        let obs = Observation { faction: FactionId(0), world: &world };
        let would_order = units_a_fresh_heuristic_would_order(FactionId(0), &obs);
        let target = *would_order.first().expect("mvp's faction 0 must get at least one unit order on day 0");
        let other = obs
            .world
            .units
            .iter()
            .find(|u| u.owner == FactionId(0) && u.alive && u.id != target)
            .map(|u| u.id)
            .expect("mvp fields more than one unit per faction");

        let mut agent = HumanAgent::new(FactionId(0));
        // `target` is deliberately left undelegated - only `other` is, so
        // `decide()` still has a reason to consult `military` at all.
        agent.delegate(other);
        let actions = agent.decide(&obs);
        assert!(
            actions.iter().all(|a| ordered_unit(a) != Some(target)),
            "an undelegated unit must never appear in decide()'s output, even though a fresh heuristic would \
             have ordered it if it were delegated: got {actions:?}"
        );
    }

    /// Taking control of a delegated unit back must stop `military` from
    /// ordering it, even on the exact same simulated tick (no `Simulation::
    /// step` happens between the two `decide()` calls below, so this is a
    /// pure test of the delegation filter, not of the world having moved
    /// on).
    ///
    /// Confirmed this can actually fail: temporarily made `HumanAgent::
    /// undelegate` a no-op (never actually removing from `self.delegated`)
    /// and re-ran - `after` still contained an order for `target`, failing
    /// the second assertion. Reverted before committing.
    #[test]
    fn taking_control_back_stops_ordering_that_unit() {
        let world = scenario::build_world();
        let obs = Observation { faction: FactionId(0), world: &world };
        let would_order = units_a_fresh_heuristic_would_order(FactionId(0), &obs);
        let target = *would_order.first().expect("mvp's faction 0 must get at least one unit order on day 0");

        let mut agent = HumanAgent::new(FactionId(0));
        agent.delegate(target);
        assert!(agent.is_delegated(target));
        let before = agent.decide(&obs);
        assert!(
            before.iter().any(|a| ordered_unit(a) == Some(target)),
            "delegating the unit must produce an order for it: got {before:?}"
        );

        agent.undelegate(target);
        assert!(!agent.is_delegated(target));
        let after = agent.decide(&obs);
        assert!(
            after.iter().all(|a| ordered_unit(a) != Some(target)),
            "taking control back must stop the AI from ordering this unit, even on the same tick: got {after:?}"
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
