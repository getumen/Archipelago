//! Player input: pan/zoom the camera, pause/resume and speed selection,
//! click-to-select/order on the map, `Esc` to deselect
//! (docs/phase7-spec.md "操作"), and Stage 7B's ("Stage 7B — 遊ぶ") keyboard
//! policy changes and diplomacy panel.
//!
//! Every system here only ever writes `SpeedRes`/`SelectedRegion`/
//! `SelectedUnits`/`MenuRegion`/`DiplomacyPanel`/`ActiveGood`/the camera's
//! own `Transform`/zoom directly - and reaches `SimRes` **only** through
//! `SimDriver::push_human_action`, exactly the same `Action`-queueing door
//! `HumanAgent::push` (docs/design.md §14: "人間も AI と同じ入口から世界に
//! 触る") opens for anything else. No system here ever calls
//! `SimDriver::tick` or mutates `Simulation`/`World` directly - that stays
//! `sim_control::advance_simulation`'s job alone.
//!
//! **No client-side legality pre-filtering beyond what a player can already
//! see on screen** (docs/phase7-spec.md "命令の可否を隠さない"): a move
//! order is issued to whatever region/sea-zone was clicked regardless of
//! adjacency, ownership of the destination, or contested status - and a
//! menu/policy action is issued regardless of whether the region is
//! currently biuldable, contested, or affordable. `Simulation::apply` is the
//! only judge; `sim_control::advance_simulation` reads its rejections back
//! out and `ui::update_player_panel` shows the reason. The only clamping
//! done here is to a policy value's own publicly-known numeric range (e.g.
//! conscription is always `0.0..=1.0`) - not hidden game state, so clamping
//! it client-side isn't hiding anything a rejection would have revealed.

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use archipelago_sim::action::Action;
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Stance, Treaty, ALL_TREATIES};
use archipelago_sim::focus::ALL_FOCI;
use archipelago_sim::good::{ALL_GOODS, GOOD_COUNT};
use archipelago_sim::ids::RegionId;
use archipelago_sim::world::{Domain, Station};

use super::setup::{region_radius, sea_zone_radius};
use super::{
    ActiveGood, DiplomacyPanel, MainCamera, MenuRegion, PlayerFaction, RegionLayout, SeaZoneCenters,
    SelectedFaction, SelectedRegion, SelectedUnits, SimRes, Speed, SpeedRes, UnitMarker,
};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
const ZOOM_STEP: f32 = 0.1;
/// Drag distance (screen pixels) below which a button press+release is
/// still treated as a click rather than a pan/drag - keeps a barely-jittered
/// click from being swallowed as an accidental drag.
const CLICK_DRAG_TOLERANCE: f32 = 4.0;

/// World-space click radius around a unit marker's rendered position.
const UNIT_CLICK_RADIUS: f32 = 14.0;

const CONSCRIPTION_STEP: f32 = 0.05;
const CIVILIAN_RATION_STEP: f32 = 0.05;
const PRIORITY_STEP: f32 = 0.1;
const IMPORT_PLAN_STEP: f32 = 5.0;

