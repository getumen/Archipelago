//! Fits the camera to the whole map once `RegionLayout` is available (at
//! startup, and again should a future scenario reload ever replace that
//! resource - `Res::is_changed` covers both without this system needing to
//! know which one happened): computes the bounding box of every region's
//! position, pads it, and sets the camera's zoom/position so that box lands
//! inside the part of the window the four UI panels (`setup::spawn_ui`)
//! don't cover.
//!
//! Runs once per `RegionLayout` change and then gets out of the way -
//! `input::mouse_pan_zoom` and `input::keyboard_pan` are the only other
//! systems touching the camera's `Transform`/`Projection`, so "keep manual
//! pan/zoom working after the initial fit" (docs/phase7-spec.md "操作")
//! holds by construction: this system never runs again once
//! `layout.is_changed()` goes false, so it can never fight a player's
//! scroll/drag/arrow-key pan.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use super::setup::MAX_REGION_RADIUS;
use super::{MainCamera, RegionLayout};

/// Screen-space margin (logical pixels) reserved for each UI panel, matching
/// `setup::spawn_ui`'s own fixed geometry (300px-wide side panels each
/// inset 10px, plus their own padding; the bottom event log runs up to
/// `EVENT_LOG_CAPACITY` lines tall). The fitted map must not land under any
/// of them.
const SAFE_LEFT: f32 = 330.0;
const SAFE_RIGHT: f32 = 330.0;
const SAFE_TOP: f32 = 70.0;
const SAFE_BOTTOM: f32 = 230.0;

/// Extra world-space padding around the region bounding box, so the
/// outermost region's own circle and name label aren't flush against the
/// safe area's edge.
const BOX_PADDING: f32 = 60.0;

/// Floor for the computed zoom, so a degenerate single-region map (a
/// near-zero-size bounding box) can't compute a near-zero scale.
const MIN_FIT_SCALE: f32 = 0.05;

pub(super) fn fit_camera_to_map(
    layout: Res<RegionLayout>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut camera: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
) {
    if !layout.is_changed() || layout.0.is_empty() {
        return;
    }
    let Ok(window) = windows.single() else { return };
    let Ok((mut transform, mut projection)) = camera.single_mut() else { return };
    let Projection::Orthographic(ortho) = &mut *projection else { return };

    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for &[x, y] in &layout.0 {
        min = min.min(Vec2::new(x, y));
        max = max.max(Vec2::new(x, y));
    }
    let pad = Vec2::splat(MAX_REGION_RADIUS + BOX_PADDING);
    min -= pad;
    max += pad;
    let box_size = max - min;
    let box_center = (max + min) / 2.0;

    let safe_w = (window.width() - SAFE_LEFT - SAFE_RIGHT).max(100.0);
    let safe_h = (window.height() - SAFE_TOP - SAFE_BOTTOM).max(100.0);

    let scale = (box_size.x / safe_w).max(box_size.y / safe_h).max(MIN_FIT_SCALE);
    ortho.scale = scale;

    // The safe area isn't centered in the window (the panels aren't
    // symmetric), so shift the camera by the offset between the safe area's
    // screen-space center and the window's, converted to world space.
    // Screen space grows down; world space grows up, hence the Y flip.
    let safe_center = Vec2::new(SAFE_LEFT + safe_w / 2.0, SAFE_TOP + safe_h / 2.0);
    let window_center = Vec2::new(window.width() / 2.0, window.height() / 2.0);
    let screen_offset = safe_center - window_center;
    let world_offset = Vec2::new(screen_offset.x, -screen_offset.y) * scale;

    let target = box_center - world_offset;
    transform.translation.x = target.x;
    transform.translation.y = target.y;
}
