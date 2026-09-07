//! Startup system: spawns the camera, the static map geometry (regions,
//! links, sea zones), the initial unit markers, and every UI node. Per-frame
//! updates to any of this live in `visuals`/`ui` - this module only ever
//! runs once.

use bevy::prelude::*;
use bevy::sprite::{Anchor, Text2dShadow};

use archipelago_sim::ids::RegionId;
use archipelago_sim::world::{LinkKind, Station, World as SimWorld};

use super::chrome;
use super::fonts::AppFont;
use super::map_mode::{ModeLegendHeader, ModeLegendRow, MODE_LEGEND_ROWS};
use super::overlay;
use super::palette::faction_color;
use super::{
    EventLogText, FactionPanelText, InspectText, MainCamera, OwnerBorderMarker, PlayerPanelText,
    RegionLabelMarker, RegionLayout, RegionMarker, RegionRadii, RightColumnRoot, SeaZoneCenters,
    SeaZoneMarker, SimRes, TopBarText, UnitMarker,
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
/// `OwnerBorderMarker`'s own layer - just behind every region's own fill
/// (`Z_REGION`) so only the ring sticking out past the fill's own edge ever
/// shows, above every link/sea-zone layer so it's never itself hidden by
/// either. See `owner_border_radius`'s own doc for the geometry this paints.
const Z_OWNER_BORDER: f32 = -0.05;
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
/// Stage 9D: the same zoom-invariant treatment as `CHOKEPOINT_MARKER_RADIUS`
/// (its own doc has the full reasoning - a short link's own geometry shrinks
/// to nothing at map-fit zoom regardless of color or thickness), for a
/// severed line instead of a saturated one. A distinct shape (triangle, not
/// `ChokepointMarker`'s diamond) and a hair smaller, so the two never read
/// as the same mark even before color is considered - the two states are
/// mutually exclusive per link (`CutLineMarker`'s own doc) but sit at the
/// same screen position, so shape is what tells a viewer which is which
/// once they're used to looking for either.
pub(super) const CUT_MARKER_RADIUS: f32 = 5.0;
/// Same layer as `Z_CHOKEPOINT_MARKER` - the two markers never coexist on
/// one link, so there is no stacking order to get right between them.
const Z_CUT_MARKER: f32 = 0.4;

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

/// How far outside a region's own fill radius its always-on owner-color
/// border ring (`OwnerBorderMarker`, `visuals::sync_owner_border`) extends -
/// see `map_mode`'s own module doc, "Ownership stays visible in every
/// mode", for why every region needs one at all regardless of the active
/// `MapMode`.
///
/// Dense hex map (`is_dense`): grows the fill's own hex radius back out to
/// the *exact* tiling circumradius `compute_region_radii` deliberately
/// shrank it from (`DENSE_HEX_FILL_RATIO`'s own doc - "leaves a thin gap of
/// background... so individual cells still read as separate") - so a
/// same-owner neighbor's border reaches that identical boundary and the two
/// meet edge to edge with neither a gap nor an overlap, and two different
/// owners' borders still meet cleanly at that shared edge instead of either
/// fusing into one blob or leaving a visible seam of bare background
/// between them.
///
/// Sparse map (population-scaled circles): there's no equivalent tiling
/// pitch to reuse, so this instead grows the radius by a fixed fraction of
/// itself, clamped into a range that stays a thin, readable ring across the
/// whole `MIN_REGION_RADIUS..=MAX_REGION_RADIUS` span `region_radius` can
/// produce.
pub(super) fn owner_border_radius(radius: f32, is_dense: bool) -> f32 {
    if is_dense {
        radius / DENSE_HEX_FILL_RATIO
    } else {
        radius + (radius * 0.18).clamp(2.0, 5.0)
    }
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

            // Stage 9D: `CutLineMarker`'s own doc - the same pre-spawned,
            // hidden-until-relevant, zoom-rescaled treatment as the
            // chokepoint marker just above, for a severed line instead of a
            // saturated one.
            commands.spawn((
                Mesh2d(meshes.add(RegularPolygon::new(CUT_MARKER_RADIUS, 3))),
                MeshMaterial2d(materials.add(ColorMaterial::from_color(overlay::COLOR_LINE_CUT))),
                Transform::from_xyz((x1 + x2) / 2.0, (y1 + y2) / 2.0, Z_CUT_MARKER),
                Visibility::Hidden,
                super::CutLineMarker { a: region.id, b: link.to },
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

        // The always-on owner-color border (`map_mode`'s own module doc,
        // "Ownership stays visible in every mode"): a slightly larger mesh
        // in the same shape as the fill below it, spawned first so it sits
        // behind (`Z_OWNER_BORDER < Z_REGION`) - only the ring sticking out
        // past the fill's own edge ever actually shows.
        let border_radius = owner_border_radius(radius, is_dense);
        let border_mesh = if is_dense { meshes.add(RegularPolygon::new(border_radius, 6)) } else { meshes.add(Circle::new(border_radius)) };
        commands.spawn((
            Mesh2d(border_mesh),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(faction_color(region.owner.index())))),
            Transform::from_xyz(x, y, Z_OWNER_BORDER),
            OwnerBorderMarker(region.id),
        ));

        let mesh = if is_dense { meshes.add(RegularPolygon::new(radius, 6)) } else { meshes.add(Circle::new(radius)) };
        commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(faction_color(region.owner.index())))),
            Transform::from_xyz(x, y, Z_REGION),
            RegionMarker(region.id),
        ));

        // Stage 7C's supply-network overlay ring (docs/phase7-spec.md "1."):
        // pre-spawned hidden (`Visibility::Hidden`), toggled visible and
        // recolored every frame by `overlay::sync_supply_overlay` while
        // `MapMode::Supply` is the active map mode.
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
        let [x, y] = station_position(world, unit.station, &layout.0, &sea_centers.0);
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
    // fixed-size pool). `spawn_left_column` places the faction summary and
    // the unit panel together, on the *left*, so this side never competes
    // with the right column below; region-action/policy/diplomacy share
    // that other column and are spawned as its children instead.
    spawn_left_column(&mut commands, &font.0);
    spawn_right_column(&mut commands, &font.0, world, player.0);
}

