//! Everything that actually touches `bevy` lives under this module tree.
//! `crate::sim_driver`/`crate::layout` (used by `sim_control`/`setup`
//! below) stay Bevy-free - see their own module docs for why. `run` is the
//! only entry point `main.rs` calls.

mod camera_fit;
mod chrome;
mod event_text;
mod fonts;
mod input;
mod map_mode;
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

pub use map_mode::MapMode;
pub use screenshot::{ScreenshotConfig, ScreenshotTrigger, MAX_BLANK_CAPTURE_ATTEMPTS, MIN_RENDER_WARMUP_FRAMES};

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
    /// `--replay <path>`, already parsed via `crate::action_codec::
    /// read_replay` - `Some` means `player` is driven by this recording
    /// (for exactly the `Layer`s it declares - see `crate::sim_driver::
    /// Replay`'s own doc) instead of live input.
    pub replay: Option<crate::sim_driver::Replay>,
    /// `--delegate-military` (docs/design.md §14, `main.rs`'s own doc): a
    /// real, player-facing way to play "I run the economy, the AI runs the
    /// war" from the very first frame, with no keyboard input required at
    /// all - `run` calls `SimDriver::delegate_military()` once at startup
    /// when this is `true`. Not tied to `--screenshot` (an earlier,
    /// screenshot-only debug flag did that instead, and consequently had no
    /// effect without `--screenshot` too - see `main.rs`'s own history
    /// note): this is a first-class way to play, not a verification
    /// convenience.
    ///
    /// `main.rs` rejects this combined with `--replay` outright: a replay's
    /// own declared `Layer` scope (`replay`'s own doc) is now how a
    /// scripted faction hands `Layer::Military` to the AI, so
    /// `--delegate-military` would either be redundant with what the file
    /// already says or - worse, if it disagreed - a flag silently unable to
    /// do anything (`SimDriver::delegate_military` only ever touches a live
    /// `HumanAgent` controller, never a `Controller::Replay`), which is
    /// exactly the silent-no-op docs/conventions.md's no-fallback rule
    /// forbids.
    pub delegate_military: bool,
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
        Action::MoveUnit { .. } | Action::HoldUnit { .. } | Action::ReinforceUnit { .. } | Action::DisbandUnit { .. } => RejectionTarget::Unit,
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
        // Stage 10 follow-up: `panels::StrikePanelRoot` now issues
        // `StrikeNode` (pre-validated the same "disabled, with a reason" way
        // `RegionActionKind` already is - `panels::StrikeKind::reason`
        // mirrors `action::apply_strike_node`'s own preconditions one for
        // one), so a genuine rejection here is a same-tick race rather than
        // the everyday case. `panels::InterdictPanelRoot` now issues
        // `InterdictLine` the identical way (`panels::interdict_reason`
        // mirrors `action::apply_interdict_line`'s own preconditions,
        // `ActionError::NoForceInRange` included - the last known gap this
        // action's contract had). Both left in this same `Policy` bucket
        // regardless - exactly like `MoveUnit`/`DisbandUnit` already sit
        // under `Unit` with no dedicated per-panel display of their own
        // (`ui::update_player_panel`'s always-on "全パネル" list is what
        // actually surfaces either) - rather than inventing a
        // `RejectionTarget` variant whose only job would be to duplicate
        // that same list.
        Action::InterdictLine { .. } | Action::StrikeNode { .. } => RejectionTarget::Policy,
    }
}

