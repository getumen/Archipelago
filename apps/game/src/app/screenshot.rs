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

use archipelago_sim::ids::RegionId;

use super::{MainCamera, RegionLayout};

/// Present as a resource only when `--screenshot <path>` was given -
/// `app::run` inserts this conditionally, so `maybe_capture_screenshot`
/// simply no-ops on every frame when it isn't there.
///
/// `open_*`/`supply_overlay`/`camera_focus_region`/`camera_zoom` are
/// verification-only conveniences (`--debug-*` flags, `main.rs`'s own doc) -
/// there is no interactive way to press `L`/`D`/`N` or scroll-zoom before a
/// screenshot fires when nothing is at the keyboard, so `app::run` reads
/// these once at startup to pre-set the corresponding resource/camera
/// framing instead. They never affect anything but initial UI-panel
/// visibility/toggle state or the camera's own `Transform` - not scenario,
/// seed, or any simulated day's outcome.
#[derive(Resource, Clone, Default)]
pub struct ScreenshotConfig {
    pub path: String,
    pub after_frames: u32,
    pub open_diplomacy: bool,
    pub open_newspaper: bool,
    pub supply_overlay: bool,
    /// `--debug-camera-region <index>`: center the camera on this region
    /// instead of the whole-map fit `camera_fit::fit_camera_to_map` computes
    /// by default - a dense map (japan47's 47 prefectures) can render a
    /// short link's overlay color at only a few pixels wide at whole-map
    /// zoom, exactly what a real player would scroll in on with the mouse
    /// wheel (`input::mouse_pan_zoom`) but a screenshot run has no mouse at
    /// all.
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

/// Counts frames since startup and, once `ScreenshotConfig::after_frames`
/// have elapsed, spawns a `Screenshot` of the primary window and stops
/// counting (`triggered`) so it fires exactly once. The observers attached
/// to that entity save the PNG (`save_to_disk`) and then queue `AppExit` -
/// both run once `ScreenshotCaptured` fires (asynchronously, a few frames
/// later, once the GPU readback lands), so the process always exits only
/// after the file is actually written, never before.
pub(super) fn maybe_capture_screenshot(
    mut commands: Commands,
    config: Option<Res<ScreenshotConfig>>,
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
    if *frame_count < config.after_frames {
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
