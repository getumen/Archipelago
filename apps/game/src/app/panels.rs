//! Stage 8B (owner ask: "HOIみたいなパネルってないの？操作方法が全然わからな
//! いよ"): real, clickable `bevy_ui` `Button`s for every order this client
//! can issue - top-bar speed controls, a region panel (recruit/build/cancel),
//! a unit panel (hold/reinforce - move stays "click a destination on the
//! map", unchanged), a policy panel, and a diplomacy panel. `input.rs`'s
//! keyboard bindings are left completely alone; every button here is just a
//! second way to reach the exact same `SimRes::push_human_action` door
//! (`mod.rs`'s own doc, "no privileged path" - a click enqueues an `Action`
//! exactly like a keypress does, and `Simulation::apply` is still the only
//! judge).
//!
//! ## Why this needs no `bevy_picking` feature
//!
//! `bevy_ui::widget::Button`/`Interaction` are driven by `bevy_ui`'s own
//! built-in `ui_focus_system` (registered unconditionally by `UiPlugin`,
//! confirmed by reading `bevy_ui-0.19.1/src/focus.rs`/`lib.rs`), which reads
//! `ButtonInput<MouseButton>`/`Window::physical_cursor_position` directly -
//! it needs no picking backend at all. `apps/game/Cargo.toml`'s feature list
//! (docs/phase7-spec.md "ビルド構成") stays exactly as Stage 7A left it; no
//! Cargo change was needed for this stage.
//!
//! Click detection here follows the standard Bevy idiom: `Changed<Interaction>`
//! filtered to `Interaction::Pressed` fires *once*, on the frame `ui_focus_
//! system` (in `PostUpdate`) first marks a node pressed - one frame after the
//! actual mouse-down, imperceptible to a player, and (crucially) decoupled
//! from `input.rs`'s own `ButtonInput<MouseButton>`-based map click handling.
//!
//! ## Click-vs-map-click
//!
//! `input::map_click_select`/`input::map_right_click_menu` hand-roll their
//! own hit testing against raw mouse input, with no awareness of what's
//! drawn on top of the map - so without a guard, clicking a button in a
//! screen corner would *also* register as a click somewhere on the map
//! underneath it. `PointerOverUi`, refreshed first in the `Update` chain by
//! `mark_pointer_over_ui`, is that guard: `true` whenever the cursor is
//! currently hovering or pressing any visible interactive node (hidden
//! nodes always report `Interaction::None` - `bevy_ui`'s own documented
//! guarantee - so a closed panel's buttons never trip this).
//!
//! ## "Disabled, with a reason" (docs/design.md §2)
//!
//! Every button whose action can be judged from what's already on screen
//! (region ownership/contested/construction state, a faction's own
//! manpower/stock, a pending treaty, the current diplomatic stance, ...)
//! computes that verdict here, read-only against `SimRes::world()` - never
//! mutating anything, and never the *only* judge: a click on a button this
//! module wrongly leaves enabled still goes through `Simulation::apply` and
//! gets rejected exactly like a keyboard order would
//! (`mod::rejection_target_of` attaches that rejection back to the issuing
//! panel). Region-panel reasons reuse `action_codec::action_error_ja`
//! verbatim (the mapping is exact - `RegionActionKind::reason` mirrors
//! `action::apply_recruit`/`apply_build`/`apply_cancel_build`'s own
//! preconditions one for one). Diplomacy/policy reasons are written fresh
//! instead, because the one `ActionError` those actions can fail with
//! (`InvalidValue`) is a catch-all shared by several unrelated conditions
//! and reads as nonsense out of context (e.g. quoting "指定した値が範囲外"
//! for "you already have this treaty" would confuse, not clarify).

use bevy::prelude::*;

use archipelago_sim::action::{self, Action, ActionError};
use archipelago_sim::air;
use archipelago_sim::balance::{AIR_UNIT_MACHINERY_COST, ATTRITION_SUPPLY_THRESHOLD, UNIT_EQUIPMENT, UNIT_MANPOWER};
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Stance, Treaty, ALL_TREATIES};
use archipelago_sim::focus::{NationalFocus, ALL_FOCI};
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId, TransportLineId, TransportNodeId, UnitId};
use archipelago_sim::military::{Branch, ALL_BRANCHES};
use archipelago_sim::world::{Domain, Station, World as SimWorld};

use super::chrome;
use super::input::MENU_ITEMS;
use super::map_mode::MapModeRes;
use super::setup::{text_font, RIGHT_COLUMN_WIDTH};
use super::{
    ActiveBranch, ActiveGood, DiplomacyPanel, LastRejection, NlCompose, PlayerFaction, PolicyPanel, RejectionTarget, RightColumnRoot, SelectedRegion, SelectedUnits, SimRes,
    SpeedRes,
};
use crate::action_codec::action_error_ja;
use crate::sim_driver::Speed;

// ---------------------------------------------------------------------
// Pointer-vs-map-click arbitration
// ---------------------------------------------------------------------

/// See this module's own doc, "Click-vs-map-click". Read by `input::
/// map_click_select`/`input::map_right_click_menu`; written only here.
#[derive(Resource, Default)]
pub(super) struct PointerOverUi(pub bool);

pub(super) fn mark_pointer_over_ui(mut pointer: ResMut<PointerOverUi>, interactions: Query<&Interaction>) {
    pointer.0 = interactions.iter().any(|i| *i != Interaction::None);
}

// ---------------------------------------------------------------------
// Right column: keyboard scroll (`setup::spawn_right_column`'s own doc has
// the overflow policy this implements)
// ---------------------------------------------------------------------

/// `ScrollPosition` units-per-second while `PageUp`/`PageDown` is held -
/// fast enough that a full panel's worth of overflow clears in well under a
/// second, slow enough not to read as a jump cut. Deliberately *not* driven
/// by the mouse wheel: `input::mouse_pan_zoom` already claims the wheel
/// unconditionally for camera pan/zoom (it doesn't check `PointerOverUi`
/// today), and this crate's `SCROLL_PAN_SPEED`-style constants live with
/// that system, not this one - reusing the wheel here would either fight
/// that binding or need it re-plumbed to gate on hover, neither of which
/// this fix's own scope calls for.
const RIGHT_COLUMN_SCROLL_SPEED: f32 = 600.0;

/// The right column's own overflow policy (`setup::spawn_right_column`'s own
/// doc, "Overflow policy"): content taller than the column's fixed,
/// window-relative height clips (`Overflow::scroll_y()`) rather than
/// spilling past the window's bottom edge - this is what makes that content
/// reachable again rather than silently lost. `ScrollPosition` is clamped to
/// the valid scrollable range by `bevy_ui`'s own layout system every frame
/// (`ScrollPosition`'s own doc), so holding either key past the actual
/// content's end is a no-op, not a bug needing its own clamp here.
pub(super) fn handle_right_column_scroll(keys: Res<ButtonInput<KeyCode>>, time: Res<Time>, mut query: Query<&mut ScrollPosition, With<RightColumnRoot>>) {
    let Ok(mut scroll) = query.single_mut() else { return };
    let delta = RIGHT_COLUMN_SCROLL_SPEED * time.delta_secs();
    if keys.pressed(KeyCode::PageDown) {
        scroll.0.y += delta;
    }
    if keys.pressed(KeyCode::PageUp) {
        scroll.0.y = (scroll.0.y - delta).max(0.0);
    }
}

// ---------------------------------------------------------------------
// Shared styling
// ---------------------------------------------------------------------

const COLOR_ENABLED: Color = Color::srgb(0.22, 0.30, 0.24);
const COLOR_ACTIVE: Color = Color::srgb(0.55, 0.42, 0.12);
const COLOR_DISABLED: Color = Color::srgba(0.22, 0.22, 0.24, 0.55);
const TEXT_ENABLED: Color = Color::srgb(0.92, 0.95, 0.92);
const TEXT_DISABLED: Color = Color::srgba(0.65, 0.65, 0.68, 0.8);
const TEXT_REASON: Color = Color::srgb(0.85, 0.45, 0.40);

fn button_node() -> Node {
    Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)), ..default() }
}

fn row_node() -> Node {
    Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), ..default() }
}

fn column_node() -> Node {
    Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }
}

fn button_bg(enabled: bool, active: bool) -> Color {
    if !enabled {
        COLOR_DISABLED
    } else if active {
        COLOR_ACTIVE
    } else {
        COLOR_ENABLED
    }
}

// ---------------------------------------------------------------------
// Top bar: speed controls
// ---------------------------------------------------------------------

#[derive(Component, Clone, Copy)]
pub(super) struct SpeedButton(pub Speed);

const SPEED_BUTTONS: [Speed; 4] = [Speed::Paused, Speed::X1, Speed::X5, Speed::X20];

pub(super) fn spawn_speed_buttons(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    parent.spawn(row_node()).with_children(|row| {
        for speed in SPEED_BUTTONS {
            row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), SpeedButton(speed)))
                .with_children(|b| {
                    b.spawn((Text::new(speed_label(speed)), text_font(13.0, font), TextColor(TEXT_ENABLED)));
                });
        }
    });
}

fn speed_label(speed: Speed) -> &'static str {
    match speed {
        Speed::Paused => "停止 [Space]",
        Speed::X1 => "1x [1]",
        Speed::X5 => "5x [2]",
        Speed::X20 => "20x [3]",
    }
}

pub(super) fn sync_speed_buttons(speed: Res<SpeedRes>, mut query: Query<(&SpeedButton, &mut BackgroundColor, &Children)>, mut text: Query<&mut TextColor>) {
    for (button, mut bg, children) in &mut query {
        let active = match button.0 {
            Speed::Paused => speed.paused,
            other => !speed.paused && speed.last_active == other,
        };
        bg.0 = if active { COLOR_ACTIVE } else { COLOR_ENABLED };
        for &child in children {
            if let Ok(mut color) = text.get_mut(child) {
                color.0 = TEXT_ENABLED;
            }
        }
    }
}

pub(super) fn handle_speed_button_clicks(mut speed: ResMut<SpeedRes>, query: Query<(&Interaction, &SpeedButton), Changed<Interaction>>) {
    for (interaction, button) in &query {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button.0 {
            Speed::Paused => speed.paused = true,
            other => {
                speed.last_active = other;
                speed.paused = false;
            }
        }
    }
}

// ---------------------------------------------------------------------
// Top bar: map-mode button (`map_mode`'s own module doc has the full list
// of modes and rationale) - the discoverable, clickable half of that
// mechanism; `input::keyboard_input`'s `M` binding is the other. Available
// in observer mode too (spawned unconditionally in `setup::spawn_ui`,
// unlike the policy/diplomacy toggles), since seeing terrain/population/
// industry/unrest never requires a `--play`ed faction.
// ---------------------------------------------------------------------

#[derive(Component)]
pub(super) struct MapModeButton;

/// Spawned with empty text - `sync_map_mode_button` fills it in on the very
/// first `Update` tick, the same "spawn empty, let the sync system fill it"
/// pattern every other dynamic panel in this crate uses, so this never needs
/// to know the mode `app::run` actually started in (`--debug-map-mode`).
pub(super) fn spawn_map_mode_button(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    parent.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), MapModeButton)).with_children(|b| {
        b.spawn((Text::new(String::new()), text_font(13.0, font), TextColor(TEXT_ENABLED)));
    });
}

pub(super) fn handle_map_mode_button_clicks(mut mode: ResMut<MapModeRes>, query: Query<&Interaction, (Changed<Interaction>, With<MapModeButton>)>) {
    for interaction in &query {
        if *interaction == Interaction::Pressed {
            mode.0 = mode.0.next();
        }
    }
}

pub(super) fn sync_map_mode_button(mode: Res<MapModeRes>, query: Query<&Children, With<MapModeButton>>, mut text: Query<&mut Text>) {
    for children in &query {
        for &child in children {
            if let Ok(mut t) = text.get_mut(child) {
                t.0 = format!("地図: {} [M]", mode.0.label());
            }
        }
    }
}

// ---------------------------------------------------------------------
// Region panel: recruit/build/cancel action buttons
// ---------------------------------------------------------------------

#[derive(Component)]
pub(super) struct RegionActionPanelRoot;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegionActionKind {
    RecruitLand,
    RecruitSea,
    /// Stage 10 follow-up (docs/phase10-spec.md "1. 基地"): the region
    /// panel's own way to raise a squadron - `action::apply_recruit`'s
    /// `Domain::Air` arm requires an operational `Airfield` node in the
    /// region, the air-domain sibling of `RecruitSea`'s port requirement.
    RecruitAir,
    BuildInfra,
    BuildPort,
    BuildCapacity,
    Repair,
    CancelBuild,
}

/// Same order as `input::MENU_ITEMS`/`input::handle_menu_keys`'s digit keys -
/// `index()` below is the shared key between the two.
const REGION_ACTION_KINDS: [RegionActionKind; 8] = [
    RegionActionKind::RecruitLand,
    RegionActionKind::RecruitSea,
    RegionActionKind::RecruitAir,
    RegionActionKind::BuildInfra,
    RegionActionKind::BuildPort,
    RegionActionKind::BuildCapacity,
    RegionActionKind::Repair,
    RegionActionKind::CancelBuild,
];

impl RegionActionKind {
    fn index(self) -> usize {
        REGION_ACTION_KINDS.iter().position(|&k| k == self).expect("every RegionActionKind is listed in REGION_ACTION_KINDS")
    }

    fn label(self, active_good: Good, active_branch: Branch) -> String {
        let base = MENU_ITEMS[self.index()];
        match self {
            RegionActionKind::BuildCapacity => format!("{base} [{}]", active_good.label()),
            // Stage 11C (docs/phase11-spec.md §4 "地図とパネルで兵科が分か
            // る"): mirrors `BuildCapacity`'s own `[good]` suffix just
            // above - `ActiveGood`'s exact convention, so which branch a
            // click on this button would actually raise is visible on the
            // button itself, not just discoverable by clicking and checking
            // afterward.
            RegionActionKind::RecruitLand => format!("{base} [{}]", active_branch.label()),
            _ => base.to_string(),
        }
    }

    /// The right-click-menu digit that does the same thing - shown so the
    /// keyboard path stays discoverable (task ask), not secret.
    fn shortcut_hint(self) -> String {
        format!("(右クリック→{})", self.index() + 1)
    }

    fn to_action(self, region: RegionId, active_good: Good, active_branch: Branch) -> Action {
        match self {
            RegionActionKind::RecruitLand => Action::RecruitUnit { region, domain: Domain::Land, branch: active_branch },
            RegionActionKind::RecruitSea => Action::RecruitUnit { region, domain: Domain::Sea, branch: Branch::Infantry },
            RegionActionKind::RecruitAir => Action::RecruitUnit { region, domain: Domain::Air, branch: Branch::Infantry },
            RegionActionKind::BuildInfra => Action::Build { region, project: Project::Infrastructure },
            RegionActionKind::BuildPort => Action::Build { region, project: Project::Port },
            RegionActionKind::BuildCapacity => Action::Build { region, project: Project::Capacity(active_good) },
            RegionActionKind::Repair => Action::Build { region, project: Project::Repair },
            RegionActionKind::CancelBuild => Action::CancelBuild { region },
        }
    }

