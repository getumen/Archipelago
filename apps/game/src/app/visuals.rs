//! Per-frame visual sync: region fill color (owner, mixed toward the
//! occupier's color as `occupation` progresses - docs/phase7-spec.md "占領
//! 進行中は所有者色と占領者色の混色にする" - then further toward a scorched-
//! earth "grime" tone by `Region::devastation`, Stage 7C's docs/phase7-spec.md
//! "3. 戦災と復興": "地域の devastation を視覚化する（マーカーの荒れ具合、
//! 色の濁り）"), sea zone tint (whichever faction currently holds the most
//! `SeaZone::control`), and unit markers (position, visibility, color, plus
//! spawning a marker for any unit created since the last frame - e.g. a
//! fresh recruit).
//!
//! Every system here only *reads* `SimRes` - never writes it. This is what
//! keeps rendering incapable of feeding anything back into the simulation
//! (docs/phase7-spec.md §0's central invariant).

use bevy::prelude::*;

use archipelago_sim::balance::{UNIT_EQUIPMENT, UNIT_MANPOWER};

use super::palette::{faction_color, Unit01, NEUTRAL};
use super::setup::station_position;
use super::{
    MainCamera, RegionLabelMarker, RegionLayout, RegionMarker, SeaZoneCenters, SeaZoneMarker,
    SelectedRegion, SimRes, UnitMarker,
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

pub(super) fn sync_region_visuals(
    sim: Res<SimRes>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    query: Query<(&RegionMarker, &MeshMaterial2d<ColorMaterial>)>,
) {
    let world = sim.0.world();
    for (marker, material_handle) in &query {
        let region = world.region(marker.0);
        let owner_color = faction_color(region.owner.index());
        let occupation_color = match region.occupier {
            Some(occupier) if region.occupation > 0.0 => {
                let occupier_color = faction_color(occupier.index());
                owner_color.mix(&occupier_color, region.occupation.clamp(0.0, 1.0))
            }
            _ => owner_color,
        };
        let devastation_mix = Unit01::new(region.devastation * MAX_DEVASTATION_MIX);
        let color = occupation_color.mix(&DEVASTATION_TINT, devastation_mix.get());
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
