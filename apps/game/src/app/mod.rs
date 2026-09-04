//! Everything that actually touches `bevy` lives under this module tree.
//! `crate::sim_driver`/`crate::layout` (used by `sim_control`/`setup`
//! below) stay Bevy-free - see their own module docs for why. `run` is the
//! only entry point `main.rs` calls.

mod camera_fit;
mod event_text;
mod fonts;
mod input;
mod newspaper;
mod overlay;
mod palette;
mod panels;
mod screenshot;
mod setup;
mod sim_control;
mod ui;
mod visuals;

use std::collections::{BTreeSet, VecDeque};

use bevy::prelude::*;

pub use screenshot::ScreenshotConfig;

use archipelago_agents::newspaper::NewspaperArticle;
use archipelago_sim::action::Action;
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use archipelago_sim::world::{LinkKind, World as SimWorld};

use crate::sim_driver::{SimDriver, Speed};

/// Stage 7B (docs/phase7-spec.md "Stage 7B — 遊ぶ"): `--play`/`--record`/
/// `--replay`, resolved by `main.rs` before `run` is ever called (faction
/// name/index lookup and `--replay` file parsing both need the loaded
/// `World`, which `main.rs` already has in hand).
pub struct PlayConfig {
    pub player: FactionId,
    /// `--record <path>`: where to write the player's per-day actions.
    /// `None` when `--record` wasn't given - playing is fully supported
    /// without recording anything.
    pub record: Option<String>,
    /// `--replay <path>`, already parsed - `Some` means `player` is driven
    /// by this recording instead of live input.
    pub replay: Option<Vec<Vec<Action>>>,
}

/// Wraps the Bevy-free `SimDriver` as a Bevy `Resource` - the seam between
/// `crate::sim_driver` and everything else in this module.
#[derive(Resource)]
pub(crate) struct SimRes(pub SimDriver);

/// `last_active` is always `Speed::X1`/`X5`/`X20`, never `Speed::Paused` -
/// `1`/`2`/`3` set it directly (see `input::keyboard_input`); `Space` only
/// ever flips `paused`, so releasing pause always resumes at whichever
/// speed was last chosen rather than forgetting it.
#[derive(Resource)]
pub(crate) struct SpeedRes {
    pub last_active: Speed,
    pub paused: bool,
}

#[derive(Resource, Default)]
pub(crate) struct SelectedRegion(pub Option<RegionId>);

/// The sea zone a plain click selected for inspection, when no unit was
/// selected first (`input::map_click_select`) - exists purely so
/// `overlay::sync_blockade_visuals` can reveal a blockaded port's causal
/// link to a sea zone on demand (Stage 7C follow-up: "quiet the blockade
/// mark; reveal the connector only when the player has selected that
/// region or that sea zone" - see `overlay`'s own module doc). Mutually
/// exclusive with `SelectedRegion`: selecting either clears the other.
#[derive(Resource, Default)]
pub(crate) struct SelectedSeaZone(pub Option<SeaZoneId>);

#[derive(Resource)]
pub(crate) struct SelectedFaction(pub FactionId);

/// Stage 7B (docs/phase7-spec.md "Stage 7B — 遊ぶ"): the human-controlled
/// faction, if `--play` was given - `None` means every faction is AI
/// (Stage 7A's observing-only mode, unchanged). Every player-input system
/// in `input.rs` gates on this before touching `SimRes`.
#[derive(Resource)]
pub(crate) struct PlayerFaction(pub Option<FactionId>);

/// Units the player has clicked to select (map-driven order issuing,
/// docs/phase7-spec.md "操作": "部隊をクリック... 複数選択可"). Stores raw
/// `UnitId.0` (not `UnitId` itself, which has no `Ord`... actually it does,
/// kept as `u32` simply to avoid importing `UnitId` into every call site
/// that only ever compares/iterates these).
#[derive(Resource, Default)]
pub(crate) struct SelectedUnits(pub BTreeSet<u32>);

