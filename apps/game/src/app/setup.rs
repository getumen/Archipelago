//! Startup system: spawns the camera, the static map geometry (regions,
//! links, sea zones), the initial unit markers, and every UI node. Per-frame
//! updates to any of this live in `visuals`/`ui` - this module only ever
//! runs once.

use bevy::prelude::*;
use bevy::sprite::{Anchor, Text2dShadow};

use archipelago_sim::ids::RegionId;
use archipelago_sim::world::{LinkKind, Station, World as SimWorld};

use super::fonts::AppFont;
use super::overlay;
use super::palette::faction_color;
use super::{
    EventLogText, FactionPanelText, InspectText, MainCamera, PlayerPanelText, RegionLabelMarker,
    RegionLayout, RegionMarker, RegionRadii, SeaZoneCenters, SeaZoneMarker, SimRes,
    SupplyOnlyLegendRow, TopBarText, UnitMarker,
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

/// Above this many regions, a map stops being able to carry the
/// population-scaled-circle-plus-permanent-label presentation the
/// `MIN_REGION_RADIUS..=MAX_REGION_RADIUS` tuning above and
/// `RegionLabelMarker`'s "always visible" policy were built for
/// (`japan47`'s 47 regions is the largest scenario that design was ever
/// tuned against). `japan_hex`'s 289 regions sit far past it; `japan47`
/// sits far under it - picked with wide margin on both sides so neither
/// existing scenario is at risk of drifting across it as either gets minor
/// future edits. Read by `compute_region_radii` (marker shape/size) and
/// `setup` (label significance) - see both for what actually changes.
pub(super) const DENSE_REGION_THRESHOLD: usize = 100;

/// How much smaller than the exact tiling circumradius
/// (`crate::layout::nearest_neighbor_pitch(..) / sqrt(3)`, the value that
/// makes adjacent pointy-top hexes touch exactly) a dense map's hex marker
/// is drawn - leaves a thin gap of background between same-color neighbours
/// so individual cells still read as separate territory up close, instead
/// of fusing into one undifferentiated blob.
const DENSE_HEX_FILL_RATIO: f32 = 0.92;

/// The population percentile (`setup::population_significance_threshold`)
/// a dense map's region must clear to keep its label "always visible"
/// (`RegionLabelMarker`'s own doc) - roughly the top decile, which keeps
/// the always-on label count in the low dozens even for `japan_hex`'s 289
/// regions (29 of them clear the 90th percentile) rather than either
/// "every region" (the original crowding problem this whole change exists
/// to fix) or "almost none" (a map that reads as anonymous colored
/// territory with no names to anchor it at all).
const POPULATION_SIGNIFICANCE_PERCENTILE: f32 = 0.90;

/// Per-region marker radius for every region on this map, indexed exactly
/// like `RegionLayout`/`RegionId::index()` - the single source of truth
/// both `setup` (spawning the mesh/supply-ring/construction-marker
/// geometry) and `overlay::sync_blockade_visuals`/`input`'s click hit-tests
/// read, so every one of them agrees on how big a region actually is drawn.
///
/// Sparse maps (`world.regions.len() <= DENSE_REGION_THRESHOLD` -
/// `mvp`/`japan47`, unchanged from Stage 7A): `region_radius(population)`
/// per region, exactly as before.
///
/// Dense maps (`japan_hex`): one shared hex-fill radius for every region,
/// derived from the scenario's own grid pitch
/// (`crate::layout::nearest_neighbor_pitch`) rather than population -
/// population-scaled circles at this density mostly cluster near
/// `MIN_REGION_RADIUS` (task's own complaint: "sparse dots with gaps"),
/// since `japan_hex`'s per-cell populations are individually much smaller
/// than `japan47`'s per-prefecture ones even though the map covers the same
/// territory. A uniform hex sized to the grid's own pitch instead tiles the
/// map as continuous colored territory - see `DENSE_HEX_FILL_RATIO` for why
/// it's not sized to *exactly* tile.
pub(super) fn compute_region_radii(world: &SimWorld, positions: &[[f32; 2]]) -> Vec<f32> {
    if world.regions.len() > DENSE_REGION_THRESHOLD {
        let pitch = crate::layout::nearest_neighbor_pitch(positions);
        let hex_radius = (pitch / 3f32.sqrt() * DENSE_HEX_FILL_RATIO).clamp(MIN_REGION_RADIUS, MAX_REGION_RADIUS);
        vec![hex_radius; world.regions.len()]
    } else {
        world.regions.iter().map(|r| region_radius(r.population)).collect()
    }
}

/// Regions whose name label stays visible on a dense map even before the
/// player zooms in (`visuals::sync_region_label_visibility`,
/// `RegionLabelMarker::always_visible`) - every faction's own capital, plus
/// the population top decile (`population_significance_threshold`).
/// Irrelevant on a sparse map, where every label is always visible
/// regardless of this set (see `setup`'s own region-spawning loop).
fn significant_regions(world: &SimWorld) -> std::collections::HashSet<RegionId> {
    let mut significant: std::collections::HashSet<RegionId> = world.factions.iter().map(|f| f.capital).collect();
    let threshold = population_significance_threshold(world);
    for region in &world.regions {
        if region.population >= threshold {
            significant.insert(region.id);
        }
    }
    significant
}

/// The population value at `POPULATION_SIGNIFICANCE_PERCENTILE` across
/// every region on the map - `significant_regions`'s population-based
/// criterion. Sorted with `f32::total_cmp` rather than the panic-on-`NaN`
/// `partial_cmp`: population is never `NaN` in practice, but this keeps the
/// sort itself infallible regardless. An empty region list returns
/// `f32::INFINITY` so the (never-reached, since `setup` never runs against
/// zero regions) percentile check below simply never matches anything,
/// rather than indexing an empty `Vec`.
fn population_significance_threshold(world: &SimWorld) -> f32 {
    let mut populations: Vec<f32> = world.regions.iter().map(|r| r.population).collect();
    if populations.is_empty() {
        return f32::INFINITY;
    }
    populations.sort_by(f32::total_cmp);
    let idx = ((populations.len() as f32) * POPULATION_SIGNIFICANCE_PERCENTILE) as usize;
    populations[idx.min(populations.len() - 1)]
}

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
pub(super) const Z_LINK: f32 = -1.0;
/// Where `overlay::sync_supply_overlay` raises a chokepoint/active-route
/// link segment to while the overlay is on - above every region's own fill
/// (`Z_REGION`) so a short link between two adjacent, densely-packed
/// regions (common on `japan47`) isn't hidden under their circles, but
/// below `Z_REGION_LABEL` so it never covers a name.
pub(super) const Z_LINK_HIGHLIGHT: f32 = 0.25;
/// Where a saturated chokepoint link renders - above `Z_LINK_HIGHLIGHT`
/// (so it always wins over an ordinary active route it happens to cross or
/// sit beside) but still below `Z_REGION_LABEL`. See `overlay`'s own module
/// doc for why a chokepoint is deliberately the single most visually
/// dominant mark this overlay ever draws.
pub(super) const Z_LINK_CHOKEPOINT: f32 = 0.35;
const Z_REGION: f32 = 0.0;
/// Stage 7C's supply-overlay ring (`overlay::sync_supply_overlay`) - just
/// above the region's own fill, below its label and any construction
/// marker, so it always reads as "around this region" rather than
/// competing with either.
const Z_SUPPLY_RING: f32 = 0.2;
/// Stage 7C's in-progress-construction marker (`overlay::sync_construction_markers`).
const Z_CONSTRUCTION: f32 = 0.3;
/// Stage 7C follow-up: a saturated-chokepoint marker at a same-owner link's
/// midpoint (`overlay`'s own module doc has the whole rationale) - above
/// `Z_LINK_CHOKEPOINT` so it always wins over the link segment it sits on,
/// still below `Z_REGION_LABEL`.
const Z_CHOKEPOINT_MARKER: f32 = 0.4;
const Z_REGION_LABEL: f32 = 0.5;
const Z_UNIT: f32 = 1.0;

/// A chokepoint marker's own local mesh size (world units at `Transform::
/// scale == 1`). `overlay::sync_supply_overlay` rescales this every frame
/// to `Vec3::splat(camera_zoom)` (the orthographic projection's current
/// `scale`), which exactly cancels that same `scale` in Bevy's world-to-
/// screen conversion - so the marker always renders at this many *screen*
/// pixels regardless of how far the camera is zoomed in or out, or how
/// long or short the underlying link is. That's the whole point: a
/// saturated strait/tunnel segment must be as findable at the default
/// whole-map fitted view as a saturated rail line, and a link-thickness
/// scale alone (`overlay::CHOKEPOINT_SCALE`) can't guarantee that - a link
/// only a few world units long shrinks to sub-pixel at map-fit zoom no
/// matter how many times its own thickness is multiplied.
pub(super) const CHOKEPOINT_MARKER_RADIUS: f32 = 6.0;

/// Supply-ring thickness (world units) - the annulus `overlay::sync_supply_overlay`
/// recolors every frame while the overlay is on.
const SUPPLY_RING_THICKNESS: f32 = 4.0;
const SUPPLY_RING_GAP: f32 = 2.0;

/// In-progress-construction marker radius and offset from the region's own
/// circle - small and off to one side so it never obscures the region's
/// owner-color fill or its supply ring.
const CONSTRUCTION_MARKER_RADIUS: f32 = 5.0;

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
    radii: Res<RegionRadii>,
    sea_centers: Res<SeaZoneCenters>,
    font: Res<AppFont>,
    player: Res<super::PlayerFaction>,
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
            japanese_label_layout(),
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

            // Stage 7C follow-up's chokepoint marker (`CHOKEPOINT_MARKER_RADIUS`'s
            // own doc): one per link pair, at the link's midpoint, pre-spawned
            // hidden and shown/rescaled every frame by
            // `overlay::sync_supply_overlay` exactly like `SupplyRingMarker`/
            // `ConstructionMarker` above - never spawned/despawned at runtime,
            // since the link graph itself never changes, only which links are
            // *currently* saturated.
            let [x1, y1] = layout.0[region.id.index()];
            let [x2, y2] = layout.0[link.to.index()];
            commands.spawn((
                Mesh2d(meshes.add(RegularPolygon::new(CHOKEPOINT_MARKER_RADIUS, 4))),
                MeshMaterial2d(materials.add(ColorMaterial::from_color(overlay::COLOR_CHOKEPOINT))),
                Transform::from_xyz((x1 + x2) / 2.0, (y1 + y2) / 2.0, Z_CHOKEPOINT_MARKER),
                Visibility::Hidden,
                super::ChokepointMarker { a: region.id, b: link.to },
            ));
        }
    }

    // Regions: a marker sized/shaped by `compute_region_radii` (population-
    // scaled circle on a sparse map, uniform hex-fill on a dense one - see
    // that function's own doc), colored by owner, plus a name label pushed
    // outward from the region's own local cluster (see
    // `crate::layout::label_push_directions`'s own doc, and the combined
    // region+sea-zone call feeding `region_label_dirs` above) rather than
    // always straight above it - at `japan47`'s density, stacking every
    // label above its region collides several neighbours' names in the
    // crowded Kyushu/Kinki/Chugoku clusters; radiating them out in whichever
    // direction is actually free spreads that collision out instead.
    //
    // `is_dense`/`significant` gate `RegionLabelMarker::always_visible`
    // (`visuals::sync_region_label_visibility`'s own doc has the full
    // policy): a sparse map keeps Stage 7B's original "every label always
    // on" behavior unconditionally; a dense map only keeps a capital or
    // population-top-decile region's label on by default; every other label
    // waits for the player to zoom in or select that region.
    let is_dense = world.regions.len() > DENSE_REGION_THRESHOLD;
    let significant = if is_dense { significant_regions(world) } else { Default::default() };
    for region in &world.regions {
        let [x, y] = layout.0[region.id.index()];
        let radius = radii.0[region.id.index()];
        let mesh = if is_dense { meshes.add(RegularPolygon::new(radius, 6)) } else { meshes.add(Circle::new(radius)) };
        commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(faction_color(region.owner.index())))),
            Transform::from_xyz(x, y, Z_REGION),
            RegionMarker(region.id),
        ));

        // Stage 7C's supply-network overlay ring (docs/phase7-spec.md "1."):
        // pre-spawned hidden (`Visibility::Hidden`), toggled visible and
        // recolored every frame by `overlay::sync_supply_overlay` while `L`
        // has the overlay on.
        commands.spawn((
            Mesh2d(meshes.add(Annulus::new(radius + SUPPLY_RING_GAP, radius + SUPPLY_RING_GAP + SUPPLY_RING_THICKNESS))),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(Color::NONE))),
            Transform::from_xyz(x, y, Z_SUPPLY_RING),
            Visibility::Hidden,
            super::SupplyRingMarker(region.id),
        ));

        // Stage 7C's in-progress-construction marker (docs/phase7-spec.md
        // "3."): pre-spawned hidden, shown (and its fill alpha driven by
        // progress) only while `Region::construction.is_some()` -
        // `overlay::sync_construction_markers`.
        commands.spawn((
            Mesh2d(meshes.add(Circle::new(CONSTRUCTION_MARKER_RADIUS))),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(Color::NONE))),
            Transform::from_xyz(x + radius * 0.7, y + radius * 0.7, Z_CONSTRUCTION),
            Visibility::Hidden,
            super::ConstructionMarker(region.id),
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
            japanese_label_layout(),
            TextColor(Color::WHITE),
            label_shadow(),
            Anchor(Vec2::new(-dx, -dy) * 0.5),
            Transform::from_xyz(label_pos.x, label_pos.y, Z_REGION_LABEL),
            RegionLabelMarker { region: region.id, always_visible: !is_dense || significant.contains(&region.id) },
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

    spawn_ui(&mut commands, &font.0, player.0.is_some());

    // Stage 8B's panel UI (owner ask - see `panels`'s own module doc):
    // spawned once here, alongside every other static UI node above - see
    // each spawn function's own doc for why it's safe to pre-spawn
    // (region/policy/diplomacy actions are a fixed set; the unit panel is a
    // fixed-size pool).
    super::panels::spawn_region_action_panel(&mut commands, &font.0);
    super::panels::spawn_unit_panel(&mut commands, &font.0);
    super::panels::spawn_policy_panel(&mut commands, &font.0);
    if let Some(player_faction) = player.0 {
        super::panels::spawn_diplomacy_panel(&mut commands, &font.0, world, player_faction);
    }
}

