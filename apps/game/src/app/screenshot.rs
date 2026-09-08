//! `--screenshot <path>` support: captures the primary window to a PNG and
//! exits with status 0. Exists so the client can be verified without a
//! human at the screen or a screenshot tool on the host (see this crate's
//! own doc comment / docs/phase7-spec.md "Stage 7A の受け入れ基準" - this
//! machine has none of `import`/`scrot`/`grim`/`maim`/`xwd`/
//! `gnome-screenshot`, so Bevy's own screenshot capability is the only way
//! to confirm the window actually draws pixels).
//!
//! This module only *reads* `SimRes` indirectly (by existing on the same
//! frame timeline as `sim_control::advance_simulation`, which already runs
//! every unpaused frame) - it never calls `SimDriver::tick` itself, so it
//! cannot touch the determinism invariant.

use bevy::app::AppExit;
use bevy::ecs::message::MessageWriter;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot, ScreenshotCaptured};

use archipelago_sim::good::Good;
use archipelago_sim::ids::RegionId;

use super::{MainCamera, MapMode, PlayerFaction, RegionLayout, SelectedUnits, SimRes};

/// `--screenshot-after <frames>` / `--screenshot-at-day <day>` (`main.rs`'s
/// own doc): when the automated capture fires.
///
/// `AfterFrames` (Stage 7A's original, unchanged behavior) counts client
/// frames since startup - which tracks game *days* only when the frame rate
/// and `Speed` happen to hold steady, so it cannot reliably aim a shot at a
/// specific day (confirmed against a real play session: the same
/// `--screenshot-after` value landed on different days across runs).
/// `AtDay` instead waits for `SimRes::world().day` to actually reach the
/// target - the same day number a player would name when describing what
/// they saw, and the target `app::run` reads to decide whether this run
/// needs to advance unattended in the first place (see `app::run`'s own
/// doc for that half).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotTrigger {
    AfterFrames(u32),
    AtDay(u32),
}

/// Present as a resource only when `--screenshot <path>` was given -
/// `app::run` inserts this conditionally, so `maybe_capture_screenshot`
/// simply no-ops on every frame when it isn't there.
///
/// `open_*`/`map_mode`/`camera_focus_region`/`camera_zoom` are
/// verification-only conveniences (`--debug-*` flags, `main.rs`'s own doc) -
/// there is no interactive way to press `M`/`D`/`N` or scroll-zoom before a
/// screenshot fires when nothing is at the keyboard, so `app::run` reads
/// these once at startup to pre-set the corresponding resource/camera
/// framing instead. They never affect anything but initial UI-panel
/// visibility/toggle state or the camera's own `Transform` - not scenario,
/// seed, or any simulated day's outcome.
#[derive(Resource, Clone)]
pub struct ScreenshotConfig {
    pub path: String,
    pub trigger: ScreenshotTrigger,
    pub open_diplomacy: bool,
    pub open_newspaper: bool,
    /// `--debug-open-policy`: Stage 8B's policy panel, same rationale as
    /// `open_diplomacy`/`open_newspaper` - no keyboard at a screenshot run
    /// to press `P` first.
    pub open_policy: bool,
    /// `--debug-map-mode <key>`: which `MapMode` to start in (`political`/
    /// `terrain`/`population`/`industry`/`unrest`/`supply`, `MapMode::key()`),
    /// so any mode - the old standalone supply overlay included, now
    /// `MapMode::Supply` - can be captured unattended. `None` keeps the
    /// ordinary `MapMode::default()` (`Political`) a live run always starts
    /// in.
    pub map_mode: Option<MapMode>,
    /// `--debug-map-mode industry:<good>`: which `Good` an `Industry`-mode
    /// screenshot run starts showing (`main.rs`'s own `good_from_key`) -
    /// `MapMode` alone has no room to carry this (it names a *mode*, not a
    /// mode-plus-commodity), so it rides alongside `map_mode` as its own
    /// field instead. `None` (a bare `industry`, or any other mode) keeps
    /// `ActiveGood::default()` (`Steel`) - the same defined default a live
    /// run always starts with, `--debug-map-mode` given or not.
    pub industry_good: Option<Good>,
    /// `--debug-select-region <region>`: pre-selects a region so Stage 8B's
    /// region panel (action buttons included) renders in a `--screenshot`
    /// run with nothing to click it open.
    pub select_region: Option<RegionId>,
    /// `--debug-select-units`: keeps `SelectedUnits` matching every
    /// *currently* living unit the `--play`ed faction owns, so Stage 8B's
    /// unit panel renders in a `--screenshot` run with nothing to click a
    /// unit marker. No effect without `--play`.
    ///
    /// `sync_debug_selected_units` recomputes this every frame from
    /// `SimRes`'s live world rather than snapshotting it once at startup -
    /// this task's own finding: a `--screenshot-at-day <D>` run for `D > 0`
    /// (the only way to screenshot a squadron at all, since none of this
    /// crate's own scenarios start with one - `docs/phase10-spec.md`'s own
    /// "シナリオに航空部隊を置くのは 10B 以降でよい") ticks the world well past
    /// day 0 before the shot fires, and a one-time day-0 snapshot goes stale
    /// long before then: the day-0 roster may since have died (`--delegate-
    /// military` fighting a real war) and can, by construction, never have
    /// contained a unit recruited afterward. Confirmed by screenshot before
    /// this fix: `--play 関東府 --delegate-military --screenshot-at-day 200
    /// --debug-select-units` rendered the unit panel's own title with zero
    /// rows beneath it - every day-0 unit id in the stale selection had
    /// since died, and the 21 units 関東府 actually owned by day 200
    /// (squadrons included) were never in it to begin with.
    pub select_units: bool,
    /// `--debug-camera-region <index>`: center the camera on this region
    /// instead of the whole-map fit `camera_fit::fit_camera_to_map` computes
    /// by default - a dense map (japan47's 47 prefectures) can render a
    /// short link's overlay color at only a few pixels wide at whole-map
    /// zoom, exactly what a real player would Ctrl+scroll in on
    /// (`input::mouse_pan_zoom`) but a screenshot run has no mouse or
    /// keyboard at all.
    pub camera_focus_region: Option<RegionId>,
    /// `--debug-camera-zoom <scale>`: the orthographic projection's `scale`
    /// to use with `camera_focus_region` - smaller is closer in. Ignored
    /// without `camera_focus_region`.
    pub camera_zoom: f32,
}