/// The region a right-click opened the recruit/build menu for - `None` when
/// no menu is open. Opening the menu never touches `SelectedUnits` (a menu
/// and a move-order-in-progress are independent bits of player intent).
#[derive(Resource, Default)]
pub(crate) struct MenuRegion(pub Option<RegionId>);

/// Diplomacy panel state (`D` to toggle, `V` to cycle `target` - see
/// `input::keyboard_input`'s diplomacy branch). docs/phase7-spec.md "条約の
/// 提案・受諾・拒否は外交パネルから".
#[derive(Resource)]
pub(crate) struct DiplomacyPanel {
    pub open: bool,
    pub target: Option<FactionId>,
}

/// Stage 8B's policy panel (`P` to toggle, or the top bar's own button,
/// `panels::PolicyToggleButton`) - conscription/civilian ration/industry
/// priority per good/logistics priority per good/import plan/national focus
/// as clickable controls (owner ask: "操作方法が全然わからないよ" - these
/// used to be keyboard-only). Never shown without a `--play`ed faction, same
/// as `DiplomacyPanel`.
#[derive(Resource, Default)]
pub(crate) struct PolicyPanel(pub bool);

/// Stage 7C (docs/phase7-spec.md "1. 補給網の可視化"): `L` toggles the
/// supply-network overlay - "常時表示だと地図が読みにくい" (always-on would
/// make the map unreadable), so this starts `false` and every overlay-only
/// visual (`overlay::sync_supply_overlay`) gates on it. Sea control,
/// blockade, devastation, and construction markers are *not* gated by
/// this - only the supply-route/chokepoint layer is (docs/phase7-spec.md
/// "Stage 7C" items 1 vs. 2/3, which name no toggle for the others).
#[derive(Resource, Default)]
pub(crate) struct SupplyOverlay(pub bool);

/// Stage 7C's natural-language proposal compose box (docs/phase7-spec.md
/// "4. 外交画面": "テキスト入力欄から送り"). Only ever active while the
/// diplomacy panel is open and a target is selected - see
/// `input::handle_diplomacy_keys`. `buffer` holds exactly what's been typed
/// so far; nothing is sent until `Enter`.
#[derive(Resource, Default)]
pub(crate) struct NlCompose {
    pub active: bool,
    pub buffer: String,
    /// External code review fix A1: set alongside `active = true` by
    /// `input::handle_diplomacy_keys`'s `T` binding, and consumed (cleared,
    /// with no text collected) by `input::nl_compose_text_input` the very
    /// next time that system runs. Both the activation and the text-
    /// collection system read this same frame's `KeyboardInput` events, but
    /// `keyboard_input` (which reads `T` via `ButtonInput<KeyCode>`, not the
    /// raw event stream) runs first in the `Update` chain - so without this
    /// flag, `nl_compose_text_input` would see `active` already `true` and
    /// collect that very same `T` keypress as the compose buffer's first
    /// character, prefixing every proposal with a stray "t"
    /// (`nl_compose_activation_key_is_not_collected_as_text`).
    pub just_activated: bool,
}

/// One newspaper issue: every living faction's article for the same
/// reporting period (`archipelago_agents::newspaper::generate_issue`'s own
/// shape) plus the period it covers, kept so the player can page back
/// through history (docs/phase7-spec.md "5. 新聞": "履歴を遡れること").
pub(crate) struct NewspaperIssue {
    pub period_start: u32,
    pub period_end: u32,
    pub articles: Vec<NewspaperArticle>,
}

/// Stage 7C's newspaper panel (`N` to toggle) - see `newspaper::tick_newspaper`
/// for how `history` is grown and `ui::update_newspaper_panel` for how it's
/// shown. This client wires no LLM backend of its own (`apps/game` never
/// gained an `archipelago-llm` dependency for this stage - out of the hard
/// constraints' scope), so every issue is `archipelago_agents::newspaper`'s
/// already-approved mechanical template fallback
/// (docs/conventions.md §3's approved-exceptions table), exactly like
/// `apps/headless`'s own default (`--agent heuristic`, no `--backend`).
#[derive(Resource, Default)]
pub(crate) struct NewspaperState {
    pub history: Vec<NewspaperIssue>,
    pub period_start: u32,
    pub period_events: Vec<archipelago_sim::event::Event>,
    pub open: bool,
    /// Index into `history` currently shown - `None` means "the latest
    /// issue", so a freshly generated issue is always what's on screen
    /// until the player pages back (`input`'s `ArrowLeft`/`ArrowRight`
    /// while the newspaper panel is open).
    pub viewing: Option<usize>,
}

