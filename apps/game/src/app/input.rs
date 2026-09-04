//! Player input: pan/zoom the camera, pause/resume and speed selection,
//! click-to-inspect a region, `Esc` to deselect
//! (docs/phase7-spec.md "操作"). Every system here only ever writes
//! `SpeedRes`/`SelectedRegion`/the camera's own `Transform`/zoom - never
//! `SimRes` - so no amount of clicking or dragging can reach into the
//! simulation itself (`super::sim_control::advance_simulation` is the only
//! system that touches `SimRes` at all, and it never reads mouse/keyboard
//! state).

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use archipelago_sim::ids::RegionId;

use super::setup::region_radius;
use super::{MainCamera, RegionLayout, SelectedFaction, SelectedRegion, SimRes, Speed, SpeedRes};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
const ZOOM_STEP: f32 = 0.1;
/// Drag distance (screen pixels) below which a left-button press+release is
/// still treated as a click rather than a pan - keeps a barely-jittered
/// click from being swallowed as an accidental drag.
const CLICK_DRAG_TOLERANCE: f32 = 4.0;

pub(super) fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut speed: ResMut<SpeedRes>,
    mut selected: ResMut<SelectedRegion>,
    mut selected_faction: ResMut<SelectedFaction>,
    sim: Res<SimRes>,
) {
    if keys.just_pressed(KeyCode::Space) {
        speed.paused = !speed.paused;
    }
    if keys.just_pressed(KeyCode::Digit1) {
        speed.last_active = Speed::X1;
        speed.paused = false;
    }
    if keys.just_pressed(KeyCode::Digit2) {
        speed.last_active = Speed::X5;
        speed.paused = false;
    }
    if keys.just_pressed(KeyCode::Digit3) {
        speed.last_active = Speed::X20;
        speed.paused = false;
    }
    if keys.just_pressed(KeyCode::Escape) {
        selected.0 = None;
    }
    // `Tab` cycles the faction summary panel through every living faction -
    // not named in docs/phase7-spec.md's own fixed keymap ("`Space` で一時
    // 停止/再開、`1` `2` `3` で速度、`Esc` で選択解除"), but the same section
    // requires "勢力を切り替えられる" for the faction summary panel and
    // names no specific control for it, so this fills that gap without
    // colliding with any spec-mandated key.
    if keys.just_pressed(KeyCode::Tab) {
        let n = sim.0.world().factions.len();
        if n > 0 {
            selected_faction.0 = archipelago_sim::ids::FactionId((selected_faction.0.0 + 1) % n as u32);
        }
    }
}

/// Right-button drag pans the camera; the scroll wheel zooms it (clamped to
/// `MIN_ZOOM..=MAX_ZOOM`) - "ドラッグでパン、ホイールでズーム". Left button
/// is deliberately left untouched here - `region_click_select` owns it, so a
/// plain left-click always means "select", never "start panning".
pub(super) fn mouse_pan_zoom(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut camera: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
) {
    let Ok((mut transform, mut projection)) = camera.single_mut() else { return };

    if mouse_buttons.pressed(MouseButton::Right) {
        let current_scale = orthographic_scale(&projection);
        for ev in motion.read() {
            // Screen-space Y grows downward; world-space Y grows upward, so
            // panning "with the drag" flips the Y delta. Scaled by the
            // current zoom so a pan feels the same speed at any zoom level.
            transform.translation.x -= ev.delta.x * current_scale;
            transform.translation.y += ev.delta.y * current_scale;
        }
    } else {
        motion.clear();
    }

    let mut scroll = 0.0f32;
    for ev in wheel.read() {
        scroll += ev.y;
    }
    if scroll != 0.0
        && let Projection::Orthographic(ortho) = &mut *projection
    {
        ortho.scale = (ortho.scale * (1.0 - scroll * ZOOM_STEP)).clamp(MIN_ZOOM, MAX_ZOOM);
    }
}

fn orthographic_scale(projection: &Projection) -> f32 {
    match projection {
        Projection::Orthographic(ortho) => ortho.scale,
        _ => 1.0,
    }
}

/// Left-click a region to inspect it (`SelectedRegion`); clicking empty map
/// space deselects. A pure nearest-region-within-its-own-radius hit test
/// against `RegionLayout` (no `bevy_picking` in Stage 7A's feature set) -
/// see docs/phase7-spec.md's build-configuration note on the feature list.
pub(super) fn region_click_select(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    layout: Res<RegionLayout>,
    sim: Res<SimRes>,
    mut selected: ResMut<SelectedRegion>,
    mut drag_start: Local<Option<Vec2>>,
) {
    let Ok(window) = windows.single() else { return };
    let Ok((camera, camera_transform)) = camera.single() else { return };

    if mouse_buttons.just_pressed(MouseButton::Left) {
        *drag_start = window.cursor_position();
        return;
    }
    if !mouse_buttons.just_released(MouseButton::Left) {
        return;
    }
    let (Some(start), Some(end)) = (*drag_start, window.cursor_position()) else {
        *drag_start = None;
        return;
    };
    *drag_start = None;
    if start.distance(end) > CLICK_DRAG_TOLERANCE {
        return; // a drag, not a click.
    }

    let Ok(world_pos) = camera.viewport_to_world_2d(camera_transform, end) else { return };

    let mut hit: Option<(RegionId, f32)> = None;
    for region in &sim.0.world().regions {
        let [x, y] = layout.0[region.id.index()];
        let d = Vec2::new(x, y).distance(world_pos);
        let radius = region_radius(region.population);
        if d <= radius {
            match hit {
                Some((_, best_d)) if best_d <= d => {}
                _ => hit = Some((region.id, d)),
            }
        }
    }
    selected.0 = hit.map(|(r, _)| r);
}
