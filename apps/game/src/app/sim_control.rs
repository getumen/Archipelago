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

use archipelago_sim::sim::Outcome;

use super::{event_text, EventLog, ScenarioMeta, SimRes, SpeedRes, EVENT_LOG_CAPACITY};

pub(super) fn advance_simulation(
    mut sim: ResMut<SimRes>,
    speed: Res<SpeedRes>,
    meta: Res<ScenarioMeta>,
    mut log: ResMut<EventLog>,
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
            log.0.push_front(format!("day {}: {line}", sim.0.world().day));
        }
    }
    while log.0.len() > EVENT_LOG_CAPACITY {
        log.0.pop_back();
    }
}