/// The left column: `FactionPanelText`'s own panel, then `panels::
/// UnitPanelRoot` below it - one `FlexDirection::Column` container instead
/// of two independently `PositionType::Absolute` boxes (the same fix
/// `spawn_right_column`'s own doc already applies on the other side, for
/// exactly the same reason - see `panels::spawn_unit_panel`'s own doc for
/// the reproduced collision this replaces). No `Overflow::scroll_y()` here
/// unlike the right column: the faction panel's own worst case (every other
/// living faction listed once, `update_faction_panel`'s own diplomacy loop)
/// is bounded by the scenario's own faction count, not by anything a player
/// or the simulation can grow without limit, so it comfortably fits every
/// shipped scenario's window without needing a scroll escape hatch.
fn spawn_left_column(commands: &mut Commands, font: &Handle<Font>) {
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            top: Val::Px(FACTION_PANEL_TOP),
            left: Val::Px(10.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(PANEL_GAP),
            ..default()
        })
        .with_children(|col| {
            col.spawn(chrome::framed(Node { width: Val::Px(300.0), flex_direction: FlexDirection::Column, row_gap: Val::Px(3.0), ..default() }))
                .insert((chrome::panel_background(), chrome::panel_border()))
                .with_children(|panel| {
                    panel.spawn(chrome::panel_title("-- 勢力 --", font));
                    panel.spawn((Node::default(), Text::new(String::new()), text_font(14.0, font), chrome::panel_body_color(), FactionPanelText));
                });

            super::panels::spawn_unit_panel(col, font);
        });
}

/// The right column's own fixed width and its margin from the window's
/// right edge (`spawn_right_column`'s own `Node`) - lifted to module scope,
/// not kept as locals inside that function alone, so every panel that must
/// stay clear of the column (`spawn_ui`'s newspaper panel, `spawn_legend`,
/// and the event log width that panel is in turn derived from) reads the
/// exact same numbers the column is actually drawn at, rather than each
/// restating its own guess at where the column starts. That drift is
/// exactly what caused two real collisions (`codex review`, and this task's
/// own reproduction): the column's own box grew from an effective 320px-wide
/// right-anchored area to this 340px one - needed so `panels::
/// spawn_region_action_panel`/`spawn_policy_panel`/`spawn_diplomacy_panel`
/// (all 340px) fit inside it without clipping - and neither the newspaper
/// panel's width nor the legend's width/position had been re-derived against
/// the new, 20px-further-left boundary that produced.
pub(super) const RIGHT_COLUMN_WIDTH: f32 = 340.0;
pub(super) const RIGHT_COLUMN_RIGHT_MARGIN: f32 = 10.0;
/// Left edge (window-relative x) of the right column's own box - the single
/// horizontal boundary every neighbouring panel below stays clear of.
pub(super) const RIGHT_COLUMN_LEFT_EDGE: f32 = super::WINDOW_WIDTH - RIGHT_COLUMN_RIGHT_MARGIN - RIGHT_COLUMN_WIDTH;

/// Minimum horizontal gap a panel keeps from a neighbour it must not overlap
/// - shared by every derived width/position below (the newspaper panel, the
/// legend, and the event log it in turn makes room for) so the fix is one
/// uniform relationship rather than each panel picking its own margin.
const PANEL_GAP: f32 = 10.0;