/// Which commodity the industry-priority/logistics-priority/import-plan
/// policy keys (`input::keyboard_input`) currently act on - cycled with
/// `G`. A single shared pointer rather than one keybinding per `Good`
/// (there are six) keeps the keymap small.
#[derive(Resource)]
pub(crate) struct ActiveGood(pub Good);

impl Default for ActiveGood {
    fn default() -> Self {
        // `Steel` - the shared input every faction's industry-priority
        // tuning actually contends over (`balance.rs`'s Stage 2A section) -
        // is the good a new player is most likely to want to adjust first.
        ActiveGood(ALL_GOODS[2])
    }
}

/// Present only when `--record <path>` was given: the recording accumulated
/// so far, rewritten to `path` after every tick that grows it
/// (`sim_control::advance_simulation`) - see `crate::action_codec`'s own
/// doc for why a full rewrite rather than an append.
#[derive(Resource)]
pub(crate) struct RecordConfig {
    pub path: String,
    pub days: Vec<Vec<Action>>,
}

/// Which panel issued a rejected order - lets each panel show only the
/// rejections it's responsible for (Stage 8B, owner ask: "今出した命令が却
/// 下された理由を、その命令を出したパネルに出す"), computed once per
/// rejection by `sim_control::advance_simulation` from the `Action` variant
/// `SimDriver::last_human_action_errors` paired it with.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RejectionTarget {
    Region(RegionId),
    Unit,
    Policy,
    Diplomacy,
}

/// One rejected order, already translated to Japanese and tagged with which
/// panel issued it.
pub(crate) struct Rejection {
    pub target: RejectionTarget,
    pub reason: &'static str,
}

/// Classifies an `Action` by which panel can issue it - the inverse of each
/// panel's own click handlers in `panels.rs` (a `RegionActionKind` always
/// builds a `RecruitUnit`/`Build`/`CancelBuild` for the region it was
/// clicked in; the diplomacy panel only ever builds the treaty/NL actions;
/// etc.) - kept as one function so the mapping can't drift between the two
/// directions.
pub(crate) fn rejection_target_of(action: &Action) -> RejectionTarget {
    match action {
        Action::MoveUnit { .. } | Action::HoldUnit { .. } | Action::ReinforceUnit { .. } => RejectionTarget::Unit,
        Action::RecruitUnit { region, .. } | Action::Build { region, .. } | Action::CancelBuild { region } => RejectionTarget::Region(*region),
        Action::SetConscription(_)
        | Action::SetIndustryPriority { .. }
        | Action::SetCivilianRation(_)
        | Action::SetImportPlan { .. }
        | Action::SetLogisticsPriority { .. }
        | Action::SetNationalFocus(_) => RejectionTarget::Policy,
        Action::ProposeTreaty { .. }
        | Action::AcceptTreaty { .. }
        | Action::RejectTreaty { .. }
        | Action::DeclareWar { .. }
        | Action::BreakTreaty { .. }
        | Action::ProposeInNaturalLanguage { .. }
        | Action::RespondToNaturalLanguageProposal { .. } => RejectionTarget::Diplomacy,
    }
}

/// The human/replay faction's most recent rejected orders, each tagged with
/// which panel issued it (docs/phase7-spec.md "命令の可否を隠さない",
/// extended per Stage 8B to attach the reason to the issuing panel, not only
/// a single corner) - cleared and refilled every tick by
/// `sim_control::advance_simulation`, so this always reflects the *last* day
/// actions were actually applied, not an accumulating log.
#[derive(Resource, Default)]
pub(crate) struct LastRejection(pub Vec<Rejection>);

