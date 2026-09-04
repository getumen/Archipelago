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

use std::collections::VecDeque;

use bevy::prelude::*;

pub use screenshot::ScreenshotConfig;

use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use archipelago_sim::world::World as SimWorld;

use crate::sim_driver::{SimDriver, Speed};

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
pub fn run(world: SimWorld, seed: u64, scenario_name: String, max_days: u32, screenshot: Option<ScreenshotConfig>) {
    let positions = crate::layout::region_positions(&world);
    let sea_centers = sea_zone_centers(&world, &positions);
    let region_count = world.regions.len();
    let unit_count = world.units.len();

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

    app.insert_resource(ClearColor(Color::srgb(0.07, 0.08, 0.10)))
        .insert_resource(SimRes(SimDriver::new(world, seed)))
        .insert_resource(SpeedRes { last_active: Speed::X1, paused: false })
        .insert_resource(SelectedRegion::default())
        .insert_resource(SelectedFaction(FactionId(0)))
        .insert_resource(EventLog::default())
        .insert_resource(ScenarioMeta { name: scenario_name, max_days })
        .insert_resource(RegionLayout(positions))
        .insert_resource(SeaZoneCenters(sea_centers))
        .add_systems(Startup, setup::setup)
        .add_systems(
            Update,
            (
                camera_fit::fit_camera_to_map,
                input::keyboard_input,
                input::mouse_pan_zoom,
                input::region_click_select,
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
            )
                .chain()
                .after(sim_control::advance_simulation),
        )
        .add_systems(
            Update,
            screenshot::maybe_capture_screenshot.after(ui::update_inspect_panel),
        );

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