/// Everything that used to be four independent `PositionType::Absolute`
/// nodes anchored to the same right edge (`InspectText` from the top,
/// `PlayerPanelText` from the bottom, and whichever of `panels::
/// RegionActionPanelRoot`/`PolicyPanelRoot`/`DiplomacyPanelRoot` happened to
/// be open, both pinned near the top) - each one sized only by its own
/// content, with no idea any of the others existed. Long region-inspect text
/// (occupied-region/construction detail) growing down could and did collide,
/// pixel for pixel, with the diplomacy panel's own buttons growing from a
/// `top` that started only 6px below `InspectText`'s own `top` (reproduced
/// with `--debug-open-diplomacy --debug-select-region`, confirmed by
/// screenshot) - and, separately, with `PlayerPanelText` growing up from the
/// bottom whenever *its own* content (a long unit list, an open menu, several
/// rejections) ran long too.
///
/// Fixed by construction rather than by re-tuning offsets (a hand-picked gap
/// only holds until someone's text is one line longer): every one of those
/// nodes is now a child of *one* `FlexDirection::Column` container
/// (`RightColumnRoot`), in reading order - `InspectText` first (region detail
/// stays nearest the top, exactly where it always was), then whichever
/// action/policy/diplomacy panel is open (`panels::sync_region_action_buttons`/
/// `sync_policy_panel`/`sync_diplomacy_panel` now toggle each root's own
/// `Node::display` between `Flex`/`None` alongside their existing
/// `Visibility` toggle, so a hidden panel also stops reserving flex space -
/// without that, the other two's height would sit as permanent dead space
/// even while closed), then `PlayerPanelText` last (player controls stay
/// below region detail, exactly as before). Flex layout stacks them
/// top-to-bottom unconditionally - two children can no longer occupy the same
/// vertical span no matter how long either one's text gets.
///
/// **Overflow policy** (explicitly decided, not left to chance): the column
/// is `Overflow::scroll_y()` - if the combined content is ever taller than
/// its own box (a long region, a long unit list, and an open panel, all at
/// once), it clips and scrolls rather than spilling past the window's own
/// bottom edge (invisible, and not recoverable by any player action - the
/// one outcome docs/conventions.md's "state has a recovery path" rule and
/// this task's own "content must never be silently lost" ask both rule out)
/// or being cut off with no way back. `panels::handle_right_column_scroll`
/// (`PageUp`/`PageDown`, documented in the on-screen legend) is that
/// recovery path - `ScrollPosition` is clamped to the valid range by
/// `bevy_ui`'s own layout system every frame, so no manual bounds-checking
/// is needed here to keep a short frame's worth of content from "scrolling"
/// into empty space.
///
/// **The box's own height tracks the window, not a startup sample of it**
/// (CLAUDE.md's "発令時点の値を焼き込まない" - never bake in a value that
/// should keep tracking current state): an earlier version pinned `height`
/// to `Val::Px(window_height - TOP - BOTTOM_MARGIN)` using the window height
/// the process happened to start with, so shrinking a resizable window (or a
/// window manager overriding the requested startup size) left the column
/// extending past the window's real bottom edge - content clipped by the
/// *window* there is unreachable by any amount of `PageDown`, since
/// `bevy_ui`'s scroll clamp only ever knows about the column's own
/// (stale-height) box, not the window around it.
///
/// Fixed with no sampling at all, rather than a system re-reading the
/// window's size every frame: `top`/`bottom` are both set and `height` is
/// left `Val::Auto`. `bevy_ui`'s own layout (`taffy`'s absolute-positioning
/// rule for a box with both opposing insets set and no explicit size on that
/// axis - the same rule every other `bottom: Val::Px(_)`-anchored panel in
/// this module already relies on to track the window's actual bottom edge,
/// e.g. the event log/legend below) fills the height in from whatever the
/// *current* window/viewport size is, every time `bevy_ui`'s layout system
/// runs - which is every frame a window resize (a drag, or a WM overriding
/// the requested startup size) actually changes it. See
/// `right_column_tests::right_column_height_is_window_relative_not_a_baked_constant`
/// for the regression this pins, and its own doc for how it was confirmed to
/// fail against the pre-fix `Val::Px` version before the fix went in.
fn spawn_right_column(commands: &mut Commands, font: &Handle<Font>, world: &SimWorld, player_faction: Option<archipelago_sim::ids::FactionId>) {
    const TOP: f32 = 40.0;
    const BOTTOM_MARGIN: f32 = 6.0;

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(TOP),
                bottom: Val::Px(BOTTOM_MARGIN),
                right: Val::Px(RIGHT_COLUMN_RIGHT_MARGIN),
                width: Val::Px(RIGHT_COLUMN_WIDTH),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            RightColumnRoot,
        ))
        .with_children(|col| {
            // Chrome-framed like every other panel now (`chrome`'s own
            // module doc), and - unlike before this task, when it was a bare
            // `Text` node that simply rendered nothing - starts
            // `Visibility::Hidden`: this panel's own text is genuinely empty
            // until a region is actually selected, and an empty bordered box
            // sitting at the top of the right column on every observer-mode
            // screen would be exactly the clutter this task's "restrained"
            // ask rules out (`ui::update_inspect_panel` keeps `Visibility` in
            // sync with that same emptiness every frame - no separate static
            // title child here, since the dynamic body's own first line
            // already reads as one, `"{region.name}（{terrain}）"`, exactly
            // like `PlayerPanelText` below already relies on its own
            // `"== プレイヤー: {name} =="` opening line). `width:
            // RIGHT_COLUMN_WIDTH` - same width as every other child in this
            // column now that each one is chrome-framed, rather than the
            // narrower `300.0`/`320.0` this and `PlayerPanelText` below used
            // to be (`AlignItems::End`, no longer needed once every child
            // shares one width, is dropped from this container's own `Node`
            // above rather than left in place doing nothing).
            col.spawn((
                // `display: Display::None` matches the initial `Visibility::
                // Hidden` right below - `ui::update_inspect_panel` keeps both
                // in sync every frame from here on (`chrome::set_panel_shown`),
                // but the very first frame reads whatever this literal spawns,
                // and a mismatched pair here would reserve this box's flex
                // space in the right column for that one frame regardless of
                // what the sync system does afterwards.
                chrome::framed(Node { display: Display::None, width: Val::Px(RIGHT_COLUMN_WIDTH), ..default() }),
                chrome::panel_background(),
                chrome::panel_border(),
                Visibility::Hidden,
                Text::new(String::new()),
                text_font(14.0, font),
                chrome::panel_body_color(),
                InspectText,
            ));

            super::panels::spawn_region_action_panel(col, font);
            super::panels::spawn_policy_panel(col, font);
            if let Some(player_faction) = player_faction {
                super::panels::spawn_diplomacy_panel(col, font, world, player_faction);
            }

            // Always shown once a faction is `--play`ed (its own dynamic
            // text always opens with `"== プレイヤー: {name} =="` - `ui::
            // update_player_panel`'s own doc - so it never needs the same
            // hide-when-empty treatment `InspectText` above gets), hidden
            // outright in observer mode via the same `Visibility` toggle
            // (`update_player_panel` sets it) rather than a permanently
            // empty chrome-framed box with nothing to show.
            col.spawn((
                chrome::framed(Node { width: Val::Px(RIGHT_COLUMN_WIDTH), ..default() }),
                chrome::panel_background(),
                chrome::panel_border(),
                Visibility::Visible,
                Text::new(String::new()),
                text_font(13.0, font),
                TextColor(chrome::PANEL_TITLE_COLOR),
                PlayerPanelText,
            ));
        });
}

