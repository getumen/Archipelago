//! Ties every system together into a single deterministic `step` (one day)
//! and the action-intake gate that runs ahead of it.

use crate::action::{self, Action, ActionError};
use crate::construction;
use crate::diplomacy;
use crate::economy;
use crate::event::Event;
use crate::ids::FactionId;
use crate::logistics;
use crate::military;
use crate::naval;
use crate::politics;
use crate::rng::Rng;
use crate::scenario;
use crate::trade;
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
    /// spec: sea control, imports, economy, construction, supply, movement,
    /// combat (land, then naval), recovery, occupation, politics,
    /// devastation recovery, then survival bookkeeping.
    pub fn step(&mut self) -> Vec<Event> {
        let mut events = Vec::new();

        // Stage 3B (docs/phase3-spec.md "Stage 3B"): diplomacy maintenance
        // runs first, so a `NonAggression` notice period expiring today (or
        // any other stance/treaty change queued by an action applied before
        // this `step()` call) is fully resolved before combat, occupation
        // and trade decide anything off `world.diplomacy` today.
        diplomacy::tick_diplomacy(&mut self.world, &mut events);

        // Stage 2D: sea control is recomputed first, from fleet positions
        // as they stood at the end of the previous tick's movement — the
        // same "snapshot before this tick's changes" convention `contested`
        // (recompute_supply, tick_imports) already follows for land. Both
        // blockade effects below (import capacity, strait throughput) read
        // this snapshot.
        naval::tick_sea_control(&mut self.world);

        // Imports land before production's civilian ration is served, so a
        // faction that can't feed itself domestically is actually helped by
        // them the same day (Stage 2C's fix for the Stage 2A structural
        // famine) rather than a day late. They're paid for out of
        // yesterday's Machinery stock, same as every other consumer that
        // runs later in this same tick draws on stock as it stood at its turn.
        trade::tick_imports(&mut self.world);
        economy::tick_economy(&mut self.world);
        // Construction draws Machinery/Steel from what production just
        // left in stock, the same way every other consumer in the tick does.
        construction::tick_construction(&mut self.world);
        logistics::recompute_supply(&mut self.world);
        logistics::distribute_supply(&mut self.world);
        military::tick_movement(&mut self.world);

        // Stage 3A (docs/phase3-spec.md "領土を得た"/"領土を失った"): snapshot
        // each faction's owned-region count before occupation resolves so
        // `politics::tick_politics` can see today's net territorial change.
        // Only `military::tick_occupation` below can flip a region's owner
        // within a single tick.
        let region_count_before: Vec<usize> = (0..self.world.factions.len())
            .map(|i| self.world.region_count(FactionId(i as u32)))
            .collect();

        let land_report = military::tick_combat(&mut self.world, &mut self.rng, &mut events);
        let naval_report = naval::tick_naval_combat(&mut self.world, &mut self.rng, &mut events);
        // Stage 2D: land and naval combat resolve independently (different
        // topologies, `Station::Region` vs `Station::Sea` units never
        // share a battle) but feed the *same* single recovery/politics pass
        // below — a fleet that broke in naval combat routs or sinks through
        // exactly the mechanism a broken land unit does, and a faction's
        // war support must see both domains' casualties, not just land's.
        let mut fought = land_report.fought;
        for (i, &f) in naval_report.fought.iter().enumerate() {
            if f {
                fought[i] = true;
            }
        }
        let mut casualties = land_report.casualties;
        for (i, &c) in naval_report.casualties.iter().enumerate() {
            casualties[i] += c;
        }

        military::tick_recovery(&mut self.world, &fought, &mut events);
        military::tick_occupation(&mut self.world, &mut events);

        let region_delta: Vec<i32> = (0..self.world.factions.len())
            .map(|i| {
                let after = self.world.region_count(FactionId(i as u32)) as i32;
                after - region_count_before[i] as i32
            })
            .collect();
        politics::tick_politics(&mut self.world, &casualties, &region_delta, &mut events);
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
