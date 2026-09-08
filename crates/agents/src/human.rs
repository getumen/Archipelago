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
//! **Military delegation** (docs/design.md §14): a player can hand
//! `Layer::Military` - the entire force-structure decision domain, not just
//! orders to whichever units happen to exist right now - to the same
//! `HeuristicAgent` logic the AI factions run. `decide()` folds in whatever
//! `military` (a `crate::composite::CompositeAgent` wrapping a
//! `HeuristicAgent` for this same faction, composed - not inherited from,
//! per docs/conventions.md §1) would have ordered. This is why `decide()`
//! now reads `obs`: it did not before, since there was nothing here that
//! needed it. That is still not a privileged path - the wrapped
//! `HeuristicAgent`'s `decide` is the exact same trait method every AI
//! faction's agent runs, and every action it produces still goes through
//! `Simulation::apply` unchanged, whether it came from `military` or the
//! player's own queue.
//!
//! `military` is a `CompositeAgent` routed to `Layer::Military` alone
//! (`archipelago_sim::action::Layer`) rather than a bare `HeuristicAgent` -
//! this *is* delegation expressed as the same layer-routing mechanism every
//! other pluggable-agent composition in this crate now uses (see
//! `crate::composite`), not a separate, ad-hoc concept.
//!
//! **Whole-layer delegation with per-unit carve-outs, not a static unit
//! set.** An earlier version of this module expressed delegation purely as
//! an opt-in `BTreeSet<UnitId>`: `military`'s output was kept only for
//! units already in that set. That broke the one operation a player who
//! says "the AI runs the war" actually performs - handing over the whole
//! army - because `RecruitUnit` (`Layer::Military`, `Action::target_unit`
//! `None`: it *creates* a unit, so there is no existing id to have been
//! added to the set) could never appear in the kept output no matter what
//! was delegated. The army could be moved and reinforced by the AI but
//! never grown or replaced - a played game silently stalls at whatever
//! force existed the moment delegation started (see the regression guard,
//! `tests::delegating_the_whole_military_lets_it_recruit`).
//!
//! `military_delegated` is the fix: when `true`, the *entire* `Layer::
//! Military` decision domain - recruitment included - is delegated, which
//! is what "hand over the military" has to mean if it is to cover force
//! structure at all (an agent that could march and reinforce units but
//! never raise or retire one couldn't meaningfully "be the military" -
//! `Layer::Military`'s own doc in `archipelago_sim::action` makes the same
//! call for the identical reason). Per-unit control is not lost to this -
//! `docs/design.md §14`'s promise that the player can always keep *some*
//! units under direct control is kept by `unit_overrides`, which under
//! whole-layer delegation acts as a carve-out set: `undelegate(unit)` holds
//! that one unit back even though everything else, recruitment included,
//! is the AI's. `delegate`/`undelegate`/`is_delegated` are exactly the same
//! three calls `apps/game`'s unit panel already used for pure per-unit
//! delegation (its "select all, then carve out exceptions" flow), so
//! turning that flow into "delegate the whole military, then carve out
//! exceptions" needed no new UI concept, only `delegate_military` added
//! ahead of it. `unit_overrides` still supports the original, narrower mode
//! too - `military_delegated == false` with a few units individually
//! `delegate`d - for a player who wants to hand off *only* those units and
//! keep everything else, recruitment included, under direct control; see
//! `decide`'s own doc for exactly how the two modes route.
//!
//! Switching `military_delegated` either direction (`delegate_military`/
//! `undelegate_military`) clears `unit_overrides`: the set means opposite
//! things in the two modes (carve-out vs. opt-in), so carrying entries
//! across a mode switch would silently repurpose them into whichever
//! meaning the new mode gives that same `UnitId`, not what the player
//! actually asked for when they added it.
//!
//! `unit_overrides` is never pruned when a unit dies or changes hands - a
//! stale `UnitId` left in the set is inert (`Observation::own_units` never
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

use archipelago_sim::action::{Action, Layer};
use archipelago_sim::agent::Agent;
use archipelago_sim::ids::{FactionId, UnitId};
use archipelago_sim::observation::Observation;

use crate::composite::CompositeAgent;

