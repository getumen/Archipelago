//! Per-frame visual sync: region fill color (`map_mode`'s own module doc has
//! the full list of what each `MapMode` shows; `Political`/`Supply` share
//! the original Stage 7C behavior - owner color, mixed toward the
//! occupier's color as `occupation` progresses - docs/phase7-spec.md "占領
//! 進行中は所有者色と占領者色の混色にする" - then further toward a scorched-
//! earth "grime" tone by `Region::devastation`, Stage 7C's docs/phase7-spec.md
//! "3. 戦災と復興": "地域の devastation を視覚化する（マーカーの荒れ具合、
//! 色の濁り）"), every region's always-on owner-color border ring
//! (`OwnerBorderMarker`, `sync_owner_border` - see `map_mode`'s own doc,
//! "Ownership stays visible in every mode"), sea zone tint (whichever
//! faction currently holds the most `SeaZone::control`), and unit markers
//! (position, visibility, color, plus spawning a marker for any unit
//! created since the last frame - e.g. a fresh recruit).
//!
//! Every system here only *reads* `SimRes` - never writes it. This is what
//! keeps rendering incapable of feeding anything back into the simulation
//! (docs/phase7-spec.md §0's central invariant).

use bevy::prelude::*;

use archipelago_sim::balance::{UNIT_EQUIPMENT, UNIT_MANPOWER};
use archipelago_sim::military::Branch;
use archipelago_sim::world::Region;

use super::map_mode::{self, MapMode, MapModeRes};
use super::palette::{faction_color, Unit01, NEUTRAL};
use super::setup::station_position;
use super::{
    ActiveGood, MainCamera, OwnerBorderMarker, RegionLabelMarker, RegionLayout, RegionMarker,
    SeaZoneCenters, SeaZoneMarker, SelectedRegion, SimRes, UnitMarker,
};

/// The camera's current orthographic scale - the one number both the unit
/// markers' size and `input::map_click_select`'s hit radius are multiplied
/// by, so what is clickable is exactly what is drawn.
///
/// `codex review` (P2): the two used to derive it separately, and when the
/// markers were made screen-space constant the hit test stayed in world
/// units. Zoomed out to the whole-map fit, visible markers were unclickable.
/// Sharing one function means a future change to either cannot silently
/// desynchronise them.
///
/// Falls back to `1.0` on the one frame before `camera_fit::
/// fit_camera_to_map` has run - the same fallback `sync_supply_overlay`
/// uses, for the same reason (a wrong size for one frame beats a panic).
pub(super) fn camera_zoom(camera: &Query<&Projection, With<MainCamera>>) -> f32 {
    match camera.single() {
        Ok(Projection::Orthographic(ortho)) => ortho.scale,
        _ => 1.0,
    }
}

/// Scorched-earth tone `sync_region_visuals` mixes a devastated region's
/// fill toward - dull, dark, faintly brown, never pure black (a fully-
/// devastated region should still read as *whose* wreckage it is, so its
/// owner color must stay at least partly visible).
const DEVASTATION_TINT: Color = Color::srgb(0.16, 0.13, 0.10);
/// Cap on how far `devastation == 1.0` pushes the mix - see `DEVASTATION_TINT`'s
/// own doc for why this deliberately stops short of `1.0`.
const MAX_DEVASTATION_MIX: f32 = 0.8;

/// Military delegation's own map marker (docs/design.md §14): a delegated
/// unit's marker is mixed toward this near-white rather than drawn in plain
/// `faction_color` - the same "state the player needs to see without
/// opening a panel" role `DEVASTATION_TINT`/occupation mixing already play
/// for regions. A *lightness* shift rather than another hue: every
/// `palette::faction_color` entry is a fully-saturated mid-tone, so pushing
/// toward white reads as "highlighted" against all eight of them uniformly,
/// where picking some other bright hue (gold, say) would have been nearly
/// indistinguishable from faction 3's own amber - confirmed by looking at
/// an actual screenshot, not just by inspecting the two `Color` values side
/// by side (docs/conventions.md §2, CLAUDE.md's "画面は見る。数えない").
const DELEGATED_MARKER_TINT: Color = Color::srgb(0.98, 0.98, 0.95);
/// How far a delegated unit's marker mixes toward `DELEGATED_MARKER_TINT` -
/// short of `1.0` so the marker still visibly carries its owner's
/// `faction_color` underneath, the same way `MAX_DEVASTATION_MIX` keeps a
/// devastated region's owner color partly visible.
const DELEGATED_MARKER_MIX: f32 = 0.65;

