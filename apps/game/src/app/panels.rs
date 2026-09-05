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

use archipelago_sim::action::{Action, ActionError};
use archipelago_sim::balance::{ATTRITION_SUPPLY_THRESHOLD, UNIT_EQUIPMENT, UNIT_MANPOWER};
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Stance, Treaty, ALL_TREATIES};
use archipelago_sim::focus::{NationalFocus, ALL_FOCI};
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId, UnitId};
use archipelago_sim::world::{Domain, Station, World as SimWorld};

use super::input::MENU_ITEMS;
use super::map_mode::MapModeRes;
use super::setup::text_font;
use super::{
    ActiveGood, DiplomacyPanel, LastRejection, NlCompose, PlayerFaction, PolicyPanel, RejectionTarget, SelectedRegion, SelectedUnits, SimRes, SpeedRes,
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
// Shared styling
// ---------------------------------------------------------------------

const COLOR_ENABLED: Color = Color::srgb(0.22, 0.30, 0.24);
const COLOR_ACTIVE: Color = Color::srgb(0.55, 0.42, 0.12);
const COLOR_DISABLED: Color = Color::srgba(0.22, 0.22, 0.24, 0.55);
const TEXT_ENABLED: Color = Color::srgb(0.92, 0.95, 0.92);
const TEXT_DISABLED: Color = Color::srgba(0.65, 0.65, 0.68, 0.8);
const TEXT_REASON: Color = Color::srgb(0.85, 0.45, 0.40);
const PANEL_BG: Color = Color::srgba(0.05, 0.06, 0.08, 0.88);

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
    BuildInfra,
    BuildPort,
    BuildCapacity,
    Repair,
    CancelBuild,
}

/// Same order as `input::MENU_ITEMS`/`input::handle_menu_keys`'s digit keys -
/// `index()` below is the shared key between the two.
const REGION_ACTION_KINDS: [RegionActionKind; 7] = [
    RegionActionKind::RecruitLand,
    RegionActionKind::RecruitSea,
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

    fn label(self, active_good: Good) -> String {
        let base = MENU_ITEMS[self.index()];
        match self {
            RegionActionKind::BuildCapacity => format!("{base} [{}]", active_good.label()),
            _ => base.to_string(),
        }
    }

    /// The right-click-menu digit that does the same thing - shown so the
    /// keyboard path stays discoverable (task ask), not secret.
    fn shortcut_hint(self) -> String {
        format!("(右クリック→{})", self.index() + 1)
    }

    fn to_action(self, region: RegionId, active_good: Good) -> Action {
        match self {
            RegionActionKind::RecruitLand => Action::RecruitUnit { region, domain: Domain::Land },
            RegionActionKind::RecruitSea => Action::RecruitUnit { region, domain: Domain::Sea },
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
    fn reason(self, world: &SimWorld, faction: FactionId, region_id: RegionId) -> Option<&'static str> {
        let region = world.regions.get(region_id.index())?;
        if region.owner != faction {
            return Some(action_error_ja(ActionError::RegionNotOwned));
        }
        if world.has_enemy_units(region_id, faction) {
            return Some(action_error_ja(ActionError::RegionContested));
        }
        match self {
            RegionActionKind::RecruitLand => recruit_reason(world, faction),
            RegionActionKind::RecruitSea => {
                if region.port <= 0.0 {
                    return Some(action_error_ja(ActionError::NoPort));
                }
                recruit_reason(world, faction)
            }
            RegionActionKind::BuildInfra | RegionActionKind::BuildPort | RegionActionKind::BuildCapacity | RegionActionKind::Repair => {
                if region.construction.is_some() {
                    Some(action_error_ja(ActionError::AlreadyBuilding))
                } else {
                    None
                }
            }
            RegionActionKind::CancelBuild => {
                if region.construction.is_none() {
                    Some(action_error_ja(ActionError::NoConstruction))
                } else {
                    None
                }
            }
        }
    }
}

fn recruit_reason(world: &SimWorld, faction: FactionId) -> Option<&'static str> {
    let f = world.faction(faction);
    if f.manpower < UNIT_MANPOWER {
        return Some(action_error_ja(ActionError::InsufficientManpower));
    }
    if f.stock[Good::Arms.index()] < UNIT_EQUIPMENT {
        return Some(action_error_ja(ActionError::InsufficientEquipment));
    }
    None
}

