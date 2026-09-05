//! The one system allowed to call `SimDriver::tick`. THE INVARIANT
//! (docs/phase7-spec.md §0, `crate::sim_driver`'s own module doc): this
//! system never reads `Time`/`Res<Time>` to decide how many ticks to run -
//! only `SpeedRes` (set exclusively by `Space`/`1`/`2`/`3` in
//! `super::input`), which counts ticks per *frame*, never per second.
//! Frame rate can vary all it likes; the number of simulated days per real
//! second changes with it (by design - "速度変更は「1 フレームあたり何 tick
//! 進めるか」であって、tick の中身を変えない"), but the days themselves are
//! always identical to what `SimDriver::tick` alone would produce.

use bevy::prelude::*;

use archipelago_agents::newspaper::NEWSPAPER_INTERVAL_DAYS;
use archipelago_sim::sim::Outcome;

use super::{event_text, rejection_target_of, EventLog, LastRejection, NewspaperState, RecordConfig, Rejection, ScenarioMeta, SimRes, SpeedRes, EVENT_LOG_CAPACITY};

/// Also where Stage 7B's `--record`/rejection-surfacing hooks in
/// (docs/phase7-spec.md "決定論" / "命令の可否を隠さない"): after every
/// `SimDriver::tick()`, this is the one place that knows exactly what the
/// human/replay faction's `decide()` returned and what `Simulation::apply`
/// did with it, so it's also the one place that appends to `RecordConfig`
/// and refreshes `LastRejection` (Stage 8B: each rejection tagged with the
/// panel that issued it, via `rejection_target_of`) - `SimDriver::
/// last_human_actions`/`last_human_action_errors` never leave `SimRes` any
/// other way.
pub(super) fn advance_simulation(
    mut sim: ResMut<SimRes>,
    speed: Res<SpeedRes>,
    meta: Res<ScenarioMeta>,
    mut log: ResMut<EventLog>,
    mut record: Option<ResMut<RecordConfig>>,
    mut rejection: ResMut<LastRejection>,
    mut news: ResMut<NewspaperState>,
) {
    if speed.paused {
        return;
    }
    let ticks = speed.last_active.ticks_per_frame();
    for _ in 0..ticks {
        if sim.0.outcome(meta.max_days) != Outcome::Ongoing {
            break;
        }
        let events = sim.0.tick();
        for event in &events {
            let line = event_text::format_event(sim.0.world(), event);
            log.0.push_front(format!("{}日目: {line}", sim.0.world().day));
        }

        // Stage 7C's newspaper panel (docs/phase7-spec.md "5. 新聞"):
        // accumulates this tick's events into the current reporting period,
        // exactly mirroring `apps/headless`'s own `--newspaper` loop body -
        // this is the one place in the client that already has both the
        // tick's raw `events` and the post-tick `World` in hand.
        news.period_events.extend(events.iter().cloned());
        if sim.0.world().day % NEWSPAPER_INTERVAL_DAYS == 0 {
            super::newspaper::publish_issue(&mut news, sim.0.world());
        }

        if sim.0.human_faction().is_some() {
            rejection.0 = sim
                .0
                .last_human_action_errors()
                .iter()
                .map(|(action, error)| Rejection { target: rejection_target_of(action), reason: crate::action_codec::action_error_ja(*error) })
                .collect();
            if let Some(record) = &mut record {
                record.days.push(sim.0.last_human_actions().to_vec());
                // Rewritten in full after every tick that grows it, not
                // appended - see `crate::action_codec`'s own doc for why (a
                // whole-file rewrite is simpler and the recording is small).
                if let Err(e) = crate::action_codec::write_record(std::path::Path::new(&record.path), &record.days) {
                    eprintln!("archipelago-game: warning: could not write --record {}: {e}", record.path);
                }
            }
        }
    }
    while log.0.len() > EVENT_LOG_CAPACITY {
        log.0.pop_back();
    }
}