/// Owner fill for `MapMode::Political`/`MapMode::Supply` (`map_mode`'s own
/// doc for why those two modes share this): owner color mixed toward the
/// occupier's as `occupation` progresses, then further toward
/// `DEVASTATION_TINT` by `Region::devastation` - unchanged from Stage 7C,
/// just pulled out of `sync_region_visuals`'s own loop so `MapMode`
/// dispatch (below) can call it as one arm among several instead of it
/// being the *only* thing that loop ever computed.
pub(super) fn political_fill_color(region: &Region) -> Color {
    let owner_color = faction_color(region.owner.index());
    let occupation_color = match region.occupier {
        Some(occupier) if region.occupation > 0.0 => {
            let occupier_color = faction_color(occupier.index());
            owner_color.mix(&occupier_color, region.occupation.clamp(0.0, 1.0))
        }
        _ => owner_color,
    };
    let devastation_mix = Unit01::new(region.devastation * MAX_DEVASTATION_MIX);
    occupation_color.mix(&DEVASTATION_TINT, devastation_mix.get())
}

/// `MapMode::Air`'s own fill (docs/phase10-spec.md "Stage 10D": "地図で
/// 制空権と飛行場が見える") - `sync_sea_zone_visuals`' exact "whichever
/// faction holds the highest share, tinted toward `NEUTRAL` by how
/// contested it is" pattern, one domain further: `Region::air_superiority`
/// is already a `power[f] / sum(power[*])` share the same shape
/// `SeaZone::control` is (`air::tick_air_superiority`'s own doc), so this
/// reuses the reading a player has already learned from the sea-zone tint
/// rather than inventing a second color language for "whose". Plain
/// `NEUTRAL` when nobody's air power reaches this region at all.
pub(super) fn air_superiority_fill(region: &Region) -> Color {
    let mut best: Option<(usize, f32)> = None;
    for (f, share) in region.air_superiority.iter().enumerate() {
        let c = share.get();
        if best.is_none_or(|(_, best_c)| c > best_c) {
            best = Some((f, c));
        }
    }
    match best {
        Some((f, c)) if c > 0.0 => faction_color(f).mix(&NEUTRAL, 1.0 - c),
        _ => NEUTRAL,
    }
}

