//! Startup system: spawns the camera, the static map geometry (regions,
//! links, sea zones), the initial unit markers, and every UI node. Per-frame
//! updates to any of this live in `visuals`/`ui` - this module only ever
//! runs once.

use bevy::prelude::*;
use bevy::sprite::{Anchor, Text2dShadow};

use archipelago_sim::ids::RegionId;
use archipelago_sim::world::{LinkKind, Station};

use super::fonts::AppFont;
use super::palette::faction_color;
use super::{
    EventLogText, FactionPanelText, InspectText, MainCamera, PlayerPanelText, RegionLayout,
    RegionMarker, SeaZoneCenters, SeaZoneMarker, SimRes, TopBarText, UnitMarker,
};

/// Region circle radius, `population.sqrt()` scaled into roughly
/// `MIN_RADIUS..=MAX_RADIUS` - docs/phase7-spec.md "描画": "大きさは人口".
///
/// Deliberately compressed hard (Stage 7A follow-up: `japan47`'s 47
/// prefectures range from population 55 to 1400, `mvp`'s 10 regions from
/// 330 to 4300 - the original `14.0..=46.0` range, applied to a map where
/// neighboring regions can sit under 40 world-units apart, let Tokyo/Kanto's
/// circle swallow every prefecture around it). Population still modulates
/// the marker (a `sqrt` curve, so it's not linear-dominated by the single
/// largest region) but no longer dominates it - most small-to-mid regions
/// now cluster near `MIN_REGION_RADIUS`, with only the handful of genuine
/// population outliers (Tokyo, Kanto, Osaka, ...) visibly larger.
pub(super) const MIN_REGION_RADIUS: f32 = 9.0;
pub(super) const MAX_REGION_RADIUS: f32 = 22.0;
const POP_SCALE: f32 = 0.4;

/// Sea zone marker radius: small and population-independent (unlike a
/// region, a sea zone's "size" isn't meaningful map data) - see
/// `sea_zone_radius`'s own doc for why this scales gently with how many
/// coastal regions border it rather than staying fixed.
const MIN_SEA_ZONE_RADIUS: f32 = 14.0;
const MAX_SEA_ZONE_RADIUS: f32 = 32.0;

/// Region name label font size. Shrunk from an earlier `13.0` (Stage 7B
/// label-crowding follow-up: at `japan47`'s scale several prefectures sit
/// under 40 world units apart - e.g. 熊本県/大分県, 京都府/滋賀県 - where
/// even a 3-4 character name at the old size ran into its neighbour's own
/// label). Paired with `label_shadow` for contrast and with `label_push_
/// directions`-driven placement (see the region-spawning loop in `setup`
/// below) rather than relied on alone - shrinking the font by itself still
/// left the densest clusters (Kyushu, Kinki, Chugoku) overlapping.
const REGION_LABEL_FONT_SIZE: f32 = 11.0;

/// Gap (world units) between a region's own circle and where its label's
/// near edge sits, in whichever direction `label_push_directions` pushes
/// it.
const LABEL_MARGIN: f32 = 4.0;

/// A small, high-contrast drop shadow behind every region name - cheap
/// legibility insurance for a label that lands over a busy area (another
/// region's circle, a link, or a same-colored background) now that labels
/// no longer all sit on the uniform dark background above their region.
fn label_shadow() -> Text2dShadow {
    Text2dShadow { offset: Vec2::new(1.0, -1.0), color: Color::BLACK.with_alpha(0.9) }
}

/// Sea zones are drawn behind (`z` below) every region/link/unit.
const Z_SEA_ZONE: f32 = -10.0;
const Z_SEA_ZONE_LABEL: f32 = -9.5;
const Z_LINK: f32 = -1.0;
const Z_REGION: f32 = 0.0;
const Z_REGION_LABEL: f32 = 0.5;
const Z_UNIT: f32 = 1.0;

pub(super) fn region_radius(population: f32) -> f32 {
    (population.max(0.0).sqrt() * POP_SCALE).clamp(MIN_REGION_RADIUS, MAX_REGION_RADIUS)
}

/// Sea zone marker radius: scales gently with how many regions border it
/// (a zone touching more coastline reads as "bigger" without needing any
/// population-style figure), clamped small so it always reads as a compact
/// token next to the coast rather than a wash over the map.
pub(super) fn sea_zone_radius(coastal_region_count: usize) -> f32 {
    (MIN_SEA_ZONE_RADIUS + 3.0 * coastal_region_count as f32).clamp(MIN_SEA_ZONE_RADIUS, MAX_SEA_ZONE_RADIUS)
}