/// Overrides the camera's framing every frame once `ScreenshotConfig::
/// camera_focus_region` names a region, running after `camera_fit::
/// fit_camera_to_map`'s own one-time whole-map fit so it always wins - see
/// `ScreenshotConfig`'s own doc for why this exists at all. A no-op
/// whenever there's no `ScreenshotConfig`, or it named no region (the
/// overwhelmingly common case - every `--screenshot` run before Stage 7C
/// used the default whole-map fit unchanged).
pub(super) fn apply_debug_camera(
    config: Option<Res<ScreenshotConfig>>,
    layout: Res<RegionLayout>,
    mut camera: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
) {
    let Some(config) = config else { return };
    let Some(region) = config.camera_focus_region else { return };
    let Some(&[x, y]) = layout.0.get(region.index()) else { return };
    let Ok((mut transform, mut projection)) = camera.single_mut() else { return };
    let Projection::Orthographic(ortho) = &mut *projection else { return };
    transform.translation.x = x;
    transform.translation.y = y;
    ortho.scale = config.camera_zoom;
}

/// Once `ScreenshotConfig::trigger` fires - `AfterFrames(n)`: this many
/// client frames have elapsed; `AtDay(d)`: the simulated day has reached
/// `d` (read from `SimRes`, already advanced this frame by `sim_control::
/// advance_simulation`, which this system runs `.after(..)` - `app::run`'s
/// own doc) - spawns a `Screenshot` of the primary window and stops
/// checking (`triggered`) so it fires exactly once. The observers attached
/// to that entity save the PNG (`save_to_disk`) and then queue `AppExit` -
/// both run once `ScreenshotCaptured` fires (asynchronously, a few frames
/// later, once the GPU readback lands), so the process always exits only
/// after the file is actually written, never before.
pub(super) fn maybe_capture_screenshot(
    mut commands: Commands,
    config: Option<Res<ScreenshotConfig>>,
    sim: Res<SimRes>,
    mut frame_count: Local<u32>,
    mut triggered: Local<bool>,
) {
    let Some(config) = config else {
        return;
    };
    if *triggered {
        return;
    }
    *frame_count += 1;
    let ready = match config.trigger {
        ScreenshotTrigger::AfterFrames(frames) => *frame_count >= frames,
        // `>=`, not `==`: at `Speed::X5`/`X20` several days tick within one
        // frame (`sim_control`'s own module doc - ticks per frame, not per
        // second), so the exact target day can be stepped over inside a
        // single frame rather than ever landing on it precisely.
        ScreenshotTrigger::AtDay(day) => sim.0.world().day >= day,
    };
    if !ready {
        return;
    }
    *triggered = true;

    let path = config.path.clone();
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path))
        .observe(exit_after_screenshot);
}

fn exit_after_screenshot(_captured: On<ScreenshotCaptured>, mut exit: MessageWriter<AppExit>) {
    exit.write(AppExit::Success);
}

/// `--debug-select-units` (`ScreenshotConfig::select_units`'s own doc for
/// why this recomputes every frame instead of snapshotting once at
/// startup): sets `SelectedUnits` to every unit the `--play`ed faction
/// currently owns and has alive, read fresh from `SimRes` each call - a
/// no-op without `--screenshot`/`--play`/`--debug-select-units`, so a live
/// (non-`--screenshot`) run is entirely unaffected (`config` is only ever
/// `Some` under `--screenshot`, matching every other `ScreenshotConfig`-gated
/// system in this module).
pub(super) fn sync_debug_selected_units(
    config: Option<Res<ScreenshotConfig>>,
    player: Res<PlayerFaction>,
    sim: Res<SimRes>,
    mut selected: ResMut<SelectedUnits>,
) {
    let Some(config) = config else {
        return;
    };
    if !config.select_units {
        return;
    }
    let Some(player_faction) = player.0 else {
        return;
    };
    selected.0 = sim.0.world().units.iter().filter(|u| u.alive && u.owner == player_faction).map(|u| u.id.0).collect();
}