/// Most-recent-first ring of formatted event lines - "直近のものから流れる"
/// (docs/phase7-spec.md "UI"): the bottom log panel.
#[derive(Resource, Default)]
pub(crate) struct EventLog(pub VecDeque<String>);

pub(crate) const EVENT_LOG_CAPACITY: usize = 14;

#[derive(Resource)]
pub(crate) struct ScenarioMeta {
    pub name: String,
    pub max_days: u32,
}

/// One `[x, y]` per region, indexed by `RegionId::index()` - computed once
/// at startup (`crate::layout::region_positions`) and never touched again;
/// coordinates are cosmetic and never feed back into `SimRes`.
#[derive(Resource)]
pub(crate) struct RegionLayout(pub Vec<[f32; 2]>);

/// One `[x, y]` per sea zone, indexed by `SeaZoneId::index()` - the centroid
/// of the zone's coastal regions' own positions (sea zones carry no
/// coordinate of their own anywhere in the scenario schema).
#[derive(Resource)]
pub(crate) struct SeaZoneCenters(pub Vec<[f32; 2]>);

#[derive(Component)]
pub(crate) struct RegionMarker(pub RegionId);

#[derive(Component)]
pub(crate) struct SeaZoneMarker(pub SeaZoneId);

#[derive(Component)]
pub(crate) struct UnitMarker(pub UnitId);

/// One segment of a region-to-region link's rendered geometry (a dashed
/// link is several rectangle entities, `setup::spawn_link`'s own doc) -
/// tagged with both endpoints and its `kind` so `overlay::sync_supply_overlay`
/// can find every segment of a given link and recolor them together for
/// the supply-overlay's chokepoint/active-route signal, without needing a
/// second, parallel copy of the link geometry.
#[derive(Component)]
pub(crate) struct LinkVisualMarker {
    pub a: RegionId,
    pub b: RegionId,
    pub kind: LinkKind,
}

/// The supply-overlay ring around one region (Stage 7C, docs/phase7-spec.md
/// "1."), pre-spawned once at startup and only ever recolored/shown-or-hidden
/// per frame - never spawned/despawned, since the region set is fixed for
/// the life of a run.
#[derive(Component)]
pub(crate) struct SupplyRingMarker(pub RegionId);

/// A region's in-progress-construction marker (Stage 7C, docs/phase7-spec.md
/// "3. 戦災と復興": "進行中の工事...を...表示"), pre-spawned once per region
/// and shown only while `Region::construction.is_some()`.
#[derive(Component)]
pub(crate) struct ConstructionMarker(pub RegionId);

/// A saturated-chokepoint marker at one same-owner link's midpoint (Stage
/// 7C follow-up), pre-spawned once per link pair alongside its
/// `LinkVisualMarker` geometry (`setup::setup`) and shown/hidden plus
/// rescaled every frame by `overlay::sync_supply_overlay` - never
/// spawned/despawned at runtime, since the region/link graph itself is
/// fixed for the life of a run (only which links are *currently* saturated
/// changes). `a`/`b` match whatever `(region.id, link.to)` orientation
/// `setup::setup`'s own link-spawning loop used, exactly like
/// `LinkVisualMarker::{a,b}`.
#[derive(Component)]
pub(crate) struct ChokepointMarker {
    pub a: RegionId,
    pub b: RegionId,
}

/// A blockaded-port marker or its line to the sea zone responsible
/// (docs/phase7-spec.md "2.": "封鎖されている港を地図上に明示する。どの
/// 海域の制海権が原因かを結ぶ") - which ports are blockaded, and by which
/// zone, changes at runtime (unlike the link/region graph itself), so
/// these are despawned and respawned fresh every frame
/// (`overlay::sync_blockade_visuals`) rather than pre-spawned and merely
/// hidden.
#[derive(Component)]
pub(crate) struct BlockadeVisual;

#[derive(Component)]
pub(crate) struct MainCamera;

#[derive(Component)]
pub(crate) struct TopBarText;

/// Stage 8B: the player faction's own key figures, shown unconditionally at
/// a glance (docs/design.md §16 owner ask) rather than only via the
/// Tab-switchable faction browser (`FactionPanelText`) - see `ui::
/// update_top_bar_player_stats`.
#[derive(Component)]
pub(crate) struct TopBarPlayerStatsText;