    /// Mirrors `action::apply_recruit`/`apply_build`/`apply_cancel_build`'s
    /// own visible preconditions - see this module's own doc, "Disabled,
    /// with a reason". Read-only; never mutates `world`.
    fn reason(self, world: &SimWorld, faction: FactionId, region_id: RegionId, active_branch: Branch) -> Option<String> {
        let region = world.regions.get(region_id.index())?;
        if region.owner != faction {
            return Some(action_error_ja(ActionError::RegionNotOwned).to_string());
        }
        if world.has_enemy_units(region_id, faction) {
            return Some(action_error_ja(ActionError::RegionContested).to_string());
        }
        match self {
            // Stage 11C: the button must read "enabled"/"disabled" against
            // whichever branch it would *actually* raise
            // (`active_branch.equipment_good()`), not the fixed `Good::
            // Infantry` every land recruit used to spend regardless of
            // choice - `Branch::equipment_good`'s own doc.
            RegionActionKind::RecruitLand => recruit_reason(world, faction, Domain::Land, active_branch.equipment_good()),
            RegionActionKind::RecruitSea => {
                if region.port <= 0.0 {
                    return Some(action_error_ja(ActionError::NoPort).to_string());
                }
                recruit_reason(world, faction, Domain::Sea, Good::Naval)
            }
            RegionActionKind::RecruitAir => {
                if !world.airfield_node_operational(region_id) {
                    return Some(action_error_ja(ActionError::NoAirfield).to_string());
                }
                recruit_reason(world, faction, Domain::Air, Good::Aircraft)
            }
            RegionActionKind::BuildInfra | RegionActionKind::BuildPort | RegionActionKind::BuildCapacity | RegionActionKind::Repair => {
                if region.construction.is_some() {
                    Some(action_error_ja(ActionError::AlreadyBuilding).to_string())
                } else {
                    None
                }
            }
            RegionActionKind::CancelBuild => {
                if region.construction.is_none() {
                    Some(action_error_ja(ActionError::NoConstruction).to_string())
                } else {
                    None
                }
            }
        }
    }
}

/// `equipment_good` is which `Good` `apply_recruit` will actually charge
/// for this specific recruit - `active_branch.equipment_good()` for
/// `Domain::Land` (Stage 11C: this varies by the player's own branch
/// choice, `Branch::equipment_good`'s own doc), `Good::Naval`/`Good::
/// Aircraft` fixed for Sea/Air (`good::Good`'s own module doc - Stage 11B
/// gave them their own commodity instead of sharing `Good::Infantry`).
/// Checking the wrong good here would let this button read "enabled" right
/// up until the simulation actually rejects the order, or "disabled" while
/// the commodity it would actually spend from is perfectly solvent.
fn recruit_reason(world: &SimWorld, faction: FactionId, domain: Domain, equipment_good: Good) -> Option<String> {
    let f = world.faction(faction);
    if f.manpower < UNIT_MANPOWER {
        return Some(action_error_ja(ActionError::InsufficientManpower).to_string());
    }
    if f.stock[equipment_good.index()] < UNIT_EQUIPMENT {
        // Defect fix: `action_error_ja(ActionError::InsufficientEquipment)`
        // is one fixed string for all three land branches plus Sea/Air, so
        // with (say) infantry equipment in stock and armour equipment at
        // zero, tabbing to 機甲 and clicking recruit used to give a reason
        // that never named armour - the button's own `[機甲]` label
        // (`RegionActionKind::label`) only partly mitigated it. This
        // already knows exactly which `Good` the sim would actually charge
        // (`equipment_good`, threaded in by every caller below), so name it.
        return Some(format!("{}が不足している", equipment_good.label()));
    }
    // Stage 10 follow-up: `action::apply_recruit`'s `Domain::Air` arm also
    // spends `Good::Machinery` (`balance::AIR_UNIT_MACHINERY_COST`) - the
    // airframe-specific cost no other domain pays, mirrored here so a
    // squadron the sim would reject as `InsufficientMachinery` never shows
    // as an enabled button in the first place.
    if domain == Domain::Air && f.stock[Good::Machinery.index()] < AIR_UNIT_MACHINERY_COST {
        return Some(action_error_ja(ActionError::InsufficientMachinery).to_string());
    }
    None
}

/// A child of `setup::spawn_right_column`'s shared flex column - not
/// independently positioned (`setup::spawn_right_column`'s own doc has the
/// full rationale). `width: Val::Px(340.0)` still fixes this panel's own
/// width (the widest of the column's children, and the width the column
/// itself is sized to - `setup::spawn_right_column`'s own `WIDTH`); it no
/// longer needs `position_type`/`top`/`right` since the column places it.
/// Starts `display: Display::None` (as well as `Visibility::Hidden`,
/// unchanged) so a closed panel also reserves no space in the column -
/// `sync_region_action_buttons` keeps both in sync with `showing` every
/// frame from here on.
pub(super) fn spawn_region_action_panel(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    parent
        .spawn((
            chrome::framed(Node {
                display: Display::None,
                width: Val::Px(340.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                ..default()
            }),
            chrome::panel_background(),
            chrome::panel_border(),
            Visibility::Hidden,
            RegionActionPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- 命令 --", font));
            for kind in REGION_ACTION_KINDS {
                // Stage 11C (`codex review` P2): the legend advertised a
                // clickable "C" control for `ActiveBranch`, but until this
                // fix the only way to change it was the keyboard - a
                // mouse-only player had no path to it at all, unlike
                // `ActiveGood`'s own `GoodTabButton` row in the policy panel
                // (`spawn_policy_panel`'s exact pattern, mirrored here).
                // Spawned directly above the "陸軍を徴募" row it feeds, so
                // the choice and its effect sit next to each other.
                if kind == RegionActionKind::RecruitLand {
                    panel
                        .spawn(Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), ..default() })
                        .with_children(|row| {
                            for branch in ALL_BRANCHES {
                                row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), BranchTabButton(branch))).with_children(|b| {
                                    b.spawn((Text::new(branch.label()), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                                });
                            }
                        });
                }
                panel.spawn(column_node()).with_children(|slot| {
                    slot.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), kind)).with_children(|b| {
                        b.spawn((Text::new(String::new()), text_font(12.0, font), TextColor(TEXT_ENABLED), RegionActionLabel(kind)));
                    });
                    slot.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), RegionActionReason(kind)));
                });
            }
        });
}

#[derive(Component, Clone, Copy)]
pub(super) struct BranchTabButton(Branch);

/// The clickable half of `ActiveBranch` (`input::keyboard_input`'s `C`
/// binding is the other) - `handle_policy_button_clicks`'s `GoodTabButton`
/// loop's exact shape: a direct-select tab, not a cycle, since with only
/// three branches a full row of named tabs costs nothing a cycle-through
/// button would save.
pub(super) fn handle_branch_tab_clicks(mut active_branch: ResMut<ActiveBranch>, query: Query<(&Interaction, &BranchTabButton), Changed<Interaction>>) {
    for (interaction, tab) in &query {
        if *interaction == Interaction::Pressed {
            active_branch.0 = tab.0;
        }
    }
}

/// Highlights whichever `BranchTabButton` matches the current
/// `ActiveBranch` - `sync_policy_panel`'s `good_tabs` loop's exact
/// convention (`COLOR_ACTIVE` for the selected tab, `COLOR_ENABLED`
/// otherwise). Runs unconditionally (not gated on the region panel's own
/// `showing` the way `sync_region_action_buttons` is) since `ActiveBranch`
/// itself is meaningful even before a region is selected - keeping this
/// tab row's highlight in sync the instant `C` (or a click) changes it,
/// not just the next time the region panel happens to redraw.
pub(super) fn sync_branch_tabs(active_branch: Res<ActiveBranch>, mut tabs: Query<(&BranchTabButton, &mut BackgroundColor)>) {
    for (tab, mut bg) in &mut tabs {
        bg.0 = if tab.0 == active_branch.0 { COLOR_ACTIVE } else { COLOR_ENABLED };
    }
}

#[derive(Component, Clone, Copy)]
pub(super) struct RegionActionLabel(RegionActionKind);

#[derive(Component, Clone, Copy)]
pub(super) struct RegionActionReason(RegionActionKind);

#[allow(clippy::too_many_arguments)]
pub(super) fn sync_region_action_buttons(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedRegion>,
    diplomacy: Res<DiplomacyPanel>,
    policy: Res<PolicyPanel>,
    active_good: Res<ActiveGood>,
    active_branch: Res<ActiveBranch>,
    mut root: Query<(&mut Visibility, &mut Node), With<RegionActionPanelRoot>>,
    mut buttons: Query<(&RegionActionKind, &mut BackgroundColor)>,
    mut labels: Query<(&RegionActionLabel, &mut Text, &mut TextColor)>,
    mut reasons: Query<(&RegionActionReason, &mut Text), Without<RegionActionLabel>>,
) {
    let Ok((mut visibility, mut node)) = root.single_mut() else { return };
    let showing = player.0.is_some() && selected.0.is_some() && !diplomacy.open && !policy.0;
    chrome::set_panel_shown(&mut visibility, &mut node, showing);
    if !showing {
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let Some(region_id) = selected.0 else { return };
    let world = sim.0.world();

    for kind in REGION_ACTION_KINDS {
        let reason = kind.reason(world, player_faction, region_id, active_branch.0);
        for (k, mut bg) in &mut buttons {
            if *k == kind {
                bg.0 = button_bg(reason.is_none(), false);
            }
        }
        for (label, mut text, mut color) in &mut labels {
            if label.0 == kind {
                text.0 = format!("{} {}", kind.label(active_good.0, active_branch.0), kind.shortcut_hint());
                color.0 = if reason.is_none() { TEXT_ENABLED } else { TEXT_DISABLED };
            }
        }
        for (r, mut text) in &mut reasons {
            if r.0 == kind {
                text.0 = reason.clone().unwrap_or_default();
            }
        }
    }
}

pub(super) fn handle_region_action_clicks(
    mut sim: ResMut<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedRegion>,
    active_good: Res<ActiveGood>,
    active_branch: Res<ActiveBranch>,
    query: Query<(&Interaction, &RegionActionKind), Changed<Interaction>>,
) {
    let Some(player_faction) = player.0 else { return };
    let Some(region_id) = selected.0 else { return };
    for (interaction, kind) in &query {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if kind.reason(sim.0.world(), player_faction, region_id, active_branch.0).is_some() {
            continue;
        }
        sim.0.push_human_action(kind.to_action(region_id, active_good.0, active_branch.0));
    }
}

// ---------------------------------------------------------------------
// Strike panel: air power's other order (docs/phase10-spec.md "3. 阻止" -
// `Action::StrikeNode` against an enemy airfield/port), the same "disabled,
// with a reason" pattern the region panel above uses. Shown for *any*
// selected region (own or foreign, at war or not) - `StrikeKind::reason`
// alone decides whether either button is actually clickable, exactly the
// way `RegionActionKind::reason` already gates recruit/build against a
// region the player doesn't own, so a player can select a hostile region and
// immediately see (and use) whichever strike targets it actually has,
// without a separate "enter strike mode" step.
// ---------------------------------------------------------------------

#[derive(Component)]
pub(super) struct StrikePanelRoot;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum StrikeKind {
    Airfield,
    Port,
}

const STRIKE_KINDS: [StrikeKind; 2] = [StrikeKind::Airfield, StrikeKind::Port];

impl StrikeKind {
    fn label(self) -> &'static str {
        match self {
            StrikeKind::Airfield => "飛行場を空爆",
            StrikeKind::Port => "港湾を空爆",
        }
    }

    /// The specific node this kind targets in `region`, if `region` has a
    /// working one.
    ///
    /// **Operational, not merely first** (`codex review`, P2). A region may
    /// declare several airfields or ports; taking the lowest-id one meant
    /// that once it was wrecked, every further click hit the same rubble
    /// while the region's other, intact nodes stayed impossible for a player
    /// to target at all. This is the fourth place in Phase 10 where "the
    /// region has one" and "here is one to use" were answered by the same
    /// lowest-id lookup - `World::operational_airfield_node`/
    /// `operational_port_node` exist so the two questions stop sharing an
    /// answer.
    fn node(self, world: &SimWorld, region: RegionId) -> Option<TransportNodeId> {
        match self {
            StrikeKind::Airfield => world.operational_airfield_node(region).map(|n| n.id),
            StrikeKind::Port => world.operational_port_node(region).map(|n| n.id),
        }
    }

    /// Mirrors `action::apply_strike_node`'s own preconditions one for one -
    /// same rationale as `RegionActionKind::reason`'s own doc. `kind` is
    /// never `NodeNotStrikeable` here: `node` above only ever names an
    /// `Airfield`/`Port` node by construction, so that `ActionError` variant
    /// can never actually apply to a button this panel offers.
    ///
    /// **Reach, not just hostility** (`codex review` P1 - closing the hole
    /// that let a faction with zero aircraft bomb any hostile node for
    /// free): `apply_strike_node` now also asks `air::units_reaching`
    /// whether `faction` owns any air unit able to reach `region` at all,
    /// and rejects with `ActionError::NoAircraftInRange` if not. Gating the
    /// button on the client side would be treating the symptom - the rule
    /// lives in the action - but the button still has to *reflect* that
    /// rule the way it already reflects `NodeNotHostile`/`NoAirfield`/
    /// `NoPort`, so this reuses the exact same `air::units_reaching` call
    /// `apply_strike_node` itself makes, not a second, client-side notion
    /// of "close enough to strike".
    fn reason(self, world: &SimWorld, faction: FactionId, region: RegionId) -> Option<&'static str> {
        if self.node(world, region).is_none() {
            return Some(action_error_ja(match self {
                StrikeKind::Airfield => ActionError::NoAirfield,
                StrikeKind::Port => ActionError::NoPort,
            }));
        }
        let owner = world.region(region).owner;
        if owner == faction || !world.diplomacy.is_at_war(faction, owner) {
            return Some(action_error_ja(ActionError::NodeNotHostile));
        }
        if air::units_reaching(world, region, faction).is_empty() {
            return Some(action_error_ja(ActionError::NoAircraftInRange));
        }
        None
    }
}

pub(super) fn spawn_strike_panel(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    parent
        .spawn((
            chrome::framed(Node {
                display: Display::None,
                width: Val::Px(RIGHT_COLUMN_WIDTH),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                ..default()
            }),
            chrome::panel_background(),
            chrome::panel_border(),
            Visibility::Hidden,
            StrikePanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- 空爆（敵の飛行場・港） --", font));
            for kind in STRIKE_KINDS {
                panel.spawn(column_node()).with_children(|slot| {
                    slot.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), kind)).with_children(|b| {
                        b.spawn((Text::new(kind.label()), text_font(12.0, font), TextColor(TEXT_ENABLED), StrikeLabel(kind)));
                    });
                    slot.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), StrikeReason(kind)));
                });
            }
        });
}

#[derive(Component, Clone, Copy)]
pub(super) struct StrikeLabel(StrikeKind);

#[derive(Component, Clone, Copy)]
pub(super) struct StrikeReason(StrikeKind);

