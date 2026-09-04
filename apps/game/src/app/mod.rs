//! Everything that actually touches `bevy` lives under this module tree.
//! `crate::sim_driver`/`crate::layout` (used by `sim_control`/`setup`
//! below) stay Bevy-free - see their own module docs for why. `run` is the
//! only entry point `main.rs` calls.

mod camera_fit;
mod event_text;
mod fonts;
mod input;
mod palette;
mod screenshot;
mod setup;
mod sim_control;
mod ui;
mod visuals;

use std::collections::{BTreeSet, VecDeque};

use bevy::prelude::*;

pub use screenshot::ScreenshotConfig;

use archipelago_sim::action::Action;
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use archipelago_sim::world::World as SimWorld;

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

/// The human/replay faction's most recent rejected orders, translated to
/// Japanese (docs/phase7-spec.md "命令の可否を隠さない") - cleared and
/// refilled every tick by `sim_control::advance_simulation`, so this always
/// reflects the *last* day actions were actually applied, not a
/// accumulating log.
#[derive(Resource, Default)]
pub(crate) struct LastRejection(pub Vec<&'static str>);

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

#[derive(Component)]
pub(crate) struct MainCamera;

#[derive(Component)]
pub(crate) struct TopBarText;

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
pub fn run(world: SimWorld, seed: u64, scenario_name: String, max_days: u32, screenshot: Option<ScreenshotConfig>, play: Option<PlayConfig>) {
    let positions = crate::layout::region_positions(&world);
    let sea_centers = sea_zone_centers(&world, &positions);
    let region_count = world.regions.len();
    let unit_count = world.units.len();

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
    fonts::load(&mut app);

    // Stage 7B (docs/phase7-spec.md "時間の進め方"): "`--play` 指定時は一時
    // 停止で開始する" - a live human player always starts paused so the
    // first day's board can actually be looked at before anything moves.
    // Observing-only (no `--play`) and a `--replay` run (nothing left for
    // the player to decide) both keep Stage 7A's running-at-1x default.
    let start_paused = player_faction.is_some() && !is_replay;

    app.insert_resource(ClearColor(Color::srgb(0.07, 0.08, 0.10)))
        .insert_resource(SimRes(SimDriver::new_with_player(world, seed, player_faction, replay_days)))
        .insert_resource(SpeedRes { last_active: Speed::X1, paused: start_paused })
        .insert_resource(SelectedRegion::default())
        .insert_resource(SelectedFaction(FactionId(0)))
        .insert_resource(EventLog::default())
        .insert_resource(ScenarioMeta { name: scenario_name, max_days })
        .insert_resource(RegionLayout(positions))
        .insert_resource(SeaZoneCenters(sea_centers))
        .insert_resource(PlayerFaction(player_faction))
        .insert_resource(SelectedUnits::default())
        .insert_resource(MenuRegion::default())
        .insert_resource(DiplomacyPanel { open: false, target: None })
        .insert_resource(ActiveGood::default())
        .insert_resource(LastRejection::default())
        .add_systems(Startup, setup::setup)
        .add_systems(
            Update,
            (
                camera_fit::fit_camera_to_map,
                input::keyboard_input,
                input::mouse_pan_zoom,
                input::map_click_select,
                input::map_right_click_menu,
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
                ui::update_top_bar,
                ui::update_faction_panel,
                ui::update_event_log,
                ui::update_inspect_panel,
                ui::update_player_panel,
            )
                .chain()
                .after(sim_control::advance_simulation),
        )
        .add_systems(
            Update,
            screenshot::maybe_capture_screenshot.after(ui::update_player_panel),
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