pub(super) fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut speed: ResMut<SpeedRes>,
    mut selected_region: ResMut<SelectedRegion>,
    mut selected_units: ResMut<SelectedUnits>,
    mut selected_faction: ResMut<SelectedFaction>,
    mut menu: ResMut<MenuRegion>,
    mut diplomacy: ResMut<DiplomacyPanel>,
    mut active_good: ResMut<ActiveGood>,
    player: Res<PlayerFaction>,
    mut sim: ResMut<SimRes>,
) {
    // The recruit/build menu, when open, takes over the number row -
    // "自国地域を右クリック: その地域で可能な命令のメニュー" - and closes on
    // any of its own keys or `Esc`.
    if let Some(region) = menu.0 {
        handle_menu_keys(&keys, region, active_good.0, &mut menu, &mut sim);
        return;
    }

    // The diplomacy panel similarly takes over its own keys while open -
    // "条約の提案・受諾・拒否は外交パネルから".
    if diplomacy.open {
        handle_diplomacy_keys(&keys, &mut diplomacy, &player, &mut sim);
        return;
    }

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
        selected_region.0 = None;
        selected_units.0.clear();
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

    let Some(player_faction) = player.0 else { return };

    // Everything below only makes sense for a `--play`ed faction - an
    // observing-only run (`player.0 == None`) never reaches here.
    if keys.just_pressed(KeyCode::KeyD) {
        diplomacy.open = true;
        if diplomacy.target.is_none() {
            diplomacy.target = sim.0.world().factions.iter().find(|f| f.id != player_faction && f.alive).map(|f| f.id);
        }
        return;
    }
    if keys.just_pressed(KeyCode::KeyG) {
        let cur = active_good.0.index();
        active_good.0 = ALL_GOODS[(cur + 1) % GOOD_COUNT];
    }
    if keys.just_pressed(KeyCode::KeyF) {
        let cur = sim.0.world().faction(player_faction).national_focus;
        let idx = ALL_FOCI.iter().position(|&f| f == cur).unwrap_or(0);
        let next = ALL_FOCI[(idx + 1) % ALL_FOCI.len()];
        sim.0.push_human_action(Action::SetNationalFocus(next));
    }
    if keys.just_pressed(KeyCode::Minus) {
        let cur = sim.0.world().faction(player_faction).conscription;
        sim.0.push_human_action(Action::SetConscription((cur - CONSCRIPTION_STEP).clamp(0.0, 1.0)));
    }
    if keys.just_pressed(KeyCode::Equal) {
        let cur = sim.0.world().faction(player_faction).conscription;
        sim.0.push_human_action(Action::SetConscription((cur + CONSCRIPTION_STEP).clamp(0.0, 1.0)));
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        let cur = sim.0.world().faction(player_faction).civilian_ration;
        sim.0.push_human_action(Action::SetCivilianRation(
            (cur - CIVILIAN_RATION_STEP).clamp(archipelago_sim::balance::CIVILIAN_RATION_MIN, archipelago_sim::balance::CIVILIAN_RATION_MAX),
        ));
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        let cur = sim.0.world().faction(player_faction).civilian_ration;
        sim.0.push_human_action(Action::SetCivilianRation(
            (cur + CIVILIAN_RATION_STEP).clamp(archipelago_sim::balance::CIVILIAN_RATION_MIN, archipelago_sim::balance::CIVILIAN_RATION_MAX),
        ));
    }
    if keys.just_pressed(KeyCode::Semicolon) {
        let good = active_good.0;
        let cur = sim.0.world().faction(player_faction).industry_priority[good.index()];
        sim.0.push_human_action(Action::SetIndustryPriority { good, weight: (cur - PRIORITY_STEP).clamp(0.0, 1.0) });
    }
    if keys.just_pressed(KeyCode::Quote) {
        let good = active_good.0;
        let cur = sim.0.world().faction(player_faction).industry_priority[good.index()];
        sim.0.push_human_action(Action::SetIndustryPriority { good, weight: (cur + PRIORITY_STEP).clamp(0.0, 1.0) });
    }
    if keys.just_pressed(KeyCode::Comma) {
        let good = active_good.0;
        let cur = sim.0.world().faction(player_faction).logistics_priority[good.index()];
        sim.0.push_human_action(Action::SetLogisticsPriority { good, weight: (cur - PRIORITY_STEP).clamp(0.0, 1.0) });
    }
    if keys.just_pressed(KeyCode::Period) {
        let good = active_good.0;
        let cur = sim.0.world().faction(player_faction).logistics_priority[good.index()];
        sim.0.push_human_action(Action::SetLogisticsPriority { good, weight: (cur + PRIORITY_STEP).clamp(0.0, 1.0) });
    }
    if keys.just_pressed(KeyCode::Digit8) {
        let good = active_good.0;
        let cur = sim.0.world().faction(player_faction).import_plan[good.index()];
        sim.0.push_human_action(Action::SetImportPlan { good, rate: (cur - IMPORT_PLAN_STEP).max(0.0) });
    }
    if keys.just_pressed(KeyCode::Digit9) {
        let good = active_good.0;
        let cur = sim.0.world().faction(player_faction).import_plan[good.index()];
        sim.0.push_human_action(Action::SetImportPlan { good, rate: cur + IMPORT_PLAN_STEP });
    }
}