pub(super) fn sync_strike_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedRegion>,
    diplomacy: Res<DiplomacyPanel>,
    policy: Res<PolicyPanel>,
    mut root: Query<(&mut Visibility, &mut Node), With<StrikePanelRoot>>,
    mut buttons: Query<(&StrikeKind, &mut BackgroundColor)>,
    mut labels: Query<(&StrikeLabel, &mut TextColor)>,
    mut reasons: Query<(&StrikeReason, &mut Text)>,
) {
    let Ok((mut visibility, mut node)) = root.single_mut() else { return };
    let showing = player.0.is_some() && selected.0.is_some() && !diplomacy.open && !policy.0;
    chrome::set_panel_shown(&mut visibility, &mut node, showing);
    if !showing {
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let Some(region_id) = selected.0 else { return };
    let world = sim.0.world();

    for kind in STRIKE_KINDS {
        let reason = kind.reason(world, player_faction, region_id);
        for (k, mut bg) in &mut buttons {
            if *k == kind {
                bg.0 = button_bg(reason.is_none(), false);
            }
        }
        for (label, mut color) in &mut labels {
            if label.0 == kind {
                color.0 = if reason.is_none() { TEXT_ENABLED } else { TEXT_DISABLED };
            }
        }
        for (r, mut text) in &mut reasons {
            if r.0 == kind {
                text.0 = reason.unwrap_or("").to_string();
            }
        }
    }
}

pub(super) fn handle_strike_clicks(
    mut sim: ResMut<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedRegion>,
    query: Query<(&Interaction, &StrikeKind), Changed<Interaction>>,
) {
    let Some(player_faction) = player.0 else { return };
    let Some(region_id) = selected.0 else { return };
    for (interaction, kind) in &query {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if kind.reason(sim.0.world(), player_faction, region_id).is_some() {
            continue;
        }
        let Some(node) = kind.node(sim.0.world(), region_id) else { continue };
        sim.0.push_human_action(Action::StrikeNode { node });
    }
}

// ---------------------------------------------------------------------
// Interdict panel: the transport network's own order (docs/phase9-spec.md
// "4. 行動" / docs/phase10-spec.md "3. 阻止" - `Action::InterdictLine`
// against a hostile transport line), the same "disabled, with a reason"
// pattern the strike panel above uses. Unlike the strike panel (a fixed
// two-kind choice per region: its airfield, its port), a region can touch
// any number of transport lines, so this borrows `UnitPanelRoot`'s own
// fixed-size row-pool shape instead of `StrikePanelRoot`'s fixed two
// buttons - `MAX_INTERDICT_ROWS` rows, with an overflow count past that.
// ---------------------------------------------------------------------

/// Enough rows that every line touching a region is actually reachable
/// through this panel, **measured from the shipped scenarios** rather than
/// picked: the busiest region touches 6 lines in `mvp.json` and 8 in both
/// `japan47.json` and `japan_hex.json` (counted over each scenario's own
/// `transport.lines`). At 4 (`codex review`, P2) the panel silently made
/// every line past the fourth impossible for a player to interdict at all -
/// there is no scrolling or paging here, so an overflow count is not an
/// alternate route to them, it is a dead end.
///
/// The overflow line stays as a guard for a future scenario that declares a
/// denser region than anything shipped today; it should not be reachable
/// with the current data.
const MAX_INTERDICT_ROWS: usize = 8;

#[derive(Component)]
pub(super) struct InterdictPanelRoot;

#[derive(Component)]
pub(super) struct InterdictRowContainer(usize);

#[derive(Component)]
pub(super) struct InterdictRowText(usize);

#[derive(Component, Clone, Copy)]
pub(super) struct InterdictButton(usize);

#[derive(Component)]
pub(super) struct InterdictReason(usize);

#[derive(Component)]
pub(super) struct InterdictOverflowText;

/// Which line (if any) each pool row currently shows - the same purpose
/// `UnitPanelSlots` serves for the unit panel: written by
/// `sync_interdict_panel`, read by `handle_interdict_clicks` so a click on
/// slot N's button resolves to the *current* frame's line at that slot,
/// not whatever line happened to occupy it when the click was queued.
#[derive(Resource, Default)]
pub(super) struct InterdictPanelSlots(pub [Option<u32>; MAX_INTERDICT_ROWS]);

/// Every transport line touching `region` at either endpoint that is owned
/// entirely by one faction other than `faction` - the "single owner, not
/// this faction's own" half of `apply_interdict_line`'s own precondition,
/// mirrored here so the panel only ever lists lines that could plausibly be
/// legal targets. Deliberately *not* filtered by war state or reach -
/// `interdict_reason` below answers those, the same split `StrikeKind::
/// node`/`StrikeKind::reason` already keep between "does a candidate exist"
/// and "is it currently usable". Ascending `TransportLineId` order
/// (`World::transport_lines`'s own storage order, never a `HashMap`), so
/// the row list is a pure, deterministic function of world state.
fn lines_touching(world: &SimWorld, faction: FactionId, region: RegionId) -> Vec<TransportLineId> {
    world
        .transport_lines
        .iter()
        .filter(|line| {
            let region_a = world.transport_node(line.from).region;
            let region_b = world.transport_node(line.to).region;
            if region_a != region && region_b != region {
                return false;
            }
            let owner_a = world.region(region_a).owner;
            let owner_b = world.region(region_b).owner;
            owner_a == owner_b && owner_a != faction
        })
        .map(|line| line.id)
        .collect()
}

/// Mirrors `action::apply_interdict_line`'s own preconditions one for one -
/// same rationale as `StrikeKind::reason`'s own doc, including reach:
/// reuses `action::any_force_reaches` verbatim, the exact function
/// `apply_interdict_line` itself now calls, rather than a second,
/// independently-invented client-side notion of "close enough" per domain.
fn interdict_reason(world: &SimWorld, faction: FactionId, line: TransportLineId) -> Option<&'static str> {
    let Some(existing) = world.transport_lines.get(line.index()) else {
        return Some(action_error_ja(ActionError::InvalidLine));
    };
    let region_a = world.transport_node(existing.from).region;
    let region_b = world.transport_node(existing.to).region;
    let owner_a = world.region(region_a).owner;
    let owner_b = world.region(region_b).owner;
    if owner_a != owner_b || owner_a == faction || !world.diplomacy.is_at_war(faction, owner_a) {
        return Some(action_error_ja(ActionError::LineNotHostile));
    }
    if !action::any_force_reaches(world, region_a, faction) && !action::any_force_reaches(world, region_b, faction) {
        return Some(action_error_ja(ActionError::NoForceInRange));
    }
    None
}

pub(super) fn spawn_interdict_panel(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    parent
        .spawn((
            chrome::framed(Node {
                display: Display::None,
                width: Val::Px(RIGHT_COLUMN_WIDTH),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                ..default()
            }),
            chrome::panel_background(),
            chrome::panel_border(),
            Visibility::Hidden,
            InterdictPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- 輸送路線を遮断 --", font));
            for slot in 0..MAX_INTERDICT_ROWS {
                panel.spawn((column_node(), Visibility::Hidden, InterdictRowContainer(slot))).with_children(|row| {
                    row.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(TEXT_ENABLED), InterdictRowText(slot)));
                    row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), InterdictButton(slot))).with_children(|b| {
                        b.spawn((Text::new("遮断"), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                    });
                    row.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), InterdictReason(slot)));
                });
            }
            panel.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(Color::srgb(0.7, 0.72, 0.75)), InterdictOverflowText));
        });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn sync_interdict_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedRegion>,
    diplomacy: Res<DiplomacyPanel>,
    policy: Res<PolicyPanel>,
    mut slots: ResMut<InterdictPanelSlots>,
    mut root: Query<(&mut Visibility, &mut Node), With<InterdictPanelRoot>>,
    mut row_containers: Query<(&InterdictRowContainer, &mut Visibility), Without<InterdictPanelRoot>>,
    mut row_texts: Query<(&InterdictRowText, &mut Text), (Without<InterdictReason>, Without<InterdictOverflowText>)>,
    mut buttons: Query<(&InterdictButton, &mut BackgroundColor)>,
    mut reasons: Query<(&InterdictReason, &mut Text), Without<InterdictRowText>>,
    mut overflow: Query<&mut Text, (With<InterdictOverflowText>, Without<InterdictRowText>, Without<InterdictReason>)>,
) {
    let Ok((mut visibility, mut node)) = root.single_mut() else { return };
    let showing = player.0.is_some() && selected.0.is_some() && !diplomacy.open && !policy.0;
    chrome::set_panel_shown(&mut visibility, &mut node, showing);
    if !showing {
        *slots = InterdictPanelSlots::default();
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let Some(region_id) = selected.0 else { return };
    let world = sim.0.world();

    let lines = lines_touching(world, player_faction, region_id);
    let mut new_slots = [None; MAX_INTERDICT_ROWS];
    for slot in 0..MAX_INTERDICT_ROWS {
        new_slots[slot] = lines.get(slot).map(|l| l.0);
    }
    slots.0 = new_slots;

    for slot in 0..MAX_INTERDICT_ROWS {
        let line = slots.0[slot].map(TransportLineId);
        for (container, mut vis) in &mut row_containers {
            if container.0 == slot {
                *vis = if line.is_some() { Visibility::Visible } else { Visibility::Hidden };
            }
        }
        let reason = line.and_then(|l| interdict_reason(world, player_faction, l));
        let row_line = line
            .and_then(|l| world.transport_lines.get(l.index()))
            .map(|l| {
                let other_region = if world.transport_node(l.from).region == region_id {
                    world.transport_node(l.to).region
                } else {
                    world.transport_node(l.from).region
                };
                format!("路線 #{} ({:.0}%) - {}", l.id.0, l.condition.get() * 100.0, world.region(other_region).name)
            })
            .unwrap_or_default();
        for (row, mut text) in &mut row_texts {
            if row.0 == slot {
                text.0 = row_line.clone();
            }
        }
        for (btn, mut bg) in &mut buttons {
            if btn.0 == slot {
                bg.0 = button_bg(reason.is_none() && line.is_some(), false);
            }
        }
        for (r, mut text) in &mut reasons {
            if r.0 == slot {
                text.0 = reason.unwrap_or("").to_string();
            }
        }
    }

    if let Ok(mut text) = overflow.single_mut() {
        text.0 = if lines.len() > MAX_INTERDICT_ROWS {
            format!("ほか {} 路線", lines.len() - MAX_INTERDICT_ROWS)
        } else {
            String::new()
        };
    }
}

pub(super) fn handle_interdict_clicks(
    mut sim: ResMut<SimRes>,
    player: Res<PlayerFaction>,
    slots: Res<InterdictPanelSlots>,
    query: Query<(&Interaction, &InterdictButton), Changed<Interaction>>,
) {
    let Some(player_faction) = player.0 else { return };
    for (interaction, btn) in &query {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(line_id) = slots.0[btn.0] else { continue };
        let line = TransportLineId(line_id);
        if interdict_reason(sim.0.world(), player_faction, line).is_some() {
            continue;
        }
        sim.0.push_human_action(Action::InterdictLine { line });
    }
}

// ---------------------------------------------------------------------
// Unit panel: per-selected-unit stats + hold/reinforce
// ---------------------------------------------------------------------

/// Fixed-size pool of visible rows (`mod.rs`'s own doc pattern: pre-spawn
/// once, mutate every frame - never despawn/respawn, which would reset
/// every row's `Interaction` component and break `Changed<Interaction>`
/// click detection). A selection larger than this shows an overflow count
/// instead of silently truncating.
const MAX_UNIT_ROWS: usize = 6;

#[derive(Component)]
pub(super) struct UnitPanelRoot;

/// The per-slot container (row text + hold/reinforce/disband buttons +
/// reinforce reason) - hidden as a whole while its slot has no unit, so an
/// empty pool slot doesn't leave a floating, unlabeled set of buttons on
/// screen.
#[derive(Component)]
pub(super) struct UnitRowContainer(usize);

#[derive(Component)]
pub(super) struct UnitRowText(usize);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnitActionKind {
    Hold,
    Reinforce,
    /// The disband-defect fix's button: `Action::DisbandUnit`, standing the
    /// unit down and returning its manpower/equipment to the faction's
    /// pools (`action::apply_disband`'s doc). Shares `reinforce_disabled_reason`
    /// with `Reinforce` below - both are refused under the exact same
    /// enemy-contact condition, not two independent checks that happen to
    /// agree.
    Disband,
}

#[derive(Component, Clone, Copy)]
pub(super) struct UnitActionButton {
    slot: usize,
    kind: UnitActionKind,
}

/// Military delegation's own button (docs/design.md §14): toggles slot
/// N's unit between player-controlled and delegated to
/// `archipelago_agents::HumanAgent`'s wrapped `HeuristicAgent`. Kept
/// separate from `UnitActionButton` rather than added as a fourth
/// `UnitActionKind` variant - unlike `Hold`/`Reinforce`/`Disband`, this
/// never produces an `Action` at all (`SimDriver::delegate_unit`/
/// `undelegate_unit`'s own doc: delegation is `HumanAgent` state, not a
/// `Simulation` mutation), so `handle_unit_action_clicks`'s `Action`-only
/// match would have needed a dead arm for it.
#[derive(Component, Clone, Copy)]
pub(super) struct UnitDelegateButton(usize);

/// The delegate button's own label text, re-synced every frame by
/// `sync_unit_panel` between "AI委任 [U]" (currently player-controlled) and
/// "操作を戻す [U]" (currently delegated), so the button always reads as an
/// action ("hand this over" / "take this back") rather than a static state
/// readout - the row text's own "[AI操作中]" marker (`sync_unit_panel`)
/// already covers the state readout itself.
#[derive(Component)]
pub(super) struct UnitDelegateLabel(usize);

#[derive(Component)]
pub(super) struct UnitReinforceReason(usize);

#[derive(Component)]
pub(super) struct UnitPanelOverflowText;

/// Which unit (if any) each pool row currently shows - written by
/// `sync_unit_panel`, read by `handle_unit_action_clicks` so a click on
/// slot N's button resolves to the *current* frame's unit at that slot.
#[derive(Resource, Default)]
pub(super) struct UnitPanelSlots(pub [Option<u32>; MAX_UNIT_ROWS]);

/// A child of `setup::spawn_left_column`'s shared flex column, not its own
/// independently `PositionType::Absolute` box - this used to sit at a fixed
/// `top: 300.0`, hand-tuned against how tall `setup::FactionPanelText`'s own
/// box (directly above it) happened to be. That guess broke as soon as this
/// task gave the faction panel a title row and chrome padding/border of its
/// own: with several diplomatic relations listed, the faction panel's real
/// height already exceeded `300.0` even before this task, and this task's
/// own added height pushed it further - reproduced with `--play 関東府
/// --debug-open-diplomacy --debug-select-region 東京1` on `japan_hex`
/// (confirmed by screenshot: this panel's own title and first unit row
/// rendered directly on top of the faction panel's diplomacy list, both
/// legible text fighting for the same pixels). Fixed the same way `setup::
/// spawn_right_column`'s own doc already fixed the equivalent right-column
/// collision: both panels are now children of one `FlexDirection::Column`
/// container, so this one always starts exactly where the faction panel's
/// own box actually ends, not where it was guessed to end.
pub(super) fn spawn_unit_panel(commands: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    commands
        .spawn((
            chrome::framed(Node { width: Val::Px(300.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }),
            chrome::panel_background(),
            chrome::panel_border(),
            Visibility::Hidden,
            UnitPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- 選択部隊 --", font));
            for slot in 0..MAX_UNIT_ROWS {
                panel.spawn((column_node(), Visibility::Hidden, UnitRowContainer(slot))).with_children(|row| {
                    row.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(TEXT_ENABLED), UnitRowText(slot)));
                    row.spawn(row_node()).with_children(|buttons| {
                        buttons
                            .spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), UnitActionButton { slot, kind: UnitActionKind::Hold }))
                            .with_children(|b| {
                                b.spawn((Text::new("待機 [H]"), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                            });
                        buttons
                            .spawn((
                                Button,
                                button_node(),
                                BackgroundColor(COLOR_ENABLED),
                                UnitActionButton { slot, kind: UnitActionKind::Reinforce },
                            ))
                            .with_children(|b| {
                                b.spawn((Text::new("補充 [J]"), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                            });
                        buttons
                            .spawn((
                                Button,
                                button_node(),
                                BackgroundColor(COLOR_ENABLED),
                                UnitActionButton { slot, kind: UnitActionKind::Disband },
                            ))
                            .with_children(|b| {
                                b.spawn((Text::new("解散 [K]"), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                            });
                        buttons
                            .spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), UnitDelegateButton(slot)))
                            .with_children(|b| {
                                b.spawn((Text::new("AI委任 [U]"), text_font(11.0, font), TextColor(TEXT_ENABLED), UnitDelegateLabel(slot)));
                            });
                    });
                    row.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), UnitReinforceReason(slot)));
                });
            }
            panel.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(Color::srgb(0.7, 0.72, 0.75)), UnitPanelOverflowText));
            panel.spawn((
                Text::new("移動: 部隊選択後、地図上の目的地をクリック"),
                text_font(10.0, font),
                TextColor(Color::srgb(0.7, 0.72, 0.75)),
            ));
        });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn sync_unit_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedUnits>,
    mut slots: ResMut<UnitPanelSlots>,
    mut root: Query<&mut Visibility, With<UnitPanelRoot>>,
    mut row_containers: Query<(&UnitRowContainer, &mut Visibility), Without<UnitPanelRoot>>,
    mut row_texts: Query<(&UnitRowText, &mut Text), (Without<UnitReinforceReason>, Without<UnitPanelOverflowText>)>,
    mut action_buttons: Query<(&UnitActionButton, &mut BackgroundColor), Without<UnitDelegateButton>>,
    mut delegate_buttons: Query<(&UnitDelegateButton, &mut BackgroundColor), Without<UnitActionButton>>,
    mut delegate_labels: Query<(&UnitDelegateLabel, &mut Text), (Without<UnitRowText>, Without<UnitReinforceReason>, Without<UnitPanelOverflowText>)>,
    mut reasons: Query<(&UnitReinforceReason, &mut Text), Without<UnitRowText>>,
    mut overflow: Query<&mut Text, (With<UnitPanelOverflowText>, Without<UnitRowText>, Without<UnitReinforceReason>)>,
) {
    let Ok(mut visibility) = root.single_mut() else { return };
    let showing = player.0.is_some() && !selected.0.is_empty();
    *visibility = if showing { Visibility::Visible } else { Visibility::Hidden };
    if !showing {
        *slots = UnitPanelSlots::default();
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let world = sim.0.world();
    let ids: Vec<u32> = selected.0.iter().copied().collect();

    let mut new_slots = [None; MAX_UNIT_ROWS];
    for slot in 0..MAX_UNIT_ROWS {
        new_slots[slot] = ids.get(slot).copied();
    }
    slots.0 = new_slots;

    for slot in 0..MAX_UNIT_ROWS {
        let unit = slots.0[slot].and_then(|id| world.units.get(id as usize)).filter(|u| u.alive);
        for (container, mut vis) in &mut row_containers {
            if container.0 == slot {
                *vis = if unit.is_some() { Visibility::Visible } else { Visibility::Hidden };
            }
        }
        // Military delegation's own indicator (docs/design.md §14): "does
        // the AI currently order this unit" is read straight off `SimRes`
        // (`SimDriver::is_delegated`), never guessed at from `World` alone -
        // delegation is `HumanAgent` state, not something `World` records.
        let delegated = unit.is_some_and(|u| sim.0.is_delegated(u.id));
        let (row_line, reinforce_reason) = match unit {
            Some(u) => (
                format!(
                    "#{} {}{}{}\n兵力{:.1} 装備{:.1} 組織{:.0} 士気{:.2} 補給{:.2}{} 経験{:.1}{}",
                    u.id.0,
                    u.name,
                    // Stage 11C (docs/phase11-spec.md §4 "地図とパネルで兵科
                    // が分かる"): a land unit's own `Branch` - `None` for
                    // every Sea/Air unit (`Unit::branch`'s own doc), so this
                    // is silently empty for them rather than printing a
                    // meaningless "[]".
                    u.branch.map(|b| format!(" [{}]", b.label())).unwrap_or_default(),
                    if delegated { " [AI操作中]" } else { "" },
                    u.manpower,
                    u.equipment,
                    u.organization,
                    u.morale,
                    u.supply,
                    supply_attrition_marker(u.supply),
                    u.experience,
                    // This task's own ask ("see their squadrons: where they
                    // are based... whether they are in transit") is scoped
                    // to `Domain::Air` alone, not generalized to every
                    // domain's row - `MAX_UNIT_ROWS` rows this tall is a
                    // real, reproducible collision with the event log panel
                    // below (`setup::spawn_left_column`'s own doc: no
                    // `Overflow::scroll_y()`, sized on the assumption every
                    // row stays two lines). Confirmed by screenshot: 6
                    // selected land units at this third line's full height
                    // pushed "...ほか N 隊"/the move hint down into the event
                    // log's own title text; the same 6 rows at two lines
                    // each (a land-only selection, this line absent) left
                    // clear space above it. A land/sea unit's own station
                    // was never shown here before this task either, so
                    // omitting it for those two domains costs nothing a
                    // player relied on - only a squadron's redeploy target
                    // (an airfield in some *other* region, never obviously
                    // "where it is" from the map glance a land unit's own
                    // marker already gives at its own region) actually
                    // needs a text answer to "where is it based".
                    if u.station.domain() == Domain::Air {
                        format!("\n{}", station_label(world, u.station, u.movement.is_some()))
                    } else {
                        String::new()
                    },
                ),
                reinforce_disabled_reason(world, player_faction, u.station),
            ),
            None => (String::new(), None),
        };
        for (row, mut text) in &mut row_texts {
            if row.0 == slot {
                text.0 = row_line.clone();
            }
        }
        for (r, mut text) in &mut reasons {
            if r.0 == slot {
                text.0 = reinforce_reason.unwrap_or("").to_string();
            }
        }
        for (button, mut bg) in &mut action_buttons {
            if button.slot != slot {
                continue;
            }
            let enabled = unit.is_some()
                && match button.kind {
                    UnitActionKind::Hold => true,
                    UnitActionKind::Reinforce | UnitActionKind::Disband => reinforce_reason.is_none(),
                };
            bg.0 = if enabled { COLOR_ENABLED } else { COLOR_DISABLED };
        }
        for (button, mut bg) in &mut delegate_buttons {
            if button.0 != slot {
                continue;
            }
            bg.0 = match (unit.is_some(), delegated) {
                (false, _) => COLOR_DISABLED,
                (true, true) => COLOR_ACTIVE,
                (true, false) => COLOR_ENABLED,
            };
        }
        for (label, mut text) in &mut delegate_labels {
            if label.0 != slot {
                continue;
            }
            text.0 = if delegated { "操作を戻す [U]".to_string() } else { "AI委任 [U]".to_string() };
        }
    }

    if let Ok(mut text) = overflow.single_mut() {
        text.0 = if ids.len() > MAX_UNIT_ROWS { format!("...ほか {} 隊", ids.len() - MAX_UNIT_ROWS) } else { String::new() };
    }
}

/// Usability fix (play-test finding #2 - "nothing tells the player when a
/// number is in trouble"): `military::tick_organization_and_morale` already
/// bleeds manpower/organization off any unit whose own `supply` sits below
/// `ATTRITION_SUPPLY_THRESHOLD`, every tick, whether or not it's fighting -
/// this just names that same threshold back to the player next to the
/// number it explains, rather than leaving them to notice their army
/// quietly shrinking with no visible cause.
fn supply_attrition_marker(supply: f32) -> &'static str {
    if supply < ATTRITION_SUPPLY_THRESHOLD {
        "※損耗中"
    } else {
        ""
    }
}

