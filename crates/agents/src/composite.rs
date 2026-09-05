//! `CompositeAgent`: routes each `Layer` (`archipelago_sim::action::Layer`)
//! to a separate `Agent`, so a faction's military, economy, grand-strategy,
//! and diplomacy decisions can each come from a different implementation -
//! design.md §14 (Human/Heuristic/LLM/RL interchangeable) and §22 (users
//! writing their own agents) both want this at layer granularity, most
//! concretely for RL: a military-only policy faces `Layer::Military`'s
//! narrow action space instead of the full flattened `Discrete`.
//!
//! `HumanAgent`'s per-unit military delegation (`crate::human`) is built on
//! top of exactly this mechanism - see that module's doc for why delegation
//! itself needs one more routing key (`Action::target_unit`) beyond `Layer`
//! alone.
//!
//! ## Absent layers
//!
//! A `Layer` no `route` call claims produces **no actions at all** - not an
//! error, not a fallback to some default agent. This is a deliberate
//! composition choice a caller makes when assembling a `CompositeAgent`
//! (docs/conventions.md §3's no-fallback rule is exactly why there is no
//! default/no-op agent silently filled in for an unclaimed layer): an RL
//! harness training a military-only policy legitimately wants zero
//! economy/diplomacy actions to ever appear for the faction it controls,
//! and forcing some placeholder agent into those layers would hide that
//! choice rather than express it. `absent_layers` reports exactly which
//! layers are currently unclaimed, for a caller that wants to check its own
//! composition rather than find out by watching a policy area go silent.
//!
//! ## One agent, several layers
//!
//! `route` takes a *set* of layers for one `Agent`, not one layer at a
//! time, so a single wrapped agent that already decides several layers'
//! worth of actions in one `decide()` call (`HeuristicAgent::decide`, which
//! produces every layer together) is asked exactly once per tick and
//! simply has its output split by `Action::layer()` - it is never invoked
//! twice in the same tick just because it owns two layers. That matters for
//! correctness, not just efficiency: `HeuristicAgent` carries per-tick
//! internal counters (`chronic_insolvency_ticks`, `focus_initialized`) that
//! a second call in the same tick would advance again, and a real HTTP-backed
//! `LlmAgent`-style agent would be billed for a second round-trip it never
//! needed.

use archipelago_sim::action::{Action, Layer, ALL_LAYERS};
use archipelago_sim::agent::Agent;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;

/// One `Agent`'s claim on a set of layers - see this module's doc for why
/// it is a set rather than a single `Layer`.
struct Route {
    layers: Vec<Layer>,
    agent: Box<dyn Agent + Send + Sync>,
}

/// Composes a faction's decisions out of one `Agent` per `Layer` - see this
/// module's doc for the routing rules and what an unclaimed layer does.
pub struct CompositeAgent {
    faction: FactionId,
    routes: Vec<Route>,
}

impl CompositeAgent {
    /// A `CompositeAgent` for `faction` with every layer unclaimed - add
    /// authorities with `route` before `decide()` is expected to produce
    /// anything (an entirely unrouted `CompositeAgent` is valid and simply
    /// always returns `Vec::new()`, per this module's "absent layers" doc).
    pub fn new(faction: FactionId) -> Self {
        CompositeAgent { faction, routes: Vec::new() }
    }

    /// Registers `agent` as the sole authority for every layer in `layers`:
    /// `decide()` calls it exactly once per tick and keeps only the actions
    /// it produces whose own `Action::layer()` is among `layers` - anything
    /// else `agent` returns (it may be a `HeuristicAgent`-style agent that
    /// decides every layer at once) is discarded, not misrouted.
    ///
    /// # Panics
    ///
    /// If any of `layers` was already claimed by an earlier `route` call on
    /// this same `CompositeAgent`. Each layer has exactly one owner; a
    /// second claim is a composition bug in the caller (which agent should
    /// actually decide this layer is now ambiguous), and per
    /// docs/conventions.md §3 that must fail loudly at the point it's
    /// introduced rather than being silently resolved by "whichever route
    /// runs last wins" or "both routes' output reaches the world".
    pub fn route(mut self, layers: impl IntoIterator<Item = Layer>, agent: Box<dyn Agent + Send + Sync>) -> Self {
        let layers: Vec<Layer> = layers.into_iter().collect();
        for &layer in &layers {
            assert!(
                !self.routes.iter().any(|r| r.layers.contains(&layer)),
                "CompositeAgent for {:?}: {layer:?} is already routed to another agent",
                self.faction,
            );
        }
        self.routes.push(Route { layers, agent });
        self
    }