/// Stage 10A: `world` is only ever consulted for a `Station::Airfield` unit
/// (to find its node's own region and reuse that region's marker position)
/// - land/sea stations still resolve straight from `region_pos`/`sea_pos`
/// exactly as before, so this stays a cheap lookup for the two domains
/// every existing scenario actually deploys.
pub(super) fn station_position(world: &SimWorld, station: Station, region_pos: &[[f32; 2]], sea_pos: &[[f32; 2]]) -> [f32; 2] {
    match station {
        Station::Region(r) => region_pos[r.index()],
        Station::Sea(z) => sea_pos[z.index()],
        Station::Airfield(n) => region_pos[world.transport_node(n).region.index()],
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

/// The top-bar chrome panel's own `top` offset (`spawn_ui`'s own top-bar
/// panel) - named once so `FACTION_PANEL_TOP` below reads the exact value
/// the panel is actually spawned at, rather than a second, independently
/// hand-picked `6.0`.
const TOP_BAR_TOP: f32 = 6.0;

/// Conservative reserved height for the top-bar panel's own box (both its
/// text lines, plus `chrome`'s own padding/border on every edge) - not
/// measured pixel-for-pixel from font metrics (this crate has no cheap way
/// to query Bevy's own text shaping ahead of a real layout pass), but
/// generous enough that neither line's rendered height can push the panel's
/// real bottom edge past it (confirmed by screenshot, this task's own
/// verification, with both lines populated - the `--play` case, the taller
/// of the two). `FACTION_PANEL_TOP` below derives its own clearance from
/// this instead of an independently-guessed number that could silently
/// drift out of sync with the top-bar panel's actual layout - exactly the
/// kind of collision `RIGHT_COLUMN_LEFT_EDGE`'s own doc already describes
/// happening twice for the right column.
const TOP_BAR_RESERVED_HEIGHT: f32 = 64.0;

/// `spawn_ui`'s faction-summary panel's own `top` - clears the top-bar
/// panel's own reserved box (`TOP_BAR_RESERVED_HEIGHT`'s own doc) by
/// `PANEL_GAP`, rather than the independently hand-picked `40.0` this used
/// to be before the top bar gained its own chrome frame.
const FACTION_PANEL_TOP: f32 = TOP_BAR_TOP + TOP_BAR_RESERVED_HEIGHT + PANEL_GAP;

fn spawn_ui(commands: &mut Commands, font: &Handle<Font>, has_player: bool) {
    // Top bar: date / scenario / speed, plus (Stage 8B) the player's own key
    // figures on a second line - one chrome-framed panel (`chrome`'s own
    // module doc) instead of two independently `PositionType::Absolute` bare
    // `Text` nodes, so the one status readout every player has on screen at
    // all times also gets a background/border like every other panel now
    // does. `TOP_BAR_RESERVED_HEIGHT`'s own doc is what everything below
    // this panel (`FACTION_PANEL_TOP`, `camera_fit::SAFE_TOP`) derives its
    // own clearance from, so this panel's frame can never silently grow into
    // a neighbour again.
    commands
        .spawn(chrome::framed(Node {
            position_type: PositionType::Absolute,
            top: Val::Px(TOP_BAR_TOP),
            left: Val::Px(10.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            ..default()
        }))
        .insert((chrome::panel_background(), chrome::panel_border()))
        .with_children(|col| {
            col.spawn((
                Node::default(),
                Text::new(String::new()),
                text_font(18.0, font),
                japanese_label_layout(),
                chrome::panel_body_color(),
                TopBarText,
            ));

            // Stage 8B: the player's own key figures (docs/design.md §16
            // owner ask - "stockpiles per commodity, manpower, stability,
            // war support, shortage" at a glance, not buried in the faction
            // browser). Empty text, and hidden (`ui::update_top_bar_player_stats`
            // keeps both in sync every frame), whenever no faction was
            // `--play`ed - an observer-mode run must not carry an empty
            // chrome-framed row taking up space for content that will never
            // arrive.
            //
            // `japanese_label_layout()` (`NoWrap`), same as `TopBarText`
            // above and every region/sea-zone label: this is a one-line
            // status readout by design, exactly like those - `update_top_bar_
            // player_stats` joins every `Good`'s own stock figure onto one
            // line, and with six goods plus manpower/stability/war-support/
            // shortage that line can run past `width: 900`; `NoWrap` keeps a
            // long line from ever wrapping down into this same panel's next
            // row instead of re-tuning `width` against today's good count.
            col.spawn((
                Node { width: Val::Px(900.0), ..default() },
                Text::new(String::new()),
                text_font(13.0, font),
                japanese_label_layout(),
                TextColor(chrome::PANEL_TITLE_COLOR),
                Visibility::Visible,
                super::TopBarPlayerStatsText,
            ));
        });

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
            // The map-mode button (`map_mode`'s own module doc, "Cycling
            // modes must be obvious and discoverable") - available in
            // observer mode too, unlike the policy/diplomacy toggles below,
            // since seeing terrain/population/industry/unrest never
            // requires a `--play`ed faction.
            super::panels::spawn_map_mode_button(row, font);
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

    // Left panel: faction summary + unit panel - `spawn_left_column` (called
    // from `setup` alongside this function, not from here - see that
    // function's own doc for why the two are now one flex container).

    // Bottom panel: event log, most recent first. Width derived (`EVENT_LOG_PANEL_WIDTH`'s
    // own doc) from wherever the legend actually starts, not restated
    // independently - the legend already sits in "the one gap this crate's
    // layout leaves free" right after this panel (`spawn_legend`'s own
    // doc), so the two have always shared this boundary; deriving it keeps
    // them sharing it by construction instead of by two constants that
    // happen to agree today.
    commands
        .spawn(chrome::framed(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(6.0),
            left: Val::Px(EVENT_LOG_PANEL_LEFT),
            width: Val::Px(EVENT_LOG_PANEL_WIDTH),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(3.0),
            ..default()
        }))
        .insert((chrome::panel_background(), chrome::panel_border()))
        .with_children(|panel| {
            panel.spawn(chrome::panel_title("-- イベント --", font));
            panel.spawn((Node::default(), Text::new(String::new()), text_font(12.0, font), chrome::panel_body_color(), EventLogText));
        });

    // The right column - `InspectText` (click-to-inspect region detail),
    // whichever of `panels::RegionActionPanelRoot`/`PolicyPanelRoot`/
    // `DiplomacyPanelRoot` is currently open, and `PlayerPanelText` (Stage
    // 7B player controls - selection state, the recruit/build menu, and the
    // most recent rejection reasons; empty whenever there is no `--play`ed
    // faction, see `ui::update_player_panel`) - is spawned together, as one
    // flex container, by `spawn_right_column` (called from `setup` alongside
    // this function, not from here - it needs `SimWorld`/`PlayerFaction`,
    // which this function doesn't take). See that function's own doc for why
    // these can no longer be four independent `PositionType::Absolute` nodes.

    // Center panel: Stage 7C's newspaper (`N` to toggle) -
    // `ui::update_newspaper_panel`. Empty text whenever the panel is closed
    // or no issue has been published yet - never a placeholder.
    //
    // Width is `NEWSPAPER_PANEL_WIDTH`, derived from the right column's own
    // left edge rather than the independent `600.0` this used to be - fixes
    // the 10px overlap `codex review` found (this panel used to reach
    // `x=940`, the right column's own box now starts at `x=930`).
    // Chrome-framed like every other panel, but starts `Visibility::Hidden`
    // (with a matching `display: Display::None`, same reasoning as
    // `spawn_right_column`'s own `InspectText`) - unlike the
    // always-something-to-say panels above, this one's own text is genuinely
    // empty whenever the panel is closed (`ui::update_newspaper_panel` keeps
    // both in sync with that same emptiness every frame via `chrome::
    // set_panel_shown`) - an empty bordered box sitting on the map with
    // nothing in it would be exactly the clutter this task's own "restrained"
    // ask rules out.
    commands.spawn((
        chrome::framed(Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            top: Val::Px(90.0),
            left: Val::Px(NEWSPAPER_PANEL_LEFT),
            width: Val::Px(NEWSPAPER_PANEL_WIDTH),
            ..default()
        }),
        chrome::panel_background(),
        chrome::panel_border(),
        Visibility::Hidden,
        Text::new(String::new()),
        text_font(14.0, font),
        TextColor(Color::srgb(0.95, 0.93, 0.85)),
        super::NewspaperPanelText,
    ));

    spawn_legend(commands, font);
}

/// A small, always-present key to every mark Stage 7C's overlays and
/// `map_mode`'s map modes can put on the map (`overlay`'s own module doc has
/// the full visual-hierarchy rationale) - so a viewer never has to read this
/// crate's source to know what a color means - plus, at the top, the
/// camera/order controls themselves (`input`'s own module doc for the full
/// binding list): without this, nothing on screen ever told a player
/// panning existed at all, let alone how to do it on a device with no
/// right-drag-capable mouse. Three groups: the controls list, two always-on
/// legend rows plus the owner-border explanation (blockade/construction/
/// border), all shown unconditionally, and the active `MapMode`'s own
/// header + swatch rows (`ModeLegendHeader`/`ModeLegendRow`), filled in and
/// shown/hidden every frame by `map_mode::sync_mode_legend` according to
/// whichever mode is current - including the supply overlay's own rows,
/// now one mode among the rest rather than a separately-toggled block. Sits
/// in the gap between the event log and the right column.
///
/// `LEGEND_PANEL_WIDTH` is this panel's one genuinely content-driven
/// constant, not derived from a neighbour - sized to fit `map_mode::
/// legend_header`'s longest line (`供給路と詰まり箇所（旧Lキー表示）`,
/// ~165px measured against the bundled font at this row's own font size)
/// without wrapping into a second line (it used to, at an older, narrower
/// width, into a ragged 2-3 line block that visually collided with the
/// `controls:` rows above it) - so this stays fixed and everything else
/// here is arranged around it instead.
///
/// `LEGEND_PANEL_LEFT` is what actually moves (`codex review`, and this
/// task's own 18px-overlap finding): it used to be an independent `772.0`,
/// picked back when the right column's own box started at `x=950` (an
/// effectively-320px-wide right-anchored area) - that box is now
/// `RIGHT_COLUMN_LEFT_EDGE` (`x=930`, `panels::
/// spawn_region_action_panel`/friends all being 340px wide), so this is
/// derived from that boundary instead: `RIGHT_COLUMN_LEFT_EDGE -
/// LEGEND_PANEL_WIDTH - PANEL_GAP`. `EVENT_LOG_PANEL_WIDTH` (`spawn_ui`'s
/// own event-log `Node`) is in turn derived from *this* - the event log and
/// the legend have always shared that boundary by design (this doc's own
/// "the gap between the event log and the right column"), so deriving one
/// from the other keeps them sharing it by construction rather than by two
/// independently hand-picked constants that happen to agree today.
/// `176.0` (the content-driven measurement this doc above describes) plus
/// `chrome::PANEL_FRAME_INSET` on both sides - this panel gained a
/// background/border/padding frame in this task, and `bevy_ui`'s border-box
/// sizing (`chrome`'s own module doc) means that frame eats into the *same*
/// `176.0` unless the outer width grows to compensate, which would have
/// wrapped `legend_header`'s own longest line right back into the two-line
/// mess this constant's own doc says this width was picked to avoid.
const LEGEND_PANEL_WIDTH: f32 = 176.0 + 2.0 * chrome::PANEL_FRAME_INSET;
const LEGEND_PANEL_LEFT: f32 = RIGHT_COLUMN_LEFT_EDGE - LEGEND_PANEL_WIDTH - PANEL_GAP;

/// `spawn_ui`'s event log `Node`'s own `left`/`width` - `EVENT_LOG_PANEL_WIDTH`'s
/// own doc (on `LEGEND_PANEL_LEFT` above) has the derivation.
const EVENT_LOG_PANEL_LEFT: f32 = 10.0;
const EVENT_LOG_PANEL_WIDTH: f32 = LEGEND_PANEL_LEFT - EVENT_LOG_PANEL_LEFT - PANEL_GAP;

/// `spawn_ui`'s newspaper panel's own `left`/`width` - derived the same way
/// (`RIGHT_COLUMN_LEFT_EDGE`'s own doc): `NEWSPAPER_PANEL_LEFT` stays fixed
/// (it never competed with the event log/legend, only the right column), so
/// only the width moves.
const NEWSPAPER_PANEL_LEFT: f32 = 340.0;
const NEWSPAPER_PANEL_WIDTH: f32 = RIGHT_COLUMN_LEFT_EDGE - NEWSPAPER_PANEL_LEFT - PANEL_GAP;

fn spawn_legend(commands: &mut Commands, font: &Handle<Font>) {
    let label_color = Color::srgba(0.85, 0.87, 0.90, 0.95);
    let header_color = Color::srgba(0.6, 0.63, 0.67, 0.9);

    commands
        .spawn(chrome::framed(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(6.0),
            left: Val::Px(LEGEND_PANEL_LEFT),
            width: Val::Px(LEGEND_PANEL_WIDTH),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(1.0),
            ..default()
        }))
        .insert((chrome::panel_background(), chrome::panel_border()))
        .with_children(|parent| {
            parent.spawn(chrome::panel_title("-- 操作と凡例 --", font));

            let mut row = |label: &str, color: Color| {
                parent.spawn((Text::new(label.to_string()), text_font(10.0, font), TextColor(color)));
            };

            // Camera/order controls (`input::mouse_pan_zoom`/`keyboard_pan`/
            // `map_click_select`/`map_right_click_menu`) - see those systems'
            // own docs for exactly what each binding does and why. Kept to
            // one line per binding, matched to the same `<=31`-char width
            // the longest existing legend row below already proves fits.
            row("操作:", header_color);
            row("移動: スクロール/中ドラッグ", label_color);
            row("移動: 矢印キー（常時）", label_color);
            row("ズーム: ctrl+スクロール/ピンチ", label_color);
            // `input::keyboard_zoom` - the device-independent zoom path this
            // task adds: works from a bare keyboard with no scroll wheel or
            // trackpad at all, exactly like "移動: 矢印キー（常時）" above
            // already does for panning. `-/=` is the same `KeyCode::Minus`/
            // `KeyCode::Equal` pair, and the same "-/=" notation,
            // `panels::PolicyField::label`'s conscription row already uses
            // in the policy panel (`"徴兵率 [-/=]"`) - `Ctrl` is what tells
            // this binding apart from that one (`input::keyboard_zoom`'s own
            // doc has the full collision-avoidance reasoning).
            row("ズーム: ctrl+ -/=（常時）", label_color);
            row("選択/命令: 左クリック", label_color);
            row("地域メニュー: 右クリック", label_color);
            row("地図モード: M ボタン/キー", label_color);
            row("対象品目: 行クリック/G", label_color);
            row("政策パネル: P ボタン/キー", label_color);
            row("外交パネル: D ボタン/キー", label_color);
            row("部隊待機/補充: ボタン/H/J", label_color);
            // `panels::handle_right_column_scroll` - the right column's own
            // overflow policy (`setup::spawn_right_column`'s own doc,
            // "Overflow policy") needs a discoverable way to actually reach
            // clipped content, not just a mechanism nobody knows exists.
            row("右パネル: PageUp/PageDown", label_color);

            row("凡例", label_color);
            row("■ 港湾封鎖中", overlay::BLOCKADE_MARKER_COLOR);
            row("■ 建設中", overlay::CONSTRUCTION_TINT);
            row("外側の輪 = 所属勢力", label_color);

            // The active `MapMode`'s own header + up to `MODE_LEGEND_ROWS`
            // swatch rows (`map_mode::sync_mode_legend` fills these in every
            // frame from `map_mode::legend_header`/`legend_entries` -
            // replaces what used to be a fixed, supply-only block here,
            // folding that overlay's own legend into the same mechanism
            // every other mode now shares).
            parent.spawn((Text::new(String::new()), text_font(10.0, font), TextColor(header_color), ModeLegendHeader));
            for i in 0..MODE_LEGEND_ROWS {
                // `Interaction`/`BackgroundColor`: `Industry` mode's own
                // rows double as its commodity picker (`map_mode::
                // ModeLegendRow`'s own doc) - every row gets both
                // unconditionally, from this one shared pool, since a
                // `Visibility::Hidden` row in every other mode is never
                // reported as clicked at all and never shows a highlight
                // either.
                parent.spawn((
                    Text::new(String::new()),
                    text_font(10.0, font),
                    TextColor(label_color),
                    BackgroundColor(Color::NONE),
                    Interaction::None,
                    Visibility::Hidden,
                    ModeLegendRow(i),
                ));
            }
        });
}

