//! Player input: pan/zoom the camera, pause/resume and speed selection,
//! click-to-select/order on the map, `Esc` to deselect
//! (docs/phase7-spec.md "操作"), and Stage 7B's ("Stage 7B — 遊ぶ") keyboard
//! policy changes and diplomacy panel.
//!
//! Every system here only ever writes `SpeedRes`/`SelectedRegion`/
//! `SelectedUnits`/`MenuRegion`/`DiplomacyPanel`/`ActiveGood`/the camera's
//! own `Transform`/zoom directly - and reaches `SimRes` only through
//! `SimDriver::push_human_action`, exactly the same `Action`-queueing door
//! `HumanAgent::push` (docs/design.md §14: "人間も AI と同じ入口から世界に
//! 触る") opens for anything else, or through `SimDriver::delegate_unit`/
//! `undelegate_unit`/`is_delegated` - military delegation is player intent
//! living in `HumanAgent` itself, not a `Simulation` mutation, so it never
//! goes through `Action`/`push_human_action` at all (see those methods'
//! own docs). No system here ever calls `SimDriver::tick` or mutates
//! `Simulation`/`World` directly - that stays `sim_control::
//! advance_simulation`'s job alone.
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

use bevy::input::gestures::PinchGesture;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use archipelago_sim::action::Action;
use archipelago_sim::balance::NL_PROPOSAL_TEXT_MAX_CHARS;
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Stance, Treaty, ALL_TREATIES};
use archipelago_sim::focus::ALL_FOCI;
use archipelago_sim::good::{ALL_GOODS, GOOD_COUNT};
use archipelago_sim::ids::RegionId;
use archipelago_sim::world::{Domain, Station};

use super::setup::sea_zone_radius;
use super::{
    ActiveGood, DiplomacyPanel, MainCamera, MenuRegion, NewspaperState, NlCompose, PlayerFaction,
    RegionLayout, RegionRadii, SeaZoneCenters, SelectedFaction, SelectedRegion, SelectedSeaZone,
    SelectedUnits, SimRes, Speed, SpeedRes, SupplyOverlay, UnitMarker,
};

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
const ZOOM_STEP: f32 = 0.1;
/// World units of camera pan per second of an arrow key held, at zoom scale
/// 1.0 (scaled by the current zoom the same way every other pan input here
/// is - `keyboard_pan`'s own doc).
const KEY_PAN_SPEED: f32 = 900.0;
/// World units of camera pan per "line" of scroll (`scroll_delta_in_lines`) -
/// `mouse_pan_zoom`'s own doc. Roughly matches the feel of the old
/// right-drag pan: one physical wheel notch (one line) moves the map about
/// as far as a short, deliberate drag did.
const SCROLL_PAN_SPEED: f32 = 60.0;
/// How much one full unit of `PinchGesture` delta (`mouse_pan_zoom`'s own
/// doc) changes the camera's relative zoom - macOS/iOS only, applied
/// directly to `zoom_by` with no extra sensitivity multiplier, since
/// Winit/Bevy already reports it as a small per-frame magnification delta
/// of the same rough order as `ZOOM_STEP`-scaled scroll. Needs on-device
/// confirmation (this crate cannot generate a real pinch gesture on this
/// Linux/X11 machine - see this module's tests for what *can* be checked
/// here instead).
const PINCH_ZOOM_SENSITIVITY: f32 = 1.0;
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