pub(super) fn station_position(station: Station, region_pos: &[[f32; 2]], sea_pos: &[[f32; 2]]) -> [f32; 2] {
    match station {
        Station::Region(r) => region_pos[r.index()],
        Station::Sea(z) => sea_pos[z.index()],
    }
}

pub(super) fn link_style(kind: LinkKind) -> (f32, Color, bool) {
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
            super::LinkVisualMarker { a: from, b: to, kind },
        ));
        return;
    }

    // Dashed link kinds (tunnel/strait): several short segments along the
    // same line, so they read as visually distinct from a solid rail/road
    // link at a glance (docs/phase7-spec.md "リンク: ... 種別で見分けられる
    // こと"). Every segment shares one `Handle<ColorMaterial>` (`material.
    // clone()` clones the handle, not the asset), so `overlay::sync_supply_
    // overlay` recoloring any one segment's material recolors all of them
    // in one write.
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
            super::LinkVisualMarker { a: from, b: to, kind },
        ));
    }
}

pub(super) fn text_font(size: f32, font: &Handle<Font>) -> TextFont {
    TextFont { font: font.clone().into(), font_size: size.into(), ..default() }
}

/// `TextLayout` for a single-line Japanese map/status label (region and
/// sea-zone names, the top bar's one-line status): never soft-wraps.
///
/// This is correct typography independent of anything below: these labels
/// must never wrap in the first place - `label_push_directions`-driven
/// placement (this module's own doc above) positions each one assuming it
/// stays a single line, and the top bar is a single status line by design.
/// `NoWrap` says so explicitly rather than relying on these labels merely
/// never being long enough to need to.
///
/// ## ICU4X segmentation: why this does *not* silence the stderr warning
///
/// Running this client (any platform) prints "ICU4X data error: No
/// segmentation model for complex script: Chinese/Japanese" on stderr
/// once per text entity per layout - confirmed on this (Linux) machine by
/// running the client and counting the lines. The proximate cause: Bevy
/// 0.19's text stack goes through Parley 0.9.0 (pinned exactly by
/// `bevy_text` 0.19.1's own `Cargo.toml` - `version = "0.9.0"`, so Cargo's
/// feature unification cannot pull in a newer, semver-incompatible Parley
/// release such as 0.11.x even indirectly), which in turn depends on
/// `icu_segmenter` 2.3.0 with only its `compiled_data` feature (not
/// `lstm`/`auto`). Verified with `cargo tree -p archipelago-game -f "{p}
/// {f}"`: `icu_segmenter v2.3.0 compiled_data` is the only feature ever
/// listed for it, unless this crate pulls in `icu_segmenter` itself with
/// `auto` - it was tried; see below for why that doesn't help either.
///
/// It would be tempting to read this as "enable `icu_segmenter`'s `auto`
/// feature from `apps/game` and Cargo's feature unification will turn it
/// on for Parley's copy too" (`icu_segmenter` is a single shared crate
/// instance across the whole dependency graph once unified) - and that
/// part is true; `cargo tree` does show the feature lands on the shared
/// crate. But reading Parley 0.9.0's own source
/// (`parley-0.9.0/src/analysis/mod.rs`, `AnalysisDataSources::
/// word_segmenter`/`line_segmenter`) shows every segmenter it builds is
/// constructed via `WordSegmenter::new_for_non_complex_scripts`/
/// `LineSegmenter::new_for_non_complex_scripts` - the one constructor
/// family that *never* loads Chinese/Japanese (or Thai/Lao/Khmer/Myanmar)
/// data, no matter which `icu_segmenter` Cargo features are compiled in.
/// Loading that data requires calling one of `icu_segmenter`'s own
/// `load_dictionary`/`load_auto`/`new_dictionary`/`new_auto` APIs, and
/// nothing in Parley 0.9.0 ever calls any of them (confirmed by grep - zero
/// matches for any of those names in that crate's source). So turning the
/// feature on changes what *compiles into* `icu_segmenter` but not what
/// Parley *calls* - the segmentation data stays unloaded regardless, and
/// enabling `auto`/`lstm` was reverted as dead weight (confirmed inert,
/// not merely unhelpful) rather than kept "just in case".
///
/// `word_segmenter()` above is also called unconditionally for every text
/// layout (regardless of `LineBreak`/wrap policy - it feeds
/// `Boundary::Word` marks used elsewhere in Parley, not line wrapping
/// specifically), so no choice of Bevy `LineBreak` on any entity avoids
/// triggering it either - confirmed empirically: switching every panel in
/// this module to `LineBreak::AnyCharacter` and every label here to
/// `NoWrap`, then running the exact same scenario, produced the exact same
/// stderr line count (8336 lines over an identical 180-frame run) as the
/// unmodified client. This is an upstream Parley 0.9.0 limitation with no
/// fix reachable from this crate's `Cargo.toml` or its `TextLayout`
/// choices - not silenced (docs/conventions.md §3 forbids suppressing a
/// real signal), just correctly diagnosed as out of this crate's reach.
/// The message is cosmetic-only: this client already renders Japanese
/// correctly with it printing (confirmed by screenshot, both here and by
/// the project owner on macOS, where the window opens and text renders
/// normally despite the same stderr noise).
fn japanese_label_layout() -> TextLayout {
    TextLayout::linebreak(LineBreak::NoWrap)
}

