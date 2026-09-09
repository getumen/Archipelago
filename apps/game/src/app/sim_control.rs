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

use super::{event_text, rejection_target_ja, rejection_target_of, EventLog, LastRejection, NewspaperState, RecordConfig, Rejection, ScenarioMeta, SimRes, SpeedRes, EVENT_LOG_CAPACITY};

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
    mut speed: ResMut<SpeedRes>,
    meta: Res<ScenarioMeta>,
    mut log: ResMut<EventLog>,
    mut record: Option<ResMut<RecordConfig>>,
    mut rejection: ResMut<LastRejection>,
    mut news: ResMut<NewspaperState>,
    screenshot: Option<Res<super::screenshot::ScreenshotConfig>>,
) {
    if speed.paused {
        return;
    }
    // Defect fix: a `ScreenshotTrigger::AtDay(d)` run starts at `Speed::X20`
    // (`app::run`'s own doc) - 20 ticks per `Update` call - so this loop
    // used to run clean past `d` before `screenshot::maybe_capture_screenshot`
    // ever got a chance to check anything, and kept right on running every
    // later frame too (nothing here ever looked at the target day at all).
    // Confirmed by hand: `--screenshot-at-day 5` against japan_hex seed 2
    // captured day 200, not day 5 - `screenshot::MIN_RENDER_WARMUP_FRAMES`
    // (10 frames) times 20 ticks/frame lands exactly there. A screenshot
    // that silently shows the wrong day is worse than one that's visibly
    // black (this crate's own module doc on `MIN_RENDER_WARMUP_FRAMES` /
    // CLAUDE.md's "検証についての教訓") - so once an `AtDay` target is
    // armed, this loop now stops ticking, mid-frame if need be, the instant
    // `world.day` reaches it, and parks the run paused there
    // (`speed.paused = true`) so no *later* frame ever ticks past it
    // either. `maybe_capture_screenshot` then only has to wait out its own
    // render-warmup floor against a world that has already stopped
    // changing, never a moving target - the day it captures is always
    // exactly the requested one.
    //
    // The `>= target` check sits at the *bottom* of the loop, right after
    // the tick that could have just reached it, not only at the top of the
    // next iteration - deliberately, not merely for tidiness: `target`
    // landing exactly on a `ticks_per_frame` multiple (20, 40, ... - e.g.
    // `--screenshot-at-day 200`) means the tick that reaches it is also the
    // very last iteration this frame's `for` loop ever runs, so a
    // top-of-loop-only check would never get a next iteration to catch it
    // in - `speed.paused` would stay `false` for one extra frame (the day
    // captured would still be exactly right, since no *further* ticking
    // happens either way, but a screenshot landing on that exact frame -
    // which `MIN_RENDER_WARMUP_FRAMES` lining up with a round target day
    // makes entirely possible - would show "実行中" over a world that has
    // in fact already stopped). Confirmed by hand: before this adjustment,
    // `--screenshot-at-day 200` against this same fixture captured the
    // right day but the wrong status text for exactly this reason.
    let day_target = screenshot.as_ref().and_then(|c| match c.trigger {
        super::screenshot::ScreenshotTrigger::AtDay(day) => Some(day),
        super::screenshot::ScreenshotTrigger::AfterFrames(_) => None,
    });
    // `codex review` (P2): day 0 is a legal target - the world starts there,
    // so `--screenshot-at-day 0` asks for the untouched opening position. The
    // post-tick check below alone would advance past it and pause on day 1,
    // silently capturing a different day than was asked for: the same class
    // of quiet lie the warm-up floor produced before it. Checked here too,
    // before any tick, and kept below for every later target.
    if let Some(target) = day_target {
        if sim.0.world().day >= target {
            speed.paused = true;
            return;
        }
    }
    let ticks = speed.last_active.ticks_per_frame();
    for _ in 0..ticks {
        if sim.0.outcome(meta.max_days) != Outcome::Ongoing {
            // Fail loudly (docs/conventions.md's no-fallback rule) instead
            // of leaving `maybe_capture_screenshot` waiting forever: once
            // the simulation reaches a terminal `Outcome` (`Victory`/
            // `Stalemate`), `world.day` will never advance again, so an
            // `--screenshot-at-day` target still short of the current day
            // can never be reached - and quietly capturing whatever day the
            // run actually stopped on would repeat the exact "shows a real,
            // valid frame of the *wrong* day" defect this fix exists to
            // remove, just with a different cause.
            if let Some(target) = day_target {
                let day = sim.0.world().day;
                if day < target {
                    eprintln!(
                        "archipelago-game: error: --screenshot-at-day {target} was requested, but the \
                         simulation reached a terminal outcome ({:?}) at day {day} - day {target} is never \
                         reached, so no screenshot was captured.",
                        sim.0.outcome(meta.max_days),
                    );
                    std::process::exit(1);
                }
            }
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
            let errors = sim.0.last_human_action_errors();
            rejection.0 = errors
                .iter()
                .map(|(action, error)| Rejection { target: rejection_target_of(action), reason: crate::action_codec::action_error_ja(*error) })
                .collect();
            // Defect fix (docs/phase7-spec.md "命令の可否を隠さない"): `rejection.0`
            // above is overwritten every tick, so a rejection used to be
            // visible in the per-panel corner box for exactly the one tick
            // it happened, then gone for good even though the player might
            // not be looking at that panel that instant. A rejection is not
            // simulation history (nothing about it happened in the game
            // world - it does not belong in `archipelago_sim::event::Event`,
            // and `Simulation::apply`'s `ActionError` return shape is a
            // fixed RL-agent API contract, mvp-spec.md §5, not something to
            // route differently for a human player), so it goes into the
            // same durable, scrolling `EventLog` the bottom panel already
            // shows real `Event`s in - tagged "却下" so it still reads as
            // feedback about a rejected order, not as something that
            // happened.
            for (action, error) in errors {
                log.0.push_front(format!(
                    "{}日目: 却下（{}）: {}",
                    sim.0.world().day,
                    rejection_target_ja(rejection_target_of(action)),
                    crate::action_codec::action_error_ja(*error),
                ));
            }
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

        // See this function's own doc above `day_target` for why this sits
        // here (right after the tick that could have just reached it)
        // rather than only at the top of the next iteration.
        if let Some(target) = day_target {
            if sim.0.world().day >= target {
                speed.paused = true;
                break;
            }
        }
    }
    while log.0.len() > EVENT_LOG_CAPACITY {
        log.0.pop_back();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use archipelago_sim::action::{Action, ActionError};
    use archipelago_sim::ids::FactionId;
    use archipelago_sim::scenario;
    use archipelago_sim::world::{Domain, Station};

    use crate::action_codec::action_error_ja;
    use crate::sim_driver::{SimDriver, Speed};

    fn run<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
        let mut system = IntoSystem::into_system(system);
        system.initialize(world);
        system.run((), world).unwrap();
    }

    fn advance_one_tick(world: &mut World) {
        world.insert_resource(SpeedRes { last_active: Speed::X1, paused: false });
        world.insert_resource(ScenarioMeta { name: "mvp".to_string(), max_days: 720 });
        world.insert_resource(EventLog::default());
        world.insert_resource(LastRejection::default());
        world.insert_resource(NewspaperState::default());
        run(world, advance_simulation);
    }

    /// Stage 10 follow-up (this task's own ask, "a rejected order must tell
    /// the player why"): a squadron redeployed to a region the player does
    /// not own must come back through `LastRejection` with `apply_move`'s
    /// own `Station::Airfield` arm's real reason
    /// (`ActionError::RegionNotOwned`), not silence - exactly the same
    /// plumbing (`rejection_target_of`/`action_error_ja`) every other
    /// domain's illegal order already goes through, now exercised for a
    /// real `Domain::Air` unit end to end (recruit it, then misorder it).
    ///
    /// Confirmed this can actually fail: temporarily cleared `rejection.0`
    /// unconditionally instead of assigning `sim.0.last_human_action_errors()`'s
    /// mapped `Vec` (i.e. made `advance_simulation` never populate
    /// `LastRejection` at all) and re-ran - the last assertion below failed
    /// (`rejection.0` stayed empty). Reverted before committing.
    #[test]
    fn illegal_air_redeploy_surfaces_its_rejection_reason() {
        let mut world = World::new();
        let mut sim = SimRes(SimDriver::new_with_player(scenario::build_world(), 1, Some(FactionId(0)), None));
        let kanto = sim.0.world().faction(FactionId(0)).capital;
        sim.0.push_human_action(Action::RecruitUnit { region: kanto, domain: Domain::Air });
        sim.0.tick();
        assert!(sim.0.last_human_action_errors().is_empty(), "recruiting a squadron at the player's own capital must succeed: {:?}", sim.0.last_human_action_errors());
        let squadron = sim
            .0
            .world()
            .units
            .iter()
            .find(|u| u.owner == FactionId(0) && u.alive && u.station.domain() == Domain::Air)
            .expect("the RecruitUnit(Air) applied above must have created a living squadron")
            .id;

        let foreign_capital = sim.0.world().faction(FactionId(1)).capital;
        let foreign_node = sim.0.world().airfield_node(foreign_capital).expect("every mvp region has an airfield node").id;
        sim.0.push_human_action(Action::MoveUnit { unit: squadron, to: Station::Airfield(foreign_node) });

        world.insert_resource(sim);
        advance_one_tick(&mut world);

        let sim = world.resource::<SimRes>();
        assert_eq!(
            sim.0.last_human_action_errors(),
            &[(Action::MoveUnit { unit: squadron, to: Station::Airfield(foreign_node) }, ActionError::RegionNotOwned)],
            "redeploying a squadron onto another faction's airfield must be rejected as RegionNotOwned"
        );
        let rejection = world.resource::<LastRejection>();
        assert!(
            rejection.0.iter().any(|r| r.reason == action_error_ja(ActionError::RegionNotOwned)),
            "the rejection must surface the real reason text to the player, got {:?}",
            rejection.0.iter().map(|r| r.reason).collect::<Vec<_>>()
        );
    }

    /// Defect fix (this task's own ask, "rejected orders vanish"): before
    /// this fix, `LastRejection` was the *only* place a rejection ever
    /// appeared, and `advance_simulation` above overwrites it every tick -
    /// so a rejection shown in a panel's corner box for the one tick it
    /// happened was completely gone the very next tick, with no trace
    /// anywhere a player could scroll back to. This drives two ticks: the
    /// first submits an illegal order (same setup as the test above), the
    /// second submits nothing. By the second tick `LastRejection` has
    /// already gone back to empty (unchanged, still correct - the corner
    /// box is about *this* tick's orders), but `EventLog` - the same
    /// durable, scrolling log real `Event`s already go into - must still
    /// carry a line about the first tick's rejection.
    ///
    /// Confirmed this can actually fail: removed the `for (action, error) in
    /// errors { log.0.push_front(...) }` loop from `advance_simulation`
    /// (restoring the pre-fix behavior) and re-ran - `LastRejection` was
    /// still correctly populated after tick 1 (the older test above still
    /// passed), but `log.0` never contained the rejection line at all, so
    /// this test's last assertion failed. Reverted before committing.
    /// `--screenshot-at-day 0` asks for the untouched opening position, and
    /// the world already starts there. Checking the target only *after*
    /// `tick()` advanced past it and paused on day 1 - a screenshot that
    /// looks perfectly fine and is of the wrong day (`codex review`, P2).
    ///
    /// Same class as the warm-up floor this flag was just fixed for: a
    /// verification tool quietly capturing something other than what was
    /// asked. CLAUDE.md's own record of pixel statistics passing on a broken
    /// screen is why the day, not the pixels, is what gets asserted here.
    ///
    /// **Confirmed this test can fail.** Removing the pre-tick target check
    /// in `advance_simulation` leaves `world().day == 1` instead of `0`,
    /// tripping the assertion below. Restored, and it passes.
    #[test]
    fn a_day_zero_screenshot_target_does_not_tick_past_it() {
        let mut world = World::new();
        world.insert_resource(SimRes(SimDriver::new(scenario::build_world(), 1)));
        world.insert_resource(SpeedRes { last_active: Speed::X20, paused: false });
        world.insert_resource(ScenarioMeta { name: "mvp".to_string(), max_days: 720 });
        world.insert_resource(EventLog::default());
        world.insert_resource(LastRejection::default());
        world.insert_resource(NewspaperState::default());
        world.insert_resource(super::super::screenshot::ScreenshotConfig {
            path: "/dev/null".to_string(),
            trigger: super::super::screenshot::ScreenshotTrigger::AtDay(0),
            open_diplomacy: false,
            open_newspaper: false,
            open_policy: false,
            map_mode: None,
            industry_good: None,
            select_region: None,
            select_units: false,
            camera_focus_region: None,
            camera_zoom: 1.0,
            debug_force_blank: false,
        });

        run(&mut world, advance_simulation);

        assert_eq!(
            world.resource::<SimRes>().0.world().day,
            0,
            "a day-0 screenshot target must hold the world at day 0, not tick past it and pause on day 1"
        );
        assert!(
            world.resource::<SpeedRes>().paused,
            "reaching the target must pause, so the status text matches the captured day"
        );
    }

    #[test]
    fn rejection_survives_past_the_tick_it_happened() {
        let mut world = World::new();
        let mut sim = SimRes(SimDriver::new_with_player(scenario::build_world(), 1, Some(FactionId(0)), None));
        let kanto = sim.0.world().faction(FactionId(0)).capital;
        sim.0.push_human_action(Action::RecruitUnit { region: kanto, domain: Domain::Air });
        sim.0.tick();
        let squadron = sim
            .0
            .world()
            .units
            .iter()
            .find(|u| u.owner == FactionId(0) && u.alive && u.station.domain() == Domain::Air)
            .expect("the RecruitUnit(Air) applied above must have created a living squadron")
            .id;
        let foreign_capital = sim.0.world().faction(FactionId(1)).capital;
        let foreign_node = sim.0.world().airfield_node(foreign_capital).expect("every mvp region has an airfield node").id;
        sim.0.push_human_action(Action::MoveUnit { unit: squadron, to: Station::Airfield(foreign_node) });

        world.insert_resource(sim);
        advance_one_tick(&mut world); // tick 1: the illegal MoveUnit is rejected here.

        // tick 2: no human action submitted at all - `LastRejection` goes
        // back to empty (correctly - nothing was rejected *this* tick), the
        // same way it always has.
        world.insert_resource(SpeedRes { last_active: Speed::X1, paused: false });
        run(&mut world, advance_simulation);

        let rejection = world.resource::<LastRejection>();
        assert!(
            rejection.0.is_empty(),
            "sanity: with no human action submitted this tick, the per-tick corner box must go back to empty: got {:?}",
            rejection.0.iter().map(|r| r.reason).collect::<Vec<_>>()
        );

        let log = world.resource::<EventLog>();
        assert!(
            log.0.iter().any(|line| line.contains(action_error_ja(ActionError::RegionNotOwned))),
            "the rejection from tick 1 must still be visible in the durable event log on tick 2, got {:?}",
            log.0
        );
    }
}