#[cfg(test)]
mod right_column_tests {
    use bevy::ecs::world::CommandQueue;

    use archipelago_sim::ids::FactionId;
    use archipelago_sim::scenario;

    use super::*;

    /// Regression guard for the collision this task fixes: `InspectText`
    /// (region detail) and `PlayerPanelText` (player controls) used to be
    /// two independent `PositionType::Absolute` nodes anchored to opposite
    /// edges of the same right-hand column - one `top: 40` growing down, the
    /// other `bottom: 6` growing up - with nothing stopping them from
    /// meeting in the middle when both had enough text (reproduced with
    /// `--debug-open-diplomacy --debug-select-region`, confirmed by
    /// screenshot; the diplomacy panel shared the same collision against
    /// `InspectText` even more directly, both starting only 6px apart from
    /// the top).
    ///
    /// `spawn_right_column` fixes this by construction: both are children of
    /// one shared `FlexDirection::Column` container (`RightColumnRoot`), so
    /// this checks exactly that structural fact rather than any particular
    /// pixel offset (which the task's own review explicitly warns against
    /// hand-tuning). Confirmed this fails without the fix: reverting
    /// `spawn_right_column` to spawn `InspectText`/`PlayerPanelText` as two
    /// top-level `PositionType::Absolute` nodes (`setup::spawn_ui`'s own
    /// pre-fix shape) makes the first assertion below fail with "InspectText
    /// has no parent" - there is no shared container to find at all.
    #[test]
    fn inspect_and_player_panels_share_one_column_stacking_container() {
        let world_data = scenario::build_world();
        let font = Handle::<Font>::default();

        let mut world = World::new();
        let mut queue = CommandQueue::default();
        {
            let mut commands = Commands::new(&mut queue, &world);
            spawn_right_column(&mut commands, &font, &world_data, Some(FactionId(0)));
        }
        queue.apply(&mut world);

        let inspect = {
            let mut q = world.query_filtered::<Entity, With<InspectText>>();
            q.iter(&world).next().expect("spawn_right_column must spawn an InspectText entity")
        };
        let player_panel = {
            let mut q = world.query_filtered::<Entity, With<PlayerPanelText>>();
            q.iter(&world).next().expect("spawn_right_column must spawn a PlayerPanelText entity")
        };

        let inspect_parent = world.get::<ChildOf>(inspect).expect("InspectText has no parent - it is not part of any shared stacking container").parent();
        let player_parent = world.get::<ChildOf>(player_panel).expect("PlayerPanelText has no parent - it is not part of any shared stacking container").parent();
        assert_eq!(
            inspect_parent, player_parent,
            "InspectText and PlayerPanelText must be children of the same container, so bevy_ui's own flex layout stacks them instead of letting two independently-anchored nodes overlap"
        );

        // A shared parent alone isn't enough - it also has to actually stack
        // its children (a `Column` flex container) rather than merely group
        // two still-independently-`Absolute` siblings under one entity that
        // does nothing layout-wise.
        let container_node = world.get::<Node>(inspect_parent).expect("the shared container must itself be a UI Node");
        assert_eq!(
            container_node.flex_direction,
            FlexDirection::Column,
            "the shared right-column container must lay its children out as a column so they stack top-to-bottom"
        );
        for (label, child) in [("InspectText", inspect), ("PlayerPanelText", player_panel)] {
            let node = world.get::<Node>(child).unwrap_or_else(|| panic!("{label} must have a Node"));
            assert_ne!(
                node.position_type,
                PositionType::Absolute,
                "{label} must not opt back into independent absolute positioning inside the stacking column - that would let it overlap its siblings again"
            );
        }
    }

