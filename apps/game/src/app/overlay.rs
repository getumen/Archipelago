//! Stage 7C's map overlays (docs/phase7-spec.md "Stage 7C — 詰める"), the
//! supply-network one now folded into `map_mode::MapMode::Supply` (`M` to
//! cycle to it, replacing the old standalone `L` toggle - see `map_mode`'s
//! own module doc for why: "a map-mode mechanism, not a pile of independent
//! toggles"):
//!
//! - the supply-network overlay (`MapMode::Supply`) - a ring around every
//!   region colored by how much of its own structural ceiling
//!   (`Region::node_throughput`) its actual `world.supply` entry realizes,
//!   plus every same-owner link recolored by whether it's the region's
//!   actual supply route, a saturated chokepoint, or merely supply-capable;
//! - in-progress construction markers (always on - docs/phase7-spec.md
//!   "3."); and
//! - blockaded-port markers, always on but deliberately quiet, plus the
//!   line to the sea zone responsible, revealed only on selection
//!   (docs/phase7-spec.md "2." - see "Blockade: quiet by default" below).
//!
//! Every system here only *reads* `SimRes`, exactly like `visuals` - see
//! that module's own doc for why that boundary matters.
//!
//! ## Visual hierarchy (salience matches importance)
//!
//! A saturated chokepoint is the single most decision-relevant thing this
//! overlay can show (docs/phase7-spec.md §2's entire reason for existing).
//! Two earlier passes at making it "loud" both backfired: coloring/
//! thickening the link alone reads fine zoomed in, but at the default
//! whole-map fitted view a short strait/tunnel segment shrinks to a
//! sub-pixel sliver no matter how many times its own thickness is
//! multiplied - thickness is a *link-space* signal, and link space keeps
//! shrinking as the camera zooms out. So a chokepoint gets two things: the
//! link itself still recolors to the purest, most saturated red used
//! anywhere here and renders thicker than its own kind's ordinary geometry
//! (`CHOKEPOINT_SCALE`) for a player already zoomed in, but the mark that
//! actually has to work at map scale is `ChokepointMarker` - a small
//! diamond at the link's midpoint, pre-spawned once per link
//! (`setup::setup`) and rescaled every frame to the camera's current
//! `Projection::scale` (`CHOKEPOINT_MARKER_RADIUS`'s own doc has the exact
//! math) so it renders at the *same screen-pixel size* regardless of zoom
//! or of how long or short the underlying link is. Routine active routes
//! recede to quiet, desaturated context by comparison (`COLOR_ACTIVE_ROUTE`).
//!
//! ## Blockade: quiet by default
//!
//! A blockade is standing state, not a per-frame event - unlike a
//! chokepoint, there is no moment-to-moment judgment call riding on
//! spotting it *immediately*. An earlier pass drew it as a thick ring
//! around the whole port region plus a permanent line to every causing sea
//! zone; on a map with several blockaded ports sharing one contested strait
//! (`japan47`'s 瀬戸内海 is the recurring case), those lines all converge on
//! one point and draw a starburst that buries the regions and labels near
//! it - the exact "amplification arms race" this design deliberately avoids
//! now. The port marker is a small solid dot off to one side of the region
//! (`BLOCKADE_MARKER_RADIUS`, positioned opposite `ConstructionMarker`'s own
//! offset so the two never collide) - present whenever `naval::
//! blockaded_ports` reports the port blockaded, but never louder than that.
//! The causal line only exists while the player has selected that region
//! (`SelectedRegion`) or that sea zone (`SelectedSeaZone`) -
//! `sync_blockade_visuals` reads both every frame it despawns/respawns the
//! blockade entities. Selecting is one click; the map stays legible until
//! a player actually asks the question "why is this port blockaded?".
//!
//! Every distinct meaning below gets its own hue, chosen so no two meanings
//! land on the same visual by accident: chokepoint is pure red, active
//! route is muted khaki, relay capacity is green, a dead relay is gray, the
//! supply ring runs rose→gray→green (with purple reserved for contested),
//! and a blockaded port is ice-blue - a hue nothing else here uses. Before
//! this pass, `COLOR_ACTIVE_ROUTE`/the old blockade marker/the ring's own
//! ~50%-fill blend all converged on the same saturated gold, so a gold mark
//! on the map couldn't say which of three unrelated things it meant. A
//! red-to-green ring blend in particular always passes through gold in
//! plain RGB interpolation (R and G both rising while B stays near zero) -
//! `RING_FULL`'s own doc has the fix. `setup::spawn_legend` keeps a small
//! always-present key to all of this, so nobody has to read this file to
//! know what a mark means.

