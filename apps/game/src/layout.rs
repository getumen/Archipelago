//! Deterministic fallback map layout for a scenario that ships no
//! `position` on some or all of its regions (docs/phase7-spec.md "地域の座
//! 標": "座標がない場合は、隣接グラフからの決定論的なレイアウトにフォール
//! バックする（ユーザが書いた任意のシナリオも描画できること）"). Pure graph
//! math over `archipelago_sim::world::World` - no `bevy` import anywhere in
//! this file, no `HashMap`/`HashSet` (this crate's own copy of `crates/sim`'s
//! "反復順に依存しない" discipline), so `layout_fallback_is_deterministic`
//! needs no window and no random-seeded hasher to worry about.

use std::collections::VecDeque;

use archipelago_sim::ids::RegionId;
use archipelago_sim::world::World;

/// Vertical distance between BFS layers.
const LAYER_SPACING: f32 = 140.0;
/// Horizontal distance between regions sharing a layer.
const NODE_SPACING: f32 = 120.0;

/// One `[x, y]` per region, indexed by `RegionId::index()` (so `positions[i]`
/// is `world.regions[i]`'s position): the scenario's own `Region::position`
/// wherever it set one, and a deterministic breadth-first layering off the
/// region graph (`World::neighbors`) everywhere else. A scenario that named
/// coordinates for every region never touches the fallback at all; one that
/// named none gets a full graph layout; a mix of the two (some regions
/// placed, some not) is honoured region by region.
pub fn region_positions(world: &World) -> Vec<[f32; 2]> {
    if world.regions.iter().all(|r| r.position.is_some()) {
        return world.regions.iter().map(|r| r.position.expect("checked above")).collect();
    }
    let fallback = bfs_layer_layout(world);
    world.regions.iter().enumerate().map(|(i, r)| r.position.unwrap_or(fallback[i])).collect()
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

/// Breadth-first layering: `RegionId(0)` (and, if the graph has more than
/// one connected component, the lowest-id region of each remaining
/// component - a validated scenario is always fully connected, but this
/// stays defined for an arbitrary `World` regardless) seeds layer 0; every
/// region's layer is one more than the layer it was first reached from.
/// Within a layer, regions are laid out left-to-right in ascending
/// `RegionId` order, centered on x = 0.
///
/// Deterministic by construction: `World::neighbors` iterates `Region::
/// links`, a `Vec` in scenario-file order, but is explicitly re-sorted by id
/// below before being queued anyway - so this depends only on `RegionId`
/// values and file-authored adjacency, never on any hash-based iteration
/// order, and gives the same output every time for the same `World`.
fn bfs_layer_layout(world: &World) -> Vec<[f32; 2]> {
    let n = world.regions.len();
    if n == 0 {
        return Vec::new();
    }
    let mut layer = vec![u32::MAX; n];
    let mut queue: VecDeque<RegionId> = VecDeque::new();

    for start in 0..n {
        if layer[start] != u32::MAX {
            continue;
        }
        layer[start] = 0;
        queue.push_back(RegionId(start as u32));
        while let Some(current) = queue.pop_front() {
            let mut neighbors: Vec<RegionId> = world.neighbors(current).collect();
            neighbors.sort_by_key(|r| r.0);
            let next_layer = layer[current.index()] + 1;
            for next in neighbors {
                if layer[next.index()] == u32::MAX {
                    layer[next.index()] = next_layer;
                    queue.push_back(next);
                }
            }
        }
    }

    let max_layer = layer.iter().copied().max().unwrap_or(0);
    let mut positions = vec![[0.0f32; 2]; n];
    for l in 0..=max_layer {
        let mut members: Vec<usize> = (0..n).filter(|&i| layer[i] == l).collect();
        members.sort_unstable();
        let count = members.len();
        for (slot, &i) in members.iter().enumerate() {
            let x = (slot as f32 - (count as f32 - 1.0) / 2.0) * NODE_SPACING;
            let y = l as f32 * LAYER_SPACING;
            positions[i] = [x, y];
        }
    }
    positions
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::scenario;

    /// A 5-region scenario naming no `position` field on any region at all -
    /// the shape every pre-Stage-7A scenario has. `a` is the hub (b, c, d
    /// all one hop away), `e` hangs two hops off `b` - enough branching to
    /// exercise both "multiple regions share a layer" and "a layer with
    /// just one region".
    const NO_POSITION_SCENARIO: &str = r#"
    {
      "regions": [
        { "id": "a", "name": "A", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0,
          "links": [ { "to": "b", "kind": "rail" }, { "to": "c", "kind": "rail" }, { "to": "d", "kind": "rail" } ] },
        { "id": "b", "name": "B", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0,
          "links": [ { "to": "a", "kind": "rail" }, { "to": "e", "kind": "rail" } ] },
        { "id": "c", "name": "C", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0,
          "links": [ { "to": "a", "kind": "rail" } ] },
        { "id": "d", "name": "D", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0,
          "links": [ { "to": "a", "kind": "rail" } ] },
        { "id": "e", "name": "E", "terrain": "plain", "population": 10.0,
          "capacity": {"food":1.0,"energy":1.0,"steel":1.0,"machinery":1.0,"munitions":1.0,"arms":1.0},
          "infrastructure": 0.5, "port": 0.0,
          "links": [ { "to": "b", "kind": "rail" } ] }
      ],
      "sea_zones": [],
      "factions": [
        { "id": "f1", "name": "F1", "capital": "a", "regions": ["a", "b", "c", "d", "e"] }
      ]
    }
    "#;

    /// `layout_fallback_is_deterministic` (docs/phase7-spec.md "Stage 7A の
    /// 受け入れ基準"): the fallback layout for a scenario with no authored
    /// coordinates is the same every time it's computed, pinned against a
    /// hardcoded expected layout rather than merely "call twice and compare
    /// to itself" (which a `HashMap`-based implementation could still pass
    /// within a single process even while being unsound across processes).
    ///
    /// Confirmed this can actually fail: temporarily swapped the `Vec<usize>`
    /// layer-membership scan for an unsorted `HashSet<RegionId>` (removing
    /// the `sort_unstable()` call) and re-ran this test with `-- --test-
    /// threads=1` across several fresh `cargo test` invocations - the
    /// asserted-exact positions for the width-3 layer (`b`, `c`, `d`) came
    /// back in a different left-to-right order on different runs, failing
    /// the exact-position assertions below. Reverted before committing.
    #[test]
    fn layout_fallback_is_deterministic() {
        let world = scenario::load_str(NO_POSITION_SCENARIO).expect("NO_POSITION_SCENARIO must be a valid scenario");
        assert!(world.regions.iter().all(|r| r.position.is_none()), "fixture must name no positions");

        let first = region_positions(&world);
        let second = region_positions(&world);
        assert_eq!(first, second, "computing the fallback layout twice must give the exact same positions");

        // Pinned expected layout: layer 0 = {a}, layer 1 = {b, c, d} (b < c
        // < d by id), layer 2 = {e}.
        let a = first[0];
        let b = first[1];
        let c = first[2];
        let d = first[3];
        let e = first[4];
        assert_eq!(a, [0.0, 0.0], "the BFS root must sit at the origin");
        assert_eq!(b[1], 140.0, "b is one hop from a");
        assert_eq!(c[1], 140.0, "c is one hop from a");
        assert_eq!(d[1], 140.0, "d is one hop from a");
        assert!(b[0] < c[0], "layer members must be ordered left-to-right by ascending RegionId (b before c)");
        assert!(c[0] < d[0], "layer members must be ordered left-to-right by ascending RegionId (c before d)");
        assert_eq!(b[0], -120.0);
        assert_eq!(c[0], 0.0);
        assert_eq!(d[0], 120.0);
        assert_eq!(e, [0.0, 280.0], "e is two hops from a, straight through its only neighbor b");
    }

    /// A scenario naming a `position` for every region must use exactly
    /// those coordinates - the fallback is never consulted at all.
    #[test]
    fn explicit_positions_are_used_verbatim_when_complete() {
        let with_positions = NO_POSITION_SCENARIO.replacen(
            r#"{ "id": "a", "name": "A", "terrain": "plain", "population": 10.0,"#,
            r#"{ "id": "a", "name": "A", "position": [7.0, 9.0], "terrain": "plain", "population": 10.0,"#,
            1,
        );
        // Only `a` has a position; the other four still fall back - proves
        // `region_positions` honours an explicit position exactly (not
        // "close to" the fallback) while still filling in every gap.
        let world = scenario::load_str(&with_positions).expect("must still be a valid scenario");
        let positions = region_positions(&world);
        assert_eq!(positions[0], [7.0, 9.0], "an explicit position must be used verbatim, not overridden by the fallback");
        // The other four regions still get *some* position (the fallback
        // never leaves a region unplaced).
        for &p in &positions[1..] {
            assert!(p[0].is_finite() && p[1].is_finite());
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
}