pub struct HumanAgent {
    faction: FactionId,
    /// Actions queued since the last `decide()` call, in the order `push`
    /// was called - `decide()` hands them to `Simulation::apply` in that
    /// same order, so a player who (say) sets conscription and then moves a
    /// unit in the same paused day has both applied in that order, matching
    /// what they clicked/typed.
    queue: Vec<Action>,
    /// Whether the entire `Layer::Military` decision domain is delegated to
    /// `military` - the primary delegation mode, see this module's own doc
    /// under "Military delegation". `false` until `delegate_military` is
    /// called.
    military_delegated: bool,
    /// Per-unit overrides against whichever mode `military_delegated` is
    /// currently in - see this module's own doc under "Military
    /// delegation" for why the same set means opposite things depending on
    /// that flag:
    /// - while `military_delegated`: a *carve-out* - units named here stay
    ///   under direct player control even though the rest of the army
    ///   (recruitment included) is the AI's.
    /// - while not `military_delegated`: the *only* units delegated,
    ///   opt-in, additively - the original per-unit-only mode.
    ///
    /// A plain `BTreeSet` (not a `HashSet`): iteration order never actually
    /// matters for correctness here (lookups are all by `contains`), but
    /// this crate follows docs/conventions.md §5's "never iterate a
    /// HashMap/HashSet" rule structurally rather than re-litigating it per
    /// call site.
    unit_overrides: BTreeSet<UnitId>,
    /// The same `HeuristicAgent` logic every AI-controlled faction runs
    /// (`crate::default_heuristic_agent`), wrapped in a `CompositeAgent`
    /// routed to `Layer::Military` alone, composed here rather than
    /// reimplemented, so a delegated unit is ordered by the literal same
    /// code path - see `decide`'s own doc and this module's "Military
    /// delegation" doc.
    military: CompositeAgent,
}