/// Menu item order, matching the number keys `handle_menu_keys` reads and
/// `ui::update_player_panel`'s legend text - kept in exactly one place so
/// the two can't drift apart.
pub(super) const MENU_ITEMS: [&str; 7] =
    ["陸軍を徴募", "艦隊を徴募（要港湾）", "インフラ建設", "港湾建設", "生産設備建設（対象品目）", "修復", "建設中止"];

fn handle_menu_keys(keys: &ButtonInput<KeyCode>, region: RegionId, active_good: archipelago_sim::good::Good, menu: &mut MenuRegion, sim: &mut SimRes) {
    if keys.just_pressed(KeyCode::Escape) {
        menu.0 = None;
        return;
    }
    let action = if keys.just_pressed(KeyCode::Digit1) {
        Some(Action::RecruitUnit { region, domain: Domain::Land })
    } else if keys.just_pressed(KeyCode::Digit2) {
        Some(Action::RecruitUnit { region, domain: Domain::Sea })
    } else if keys.just_pressed(KeyCode::Digit3) {
        Some(Action::Build { region, project: Project::Infrastructure })
    } else if keys.just_pressed(KeyCode::Digit4) {
        Some(Action::Build { region, project: Project::Port })
    } else if keys.just_pressed(KeyCode::Digit5) {
        // "生産設備建設（対象品目）" - `G` picks which good's capacity.
        Some(Action::Build { region, project: Project::Capacity(active_good) })
    } else if keys.just_pressed(KeyCode::Digit6) {
        Some(Action::Build { region, project: Project::Repair })
    } else if keys.just_pressed(KeyCode::Digit7) {
        Some(Action::CancelBuild { region })
    } else {
        None
    };

    if let Some(action) = action {
        sim.0.push_human_action(action);
        menu.0 = None;
    }
}

fn handle_diplomacy_keys(keys: &ButtonInput<KeyCode>, diplomacy: &mut DiplomacyPanel, player: &PlayerFaction, sim: &mut SimRes) {
    let Some(player_faction) = player.0 else {
        diplomacy.open = false;
        return;
    };
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::KeyD) {
        diplomacy.open = false;
        return;
    }
    if keys.just_pressed(KeyCode::KeyV) {
        let world = sim.0.world();
        let mut others: Vec<_> = world.factions.iter().filter(|f| f.id != player_faction && f.alive).map(|f| f.id).collect();
        others.sort_by_key(|f| f.0);
        if !others.is_empty() {
            let cur_pos = diplomacy.target.and_then(|t| others.iter().position(|&o| o == t)).unwrap_or(0);
            diplomacy.target = Some(others[(cur_pos + 1) % others.len()]);
        }
        return;
    }
    let Some(target) = diplomacy.target else { return };

    for (i, &treaty) in ALL_TREATIES.iter().enumerate() {
        let digit = match i {
            0 => KeyCode::Digit1,
            1 => KeyCode::Digit2,
            2 => KeyCode::Digit3,
            3 => KeyCode::Digit4,
            4 => KeyCode::Digit5,
            5 => KeyCode::Digit6,
            _ => continue,
        };
        if keys.just_pressed(digit) {
            sim.0.push_human_action(Action::ProposeTreaty { to: target, treaty });
            return;
        }
    }

    if keys.just_pressed(KeyCode::KeyA) {
        let world = sim.0.world();
        if let Some(p) = world.diplomacy.pending.iter().find(|p| p.from == target && p.to == player_faction) {
            sim.0.push_human_action(Action::AcceptTreaty { from: target, treaty: p.treaty });
        }
        return;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        let world = sim.0.world();
        if let Some(p) = world.diplomacy.pending.iter().find(|p| p.from == target && p.to == player_faction) {
            sim.0.push_human_action(Action::RejectTreaty { from: target, treaty: p.treaty });
        }
        return;
    }
    if keys.just_pressed(KeyCode::KeyW) {
        sim.0.push_human_action(Action::DeclareWar { to: target });
        return;
    }
    if keys.just_pressed(KeyCode::KeyB) {
        let treaty = current_breakable_treaty(sim.0.world(), player_faction, target);
        sim.0.push_human_action(Action::BreakTreaty { with: target, treaty });
    }
}

