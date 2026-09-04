//! Map layout for `apps/game` (docs/phase7-spec.md "地域の座標"): every
//! region's on-screen position comes straight from the scenario file's own
//! `Region::position`.
//!
//! External code review fix B4: this module used to also compute a
//! deterministic breadth-first graph layout for any region a scenario left
//! unplaced, so a coordinate-free scenario could still be rendered. The
//! project owner rejected that fallback (docs/conventions.md §3, フォールバ
//! ック原則禁止): `Region::position` is required now
//! (`archipelago_sim::scenario::parse_position`), so a scenario missing one
//! anywhere is rejected with a specific `ScenarioError::Schema` at load time
//! (`scenario::load_file`/`load_str`, before `apps/game::app::run` - and
//! therefore this module - ever sees the `World`), and `region_positions`
//! below can simply read every region's own coordinate straight through.

use archipelago_sim::world::World;

/// One `[x, y]` per region, indexed by `RegionId::index()` (so `positions[i]`
/// is `world.regions[i]`'s position): exactly the scenario's own
/// `Region::position` for every region. Infallible - `world` is only ever
/// built by `archipelago_sim::scenario::build_world`/`load_str`/`load_file`
/// (`app::run`'s own doc), every one of which already rejects a scenario
/// missing a `position` anywhere before a `World` is ever produced at all.
pub fn region_positions(world: &World) -> Vec<[f32; 2]> {
    world.regions.iter().map(|r| r.position).collect()
}

/// One outward-pointing unit vector `[dx, dy]` per region, indexed the same
/// way as `region_positions`'s own output: the direction `setup::setup`
/// should push that region's name label away from its marker before
/// drawing it, so a dense cluster of regions (`japan47`'s Kyushu/Kinki/
/// Chugoku prefectures, several of which sit under 40 world units apart)
/// spreads its labels out in different directions instead of stacking every
/// one straight above its region, where neighbouring names collide.
///
/// Each direction points away from that region's own inverse-square-
/// distance-weighted centroid of every *other* region - electrostatic
/// repulsion, not a fixed "away from the map center" rule, so it reacts to
/// purely *local* crowding (a region can sit near the map's overall center
/// yet still push its label outward from its own tight neighbourhood, and
/// one sitting far from the center but with no nearby neighbours gets
/// almost no push at all, since every weight `1 / distance^2` decays fast).
/// Falls back to straight up (`[0.0, 1.0]`, `setup::setup`'s pre-Stage-7B
/// default placement) for a single-region map or the vanishingly rare case
/// where a region's weighted centroid lands exactly on its own position.
pub fn label_push_directions(positions: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let n = positions.len();
    let mut out = vec![[0.0f32, 1.0f32]; n];
    for i in 0..n {
        let [xi, yi] = positions[i];
        let mut weight_sum = 0.0f32;
        let mut centroid_x = 0.0f32;
        let mut centroid_y = 0.0f32;
        for (j, &[xj, yj]) in positions.iter().enumerate() {
            if j == i {
                continue;
            }
            let dx = xj - xi;
            let dy = yj - yi;
            // Floored so two regions sharing (or nearly sharing) a position
            // can't blow this weight up toward infinity.
            let dist_sq = (dx * dx + dy * dy).max(1e-3);
            let weight = 1.0 / dist_sq;
            weight_sum += weight;
            centroid_x += weight * xj;
            centroid_y += weight * yj;
        }
        if weight_sum <= 0.0 {
            continue; // n == 1: no other region to push away from.
        }
        let dx = xi - centroid_x / weight_sum;
        let dy = yi - centroid_y / weight_sum;
        let len = (dx * dx + dy * dy).sqrt();
        if len > 1e-3 {
            out[i] = [dx / len, dy / len];
        }
    }
    out
}