/// "Where they are based, and whether they are in transit" (this task's own
/// ask for squadron visibility) - `sync_unit_panel`'s only caller passes a
/// `Domain::Air` unit's own `Station::Airfield`, which this reads back as
/// the region the airfield sits in, exactly the way a player already thinks
/// about a squadron's base (`docs/phase10-spec.md "1. 基地"`), not as the
/// underlying `TransportNodeId` - the node id has no meaning to a player,
/// only to `Action::MoveUnit`'s own `to` field. Written generically over
/// every `Station` variant regardless (a land/sea unit's own location was
/// never shown here before this task, and stays that way - seeing this
/// task's own doc at the call site for why the extra line is `Domain::Air`-
/// only), rather than restricted to `Station::Airfield` alone, so a future
/// second caller for another domain costs nothing here.
fn station_label(world: &SimWorld, station: Station, in_transit: bool) -> String {
    let base = match station {
        Station::Region(r) => format!("{}: {}", domain_label(station.domain()), world.region(r).name),
        Station::Sea(z) => format!("{}: {}", domain_label(station.domain()), world.sea_zone(z).name),
        Station::Airfield(node) => format!("{}: {}", domain_label(station.domain()), world.region(world.transport_node(node).region).name),
    };
    if in_transit {
        format!("{base}（移動中）")
    } else {
        base
    }
}

fn domain_label(domain: Domain) -> &'static str {
    match domain {
        Domain::Land => "陸軍",
        Domain::Sea => "艦隊",
        Domain::Air => "飛行隊",
    }
}

fn reinforce_disabled_reason(world: &SimWorld, faction: FactionId, station: Station) -> Option<&'static str> {
    let pinned = match station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
        Station::Airfield(node) => world.has_enemy_units(world.transport_node(node).region, faction),
    };
    if pinned {
        Some(action_error_ja(ActionError::RegionContested))
    } else {
        None
    }
}

pub(super) fn handle_unit_action_clicks(mut sim: ResMut<SimRes>, slots: Res<UnitPanelSlots>, query: Query<(&Interaction, &UnitActionButton), Changed<Interaction>>) {
    for (interaction, button) in &query {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(unit_id) = slots.0[button.slot] else { continue };
        let action = match button.kind {
            UnitActionKind::Hold => Action::HoldUnit { unit: UnitId(unit_id) },
            UnitActionKind::Reinforce => Action::ReinforceUnit { unit: UnitId(unit_id) },
            UnitActionKind::Disband => Action::DisbandUnit { unit: UnitId(unit_id) },
        };
        sim.0.push_human_action(action);
    }
}

/// Military delegation's click handler (docs/design.md §14) - the mouse
/// path to the same toggle `input::keyboard_input`'s `U` binding reaches,
/// one unit at a time instead of the whole current selection. Reads
/// `SimDriver::is_delegated` itself (not the button's current background
/// color) to decide which way to toggle, so a click always reflects this
/// frame's real state even if `sync_unit_panel` hasn't repainted the button
/// yet.
pub(super) fn handle_unit_delegate_clicks(mut sim: ResMut<SimRes>, slots: Res<UnitPanelSlots>, query: Query<(&Interaction, &UnitDelegateButton), Changed<Interaction>>) {
    for (interaction, button) in &query {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Some(unit_id) = slots.0[button.0] else { continue };
        let unit = UnitId(unit_id);
        if sim.0.is_delegated(unit) {
            sim.0.undelegate_unit(unit);
        } else {
            sim.0.delegate_unit(unit);
        }
    }
}

// ---------------------------------------------------------------------
// Policy panel
// ---------------------------------------------------------------------

#[derive(Component)]
pub(super) struct PolicyPanelRoot;

#[derive(Component)]
pub(super) struct PolicyToggleButton;

#[derive(Component, Clone, Copy)]
pub(super) struct GoodTabButton(Good);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum PolicyField {
    Conscription,
    Ration,
    Industry,
    Logistics,
    Import,
}

const POLICY_FIELDS: [PolicyField; 5] =
    [PolicyField::Conscription, PolicyField::Ration, PolicyField::Industry, PolicyField::Logistics, PolicyField::Import];

const CONSCRIPTION_STEP: f32 = 0.05;
const CIVILIAN_RATION_STEP: f32 = 0.05;
const PRIORITY_STEP: f32 = 0.1;
const IMPORT_PLAN_STEP: f32 = 5.0;

impl PolicyField {
    fn label(self) -> &'static str {
        match self {
            PolicyField::Conscription => "徴兵率 [-/=]",
            PolicyField::Ration => "配給率 [\u{5b}/\u{5d}]",
            PolicyField::Industry => "生産優先度 [;/']",
            PolicyField::Logistics => "物流優先度 [,/.]",
            PolicyField::Import => "輸入計画 [8/9]",
        }
    }

    fn value_text(self, faction: &archipelago_sim::world::Faction, good: Good) -> String {
        match self {
            PolicyField::Conscription => format!("{:.2}", faction.conscription),
            PolicyField::Ration => format!("{:.2}", faction.civilian_ration),
            PolicyField::Industry => format!("{:.2} ({})", faction.industry_priority[good.index()], good.label()),
            PolicyField::Logistics => format!("{:.2} ({})", faction.logistics_priority[good.index()], good.label()),
            PolicyField::Import => format!("{:.1} ({})", faction.import_plan[good.index()], good.label()),
        }
    }

    /// `None` unless the field's step buttons are ever disabled for a
    /// reason other than "already at the clamp" (every field here already
    /// clamps client-side before sending, matching `input.rs`'s own keyboard
    /// bindings - so this only fires for `Import` against a non-importable
    /// good, per `action::apply_set_import_plan`'s own rule).
    fn disabled_reason(self, good: Good) -> Option<&'static str> {
        if self == PolicyField::Import && good != Good::Food && good != Good::Energy {
            Some("食料・エネルギーのみ輸入可能")
        } else {
            None
        }
    }

    fn action(self, faction: &archipelago_sim::world::Faction, good: Good, increase: bool) -> Action {
        match self {
            PolicyField::Conscription => {
                let step = if increase { CONSCRIPTION_STEP } else { -CONSCRIPTION_STEP };
                Action::SetConscription((faction.conscription + step).clamp(0.0, 1.0))
            }
            PolicyField::Ration => {
                let step = if increase { CIVILIAN_RATION_STEP } else { -CIVILIAN_RATION_STEP };
                Action::SetCivilianRation(
                    (faction.civilian_ration + step)
                        .clamp(archipelago_sim::balance::CIVILIAN_RATION_MIN, archipelago_sim::balance::CIVILIAN_RATION_MAX),
                )
            }
            PolicyField::Industry => {
                let step = if increase { PRIORITY_STEP } else { -PRIORITY_STEP };
                Action::SetIndustryPriority { good, weight: (faction.industry_priority[good.index()] + step).clamp(0.0, 1.0) }
            }
            PolicyField::Logistics => {
                let step = if increase { PRIORITY_STEP } else { -PRIORITY_STEP };
                Action::SetLogisticsPriority { good, weight: (faction.logistics_priority[good.index()] + step).clamp(0.0, 1.0) }
            }
            PolicyField::Import => {
                let step = if increase { IMPORT_PLAN_STEP } else { -IMPORT_PLAN_STEP };
                Action::SetImportPlan { good, rate: (faction.import_plan[good.index()] + step).max(0.0) }
            }
        }
    }
}

#[derive(Component, Clone, Copy)]
pub(super) struct PolicyStepButton {
    field: PolicyField,
    increase: bool,
}

#[derive(Component, Clone, Copy)]
pub(super) struct PolicyValueText(PolicyField);

#[derive(Component)]
pub(super) struct PolicyFocusReasonText;

#[derive(Component, Clone, Copy)]
pub(super) struct FocusButton(NationalFocus);

/// A child of `setup::spawn_right_column`'s shared flex column - see
/// `spawn_region_action_panel`'s own doc for why `position_type`/`top`/
/// `right` are gone and `display: Display::None` was added alongside the
/// pre-existing `Visibility::Hidden`.
pub(super) fn spawn_policy_panel(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>) {
    parent
        .spawn((
            chrome::framed(Node {
                display: Display::None,
                width: Val::Px(340.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            }),
            chrome::panel_background(),
            chrome::panel_border(),
            Visibility::Hidden,
            PolicyPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- 政策 [P で閉じる] --", font));

            panel.spawn((Text::new("対象品目 [G で切替]:"), text_font(11.0, font), TextColor(Color::srgb(0.8, 0.82, 0.85))));
            // Stage 11A grew `ALL_GOODS` from six entries to eight
            // (`good::Good`'s own doc: `Infantry`/`Armour`/`Artillery`
            // replace `Arms`) - `row_node()`'s plain, non-wrapping row no
            // longer fits every tab on one line inside this panel's fixed
            // 340px width (`codex review` P2: the tail entries would
            // overflow the panel and become unclickable). `FlexWrap::Wrap`
            // lets the row spill onto a second line instead of clipping.
            panel
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: Val::Px(4.0),
                    row_gap: Val::Px(3.0),
                    ..default()
                })
                .with_children(|row| {
                    for good in ALL_GOODS {
                        row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), GoodTabButton(good))).with_children(|b| {
                            b.spawn((Text::new(good.label()), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                        });
                    }
                });

            for field in POLICY_FIELDS {
                panel.spawn(row_node()).with_children(|row| {
                    row.spawn((Text::new(field.label()), text_font(12.0, font), TextColor(TEXT_ENABLED), PolicyValueText(field)));
                    row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), PolicyStepButton { field, increase: false }))
                        .with_children(|b| {
                            b.spawn((Text::new("-"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
                        });
                    row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), PolicyStepButton { field, increase: true }))
                        .with_children(|b| {
                            b.spawn((Text::new("+"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
                        });
                });
            }

            panel.spawn((Text::new("国家方針 [F で循環]:"), text_font(11.0, font), TextColor(Color::srgb(0.8, 0.82, 0.85))));
            panel.spawn(Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), ..default() }).with_children(|col| {
                for focus in ALL_FOCI {
                    col.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), FocusButton(focus))).with_children(|b| {
                        b.spawn((Text::new(focus.label()), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                    });
                }
            });
            panel.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), PolicyFocusReasonText));

            panel.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(TEXT_REASON), PolicyRejectionText));
        });
}