pub(super) fn spawn_region_action_panel(commands: &mut Commands, font: &Handle<Font>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(230.0),
                right: Val::Px(10.0),
                width: Val::Px(340.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            Visibility::Hidden,
            RegionActionPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn((Text::new("-- 命令 --"), text_font(13.0, font), TextColor(Color::srgb(0.85, 0.87, 0.90))));
            for kind in REGION_ACTION_KINDS {
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
    mut root: Query<&mut Visibility, With<RegionActionPanelRoot>>,
    mut buttons: Query<(&RegionActionKind, &mut BackgroundColor)>,
    mut labels: Query<(&RegionActionLabel, &mut Text, &mut TextColor)>,
    mut reasons: Query<(&RegionActionReason, &mut Text), Without<RegionActionLabel>>,
) {
    let Ok(mut visibility) = root.single_mut() else { return };
    let showing = player.0.is_some() && selected.0.is_some() && !diplomacy.open && !policy.0;
    *visibility = if showing { Visibility::Visible } else { Visibility::Hidden };
    if !showing {
        return;
    }
    let Some(player_faction) = player.0 else { return };
    let Some(region_id) = selected.0 else { return };
    let world = sim.0.world();

    for kind in REGION_ACTION_KINDS {
        let reason = kind.reason(world, player_faction, region_id);
        for (k, mut bg) in &mut buttons {
            if *k == kind {
                bg.0 = button_bg(reason.is_none(), false);
            }
        }
        for (label, mut text, mut color) in &mut labels {
            if label.0 == kind {
                text.0 = format!("{} {}", kind.label(active_good.0), kind.shortcut_hint());
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

pub(super) fn handle_region_action_clicks(
    mut sim: ResMut<SimRes>,
    player: Res<PlayerFaction>,
    selected: Res<SelectedRegion>,
    active_good: Res<ActiveGood>,
    query: Query<(&Interaction, &RegionActionKind), Changed<Interaction>>,
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
        sim.0.push_human_action(kind.to_action(region_id, active_good.0));
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

pub(super) fn spawn_unit_panel(commands: &mut Commands, font: &Handle<Font>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(300.0),
                left: Val::Px(10.0),
                width: Val::Px(300.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            Visibility::Hidden,
            UnitPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn((Text::new("-- 選択部隊 --"), text_font(13.0, font), TextColor(Color::srgb(0.85, 0.87, 0.90))));
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
                    "#{} {}{}\n兵力{:.1} 装備{:.1} 組織{:.0} 士気{:.2} 補給{:.2}{} 経験{:.1}",
                    u.id.0,
                    u.name,
                    if delegated { " [AI操作中]" } else { "" },
                    u.manpower,
                    u.equipment,
                    u.organization,
                    u.morale,
                    u.supply,
                    supply_attrition_marker(u.supply),
                    u.experience
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

fn reinforce_disabled_reason(world: &SimWorld, faction: FactionId, station: Station) -> Option<&'static str> {
    let pinned = match station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
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

pub(super) fn spawn_policy_panel(commands: &mut Commands, font: &Handle<Font>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(46.0),
                right: Val::Px(10.0),
                width: Val::Px(340.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            Visibility::Hidden,
            PolicyPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn((Text::new("-- 政策 [P で閉じる] --"), text_font(14.0, font), TextColor(Color::srgb(1.0, 0.82, 0.45))));

            panel.spawn((Text::new("対象品目 [G で切替]:"), text_font(11.0, font), TextColor(Color::srgb(0.8, 0.82, 0.85))));
            panel.spawn(row_node()).with_children(|row| {
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
    mut root: Query<&mut Visibility, With<PolicyPanelRoot>>,
    mut good_tabs: Query<(&GoodTabButton, &mut BackgroundColor)>,
    mut value_texts: Query<(&PolicyValueText, &mut Text, &mut TextColor), (Without<PolicyRejectionText>, Without<PolicyFocusReasonText>)>,
    mut step_buttons: Query<(&PolicyStepButton, &mut BackgroundColor), Without<GoodTabButton>>,
    mut focus_buttons: Query<(&FocusButton, &mut BackgroundColor), (Without<GoodTabButton>, Without<PolicyStepButton>)>,
    mut focus_reason: Query<&mut Text, (With<PolicyFocusReasonText>, Without<PolicyValueText>, Without<PolicyRejectionText>)>,
    mut rejection_text: Query<&mut Text, (With<PolicyRejectionText>, Without<PolicyValueText>, Without<PolicyFocusReasonText>)>,
) {
    let Ok(mut visibility) = root.single_mut() else { return };
    let showing = player.0.is_some() && policy.0 && !diplomacy.open;
    *visibility = if showing { Visibility::Visible } else { Visibility::Hidden };
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

/// Spawns the diplomacy panel, including one `DiplomacyTargetButton` per
/// faction other than `player` that exists at scenario-load time - the
/// faction roster is fixed for the life of a run (only `Faction::alive`
/// changes), so this needs no pool/respawn (`mod.rs`'s own doc pattern).
pub(super) fn spawn_diplomacy_panel(commands: &mut Commands, font: &Handle<Font>, world: &SimWorld, player: FactionId) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(46.0),
                right: Val::Px(10.0),
                width: Val::Px(340.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            Visibility::Hidden,
            DiplomacyPanelRoot,
        ))
        .with_children(|panel| {
            panel.spawn((Text::new("-- 外交 [D で閉じる] --"), text_font(14.0, font), TextColor(Color::srgb(1.0, 0.82, 0.45))));

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
    mut root: Query<&mut Visibility, With<DiplomacyPanelRoot>>,
    mut target_buttons: Query<(&DiplomacyTargetButton, &mut BackgroundColor)>,
    mut treaty_buttons: Query<(&TreatyProposeButton, &mut BackgroundColor), Without<DiplomacyTargetButton>>,
    mut treaty_reasons: Query<(&TreatyReasonText, &mut Text)>,
    mut action_buttons: Query<(&DiplomacyAction, &mut BackgroundColor), (Without<TreatyProposeButton>, Without<DiplomacyTargetButton>)>,
    mut text_slots: Query<(&DiplomacyTextSlot, &mut Text), Without<TreatyReasonText>>,
) {
    let Ok(mut visibility) = root.single_mut() else { return };
    let showing = player.0.is_some() && diplomacy.open;
    *visibility = if showing { Visibility::Visible } else { Visibility::Hidden };
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
        format!("対象: {}   関係: {:?}   感情: {:.0}", tf.name, stance, world.diplomacy.opinion(player_faction, target)),
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

    use archipelago_sim::ids::FactionId;
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
        world.spawn((Interaction::Pressed, RegionActionKind::RecruitLand));

        run(&mut world, handle_region_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert_eq!(sim.0.last_human_actions(), &[Action::RecruitUnit { region: capital, domain: Domain::Land }], "the click must have queued exactly one RecruitUnit(Land) for the player's capital");
        assert!(sim.0.last_human_action_errors().is_empty(), "a legal recruit order must not be rejected: {:?}", sim.0.last_human_action_errors());
        let units_after = sim.0.world().units.iter().filter(|u| u.owner == FactionId(0)).count();
        assert_eq!(units_after, units_before + 1, "the recruited unit must actually exist in the world after the tick");
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
        world.spawn((Interaction::Pressed, RegionActionKind::RecruitLand));

        run(&mut world, handle_region_action_clicks);

        let mut sim = world.resource_mut::<SimRes>();
        sim.0.tick();
        assert!(sim.0.last_human_actions().is_empty(), "a click on a region the player doesn't own must never be queued, but got {:?}", sim.0.last_human_actions());
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
}
