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

/// Present as a resource only when `--screenshot <path>` was given -
/// `app::run` inserts this conditionally, so `maybe_capture_screenshot`
/// simply no-ops on every frame when it isn't there.
#[derive(Resource, Clone)]
pub struct ScreenshotConfig {
    pub path: String,
    pub after_frames: u32,
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