#[derive(Component)]
pub(super) struct PolicyRejectionText;

#[allow(clippy::too_many_arguments)]
pub(super) fn sync_policy_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    policy: Res<PolicyPanel>,
    diplomacy: Res<DiplomacyPanel>,
    active_good: Res<ActiveGood>,
    rejection: Res<LastRejection>,
    mut root: Query<(&mut Visibility, &mut Node), With<PolicyPanelRoot>>,
    mut good_tabs: Query<(&GoodTabButton, &mut BackgroundColor)>,
    mut value_texts: Query<(&PolicyValueText, &mut Text, &mut TextColor), (Without<PolicyRejectionText>, Without<PolicyFocusReasonText>)>,
    mut step_buttons: Query<(&PolicyStepButton, &mut BackgroundColor), Without<GoodTabButton>>,
    mut focus_buttons: Query<(&FocusButton, &mut BackgroundColor), (Without<GoodTabButton>, Without<PolicyStepButton>)>,
    mut focus_reason: Query<&mut Text, (With<PolicyFocusReasonText>, Without<PolicyValueText>, Without<PolicyRejectionText>)>,
    mut rejection_text: Query<&mut Text, (With<PolicyRejectionText>, Without<PolicyValueText>, Without<PolicyFocusReasonText>)>,
) {
    let Ok((mut visibility, mut node)) = root.single_mut() else { return };
    let showing = player.0.is_some() && policy.0 && !diplomacy.open;
    chrome::set_panel_shown(&mut visibility, &mut node, showing);
    if !showing {
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let world = sim.0.world();
    let faction = world.faction(player_faction);
    let good = active_good.0;

    for (tab, mut bg) in &mut good_tabs {
        bg.0 = if tab.0 == good { COLOR_ACTIVE } else { COLOR_ENABLED };
    }

    for field in POLICY_FIELDS {
        let reason = field.disabled_reason(good);
        for (v, mut text, mut color) in &mut value_texts {
            if v.0 == field {
                text.0 = format!("{}: {}", field.label(), field.value_text(faction, good));
                color.0 = if reason.is_none() { TEXT_ENABLED } else { TEXT_DISABLED };
            }
        }
        for (button, mut bg) in &mut step_buttons {
            if button.field == field {
                bg.0 = if reason.is_none() { COLOR_ENABLED } else { COLOR_DISABLED };
            }
        }
    }

    let transitioning = faction.focus_transition_days > 0;
    for (button, mut bg) in &mut focus_buttons {
        let is_current = button.0 == faction.national_focus;
        let disabled = transitioning && !is_current;
        bg.0 = if disabled {
            COLOR_DISABLED
        } else if is_current {
            COLOR_ACTIVE
        } else {
            COLOR_ENABLED
        };
    }
    if let Ok(mut text) = focus_reason.single_mut() {
        text.0 = if transitioning {
            format!("方針転換中（あと{}日、現在: {}）", faction.focus_transition_days, faction.national_focus.label())
        } else {
            String::new()
        };
    }

    if let Ok(mut text) = rejection_text.single_mut() {
        let lines: Vec<&str> = rejection.0.iter().filter(|r| r.target == RejectionTarget::Policy).map(|r| r.reason).collect();
        text.0 = if lines.is_empty() { String::new() } else { format!("!! 却下 !!\n{}", lines.join("\n")) };
    }
}

pub(super) fn handle_policy_button_clicks(
    mut sim: ResMut<SimRes>,
    player: Res<PlayerFaction>,
    mut active_good: ResMut<ActiveGood>,
    good_tabs: Query<(&Interaction, &GoodTabButton), Changed<Interaction>>,
    step_buttons: Query<(&Interaction, &PolicyStepButton), Changed<Interaction>>,
    focus_buttons: Query<(&Interaction, &FocusButton), Changed<Interaction>>,
) {
    for (interaction, tab) in &good_tabs {
        if *interaction == Interaction::Pressed {
            active_good.0 = tab.0;
        }
    }
    let Some(player_faction) = player.0 else { return };
    for (interaction, button) in &step_buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let good = active_good.0;
        if button.field.disabled_reason(good).is_some() {
            continue;
        }
        let faction = sim.0.world().faction(player_faction).clone();
        sim.0.push_human_action(button.field.action(&faction, good, button.increase));
    }
    for (interaction, button) in &focus_buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let faction = sim.0.world().faction(player_faction);
        if faction.focus_transition_days > 0 && button.0 != faction.national_focus {
            continue;
        }
        sim.0.push_human_action(Action::SetNationalFocus(button.0));
    }
}

pub(super) fn handle_policy_toggle(mut policy: ResMut<PolicyPanel>, mut diplomacy: ResMut<DiplomacyPanel>, query: Query<&Interaction, (Changed<Interaction>, With<PolicyToggleButton>)>) {
    for interaction in &query {
        if *interaction == Interaction::Pressed {
            policy.0 = !policy.0;
            if policy.0 {
                diplomacy.open = false;
            }
        }
    }
}

// ---------------------------------------------------------------------
// Diplomacy panel
// ---------------------------------------------------------------------

#[derive(Component)]
pub(super) struct DiplomacyPanelRoot;

#[derive(Component)]
pub(super) struct DiplomacyToggleButton;

#[derive(Component, Clone, Copy)]
pub(super) struct DiplomacyTargetButton(pub FactionId);

#[derive(Component, Clone, Copy)]
pub(super) struct TreatyProposeButton(Treaty);

#[derive(Component, Clone, Copy)]
pub(super) struct TreatyReasonText(Treaty);

/// Every other diplomacy button, collapsed onto one component (instead of
/// one marker `struct` each) purely to keep `sync_diplomacy_panel`/
/// `handle_diplomacy_button_clicks` under Bevy's per-system parameter limit -
/// each used to be its own `Query` parameter; grouped like this they're one
/// each, matched by `match`/`==` instead of by type.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiplomacyAction {
    Accept,
    Reject,
    DeclareWar,
    Break,
    Compose,
}

/// Same idea as `DiplomacyAction`, for the panel's free-standing status/
/// reason lines.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum DiplomacyTextSlot {
    Status,
    AcceptRejectReason,
    WarReason,
    ComposeReason,
    ComposeStatus,
    Rejection,
}

pub(super) fn treaty_label_ja(t: Treaty) -> &'static str {
    match t {
        Treaty::Ceasefire => "停戦",
        Treaty::NonAggression => "不可侵条約",
        Treaty::Alliance => "同盟",
        Treaty::MilitaryAccess => "通行権",
        Treaty::PortAccess => "港湾利用",
        Treaty::TradeAgreement => "貿易協定",
    }
}

/// Japanese label for a `Stance` - the same wording `apps/headless/src/
/// report.rs::stance_label` uses for its own console report, duplicated
/// here rather than shared (that crate is binary-only, no library target -
/// `event_text`'s own module doc gives the identical reason for its own
/// duplicated `treaty_label`). Kept in `apps/game`, not `crates/sim`,
/// alongside `treaty_label_ja` above: `Stance::key()` already covers every
/// machine-facing need (scenario/`--json`), so a Japanese display label is a
/// pure presentation concern with only this client and `apps/headless` as
/// consumers - neither of which is `crates/sim` itself.
pub(super) fn stance_label_ja(stance: Stance) -> &'static str {
    match stance {
        Stance::War => "交戦",
        Stance::Ceasefire => "停戦",
        Stance::NonAggression => "不可侵",
        Stance::Alliance => "同盟",
    }
}

/// Spawns the diplomacy panel, including one `DiplomacyTargetButton` per
/// faction other than `player` that exists at scenario-load time - the
/// faction roster is fixed for the life of a run (only `Faction::alive`
/// changes), so this needs no pool/respawn (`mod.rs`'s own doc pattern).
/// A child of `setup::spawn_right_column`'s shared flex column - see
/// `spawn_region_action_panel`'s own doc for why `position_type`/`top`/
/// `right` are gone and `display: Display::None` was added alongside the
/// pre-existing `Visibility::Hidden`.
pub(super) fn spawn_diplomacy_panel(parent: &mut ChildSpawnerCommands<'_>, font: &Handle<Font>, world: &SimWorld, player: FactionId) {
    parent
        .spawn((
            chrome::framed(Node {
                display: Display::None,
                width: Val::Px(340.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            }),
            chrome::panel_background(),
            chrome::panel_border(),
            Visibility::Hidden,
            DiplomacyPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- 外交 [D で閉じる] --", font));

            panel.spawn((Text::new("対象:"), text_font(11.0, font), TextColor(Color::srgb(0.8, 0.82, 0.85))));
            panel.spawn(row_node()).with_children(|row| {
                for other in &world.factions {
                    if other.id == player {
                        continue;
                    }
                    row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), DiplomacyTargetButton(other.id))).with_children(|b| {
                        b.spawn((Text::new(other.name.clone()), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                    });
                }
            });

            panel.spawn((Text::new(String::new()), text_font(12.0, font), TextColor(TEXT_ENABLED), DiplomacyTextSlot::Status));

            for treaty in ALL_TREATIES {
                panel.spawn(row_node()).with_children(|row| {
                    row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), TreatyProposeButton(treaty))).with_children(|b| {
                        b.spawn((Text::new(format!("{}を提案", treaty_label_ja(treaty))), text_font(11.0, font), TextColor(TEXT_ENABLED)));
                    });
                    row.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), TreatyReasonText(treaty)));
                });
            }

            panel.spawn(row_node()).with_children(|row| {
                row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), DiplomacyAction::Accept)).with_children(|b| {
                    b.spawn((Text::new("受諾 [A]"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
                });
                row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), DiplomacyAction::Reject)).with_children(|b| {
                    b.spawn((Text::new("拒否 [R]"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
                });
            });
            panel.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), DiplomacyTextSlot::AcceptRejectReason));

            panel.spawn(row_node()).with_children(|row| {
                row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), DiplomacyAction::DeclareWar)).with_children(|b| {
                    b.spawn((Text::new("宣戦 [W]"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
                });
                row.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), DiplomacyAction::Break)).with_children(|b| {
                    b.spawn((Text::new("破棄 [B]"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
                });
            });
            panel.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), DiplomacyTextSlot::WarReason));

            panel.spawn((Button, button_node(), BackgroundColor(COLOR_ENABLED), DiplomacyAction::Compose)).with_children(|b| {
                b.spawn((Text::new("自然言語で提案 [T]"), text_font(12.0, font), TextColor(TEXT_ENABLED)));
            });
            panel.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(TEXT_REASON), DiplomacyTextSlot::ComposeReason));
            panel.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(Color::srgb(0.85, 0.85, 0.6)), DiplomacyTextSlot::ComposeStatus));

            panel.spawn((Text::new(String::new()), text_font(11.0, font), TextColor(TEXT_REASON), DiplomacyTextSlot::Rejection));
        });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn sync_diplomacy_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    diplomacy: Res<DiplomacyPanel>,
    nl_compose: Res<NlCompose>,
    rejection: Res<LastRejection>,
    mut root: Query<(&mut Visibility, &mut Node), With<DiplomacyPanelRoot>>,
    mut target_buttons: Query<(&DiplomacyTargetButton, &mut BackgroundColor)>,
    mut treaty_buttons: Query<(&TreatyProposeButton, &mut BackgroundColor), Without<DiplomacyTargetButton>>,
    mut treaty_reasons: Query<(&TreatyReasonText, &mut Text)>,
    mut action_buttons: Query<(&DiplomacyAction, &mut BackgroundColor), (Without<TreatyProposeButton>, Without<DiplomacyTargetButton>)>,
    mut text_slots: Query<(&DiplomacyTextSlot, &mut Text), Without<TreatyReasonText>>,
) {
    let Ok((mut visibility, mut node)) = root.single_mut() else { return };
    let showing = player.0.is_some() && diplomacy.open;
    chrome::set_panel_shown(&mut visibility, &mut node, showing);
    if !showing {
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let world = sim.0.world();

    for (button, mut bg) in &mut target_buttons {
        let active = diplomacy.target == Some(button.0);
        bg.0 = if active { COLOR_ACTIVE } else { COLOR_ENABLED };
    }

    let set_text = |slots: &mut Query<(&DiplomacyTextSlot, &mut Text), Without<TreatyReasonText>>, slot: DiplomacyTextSlot, value: String| {
        for (s, mut text) in slots.iter_mut() {
            if *s == slot {
                text.0 = value.clone();
            }
        }
    };

    let Some(target) = diplomacy.target else {
        set_text(&mut text_slots, DiplomacyTextSlot::Status, "対象となる勢力がいない".to_string());
        return;
    };
    let tf = world.faction(target);
    let stance = world.diplomacy.stance(player_faction, target);
    set_text(
        &mut text_slots,
        DiplomacyTextSlot::Status,
        format!("対象: {}   関係: {}   感情: {:.0}", tf.name, stance_label_ja(stance), world.diplomacy.opinion(player_faction, target)),
    );

    for treaty in ALL_TREATIES {
        let reason = treaty_propose_reason(world, player_faction, target, treaty);
        for (button, mut bg) in &mut treaty_buttons {
            if button.0 == treaty {
                bg.0 = if reason.is_none() { COLOR_ENABLED } else { COLOR_DISABLED };
            }
        }
        for (r, mut text) in &mut treaty_reasons {
            if r.0 == treaty {
                text.0 = reason.unwrap_or("").to_string();
            }
        }
    }

    let incoming = world.diplomacy.pending.iter().any(|p| p.from == target && p.to == player_faction);
    let pending_nl = world.diplomacy.pending_nl.iter().find(|p| p.from == player_faction && p.to == target);
    let war_reason_text = match stance {
        Stance::War => Some("既に交戦中"),
        Stance::NonAggression => Some("不可侵条約を破棄してから宣戦できる"),
        Stance::Alliance => Some("同盟を破棄してから宣戦できる"),
        Stance::Ceasefire => None,
    };
    for (action, mut bg) in &mut action_buttons {
        bg.0 = match action {
            DiplomacyAction::Accept | DiplomacyAction::Reject => {
                if incoming {
                    COLOR_ENABLED
                } else {
                    COLOR_DISABLED
                }
            }
            DiplomacyAction::DeclareWar => {
                if war_reason_text.is_none() {
                    COLOR_ENABLED
                } else {
                    COLOR_DISABLED
                }
            }
            DiplomacyAction::Break => COLOR_ENABLED,
            DiplomacyAction::Compose => {
                if pending_nl.is_none() {
                    COLOR_ENABLED
                } else {
                    COLOR_DISABLED
                }
            }
        };
    }

    let accept_reject_reason = if incoming {
        let treaty = world.diplomacy.pending.iter().find(|p| p.from == target && p.to == player_faction).map(|p| p.treaty);
        treaty.map(|t| format!("相手からの提案: {}", treaty_label_ja(t))).unwrap_or_default()
    } else {
        "相手からの提案がない".to_string()
    };
    set_text(&mut text_slots, DiplomacyTextSlot::AcceptRejectReason, accept_reject_reason);
    set_text(&mut text_slots, DiplomacyTextSlot::WarReason, war_reason_text.unwrap_or("").to_string());
    set_text(&mut text_slots, DiplomacyTextSlot::ComposeReason, pending_nl.map(|_| "返答待ちの提案がある".to_string()).unwrap_or_default());
    let compose_status = if nl_compose.active {
        format!("入力中> {}_\n(Enter で送信, Esc でキャンセル)", nl_compose.buffer)
    } else {
        pending_nl.map(|p| format!("送信済み（返答待ち）: 「{}」", p.text)).unwrap_or_default()
    };
    set_text(&mut text_slots, DiplomacyTextSlot::ComposeStatus, compose_status);

    let rejection_lines: Vec<&str> = rejection.0.iter().filter(|r| r.target == RejectionTarget::Diplomacy).map(|r| r.reason).collect();
    let rejection_text = if rejection_lines.is_empty() { String::new() } else { format!("!! 却下 !!\n{}", rejection_lines.join("\n")) };
    set_text(&mut text_slots, DiplomacyTextSlot::Rejection, rejection_text);
}

fn treaty_propose_reason(world: &SimWorld, faction: FactionId, target: FactionId, treaty: Treaty) -> Option<&'static str> {
    if world.diplomacy.has_treaty(faction, target, treaty) {
        return Some("既に締結済み");
    }
    if world.diplomacy.cooldown(faction, target, treaty) > 0 {
        return Some("クールダウン中");
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_diplomacy_button_clicks(
    mut sim: ResMut<SimRes>,
    player: Res<PlayerFaction>,
    mut diplomacy: ResMut<DiplomacyPanel>,
    mut policy: ResMut<PolicyPanel>,
    mut nl_compose: ResMut<NlCompose>,
    target_buttons: Query<(&Interaction, &DiplomacyTargetButton), Changed<Interaction>>,
    toggle: Query<&Interaction, (Changed<Interaction>, With<DiplomacyToggleButton>)>,
    treaty_buttons: Query<(&Interaction, &TreatyProposeButton), Changed<Interaction>>,
    action_buttons: Query<(&Interaction, &DiplomacyAction), Changed<Interaction>>,
) {
    for interaction in &toggle {
        if *interaction == Interaction::Pressed {
            let Some(player_faction) = player.0 else { continue };
            diplomacy.open = !diplomacy.open;
            if diplomacy.open {
                policy.0 = false;
                if diplomacy.target.is_none() {
                    diplomacy.target = sim.0.world().factions.iter().find(|f| f.id != player_faction && f.alive).map(|f| f.id);
                }
            }
        }
    }
    for (interaction, button) in &target_buttons {
        if *interaction == Interaction::Pressed {
            diplomacy.target = Some(button.0);
        }
    }

    let Some(player_faction) = player.0 else { return };
    let Some(target) = diplomacy.target else { return };

    for (interaction, button) in &treaty_buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if treaty_propose_reason(sim.0.world(), player_faction, target, button.0).is_some() {
            continue;
        }
        sim.0.push_human_action(Action::ProposeTreaty { to: target, treaty: button.0 });
    }
    for (interaction, action) in &action_buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            DiplomacyAction::Accept => {
                let pending = sim.0.world().diplomacy.pending.iter().find(|p| p.from == target && p.to == player_faction).map(|p| p.treaty);
                if let Some(treaty) = pending {
                    sim.0.push_human_action(Action::AcceptTreaty { from: target, treaty });
                }
            }
            DiplomacyAction::Reject => {
                let pending = sim.0.world().diplomacy.pending.iter().find(|p| p.from == target && p.to == player_faction).map(|p| p.treaty);
                if let Some(treaty) = pending {
                    sim.0.push_human_action(Action::RejectTreaty { from: target, treaty });
                }
            }
            DiplomacyAction::DeclareWar => {
                if sim.0.world().diplomacy.stance(player_faction, target) == Stance::Ceasefire {
                    sim.0.push_human_action(Action::DeclareWar { to: target });
                }
            }
            DiplomacyAction::Break => {
                let treaty = current_breakable_treaty(sim.0.world(), player_faction, target);
                sim.0.push_human_action(Action::BreakTreaty { with: target, treaty });
            }
            DiplomacyAction::Compose => {
                let already_pending = sim.0.world().diplomacy.pending_nl.iter().any(|p| p.from == player_faction && p.to == target);
                if !already_pending {
                    nl_compose.active = true;
                    nl_compose.buffer.clear();
                    nl_compose.just_activated = true;
                }
            }
        }
    }
}