fn spawn_ui(commands: &mut Commands, font: &Handle<Font>, has_player: bool) {
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
        japanese_label_layout(),
        TextColor(Color::WHITE),
        TopBarText,
    ));

    // Stage 8B: the player's own key figures, on a second top-bar line -
    // docs/design.md §16 owner ask ("stockpiles per commodity, manpower,
    // stability, war support, shortage" at a glance, not buried in the
    // faction browser). Empty (no text) whenever no faction was `--play`ed -
    // `ui::update_top_bar_player_stats`.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(26.0),
            left: Val::Px(10.0),
            width: Val::Px(900.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(13.0, font),
        TextColor(Color::srgb(1.0, 0.82, 0.45)),
        super::TopBarPlayerStatsText,
    ));

    // Stage 8B: clickable speed controls + the policy/diplomacy panel
    // toggles, next to the existing status line - `panels::sync_speed_buttons`/
    // `handle_speed_button_clicks` and `panels::handle_policy_toggle`/
    // `handle_diplomacy_button_clicks`'s own `DiplomacyToggleButton` branch.
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            top: Val::Px(4.0),
            left: Val::Px(600.0),
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(10.0),
            ..default()
        })
        .with_children(|row| {
            super::panels::spawn_speed_buttons(row, font);
            // Observer mode (no `--play`ed faction, `has_player == false`)
            // has no policy/diplomacy to control - `input::keyboard_input`'s
            // own `D`/policy bindings likewise only fire once a player
            // exists (`let Some(player_faction) = player.0 else { return }`),
            // so these buttons simply aren't offered rather than sitting
            // there as a no-op.
            if has_player {
                row.spawn((Button, Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)), ..default() }, BackgroundColor(Color::srgb(0.22, 0.30, 0.24)), super::panels::PolicyToggleButton))
                    .with_children(|b| {
                        b.spawn((Text::new("政策 [P]"), text_font(13.0, font), TextColor(Color::WHITE)));
                    });
                row.spawn((
                    Button,
                    Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)), ..default() },
                    BackgroundColor(Color::srgb(0.22, 0.30, 0.24)),
                    super::panels::DiplomacyToggleButton,
                ))
                .with_children(|b| {
                    b.spawn((Text::new("外交 [D]"), text_font(13.0, font), TextColor(Color::WHITE)));
                });
            }
        });

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

    // Center panel: Stage 7C's newspaper (`N` to toggle) -
    // `ui::update_newspaper_panel`. Empty text whenever the panel is closed
    // or no issue has been published yet - never a placeholder.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(90.0),
            left: Val::Px(340.0),
            width: Val::Px(600.0),
            ..default()
        },
        Text::new(String::new()),
        text_font(14.0, font),
        TextColor(Color::srgb(0.95, 0.93, 0.85)),
        super::NewspaperPanelText,
    ));

    spawn_legend(commands, font);
}

