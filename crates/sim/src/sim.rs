//! Ties every system together into a single deterministic `step` (one day)
//! and the action-intake gate that runs ahead of it.

use crate::action::{self, Action, ActionError};
use crate::construction;
use crate::economy;
use crate::event::Event;
use crate::ids::FactionId;
use crate::logistics;
use crate::military;
use crate::politics;
use crate::rng::Rng;
use crate::scenario;
use crate::world::World;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Outcome {
    Ongoing,
    Victory(FactionId),
    Stalemate,
}

pub struct Simulation {
    pub world: World,
    pub rng: Rng,
}

impl Simulation {
    pub fn new(seed: u64) -> Self {
        Simulation {
            world: scenario::build_world(),
            rng: Rng::new(seed),
        }
    }

    /// Validates and applies a faction's actions, discarding invalid ones
    /// and reporting why. Call this before `step` for each acting faction.
    pub fn apply(&mut self, faction: FactionId, actions: &[Action]) -> Vec<ActionError> {
        let mut errors = Vec::new();
        for &act in actions {
            if let Err(e) = action::apply_action(&mut self.world, faction, act) {
                errors.push(e);
            }
        }
        errors
    }

    /// Advances the simulation by one day, in the fixed tick order from the
    /// spec: economy, construction, supply, movement, combat, recovery,
    /// occupation, politics, devastation recovery, then survival bookkeeping.
    pub fn step(&mut self) -> Vec<Event> {
        let mut events = Vec::new();

        economy::tick_economy(&mut self.world);
        // Construction draws Machinery/Steel from what production just
        // left in stock, the same way every other consumer in the tick does.
        construction::tick_construction(&mut self.world);
        logistics::recompute_supply(&mut self.world);
        logistics::distribute_supply(&mut self.world);
        military::tick_movement(&mut self.world);
        let report = military::tick_combat(&mut self.world, &mut self.rng, &mut events);
        military::tick_recovery(&mut self.world, &report.fought, &mut events);
        military::tick_occupation(&mut self.world, &mut events);
        politics::tick_politics(&mut self.world, &report.casualties);
        // Runs after politics so it sees today's freshly computed
        // unrest/stability, per the Stage 2B recovery formula.
        construction::tick_devastation_recovery(&mut self.world);

        for f_idx in 0..self.world.factions.len() {
            let faction_id = FactionId(f_idx as u32);
            if !self.world.factions[f_idx].alive {
                continue;
            }
            if self.world.region_count(faction_id) == 0 {
                self.world.factions[f_idx].alive = false;
                for unit in self.world.units.iter_mut() {
                    if unit.owner == faction_id {
                        unit.alive = false;
                    }
                }
                events.push(Event::FactionEliminated { faction: faction_id });
            }
        }

        self.world.day += 1;
        events
    }

    /// `Outcome::Victory` once only one faction survives, `Outcome::Stalemate`
    /// once the day limit is reached (or nobody survives), `Outcome::Ongoing`
    /// otherwise.
    pub fn outcome(&self, max_days: u32) -> Outcome {
        let alive: Vec<FactionId> = self
            .world
            .factions
            .iter()
            .filter(|f| f.alive)
            .map(|f| f.id)
            .collect();
        if alive.len() == 1 {
            return Outcome::Victory(alive[0]);
        }
        if alive.is_empty() || self.world.day >= max_days {
            return Outcome::Stalemate;
        }
        Outcome::Ongoing
    }
}