use std::collections::BTreeMap;

use bevy::prelude::*;

use archipelago_sim::ids::{RegionId, SeaZoneId};
use archipelago_sim::logistics::{self, LinkThroughput, SupplyRegionRoute, SupplySource};
use archipelago_sim::naval::{self, PortBlockade};

use super::map_mode::{MapMode, MapModeRes};
use super::palette::Unit01;
use super::setup::{link_style, Z_LINK, Z_LINK_CHOKEPOINT, Z_LINK_HIGHLIGHT};
use super::{
    BlockadeVisual, ChokepointMarker, ConstructionMarker, CutLineMarker, LinkVisualMarker,
    MainCamera, RegionLayout, RegionRadii, SeaZoneCenters, SelectedRegion, SelectedSeaZone, SimRes,
    SupplyRingMarker,
};

/// Chokepoint tint - a link whose flow has reached its own `max_throughput`
/// in at least one direction it's actually used. See this module's own doc
/// for why this is deliberately the single most visually dominant mark the
/// overlay ever draws.
pub(super) const COLOR_CHOKEPOINT: Color = Color::srgb(1.0, 0.08, 0.05);
/// How much thicker than its own `LinkKind`'s ordinary thickness a
/// chokepoint segment renders, applied as `transform.scale.y` (the
/// pre-rotation mesh's thickness axis - see `setup::spawn_link`) rather
/// than a second mesh, so this works identically whether the underlying
/// link is solid or dashed.
const CHOKEPOINT_SCALE: f32 = 2.4;
/// Active-route tint - this link is the one a region's `world.supply`
/// currently comes through (`SupplySource::Relay`). Deliberately muted and
/// desaturated relative to `COLOR_CHOKEPOINT`: routine, working supply is
/// quiet context here, not the headline.
pub(super) const COLOR_ACTIVE_ROUTE: Color = Color::srgb(0.58, 0.52, 0.36);
/// A same-owner link that could relay but currently carries little -
/// blended toward `COLOR_RELAY_FULL` by how much of its own capacity it
/// actually uses.
const COLOR_RELAY_LOW: Color = Color::srgb(0.20, 0.30, 0.20);
pub(super) const COLOR_RELAY_FULL: Color = Color::srgb(0.35, 0.85, 0.35);
/// A same-owner link whose source can't currently relay at all (contested)
/// - present in the graph but structurally inert this instant.
const COLOR_RELAY_DEAD: Color = Color::srgb(0.30, 0.30, 0.33);
/// Stage 9D (docs/phase9-spec.md "5. クライアント": "遮断されている路線を区別
/// する"): every real `transport::TransportLine` between this region pair
/// has dropped to (essentially) zero effective capacity - a route severed by
/// `Action::InterdictLine`, sustained war damage, or devastation, as opposed
/// to `COLOR_RELAY_DEAD` (no transport line connects this pair at all) or a
/// merely idle-but-healthy one (`COLOR_RELAY_LOW`). A burnt copper/rust,
/// deliberately in the same "this route is a problem" warm-red family as
/// `COLOR_CHOKEPOINT` but shifted toward orange and away from its piercing
/// saturation, so the two read as related-but-distinct severities on the map
/// (a chokepoint is still carrying everything it can; a cut line is
/// carrying nothing) without either vanishing into the other. Bright enough
/// to double as this mode's own legend *text* color (`map_mode::
/// sync_mode_legend` colors a row's label with its swatch directly) -
/// confirmed by screenshot: an earlier, much darker candidate
/// (`srgb(0.32, 0.04, 0.04)`) read fine as a map line but was nearly
/// illegible as a legend row's label text against the panel's near-black
/// background.
pub(super) const COLOR_LINE_CUT: Color = Color::srgb(0.80, 0.35, 0.10);
/// How small a link's own summed `LinkThroughput::capacity()` has to be to
/// count as "cut" rather than merely "very constrained" - guards against
/// float noise in the flow allocation, the same role `SATURATION_EPSILON`
/// plays for the opposite (saturated) extreme.
const CUT_EPSILON: f32 = 1e-3;