/// A small, always-present key to every mark Stage 7C's overlays can put on
/// the map (`overlay`'s own module doc has the full visual-hierarchy
/// rationale) - so a viewer never has to read this crate's source to know
/// what a color means - plus, at the top, the camera/order controls
/// themselves (`input`'s own module doc for the full binding list): without
/// this, nothing on screen ever told a player panning existed at all, let
/// alone how to do it on a device with no right-drag-capable mouse. Three
/// groups: the controls list and two always-on-legend rows
/// (blockade/construction), both shown unconditionally, and the supply-
/// overlay-specific rows, shown only while `SupplyOverlay` (`L`) is on -
/// `overlay::sync_legend_visibility` toggles those together with the
/// overlay itself. Sits in the one gap the rest of this crate's UI layout
/// leaves free at the bottom of the window, between the event log
/// (`left: 10, width: 760`, ending at `770`) and the player panel
/// (`right: 10, width: 320`, starting at `950`).
fn spawn_legend(commands: &mut Commands, font: &Handle<Font>) {
    let label_color = Color::srgba(0.85, 0.87, 0.90, 0.95);
    let header_color = Color::srgba(0.6, 0.63, 0.67, 0.9);

    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(6.0),
            left: Val::Px(775.0),
            width: Val::Px(170.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(1.0),
            ..default()
        })
        .with_children(|parent| {
            let mut row = |label: &str, color: Color, supply_only: bool| {
                let mut e = parent.spawn((Text::new(label.to_string()), text_font(10.0, font), TextColor(color)));
                if supply_only {
                    e.insert((Visibility::Hidden, SupplyOnlyLegendRow));
                }
            };

            // Camera/order controls (`input::mouse_pan_zoom`/`keyboard_pan`/
            // `map_click_select`/`map_right_click_menu`) - see those systems'
            // own docs for exactly what each binding does and why. Kept to
            // one line per binding, matched to the same `<=31`-char width
            // the longest existing legend row below already proves fits.
            row("controls:", header_color, false);
            row("pan: scroll or middle-drag", label_color, false);
            row("pan: arrow keys (always)", label_color, false);
            row("zoom: ctrl+scroll / pinch", label_color, false);
            row("select/order: left click", label_color, false);
            row("region menu: right click", label_color, false);
            row("policy panel: P button/key", label_color, false);
            row("diplomacy panel: D button/key", label_color, false);
            row("unit hold/reinforce: buttons or H/J", label_color, false);

            row("legend", label_color, false);
            row("■ port blockaded", overlay::BLOCKADE_MARKER_COLOR, false);
            row("■ under construction", overlay::CONSTRUCTION_TINT, false);
            row("supply overlay (L):", header_color, true);
            row("■ chokepoint (saturated)", overlay::COLOR_CHOKEPOINT, true);
            row("■ active supply route", overlay::COLOR_ACTIVE_ROUTE, true);
            row("■ relay route (spare capacity)", overlay::COLOR_RELAY_FULL, true);
            row("● ring: region starved", overlay::RING_STARVED, true);
            row("● ring: region full", overlay::RING_FULL, true);
            row("● ring: contested", overlay::RING_CONTESTED, true);
        });
}