/// The scenario's own effective grid "pitch" - the median nearest-neighbor
/// distance between region centers - used by `app::setup::compute_region_radii`
/// to size a filled hex marker for a dense, regularly-spaced scenario
/// (`japan_hex`, 289 regions on a 40-world-unit hex grid) so adjacent cells
/// tile without gaps or overlap, instead of `app::setup::region_radius`'s
/// population-driven circle - tuned for the much sparser `mvp`/`japan47`
/// scenarios, where a region's own on-screen footprint isn't itself
/// meaningful grid data the way a hex cell's is.
///
/// Median, not mean or minimum: `japan_hex` has exactly one region whose
/// nearest neighbor sits at 80 world units (twice the grid's own 40-unit
/// pitch - evidently an edge case in how that scenario was generated) -
/// a mean or minimum would let that one outlier skew the whole map's
/// marker size; the median doesn't move for a single outlier among 289
/// regions.
///
/// Returns `0.0` for fewer than two regions - there is no neighbor to
/// measure, and no caller sizes anything from this without also handling a
/// degenerate/single-region map on its own.
pub fn nearest_neighbor_pitch(positions: &[[f32; 2]]) -> f32 {
    let n = positions.len();
    if n < 2 {
        return 0.0;
    }
    let mut nearest: Vec<f32> = Vec::with_capacity(n);
    for (i, &[xi, yi]) in positions.iter().enumerate() {
        let mut best = f32::INFINITY;
        for (j, &[xj, yj]) in positions.iter().enumerate() {
            if i == j {
                continue;
            }
            let dx = xj - xi;
            let dy = yj - yi;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < best {
                best = dist;
            }
        }
        nearest.push(best);
    }
    // Deterministic total order (`f32::total_cmp`) rather than the
    // panic-on-NaN partial-order `sort_by(f32::partial_cmp)` would need -
    // positions are always finite scenario data, but the total order costs
    // nothing and keeps this infallible regardless.
    nearest.sort_by(f32::total_cmp);
    nearest[n / 2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::scenario;

    /// A 5-region scenario naming a `position` for every region - `a` is the
    /// hub (b, c, d all one hop away), `e` hangs two hops off `b`, though the
    /// graph shape itself is no longer load-bearing for this module (only
    /// `Region::position` is read); kept branching anyway so this fixture
    /// still exercises `scenario::load_str`'s ordinary connectivity checks.
    const ALL_POSITIONS_SCENARIO: &str = r#"
    {
      "regions": [
        { "id": "a", "name": "A", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0, "position": [7.0, 9.0],
          "links": [ { "to": "b", "kind": "rail" }, { "to": "c", "kind": "rail" }, { "to": "d", "kind": "rail" } ] },
        { "id": "b", "name": "B", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0, "position": [-120.0, 140.0],
          "links": [ { "to": "a", "kind": "rail" }, { "to": "e", "kind": "rail" } ] },
        { "id": "c", "name": "C", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0, "position": [0.0, 140.0],
          "links": [ { "to": "a", "kind": "rail" } ] },
        { "id": "d", "name": "D", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0, "position": [120.0, 140.0],
          "links": [ { "to": "a", "kind": "rail" } ] },
        { "id": "e", "name": "E", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0, "position": [0.0, 280.0],
          "links": [ { "to": "b", "kind": "rail" } ] }
      ],
      "sea_zones": [],
      "factions": [
        { "id": "f1", "name": "F1", "capital": "a", "regions": ["a", "b", "c", "d", "e"] }
      ],
      "diplomacy": { "blocs": [] },
      "victory": [ { "type": "conquest" } ]
    }
    "#;

    /// External code review fix B4: `region_positions` reads every region's
    /// own `Region::position` verbatim - no fallback layout exists to fall
    /// back to any more (this module's own doc explains why the previous
    /// `layout_fallback_is_deterministic`/BFS-layout test was removed rather
    /// than kept: it described a fallback that no longer exists).
    #[test]
    fn region_positions_reads_every_region_verbatim() {
        let world = scenario::load_str(ALL_POSITIONS_SCENARIO).expect("ALL_POSITIONS_SCENARIO must be a valid scenario");
        let positions = region_positions(&world);
        assert_eq!(
            positions,
            vec![[7.0, 9.0], [-120.0, 140.0], [0.0, 140.0], [120.0, 140.0], [0.0, 280.0]],
            "region_positions must return each region's own scenario-authored position, unchanged"
        );
    }

    /// External code review fix B4: a scenario that leaves even one region's
    /// `position` unset is now rejected outright at scenario-load time
    /// (`archipelago_sim::scenario::parse_position`'s own tests cover the
    /// error in detail) - `apps/game::layout` never gets a `World` to fall
    /// back for in the first place. This is the "reject a scenario that
    /// lacks them" half of B4's fix, exercised from this crate's own call
    /// site rather than only from `crates/sim`.
    #[test]
    fn scenario_missing_a_position_is_rejected_before_layout_ever_runs() {
        let missing_one = ALL_POSITIONS_SCENARIO.replacen(r#", "position": [7.0, 9.0],"#, ",", 1);
        match scenario::load_str(&missing_one) {
            Err(archipelago_sim::scenario::ScenarioError::Schema(msg)) => {
                assert!(msg.contains("position"), "expected the error to name `position`, got {msg:?}");
            }
            other => panic!("expected a distinct Schema error for a scenario missing one region's position, got {other:?}"),
        }
    }

    /// A single-region map has nothing to push away from - `label_push_
    /// directions` must fall back to its documented default (straight up)
    /// rather than dividing by a zero weight sum.
    #[test]
    fn label_push_direction_defaults_to_up_for_a_single_region() {
        let dirs = label_push_directions(&[[5.0, -3.0]]);
        assert_eq!(dirs, vec![[0.0, 1.0]]);
    }

    /// Three regions in a tight horizontal row, `left`/`right` far enough
    /// from `center` that only `center`'s crowding actually matters here:
    /// `center` sits exactly between two equally-close neighbours, so its
    /// push direction must point along the row's perpendicular (straight
    /// up or down, `dx == 0.0`), not toward either neighbour.
    #[test]
    fn label_push_direction_points_away_from_a_symmetric_pair() {
        let positions = [[-10.0, 0.0], [0.0, 0.0], [10.0, 0.0]];
        let dirs = label_push_directions(&positions);
        let center = dirs[1];
        assert!(center[0].abs() < 1e-4, "a region flanked symmetrically must not push sideways, got {center:?}");
        assert!(center[1].abs() > 0.99, "a region flanked symmetrically must push straight up or down, got {center:?}");
    }

    /// A region crowded by several close neighbours on one side, with one
    /// far-off region on the opposite side, must push away from the *close*
    /// cluster (electrostatic weighting: `1 / distance^2` makes distant
    /// regions negligible) rather than toward the map's unweighted average
    /// position, which the far region would pull noticeably off-axis.
    #[test]
    fn label_push_direction_is_dominated_by_the_nearest_cluster() {
        // `subject` at the origin, three close neighbours clustered just to
        // its +x side, and one lone region far away to its -x side.
        let positions = [
            [0.0, 0.0],   // subject
            [8.0, 6.0],   // close cluster
            [8.0, -6.0],  // close cluster
            [10.0, 0.0],  // close cluster
            [-500.0, 0.0], // distant outlier
        ];
        let dirs = label_push_directions(&positions);
        let subject = dirs[0];
        assert!(subject[0] < -0.9, "expected a push dominated by the close +x cluster (away from it, i.e. -x), got {subject:?}");
    }

    /// Two coincident (or near-coincident) regions must not produce a
    /// division that blows the weight up toward infinity/NaN - the `max`
    /// floor on `dist_sq` keeps this finite, and this exact scenario ships
    /// nowhere in either scenario file but must never crash `setup::setup`
    /// if some future scenario ever authored it.
    #[test]
    fn label_push_direction_stays_finite_for_coincident_regions() {
        let positions = [[3.0, 3.0], [3.0, 3.0], [0.0, 0.0]];
        let dirs = label_push_directions(&positions);
        for d in dirs {
            assert!(d[0].is_finite() && d[1].is_finite(), "push direction must stay finite, got {d:?}");
        }
    }

    #[test]
    fn nearest_neighbor_pitch_is_zero_for_fewer_than_two_regions() {
        assert_eq!(nearest_neighbor_pitch(&[]), 0.0);
        assert_eq!(nearest_neighbor_pitch(&[[5.0, -3.0]]), 0.0);
    }

    /// A regular 2x2 grid, pitch 10 on both axes: every point's nearest
    /// neighbor sits exactly 10 away, so the median must be exactly 10, not
    /// some diagonal distance.
    #[test]
    fn nearest_neighbor_pitch_reads_a_regular_grid_exactly() {
        let positions = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0], [10.0, 10.0]];
        assert_eq!(nearest_neighbor_pitch(&positions), 10.0);
    }

    /// Four points on a pitch-10 grid plus one far outlier: the outlier's
    /// own nearest-neighbor distance is huge, but it's exactly one value
    /// among five - the median must still land on the grid's own 10, not be
    /// dragged toward the outlier the way a mean would be. Mirrors
    /// `japan_hex`'s real one-region 80-unit edge case at this function's
    /// own doc.
    #[test]
    fn nearest_neighbor_pitch_is_robust_to_a_single_outlier() {
        let positions = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0], [10.0, 10.0], [1000.0, 1000.0]];
        assert_eq!(nearest_neighbor_pitch(&positions), 10.0);
    }
}