/// The supply ring's low-fulfillment end - a warm rose, not pure red
/// (`COLOR_CHOKEPOINT` already owns pure red for this overlay's single most
/// urgent mark; reusing it here would blur the two). Paired with
/// `RING_FULL` below so their midpoint blend lands on neutral gray, not
/// gold - see that constant's own doc.
pub(super) const RING_STARVED: Color = Color::srgb(0.85, 0.20, 0.45);
/// The supply ring's full-fulfillment end, deliberately chosen alongside
/// `RING_STARVED` so `Color::mix`'s linear RGB blend between them lands on
/// a neutral gray at the midpoint rather than gold: both endpoints carry a
/// similar blue component, whereas a plain red→green blend (R and G both
/// rising, B near zero throughout) always reads as amber/gold at ~50% -
/// exactly the tone `COLOR_ACTIVE_ROUTE`/the blockade marker used to share,
/// which is what made a gold mark on the map ambiguous.
pub(super) const RING_FULL: Color = Color::srgb(0.20, 0.85, 0.55);
pub(super) const RING_CONTESTED: Color = Color::srgb(0.65, 0.25, 0.80);

/// Blockaded-port marker tint - a saturated ice-blue, deliberately in a hue
/// family nothing else this overlay draws uses (chokepoint is pure red,
/// active route is khaki, the supply ring runs rose→gray→green plus
/// contested purple, relay links are green/gray), and thematically apt: a
/// blockade is a *sea*-control effect (docs/phase7-spec.md "2. 制海権と
/// 封鎖"). Only ever drawn on a port `naval::blockaded_ports` reports
/// blockaded this instant - never on a region merely having a port.
pub(super) const BLOCKADE_MARKER_COLOR: Color = Color::srgb(0.15, 0.70, 0.95);
/// The blockaded-port marker's own radius - a small solid dot next to the
/// region, not a ring around it (this module's own doc, "Blockade: quiet
/// by default"). Deliberately close to `setup::CONSTRUCTION_MARKER_RADIUS`
/// so the two read as the same *kind* of thing (a small always-on status
/// dot) rather than one shouting louder than the other.
const BLOCKADE_MARKER_RADIUS: f32 = 5.0;
/// How far off the region's own circle the blockade dot sits, as a
/// fraction of the region's radius - mirrors `setup::setup`'s construction-
/// marker offset (`x + radius * 0.7`) but at the opposite corner
/// (`-radius * 0.7` on both axes) so a region that's simultaneously
/// blockaded and under construction never draws both dots on top of each
/// other.
const BLOCKADE_MARKER_OFFSET: f32 = 0.7;
/// Z for the blockade dot itself - above the supply ring/construction
/// marker's own layer, below region labels.
const Z_BLOCKADE_MARKER: f32 = 0.32;
/// Z for a blockade's causal line, revealed only on selection - still below
/// region labels so a revealed line never covers a name.
const Z_BLOCKADE_LINE: f32 = 0.28;
const BLOCKADE_LINE_THICKNESS: f32 = 2.5;

/// In-progress-construction marker tint - also referenced by the legend
/// (`setup::spawn_legend`) so its swatch always matches exactly what
/// `sync_construction_markers` paints.
pub(super) const CONSTRUCTION_TINT: Color = Color::srgb(1.0, 0.75, 0.20);

/// Every same-owner directed link's throughput this tick, keyed `(from,
/// to)` - built once per frame so both the ring and link recoloring below
/// share one read of `logistics::supply_link_flows` rather than each
/// recomputing it.
fn flow_index(world: &archipelago_sim::world::World) -> BTreeMap<(RegionId, RegionId), LinkThroughput> {
    logistics::supply_link_flows(world).into_iter().map(|f| ((f.from, f.to), f.throughput)).collect()
}