    pub fn faction(&self) -> FactionId {
        self.faction
    }

    /// Every `Layer` no `route` call has claimed yet - see this module's
    /// doc under "Absent layers" for what that means for `decide()`'s
    /// output.
    pub fn absent_layers(&self) -> Vec<Layer> {
        ALL_LAYERS.into_iter().filter(|l| !self.routes.iter().any(|r| r.layers.contains(l))).collect()
    }
}

impl Agent for CompositeAgent {
    fn name(&self) -> &str {
        "CompositeAgent"
    }

    /// Calls every routed agent exactly once, in the fixed order `route`
    /// was called, keeping only each one's own claimed-layer actions. A
    /// layer nothing claimed contributes nothing - not skipped as an
    /// error, simply never produced (see this module's doc).
    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = Vec::new();
        for route in &mut self.routes {
            let produced = route.agent.decide(obs);
            actions.extend(produced.into_iter().filter(|a| route.layers.contains(&a.layer())));
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;
    use archipelago_sim::scenario;
    use crate::HeuristicAgent;

    /// A stub `Agent` that always returns the same fixed script of actions,
    /// regardless of `obs` - lets these tests assert exactly which of a
    /// known output `CompositeAgent` keeps, without depending on
    /// `HeuristicAgent`'s actual tuning. Also counts how many times
    /// `decide` was called, via a handle the test keeps alongside it (an
    /// `Arc`, not an `Rc`: `CompositeAgent::route` requires `Send + Sync`,
    /// the same bound `apps/game`'s own `Box<dyn Agent + Send + Sync>`
    /// controllers already need), for the "One agent, several layers"
    /// correctness property described in this module's own doc.
    struct Scripted {
        actions: Vec<Action>,
        calls: Arc<AtomicU32>,
    }

    impl Scripted {
        fn new(actions: Vec<Action>) -> Self {
            Scripted { actions, calls: Arc::new(AtomicU32::new(0)) }
        }
    }

    impl Agent for Scripted {
        fn name(&self) -> &str {
            "Scripted"
        }
        fn decide(&mut self, _obs: &Observation) -> Vec<Action> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.actions.clone()
        }
    }