#[derive(Component)]
pub(crate) struct FactionPanelText;

#[derive(Component)]
pub(crate) struct EventLogText;

#[derive(Component)]
pub(crate) struct InspectText;

/// Stage 7B's player-facing panel: selection state, the recruit/build menu,
/// the diplomacy panel, current policy values, and the most recent
/// rejection reasons - see `ui::update_player_panel`.
#[derive(Component)]
pub(crate) struct PlayerPanelText;

/// Stage 7C's newspaper panel (`N` to toggle) - see `ui::update_newspaper_panel`.
#[derive(Component)]
pub(crate) struct NewspaperPanelText;

/// Stage 7C visual-hierarchy follow-up: marks a legend row (`setup::spawn_legend`)
/// that only means something while the supply overlay itself is on - shown/
/// hidden together with `SupplyOverlay` by `overlay::sync_legend_visibility`.
/// The legend's other rows (blockade/construction - both always-on features)
/// never carry this and stay permanently visible.
#[derive(Component)]
pub(crate) struct SupplyOnlyLegendRow;

/// Builds and runs the Bevy `App`. `world` must already be validated
/// (`archipelago_sim::scenario::build_world`/`load_str`/`load_file`) -
/// `main.rs` never constructs one any other way.
///
/// `screenshot`, when given, makes this run's own `Update` schedule (see
/// `screenshot::maybe_capture_screenshot`) capture the primary window to
/// disk after `ScreenshotConfig::after_frames` frames and exit with status
/// 0 - this never reaches into `SimRes`/`SimDriver` on its own; the
/// simulated days that accumulate before the shot are purely a side effect
/// of `sim_control::advance_simulation` already running every unpaused
/// frame at the default `Speed::X1`.
///
/// `cjk_font_override` is `main.rs`'s already-parsed `--cjk-font <path>` -
/// see `fonts::load`/`fonts::resolve` for how it's combined with
/// `ARCHIPELAGO_CJK_FONT` and this platform's own candidate search.
pub fn run(
    world: SimWorld,
    seed: u64,
    scenario_name: String,
    max_days: u32,
    screenshot: Option<ScreenshotConfig>,
    play: Option<PlayConfig>,
    cjk_font_override: Option<String>,
) {
    let positions = crate::layout::region_positions(&world);
    let sea_centers = sea_zone_centers(&world, &positions);
    let region_count = world.regions.len();
    let unit_count = world.units.len();
    let start_day = world.day;

    let player_faction = play.as_ref().map(|p| p.player);
    let record_path = play.as_ref().and_then(|p| p.record.clone());
    let replay_days = play.as_ref().and_then(|p| p.replay.clone());
    let is_replay = replay_days.is_some();

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: format!("Archipelago — {scenario_name}"),
            resolution: bevy::window::WindowResolution::new(1280, 800),
            ..default()
        }),
        ..default()
    }));

    // Loaded directly against `app.world_mut()` (not a `Startup` system) so
    // the `fonts::AppFont` resource it inserts is guaranteed to exist before
    // `setup::setup` - which reads it to build every `TextFont` this crate
    // spawns - runs, with no need to reason about command-flush ordering
    // between two `Startup` systems.
    fonts::load(&mut app, cjk_font_override);

    // Stage 7B (docs/phase7-spec.md "時間の進め方"): "`--play` 指定時は一時
    // 停止で開始する" - a live human player always starts paused so the
    // first day's board can actually be looked at before anything moves.
    // Observing-only (no `--play`) and a `--replay` run (nothing left for
    // the player to decide) both keep Stage 7A's running-at-1x default.
    let start_paused = player_faction.is_some() && !is_replay;

    // Verification-only conveniences (`--debug-*`, `screenshot::ScreenshotConfig`'s
    // own doc): with no keyboard at the wheel before an automated screenshot
    // fires, these let a panel/overlay start already open instead of
    // needing a live `L`/`D`/`N` press. `false` (every default) whenever
    // `--screenshot` wasn't given at all - identical to Stage 7A/7B's
    // startup state.
    let debug_open_diplomacy = screenshot.as_ref().is_some_and(|c| c.open_diplomacy);
    let debug_open_newspaper = screenshot.as_ref().is_some_and(|c| c.open_newspaper);
    let debug_open_policy = screenshot.as_ref().is_some_and(|c| c.open_policy);
    let debug_supply_overlay = screenshot.as_ref().is_some_and(|c| c.supply_overlay);
    // Computed from `world` here, before it moves into `SimDriver::new_with_player`
    // below - same "lowest-id other living faction" default `input::
    // keyboard_input`'s own `D` binding picks.
    let debug_diplomacy_target = if debug_open_diplomacy {
        player_faction.and_then(|p| world.factions.iter().find(|f| f.id != p && f.alive).map(|f| f.id))
    } else {
        None
    };
    let debug_select_region = screenshot.as_ref().and_then(|c| c.select_region);
    // `--debug-select-units` (`ScreenshotConfig::select_units`'s own doc):
    // every living unit the `--play`ed faction owns, computed from `world`
    // here for the same reason `debug_diplomacy_target` is - `world` moves
    // into `SimDriver::new_with_player` right below.
    let debug_selected_units: BTreeSet<u32> = if screenshot.as_ref().is_some_and(|c| c.select_units) {
        player_faction
            .map(|p| world.units.iter().filter(|u| u.alive && u.owner == p).map(|u| u.id.0).collect())
            .unwrap_or_default()
    } else {
        BTreeSet::new()
    };

    app.insert_resource(ClearColor(Color::srgb(0.07, 0.08, 0.10)))
        .insert_resource(SimRes(SimDriver::new_with_player(world, seed, player_faction, replay_days)))
        .insert_resource(SpeedRes { last_active: Speed::X1, paused: start_paused })
        .insert_resource(SelectedRegion(debug_select_region))
        .insert_resource(SelectedSeaZone::default())
        .insert_resource(SelectedFaction(FactionId(0)))
        .insert_resource(EventLog::default())
        .insert_resource(ScenarioMeta { name: scenario_name, max_days })
        .insert_resource(RegionLayout(positions))
        .insert_resource(SeaZoneCenters(sea_centers))
        .insert_resource(PlayerFaction(player_faction))
        .insert_resource(SelectedUnits(debug_selected_units))
        .insert_resource(MenuRegion::default())
        .insert_resource(DiplomacyPanel { open: debug_open_diplomacy, target: debug_diplomacy_target })
        .insert_resource(PolicyPanel(debug_open_policy))
        .insert_resource(ActiveGood::default())
        .insert_resource(LastRejection::default())
        .insert_resource(SupplyOverlay(debug_supply_overlay))
        .insert_resource(NlCompose::default())
        .insert_resource(panels::PointerOverUi::default())
        .insert_resource(panels::UnitPanelSlots::default())
        .insert_resource(NewspaperState { period_start: start_day, open: debug_open_newspaper, ..Default::default() })
        .add_systems(Startup, setup::setup)
        .add_systems(
            Update,
            (
                panels::mark_pointer_over_ui,
                camera_fit::fit_camera_to_map,
                screenshot::apply_debug_camera,
                input::keyboard_input,
                input::nl_compose_text_input,
                input::mouse_pan_zoom,
                input::keyboard_pan,
                input::map_click_select,
                input::map_right_click_menu,
                // Every panel button click just enqueues an `Action` through
                // the exact same `SimRes::push_human_action` door the
                // keyboard bindings above use (this module's own doc, "no
                // privileged path") - ordered here, before `advance_
                // simulation`, so a click lands in this frame's tick exactly
                // like a keypress does.
                panels::handle_speed_button_clicks,
                panels::handle_region_action_clicks,
                panels::handle_unit_action_clicks,
                panels::handle_policy_toggle,
                panels::handle_policy_button_clicks,
                panels::handle_diplomacy_button_clicks,
                sim_control::advance_simulation,
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                visuals::sync_region_visuals,
                visuals::sync_sea_zone_visuals,
                visuals::sync_unit_visuals,
                overlay::sync_supply_overlay,
                overlay::sync_construction_markers,
                overlay::sync_blockade_visuals,
                overlay::sync_legend_visibility,
                ui::update_top_bar,
                ui::update_top_bar_player_stats,
                ui::update_faction_panel,
                ui::update_event_log,
                ui::update_inspect_panel,
                ui::update_player_panel,
                ui::update_newspaper_panel,
            )
                .chain()
                .after(sim_control::advance_simulation),
        )
        .add_systems(
            // Split from the block above - Bevy's `.chain()` tuple impl has
            // a fixed maximum arity, and the two together (14 + 5) exceed
            // it. Both blocks read post-tick `SimRes` state read-only and
            // write disjoint UI entities, so the only ordering that
            // matters - after `advance_simulation` - is preserved by
            // chaining this block after the first block's own last system.
            Update,
            (
                panels::sync_speed_buttons,
                panels::sync_region_action_buttons,
                panels::sync_unit_panel,
                panels::sync_policy_panel,
                panels::sync_diplomacy_panel,
            )
                .chain()
                .after(ui::update_newspaper_panel),
        )
        .add_systems(
            Update,
            screenshot::maybe_capture_screenshot.after(panels::sync_diplomacy_panel),
        );

    if let Some(path) = record_path {
        app.insert_resource(RecordConfig { path, days: Vec::new() });
    }

    if let Some(config) = screenshot {
        app.insert_resource(config);
    }

    eprintln!(
        "archipelago-game: starting with {region_count} regions, {} sea zones, {} factions, {unit_count} initial units",
        app.world().resource::<SeaZoneCenters>().0.len(),
        app.world().resource::<SimRes>().0.world().factions.len(),
    );

    app.run();
}