/// Which `Treaty` the `B` (break) diplomacy key targets: whichever of the
/// currently-active stance/grants is "most binding", so a single key can
/// stand in for "undo whatever I have with them" without the player having
/// to remember which of five treaty kinds is actually in force. Falls back
/// to `Alliance` (the highest-priority kind) when nothing is active at all -
/// `Simulation::apply` then rejects it (there's genuinely nothing to break),
/// and that rejection is exactly the honest feedback docs/phase7-spec.md
/// asks for, not a case worth hiding behind a client-side no-op.
fn current_breakable_treaty(world: &archipelago_sim::world::World, a: archipelago_sim::ids::FactionId, b: archipelago_sim::ids::FactionId) -> Treaty {
    match world.diplomacy.stance(a, b) {
        Stance::Alliance => return Treaty::Alliance,
        Stance::NonAggression => return Treaty::NonAggression,
        _ => {}
    }
    if world.diplomacy.has_military_access(a, b) {
        return Treaty::MilitaryAccess;
    }
    if world.diplomacy.has_port_access(a, b) {
        return Treaty::PortAccess;
    }
    if world.diplomacy.has_trade_agreement(a, b) {
        return Treaty::TradeAgreement;
    }
    Treaty::Alliance
}

/// Right-button drag pans the camera; the scroll wheel zooms it (clamped to
/// `MIN_ZOOM..=MAX_ZOOM`) - "ドラッグでパン、ホイールでズーム". Left button
/// is deliberately left untouched here - `map_click_select` owns it, so a
/// plain left-click always means "select/order", never "start panning".
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

/// Converts a left-click release into a world-space point, or `None` if it
/// wasn't a click (still dragging, or no window/camera) - shared by
/// `map_click_select`/`map_right_click_menu`.
fn click_world_pos(
    button: MouseButton,
    mouse_buttons: &ButtonInput<MouseButton>,
    windows: &Query<&Window, With<PrimaryWindow>>,
    camera: &Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    drag_start: &mut Local<Option<Vec2>>,
) -> Option<Vec2> {
    let window = windows.single().ok()?;
    let (camera, camera_transform) = camera.single().ok()?;

    if mouse_buttons.just_pressed(button) {
        **drag_start = window.cursor_position();
        return None;
    }
    if !mouse_buttons.just_released(button) {
        return None;
    }
    let (start, end) = (drag_start.take()?, window.cursor_position()?);
    if start.distance(end) > CLICK_DRAG_TOLERANCE {
        return None; // a drag, not a click.
    }
    camera.viewport_to_world_2d(camera_transform, end).ok()
}