impl HumanAgent {
    pub fn new(faction: FactionId) -> Self {
        HumanAgent {
            faction,
            queue: Vec::new(),
            military_delegated: false,
            unit_overrides: BTreeSet::new(),
            military: CompositeAgent::new(faction)
                .route([Layer::Military], Box::new(crate::default_heuristic_agent(faction.index()))),
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

    /// Hands the entire `Layer::Military` decision domain - every unit's
    /// day-to-day orders *and* recruitment - to `military` from the next
    /// `decide()` call onward, clearing any per-unit carve-out/opt-in state
    /// (see this module's own doc for why a mode switch clears
    /// `unit_overrides`). Idempotent - calling this while already
    /// whole-layer delegated only clears `unit_overrides` again (dropping
    /// any carve-outs), it does not toggle anything off.
    pub fn delegate_military(&mut self) {
        self.military_delegated = true;
        self.unit_overrides.clear();
    }

    /// Takes the whole military back under direct player control -
    /// `military` is no longer consulted at all until `delegate_military`
    /// or `delegate` is called again. Clears `unit_overrides` for the same
    /// reason `delegate_military` does. Idempotent - a no-op if the whole
    /// military wasn't delegated (any pure per-unit delegations from
    /// `delegate` are cleared too, matching "take the whole military back"
    /// meaning exactly that).
    pub fn undelegate_military(&mut self) {
        self.military_delegated = false;
        self.unit_overrides.clear();
    }

    /// Whether the entire `Layer::Military` decision domain is currently
    /// delegated - read by `apps/game`'s policy panel to show "AI runs the
    /// war" state distinctly from individual delegated units.
    pub fn is_military_delegated(&self) -> bool {
        self.military_delegated
    }

    /// Hands `unit`'s orders to `military` from the next `decide()` call
    /// onward: while the whole military is delegated, this removes `unit`
    /// from the carve-out set (it goes back to following delegation like
    /// everything else); otherwise it adds `unit` to the per-unit opt-in
    /// set. Idempotent either way - delegating an already-delegated unit
    /// changes nothing.
    pub fn delegate(&mut self, unit: UnitId) {
        if self.military_delegated {
            self.unit_overrides.remove(&unit);
        } else {
            self.unit_overrides.insert(unit);
        }
    }

    /// Takes `unit` back under direct player control: while the whole
    /// military is delegated, this carves `unit` out (every *other* unit,
    /// and recruitment, stays the AI's); otherwise it removes `unit` from
    /// the per-unit opt-in set. Idempotent either way - taking back a unit
    /// that isn't currently delegated changes nothing. `military` simply
    /// stops being asked to order this unit; any move already under way
    /// (`Unit::movement`) is untouched, exactly as taking direct control of
    /// an AI faction's unit via `Simulation` never resets its progress.
    pub fn undelegate(&mut self, unit: UnitId) {
        if self.military_delegated {
            self.unit_overrides.insert(unit);
        } else {
            self.unit_overrides.remove(&unit);
        }
    }

    /// Whether `unit` is currently delegated - read by `apps/game`'s unit
    /// panel/map visuals to mark AI-controlled units. Under whole-layer
    /// delegation this is `true` for every unit *except* an explicit
    /// carve-out; otherwise it is `true` only for units explicitly
    /// `delegate`d.
    pub fn is_delegated(&self, unit: UnitId) -> bool {
        if self.military_delegated {
            !self.unit_overrides.contains(&unit)
        } else {
            self.unit_overrides.contains(&unit)
        }
    }
}

impl Agent for HumanAgent {
    fn name(&self) -> &str {
        "HumanAgent"
    }

    /// Drains whatever `push` accumulated since the last call, then - only
    /// while there is *something* to delegate at all (the whole military,
    /// or at least one individually-opted-in unit) - asks `military` (a
    /// `CompositeAgent` routed to `Layer::Military` alone, wrapping the same
    /// `HeuristicAgent` an AI-controlled faction of this index would run)
    /// what it would order this tick, and appends whichever of those orders
    /// `is_action_delegated` keeps. Everything outside `Layer::Military` was
    /// already discarded by `military` itself (`CompositeAgent`'s own layer
    /// filtering) - `docs/design.md §14` leaves all of that to the player.
    /// The wrapped `HeuristicAgent` still only actually decides once every
    /// `period` days - identical cadence to an AI-controlled faction of the
    /// same index - so most calls here cost nothing beyond that early
    /// return.
    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = std::mem::take(&mut self.queue);
        if self.military_delegated || !self.unit_overrides.is_empty() {
            let ai_actions = self.military.decide(obs);
            actions.extend(ai_actions.into_iter().filter(|a| self.is_action_delegated(a)));
        }
        actions
    }
}

impl HumanAgent {
    /// Whether one of `military`'s own `Layer::Military` outputs should
    /// actually reach the player's faction this tick. Routes on
    /// `Action::target_unit` (the same exhaustive, compiler-checked
    /// classification `Action::layer` is, see `archipelago_sim::action`):
    /// an action that targets an existing unit follows `is_delegated` for
    /// that unit exactly as before; an action that targets no unit at all
    /// (today, only `RecruitUnit`, which *creates* a unit rather than
    /// commanding one that already exists, so there is no per-unit
    /// carve-out/opt-in it could possibly be checked against) is kept only
    /// under whole-layer delegation, since deciding how large the army is
    /// and where is a force-structure decision, not an order to some unit
    /// that does or doesn't happen to be in a set.
    fn is_action_delegated(&self, action: &Action) -> bool {
        match action.target_unit() {
            Some(unit) => self.is_delegated(unit),
            None => self.military_delegated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::action::ActionError;
    use archipelago_sim::scenario;
    use archipelago_sim::sim::Simulation;
    use archipelago_sim::world::Station;
    use crate::HeuristicAgent;

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
                // An air unit ordered onto a plain land `Station::Region` is always rejected too - `apply_move`'s
                // `Station::Airfield` arm (Stage 10 follow-up: `Domain::Air` `MoveUnit` support is real now) only
                // ever accepts another `Station::Airfield` destination, the same way a fleet's own arm only ever
                // accepts `Station::Sea`, so a `Station::Region` target stays illegal for a squadron regardless.
                Station::Airfield(_) => true,
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
    /// `HumanAgent`'s first decision without ever calling `decide()` on the
    /// `HumanAgent` under test itself (which would advance its internal
    /// counters and make the test's own peek interfere with what it's
    /// trying to observe).
    fn units_a_fresh_heuristic_would_order(faction: FactionId, obs: &Observation) -> Vec<UnitId> {
        let mut peek = crate::default_heuristic_agent(faction.index());
        peek.decide(obs).iter().filter_map(Action::target_unit).collect()
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
            would_order.iter().any(|&u| actions.iter().any(|a| a.target_unit() == Some(u))),
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
            actions.iter().all(|a| a.target_unit() != Some(target)),
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
            before.iter().any(|a| a.target_unit() == Some(target)),
            "delegating the unit must produce an order for it: got {before:?}"
        );

        agent.undelegate(target);
        assert!(!agent.is_delegated(target));
        let after = agent.decide(&obs);
        assert!(
            after.iter().all(|a| a.target_unit() != Some(target)),
            "taking control back must stop the AI from ordering this unit, even on the same tick: got {after:?}"
        );
    }

    /// Per-unit-only delegation (`delegate(unit)` without ever calling
    /// `delegate_military`) must never authorize `RecruitUnit`, even while
    /// `military` is actively running for other reasons - it targets no
    /// existing unit, so there is no per-unit opt-in it could possibly
    /// satisfy. Force structure is a whole-layer decision (this module's
    /// own doc), not something a handful of individually-delegated units
    /// can imply.
    #[test]
    fn per_unit_delegation_alone_never_recruits() {
        let mut sim = Simulation::with_world(scenario::build_world(), 1);
        let player = FactionId(0);
        let n = sim.world.factions.len();

        let obs = Observation { faction: player, world: &sim.world };
        let would_order = units_a_fresh_heuristic_would_order(player, &obs);
        let mut agent = HumanAgent::new(player);
        for &unit in &would_order {
            agent.delegate(unit);
        }
        assert!(!agent.is_military_delegated(), "delegate(unit) alone must never flip on whole-layer delegation");

        let mut ai: Vec<HeuristicAgent> = (1..n).map(crate::default_heuristic_agent).collect();
        for _ in 0..300 {
            if sim.world.day >= 300 {
                break;
            }
            for i in 0..n {
                if !sim.world.factions[i].alive {
                    continue;
                }
                let faction = FactionId(i as u32);
                let o = Observation { faction, world: &sim.world };
                let actions = if faction == player { agent.decide(&o) } else { ai[i - 1].decide(&o) };
                assert!(
                    faction != player || actions.iter().all(|a| !matches!(a, Action::RecruitUnit { .. })),
                    "per-unit delegation alone must never produce a RecruitUnit action, got {actions:?}"
                );
                sim.apply(faction, &actions);
            }
            sim.step();
        }
    }

    /// The regression guard for this module's central fix (see this
    /// module's own doc under "Whole-layer delegation with per-unit
    /// carve-outs, not a static unit set"): delegating the whole military
    /// must let it actually grow the army over time via `RecruitUnit`, not
    /// only reshuffle whatever units existed the moment delegation started.
    ///
    /// Confirmed this can actually fail: temporarily made `is_action_delegated`
    /// return `false` for every action with no `target_unit` (i.e. restored
    /// this module's pre-fix behaviour, where `RecruitUnit` could never pass
    /// the filter) and re-ran - `recruits` stayed `0` for the full 300 days
    /// even with the whole military delegated. Reverted before committing.
    #[test]
    fn delegating_the_whole_military_lets_it_recruit() {
        let mut sim = Simulation::with_world(scenario::build_world(), 1);
        let player = FactionId(0);
        let n = sim.world.factions.len();

        let mut human = HumanAgent::new(player);
        human.delegate_military();
        let mut ai: Vec<HeuristicAgent> = (1..n).map(crate::default_heuristic_agent).collect();

        let mut recruits = 0u32;
        const DAYS: u32 = 300;
        while sim.world.day < DAYS {
            for i in 0..n {
                if !sim.world.factions[i].alive {
                    continue;
                }
                let faction = FactionId(i as u32);
                let obs = Observation { faction, world: &sim.world };
                let actions = if faction == player { human.decide(&obs) } else { ai[i - 1].decide(&obs) };
                if faction == player {
                    recruits += actions.iter().filter(|a| matches!(a, Action::RecruitUnit { .. })).count() as u32;
                }
                sim.apply(faction, &actions);
            }
            sim.step();
        }
        assert!(
            recruits > 0,
            "a whole-military-delegated faction must actually recruit new units over {DAYS} days, got {recruits} RecruitUnit actions"
        );
    }

    /// Air power's own instance of the fix above (docs/design.md §14: "人間
    /// も AI と同じ入口から世界に触る" has to hold for every domain, not just
    /// land/sea): `is_action_delegated` routes purely on `Action::
    /// target_unit`, with no domain-specific match arm anywhere in this
    /// module (see this module's own doc, "Whole-layer delegation with
    /// per-unit carve-outs") - so a whole-military-delegated player's
    /// squadrons should already be recruited, flown, and struck through
    /// exactly like a `HeuristicAgent`-controlled faction's, with no code
    /// path here that singles Domain::Air out as unsupported. This nails
    /// that down for `RecruitUnit { domain: Domain::Air, .. }` specifically -
    /// `crate::tests::heuristic_agent_recruits_air_when_it_can_afford_it`
    /// already proves the wrapped `HeuristicAgent` alone emits this on
    /// mvp's very first `decide()` call, so one delegated call is enough to
    /// show it survives `is_action_delegated`'s filter unchanged.
    ///
    /// Confirmed this can actually fail: temporarily changed `is_action_
    /// delegated` to `match action.target_unit() { Some(unit) =>
    /// self.is_delegated(unit), None => false }` (the old, pre-fix
    /// "RecruitUnit can never be delegated" shape this module's own doc
    /// describes under "Whole-layer delegation with per-unit carve-outs") -
    /// the assertion below then failed, `actions` containing no
    /// `RecruitUnit` at all despite whole-layer delegation being on.
    /// Reverted before committing.
    #[test]
    fn whole_military_delegation_recruits_air_squadrons() {
        let world = scenario::build_world();
        let obs = Observation { faction: FactionId(0), world: &world };

        let mut human = HumanAgent::new(FactionId(0));
        human.delegate_military();
        let actions = human.decide(&obs);

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::RecruitUnit { domain: archipelago_sim::world::Domain::Air, .. })),
            "a whole-military-delegated HumanAgent must recruit air squadrons exactly like a HeuristicAgent \
             faction would on the same tick, but got: {actions:?}"
        );
    }

    /// The carve-out half of whole-layer delegation: delegating the entire
    /// military, then taking one unit back, must stop the AI from ordering
    /// *that* unit while everything else - including recruitment - stays
    /// delegated.
    #[test]
    fn whole_military_delegation_supports_a_per_unit_carve_out() {
        let world = scenario::build_world();
        let obs = Observation { faction: FactionId(0), world: &world };
        let would_order = units_a_fresh_heuristic_would_order(FactionId(0), &obs);
        let carved_out = *would_order.first().expect("mvp's faction 0 must get at least one unit order on day 0");
        let other = obs
            .world
            .units
            .iter()
            .find(|u| u.owner == FactionId(0) && u.alive && u.id != carved_out)
            .map(|u| u.id)
            .expect("mvp fields more than one unit per faction");

        let mut agent = HumanAgent::new(FactionId(0));
        agent.delegate_military();
        assert!(agent.is_delegated(carved_out), "everything must start delegated once the whole military is");
        assert!(agent.is_delegated(other));

        agent.undelegate(carved_out);
        assert!(!agent.is_delegated(carved_out), "an explicit carve-out must stop being delegated");
        assert!(agent.is_delegated(other), "carving out one unit must not affect any other unit");
        assert!(agent.is_military_delegated(), "carving out one unit must not turn off whole-layer delegation itself");

        let actions = agent.decide(&obs);
        assert!(
            actions.iter().all(|a| a.target_unit() != Some(carved_out)),
            "the carved-out unit must receive no AI order: got {actions:?}"
        );
        assert!(
            would_order.iter().filter(|&&u| u != carved_out).any(|&u| actions.iter().any(|a| a.target_unit() == Some(u))),
            "every other unit must still be ordered by the AI: got {actions:?}"
        );

        // Re-delegating the carved-out unit explicitly must fold it back
        // in, exactly like the panel's "select all, then carve out
        // exceptions, then change your mind about one" flow.
        agent.delegate(carved_out);
        assert!(agent.is_delegated(carved_out));
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