fn route_index(world: &archipelago_sim::world::World) -> BTreeMap<RegionId, SupplyRegionRoute> {
    logistics::supply_routes(world).into_iter().map(|r| (r.region, r)).collect()
}

/// The supply-overlay ring's color for one region - `None` while the
/// overlay is off (the ring stays hidden and its color is never read).
fn ring_color(route: &SupplyRegionRoute, node_throughput: f32) -> Color {
    if route.contested {
        return RING_CONTESTED;
    }
    let ratio = Unit01::new(if node_throughput > 0.0 { route.cap / node_throughput } else { 0.0 });
    RING_STARVED.mix(&RING_FULL, ratio.get())
}

/// `sync_supply_overlay`'s chokepoint-marker query, factored out purely to
/// keep that query's own type simple (clippy's `type_complexity`) - see its
/// `Without<...>` filter's own doc, inline below, for what it's for.
type ChokepointMarkerQuery = (&'static ChokepointMarker, &'static mut Visibility, &'static mut Transform);
type ChokepointMarkerFilter = (Without<LinkVisualMarker>, Without<SupplyRingMarker>);

/// The `CutLineMarker` counterpart of `ChokepointMarkerQuery`/`ChokepointMarkerFilter`
/// above - same reasoning, same shape, a different marker component.
type CutMarkerQuery = (&'static CutLineMarker, &'static mut Visibility, &'static mut Transform);
type CutMarkerFilter = (Without<LinkVisualMarker>, Without<SupplyRingMarker>, Without<ChokepointMarker>);

/// `sync_blockade_visuals`'s change-detection cache (External code review
/// fix A2), factored out for the same reason as `ChokepointMarkerQuery`
/// above (clippy's `type_complexity`): the exact state - `naval::
/// blockaded_ports`'s own result, plus which region/sea-zone is selected -
/// the currently-spawned `BlockadeVisual` entities were built from.
type BlockadeVisualState = (Vec<PortBlockade>, (Option<RegionId>, Option<SeaZoneId>));

/// Recolors every region's supply ring and every same-owner link's
/// geometry, shows/hides the rings, chokepoint markers and (Stage 9D)
/// cut-line markers, and rescales each marker to the camera's current zoom,
/// while `MapMode::Supply` is the active map mode (docs/phase7-spec.md "1.
/// 補給網の可視化", now reached via `M` instead of a standalone `L` toggle -
/// see `map_mode`'s own module doc). See this module's own doc ("Visual
/// hierarchy") for why the chokepoint marker's scale is tied to the camera
/// at all - `CutLineMarker` needs the exact same treatment for the exact
/// same reason.
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_supply_overlay(
    sim: Res<SimRes>,
    mode: Res<MapModeRes>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut rings: Query<(&SupplyRingMarker, &MeshMaterial2d<ColorMaterial>, &mut Visibility)>,
    mut links: Query<(&LinkVisualMarker, &MeshMaterial2d<ColorMaterial>, &mut Transform)>,
    // `Without<LinkVisualMarker>`/`Without<SupplyRingMarker>` aren't
    // topologically necessary (a `ChokepointMarker` entity never carries
    // either component - they're spawned separately in `setup::setup`) but
    // are required anyway: Bevy's query-conflict checker only considers a
    // query's own `With`/`Without` filters, not which components another
    // query's fetch happens to require, so without these this query's
    // shared `&mut Visibility`/`&mut Transform` writes against `rings`/
    // `links` above panic as B0001 at startup.
    mut chokepoints: Query<'_, '_, ChokepointMarkerQuery, ChokepointMarkerFilter>,
    mut cut_markers: Query<'_, '_, CutMarkerQuery, CutMarkerFilter>,
    camera: Query<&Projection, With<MainCamera>>,
) {
    let world = sim.0.world();

    if mode.0 != MapMode::Supply {
        for (_, _, mut visibility) in &mut rings {
            *visibility = Visibility::Hidden;
        }
        for (marker, material_handle, mut transform) in &mut links {
            let (_, color, _) = link_style(marker.kind);
            if let Some(mut mat) = materials.get_mut(&material_handle.0) {
                mat.color = color;
            }
            transform.translation.z = Z_LINK;
            // Undoes any `CHOKEPOINT_SCALE` thickness boost from the last
            // frame the overlay was on - otherwise a link that was a
            // chokepoint the moment `L` got pressed off would stay rendered
            // thicker than its own `LinkKind` forever after.
            transform.scale = Vec3::ONE;
        }
        for (_, mut visibility, _) in &mut chokepoints {
            *visibility = Visibility::Hidden;
        }
        for (_, mut visibility, _) in &mut cut_markers {
            *visibility = Visibility::Hidden;
        }
        return;
    }

    let routes = route_index(world);
    let flows = flow_index(world);

    for (marker, material_handle, mut visibility) in &mut rings {
        *visibility = Visibility::Visible;
        let Some(route) = routes.get(&marker.0) else { continue };
        let node_throughput = world.region(marker.0).node_throughput();
        if let Some(mut mat) = materials.get_mut(&material_handle.0) {
            mat.color = ring_color(route, node_throughput);
        }
    }

    // One entry per link pair this frame - shared below by both the link
    // recoloring loop and the chokepoint-marker loop, so "is this link a
    // chokepoint right now" is computed exactly once per pair rather than
    // twice (the marker's `(a, b)` always matches its link's, since
    // `setup::setup` pre-spawns both from the same loop iteration).
    let mut chokepoint_pairs: BTreeMap<(RegionId, RegionId), bool> = BTreeMap::new();
    // Same shape as `chokepoint_pairs`, for `CutLineMarker` below - "is this
    // link cut right now", computed once here and reused rather than
    // recomputed in the marker loop.
    let mut cut_pairs: BTreeMap<(RegionId, RegionId), bool> = BTreeMap::new();

    for (marker, material_handle, mut transform) in &mut links {
        let a = marker.a;
        let b = marker.b;
        let same_owner = world.region(a).owner == world.region(b).owner;
        // Chokepoint/active-route links are lifted above every region's own
        // fill (`Z_LINK_HIGHLIGHT`) - on a dense map (japan47) two adjacent
        // regions' circles can otherwise bury a short link almost entirely,
        // which would defeat the entire point of flagging it
        // (docs/phase7-spec.md "1.": "明示する" - "clearly indicated", not
        // "technically present in the data"). Anything else sits at the
        // ordinary below-regions `Z_LINK`, unchanged from Stage 7A.
        // The third element is `CHOKEPOINT_SCALE` for a chokepoint, `1.0`
        // otherwise - applied to `transform.scale.y` below, the mesh's own
        // pre-rotation thickness axis (`setup::spawn_link`), so a
        // chokepoint always renders thicker than its own `LinkKind`'s
        // ordinary geometry regardless of whether that geometry is a solid
        // `Rail`/`Road` or a dashed `Tunnel`/`Strait` segment - this
        // overlay's single most decision-relevant mark must never end up
        // *thinner* than an ordinary route sitting right next to it.
        let (color, z, scale_y) = if !same_owner {
            (link_style(marker.kind).1, Z_LINK, 1.0)
        } else {
            let flow_ab = flows.get(&(a, b)).copied();
            let flow_ba = flows.get(&(b, a)).copied();
            let is_chokepoint = flow_ab.is_some_and(LinkThroughput::is_saturated) || flow_ba.is_some_and(LinkThroughput::is_saturated);
            chokepoint_pairs.insert((a, b), is_chokepoint);
            let is_active_route = routes.get(&b).map(|r| r.source) == Some(SupplySource::Relay(a))
                || routes.get(&a).map(|r| r.source) == Some(SupplySource::Relay(b));
            // Stage 9D: a line present in the graph (so not `COLOR_RELAY_DEAD`
            // below - that means "no transport line connects this pair at
            // all") whose own summed capacity has dropped to (essentially)
            // zero - severed rather than merely idle. Every `TransportLine`
            // between this pair has to be this reduced (`supply_link_flows`
            // sums every line's capacity for the pair), not just one of
            // several parallel routes, so this never fires while a healthy
            // alternate route still keeps the pair usable.
            let is_cut = flow_ab.is_some_and(|t| t.capacity() <= CUT_EPSILON) || flow_ba.is_some_and(|t| t.capacity() <= CUT_EPSILON);
            cut_pairs.insert((a, b), is_cut);
            if is_chokepoint {
                (COLOR_CHOKEPOINT, Z_LINK_CHOKEPOINT, CHOKEPOINT_SCALE)
            } else if is_cut {
                (COLOR_LINE_CUT, Z_LINK_HIGHLIGHT, 1.0)
            } else if is_active_route {
                (COLOR_ACTIVE_ROUTE, Z_LINK_HIGHLIGHT, 1.0)
            } else if let (None, None) = (flow_ab, flow_ba) {
                (COLOR_RELAY_DEAD, Z_LINK, 1.0)
            } else {
                let ratio = flow_ab.map(LinkThroughput::ratio).unwrap_or(0.0).max(flow_ba.map(LinkThroughput::ratio).unwrap_or(0.0));
                (COLOR_RELAY_LOW.mix(&COLOR_RELAY_FULL, Unit01::new(ratio).get()), Z_LINK, 1.0)
            }
        };
        if let Some(mut mat) = materials.get_mut(&material_handle.0) {
            mat.color = color;
        }
        transform.translation.z = z;
        transform.scale = Vec3::new(1.0, scale_y, 1.0);
    }

    // Screen-space-constant chokepoint markers - `CHOKEPOINT_MARKER_RADIUS`'s
    // own doc has the exact reasoning: multiplying the pre-spawned mesh's
    // `Transform::scale` by the camera's own current `Projection::scale`
    // exactly cancels that same factor in Bevy's world-to-screen mapping, so
    // the marker always covers the same number of screen pixels regardless
    // of zoom. Falls back to `1.0` on the one frame before `camera_fit::
    // fit_camera_to_map` has ever run (no `Projection::Orthographic` read
    // yet) - a visibly-wrong marker size for a single frame beats panicking.
    let zoom = match camera.single() {
        Ok(Projection::Orthographic(ortho)) => ortho.scale,
        _ => 1.0,
    };
    for (marker, mut visibility, mut transform) in &mut chokepoints {
        let is_chokepoint = chokepoint_pairs.get(&(marker.a, marker.b)).copied().unwrap_or(false);
        *visibility = if is_chokepoint { Visibility::Visible } else { Visibility::Hidden };
        transform.scale = Vec3::splat(zoom);
    }
    for (marker, mut visibility, mut transform) in &mut cut_markers {
        let is_cut = cut_pairs.get(&(marker.a, marker.b)).copied().unwrap_or(false);
        *visibility = if is_cut { Visibility::Visible } else { Visibility::Hidden };
        transform.scale = Vec3::splat(zoom);
    }
}

/// Shows/hides and fills each region's in-progress-construction marker -
/// always on, not gated by `MapMode` (docs/phase7-spec.md "3. 戦災と
/// 復興").
pub(super) fn sync_construction_markers(
    sim: Res<SimRes>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut query: Query<(&ConstructionMarker, &MeshMaterial2d<ColorMaterial>, &mut Visibility)>,
) {
    let world = sim.0.world();
    for (marker, material_handle, mut visibility) in &mut query {
        let region = world.region(marker.0);
        match &region.construction {
            None => *visibility = Visibility::Hidden,
            Some(construction) => {
                *visibility = Visibility::Visible;
                let progress = Unit01::new(if construction.required > 0.0 { construction.invested / construction.required } else { 0.0 });
                if let Some(mut mat) = materials.get_mut(&material_handle.0) {
                    // `CONSTRUCTION_TINT`, growing more opaque as the
                    // project nears completion - a marker that just
                    // appeared vs. one about to finish both read as "under
                    // construction", but the second reads as further along.
                    mat.color = CONSTRUCTION_TINT.with_alpha(0.35 + 0.55 * progress.get());
                }
            }
        }
    }
}

/// Despawns and respawns every blockade marker/line for whatever
/// `naval::blockaded_ports` reports right now - always on, not gated by
/// `MapMode` (docs/phase7-spec.md "2. 制海権と封鎖": "封鎖されている港
/// を地図上に明示する。どの海域の制海権が原因かを結ぶ"). Which ports are
/// blockaded changes at runtime, unlike the static region/link graph the
/// rest of this crate's map geometry pre-spawns once - see `BlockadeVisual`'s
/// own doc.
///
/// External code review fix A2 (P2): this used to despawn and rebuild every
/// entity unconditionally, every single frame, for as long as *any* port
/// stayed blockaded - continuous ECS/asset/render churn proportional to the
/// blockade count for the entire rest of a multi-minute session, not just
/// the frame something actually changed. `last` (a `Local`, so it persists
/// frame to frame the same way `map_mode::sync_mode_legend`'s own dispatch
/// on the active `MapModeRes` does for its simpler case) caches the exact state the current
/// entities were built from - `naval::blockaded_ports`'s own result plus
/// which region/sea-zone is selected, since selection changes which causal
/// lines are revealed (see this module's own doc, "Blockade: quiet by
/// default") - and the whole rebuild below is skipped whenever nothing in
/// it has actually moved since last frame.
///
/// The port marker itself is always spawned for every blockaded port; the
/// line to a causing sea zone is spawned only while the player has that
/// region or that sea zone selected - see this module's own doc ("Blockade:
/// quiet by default") for why.
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_blockade_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    sim: Res<SimRes>,
    layout: Res<RegionLayout>,
    radii: Res<RegionRadii>,
    sea_centers: Res<SeaZoneCenters>,
    selected_region: Res<SelectedRegion>,
    selected_sea_zone: Res<SelectedSeaZone>,
    existing: Query<Entity, With<BlockadeVisual>>,
    mut last: Local<Option<BlockadeVisualState>>,
) {
    let world = sim.0.world();
    let blockades = naval::blockaded_ports(world);
    let selection = (selected_region.0, selected_sea_zone.0);

    let unchanged = last.as_ref().is_some_and(|(prev_blockades, prev_selection)| {
        *prev_blockades == blockades && *prev_selection == selection
    });
    if unchanged {
        return; // Nothing a rebuild would change - leave last frame's entities alone.
    }
    *last = Some((blockades.clone(), selection));

    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if blockades.is_empty() {
        return;
    }

    for blockade in &blockades {
        let [rx, ry] = layout.0[blockade.region.index()];
        let radius = radii.0[blockade.region.index()];

        // A small solid dot off to one side of the port region - deliberately
        // not a ring around it and not connected to anything by default
        // (`BLOCKADE_MARKER_RADIUS`'s own doc has why). `BLOCKADE_MARKER_COLOR`'s
        // own doc has why this uses a hue nothing else here does.
        commands.spawn((
            Mesh2d(meshes.add(Circle::new(BLOCKADE_MARKER_RADIUS))),
            MeshMaterial2d(materials.add(ColorMaterial::from_color(BLOCKADE_MARKER_COLOR))),
            Transform::from_xyz(rx - radius * BLOCKADE_MARKER_OFFSET, ry - radius * BLOCKADE_MARKER_OFFSET, Z_BLOCKADE_MARKER),
            BlockadeVisual,
        ));

        // A line to each sea zone actually responsible - only while the
        // player has asked, by selecting the port region or that sea zone.
        let revealed =
            selected_region.0 == Some(blockade.region) || blockade.causes.iter().any(|&zone| selected_sea_zone.0 == Some(zone));
        if !revealed {
            continue;
        }
        for &zone in &blockade.causes {
            let [zx, zy] = sea_centers.0[zone.index()];
            let dx = zx - rx;
            let dy = zy - ry;
            let length = (dx * dx + dy * dy).sqrt();
            if length <= 0.0 {
                continue;
            }
            let angle = dy.atan2(dx);
            commands.spawn((
                Mesh2d(meshes.add(Rectangle::new(length, BLOCKADE_LINE_THICKNESS))),
                MeshMaterial2d(materials.add(ColorMaterial::from_color(BLOCKADE_MARKER_COLOR))),
                Transform::from_xyz((rx + zx) / 2.0, (ry + zy) / 2.0, Z_BLOCKADE_LINE).with_rotation(Quat::from_rotation_z(angle)),
                BlockadeVisual,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    use archipelago_sim::ids::RegionId;

    use crate::sim_driver::SimDriver;

    /// The embedded mvp scenario, with faction 1 given full hostile control
    /// of the one sea zone touching region 0 (`hokkaido`, faction 0's own
    /// port) - `Diplomacy::new` starts every faction pair at `War`, so this
    /// alone is enough to trip `naval::is_port_blockaded`
    /// (`BLOCKADE_CONTROL_THRESHOLD`).
    fn blockaded_world() -> archipelago_sim::world::World {
        let mut world = archipelago_sim::scenario::build_world();
        let mut control = vec![0.0; world.factions.len()];
        control[1] = 1.0;
        world.sea_zones[0].control = control;
        assert!(naval::is_port_blockaded(&world, RegionId(0)), "fixture must actually blockade region 0");
        world
    }

    fn setup(world: &mut World, sim_world: archipelago_sim::world::World) {
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<ColorMaterial>>();
        // Distinct, non-coincident positions per region/zone - the causal
        // line `sync_blockade_visuals` draws is skipped outright when its
        // two endpoints land on the same point (`length <= 0.0`, guarding
        // against a zero-length mesh), which an all-zero placeholder layout
        // would trip on every single region/zone pair.
        world.insert_resource(RegionLayout((0..sim_world.regions.len()).map(|i| [i as f32 * 100.0, 0.0]).collect()));
        world.insert_resource(RegionRadii(sim_world.regions.iter().map(|r| super::super::setup::region_radius(r.population)).collect()));
        world.insert_resource(SeaZoneCenters((0..sim_world.sea_zones.len()).map(|i| [i as f32 * 100.0, 500.0]).collect()));
        world.insert_resource(SelectedRegion(None));
        world.insert_resource(SelectedSeaZone(None));
        world.insert_resource(SimRes(SimDriver::new(sim_world, 1)));
    }

    fn blockade_entities(world: &mut World) -> BTreeSet<Entity> {
        let mut query = world.query_filtered::<Entity, With<BlockadeVisual>>();
        query.iter(world).collect()
    }

    /// External code review fix A2 (P2): while a port stays blockaded,
    /// `sync_blockade_visuals` used to despawn every blockade entity and
    /// allocate fresh mesh/material assets *every single frame*, forever,
    /// for as long as the blockade lasted - continuous ECS/asset/render
    /// churn proportional to the blockade count for the rest of a session.
    /// Two consecutive runs with nothing changed must now leave the exact
    /// same entities in place: a despawn followed by a respawn always
    /// produces *different* `Entity` ids even when the visual result looks
    /// identical, so comparing the id sets directly proves whether a
    /// rebuild actually happened, not just whether the outcome looks right.
    #[test]
    fn unchanged_blockade_state_does_not_respawn_entities() {
        let mut world = World::new();
        setup(&mut world, blockaded_world());

        let mut system = IntoSystem::into_system(sync_blockade_visuals);
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();

        let after_first = blockade_entities(&mut world);
        assert!(!after_first.is_empty(), "a blockaded port must produce at least the marker entity");

        system.run((), &mut world).unwrap();
        let after_second = blockade_entities(&mut world);

        assert_eq!(after_first, after_second, "an unchanged blockade/selection state must not despawn or respawn any entity");
    }

    /// The other half of the same fix: a real change (here, selecting the
    /// blockaded region, which reveals its causal line - this module's own
    /// doc, "Blockade: quiet by default") must still rebuild - the cache
    /// must never get stuck showing stale entities.
    #[test]
    fn changed_selection_does_respawn_entities() {
        let mut world = World::new();
        setup(&mut world, blockaded_world());

        let mut system = IntoSystem::into_system(sync_blockade_visuals);
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();
        let before = blockade_entities(&mut world);

        world.resource_mut::<SelectedRegion>().0 = Some(RegionId(0));
        system.run((), &mut world).unwrap();
        let after = blockade_entities(&mut world);

        assert_ne!(before, after, "selecting the blockaded region must rebuild the blockade visuals");
        assert!(after.len() > before.len(), "revealing the causal line should add entities, not just replace them 1:1");
    }
}