/// One `[x, y]` per sea zone: the centroid of its coastal regions' own
/// positions, then pushed further away from the *overall* map's centroid -
/// docs/phase7-spec.md "海域: 地域の外側に" ("sea zones sit outside the
/// regions"). A bare coastal centroid tends to land among (or even inside)
/// the very regions it borders rather than outside them (e.g. a zone
/// bordering three regions arranged around a bay sits at the bay's center,
/// not out at sea); pushing it further out along the direction from the
/// map's own center fixes that without needing any sea-zone coordinate data
/// in the scenario schema at all.
fn sea_zone_centers(world: &SimWorld, positions: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let map_centroid = centroid(positions);
    let push_distance = outward_push_distance(positions);

    world
        .sea_zones
        .iter()
        .map(|z| {
            if z.coast.is_empty() {
                return map_centroid;
            }
            let coastal_positions: Vec<[f32; 2]> = z.coast.iter().map(|&r| positions[r.index()]).collect();
            let coastal_centroid = centroid(&coastal_positions);
            let dir = Vec2::from(coastal_centroid) - Vec2::from(map_centroid);
            let dir = if dir.length_squared() > 1e-6 { dir.normalize() } else { Vec2::new(1.0, 0.0) };
            (Vec2::from(coastal_centroid) + dir * push_distance).into()
        })
        .collect()
}

fn centroid(positions: &[[f32; 2]]) -> [f32; 2] {
    if positions.is_empty() {
        return [0.0, 0.0];
    }
    let mut sum = Vec2::ZERO;
    for &p in positions {
        sum += Vec2::from(p);
    }
    (sum / positions.len() as f32).into()
}

/// How far outward (world units) to push a sea zone marker from its coastal
/// centroid: a fraction of the map's own bounding-box diagonal, so it scales
/// with the map (a 47-region map needs a bigger push than a 10-region one)
/// rather than a fixed constant that's too small for a spread-out map or too
/// large for a compact one.
fn outward_push_distance(positions: &[[f32; 2]]) -> f32 {
    if positions.is_empty() {
        return 0.0;
    }
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for &p in positions {
        let v = Vec2::from(p);
        min = min.min(v);
        max = max.max(v);
    }
    ((max - min).length() * 0.12).max(40.0)
}
