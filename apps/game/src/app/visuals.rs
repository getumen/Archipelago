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
use archipelago_sim::world::Region;

use super::map_mode::{self, MapMode, MapModeRes};
use super::palette::{faction_color, Unit01, NEUTRAL};
use super::setup::station_position;
use super::{
    MainCamera, OwnerBorderMarker, RegionLabelMarker, RegionLayout, RegionMarker, SeaZoneCenters,
    SeaZoneMarker, SelectedRegion, SimRes, UnitMarker,
};

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

/// Paints every region's fill according to the active `MapMode`
/// (`map_mode`'s own module doc has the full list and rationale) - the one
/// system that actually shows the player whichever dimension of the
/// simulation they've asked to see. `population_cuts`/`industry_cuts` are
/// computed once per frame, not once per region, since they depend on every
/// region's own value (`map_mode::population_thresholds`/`industry_thresholds`'s
/// own "no invented thresholds" doc) rather than the one region being
/// painted.
pub(super) fn sync_region_visuals(
    sim: Res<SimRes>,
    mode: Res<MapModeRes>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    query: Query<(&RegionMarker, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();
    let population_cuts = map_mode::population_thresholds(world);
    let industry_cuts = map_mode::industry_thresholds(world);
    for (marker, material_handle) in &query {
        let region = world.region(marker.0);
        let color = match mode.0 {
            MapMode::Political | MapMode::Supply => political_fill_color(region),
            MapMode::Terrain => map_mode::terrain_fill(region.terrain),
            MapMode::Population => map_mode::population_fill(region.population, population_cuts),
            MapMode::Industry => map_mode::industry_fill(region, industry_cuts),
            MapMode::Unrest => map_mode::unrest_fill(region),
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

pub(super) fn sync_unit_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    sim: Res<SimRes>,
    layout: Res<RegionLayout>,
    sea_centers: Res<SeaZoneCenters>,
    mut existing: Query<(&UnitMarker, &mut Transform, &mut Visibility, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();

    let mut known = std::collections::HashSet::new();
    for (marker, _, _, _) in &existing {
        known.insert(marker.0.index());
    }
    for unit in &world.units {
        if !known.contains(&unit.id.index()) {
            commands.spawn((
                Mesh2d(meshes.add(RegularPolygon::new(6.0, 3))),
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
        let [cx, cy] = station_position(unit.station, &layout.0, &sea_centers.0);
        let (slot, count) = slot_of.get(&marker.0.0).copied().unwrap_or((0, 1));
        let offset = ring_offset(slot, count, 14.0);
        transform.translation.x = cx + offset.x;
        transform.translation.y = cy + offset.y;
        let strength = ((unit.manpower / UNIT_MANPOWER) + (unit.equipment / UNIT_EQUIPMENT)) / 2.0;
        transform.scale = Vec3::splat(strength.clamp(0.4, 1.3));
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
        let population_cuts = map_mode::population_thresholds(&sim_world);
        let industry_cuts = map_mode::industry_thresholds(&sim_world);
        world.insert_resource(SimRes(SimDriver::new(sim_world, 1)));

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
                MapMode::Industry => map_mode::industry_fill(&region, industry_cuts),
                MapMode::Unrest => map_mode::unrest_fill(&region),
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
}