/// Mirrors `input::current_breakable_treaty` exactly (same rationale: `B`
/// stands for "undo whatever's currently active with them" without the
/// player needing to remember which of five treaty kinds is in force).
fn current_breakable_treaty(world: &SimWorld, a: FactionId, b: FactionId) -> Treaty {
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

#[cfg(test)]
mod tests {
    use super::*;

    use archipelago_sim::balance::UNIT_ORG;
    use archipelago_sim::ids::FactionId;
    use archipelago_sim::military;
    use archipelago_sim::scenario;

    use crate::sim_driver::SimDriver;

    /// A `SimRes` with faction 0 as the live human player, on the unmodified
    /// MVP scenario - every test below drives one panel's own click handler
    /// against this, then (where the effect can only be observed after a
    /// tick) calls `SimDriver::tick` once and reads `last_human_actions`/
    /// `last_human_action_errors` back, exactly like `sim_control::
    /// advance_simulation` does in the real client.
    fn player_sim() -> SimRes {
        SimRes(SimDriver::new_with_player(scenario::build_world(), 1, Some(FactionId(0)), None))
    }

    /// A fresh full-strength air unit based at `airfield` - `crates/sim`'s
    /// own `push_full_strength_air_unit`, one crate over, needed by the
    /// strike-panel tests below now that `apply_strike_node` refuses a
    /// strike from a faction with no air unit in reach
    /// (`ActionError::NoAircraftInRange`, `codex review` P1):
    /// `scenario::build_world` never places any air unit at start (Stage
    /// 10A - units are synthesized land-only), so a test that needs one has
    /// to add it itself.
    fn push_air_unit(world: &mut SimWorld, faction: FactionId, airfield: TransportNodeId) -> UnitId {
        let id = UnitId(world.units.len() as u32);
        world.units.push(military::Unit {
            id,
            owner: faction,
            name: "Test Squadron".to_string(),
            station: Station::Airfield(airfield),
            movement: None,
            manpower: UNIT_MANPOWER,
            equipment: UNIT_EQUIPMENT,
            organization: UNIT_ORG,
            morale: 1.0,
            supply: 1.0,
            arms_delivery: 1.0,
            arms_budget: 0.0,
            arms_delivery_station: Station::Airfield(airfield),
            experience: 0.0,
            alive: true,
            branch: None,
        });
        id
    }

    /// The enemy-owned region nearest `from` (by straight-line
    /// `world::Region::position` distance) - used by the strike-panel tests
    /// below to pick a genuinely in-`AIR_OPERATING_RADIUS_KM`-range hostile
    /// target under `mvp.json`'s real-kilometre-scale layout
    /// (`tools/rescale_positions.py`, docs/phase10-spec.md gap report),
    /// rather than assuming any particular named region (e.g. a capital)
    /// still qualifies. Never duplicates `air::geographic_distance` itself
    /// (that stays `pub(crate)` to `archipelago-sim`) - just enough of the
    /// same straight-line formula to rank candidates in a test.
    fn nearest_enemy_region(world: &SimWorld, own: FactionId, from: [f32; 2]) -> archipelago_sim::ids::RegionId {
        fn distance2(a: [f32; 2], b: [f32; 2]) -> f32 {
            let dx = a[0] - b[0];
            let dy = a[1] - b[1];
            dx * dx + dy * dy
        }
        world
            .regions
            .iter()
            .filter(|r| r.owner != own)
            .min_by(|a, b| distance2(a.position, from).total_cmp(&distance2(b.position, from)))
            .map(|r| r.id)
            .expect("mvp has at least one region not owned by faction 0")
    }

    fn run<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
        let mut system = IntoSystem::into_system(system);
        system.initialize(world);
        system.run((), world).unwrap();
    }

    /// Regression guard for the speed buttons (`spawn_speed_buttons`/`sync_
    /// speed_buttons`): clicking `1x`/`5x`/`20x` must set `last_active` and
    /// unpause, exactly like the `1`/`2`/`3` keyboard bindings do. Checked
    /// this fails when broken: temporarily swapped the `match` arm to leave
    /// `paused` untouched (`Speed::Paused => speed.paused = true, other =>
    /// speed.last_active = other,` with the `paused = false` line deleted) -
    /// the second assertion below then fails (`paused` stays `true` from the
    /// setup), confirming the test actually exercises that line.
    #[test]
    fn speed_button_click_sets_active_speed_and_unpauses() {
        let mut world = World::new();
        world.insert_resource(SpeedRes { last_active: Speed::X1, paused: true });
        world.spawn((Interaction::Pressed, SpeedButton(Speed::X20)));

        run(&mut world, handle_speed_button_clicks);

        let speed = world.resource::<SpeedRes>();
        assert_eq!(speed.last_active, Speed::X20, "clicking the 20x button must select it");
        assert!(!speed.paused, "clicking any active-speed button must unpause");
    }

    /// The `Paused` button must set `paused` without touching whichever
    /// speed was last active (`Space`'s own semantics - see `SpeedRes`'s own
    /// doc: "releasing pause always resumes at whichever speed was last
    /// chosen").
    #[test]
    fn speed_button_pause_click_sets_paused_without_changing_last_active() {
        let mut world = World::new();
        world.insert_resource(SpeedRes { last_active: Speed::X5, paused: false });
        world.spawn((Interaction::Pressed, SpeedButton(Speed::Paused)));

        run(&mut world, handle_speed_button_clicks);

        let speed = world.resource::<SpeedRes>();
        assert!(speed.paused, "clicking the pause button must pause");
        assert_eq!(speed.last_active, Speed::X5, "pausing must not change the remembered active speed");
    }

    /// Regression guard for the map-mode button's click handler
    /// (`handle_map_mode_button_clicks`) - the clickable, discoverable half
    /// of `map_mode`'s cycling mechanism (`input::keyboard_input`'s `M`
    /// binding is the other, tested in `input`'s own test module). Checked
    /// this fails when broken: temporarily changed the handler to compare
    /// `Interaction::Hovered` instead of `Pressed` - this test then fails
    /// (the mode never advances from a `Pressed` interaction).
    #[test]
    fn map_mode_button_click_advances_to_the_next_mode() {
        use super::super::map_mode::MapMode;

        let mut world = World::new();
        world.insert_resource(MapModeRes(MapMode::Political));
        world.spawn((Interaction::Pressed, MapModeButton));

        run(&mut world, handle_map_mode_button_clicks);

        assert_eq!(world.resource::<MapModeRes>().0, MapMode::Terrain, "a click must advance from Political to Terrain");
    }

    /// `sync_map_mode_button` must actually update the button's own visible
    /// label text to name the current mode - otherwise the button exists but
    /// doesn't satisfy "the current mode must be named on screen".
    #[test]
    fn map_mode_button_label_names_the_active_mode() {
        use super::super::map_mode::MapMode;

        let mut world = World::new();
        world.insert_resource(MapModeRes(MapMode::Population));
        world
            .spawn((MapModeButton,))
            .with_children(|b| {
                b.spawn(Text::new(String::new()));
            });

        run(&mut world, sync_map_mode_button);

        let mut q = world.query_filtered::<&Text, Without<MapModeButton>>();
        let label = q.iter(&world).next().expect("sync_map_mode_button must update the button's child Text").0.clone();
        assert!(label.contains("人口"), "the button label must name the active mode, got: {label}");
        assert!(label.contains('M'), "the button label must keep the M hotkey hint visible, got: {label}");
    }

    /// Regression guard for the region panel's recruit/build/cancel buttons
    /// (`handle_region_action_clicks`): a click on an *enabled* button (the
    /// player's own, uncontested, not-currently-building capital) must
    /// enqueue exactly the `Action` `RegionActionKind::to_action` builds,
    /// and that action must actually be accepted by `Simulation::apply` -
    /// checked by ticking once and reading both `last_human_actions` (what
    /// was queued) and `last_human_action_errors` (empty means it wasn't
    /// rejected). Checked this fails when broken: temporarily made
    /// `handle_region_action_clicks` always push `Action::CancelBuild` (the
    /// wrong action) regardless of `kind` - the first assertion below then
    /// fails, restored after.
    #[test]
    fn region_action_click_enqueues_recruit_for_the_players_own_region() {
        let mut world = World::new();
        let sim = player_sim();
        let capital = sim.0.world().faction(FactionId(0)).capital;
        let units_before = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0)).count();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(capital)));
        world.insert_resource(ActiveGood::default());
        world.insert_resource(ActiveBranch::default());
        world.spawn((Interaction::Pressed, RegionActionKind::RecruitLand));

        run(&mut world, handle_region_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::RecruitUnit { region: capital, domain: Domain::Land, branch: Branch::Infantry }], "the click must have queued exactly one RecruitUnit(Land) for the player's capital");
        assert!(sim.0.last_human_action_errors().is_empty(), "a legal recruit order must not be rejected: {:?}", sim.0.last_human_action_errors());
        let units_after = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0)).count();
        assert_eq!(units_after, units_before + 1, "the recruited unit must actually exist in the world after the tick");
    }

    /// Stage 11C (docs/phase11-spec.md §4 "地図とパネルで兵科が分かる"): a
    /// player must be able to *choose* which land branch a click on
    /// "RecruitLand" actually raises - `ActiveBranch` (cycled with `C`,
    /// `input::keyboard_input`'s own doc), read by both
    /// `RegionActionKind::to_action` (what gets queued) and `::reason`
    /// (whether the button is enabled at all - `Branch::Armour`'s own
    /// `Good::Armour` stock here, not `Good::Infantry`). Mirrors
    /// `region_action_click_enqueues_recruit_for_the_players_own_region`
    /// above exactly, with `ActiveBranch(Branch::Armour)` in place of the
    /// `Infantry` default.
    ///
    /// Checked this fails when broken: temporarily hardcoded
    /// `RegionActionKind::to_action`'s `RecruitLand` arm back to
    /// `branch: Branch::Infantry` (Stage 11B's own behaviour, ignoring
    /// `active_branch`) - the first assertion below then failed (queued
    /// `branch: Infantry` instead of `Armour`). Reverted before committing.
    #[test]
    fn region_action_click_enqueues_the_players_chosen_branch() {
        let mut world = World::new();
        // Built directly (not through `player_sim()`) so `Good::Armour`'s
        // stock can be topped up *before* `SimDriver` owns the world -
        // `sim_driver::SimDriver` exposes no mutable world accessor, on
        // purpose (`push_human_action` is the only way a caller is meant to
        // change a live `SimDriver`'s state). This proves the button reads
        // the *chosen* branch's commodity, not whichever one happens to
        // also be stocked (`scenario::build_world` starts every faction
        // with both).
        let mut sim_world = scenario::build_world();
        sim_world.faction_mut(FactionId(0)).stock[Good::Armour.index()] = 100.0;
        let capital = sim_world.faction(FactionId(0)).capital;
        let sim = SimRes(SimDriver::new_with_player(sim_world, 1, Some(FactionId(0)), None));
        let units_before = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0)).count();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(capital)));
        world.insert_resource(ActiveGood::default());
        world.insert_resource(ActiveBranch(Branch::Armour));
        world.spawn((Interaction::Pressed, RegionActionKind::RecruitLand));

        run(&mut world, handle_region_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(
            sim.0.last_human_actions(),
            &[Action::RecruitUnit { region: capital, domain: Domain::Land, branch: Branch::Armour }],
            "the click must have queued a RecruitUnit(Land) for whichever branch ActiveBranch currently selects"
        );
        assert!(sim.0.last_human_action_errors().is_empty(), "a legal Armour recruit order must not be rejected: {:?}", sim.0.last_human_action_errors());
        let units_after = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0)).count();
        assert_eq!(units_after, units_before + 1, "the recruited Armour unit must actually exist in the world after the tick");
        let recruited_branch = sim.0.world().units.iter().find(|u| u.owner == FactionId(0) && u.station == Station::Region(capital) && u.branch == Some(Branch::Armour));
        assert!(recruited_branch.is_some(), "the newly-recruited unit must actually carry Branch::Armour, not just the action that raised it");
    }

    /// The other half of "disabled, with a reason": a click on a region the
    /// player does *not* own must be silently declined by the handler
    /// itself (`RegionActionKind::reason` returns `Some(..)`), never queued
    /// at all - the button being visually disabled is cosmetic; this is
    /// what actually stops the order. Checked this fails when broken:
    /// temporarily deleted the `if kind.reason(...).is_some() { continue }`
    /// guard in `handle_region_action_clicks` - the assertion below then
    /// fails (an action *was* queued for a region the player doesn't own).
    #[test]
    fn region_action_click_on_a_foreign_region_is_not_enqueued() {
        let mut world = World::new();
        let sim = player_sim();
        let foreign = sim.0.world().faction(FactionId(1)).capital;
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(foreign)));
        world.insert_resource(ActiveGood::default());
        world.insert_resource(ActiveBranch::default());
        world.spawn((Interaction::Pressed, RegionActionKind::RecruitLand));

        run(&mut world, handle_region_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert!(sim.0.last_human_actions().is_empty(), "a click on a region the player doesn't own must never be queued, but got {:?}", sim.0.last_human_actions());
    }

    /// Defect fix: `recruit_reason`'s `InsufficientEquipment` case used to
    /// return `action_error_ja(ActionError::InsufficientEquipment)` - one
    /// fixed string ("装備が不足している") regardless of which of the three
    /// land branches (or Sea/Air) was actually short. With infantry
    /// equipment fully stocked and armour equipment exhausted, tabbing to
    /// 機甲 and reading the button's own disabled reason never named armour
    /// - the button's `[機甲]` label (`RegionActionKind::label`) only
    /// partly mitigated it. This checks the reason now names the specific
    /// commodity (`Good::Armour.label()`, "機甲装備") that is actually
    /// short, and that the still-fully-stocked infantry branch stays
    /// enabled - proving this is a branch-specific check, not a blanket
    /// "something is low" one.
    ///
    /// Checked this fails when broken: temporarily reverted `recruit_
    /// reason`'s `InsufficientEquipment` arm to `action_error_ja(ActionError::
    /// InsufficientEquipment).to_string()` (the pre-fix behaviour) - the
    /// `reason.contains(Good::Armour.label())` assertion below then failed
    /// (`"装備が不足している"` does not contain `"機甲装備"`). Reverted
    /// before committing.
    #[test]
    fn insufficient_equipment_reason_names_the_short_branch() {
        let mut sim_world = scenario::build_world();
        sim_world.faction_mut(FactionId(0)).stock[Good::Armour.index()] = 0.0;
        assert!(
            sim_world.faction(FactionId(0)).stock[Good::Infantry.index()] >= UNIT_EQUIPMENT,
            "this test needs infantry equipment in stock so the shortage below is armour-specific, not blanket"
        );
        let capital = sim_world.faction(FactionId(0)).capital;
        let sim = SimRes(SimDriver::new_with_player(sim_world, 1, Some(FactionId(0)), None));

        let reason = RegionActionKind::RecruitLand.reason(sim.0.world(), FactionId(0), capital, Branch::Armour);
        let reason = reason.expect("armour equipment is exhausted, so RecruitLand must read as disabled while 機甲 is the active branch");
        assert!(
            reason.contains(Good::Armour.label()),
            "the reason must name the specific branch/commodity that is actually short (機甲装備), not a generic \
             'insufficient equipment' message: {reason}"
        );

        let infantry_reason = RegionActionKind::RecruitLand.reason(sim.0.world(), FactionId(0), capital, Branch::Infantry);
        assert!(
            infantry_reason.is_none(),
            "infantry equipment is still fully stocked, so recruiting infantry must stay enabled: {infantry_reason:?}"
        );
    }

    /// Stage 10 follow-up (this task's own ask: a human player must be able
    /// to raise a squadron, not just watch the AI fly one): a click on the
    /// region panel's new `RecruitAir` button, at the player's own capital
    /// (every mvp region has an operational airfield node - `docs/design.md`
    /// scenario data), must enqueue `RecruitUnit { domain: Domain::Air, branch: Branch::Infantry }` and
    /// that action must actually create a living squadron once applied -
    /// mirrors `region_action_click_enqueues_recruit_for_the_players_own_region`
    /// exactly, for the one domain that test doesn't cover.
    ///
    /// Confirmed this can actually fail: temporarily left `RecruitAir` out of
    /// `RegionActionKind::to_action`'s match (falling through to a
    /// compile error is the honest failure mode for an exhaustive match, but
    /// to get a *runtime* red instead, swapped its arm to build
    /// `Action::RecruitUnit { region, domain: Domain::Land, branch: Branch::Infantry }`) - the first
    /// assertion below then failed (got a `Domain::Land` recruit instead of
    /// `Domain::Air`). Reverted before committing.
    #[test]
    fn region_action_click_enqueues_recruit_air_for_the_players_own_region() {
        let mut world = World::new();
        let sim = player_sim();
        let capital = sim.0.world().faction(FactionId(0)).capital;
        let air_units_before = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0) && u.station.domain() == Domain::Air).count();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(capital)));
        world.insert_resource(ActiveGood::default());
        world.insert_resource(ActiveBranch::default());
        world.spawn((Interaction::Pressed, RegionActionKind::RecruitAir));

        run(&mut world, handle_region_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(
            sim.0.last_human_actions(),
            &[Action::RecruitUnit { region: capital, domain: Domain::Air, branch: Branch::Infantry }],
            "the click must have queued exactly one RecruitUnit(Air) for the player's capital"
        );
        assert!(sim.0.last_human_action_errors().is_empty(), "a legal air recruit order must not be rejected: {:?}", sim.0.last_human_action_errors());
        let air_units_after = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0) && u.alive && u.station.domain() == Domain::Air).count();
        assert_eq!(air_units_after, air_units_before + 1, "the recruited squadron must actually exist in the world after the tick");
    }

    /// The strike panel's own click handler (`handle_strike_clicks`): a
    /// click on `StrikeKind::Airfield` while an enemy region (mvp starts
    /// every faction at war with every other - `Diplomacy::new`'s own
    /// default) is selected must enqueue `Action::StrikeNode` naming that
    /// region's own airfield node, and the strike must actually land
    /// (`Simulation::apply` accepts it, and the node's `condition` drops).
    ///
    /// Confirmed this can actually fail: temporarily hardcoded
    /// `handle_strike_clicks` to always resolve `StrikeKind::Port.node(...)`
    /// regardless of which `kind` was actually clicked - the assertion below
    /// then failed (`TransportNodeId(13)` expected vs `TransportNodeId(26)`
    /// actually queued, mvp's kanto airfield/port nodes). Reverted before
    /// committing.
    ///
    /// **Needs a reachable squadron** (`codex review` P1,
    /// `ActionError::NoAircraftInRange`): `player_sim`'s default world has
    /// no air units at all, so this test builds its own `sim_world` and
    /// gives faction 0 one at its own capital's airfield first, then
    /// targets the nearest enemy-owned region (`nearest_enemy_region`)
    /// rather than assuming faction 1's own capital is close enough - since
    /// `mvp.json` was rescaled onto a real kilometre plane
    /// (`tools/rescale_positions.py`, docs/phase10-spec.md gap report),
    /// mvp's 関東/近畿 capitals are now over 400km apart, well outside
    /// `air::AIR_OPERATING_RADIUS_KM`'s 300km, even though a genuinely
    /// nearby enemy region (信越・北陸, ~200km) still exists.
    #[test]
    fn strike_panel_click_enqueues_strike_node_against_a_hostile_airfield() {
        let mut world = World::new();
        let mut sim_world = scenario::build_world();
        let own_capital = sim_world.faction(FactionId(0)).capital;
        let own_airfield = sim_world.airfield_node(own_capital).expect("every mvp region has an airfield node").id;
        push_air_unit(&mut sim_world, FactionId(0), own_airfield);
        let foreign = nearest_enemy_region(&sim_world, FactionId(0), sim_world.region(own_capital).position);
        let node = sim_world.airfield_node(foreign).expect("every mvp region has an airfield node").id;
        let condition_before = sim_world.transport_node(node).condition.get();
        let sim = SimRes(SimDriver::new_with_player(sim_world, 1, Some(FactionId(0)), None));
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(foreign)));
        world.spawn((Interaction::Pressed, StrikeKind::Airfield));

        run(&mut world, handle_strike_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::StrikeNode { node }], "the click must have queued exactly one StrikeNode against the enemy capital's airfield");
        assert!(sim.0.last_human_action_errors().is_empty(), "a legal strike against a hostile airfield must not be rejected: {:?}", sim.0.last_human_action_errors());
        let condition_after = sim.0.world().transport_node(node).condition.get();
        assert!(condition_after < condition_before, "the struck airfield's own condition must actually drop: before {condition_before}, after {condition_after}");
    }

    /// A region with two airfields, its first wrecked, must still be
    /// strikeable at the second (`codex review`, P2).
    ///
    /// `StrikeKind::node` used to return the region's lowest-id node of the
    /// kind, so once that one was rubble every further click hit the same
    /// rubble and the region's other, intact field could never be targeted
    /// at all. This is the fourth appearance in Phase 10 of "does the region
    /// have one" and "give me one to use" sharing a lowest-id lookup.
    ///
    /// **Confirmed this test can fail.** Reverting `StrikeKind::node` to
    /// `World::airfield_node` makes the click queue a `StrikeNode` against
    /// the wrecked first node instead of the intact second one, tripping the
    /// assertion below. Restored, and it passes.
    #[test]
    fn strike_panel_targets_an_operational_node_when_the_first_is_wrecked() {
        let mut world = World::new();

        // Built up on the plain `SimWorld` first: `SimDriver` deliberately
        // exposes the world read-only, so the second airfield and the first
        // one's ruin have to exist before the driver is constructed.
        let mut sim_world = scenario::build_world();
        let own_capital = sim_world.faction(FactionId(0)).capital;
        let own_airfield = sim_world.airfield_node(own_capital).expect("every mvp region has an airfield node").id;
        push_air_unit(&mut sim_world, FactionId(0), own_airfield);
        let foreign = nearest_enemy_region(&sim_world, FactionId(0), sim_world.region(own_capital).position);
        let wrecked = sim_world.airfield_node(foreign).expect("every mvp region has an airfield node").id;
        let intact = archipelago_sim::ids::TransportNodeId(sim_world.transport_nodes.len() as u32);
        sim_world.transport_nodes.push(archipelago_sim::transport::TransportNode {
            id: intact,
            name: "spare airfield".to_string(),
            kind: archipelago_sim::transport::TransportNodeKind::Airfield,
            region: foreign,
            condition: archipelago_sim::transport::Condition::FULL,
        });
        sim_world.transport_nodes[wrecked.index()].condition =
            archipelago_sim::transport::Condition::new(0.0).expect("0.0 is a valid condition");
        let sim = SimRes(SimDriver::new_with_player(sim_world, 1, Some(FactionId(0)), None));

        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(foreign)));
        world.spawn((Interaction::Pressed, StrikeKind::Airfield));

        run(&mut world, handle_strike_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(
            sim.0.last_human_actions(),
            &[Action::StrikeNode { node: intact }],
            "the click must target the region's still-operational airfield, not the wrecked one a lowest-id lookup returns"
        );
    }

    /// The other half of "disabled, with a reason", for strike: a click on
    /// the player's *own* region (never hostile to itself) must be declined
    /// by the handler, never queued - `StrikeKind::reason`'s own
    /// `NodeNotHostile` check is what stops it, not the button's visual
    /// state alone.
    ///
    /// Confirmed this can actually fail: temporarily deleted the
    /// `if kind.reason(...).is_some() { continue }` guard in
    /// `handle_strike_clicks` - the assertion below then failed (a
    /// `StrikeNode` action *was* queued against the player's own capital).
    /// Reverted before committing.
    #[test]
    fn strike_panel_click_on_the_players_own_region_is_not_enqueued() {
        let mut world = World::new();
        let sim = player_sim();
        let capital = sim.0.world().faction(FactionId(0)).capital;
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedRegion(Some(capital)));
        world.spawn((Interaction::Pressed, StrikeKind::Airfield));

        run(&mut world, handle_strike_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert!(sim.0.last_human_actions().is_empty(), "a strike click on the player's own region must never be queued, but got {:?}", sim.0.last_human_actions());
    }

    /// The hole this panel closes (`codex review` P1): before `apply_strike_
    /// node` gated itself on `air::units_reaching`, a faction with zero
    /// aircraft anywhere could still strike any hostile airfield/port for
    /// free, and this panel's button offered exactly that with no
    /// indication anything was wrong. `player_sim`'s default world already
    /// has no air units at all (Stage 10A never places any at scenario
    /// start), so - unlike the two tests above, which had to add one - this
    /// is the *unmodified* default state, proving the button now reflects
    /// `ActionError::NoAircraftInRange` on its own without any extra setup.
    ///
    /// Confirmed this fails without the fix: temporarily removed the
    /// `air::units_reaching(...).is_empty()` check from `StrikeKind::
    /// reason`. Re-ran: `reason` came back `None` (the button reads as
    /// enabled) and the click actually queued a `StrikeNode` action, both
    /// assertions below tripping. Restored, and it passes.
    #[test]
    fn strike_panel_button_is_disabled_with_reason_when_no_aircraft_can_reach() {
        let sim = player_sim();
        let player_faction = FactionId(0);
        let foreign = sim.0.world().faction(FactionId(1)).capital;
        assert!(
            sim.0.world().units.iter().all(|u| u.owner != player_faction || u.station.domain() != Domain::Air),
            "sanity: player_sim's default world has no air units for the player at all"
        );

        assert_eq!(
            StrikeKind::Airfield.reason(sim.0.world(), player_faction, foreign),
            Some(action_error_ja(ActionError::NoAircraftInRange)),
            "with no reachable aircraft, the button must be disabled with exactly this reason"
        );

        let mut world = World::new();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(player_faction)));
        world.insert_resource(SelectedRegion(Some(foreign)));
        world.spawn((Interaction::Pressed, StrikeKind::Airfield));

        run(&mut world, handle_strike_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert!(
            sim.0.last_human_actions().is_empty(),
            "a strike click with no reachable aircraft must never be queued, but got {:?}",
            sim.0.last_human_actions()
        );
    }

    /// The interdict panel's own "disabled, with a reason" hole
    /// (`ActionError::NoForceInRange`, the shape this task closed one action
    /// later than `StrikeNode`'s own `NoAircraftInRange`): `player_sim`'s
    /// default `mvp.json` world starts faction 0 with land units only at its
    /// own capital (関東, region 3) and its owned neighbors - none of which
    /// border 九州 (region 9) or its only neighbor (中国, region 7), so
    /// mvp's transport line 9 (entirely inside 九州, faction 2's own
    /// territory) is genuinely out of reach for every domain: no land unit
    /// anywhere near it, and `player_sim` places no naval or air units at
    /// all (Stage 2D/10A - both are synthesized only when a test adds them).
    ///
    /// Confirmed this fails without the fix: temporarily removed the
    /// `action::any_force_reaches` check from `interdict_reason`. Re-ran:
    /// `interdict_reason` came back `None` (the button reads as enabled) and
    /// the click actually queued an `InterdictLine` action, both assertions
    /// below tripping. Restored, and it passes.
    #[test]
    fn interdict_panel_button_is_disabled_with_reason_when_no_force_can_reach() {
        let sim = player_sim();
        let player_faction = FactionId(0);
        let far_region = archipelago_sim::ids::RegionId(9); // 九州, faction 2's own
        let line = TransportLineId(9); // entirely inside 九州 - see this test's own doc
        assert_eq!(
            sim.0.world().transport_node(sim.0.world().transport_line(line).from).region,
            far_region,
            "test setup: mvp transport line 9 must sit inside region 9"
        );
        assert!(
            sim.0.world().units.iter().all(|u| u.owner != player_faction || u.station.domain() != Domain::Land
                || !matches!(u.station, Station::Region(r) if r == far_region || r == archipelago_sim::ids::RegionId(7))),
            "sanity: this test relies on player_sim placing no player-owned unit in or adjacent to region 9"
        );

        assert_eq!(
            interdict_reason(sim.0.world(), player_faction, line),
            Some(action_error_ja(ActionError::NoForceInRange)),
            "with no force of any domain able to reach either endpoint, the button must be disabled with exactly this reason"
        );

        let mut world = World::new();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(player_faction)));
        let mut slots = InterdictPanelSlots::default();
        slots.0[0] = Some(line.0);
        world.insert_resource(slots);
        world.spawn((Interaction::Pressed, InterdictButton(0)));

        run(&mut world, handle_interdict_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert!(
            sim.0.last_human_actions().is_empty(),
            "an interdict click with no reachable force must never be queued, but got {:?}",
            sim.0.last_human_actions()
        );
    }

    /// The positive control for the test above: a line whose own endpoint
    /// region directly borders the player's territory (mvp region 4, 信越・
    /// 北陸, adjacent to faction 0's own capital region 3 - `action::
    /// any_force_reaches`'s land leg) must read enabled and actually enqueue
    /// `Action::InterdictLine`, using nothing but `player_sim`'s unmodified
    /// starting land units - proving the panel's "reachable" path works, not
    /// only its "unreachable" one.
    #[test]
    fn interdict_panel_click_enqueues_interdict_line_against_a_reachable_hostile_line() {
        let sim = player_sim();
        let player_faction = FactionId(0);
        let line = TransportLineId(4); // entirely inside 信越・北陸, faction 1's own, bordering faction 0's capital
        let region = sim.0.world().transport_node(sim.0.world().transport_line(line).from).region;
        assert_eq!(sim.0.world().region(region).owner, FactionId(1), "test setup: line 4 must be faction 1's own");
        assert_eq!(interdict_reason(sim.0.world(), player_faction, line), None, "line 4 must read as a legal, reachable target with player_sim's unmodified starting units");
        let condition_before = sim.0.world().transport_line(line).condition.get();

        let mut world = World::new();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(player_faction)));
        let mut slots = InterdictPanelSlots::default();
        slots.0[0] = Some(line.0);
        world.insert_resource(slots);
        world.spawn((Interaction::Pressed, InterdictButton(0)));

        run(&mut world, handle_interdict_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::InterdictLine { line }], "the click must have queued exactly one InterdictLine against the reachable hostile line");
        assert!(sim.0.last_human_action_errors().is_empty(), "a legal, reachable InterdictLine must not be rejected: {:?}", sim.0.last_human_action_errors());
        let condition_after = sim.0.world().transport_line(line).condition.get();
        assert!(condition_after < condition_before, "the interdicted line's own condition must actually drop: before {condition_before}, after {condition_after}");
    }

    /// Regression guard for the unit panel's per-slot Hold/Reinforce buttons
    /// (`handle_unit_action_clicks`): the click must resolve `slot` through
    /// `UnitPanelSlots` to the *correct* unit id and the *correct* action
    /// kind - both are easy to get backwards (wrong slot index, or Hold/
    /// Reinforce swapped) without any type error. Checked this fails when
    /// broken: temporarily hardcoded `slots.0[0]` instead of `slots.0
    /// [button.slot]` - with the button spawned at `slot: 1` below, the
    /// assertion then fails (`unit` resolves to whatever's in slot 0, or
    /// `None`), restored after.
    #[test]
    fn unit_action_click_resolves_the_correct_slot_and_kind() {
        let mut world = World::new();
        let sim = player_sim();
        let unit = sim.0.world().units.iter().find(|u| u.owner == FactionId(0) && u.alive).expect("faction 0 starts with a living unit").id;
        world.insert_resource(sim);
        let mut slots = UnitPanelSlots::default();
        slots.0[1] = Some(unit.0);
        world.insert_resource(slots);
        world.spawn((Interaction::Pressed, UnitActionButton { slot: 1, kind: UnitActionKind::Hold }));

        run(&mut world, handle_unit_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::HoldUnit { unit }], "the click must queue HoldUnit for exactly the unit in slot 1");
        assert!(sim.0.last_human_action_errors().is_empty());
    }

    /// The disband-defect fix's button: a click on `UnitActionKind::Disband`
    /// must queue `Action::DisbandUnit` for the unit in that slot, and the
    /// unit must actually be gone (and the faction's force smaller) once
    /// that action is applied - not just that the right JSON-shaped enum
    /// variant was pushed. Checked this fails when broken: temporarily
    /// mapped `UnitActionKind::Disband` to `Action::HoldUnit` in
    /// `handle_unit_action_clicks` - the first assertion below then fails.
    #[test]
    fn unit_action_click_disband_removes_the_unit() {
        let mut world = World::new();
        let sim = player_sim();
        let before_count = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0) && u.alive).count();
        let unit = sim.0.world().units.iter().find(|u| u.owner == FactionId(0) && u.alive).expect("faction 0 starts with a living unit").id;
        world.insert_resource(sim);
        let mut slots = UnitPanelSlots::default();
        slots.0[1] = Some(unit.0);
        world.insert_resource(slots);
        world.spawn((Interaction::Pressed, UnitActionButton { slot: 1, kind: UnitActionKind::Disband }));

        run(&mut world, handle_unit_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::DisbandUnit { unit }], "the click must queue DisbandUnit for exactly the unit in slot 1");
        assert!(sim.0.last_human_action_errors().is_empty());
        assert!(!sim.0.world().unit(unit).alive, "the disbanded unit must be gone after the tick that applies it");
        let after_count = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0) && u.alive).count();
        assert_eq!(after_count, before_count - 1, "the faction's living force must shrink by exactly one");
    }

    /// Regression guard for the policy panel's conscription `+` button
    /// (`handle_policy_button_clicks`): must enqueue `SetConscription` at
    /// exactly the current value plus one step, clamped - the same formula
    /// `input::keyboard_input`'s `=` key uses. Checked this fails when
    /// broken: temporarily used `button.field.action(&faction, good, false)`
    /// (always "decrease", ignoring `button.increase`) - the assertion below
    /// then fails (queues a *decrease* instead), restored after.
    #[test]
    fn policy_step_button_click_enqueues_conscription_increase() {
        let mut world = World::new();
        let sim = player_sim();
        let current = sim.0.world().faction(FactionId(0)).conscription;
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(ActiveGood::default());
        world.insert_resource(ActiveBranch::default());
        world.spawn((Interaction::Pressed, PolicyStepButton { field: PolicyField::Conscription, increase: true }));
        // The other two click sources `handle_policy_button_clicks` reads -
        // spawned empty so the system's other `Query`s simply match nothing.

        run(&mut world, handle_policy_button_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(
            sim.0.last_human_actions(),
            &[Action::SetConscription((current + CONSCRIPTION_STEP).clamp(0.0, 1.0))],
            "the click must queue SetConscription at exactly one step above the pre-click value"
        );
        assert!(sim.0.last_human_action_errors().is_empty());
    }

    /// Regression guard for the diplomacy panel's target tabs
    /// (`handle_diplomacy_button_clicks`): clicking another faction's tab
    /// must set `DiplomacyPanel::target` to *that* faction, not merely to
    /// "some faction" - easy to get wrong if the click loop reads the wrong
    /// field off the query. No tick needed; this is a pure resource write.
    #[test]
    fn diplomacy_target_button_click_sets_the_clicked_faction() {
        let mut world = World::new();
        world.insert_resource(player_sim());
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(DiplomacyPanel { open: true, target: Some(FactionId(1)) });
        world.insert_resource(PolicyPanel::default());
        world.insert_resource(NlCompose::default());
        world.spawn((Interaction::Pressed, DiplomacyTargetButton(FactionId(2))));

        run(&mut world, handle_diplomacy_button_clicks);

        assert_eq!(world.resource::<DiplomacyPanel>().target, Some(FactionId(2)), "clicking faction 2's tab must retarget the panel to faction 2, not leave/guess another faction");
    }

    /// Regression guard for the diplomacy panel's treaty-propose buttons:
    /// clicking one must enqueue `ProposeTreaty` for exactly the clicked
    /// `Treaty` and the currently-selected target. Checked this fails when
    /// broken: temporarily hardcoded `Treaty::Alliance` in the `Propose
    /// Treaty` action regardless of `button.0` - with `NonAggression`
    /// clicked below, the assertion then fails, restored after.
    #[test]
    fn diplomacy_treaty_button_click_enqueues_propose_for_the_clicked_treaty() {
        let mut world = World::new();
        world.insert_resource(player_sim());
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(DiplomacyPanel { open: true, target: Some(FactionId(1)) });
        world.insert_resource(PolicyPanel::default());
        world.insert_resource(NlCompose::default());
        world.spawn((Interaction::Pressed, TreatyProposeButton(Treaty::NonAggression)));

        run(&mut world, handle_diplomacy_button_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::ProposeTreaty { to: FactionId(1), treaty: Treaty::NonAggression }]);
        assert!(sim.0.last_human_action_errors().is_empty());
    }

    /// "Disabled, with a reason" for diplomacy: clicking Accept with no
    /// incoming proposal from the current target must not queue anything -
    /// mirrors `region_action_click_on_a_foreign_region_is_not_enqueued`
    /// for the diplomacy panel's own guard.
    #[test]
    fn diplomacy_accept_click_with_no_pending_proposal_is_not_enqueued() {
        let mut world = World::new();
        world.insert_resource(player_sim());
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(DiplomacyPanel { open: true, target: Some(FactionId(1)) });
        world.insert_resource(PolicyPanel::default());
        world.insert_resource(NlCompose::default());
        world.spawn((Interaction::Pressed, DiplomacyAction::Accept));

        run(&mut world, handle_diplomacy_button_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert!(sim.0.last_human_actions().is_empty(), "accepting with nothing pending must not queue AcceptTreaty, but got {:?}", sim.0.last_human_actions());
    }

    /// Play-test finding #2's regression guard for the unit panel: a unit
    /// whose `supply` has dropped below `ATTRITION_SUPPLY_THRESHOLD` - the
    /// exact point `military::tick_organization_and_morale` starts bleeding
    /// its manpower/organization every tick, fighting or not - must show
    /// `supply_attrition_marker`'s text next to its 補給 figure; a
    /// fully-supplied unit must not. Checked this fails when broken:
    /// temporarily changed `supply_attrition_marker`'s comparison to
    /// `supply < 0.0` (never true) - the "below threshold" assertion below
    /// then fails.
    #[test]
    fn unit_panel_marks_supply_below_the_attrition_threshold() {
        let mut world = World::new();
        let mut sim = player_sim();
        let unit_id = sim.0.world().units.iter().find(|u| u.owner == FactionId(0) && u.alive).expect("faction 0 starts with a living unit").id;
        assert!(
            sim.0.world().unit(unit_id).supply >= ATTRITION_SUPPLY_THRESHOLD,
            "test precondition: the unit must start fully supplied, not already in attrition"
        );
        sim.0.sim.world.unit_mut(unit_id).supply = ATTRITION_SUPPLY_THRESHOLD - 0.1;
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedUnits([unit_id.0].into_iter().collect()));
        world.insert_resource(UnitPanelSlots::default());

        world.spawn((Visibility::Hidden, UnitPanelRoot));
        world.spawn((Visibility::Hidden, UnitRowContainer(0)));
        world.spawn((Text::new(String::new()), UnitRowText(0)));
        world.spawn((Text::new(String::new()), UnitReinforceReason(0)));
        world.spawn((Text::new(String::new()), UnitPanelOverflowText));
        for kind in [UnitActionKind::Hold, UnitActionKind::Reinforce, UnitActionKind::Disband] {
            world.spawn((BackgroundColor(COLOR_ENABLED), UnitActionButton { slot: 0, kind }));
        }

        run(&mut world, sync_unit_panel);

        let mut q = world.query::<(&UnitRowText, &Text)>();
        let row_text = q.iter(&world).find(|(r, _)| r.0 == 0).map(|(_, t)| t.0.clone()).expect("slot 0 must have been rendered");
        assert!(row_text.contains("※損耗中"), "supply below ATTRITION_SUPPLY_THRESHOLD must be marked, got: {row_text}");
    }

    /// `codex review` (P2): the legend's "徴募兵科: C ボタン/キー" row claims
    /// a clickable control exists for `ActiveBranch` - `BranchTabButton`
    /// (`handle_branch_tab_clicks`) is that control, `MapModeButton`'s exact
    /// pattern one level up (`map_mode_button_click_advances_to_the_next_mode`
    /// above). A mouse-only player must be able to pick a branch without
    /// ever touching the keyboard.
    ///
    /// Checked this fails when broken: temporarily made
    /// `handle_branch_tab_clicks` a no-op - the assertion below then failed
    /// (`ActiveBranch` stayed at its `Infantry` default after the click).
    /// Reverted before committing.
    #[test]
    fn branch_tab_click_selects_that_branch() {
        let mut world = World::new();
        world.insert_resource(ActiveBranch::default());
        world.spawn((Interaction::Pressed, BranchTabButton(Branch::Armour)));

        run(&mut world, handle_branch_tab_clicks);

        assert_eq!(world.resource::<ActiveBranch>().0, Branch::Armour, "a click on the Armour tab must select Branch::Armour");
    }

    /// `sync_branch_tabs` must actually highlight whichever tab matches the
    /// current `ActiveBranch`, not just leave every tab the same color -
    /// otherwise the control exists but a player has no way to see *which*
    /// branch is currently selected without also reading the recruit
    /// button's own `[branch]` label.
    #[test]
    fn branch_tab_highlights_the_active_branch() {
        let mut world = World::new();
        world.insert_resource(ActiveBranch(Branch::Artillery));
        world.spawn((BackgroundColor(COLOR_ENABLED), BranchTabButton(Branch::Infantry)));
        world.spawn((BackgroundColor(COLOR_ENABLED), BranchTabButton(Branch::Armour)));
        world.spawn((BackgroundColor(COLOR_ENABLED), BranchTabButton(Branch::Artillery)));

        run(&mut world, sync_branch_tabs);

        let mut q = world.query::<(&BranchTabButton, &BackgroundColor)>();
        for (tab, bg) in q.iter(&world) {
            let expected = if tab.0 == Branch::Artillery { COLOR_ACTIVE } else { COLOR_ENABLED };
            assert_eq!(bg.0, expected, "tab {:?} must be {} (COLOR_ACTIVE iff it is the current ActiveBranch)", tab.0, if tab.0 == Branch::Artillery { "highlighted" } else { "unhighlighted" });
        }
    }
}