pub(super) fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    sim: Res<SimRes>,
    layout: Res<RegionLayout>,
    sea_centers: Res<SeaZoneCenters>,
    font: Res<AppFont>,
) {
    commands.spawn((Camera2d, MainCamera));

    let world = sim.0.world();

    // Every label-bearing point on the map - region markers *and* sea zone
    // markers - feeds one shared `label_push_directions` call, so a region's
    // label repels from nearby sea zone markers (and vice versa) exactly
    // like it repels from a crowded neighbour region. Computing this against
    // regions alone left a real collision: `japan47`'s 山口県 label pushed
    // straight into 瀬戸内海's own label, and 熊本県 into 太平洋南's, since
    // neither zone was part of the crowding math at all. Regions come first
    // (`layout.0` is already `RegionId`-indexed), sea zones after
    // (`sea_centers.0` is `SeaZoneId`-indexed) - `region_count` is the split
    // point used below to slice the combined output back apart.
    let region_count = world.regions.len();
    let all_positions: Vec<[f32; 2]> = layout.0.iter().copied().chain(sea_centers.0.iter().copied()).collect();
    let label_dirs = crate::layout::label_push_directions(&all_positions);
    let (region_label_dirs, sea_label_dirs) = label_dirs.split_at(region_count);

    // Sea zones first, so they render behind everything else. Docs/
    // phase7-spec.md "海域: 地域の外側に...薄く塗る" ("outside the regions,
    // painted thinly") - a small, semi-transparent marker offset away from
    // the landmass (`mod::sea_zone_centers` pushes each center outward from
    // the overall map centroid) rather than the original huge fixed-radius
    // circle, which at this map's scale buried the entire coastline under
    // itself instead of merely suggesting the sea nearby.
    for zone in &world.sea_zones {
        let [x, y] = sea_centers.0[zone.id.index()];
        let radius = sea_zone_radius(zone.coast.len());
        commands.spawn((
            Mesh2d(meshes.add(Circle::new(radius))),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(Color::srgba(0.5, 0.55, 0.6, 0.45)))),
            Transform::from_xyz(x, y, Z_SEA_ZONE),
            SeaZoneMarker(zone.id),
        ));
        let [dx, dy] = sea_label_dirs[zone.id.index()];
        let label_pos = Vec2::new(x, y) + Vec2::new(dx, dy) * (radius + LABEL_MARGIN);
        commands.spawn((
            Text2d::new(zone.name.clone()),
            TextFont { font: font.0.clone().into(), font_size: 11.0.into(), ..default() },
            TextColor(Color::srgba(0.85, 0.88, 0.92, 0.8)),
            label_shadow(),
            Anchor(Vec2::new(-dx, -dy) * 0.5),
            Transform::from_xyz(label_pos.x, label_pos.y, Z_SEA_ZONE_LABEL),
        ));
    }

    // Links: one thin rectangle per direction pair, drawn once (a link's
    // `kind` never changes at runtime, so this geometry is static).
    let mut drawn_pairs: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    for region in &world.regions {
        for link in &region.links {
            let a = region.id.0;
            let b = link.to.0;
            let key = (a.min(b), a.max(b));
            if !drawn_pairs.insert(key) {
                continue;
            }
            spawn_link(&mut commands, &mut meshes, &mut materials, &layout.0, region.id, link.to, link.kind);
        }
    }

    // Regions: a circle sized by population, colored by owner, plus a name
    // label pushed outward from the region's own local cluster (see
    // `crate::layout::label_push_directions`'s own doc, and the combined
    // region+sea-zone call feeding `region_label_dirs` above) rather than
    // always straight above it - at `japan47`'s density, stacking every
    // label above its region collides several neighbours' names in the
    // crowded Kyushu/Kinki/Chugoku clusters; radiating them out in whichever
    // direction is actually free spreads that collision out instead.
    for region in &world.regions {
        let [x, y] = layout.0[region.id.index()];
        let radius = region_radius(region.population);
        commands.spawn((
            Mesh2d(meshes.add(Circle::new(radius))),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(faction_color(region.owner.index())))),
            Transform::from_xyz(x, y, Z_REGION),
            RegionMarker(region.id),
        ));
        let [dx, dy] = region_label_dirs[region.id.index()];
        let label_pos = Vec2::new(x, y) + Vec2::new(dx, dy) * (radius + LABEL_MARGIN);
        // The anchor sits on the side of the label facing the region (so
        // the label's *content* extends away from it, in the push
        // direction) - the opposite side from where it's pushed, hence the
        // negation. `Anchor::BOTTOM_CENTER` (dir == straight up, this loop's
        // pre-Stage-7B behavior) is exactly `Anchor(-[0, 1] * 0.5)`, so an
        // isolated region with no crowding to react to renders identically
        // to before.
        commands.spawn((
            Text2d::new(region.name.clone()),
            TextFont { font: font.0.clone().into(), font_size: REGION_LABEL_FONT_SIZE.into(), ..default() },
            TextColor(Color::WHITE),
            label_shadow(),
            Anchor(Vec2::new(-dx, -dy) * 0.5),
            Transform::from_xyz(label_pos.x, label_pos.y, Z_REGION_LABEL),
        ));
    }

    // Units: one small marker per starting unit, positioned via its current
    // `Station`. `visuals::sync_unit_visuals` keeps these (and any later-
    // recruited unit's own marker) up to date every frame after this.
    for unit in &world.units {
        let [x, y] = station_position(unit.station, &layout.0, &sea_centers.0);
        commands.spawn((
            Mesh2d(meshes.add(RegularPolygon::new(6.0, 3))),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(faction_color(unit.owner.index())))),
            Transform::from_xyz(x, y, Z_UNIT),
            Visibility::from(if unit.alive { Visibility::Visible } else { Visibility::Hidden }),
            UnitMarker(unit.id),
        ));
    }

    spawn_ui(&mut commands, &font.0);
}