/// Left-click: selects a unit (toggling membership in `SelectedUnits`,
/// docs/phase7-spec.md "部隊をクリック... 複数選択可"), or - if units are
/// already selected - issues a `MoveUnit` order for every one of them to
/// whichever region/sea-zone was clicked ("選択中に隣接地域をクリック" -
/// extended here to any clicked station, not just an adjacent one, since
/// adjacency is `Simulation::apply`'s call to make, not this system's - see
/// this module's own doc), or otherwise just selects the region for
/// inspection (Stage 7A behaviour, unchanged).
pub(super) fn map_click_select(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    layout: Res<RegionLayout>,
    sea_centers: Res<SeaZoneCenters>,
    units: Query<(&UnitMarker, &Transform)>,
    player: Res<PlayerFaction>,
    mut sim: ResMut<SimRes>,
    mut selected_region: ResMut<SelectedRegion>,
    mut selected_units: ResMut<SelectedUnits>,
    mut drag_start: Local<Option<Vec2>>,
) {
    let Some(world_pos) = click_world_pos(MouseButton::Left, &mouse_buttons, &windows, &camera, &mut drag_start) else { return };

    // 1. Unit hit test - only the player's own living units are selectable.
    if let Some(player_faction) = player.0 {
        let mut hit: Option<(u32, f32)> = None;
        for (marker, transform) in &units {
            let Some(unit) = sim.0.world().units.get(marker.0.index()) else { continue };
            if !unit.alive || unit.owner != player_faction {
                continue;
            }
            let pos = transform.translation.truncate();
            let d = pos.distance(world_pos);
            if d <= UNIT_CLICK_RADIUS && hit.is_none_or(|(_, best)| d < best) {
                hit = Some((marker.0.0, d));
            }
        }
        if let Some((unit_id, _)) = hit {
            if !selected_units.0.remove(&unit_id) {
                selected_units.0.insert(unit_id);
            }
            return;
        }
    }

    // 2. Region hit test.
    let mut region_hit: Option<(RegionId, f32)> = None;
    for region in &sim.0.world().regions {
        let [x, y] = layout.0[region.id.index()];
        let d = Vec2::new(x, y).distance(world_pos);
        let radius = region_radius(region.population);
        if d <= radius && region_hit.is_none_or(|(_, best)| d < best) {
            region_hit = Some((region.id, d));
        }
    }
    if let Some((region_id, _)) = region_hit {
        if !selected_units.0.is_empty() {
            issue_move_orders(&mut sim, &selected_units, Station::Region(region_id));
            selected_units.0.clear();
        }
        selected_region.0 = Some(region_id);
        return;
    }

    // 3. Sea-zone hit test - only meaningful as a fleet move target;
    // Stage 7A never supported clicking a sea zone for inspection either.
    if !selected_units.0.is_empty() {
        let world = sim.0.world();
        let mut zone_hit: Option<(archipelago_sim::ids::SeaZoneId, f32)> = None;
        for zone in &world.sea_zones {
            let [x, y] = sea_centers.0[zone.id.index()];
            let radius = sea_zone_radius(zone.coast.len());
            let d = Vec2::new(x, y).distance(world_pos);
            if d <= radius && zone_hit.is_none_or(|(_, best)| d < best) {
                zone_hit = Some((zone.id, d));
            }
        }
        if let Some((zone_id, _)) = zone_hit {
            issue_move_orders(&mut sim, &selected_units, Station::Sea(zone_id));
            selected_units.0.clear();
            return;
        }
    }

    // Empty space: deselect the inspected region (Stage 7A behaviour).
    // Unit selection is left alone - only Esc or a completed order clears
    // it, so a stray miss-click can't silently discard a multi-select.
    selected_region.0 = None;
}

fn issue_move_orders(sim: &mut SimRes, selected_units: &SelectedUnits, to: Station) {
    for &raw_id in &selected_units.0 {
        sim.0.push_human_action(Action::MoveUnit { unit: archipelago_sim::ids::UnitId(raw_id), to });
    }
}

/// Right-click an own region to open the recruit/build menu
/// (`MenuRegion`) - "自国地域を右クリック: その地域で可能な命令のメニュー"。
/// A region not owned by the player still just gets ignored here (opening a
/// menu of orders for someone else's territory isn't a "rejection" worth
/// surfacing - there is no order to attempt yet, since no menu choice has
/// been made). Uses the same click-vs-drag distinction as
/// `map_click_select`/`mouse_pan_zoom`'s own right-button pan, so panning
/// (a drag) and opening the menu (a clean click) never fight each other.
pub(super) fn map_right_click_menu(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    layout: Res<RegionLayout>,
    player: Res<PlayerFaction>,
    sim: Res<SimRes>,
    mut menu: ResMut<MenuRegion>,
    mut drag_start: Local<Option<Vec2>>,
) {
    let Some(player_faction) = player.0 else { return };
    let Some(world_pos) = click_world_pos(MouseButton::Right, &mouse_buttons, &windows, &camera, &mut drag_start) else { return };

    let mut hit: Option<(RegionId, f32)> = None;
    for region in &sim.0.world().regions {
        let [x, y] = layout.0[region.id.index()];
        let d = Vec2::new(x, y).distance(world_pos);
        let radius = region_radius(region.population);
        if d <= radius && hit.is_none_or(|(_, best)| d < best) {
            hit = Some((region.id, d));
        }
    }
    if let Some((region_id, _)) = hit
        && sim.0.world().region(region_id).owner == player_faction
    {
        menu.0 = Some(region_id);
    }
}