    /// Pins the fix for the "bakes in the startup window height" bug
    /// `codex review` found (`spawn_right_column`'s own doc, "The box's own
    /// height tracks the window, not a startup sample of it"): an earlier
    /// version computed the column's `height` once, from whatever window
    /// height the process happened to start with, and stored it as a fixed
    /// `Val::Px`. Shrinking a resizable window (or a window manager
    /// overriding the requested startup size) then left the column
    /// extending past the window's real bottom edge - content clipped by
    /// the *window* there is unreachable by any amount of `PageDown`, since
    /// `bevy_ui`'s scroll clamp only ever knows about the column's own
    /// (stale) box.
    ///
    /// Checked structurally, the same way the test above checks its own
    /// property, rather than by spinning up a real window and resizing it:
    /// the column's `height` must be `Val::Auto` with both `top` and
    /// `bottom` set. That is not an arbitrary stand-in for "tracks the
    /// window" - it is `taffy`'s (`bevy_ui`'s layout engine) own rule for an
    /// absolutely positioned box with both opposing insets set and no
    /// explicit size on that axis: the height is filled in from whatever
    /// the *current* window/viewport size is, every time `bevy_ui`'s layout
    /// system runs - the same rule every other `bottom: Val::Px(_)`-anchored
    /// panel in this module (the event log, the legend) already relies on
    /// to track the window's actual bottom edge. No system anywhere has to
    /// re-sample the window and write a new `Val::Px` for that to hold.
    ///
    /// Confirmed this fails against the pre-fix shape: give
    /// `spawn_right_column` back its old `window_height: f32` parameter and
    /// `height: Val::Px((window_height - TOP - BOTTOM_MARGIN).max(0.0))`
    /// (with no `bottom` field at all), and the first assertion below fails
    /// immediately - `Val::Px(754.0) != Val::Auto` for a `window_height` of
    /// `800.0` - because the height is a number baked in from whatever was
    /// passed at spawn time, not a window-relative constraint at all.
    #[test]
    fn right_column_height_is_window_relative_not_a_baked_constant() {
        let world_data = scenario::build_world();
        let font = Handle::<Font>::default();

        let mut world = World::new();
        let mut queue = CommandQueue::default();
        {
            let mut commands = Commands::new(&mut queue, &world);
            spawn_right_column(&mut commands, &font, &world_data, Some(FactionId(0)));
        }
        queue.apply(&mut world);

        let root = {
            let mut q = world.query_filtered::<Entity, With<RightColumnRoot>>();
            q.iter(&world).next().expect("spawn_right_column must spawn a RightColumnRoot entity")
        };
        let node = world.get::<Node>(root).expect("RightColumnRoot must have a Node");

        assert_eq!(
            node.height,
            Val::Auto,
            "the column's height must not be a baked Val::Px sampled from the window at spawn time - it must stay Val::Auto so bevy_ui computes it fresh from the *current* window size on every layout pass, not a snapshot taken once at startup"
        );
        assert_ne!(
            node.top,
            Val::Auto,
            "top must be a concrete inset - with height left Val::Auto, bevy_ui's absolute-layout rule only fills the height in from the window's current size when both top and bottom are set"
        );
        assert_ne!(
            node.bottom,
            Val::Auto,
            "bottom must be a concrete inset (not left at the default Val::Auto) - otherwise height has no second edge to be computed between and stays generically auto-sized to content instead of tracking the window"
        );
    }
}