pub(super) fn station_position(station: Station, region_pos: &[[f32; 2]], sea_pos: &[[f32; 2]]) -> [f32; 2] {
    match station {
        Station::Region(r) => region_pos[r.index()],
        Station::Sea(z) => sea_pos[z.index()],
    }
}

fn link_style(kind: LinkKind) -> (f32, Color, bool) {
    // (thickness, color, dashed)
    match kind {
        LinkKind::Rail => (5.0, Color::srgb(0.85, 0.85, 0.88), false),
        LinkKind::Road => (2.0, Color::srgb(0.6, 0.6, 0.62), false),
        LinkKind::Tunnel => (3.0, Color::srgb(0.7, 0.55, 0.35), true),
        LinkKind::Strait => (3.0, Color::srgb(0.35, 0.55, 0.85), true),
        // Never authored in a scenario's `links` (see `LinkKind::key`'s own
        // doc) - kept only so this match stays exhaustive without a
        // catch-all hiding a future real variant.
        LinkKind::Sea => (1.0, Color::NONE, true),
    }
}

fn spawn_link(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    positions: &[[f32; 2]],
    from: RegionId,
    to: RegionId,
    kind: LinkKind,
) {
    let [x1, y1] = positions[from.index()];
    let [x2, y2] = positions[to.index()];
    let dx = x2 - x1;
    let dy = y2 - y1;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 0.0 {
        return;
    }
    let angle = dy.atan2(dx);
    let (thickness, color, dashed) = link_style(kind);
    let material = materials.add(ColorMaterial::from_color(color));

    if !dashed {
        let mesh = meshes.add(Rectangle::new(length, thickness));
        commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(material),
            Transform::from_xyz((x1 + x2) / 2.0, (y1 + y2) / 2.0, Z_LINK).with_rotation(Quat::from_rotation_z(angle)),
        ));
        return;
    }

    // Dashed link kinds (tunnel/strait): several short segments along the
    // same line, so they read as visually distinct from a solid rail/road
    // link at a glance (docs/phase7-spec.md "リンク: ... 種別で見分けられる
    // こと").
    const DASH_COUNT: usize = 7;
    let mesh = meshes.add(Rectangle::new((length / DASH_COUNT as f32) * 0.55, thickness));
    for i in 0..DASH_COUNT {
        let t = (i as f32 + 0.5) / DASH_COUNT as f32;
        let x = x1 + dx * t;
        let y = y1 + dy * t;
        commands.spawn((
            Mesh2d(mesh.clone()),
            MeshMaterial2d(material.clone()),
            Transform::from_xyz(x, y, Z_LINK).with_rotation(Quat::from_rotation_z(angle)),
        ));
    }
}

fn text_font(size: f32, font: &Handle<Font>) -> TextFont {
    TextFont { font: font.clone().into(), font_size: size.into(), ..default() }
}

fn spawn_ui(commands: &mut Commands, font: &Handle<Font>) {
    // Top bar: date / scenario / speed.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(6.0),
            left: Val::Px(10.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(18.0, font),
        TextColor(Color::WHITE),
        TopBarText,
    ));

    // Left panel: faction summary.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(40.0),
            left: Val::Px(10.0),
            width: Val::Px(300.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(14.0, font),
        TextColor(Color::srgb(0.92, 0.92, 0.95)),
        FactionPanelText,
    ));

    // Bottom panel: event log, most recent first.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(6.0),
            left: Val::Px(10.0),
            width: Val::Px(760.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(12.0, font),
        TextColor(Color::srgb(0.8, 0.85, 0.8)),
        EventLogText,
    ));

    // Right panel: click-to-inspect region detail.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(40.0),
            right: Val::Px(10.0),
            width: Val::Px(300.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(14.0, font),
        TextColor(Color::srgb(0.92, 0.92, 0.95)),
        InspectText,
    ));

    // Bottom-right panel: Stage 7B player controls - selection state, the
    // recruit/build menu, the diplomacy panel, current policy values, and
    // the most recent rejection reasons. Empty (no text spawned) whenever
    // there is no `--play`ed faction - see `ui::update_player_panel`.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(6.0),
            right: Val::Px(10.0),
            width: Val::Px(320.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(13.0, font),
        TextColor(Color::srgb(1.0, 0.82, 0.45)),
        PlayerPanelText,
    ));
}