    fn obs(world: &archipelago_sim::world::World) -> Observation<'_> {
        Observation { faction: FactionId(0), world }
    }

    /// A `CompositeAgent` with military routed to one agent and economy to
    /// another must return exactly the union of each one's *own-layer*
    /// output - nothing from a non-claimed layer either agent happened to
    /// also produce.
    ///
    /// Confirmed this can actually fail: temporarily removed the
    /// `route.layers.contains(&a.layer())` filter in `decide` (keeping
    /// every action from every routed agent unconditionally) and re-ran -
    /// the assertion failed because the military route's `SetConscription`
    /// (which it has no business producing, but this stub deliberately
    /// does to prove the filter matters) leaked into the output. Reverted
    /// before committing.
    #[test]
    fn composite_routes_each_layer_to_its_own_agent() {
        let world = scenario::build_world();
        let unit = world.units.iter().find(|u| u.owner == FactionId(0)).unwrap().id;

        let military = Scripted::new(vec![
            Action::HoldUnit { unit },
            // Deliberately out-of-layer output from the military agent -
            // must never appear in the composite's result.
            Action::SetConscription(0.9),
        ]);
        let economy = Scripted::new(vec![
            Action::SetConscription(0.4),
            // Deliberately out-of-layer output from the economy agent.
            Action::HoldUnit { unit },
        ]);

        let mut composite = CompositeAgent::new(FactionId(0))
            .route([Layer::Military], Box::new(military))
            .route([Layer::Economy, Layer::GrandStrategy, Layer::Diplomacy], Box::new(economy));

        let actions = composite.decide(&obs(&world));
        assert_eq!(
            actions,
            vec![Action::HoldUnit { unit }, Action::SetConscription(0.4)],
            "each route must contribute only the actions matching its own claimed layers: {actions:?}"
        );
    }

    /// A layer nothing was ever routed for must never appear in the output,
    /// even when a routed agent (incorrectly, or just incidentally) also
    /// produces an action in that layer.
    ///
    /// Confirmed this can actually fail: temporarily routed `Layer::Economy`
    /// to the same `military` agent as `Layer::Military` (i.e. made
    /// `Diplomacy` no longer absent) and re-ran with the original
    /// assertion - it failed the moment `Diplomacy` had a route, showing the
    /// test really is pinned to "no route exists", not just "no diplomacy
    /// action happened to be produced". Reverted before committing.
    #[test]
    fn absent_layer_produces_nothing() {
        let world = scenario::build_world();
        let unit = world.units.iter().find(|u| u.owner == FactionId(0)).unwrap().id;

        // Only Military is routed - Economy, GrandStrategy and Diplomacy
        // are all left unclaimed on purpose.
        let military = Scripted::new(vec![Action::HoldUnit { unit }]);
        let mut composite = CompositeAgent::new(FactionId(0)).route([Layer::Military], Box::new(military));

        assert_eq!(
            composite.absent_layers(),
            vec![Layer::Economy, Layer::GrandStrategy, Layer::Diplomacy],
            "every layer not explicitly routed must be reported absent"
        );

        let actions = composite.decide(&obs(&world));
        assert_eq!(actions, vec![Action::HoldUnit { unit }], "an absent layer must contribute zero actions, not a default/fallback one");
        assert!(
            actions.iter().all(|a| a.layer() == Layer::Military),
            "only the routed layer's actions may appear: {actions:?}"
        );
    }

    /// An agent routed to more than one layer must be asked to `decide`
    /// exactly once per tick, not once per layer it owns - see this
    /// module's doc under "One agent, several layers" for why a second call
    /// would be a correctness bug, not just waste.
    ///
    /// Confirmed this can actually fail: temporarily changed `decide` to
    /// call `route.agent.decide(obs)` once per layer in `route.layers`
    /// (i.e. duplicating the call the way a naive per-layer loop would) and
    /// re-ran - `calls.get()` came back `3` instead of `1`. Reverted before
    /// committing.
    #[test]
    fn agent_routed_to_several_layers_is_called_once() {
        let world = scenario::build_world();
        let script = Scripted::new(vec![Action::SetConscription(0.5)]);
        let calls = Arc::clone(&script.calls);

        let mut composite = CompositeAgent::new(FactionId(0))
            .route([Layer::Economy, Layer::GrandStrategy, Layer::Diplomacy], Box::new(script));

        composite.decide(&obs(&world));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an agent claiming 3 layers must still be asked to decide exactly once per tick"
        );
    }

    /// Claiming an already-routed layer a second time is a composition bug
    /// and must panic rather than silently pick a winner.
    #[test]
    #[should_panic(expected = "already routed")]
    fn double_routing_a_layer_panics() {
        let military_a = Scripted::new(vec![]);
        let military_b = Scripted::new(vec![]);
        let _ = CompositeAgent::new(FactionId(0))
            .route([Layer::Military], Box::new(military_a))
            .route([Layer::Military, Layer::Economy], Box::new(military_b));
    }

    // -------------------------------------------------------------------
    // Demonstration: a mixed-agent faction is a genuine mixture, not
    // reducible to either ingredient agent alone. Everything above this
    // point tests the routing mechanism in isolation with stub agents;
    // this actually plays the embedded mvp scenario with two real, very
    // differently tuned `HeuristicAgent`s and shows the *composed* faction
    // exhibits both ingredients' signature behaviour, at once, over a real
    // run - the point of building `Layer`/`CompositeAgent` at all.
    // -------------------------------------------------------------------

    /// `caution` far below `HeuristicAgent::new`'s documented "attacks at
    /// parity" (~1.0) baseline, at `peace_disposition`'s neutral default -
    /// `offensive()`/`naval_ops()` launch an attack even at a real
    /// disadvantage, and this agent's *own* `Layer::Diplomacy` behaviour
    /// (were it running the whole faction) is the ordinary baseline: it
    /// still sues for `Ceasefire` once its own aggression costs it enough
    /// casualties (`evaluate_proposal`'s `MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING`
    /// gate), just at the neutral rate, not an eager one.
    const HAWK_CAUTION: f32 = 0.3;
    const HAWK_PEACE_DISPOSITION: f32 = 1.0;

    /// `caution` high enough that `offensive()` essentially never fires (it
    /// needs a large edge it won't have), paired with a `peace_disposition`
    /// far above neutral - `diplomacy_ai`'s/`evaluate_proposal`'s
    /// `outmatched` check (`own_power < enemy_power * PEACE_SEEK_BASE_RATIO
    /// * peace_disposition`) becomes easy to satisfy for any real casualty
    /// count. On its *own* (running the whole faction, so its own low
    /// attack rate is also what keeps its own casualties low),
    /// `MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING` rarely trips, so this agent
    /// alone doesn't actually sue for peace that often either - see the
    /// test's own doc for why that makes the *mixed* result more striking,
    /// not less.
    const DOVE_CAUTION: f32 = 5.0;
    const DOVE_PEACE_DISPOSITION: f32 = 16.0;

    /// Plays the embedded mvp scenario for `days`, with `faction` decided
    /// entirely by `agent` and every other faction by the ordinary
    /// `crate::default_heuristic_agent`. Returns `(attacks, peace_seeking)`:
    /// - `attacks`: `MoveUnit` orders whose destination region `faction`
    ///   does not already own at the moment the order is given - the
    ///   observable signature of `offensive()` picking a target, as opposed
    ///   to `advance_interior`'s repositioning within owned territory
    ///   (which only ever retargets a region `faction` already holds).
    /// - `peace_seeking`: `ProposeTreaty`/`AcceptTreaty` actions toward
    ///   `Ceasefire`/`NonAggression`/`Alliance` - `diplomacy_ai`'s
    ///   `peace_disposition`-driven behaviour.
    fn play(agent: Box<dyn Agent + Send + Sync>, faction: FactionId, days: u32) -> (u32, u32) {
        let mut sim = archipelago_sim::sim::Simulation::with_world(scenario::build_world(), 1);
        let n = sim.world.factions.len();
        let mut agent = Some(agent);
        let mut agents: Vec<Box<dyn Agent + Send + Sync>> = (0..n)
            .map(|i| {
                if i == faction.index() {
                    agent.take().expect("faction.index() is visited exactly once")
                } else {
                    Box::new(crate::default_heuristic_agent(i))
                }
            })
            .collect();

        let (mut attacks, mut peace_seeking) = (0u32, 0u32);
        while sim.world.day < days {
            for i in 0..n {
                if !sim.world.factions[i].alive {
                    continue;
                }
                let f = FactionId(i as u32);
                let obs = Observation { faction: f, world: &sim.world };
                let actions = agents[i].decide(&obs);
                if f == faction {
                    for action in &actions {
                        match action {
                            Action::MoveUnit { to: archipelago_sim::world::Station::Region(r), .. } => {
                                if sim.world.region(*r).owner != faction {
                                    attacks += 1;
                                }
                            }
                            Action::ProposeTreaty { treaty, .. } | Action::AcceptTreaty { treaty, .. }
                                if matches!(
                                    treaty,
                                    archipelago_sim::diplomacy::Treaty::Ceasefire
                                        | archipelago_sim::diplomacy::Treaty::NonAggression
                                        | archipelago_sim::diplomacy::Treaty::Alliance
                                ) =>
                            {
                                peace_seeking += 1;
                            }
                            _ => {}
                        }
                    }
                }
                sim.apply(f, &actions);
            }
            sim.step();
        }
        (attacks, peace_seeking)
    }

    /// A faction built from `route([Military], hawk).route([Economy,
    /// GrandStrategy, Diplomacy], dove)` must attack like the hawk while
    /// its diplomacy reflects the dove's disposition, not the hawk's - a
    /// combination *neither* pure agent alone produces.
    ///
    /// The mixed result is not merely "in between" the two pure runs; it is
    /// *more* peace-seeking than either. `Layer::Diplomacy`'s
    /// `peace_disposition` effect (`evaluate_proposal`/`diplomacy_ai`'s
    /// `outmatched` check) only ever fires once `Faction::casualties`
    /// clears `MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING` - and in the *mixed*
    /// run, the real `World` casualties this reads are driven by whichever
    /// agent actually controls `Layer::Military`, i.e. the hawk's
    /// aggression, not the dove's. So the mixed faction gets the hawk's
    /// casualty count *and* the dove's eagerness to act on it - a
    /// combination that requires both ingredients at once and that neither
    /// pure run, controlling its own military too, ever reaches on its own
    /// (the pure hawk never gets the dove's eagerness; the pure dove never
    /// generates the hawk's casualties). That is the actual point of
    /// building `Layer`/`CompositeAgent`: routing decisions by layer
    /// composes real, causally-connected behaviour across the boundary,
    /// not just independent slices of a fixed script.
    ///
    /// `flipped` - the same two agents with their layer assignments
    /// swapped - is the built-in "confirmed this can fail" check this
    /// module's other tests do by hand-editing and reverting: if `decide`
    /// routed by anything other than which agent actually owns
    /// `Layer::Military` here, `mixed` and `flipped` would look the same.
    /// They don't - `flipped` attacks far less (dove's own low rate) and
    /// seeks peace far less than `mixed` (hawk's diplomacy layer, reading a
    /// low-casualty world its own passive military produced).
    #[test]
    fn mixed_agent_composition_is_a_genuine_mixture() {
        const DAYS: u32 = 300;
        let faction = FactionId(0);

        let hawk = || {
            Box::new(HeuristicAgent::with_peace_disposition(faction, HAWK_CAUTION, HAWK_PEACE_DISPOSITION))
                as Box<dyn Agent + Send + Sync>
        };
        let dove = || {
            Box::new(HeuristicAgent::with_peace_disposition(faction, DOVE_CAUTION, DOVE_PEACE_DISPOSITION))
                as Box<dyn Agent + Send + Sync>
        };

        let (pure_hawk_attacks, pure_hawk_peace) = play(hawk(), faction, DAYS);
        let (pure_dove_attacks, pure_dove_peace) = play(dove(), faction, DAYS);

        let mixed: Box<dyn Agent + Send + Sync> = Box::new(
            CompositeAgent::new(faction)
                .route([Layer::Military], hawk())
                .route([Layer::Economy, Layer::GrandStrategy, Layer::Diplomacy], dove()),
        );
        let (mixed_attacks, mixed_peace) = play(mixed, faction, DAYS);

        let flipped: Box<dyn Agent + Send + Sync> = Box::new(
            CompositeAgent::new(faction)
                .route([Layer::Military], dove())
                .route([Layer::Economy, Layer::GrandStrategy, Layer::Diplomacy], hawk()),
        );
        let (flipped_attacks, flipped_peace) = play(flipped, faction, DAYS);

        println!(
            "pure hawk: attacks={pure_hawk_attacks} peace={pure_hawk_peace} | \
             pure dove: attacks={pure_dove_attacks} peace={pure_dove_peace} | \
             mixed (hawk mil / dove rest): attacks={mixed_attacks} peace={mixed_peace} | \
             flipped (dove mil / hawk rest): attacks={flipped_attacks} peace={flipped_peace}"
        );

        assert!(
            pure_hawk_attacks > pure_dove_attacks,
            "sanity: the pure hawk must attack more than the pure dove over {DAYS} days \
             (hawk={pure_hawk_attacks}, dove={pure_dove_attacks}) or this setup can't demonstrate anything"
        );

        // The mixture attacks like the hawk (its Layer::Military) - nowhere
        // near collapsing to the dove's own, much lower, rate.
        assert!(
            mixed_attacks > pure_dove_attacks * 2,
            "mixed (hawk military) should attack far more than the pure dove ever does on its own: \
             mixed={mixed_attacks}, pure_dove={pure_dove_attacks}"
        );
        assert!(
            mixed_attacks >= pure_hawk_attacks * 3 / 4,
            "mixed (hawk military) should attack roughly as often as the pure hawk: \
             mixed={mixed_attacks}, pure_hawk={pure_hawk_attacks}"
        );

        // ...while its Layer::Diplomacy reflects the dove's disposition
        // reacting to the hawk-driven world, exceeding *both* pure runs -
        // see this test's own doc for why that super-additive result is
        // exactly what a working layer boundary predicts here.
        assert!(
            mixed_peace > pure_hawk_peace,
            "mixed must seek peace more than the pure hawk - its Diplomacy layer comes from the dove, not the \
             hawk's own neutral disposition: mixed={mixed_peace}, pure_hawk={pure_hawk_peace}"
        );
        assert!(
            mixed_peace > pure_dove_peace,
            "mixed must seek peace more than the pure dove - the dove's own passive military never generates the \
             casualties its disposition needs to act on: mixed={mixed_peace}, pure_dove={pure_dove_peace}"
        );

        // The routing direction is what's doing the work, not some
        // incidental property of running two HeuristicAgents together:
        // swapping which one owns Layer::Military swaps the whole profile.
        assert!(
            flipped_attacks < mixed_attacks,
            "flipping which agent owns Layer::Military must sharply cut attacks: \
             flipped={flipped_attacks}, mixed={mixed_attacks}"
        );
        assert!(
            flipped_peace < mixed_peace,
            "flipping which agent owns Layer::Military (and therefore who generates the world's casualties) must \
             cut peace-seeking too: flipped={flipped_peace}, mixed={mixed_peace}"
        );
    }
}