#[allow(clippy::too_many_arguments)]
pub(super) fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut speed: ResMut<SpeedRes>,
    mut selected_region: ResMut<SelectedRegion>,
    mut selected_sea_zone: ResMut<SelectedSeaZone>,
    mut selected_units: ResMut<SelectedUnits>,
    mut selected_faction: ResMut<SelectedFaction>,
    mut menu: ResMut<MenuRegion>,
    mut diplomacy: ResMut<DiplomacyPanel>,
    mut policy: ResMut<super::PolicyPanel>,
    mut active_good: ResMut<ActiveGood>,
    player: Res<PlayerFaction>,
    mut sim: ResMut<SimRes>,
    mut nl_compose: ResMut<NlCompose>,
    mut supply_overlay: ResMut<SupplyOverlay>,
    mut newspaper: ResMut<NewspaperState>,
) {
    // While composing a natural-language proposal, every key here is
    // suppressed - `input::nl_compose_text_input` owns the keyboard
    // entirely until `Enter`/`Esc` ends compose mode (docs/phase7-spec.md
    // "4. 外交画面": "テキスト入力欄から送り"). Otherwise typing "w" to
    // compose a sentence would also fire `handle_diplomacy_keys`'s
    // declare-war binding.
    if nl_compose.active {
        return;
    }

    // `L` (supply overlay) and `N` (newspaper) work everywhere, observing-only
    // included, and are never captured by the menu/diplomacy panels below -
    // docs/phase7-spec.md "補給網の表示は L キーでトグルする".
    if keys.just_pressed(KeyCode::KeyL) {
        supply_overlay.0 = !supply_overlay.0;
    }
    if keys.just_pressed(KeyCode::KeyN) {
        newspaper.open = !newspaper.open;
    }
    if newspaper.open {
        if keys.just_pressed(KeyCode::ArrowLeft) {
            let earliest = 0;
            let current = newspaper.viewing.unwrap_or(newspaper.history.len().saturating_sub(1));
            newspaper.viewing = Some(current.saturating_sub(1).max(earliest));
        }
        if keys.just_pressed(KeyCode::ArrowRight) {
            let last = newspaper.history.len().saturating_sub(1);
            let current = newspaper.viewing.unwrap_or(last);
            // External code review fix A3: paging onto the newest issue
            // (`current + 1 == last`) must land on `None` ("the latest
            // issue" - `NewspaperState::viewing`'s own doc), not
            // `Some(last)` - otherwise a fresh issue published afterward
            // never appears until the player pages away and back, since
            // `Some(last)` pins the reader to whatever `last` was at the
            // moment they paged there instead of tracking "whatever is
            // newest right now".
            newspaper.viewing = if current + 1 >= last { None } else { Some(current + 1) };
        }
    }

    // The recruit/build menu, when open, takes over the number row -
    // "自国地域を右クリック: その地域で可能な命令のメニュー" - and closes on
    // any of its own keys or `Esc`.
    if let Some(region) = menu.0 {
        handle_menu_keys(&keys, region, active_good.0, &mut menu, &mut sim);
        return;
    }

    // The diplomacy panel similarly takes over its own keys while open -
    // "条約の提案・受諾・拒否は外交パネルから". `P` still switches straight to
    // the policy panel from here (mirrors `panels::handle_policy_toggle`'s
    // mouse path, which isn't gated on `diplomacy.open` at all).
    if diplomacy.open {
        if keys.just_pressed(KeyCode::KeyP) {
            diplomacy.open = false;
            policy.0 = true;
            return;
        }
        handle_diplomacy_keys(&keys, &mut diplomacy, &player, &mut sim, &mut nl_compose);
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
        selected_sea_zone.0 = None;
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
        policy.0 = false;
        if diplomacy.target.is_none() {
            diplomacy.target = sim.0.world().factions.iter().find(|f| f.id != player_faction && f.alive).map(|f| f.id);
        }
        return;
    }
    // Stage 8B's policy panel (`panels::PolicyPanelRoot`) - toggled the same
    // way `D` toggles diplomacy.
    if keys.just_pressed(KeyCode::KeyP) {
        policy.0 = !policy.0;
        if policy.0 {
            diplomacy.open = false;
        }
        return;
    }
    // Stage 8B: hold/reinforce every currently selected unit - previously
    // reachable through no input path at all in this client (only
    // `MoveUnit`, via a map click, existed before). `panels::UnitPanelRoot`'s
    // per-unit buttons do the same, one unit at a time; these two keys act
    // on the whole current selection at once.
    if keys.just_pressed(KeyCode::KeyH) {
        for &unit in &selected_units.0 {
            sim.0.push_human_action(Action::HoldUnit { unit: archipelago_sim::ids::UnitId(unit) });
        }
    }
    if keys.just_pressed(KeyCode::KeyJ) {
        for &unit in &selected_units.0 {
            sim.0.push_human_action(Action::ReinforceUnit { unit: archipelago_sim::ids::UnitId(unit) });
        }
    }
    // Disband-defect fix: stand down every currently selected unit -
    // `panels::UnitPanelRoot`'s per-unit "解散 [K]" button does the same,
    // one unit at a time.
    if keys.just_pressed(KeyCode::KeyK) {
        for &unit in &selected_units.0 {
            sim.0.push_human_action(Action::DisbandUnit { unit: archipelago_sim::ids::UnitId(unit) });
        }
    }
    // Military delegation (docs/design.md §14): `U` toggles every currently
    // selected unit's delegated state independently (a mixed selection
    // hands over whichever aren't already delegated and takes back
    // whichever are, rather than forcing the whole selection to one state) -
    // `panels::UnitPanelRoot`'s per-unit "AI委任 [U]"/"操作を戻す [U]" button
    // does the same, one unit at a time. This never goes through
    // `push_human_action`/`Action` at all - delegation is player intent
    // living in `archipelago_agents::HumanAgent`, not a `Simulation`
    // mutation (`SimDriver::delegate_unit`/`undelegate_unit`'s own doc).
    if keys.just_pressed(KeyCode::KeyU) {
        for &unit in &selected_units.0 {
            let unit = archipelago_sim::ids::UnitId(unit);
            if sim.0.is_delegated(unit) {
                sim.0.undelegate_unit(unit);
            } else {
                sim.0.delegate_unit(unit);
            }
        }
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

fn handle_diplomacy_keys(
    keys: &ButtonInput<KeyCode>,
    diplomacy: &mut DiplomacyPanel,
    player: &PlayerFaction,
    sim: &mut SimRes,
    nl_compose: &mut NlCompose,
) {
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

    // `T`: open the natural-language proposal compose box, targeting
    // whoever `V` currently has selected (docs/phase7-spec.md "4. 外交画面":
    // "テキスト入力欄から送り"). `input::nl_compose_text_input` (a separate
    // system) owns every keystroke from here until `Enter`/`Esc`.
    if keys.just_pressed(KeyCode::KeyT) {
        nl_compose.active = true;
        nl_compose.buffer.clear();
        // External code review fix A1: this same `T` press is still sitting
        // unread in this frame's `KeyboardInput` event queue -
        // `nl_compose_text_input` runs later in the same `Update` chain and
        // would otherwise read it as the first character typed. See
        // `NlCompose::just_activated`'s own doc.
        nl_compose.just_activated = true;
        return;
    }

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

/// Pans and zooms the camera without ever touching plain left-click
/// (`map_click_select` owns it - a plain left-click always means
/// "select/order", never "start panning") or the right button
/// (`map_right_click_menu` owns it in `--play` mode for the region menu -
/// this module's own doc, "Right-click is also already taken by the region
/// menu").
///
/// This system used to bind panning to a right-button drag - "ドラッグでパン、
/// ホイールでズーム" - but that binding is unreachable on a macOS trackpad
/// with no external mouse: a two-finger tap registers as a right click, but
/// there is no way to *hold it down and drag*. Scroll has no such problem -
/// Bevy delivers a trackpad's two-finger scroll as ordinary `MouseWheel`
/// messages (`MouseScrollUnit::Pixel`) exactly like a physical wheel's
/// notches (`MouseScrollUnit::Line`), so it reaches every device this
/// module cares about through one event type. The bindings, all optional
/// and overlapping - a trackpad-only macOS machine can reach every one of
/// these except the middle-drag:
///
/// - **Scroll, no modifier: pans.** The primary scheme, and the whole
///   reason the right-drag was replaced - a two-finger trackpad scroll
///   needs no button held at all.
/// - **Ctrl+scroll: zooms.** Mirrors the near-universal "hold Ctrl, scroll
///   to zoom" convention (browsers, IDEs, image viewers) - lets a plain
///   wheel mouse zoom without a second control, and works identically from
///   a trackpad (hold Ctrl, two-finger-scroll).
/// - **Trackpad pinch (`PinchGesture`, macOS/iOS only): zooms.** The native
///   gesture, delivered on its own channel independent of `MouseWheel`, so
///   pinch-to-zoom and two-finger-scroll-to-pan are simultaneously
///   available exactly the way every other macOS app with a canvas behaves
///   - no modifier needed for either.
/// - **Middle-button drag: pans.** An extra convenience for a three-button
///   desktop mouse, reusing the same drag math the old right-drag used;
///   changes nothing for a trackpad-only player, who has no middle button
///   to press.
///
/// `keyboard_pan` (below) is the one panning input that is never optional -
/// see its own doc for why it, alone, isn't listed here as "just another
/// option".
pub(super) fn mouse_pan_zoom(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut pinch: MessageReader<PinchGesture>,
    mut camera: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
) {
    let Ok((mut transform, mut projection)) = camera.single_mut() else { return };
    let current_scale = orthographic_scale(&projection);

    if mouse_buttons.pressed(MouseButton::Middle) {
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

    let ctrl_held = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let mut scroll = Vec2::ZERO;
    for ev in wheel.read() {
        scroll += scroll_delta_in_lines(ev);
    }
    if ctrl_held {
        if scroll.y != 0.0 {
            zoom_by(&mut projection, scroll.y * ZOOM_STEP);
        }
    } else if scroll != Vec2::ZERO {
        transform.translation.x += scroll.x * SCROLL_PAN_SPEED * current_scale;
        transform.translation.y -= scroll.y * SCROLL_PAN_SPEED * current_scale;
    }

    for ev in pinch.read() {
        zoom_by(&mut projection, ev.0 * PINCH_ZOOM_SENSITIVITY);
    }
}

/// Normalizes a `MouseWheel` event's delta to "lines", the unit a physical
/// wheel notch already reports one of - a trackpad's `MouseScrollUnit::
/// Pixel` delta is divided by Bevy's own published pixels-per-line
/// (`MouseScrollUnit::SCROLL_UNIT_CONVERSION_FACTOR`) so `mouse_pan_zoom`
/// can apply exactly one pan/zoom formula to either source instead of
/// calibrating each separately.
fn scroll_delta_in_lines(ev: &MouseWheel) -> Vec2 {
    match ev.unit {
        MouseScrollUnit::Line => Vec2::new(ev.x, ev.y),
        MouseScrollUnit::Pixel => Vec2::new(ev.x, ev.y) / MouseScrollUnit::SCROLL_UNIT_CONVERSION_FACTOR,
    }
}

/// Applies a relative zoom change to an orthographic projection, clamped to
/// `MIN_ZOOM..=MAX_ZOOM` - shared by `mouse_pan_zoom`'s Ctrl+scroll and
/// pinch-gesture branches so the two stay numerically consistent (a
/// positive `relative` always zooms in, matching both "scroll up to zoom
/// in" and `PinchGesture`'s own "positive delta = magnify" convention). A
/// no-op on a non-orthographic projection (this client never uses one, but
/// `Projection` is the type `MainCamera`'s query returns).
fn zoom_by(projection: &mut Projection, relative: f32) {
    if let Projection::Orthographic(ortho) = projection {
        ortho.scale = (ortho.scale * (1.0 - relative)).clamp(MIN_ZOOM, MAX_ZOOM);
    }
}

fn orthographic_scale(projection: &Projection) -> f32 {
    match projection {
        Projection::Orthographic(ortho) => ortho.scale,
        _ => 1.0,
    }
}

/// Arrow-key camera pan - the one panning input that is never optional
/// ("the map is always movable"). Every binding in `mouse_pan_zoom` targets
/// a specific pointing device or gesture (a trackpad's two-finger scroll, a
/// wheel mouse, a three-button mouse's middle button, a macOS pinch); this
/// one only needs a keyboard, which every platform this client runs on has,
/// so it is the single guarantee the map can always be moved.
///
/// Deliberately arrow keys, not WASD: `W`/`A`/`D` are already bound while
/// the diplomacy panel is open (`W` declares war, `A` accepts a treaty, `D`
/// closes the panel - `handle_diplomacy_keys`), and reusing them here on
/// `pressed()` would fire both effects from one keypress - holding `W` to
/// pan the camera during a negotiation would also declare war. Arrow keys
/// collide with nothing there. Their only other user is `keyboard_input`'s
/// newspaper pager (`ArrowLeft`/`ArrowRight`, gated on `newspaper.open`),
/// which is a harmless double-fire - turning a page while also panning the
/// camera underneath it is not a mistake worth guarding against, unlike
/// accidentally declaring war.
///
/// Not gated behind `nl_compose.active`/`menu.0`/`diplomacy.open` at all,
/// on purpose (unlike `keyboard_input`, which returns early for all three) -
/// this is the fallback of last resort, so it must not share their
/// exclusivity.
pub(super) fn keyboard_pan(keys: Res<ButtonInput<KeyCode>>, time: Res<Time>, mut camera: Query<(&mut Transform, &Projection), With<MainCamera>>) {
    let Ok((mut transform, projection)) = camera.single_mut() else { return };

    let mut dir = Vec2::ZERO;
    if keys.pressed(KeyCode::ArrowUp) {
        dir.y += 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        dir.y -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowLeft) {
        dir.x -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        dir.x += 1.0;
    }
    if dir == Vec2::ZERO {
        return;
    }

    let scale = orthographic_scale(projection);
    transform.translation += (dir.normalize() * KEY_PAN_SPEED * scale * time.delta_secs()).extend(0.0);
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
/// this module's own doc), or otherwise just selects the region (Stage 7A
/// behaviour, unchanged) or sea zone for inspection. Sea-zone selection is
/// new (Stage 7C follow-up) and drives exactly one thing right now:
/// `overlay::sync_blockade_visuals` reveals a blockaded port's line to its
/// causing sea zone only while that region or that sea zone is selected -
/// there is no sea-zone inspect panel. Region and sea-zone selection are
/// mutually exclusive; selecting one clears the other.
#[allow(clippy::too_many_arguments)]
pub(super) fn map_click_select(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    layout: Res<RegionLayout>,
    radii: Res<RegionRadii>,
    sea_centers: Res<SeaZoneCenters>,
    units: Query<(&UnitMarker, &Transform)>,
    player: Res<PlayerFaction>,
    mut sim: ResMut<SimRes>,
    mut selected_region: ResMut<SelectedRegion>,
    mut selected_sea_zone: ResMut<SelectedSeaZone>,
    mut selected_units: ResMut<SelectedUnits>,
    pointer_over_ui: Res<super::panels::PointerOverUi>,
    mut drag_start: Local<Option<Vec2>>,
) {
    let Some(world_pos) = click_world_pos(MouseButton::Left, &mouse_buttons, &windows, &camera, &mut drag_start) else { return };
    // `panels`'s own module doc, "Click-vs-map-click": a click released over
    // any visible panel button must not also register as a map click.
    if pointer_over_ui.0 {
        return;
    }

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
        let radius = radii.0[region.id.index()];
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
        selected_sea_zone.0 = None;
        return;
    }

    // 3. Sea-zone hit test - a fleet move target when units are selected
    // (unchanged), otherwise selects the zone for inspection (new - see
    // this function's own doc).
    let mut zone_hit: Option<(archipelago_sim::ids::SeaZoneId, f32)> = None;
    for zone in &sim.0.world().sea_zones {
        let [x, y] = sea_centers.0[zone.id.index()];
        let radius = sea_zone_radius(zone.coast.len());
        let d = Vec2::new(x, y).distance(world_pos);
        if d <= radius && zone_hit.is_none_or(|(_, best)| d < best) {
            zone_hit = Some((zone.id, d));
        }
    }
    if let Some((zone_id, _)) = zone_hit {
        if !selected_units.0.is_empty() {
            issue_move_orders(&mut sim, &selected_units, Station::Sea(zone_id));
            selected_units.0.clear();
        } else {
            selected_sea_zone.0 = Some(zone_id);
            selected_region.0 = None;
        }
        return;
    }

    // Empty space: deselect the inspected region/sea-zone (Stage 7A
    // behaviour, extended to the new sea-zone selection). Unit selection is
    // left alone - only Esc or a completed order clears it, so a stray
    // miss-click can't silently discard a multi-select.
    selected_region.0 = None;
    selected_sea_zone.0 = None;
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
/// `map_click_select` (`click_world_pos`'s `CLICK_DRAG_TOLERANCE`), so a
/// stray drag on the right button (e.g. a slightly-sliding two-finger tap)
/// just opens nothing rather than misfiring the menu - right-button drag
/// itself no longer pans (`mouse_pan_zoom`'s own doc: that binding was
/// unreachable on a macOS trackpad and this button was already overloaded
/// with the region menu), so there is nothing left to fight over here.
pub(super) fn map_right_click_menu(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    layout: Res<RegionLayout>,
    radii: Res<RegionRadii>,
    player: Res<PlayerFaction>,
    sim: Res<SimRes>,
    mut menu: ResMut<MenuRegion>,
    pointer_over_ui: Res<super::panels::PointerOverUi>,
    mut drag_start: Local<Option<Vec2>>,
) {
    let Some(player_faction) = player.0 else { return };
    let Some(world_pos) = click_world_pos(MouseButton::Right, &mouse_buttons, &windows, &camera, &mut drag_start) else { return };
    if pointer_over_ui.0 {
        return;
    }

    let mut hit: Option<(RegionId, f32)> = None;
    for region in &sim.0.world().regions {
        let [x, y] = layout.0[region.id.index()];
        let d = Vec2::new(x, y).distance(world_pos);
        let radius = radii.0[region.id.index()];
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

/// Owns every keystroke while `NlCompose::active` (docs/phase7-spec.md "4.
/// 外交画面": "テキスト入力欄から送り") - `keyboard_input` returns
/// immediately without touching anything while this is true, so no other
/// binding (menu digits, diplomacy treaty keys, `L`/`N`, ...) can fire
/// mid-sentence.
///
/// `Enter` submits (`Action::ProposeInNaturalLanguage` to whichever faction
/// `DiplomacyPanel::target` currently names) and exits compose mode;
/// `Escape` cancels without sending anything; `Backspace` deletes the last
/// character; anything else that produces text (`KeyboardInput::logical_key`
/// being `Key::Character`) is appended, capped at
/// `NL_PROPOSAL_TEXT_MAX_CHARS` - the same public, known limit
/// `action::apply_propose_nl` itself enforces, so clamping it here isn't
/// hiding a rejection the player couldn't already know about
/// (`input`'s own module doc, "命令の可否を隠さない").
pub(super) fn nl_compose_text_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut key_events: MessageReader<KeyboardInput>,
    mut nl_compose: ResMut<NlCompose>,
    diplomacy: Res<DiplomacyPanel>,
    player: Res<PlayerFaction>,
    mut sim: ResMut<SimRes>,
) {
    if !nl_compose.active {
        key_events.clear();
        return;
    }

    // External code review fix A1: this frame's own `T` keypress (the one
    // `handle_diplomacy_keys` just read via `ButtonInput<KeyCode>` to set
    // `active = true`) is still sitting in `key_events` - drain it unread,
    // without treating it as the compose buffer's first character, and
    // start actually collecting text from the *next* frame on.
    // `NlCompose::just_activated`'s own doc has the full ordering reasoning.
    if nl_compose.just_activated {
        nl_compose.just_activated = false;
        key_events.clear();
        return;
    }

    if keys.just_pressed(KeyCode::Escape) {
        nl_compose.active = false;
        nl_compose.buffer.clear();
        key_events.clear();
        return;
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        // The sending faction is implicit in `push_human_action` (it only
        // ever queues for the human/replay controller) - just need to know
        // a player exists at all and that a target is selected.
        if player.0.is_some()
            && let Some(target) = diplomacy.target
        {
            sim.0.push_human_action(Action::ProposeInNaturalLanguage { to: target, text: nl_compose.buffer.clone() });
        }
        nl_compose.active = false;
        nl_compose.buffer.clear();
        key_events.clear();
        return;
    }
    if keys.just_pressed(KeyCode::Backspace) {
        nl_compose.buffer.pop();
    }

    for ev in key_events.read() {
        if ev.state != ButtonState::Pressed {
            continue;
        }
        if let Key::Character(s) = &ev.logical_key {
            for ch in s.chars() {
                if nl_compose.buffer.chars().count() < NL_PROPOSAL_TEXT_MAX_CHARS {
                    nl_compose.buffer.push(ch);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::input::touch::TouchPhase;

    use super::*;

    use archipelago_sim::ids::FactionId;

    use crate::sim_driver::SimDriver;

    /// External code review fix A1 (P1): pressing `T` to enter natural-
    /// language compose mode used to run `nl_compose_text_input` later in
    /// the *same* frame it activated, reading the very `KeyboardInput`
    /// event that fired the `T` binding - every composed proposal began
    /// with a stray "t" (see `NlCompose::just_activated`'s own doc).
    ///
    /// This drives `nl_compose_text_input` directly against a `World`,
    /// reusing one `System` instance across two calls so its own `Local`
    /// event-cursor state persists between them exactly the way the real
    /// `Update` schedule persists it frame to frame (a fresh `System` per
    /// call, e.g. via `run_system_once`, would reset that cursor and read
    /// every still-buffered message from scratch each time - not what
    /// happens in the real game loop this bug lived in).
    ///
    /// Checked this actually exercises the bug: temporarily reverted the
    /// `just_activated`-drain branch in `nl_compose_text_input` and reran -
    /// the first assertion below then fails with `buffer == "t"` instead of
    /// `""`, and the second fails with `"th"` instead of `"h"`, matching
    /// exactly the review's "every proposal begins with a stray t" report.
    #[test]
    fn nl_compose_activation_key_is_not_collected_as_text() {
        let mut world = World::new();
        world.init_resource::<Messages<KeyboardInput>>();
        world.insert_resource(ButtonInput::<KeyCode>::default());
        world.insert_resource(NlCompose { active: true, buffer: String::new(), just_activated: true });
        world.insert_resource(DiplomacyPanel { open: true, target: Some(FactionId(1)) });
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SimRes(SimDriver::new(archipelago_sim::scenario::build_world(), 1)));

        let mut system = IntoSystem::into_system(nl_compose_text_input);
        system.initialize(&mut world);

        // Frame 1: exactly the state `handle_diplomacy_keys`'s `T` binding
        // leaves behind - `just_activated` set, and the `T` keypress itself
        // still unread in this frame's own `KeyboardInput` queue.
        world.resource_mut::<Messages<KeyboardInput>>().write(KeyboardInput {
            key_code: KeyCode::KeyT,
            logical_key: Key::Character("t".into()),
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        system.run((), &mut world).unwrap();

        assert_eq!(world.resource::<NlCompose>().buffer, "", "the activation keypress must not be collected as text");
        assert!(!world.resource::<NlCompose>().just_activated, "the activation flag must be consumed after one run");

        // Frame 2: an ordinary keystroke, now that compose mode is actually
        // collecting text - proves the fix doesn't just eat every keypress.
        world.resource_mut::<Messages<KeyboardInput>>().write(KeyboardInput {
            key_code: KeyCode::KeyH,
            logical_key: Key::Character("h".into()),
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        system.run((), &mut world).unwrap();

        assert_eq!(world.resource::<NlCompose>().buffer, "h", "a real keystroke after activation must still be collected");
    }

    /// Spawns the one entity `mouse_pan_zoom`/`keyboard_pan` both query for -
    /// shared by every test below instead of a `Camera2d` bundle, since
    /// these systems only ever touch `MainCamera`'s own `Transform`/
    /// `Projection` and a bare `World::new()` (no render plugins) never
    /// resolves `Camera2d`'s required-component chain the way `App` would.
    fn spawn_main_camera(world: &mut World) {
        world.spawn((MainCamera, Transform::default(), Projection::Orthographic(OrthographicProjection::default_2d())));
    }

    fn camera_state(world: &mut World) -> (Vec3, f32) {
        let mut q = world.query_filtered::<(&Transform, &Projection), With<MainCamera>>();
        let (transform, projection) = q.single(world).unwrap();
        (transform.translation, orthographic_scale(projection))
    }

    /// Keyboard panning is the one binding the task requires to be
    /// unreachable-proof (`keyboard_pan`'s own doc, "the map is always
    /// movable") - this is the one path this Linux/X11 machine can actually
    /// exercise end to end, since every other binding below depends on a
    /// real trackpad/mouse the CI/dev box doesn't have either. Checked this
    /// actually exercises the movement, not a no-op: temporarily changed
    /// `KEY_PAN_SPEED` to `0.0` and reran - this assertion then fails
    /// (`translation.y == 0.0` instead of `900.0`), and restored afterward.
    #[test]
    fn keyboard_pan_moves_camera_with_arrow_up() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowUp);
        world.insert_resource(keys);
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_secs_f32(1.0));
        world.insert_resource(time);

        let mut system = IntoSystem::into_system(keyboard_pan);
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();

        let (translation, scale) = camera_state(&mut world);
        assert_eq!(translation, Vec3::new(0.0, 900.0, 0.0), "one second of ArrowUp must move the camera up by exactly 900 world units - a literal, not `KEY_PAN_SPEED`, so this also catches an accidental retune, not just a broken axis/sign");
        assert_eq!(scale, 1.0, "arrow-key panning must never touch zoom");
    }

    /// Regression guard for the two failure modes a "held-key" system can
    /// have without ever failing to compile: doing nothing when it should
    /// move (covered above), and moving when it shouldn't. No key held here
    /// - the camera must not drift on its own every frame.
    #[test]
    fn keyboard_pan_is_a_noop_with_no_keys_held() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        world.insert_resource(ButtonInput::<KeyCode>::default());
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_secs_f32(1.0));
        world.insert_resource(time);

        let mut system = IntoSystem::into_system(keyboard_pan);
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();

        let (translation, _) = camera_state(&mut world);
        assert_eq!(translation, Vec3::ZERO, "no key held must never move the camera");
    }

    /// Guards the direction accumulation itself, not just "some key does
    /// something": opposite arrow keys held together must cancel exactly,
    /// not add up or fall through to only one of the two branches.
    #[test]
    fn keyboard_pan_cancels_opposite_arrow_keys() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowLeft);
        keys.press(KeyCode::ArrowRight);
        keys.press(KeyCode::ArrowUp);
        keys.press(KeyCode::ArrowDown);
        world.insert_resource(keys);
        let mut time = Time::<()>::default();
        time.advance_by(Duration::from_secs_f32(1.0));
        world.insert_resource(time);

        let mut system = IntoSystem::into_system(keyboard_pan);
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();

        let (translation, _) = camera_state(&mut world);
        assert_eq!(translation, Vec3::ZERO, "opposite held arrow keys must cancel out, not stack");
    }

    /// The whole point of moving panning off scroll's plain form: a
    /// trackpad's two-finger scroll (or a physical wheel notch) pans with
    /// no modifier and no button held at all - the one thing a macOS
    /// trackpad user with no external mouse could never do with the old
    /// right-drag binding. Checked this fails when broken: temporarily made
    /// the pan branch a no-op (`if false && !ctrl_held`) and reran - this
    /// assertion then fails (`translation == Vec3::ZERO`), restored after.
    #[test]
    fn mouse_wheel_scroll_pans_without_ctrl() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        world.init_resource::<Messages<MouseWheel>>();
        world.init_resource::<Messages<MouseMotion>>();
        world.init_resource::<Messages<PinchGesture>>();
        world.insert_resource(ButtonInput::<MouseButton>::default());
        world.insert_resource(ButtonInput::<KeyCode>::default());

        let mut system = IntoSystem::into_system(mouse_pan_zoom);
        system.initialize(&mut world);

        world.resource_mut::<Messages<MouseWheel>>().write(MouseWheel { unit: MouseScrollUnit::Line, x: 0.0, y: 1.0, window: Entity::PLACEHOLDER, phase: TouchPhase::Moved });
        system.run((), &mut world).unwrap();

        let (translation, scale) = camera_state(&mut world);
        assert_eq!(translation, Vec3::new(0.0, -60.0, 0.0), "one line of unmodified scroll must pan by exactly -60 world units (literal, not `SCROLL_PAN_SPEED`, for the same reason as the keyboard-pan test above)");
        assert_eq!(scale, 1.0, "unmodified scroll must never touch zoom");
    }

    /// The other half of the same split: holding Ctrl turns that identical
    /// scroll event into a zoom instead - the near-universal "Ctrl+scroll
    /// zooms" convention, and the only zoom control a plain wheel mouse
    /// (no pinch gesture available at all) has under this scheme.
    #[test]
    fn ctrl_scroll_zooms_instead_of_panning() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        world.init_resource::<Messages<MouseWheel>>();
        world.init_resource::<Messages<MouseMotion>>();
        world.init_resource::<Messages<PinchGesture>>();
        world.insert_resource(ButtonInput::<MouseButton>::default());
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);
        world.insert_resource(keys);

        let mut system = IntoSystem::into_system(mouse_pan_zoom);
        system.initialize(&mut world);

        world.resource_mut::<Messages<MouseWheel>>().write(MouseWheel { unit: MouseScrollUnit::Line, x: 0.0, y: 1.0, window: Entity::PLACEHOLDER, phase: TouchPhase::Moved });
        system.run((), &mut world).unwrap();

        let (translation, scale) = camera_state(&mut world);
        assert_eq!(translation, Vec3::ZERO, "Ctrl+scroll must never pan");
        assert_eq!(scale, 0.9, "Ctrl+scroll must zoom the projection scale to exactly 0.9 (literal, not `ZOOM_STEP`)");
    }

    /// macOS/iOS's native two-finger pinch (`PinchGesture`) zooms
    /// independently of the scroll channel above, so pinch-to-zoom and
    /// two-finger-scroll-to-pan are both available on the same trackpad at
    /// once - see `mouse_pan_zoom`'s own doc for why that split matters.
    #[test]
    fn pinch_gesture_zooms() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        world.init_resource::<Messages<MouseWheel>>();
        world.init_resource::<Messages<MouseMotion>>();
        world.init_resource::<Messages<PinchGesture>>();
        world.insert_resource(ButtonInput::<MouseButton>::default());
        world.insert_resource(ButtonInput::<KeyCode>::default());

        let mut system = IntoSystem::into_system(mouse_pan_zoom);
        system.initialize(&mut world);

        world.resource_mut::<Messages<PinchGesture>>().write(PinchGesture(0.2));
        system.run((), &mut world).unwrap();

        let (translation, scale) = camera_state(&mut world);
        assert_eq!(translation, Vec3::ZERO, "a pinch gesture must never pan");
        assert_eq!(scale, 0.8, "a positive pinch delta (magnify) must zoom the projection scale to exactly 0.8 (literal, not `PINCH_ZOOM_SENSITIVITY`)");
    }

    /// The extra desktop-mouse convenience: a middle-button drag pans just
    /// like the old right-drag used to, without occupying the right button
    /// `map_right_click_menu` needs for the region menu, or the left button
    /// `map_click_select` needs for select/order.
    #[test]
    fn middle_button_drag_pans() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        world.init_resource::<Messages<MouseWheel>>();
        world.init_resource::<Messages<MouseMotion>>();
        world.init_resource::<Messages<PinchGesture>>();
        let mut buttons = ButtonInput::<MouseButton>::default();
        buttons.press(MouseButton::Middle);
        world.insert_resource(buttons);
        world.insert_resource(ButtonInput::<KeyCode>::default());

        let mut system = IntoSystem::into_system(mouse_pan_zoom);
        system.initialize(&mut world);

        world.resource_mut::<Messages<MouseMotion>>().write(MouseMotion { delta: Vec2::new(10.0, 20.0) });
        system.run((), &mut world).unwrap();

        let (translation, _) = camera_state(&mut world);
        assert_eq!(translation, Vec3::new(-10.0, 20.0, 0.0), "a middle-button drag must pan by its motion delta");
    }

    /// The hard requirement from the task this fix was built for: plain
    /// left-click (with or without motion) must never pan the camera -
    /// `map_click_select` alone owns it, always meaning select/order.
    #[test]
    fn left_button_drag_does_not_pan_camera() {
        let mut world = World::new();
        spawn_main_camera(&mut world);
        world.init_resource::<Messages<MouseWheel>>();
        world.init_resource::<Messages<MouseMotion>>();
        world.init_resource::<Messages<PinchGesture>>();
        let mut buttons = ButtonInput::<MouseButton>::default();
        buttons.press(MouseButton::Left);
        world.insert_resource(buttons);
        world.insert_resource(ButtonInput::<KeyCode>::default());

        let mut system = IntoSystem::into_system(mouse_pan_zoom);
        system.initialize(&mut world);

        world.resource_mut::<Messages<MouseMotion>>().write(MouseMotion { delta: Vec2::new(50.0, 50.0) });
        system.run((), &mut world).unwrap();

        let (translation, scale) = camera_state(&mut world);
        assert_eq!(translation, Vec3::ZERO, "a left-button drag must never pan the camera");
        assert_eq!(scale, 1.0, "a left-button drag must never zoom the camera either");
    }
}