/// Japanese label for a `RejectionTarget`, used only by `sim_control::
/// advance_simulation`'s durable event-log line for a rejection (defect fix:
/// a rejection used to be visible for exactly the one tick it happened, in
/// the per-panel corner box below only, and nowhere else - see
/// `advance_simulation`'s own doc for where the durable copy lives and why).
pub(crate) fn rejection_target_ja(target: RejectionTarget) -> &'static str {
    match target {
        RejectionTarget::Region(_) => "地域",
        RejectionTarget::Unit => "部隊",
        RejectionTarget::Policy => "政策",
        RejectionTarget::Diplomacy => "外交",
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

/// One marker radius per region, indexed exactly like `RegionLayout` -
/// `setup::compute_region_radii`'s own doc has the full population-circle-
/// vs-hex-fill policy. Computed once, alongside `RegionLayout`, and shared
/// by every system that needs to know how big a region is actually drawn
/// (`setup::setup`'s own mesh/supply-ring/construction-marker geometry,
/// `overlay::sync_blockade_visuals`'s marker offset, and `input`'s
/// region click/right-click hit-tests) so all of them always agree.
#[derive(Resource)]
pub(crate) struct RegionRadii(pub Vec<f32>);

#[derive(Component)]
pub(crate) struct RegionMarker(pub RegionId);

#[derive(Component)]
pub(crate) struct SeaZoneMarker(pub SeaZoneId);

/// A region name label's dense-map visibility policy
/// (`visuals::sync_region_label_visibility`) - attached to every region
/// label `setup::setup` spawns, sparse or dense alike.
///
/// `always_visible` is unconditionally `true` on a sparse map
/// (`setup::DENSE_REGION_THRESHOLD`, `mvp`/`japan47`): Stage 7B's original
/// "every label always on" behavior, exactly unchanged - the per-frame
/// system's zoom/selection checks then never matter, since the `||` they
/// sit behind already short-circuits true. On a dense map (`japan_hex`) it's
/// `true` only for a faction capital or a population-top-decile region
/// (`setup::significant_regions`) - every other label starts hidden and is
/// revealed only once the player zooms in past `visuals::
/// LABEL_ZOOM_THRESHOLD` or selects that exact region.
#[derive(Component)]
pub(crate) struct RegionLabelMarker {
    pub region: RegionId,
    pub always_visible: bool,
}

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

/// Stage 9D (docs/phase9-spec.md "5. クライアント"): the same zoom-invariant-
/// marker treatment `ChokepointMarker` gets, for a severed line instead of a
/// saturated one (`overlay::COLOR_LINE_CUT`'s own doc) - a thin line segment
/// alone shrinks to nothing at the default whole-map fitted zoom exactly the
/// way an ordinary chokepoint link would without its own marker
/// (`overlay`'s module doc, "Visual hierarchy"). Mutually exclusive with
/// `ChokepointMarker` on any given link pair (a saturated line is still
/// carrying its own full capacity; a cut line carries none), so the two
/// never need to coexist visually on the same link.
#[derive(Component)]
pub(crate) struct CutLineMarker {
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

/// The right-hand column's shared flex container (`setup::spawn_right_column`):
/// `InspectText`, whichever of `panels::RegionActionPanelRoot`/
/// `PolicyPanelRoot`/`DiplomacyPanelRoot` is currently shown, and
/// `PlayerPanelText` are all children of this one node, stacked top-to-bottom
/// by `bevy_ui`'s own flex layout rather than each independently anchored to
/// an edge - see `spawn_right_column`'s own doc for why (it replaces a real,
/// reproducible collision between independently-`PositionType::Absolute`
/// panels that shared this edge and knew nothing about each other).
/// `panels::handle_right_column_scroll` is the only system that reads this
/// marker directly (to find the `ScrollPosition` to adjust); every panel's
/// own visibility/content is still owned by its usual sync system.
#[derive(Component)]
pub(crate) struct RightColumnRoot;

/// Stage 7C's newspaper panel (`N` to toggle) - see `ui::update_newspaper_panel`.
#[derive(Component)]
pub(crate) struct NewspaperPanelText;

/// The always-on owner-color border ring drawn just behind every region's
/// own fill (`map_mode`'s own module doc, "Ownership stays visible in every
/// mode") - pre-spawned once per region in `setup::setup`, alongside
/// `RegionMarker` itself, and recolored (never spawned/despawned) every
/// frame by `visuals::sync_owner_border`.
#[derive(Component)]
pub(crate) struct OwnerBorderMarker(pub RegionId);

/// Stage 10D (docs/phase10-spec.md "Stage 10D": "地図で...飛行場が見える"):
/// one region's airfield status marker - pre-spawned once per region
/// alongside `RegionMarker`/`ConstructionMarker` (`setup::setup`, the same
/// "always spawn, toggle `Visibility`" convention those two already use,
/// since the region graph itself never changes at runtime) and shown only
/// while `MapMode::Air` is active *and* the region actually has an airfield
/// node, colored by whether it's currently operational or struck
/// (`overlay::sync_airfield_markers`).
#[derive(Component)]
pub(crate) struct AirfieldMarker(pub RegionId);

/// Defect fix (a struck port had no map indicator at all, even though a
/// struck airfield already gets `AirfieldMarker`): the port twin of
/// `AirfieldMarker`, same "pre-spawn hidden alongside `RegionMarker`, toggle
/// `Visibility`" convention, same `MapMode::Air`-gated existence check
/// (`world.has_port_node`) and operational/struck coloring
/// (`world.port_node_operational`) - `overlay::sync_port_markers`.
#[derive(Component)]
pub(crate) struct PortMarker(pub RegionId);

/// Builds and runs the Bevy `App`. `world` must already be validated
/// (`archipelago_sim::scenario::build_world`/`load_str`/`load_file`) -
/// `main.rs` never constructs one any other way.
///
/// `screenshot`, when given, makes this run's own `Update` schedule (see
/// `screenshot::maybe_capture_screenshot`) capture the primary window to
/// disk once `ScreenshotConfig::trigger` fires and exit with status 0 -
/// this never reaches into `SimRes`/`SimDriver` on its own; the simulated
/// days that accumulate before the shot are purely a side effect of
/// `sim_control::advance_simulation` already running every unpaused frame.
///
/// A `ScreenshotTrigger::AtDay` target changes two more startup defaults
/// below (`start_paused`/`SpeedRes`), both otherwise untouched by
/// `screenshot` at all: a live `--play` run normally starts paused
/// (docs/phase7-spec.md "`--play` 指定時は一時停止で開始する") with nobody
/// at the keyboard to press `Space`/`1`/`2`/`3` first, which would leave an
/// `AtDay` target forever unreached in an unattended `--screenshot` run -
/// so this run instead starts unpaused, at `Speed::X20`, whenever a day
/// target is what it's waiting for. `AfterFrames` needs neither override -
/// it was already reachable from a paused `--play` run (day 0, held on
/// frame 1 by `SpeedRes::paused`), and Stage 7B's own "starts paused" rule
/// for a *live* player stays exactly as specified otherwise.
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
    // Computed against `&world` before it moves into `SimDriver::new_with_player`
    // below - see `RegionRadii`'s own doc for why every rendering system
    // shares this one Vec instead of each recomputing its own.
    let region_radii = setup::compute_region_radii(&world, &positions);
    let window_height = window_height_for_layout(&positions);

    let player_faction = play.as_ref().map(|p| p.player);
    let record_path = play.as_ref().and_then(|p| p.record.clone());
    let replay_days = play.as_ref().and_then(|p| p.replay.clone());
    let is_replay = replay_days.is_some();

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: format!("Archipelago — {scenario_name}"),
            resolution: bevy::window::WindowResolution::new(WINDOW_WIDTH as u32, window_height as u32),
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
    //
    // Overridden only by an `AtDay` screenshot target - see this function's
    // own doc, "A `ScreenshotTrigger::AtDay` target changes two more
    // startup defaults".
    let day_targeted_screenshot = matches!(screenshot.as_ref().map(|c| c.trigger), Some(ScreenshotTrigger::AtDay(_)));
    let start_paused = player_faction.is_some() && !is_replay && !day_targeted_screenshot;

    // Verification-only conveniences (`--debug-*`, `screenshot::ScreenshotConfig`'s
    // own doc): with no keyboard at the wheel before an automated screenshot
    // fires, these let a panel/overlay start already open instead of
    // needing a live `L`/`D`/`N` press. `false` (every default) whenever
    // `--screenshot` wasn't given at all - identical to Stage 7A/7B's
    // startup state.
    let debug_open_diplomacy = screenshot.as_ref().is_some_and(|c| c.open_diplomacy);
    let debug_open_newspaper = screenshot.as_ref().is_some_and(|c| c.open_newspaper);
    let debug_open_policy = screenshot.as_ref().is_some_and(|c| c.open_policy);
    // `--debug-map-mode <key>`: which `MapMode` a `--screenshot` run starts
    // in, so every mode can be captured unattended (no keyboard at the
    // wheel to press `M` first) - defaults to `MapMode::Political`, exactly
    // as a live run always has, when not given at all.
    let debug_map_mode = screenshot.as_ref().and_then(|c| c.map_mode).unwrap_or_default();
    // `--debug-map-mode industry:<good>` (`ScreenshotConfig::industry_good`'s
    // own doc): which `Good` an `Industry`-mode screenshot run starts
    // showing. `None` (every other `--debug-map-mode` value, or a bare
    // `industry` with no `:<good>` suffix) keeps `ActiveGood::default()`
    // (`Steel`) - exactly what a live run always starts with.
    let debug_industry_good = screenshot.as_ref().and_then(|c| c.industry_good);
    // Computed from `world` here, before it moves into `SimDriver::new_with_player`
    // below - same "lowest-id other living faction" default `input::
    // keyboard_input`'s own `D` binding picks.
    let debug_diplomacy_target = if debug_open_diplomacy {
        player_faction.and_then(|p| world.factions.iter().find(|f| f.id != p && f.alive).map(|f| f.id))
    } else {
        None
    };
    let debug_select_region = screenshot.as_ref().and_then(|c| c.select_region);
    let mut sim_driver = SimDriver::new_with_player(world, seed, player_faction, replay_days);
    // `--delegate-military` (`PlayConfig::delegate_military`'s own doc):
    // one `SimDriver::delegate_military()` call at startup hands the entire
    // `Layer::Military` decision domain - recruitment included - to the AI,
    // covering every unit the player currently owns *and* every one it
    // raises later, with no per-unit loop of any kind needed here (that was
    // the bug this flag's own history fixed: a per-unit snapshot at startup
    // never grew past whatever force existed the moment delegation began).
    if play.as_ref().is_some_and(|p| p.delegate_military) {
        sim_driver.delegate_military();
    }

    app.insert_resource(ClearColor(Color::srgb(0.07, 0.08, 0.10)))
        .insert_resource(SimRes(sim_driver))
        .insert_resource(SpeedRes {
            // `X20` only for the unattended `AtDay` case (this function's
            // own doc) - reaching, say, day 719 at `X1` would need 719
            // frames instead of ~36. Every other run keeps Stage 7A's `X1`
            // default, unchanged.
            last_active: if day_targeted_screenshot { Speed::X20 } else { Speed::X1 },
            paused: start_paused,
        })
        .insert_resource(SelectedRegion(debug_select_region))
        .insert_resource(SelectedSeaZone::default())
        // Usability fix (play-test finding #1): defaults to the `--play`ed
        // faction, not always `FactionId(0)` - a player starting as any
        // faction other than the first now sees their own nation in the
        // left-hand panel from the very first frame, not some other
        // faction's view of them (`ui::update_faction_panel`'s own doc has
        // the full story). Observing-only (`player_faction == None`) keeps
        // the original `FactionId(0)` default - there is no "player's own"
        // faction to prefer.
        .insert_resource(SelectedFaction(player_faction.unwrap_or(FactionId(0))))
        .insert_resource(EventLog::default())
        .insert_resource(ScenarioMeta { name: scenario_name, max_days })
        .insert_resource(RegionLayout(positions))
        .insert_resource(RegionRadii(region_radii))
        .insert_resource(SeaZoneCenters(sea_centers))
        .insert_resource(PlayerFaction(player_faction))
        .insert_resource(SelectedUnits::default())
        .insert_resource(MenuRegion::default())
        .insert_resource(DiplomacyPanel { open: debug_open_diplomacy, target: debug_diplomacy_target })
        .insert_resource(PolicyPanel(debug_open_policy))
        .insert_resource(debug_industry_good.map(ActiveGood).unwrap_or_default())
        .insert_resource(LastRejection::default())
        .insert_resource(map_mode::MapModeRes(debug_map_mode))
        .insert_resource(NlCompose::default())
        .insert_resource(panels::PointerOverUi::default())
        .insert_resource(panels::UnitPanelSlots::default())
        .insert_resource(panels::InterdictPanelSlots::default())
        .insert_resource(NewspaperState { period_start: start_day, open: debug_open_newspaper, ..Default::default() })
        // Cross-attempt state for `screenshot::maybe_capture_screenshot`/
        // `handle_screenshot_captured` (that resource's own doc) - always
        // present, like every other resource in this list, and inert
        // whenever there is no `ScreenshotConfig` for it to coordinate.
        .init_resource::<screenshot::ScreenshotAttempts>()
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
                input::keyboard_zoom,
                input::map_click_select,
                input::map_right_click_menu,
                // Every panel button click just enqueues an `Action` through
                // the exact same `SimRes::push_human_action` door the
                // keyboard bindings above use (this module's own doc, "no
                // privileged path") - ordered here, before `advance_
                // simulation`, so a click lands in this frame's tick exactly
                // like a keypress does.
                panels::handle_speed_button_clicks,
                panels::handle_map_mode_button_clicks,
                map_mode::handle_legend_row_clicks,
                panels::handle_region_action_clicks,
                panels::handle_unit_action_clicks,
                panels::handle_unit_delegate_clicks,
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
            // A standalone call rather than folded into the 13-system chain
            // above - that tuple is already at the practical size this
            // crate's own `.chain()` calls have been kept under elsewhere
            // (see the next block's own doc), so these go here instead.
            // Ordering only needs `SimRes`/`SelectedRegion`/`MapModeRes` to
            // be this frame's own post-tick state, same as every system in
            // the chain above - `.after(...)` alone (no `.chain()`, nothing
            // else in this call to chain against) gets that without needing
            // to grow that tuple at all. The five are independent of each
            // other (disjoint entities: labels, `OwnerBorderMarker`
            // materials, legend UI text, and - Stage 10D, extended by this
            // task's own port-marker defect fix - `AirfieldMarker`/
            // `PortMarker` materials), so no relative order between them is
            // needed either.
            Update,
            (
                visuals::sync_region_label_visibility,
                visuals::sync_owner_border,
                map_mode::sync_mode_legend,
                overlay::sync_airfield_markers,
                overlay::sync_port_markers,
            )
                .after(sim_control::advance_simulation),
        )
        .add_systems(
            // Split from the block above - Bevy's `.chain()` tuple impl has
            // a fixed maximum arity, and the two together (13 + 6) exceed
            // it. Both blocks read post-tick `SimRes` state read-only and
            // write disjoint UI entities, so the only ordering that
            // matters - after `advance_simulation` - is preserved by
            // chaining this block after the first block's own last system.
            Update,
            (
                panels::sync_speed_buttons,
                panels::sync_map_mode_button,
                panels::sync_region_action_buttons,
                panels::sync_strike_panel,
                panels::sync_interdict_panel,
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
        )
        // Independent of every chain above (reads only keyboard/time, writes
        // only the right column's own `ScrollPosition`) - `panels::
        // spawn_right_column`'s own doc has the overflow policy this serves.
        .add_systems(Update, panels::handle_right_column_scroll)
        // A standalone call rather than folded into the first 21-system
        // `.chain()` above (already at, and best not pushed past, this
        // crate's own empirically-found tuple-arity ceiling - see the
        // second `sync_*` block's own doc for how that ceiling was found).
        // `.before(...)` alone gets this the one ordering guarantee it
        // actually needs - a click must enqueue its `Action` before
        // `advance_simulation` ticks, exactly like every click handler in
        // that chain - without growing the tuple at all.
        .add_systems(Update, panels::handle_strike_clicks.before(sim_control::advance_simulation))
        // Same standalone-call reasoning as `handle_strike_clicks` right
        // above (tuple-arity ceiling, not a real ordering difference) - a
        // click here just needs to land before `advance_simulation` ticks.
        .add_systems(Update, panels::handle_interdict_clicks.before(sim_control::advance_simulation))
        // `--debug-select-units` (`screenshot::ScreenshotConfig::select_units`'s
        // own doc): must see this frame's post-tick roster (`.after(...)`)
        // and land before `panels::sync_unit_panel` reads `SelectedUnits`
        // for the same frame's UI (`.before(...)`) - a separate call rather
        // than grown into either chained block above for the identical
        // tuple-arity reason `handle_strike_clicks` already is.
        .add_systems(
            Update,
            screenshot::sync_debug_selected_units.after(sim_control::advance_simulation).before(panels::sync_unit_panel),
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

/// Window width - fixed, never adapted per scenario (unlike `window_height_
/// for_layout` below): every panel in `setup::spawn_ui`/`panels` is
/// positioned with an absolute `Val::Px` offset from an edge (`right: Val::
/// Px(10.0)`, etc.), which stays correct at any height but would need a
/// wholesale relayout to track a varying width safely.
const WINDOW_WIDTH: f32 = 1280.0;
/// Window height floor - the original Stage 7A default, and still exactly
/// what a landscape-ish map (`mvp`'s bounding box is wider than tall)
/// computes to below, so neither existing scenario's window size changes.
const DEFAULT_WINDOW_HEIGHT: f32 = 800.0;
/// Window height ceiling - keeps a pathological scenario (an extremely
/// tall/narrow region layout) from demanding an unreasonably large window.
const MAX_WINDOW_HEIGHT: f32 = 1300.0;

/// Chooses the window's own height so its "safe" (panel-free) area
/// (`camera_fit::SAFE_LEFT`/`RIGHT`/`TOP`/`BOTTOM`) ends up roughly the same
/// *aspect ratio* as the region layout's own bounding box, instead of the
/// fixed `DEFAULT_WINDOW_HEIGHT` alone.
///
/// `japan_hex`'s 289-region archipelago runs diagonally across a bounding
/// box far taller than it is wide (~1580 x 2113 world units, aspect 0.75);
/// the original fixed 800px-tall window's own safe area is *wider* than
/// tall (aspect ~1.24) - `camera_fit::fit_camera_to_map`'s uniform,
/// aspect-preserving scale then has to fit the box's own height, leaving
/// roughly 38% of the safe area's width sitting empty on either side, with
/// no distortion-free way for `fit_camera_to_map` alone to close that gap.
/// Widening *this* window's height to better match the box's own aspect
/// closes most of it without touching `fit_camera_to_map`'s math, or the
/// scenario's own `Region::position` data, at all.
///
/// Width deliberately stays fixed (`WINDOW_WIDTH`) rather than also being
/// adapted - see that constant's own doc. Clamped to
/// `DEFAULT_WINDOW_HEIGHT..=MAX_WINDOW_HEIGHT`: `mvp`/`japan47`'s own
/// bounding boxes are already close enough to the default safe area's
/// aspect that the unclamped formula computes at or below
/// `DEFAULT_WINDOW_HEIGHT` for both (checked directly in this function's
/// own tests), so the floor leaves them at exactly the original window
/// size - no regression for either.
fn window_height_for_layout(positions: &[[f32; 2]]) -> f32 {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for &p in positions {
        let v = Vec2::from(p);
        min = min.min(v);
        max = max.max(v);
    }
    let box_size = max - min;
    if !(box_size.x > 0.0 && box_size.y > 0.0) {
        return DEFAULT_WINDOW_HEIGHT; // Fewer than two distinct positions - nothing to match an aspect ratio to.
    }
    let safe_w = WINDOW_WIDTH - camera_fit::SAFE_LEFT - camera_fit::SAFE_RIGHT;
    let desired_safe_h = safe_w * (box_size.y / box_size.x);
    (desired_safe_h + camera_fit::SAFE_TOP + camera_fit::SAFE_BOTTOM).clamp(DEFAULT_WINDOW_HEIGHT, MAX_WINDOW_HEIGHT)
}

#[cfg(test)]
mod window_height_tests {
    use super::*;

    /// `mvp`'s own bounding box (520 x 400 world units, from `scenarios/
    /// mvp.json`'s own `Region::position` spread) is already close to the
    /// default safe area's aspect ratio, so this must land close to
    /// `DEFAULT_WINDOW_HEIGHT` rather than growing dramatically the way the
    /// tall/narrow `japan_hex`-shaped box below legitimately does.
    ///
    /// This used to assert exact equality with `DEFAULT_WINDOW_HEIGHT` (the
    /// clamp floor) - this task's panel-chrome pass grew `camera_fit::
    /// SAFE_TOP`/`SAFE_BOTTOM` (every panel along those edges gained a real
    /// background/border/padding, and several gained a new title row), which
    /// pushes `mvp`'s own unclamped computation a little past that floor for
    /// the first time (confirmed: this assertion, written against the old
    /// `SAFE_TOP`/`SAFE_BOTTOM`, failed with `left: 836.9231, right: 800.0`
    /// once those margins grew - i.e. this is a real, deliberate change in
    /// `mvp`'s own window size, not a coincidence this test should paper
    /// over). Widened to a tolerance rather than tightened back to an exact
    /// literal that would just go stale the next time a margin is retuned -
    /// docs/conventions.md's own "許容幅は広く取る" - while still catching
    /// the actual regression this test exists for: `mvp` ballooning anywhere
    /// close to `japan_hex`'s own several-hundred-pixel growth below.
    #[test]
    fn mvp_shaped_layout_stays_close_to_the_default_height() {
        let positions = [[0.0, 0.0], [520.0, 0.0], [0.0, 400.0], [520.0, 400.0]];
        let height = window_height_for_layout(&positions);
        assert!(height >= DEFAULT_WINDOW_HEIGHT, "must never go below the floor, got {height}");
        assert!(height <= DEFAULT_WINDOW_HEIGHT + 100.0, "a landscape-ish map like mvp must stay close to the default height, got {height}");
    }

    /// A tall, narrow bounding box (`japan_hex`'s own ~1580 x 2113 shape)
    /// must grow the window well past the default - this is the whole point
    /// of the function - but never past `MAX_WINDOW_HEIGHT`.
    #[test]
    fn tall_narrow_layout_grows_the_window_within_the_ceiling() {
        let positions = [[0.0, 0.0], [1580.0, 0.0], [0.0, 2113.0], [1580.0, 2113.0]];
        let height = window_height_for_layout(&positions);
        assert!(height > DEFAULT_WINDOW_HEIGHT, "a tall map must grow the window, got {height}");
        assert!(height <= MAX_WINDOW_HEIGHT, "must never exceed the ceiling, got {height}");
    }

    /// Fewer than two distinct positions: no bounding-box aspect ratio
    /// exists to match, so this must fall back to the default rather than
    /// dividing by a zero-size box.
    #[test]
    fn degenerate_layout_falls_back_to_the_default() {
        assert_eq!(window_height_for_layout(&[]), DEFAULT_WINDOW_HEIGHT);
        assert_eq!(window_height_for_layout(&[[3.0, 4.0]]), DEFAULT_WINDOW_HEIGHT);
        assert_eq!(window_height_for_layout(&[[3.0, 4.0], [3.0, 9.0]]), DEFAULT_WINDOW_HEIGHT); // zero width
    }
}
