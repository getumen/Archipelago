//! Ties every system together into a single deterministic `step` (one day)
//! and the action-intake gate that runs ahead of it.

use std::time::{Duration, Instant};

use crate::action::{self, Action, ActionError};
use crate::construction;
use crate::diplomacy;
use crate::economy;
use crate::event::Event;
use crate::focus;
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

/// Per-system wall-clock time spent in one `Simulation::step_timed` call,
/// grouped the way docs/phase6-spec.md's `--bench` names them ("システム
/// 別: economy / logistics / military / politics"). Every tick system falls
/// into exactly one bucket - see `step_timed`'s body for which.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepTimings {
    pub economy: Duration,
    pub logistics: Duration,
    pub military: Duration,
    pub politics: Duration,
}

impl StepTimings {
    pub fn total(&self) -> Duration {
        self.economy + self.logistics + self.military + self.politics
    }
}

pub struct Simulation {
    pub world: World,
    pub rng: Rng,
}

impl Simulation {
    /// Builds a fresh `Simulation` on the embedded default scenario
    /// (`scenario::build_world`, `scenarios/mvp.json`) - unchanged since
    /// before Stage 6A, so every existing caller keeps compiling and
    /// behaving exactly as before. Use `with_world` to run a `--scenario`-
    /// loaded (or otherwise custom) map instead.
    pub fn new(seed: u64) -> Self {
        Simulation::with_world(scenario::build_world(), seed)
    }

    /// Builds a `Simulation` on an already-built `World` - Stage 6A
    /// (docs/phase6-spec.md "Stage 6A"), what `--scenario <path>` (headless
    /// and the API server) uses once `scenario::load_file` has parsed and
    /// validated the file. `world` is assumed already valid; `scenario::
    /// load_str`/`load_file` are the only supported way to get one from
    /// untrusted data.
    pub fn with_world(world: World, seed: u64) -> Self {
        Simulation { world, rng: Rng::new(seed) }
    }

    /// Validates and applies a faction's actions, discarding invalid ones
    /// and reporting why. Call this before `step` for each acting faction.
    pub fn apply(&mut self, faction: FactionId, actions: &[Action]) -> Vec<ActionError> {
        let mut errors = Vec::new();
        for act in actions.iter().cloned() {
            if let Err(e) = action::apply_action(&mut self.world, faction, act) {
                errors.push(e);
            }
        }
        errors
    }

    /// Advances the simulation by one day. `step_timed`'s thin wrapper,
    /// discarding the per-system timings - see it for the actual tick order
    /// and every step's own doc.
    pub fn step(&mut self) -> Vec<Event> {
        self.step_timed().0
    }

    /// `step`, additionally returning how long each of the four
    /// docs/phase6-spec.md `--bench` buckets (economy / logistics /
    /// military / politics) took this tick. Exactly the same tick order and
    /// logic as `step` - the `Instant::now()` calls around each system add
    /// negligible overhead and read nothing any system depends on, so this
    /// is not a second implementation to keep in sync, just `step` with a
    /// stopwatch: sea control, imports, economy, construction, supply,
    /// movement, combat (land, then naval), recovery, occupation, politics,
    /// devastation recovery, then survival bookkeeping.
    pub fn step_timed(&mut self) -> (Vec<Event>, StepTimings) {
        let mut events = Vec::new();
        let mut timings = StepTimings::default();

        // Stage 3B (docs/phase3-spec.md "Stage 3B"): diplomacy maintenance
        // runs first, so a `NonAggression` notice period expiring today (or
        // any other stance/treaty change queued by an action applied before
        // this `step()` call) is fully resolved before combat, occupation
        // and trade decide anything off `world.diplomacy` today.
        let t0 = Instant::now();
        diplomacy::tick_diplomacy(&mut self.world, &mut events);

        // Stage 3C (docs/phase3-spec.md "Stage 3C — 国家方針"): counts down
        // every faction's in-progress `NationalFocus` switch, the same
        // "maintenance runs before anything reads today's state" slot
        // `tick_diplomacy`'s own countdowns occupy.
        focus::tick_national_focus(&mut self.world);
        timings.politics += t0.elapsed();

        // Stage 2D: sea control is recomputed first, from fleet positions
        // as they stood at the end of the previous tick's movement — the
        // same "snapshot before this tick's changes" convention `contested`
        // (recompute_supply, tick_imports) already follows for land. Both
        // blockade effects below (import capacity, strait throughput) read
        // this snapshot.
        let t1 = Instant::now();
        naval::tick_sea_control(&mut self.world);
        timings.military += t1.elapsed();

        // Imports land before production's civilian ration is served, so a
        // faction that can't feed itself domestically is actually helped by
        // them the same day (Stage 2C's fix for the Stage 2A structural
        // famine) rather than a day late. They're paid for out of
        // yesterday's Machinery stock, same as every other consumer that
        // runs later in this same tick draws on stock as it stood at its turn.
        let t2 = Instant::now();
        trade::tick_imports(&mut self.world);
        economy::tick_economy(&mut self.world);
        // Construction draws Machinery/Steel from what production just
        // left in stock, the same way every other consumer in the tick does.
        construction::tick_construction(&mut self.world);
        timings.economy += t2.elapsed();

        let t3 = Instant::now();
        logistics::recompute_supply(&mut self.world);
        logistics::distribute_supply(&mut self.world);
        timings.logistics += t3.elapsed();

        let t4 = Instant::now();
        military::tick_movement(&mut self.world);
        timings.military += t4.elapsed();

        // Stage 3A (docs/phase3-spec.md "領土を得た"/"領土を失った"): snapshot
        // each faction's owned-region count before occupation resolves so
        // `politics::tick_politics` can see today's net territorial change.
        // Only `military::tick_occupation` below can flip a region's owner
        // within a single tick.
        let region_count_before: Vec<usize> = (0..self.world.factions.len())
            .map(|i| self.world.region_count(FactionId(i as u32)))
            .collect();

        let t4b = Instant::now();
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
        timings.military += t4b.elapsed();

        let region_delta: Vec<i32> = (0..self.world.factions.len())
            .map(|i| {
                let after = self.world.region_count(FactionId(i as u32)) as i32;
                after - region_count_before[i] as i32
            })
            .collect();
        let t5 = Instant::now();
        politics::tick_politics(&mut self.world, &casualties, &region_delta, &mut events);
        timings.politics += t5.elapsed();
        // Runs after politics so it sees today's freshly computed
        // unrest/stability, per the Stage 2B recovery formula.
        let t6 = Instant::now();
        construction::tick_devastation_recovery(&mut self.world);
        timings.economy += t6.elapsed();

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
        (events, timings)
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