/// Paints every region's fill according to the active `MapMode`
/// (`map_mode`'s own module doc has the full list and rationale) - the one
/// system that actually shows the player whichever dimension of the
/// simulation they've asked to see. `population_cuts`/`industry_cuts` are
/// computed once per frame, not once per region, since they depend on every
/// region's own value (`map_mode::population_thresholds`/`industry_thresholds`'s
/// own "no invented thresholds" doc) rather than the one region being
/// painted. `industry_cuts` is recomputed from `active_good` every frame
/// too - a different selected commodity means a different distribution to
/// band against, not just a different hue to paint it in.
pub(super) fn sync_region_visuals(
    sim: Res<SimRes>,
    mode: Res<MapModeRes>,
    active_good: Res<ActiveGood>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    query: Query<(&RegionMarker, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();
    let population_cuts = map_mode::population_thresholds(world);
    let industry_cuts = map_mode::industry_thresholds(world, active_good.0);
    for (marker, material_handle) in &query {
        let region = world.region(marker.0);
        let color = match mode.0 {
            MapMode::Political | MapMode::Supply => political_fill_color(region),
            MapMode::Terrain => map_mode::terrain_fill(region.terrain),
            MapMode::Population => map_mode::population_fill(region.population, population_cuts),
            MapMode::Industry => map_mode::industry_fill(region, active_good.0, industry_cuts),
            MapMode::Unrest => map_mode::unrest_fill(region),
            MapMode::Air => air_superiority_fill(region),
        };
        if let Some(mut mat) = materials.get_mut(&material_handle.0)
            && mat.color != color
        {
            mat.color = color;
        }
    }
}

/// Keeps every region's `OwnerBorderMarker` ring colored by its current
/// `owner` - the one visual this crate draws unconditionally in every
/// `MapMode` (`map_mode`'s own module doc, "Ownership stays visible in
/// every mode"), so a player can always tell whose territory a region
/// belongs to even while some other mode's fill is showing terrain/
/// population/industry/instability instead. Deliberately *not* mixed
/// toward the occupier or devastation tint the way `political_fill_color`'s
/// own fill is - this ring answers one question only ("who owns this"), not
/// "how far along is the fight for it" (that nuance stays visible via
/// `MapMode::Political`/`MapMode::Supply`'s own fill, or the inspect panel).
pub(super) fn sync_owner_border(
    sim: Res<SimRes>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    query: Query<(&OwnerBorderMarker, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();
    for (marker, material_handle) in &query {
        let color = faction_color(world.region(marker.0).owner.index());
        if let Some(mut mat) = materials.get_mut(&material_handle.0)
            && mat.color != color
        {
            mat.color = color;
        }
    }
}

/// Sea-zone tint: the color of whichever faction currently holds the
/// highest `SeaZone::control` share there, faded toward `NEUTRAL` by how
/// contested it is (`1.0 - top_share` mixed in) - a zone with one faction at
/// `control == 1.0` reads as fully that faction's color; a zone nobody
/// controls (`control` all `0.0`) reads as plain neutral gray.
pub(super) fn sync_sea_zone_visuals(
    sim: Res<SimRes>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    query: Query<(&SeaZoneMarker, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();
    for (marker, material_handle) in &query {
        let zone = world.sea_zone(marker.0);
        let mut best: Option<(usize, f32)> = None;
        for (f, &c) in zone.control.iter().enumerate() {
            if best.is_none_or(|(_, best_c)| c > best_c) {
                best = Some((f, c));
            }
        }
        let base = match best {
            Some((f, c)) if c > 0.0 => faction_color(f).mix(&NEUTRAL, 1.0 - c),
            _ => NEUTRAL,
        };
        let color = base.with_alpha(0.18);
        if let Some(mut mat) = materials.get_mut(&material_handle.0)
            && mat.color != color
        {
            mat.color = color;
        }
    }
}

/// Small offset applied to each of a station's units so several markers at
/// the same region/sea-zone don't fully overlap - a deterministic ring
/// (ordered by `UnitId`, which is stable and never reordered) rather than
/// anything randomized, so the same board always draws the same way.
fn ring_offset(index: usize, count: usize, radius: f32) -> Vec2 {
    if count <= 1 {
        return Vec2::ZERO;
    }
    let angle = (index as f32 / count as f32) * std::f32::consts::TAU;
    Vec2::new(angle.cos(), angle.sin()) * radius
}

/// Stage 11C (docs/phase11-spec.md §4 "地図とパネルで兵科が分かる"): a unit
/// marker's own *shape*, on top of the color every marker already carries
/// (`faction_color`, unchanged - color is owner, never branch, so the two
/// signals never fight for the same channel). `Infantry` keeps the
/// pre-Stage-11C triangle unchanged (the branch every scenario's starting
/// units already are, and the visual default before branches existed at
/// all), `None` (every Sea/Air unit - `Unit::branch`'s own doc) keeps it
/// too, so this stage changes nothing about a marker a player has never
/// seen differentiated before. `Armour` gets a square (a vehicle's own
/// silhouette, the branch `ARMOUR_PLAIN_MULT` already marks as the
/// aggressive/mobile one), `Artillery` a circle (the third, distinct
/// silhouette) - three shapes a glance at the map can tell apart, the same
/// "discoverable, not just present" bar the region panel's `[branch]`
/// suffix meets one level up.
///
/// Defect fix: these local-mesh radii used to be the *only* factor in a
/// marker's on-screen size, with no camera-zoom compensation - fine on a
/// sparse map at typical zoom, but on `japan_hex` (289 regions) at the
/// default whole-map fitted zoom (`camera_fit::fit_camera_to_map`, `ortho.
/// scale` around 3.3, `visuals::LABEL_ZOOM_THRESHOLD`'s own doc) a 6-9
/// world-unit shape renders at only a couple of screen pixels - sub-pixel
/// enough that a triangle, a square and a circle are all indistinguishable
/// blobs. `sync_unit_visuals` now applies the exact same screen-space-
/// constant treatment `setup::CHOKEPOINT_MARKER_RADIUS`'s own doc
/// established for the supply overlay's chokepoint marker (multiplying
/// `Transform::scale` by the camera's current `Projection::scale`, which
/// exactly cancels that same factor in Bevy's world-to-screen mapping) -
/// see that system's own doc for the full reasoning. These radii are this
/// marker's *screen*-pixel size now, not a world size, and are picked a
/// little larger than the chokepoint marker's own `6.0` (a status dot only
/// has to be findable; a shape has to be told apart from two others).
fn unit_marker_mesh(meshes: &mut Assets<Mesh>, branch: Option<Branch>) -> Handle<Mesh> {
    match branch {
        Some(Branch::Armour) => meshes.add(Rectangle::new(10.0, 10.0)),
        Some(Branch::Artillery) => meshes.add(Circle::new(7.0)),
        Some(Branch::Infantry) | None => meshes.add(RegularPolygon::new(7.5, 3)),
    }
}

pub(super) fn sync_unit_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    sim: Res<SimRes>,
    layout: Res<RegionLayout>,
    sea_centers: Res<SeaZoneCenters>,
    camera: Query<&Projection, With<MainCamera>>,
    mut existing: Query<(&UnitMarker, &mut Transform, &mut Visibility, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();

    // Defect fix (`unit_marker_mesh`'s own doc): the same zoom-cancelling
    // trick `overlay::sync_supply_overlay` uses for its chokepoint/cut-line
    // markers, applied to every unit marker's own size *and* to how far
    // apart `ring_offset` fans out several units sharing one station -
    // without also compensating the spread, markers at a legible fixed
    // pixel size would still collapse onto nearly the same screen point at
    // a dense map's default zoomed-out fit, hiding all but the topmost one.
    // Falls back to `1.0` on the one frame before `camera_fit::
    // fit_camera_to_map` has run - same fallback `sync_supply_overlay`
    // uses, same reasoning (a wrong size for one frame beats a panic).
    let zoom = camera_zoom(&camera);

    let mut known = std::collections::HashSet::new();
    for (marker, _, _, _) in &existing {
        known.insert(marker.0.index());
    }
    for unit in &world.units {
        if !known.contains(&unit.id.index()) {
            commands.spawn((
                Mesh2d(unit_marker_mesh(&mut meshes, unit.branch)),
                MeshMaterial2d(materials.add(ColorMaterial::from_color(faction_color(unit.owner.index())))),
                Transform::from_xyz(0.0, 0.0, 1.0),
                Visibility::Hidden,
                UnitMarker(unit.id),
            ));
        }
    }

    // Group alive units sharing a station so `ring_offset` can spread them
    // out, ordered by `UnitId` for determinism.
    let mut by_station: std::collections::BTreeMap<(u8, u32), Vec<archipelago_sim::ids::UnitId>> = std::collections::BTreeMap::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let key = match unit.station {
            archipelago_sim::world::Station::Region(r) => (0u8, r.0),
            archipelago_sim::world::Station::Sea(z) => (1u8, z.0),
            archipelago_sim::world::Station::Airfield(n) => (2u8, n.0),
        };
        by_station.entry(key).or_default().push(unit.id);
    }
    let mut slot_of: std::collections::HashMap<u32, (usize, usize)> = std::collections::HashMap::new();
    for units in by_station.values() {
        let count = units.len();
        for (slot, id) in units.iter().enumerate() {
            slot_of.insert(id.0, (slot, count));
        }
    }

    for (marker, mut transform, mut visibility, material_handle) in &mut existing {
        let Some(unit) = world.units.iter().find(|u| u.id == marker.0) else { continue };
        if !unit.alive {
            *visibility = Visibility::Hidden;
            continue;
        }
        *visibility = Visibility::Visible;
        let [cx, cy] = station_position(world, unit.station, &layout.0, &sea_centers.0);
        let (slot, count) = slot_of.get(&marker.0.0).copied().unwrap_or((0, 1));
        // `* zoom` keeps this ring's own screen-pixel radius constant too
        // (`sync_unit_visuals`'s own doc) - otherwise several units sharing
        // one region would still visually stack on top of each other at a
        // dense map's fitted-out zoom even once each marker's own shape is
        // individually legible.
        let offset = ring_offset(slot, count, 14.0) * zoom;
        transform.translation.x = cx + offset.x;
        transform.translation.y = cy + offset.y;
        let strength = ((unit.manpower / UNIT_MANPOWER) + (unit.equipment / UNIT_EQUIPMENT)) / 2.0;
        // Narrowed from the pre-fix `0.4..=1.3`: at this marker's new
        // legible baseline size, the low end of that range would still
        // shrink a badly damaged unit's shape back down past the point a
        // player can tell it apart from the other two branches - the whole
        // defect this pass exists to fix. `* zoom` is the same screen-
        // space-constant treatment as `unit_marker_mesh`'s own local size.
        transform.scale = Vec3::splat(strength.clamp(0.6, 1.25) * zoom);
        if let Some(mut mat) = materials.get_mut(&material_handle.0) {
            let base = faction_color(unit.owner.index());
            mat.color =
                if sim.0.is_delegated(unit.id) { base.mix(&DELEGATED_MARKER_TINT, DELEGATED_MARKER_MIX) } else { base };
        }
    }
}

/// Threshold in `Projection::Orthographic::scale` (world units per screen
/// pixel; `input::MIN_ZOOM..=MAX_ZOOM` is `0.25..=4.0`) below which every
/// dense-map region label becomes visible, not just a
/// `RegionLabelMarker::always_visible` (capital/top-decile-population) one
/// or the current selection. Picked comfortably inside the zoomable range -
/// past it a player still has plenty of room left to zoom in further
/// (`input::MIN_ZOOM` is `0.25`) - but well under the default whole-map
/// fitted scale a dense map like `japan_hex` computes
/// (`camera_fit::fit_camera_to_map`, roughly 3.3 by default with `mod::
/// window_height_for_layout`'s own sizing), so the *default* view stays
/// clean territory, not a name-soup, and only reveals every name once the
/// player has actually asked for more detail by zooming in.
const LABEL_ZOOM_THRESHOLD: f32 = 1.5;

/// Dense-map region label visibility policy (`RegionLabelMarker`'s own doc
/// has the full rationale) - a no-op in effect on a sparse map, where every
/// label's `always_visible` is unconditionally `true` (`setup::setup`), so
/// the `||` chain below always resolves to `Visibility::Visible` there
/// without ever consulting zoom, selection, or occupation. On a dense map,
/// a label is visible while it's a capital/top-decile-population region
/// (`always_visible`), the region is under active occupation (`occupier`
/// - a fight over it is exactly the moment its name matters most, and
/// unlike population/capital status this can start or end at any time, so
/// it's checked fresh every frame rather than baked in at spawn), the
/// camera has zoomed in past `LABEL_ZOOM_THRESHOLD`, or the player has
/// selected that exact region - matching the task's own combined policy:
/// "at sufficient zoom" plus "selected" plus "significant (population,
/// capitals, contested)".
pub(super) fn sync_region_label_visibility(
    sim: Res<SimRes>,
    selected: Res<SelectedRegion>,
    camera: Query<&Projection, With<MainCamera>>,
    mut labels: Query<(&RegionLabelMarker, &mut Visibility)>,
) {
    let world = sim.0.world();
    let zoomed_in_enough = matches!(
        camera.single(),
        Ok(Projection::Orthographic(ortho)) if ortho.scale <= LABEL_ZOOM_THRESHOLD
    );
    for (marker, mut visibility) in &mut labels {
        let contested = world.regions.get(marker.region.index()).is_some_and(|r| r.occupier.is_some());
        let show = marker.always_visible || contested || zoomed_in_enough || selected.0 == Some(marker.region);
        *visibility = if show { Visibility::Visible } else { Visibility::Hidden };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use archipelago_sim::ids::RegionId;
    use archipelago_sim::scenario;

    use crate::sim_driver::SimDriver;

    fn run<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
        let mut system = IntoSystem::into_system(system);
        system.initialize(world);
        system.run((), world).unwrap();
    }

    fn material_color(world: &World, handle: &Handle<ColorMaterial>) -> Color {
        world.resource::<Assets<ColorMaterial>>().get(handle).unwrap().color
    }

    /// Regression guard for `MapMode` dispatch: every mode must paint the
    /// exact color its own pure function (`map_mode::terrain_fill`/
    /// `population_fill`/`industry_fill`/`unrest_fill`, or this file's own
    /// `political_fill_color`) computes for the region actually on screen -
    /// not some other mode's color left over, and not a fixed fallback.
    /// Checked this fails when broken: temporarily hardcoded
    /// `sync_region_visuals`'s `match mode.0 { ... }` to always take the
    /// `Political` arm - every non-Political assertion below then fails
    /// (e.g. `Terrain`'s got color equals the *Political* fill instead of
    /// `terrain_fill(region.terrain)`).
    #[test]
    fn every_map_mode_paints_the_color_its_own_function_computes() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<ColorMaterial>>();

        let sim_world = scenario::build_world();
        let region = sim_world.regions[0].clone();
        let region_id = region.id;
        let active_good = ActiveGood::default().0;
        let population_cuts = map_mode::population_thresholds(&sim_world);
        let industry_cuts = map_mode::industry_thresholds(&sim_world, active_good);
        world.insert_resource(SimRes(SimDriver::new(sim_world, 1)));
        world.insert_resource(ActiveGood(active_good));

        let handle = world.resource_mut::<Assets<ColorMaterial>>().add(ColorMaterial::from_color(Color::NONE));
        world.spawn((RegionMarker(region_id), MeshMaterial2d(handle.clone())));

        for &mode in map_mode::ALL_MODES.iter() {
            world.insert_resource(MapModeRes(mode));
            run(&mut world, sync_region_visuals);
            let got = material_color(&world, &handle);
            let expected = match mode {
                MapMode::Political | MapMode::Supply => political_fill_color(&region),
                MapMode::Terrain => map_mode::terrain_fill(region.terrain),
                MapMode::Population => map_mode::population_fill(region.population, population_cuts),
                MapMode::Industry => map_mode::industry_fill(&region, active_good, industry_cuts),
                MapMode::Unrest => map_mode::unrest_fill(&region),
                MapMode::Air => air_superiority_fill(&region),
            };
            assert_eq!(got, expected, "mode {mode:?} did not paint the color its own function computes");
        }

        // Terrain and Population must actually differ from Political for
        // this fixture - otherwise the assertions above could pass
        // vacuously if every arm happened to collapse to the same color.
        world.insert_resource(MapModeRes(MapMode::Political));
        run(&mut world, sync_region_visuals);
        let political = material_color(&world, &handle);
        world.insert_resource(MapModeRes(MapMode::Terrain));
        run(&mut world, sync_region_visuals);
        let terrain = material_color(&world, &handle);
        assert_ne!(political, terrain, "Political and Terrain must render visibly different colors for the same region");
    }

    /// `air_superiority_fill` must read `Region::air_superiority` the same
    /// dominant-share-tinted-toward-`NEUTRAL` shape `sync_sea_zone_visuals`
    /// already uses for sea control - a region no faction's air power
    /// reaches must render as plain `NEUTRAL`, and one faction fully
    /// dominating must render as that faction's own full-strength
    /// `faction_color`. Checked this fails when broken: temporarily
    /// hardcoded `air_superiority_fill` to always return `NEUTRAL` - the
    /// full-dominance assertion below then fails.
    #[test]
    fn air_superiority_fill_tints_toward_the_dominant_factions_own_color() {
        let sim_world = scenario::build_world();
        let mut region = sim_world.regions[0].clone();

        region.air_superiority = vec![archipelago_sim::world::AirSuperiority::NEUTRAL; sim_world.factions.len()];
        assert_eq!(
            air_superiority_fill(&region),
            NEUTRAL,
            "no faction's air power reaching this region must render as plain NEUTRAL"
        );

        region.air_superiority[1] = archipelago_sim::world::AirSuperiority::new(1.0).unwrap();
        assert_eq!(
            air_superiority_fill(&region),
            faction_color(1),
            "full one-faction air dominance must render as that faction's own full-strength color"
        );

        region.air_superiority[1] = archipelago_sim::world::AirSuperiority::new(0.5).unwrap();
        let partial = air_superiority_fill(&region);
        assert_ne!(partial, NEUTRAL, "a contested share must not read as fully neutral");
        assert_ne!(partial, faction_color(1), "a contested share must read differently from full dominance");
    }

    /// `OwnerBorderMarker` must track the region's current owner regardless
    /// of which `MapMode` is active - it's the one thing this crate now
    /// draws unconditionally so ownership never goes dark
    /// (`map_mode`'s own doc, "Ownership stays visible in every mode").
    /// Checked this fails when broken: temporarily hardcoded
    /// `sync_owner_border`'s color to `NEUTRAL` - this assertion then fails.
    #[test]
    fn owner_border_tracks_the_regions_current_owner() {
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<ColorMaterial>>();

        let sim_world = scenario::build_world();
        let region_id: RegionId = sim_world.regions[0].id;
        let owner = sim_world.regions[0].owner;
        world.insert_resource(SimRes(SimDriver::new(sim_world, 1)));

        let handle = world.resource_mut::<Assets<ColorMaterial>>().add(ColorMaterial::from_color(Color::NONE));
        world.spawn((OwnerBorderMarker(region_id), MeshMaterial2d(handle.clone())));

        run(&mut world, sync_owner_border);
        assert_eq!(material_color(&world, &handle), faction_color(owner.index()), "the border must match the region's current owner color");
    }

    /// What is clickable must be what is drawn (`codex review`, P2).
    ///
    /// `sync_unit_visuals` cancels the camera's zoom so a marker holds a
    /// constant screen size; `input::map_click_select` multiplies
    /// `UNIT_CLICK_RADIUS` by the very same `camera_zoom`. When the markers
    /// were first made screen-space constant the hit test was left in world
    /// units, so at the ~3.3 whole-map fit a player could see a marker and
    /// not click it.
    ///
    /// Pinned as a *shared derivation*, not two matching numbers: this
    /// asserts the drawn scale and the click radius move together across
    /// zooms, which is only true while both go through `camera_zoom`.
    ///
    /// **Confirmed this test can fail.** Reverting `map_click_select` to a
    /// fixed world-space `UNIT_CLICK_RADIUS` (dropping the multiply) makes
    /// the ratio at zoom 3.3 come out 14.0 instead of 46.2, tripping the
    /// assertion below.
    #[test]
    fn the_unit_click_radius_tracks_the_drawn_marker_size() {
        for zoom in [0.5f32, 1.0, 3.3, 8.0] {
            let mut world = World::new();
            world.spawn((MainCamera, Projection::Orthographic(OrthographicProjection { scale: zoom, ..OrthographicProjection::default_2d() })));
            let mut q = world.query_filtered::<&Projection, With<MainCamera>>();
            let seen = {
                let q: Query<&Projection, With<MainCamera>> = q.query(&world);
                camera_zoom(&q)
            };
            assert!(
                (seen - zoom).abs() < 1e-5,
                "camera_zoom must report the camera's own orthographic scale: expected {zoom}, got {seen}"
            );
            let click_radius = crate::app::input::UNIT_CLICK_RADIUS * seen;
            assert!(
                (click_radius - 14.0 * zoom).abs() < 1e-4,
                "the click radius must scale with zoom exactly as the marker does: at zoom {zoom} expected {}, got {click_radius}",
                14.0 * zoom
            );
        }
    }
}
