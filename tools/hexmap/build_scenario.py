#!/usr/bin/env python3
"""Hex map generator (docs/phase8-spec.md, all sections; section 4 for
verification). Not part of the Rust build (`crates/sim` gains no dependency
from this); run it manually and commit the resulting `scenarios/*.json`.

Pipeline:
  1. Fetch/cache GSI elevation tiles covering Japan, assemble into one
     mosaic (dem.py).
  2. Lay a pointy-top axial hex grid over the same area with a simple
     equirectangular + fixed-cos(lat) projection (hexgrid.py).
  3. Assign every mosaic pixel to its nearest hex center (exact hex Voronoi
     partition via cube rounding) and aggregate per-hex land fraction,
     median elevation and relief from the assigned land pixels.
  4. Adopt hexes whose center is land and whose land pixel fraction clears
     a threshold; classify terrain from median + relief (terrain.py).
  5. Link geometrically-adjacent land hexes (Rail/Road by terrain).
  6. Force the three named Phase 2 chokepoints (Kanmon/Seikan/Setouchi) to
     the correct link kind regardless of what the generic rule produced.
  7. Bridge remaining disconnected land components with Strait links up to
     a max real-world gap; leave anything farther isolated.
  8. Stage 8B (docs/phase8-spec.md sections 2-3): assign each hex a
     prefecture (nearest capital) and, through it, a population share, a
     per-Good production capacity, a port size and an infrastructure level;
     promote densely-populated Plain hexes to Urban; group prefectures into
     8 regional factions and remap every strait/bridge link onto one of 8
     real sea zones.
  9. Emit a `Scenario`-shaped JSON file.

Stage 8B calibration (docs/phase8-spec.md section 2: "合計値の目標を先に決
め、それに合わせて係数を較正する"). `crates/agents/src/lib.rs` sizes a
faction's land army as `unit_cap = 3 + industry_total/5`, where
`industry_total` is the faction's own sum of Energy/Steel/Machinery/
Munitions/Arms capacity; `crates/sim/src/balance.rs`'s
`SUPPLY_NEED_PER_MANPOWER = 1.0` then makes every one of those units draw
1.0 Munitions/day just to hold together (`logistics::distribute_supply`).
Per-faction, at operating efficiency `eff`, that upkeep is met only if

    CAPACITY_SHARE_MUNITIONS * eff * industry_total_f >= (3 + industry_total_f/5) * SAFETY

(`SAFETY` a margin for `COMBAT_SUPPLY_MULT = 2.5` combat spikes on whatever
share of the army is `unit_contested` at once - this map's alternating-bloc
diplomacy, `regions.py`'s doc, keeps most factions with some front open most
of the time). As `industry_total_f -> infinity` this reduces to the
scale-free bound `CAPACITY_SHARE_MUNITIONS >= 0.2*SAFETY/eff`; `japan47`
gives Munitions only ~13% of its national total against a ~29% break-even
bound at a generous `eff=0.7`/`SAFETY=1`, which is why every faction there
converges on `munitions == 0` regardless of how long the run goes.
`constants.CAPACITY_SHARE_MUNITIONS`'s own doc has the full two-round
derivation (a first attempt at 0.38 broke on this external bound; sizing
Munitions correctly then broke Steel/Energy's *internal* self-sufficiency,
requiring a second pass) landing on 0.42/0.28/0.22/0.05/0.03 for Munitions/
Energy/Steel/Machinery/Arms.

For a *small* `industry_total_f` the flat "+3" term dominates instead, and
no realistic share split covers it in isolation - the bound becomes
`CAPACITY_SHARE_MUNITIONS*eff >= SAFETY*(3/industry_total_f + 0.2)`, which
blows up as `industry_total_f -> 0`. The only lever this generator has
against that is `constants.TARGET_INDUSTRY_TOTAL` itself, which sets every
faction's `industry_total_f` proportional to its population share
(`build_capacities`) and was raised from an initial 160 to 240 specifically
because the smaller/quieter factions (Hokkaido in particular - stable
territory throughout a trial run, yet `industry_total_f` too small to clear
even its own peacetime "+3" floor) still couldn't sustain Munitions at 160.
`unit_cap` summed over the 8 factions this generator builds comes to
`8*3 + 240/5 = 72`, ~9 land units/faction on average (Kanto's population
share puts it well above that; Shikoku, the smallest, still clears
`3 + (240 * 361/12495)/5 ≈ 4.4`) - see the Stage 8B report for the actual
per-faction Munitions trend the generated map produces across seeds,
including the residual cases (a faction ground down to near-zero territory
by real conquest, or Hokkaido's persistently thin economy) that no
capacity calibration alone can fully cover.

Usage:
    venv/bin/python3 tools/hexmap/build_scenario.py \\
        --out scenarios/japan_hex.json --cache-dir <dir>
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
from collections import defaultdict

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import constants
import dem
import hexgrid
import prefecture_population
import regions as region_mod
import sea_zones as sea_zone_mod
import terrain as terrain_mod
from prefecture_capitals import CAPITALS
from terrain_kind import Terrain


# --- Union-Find --------------------------------------------------------------

class UnionFind:
    def __init__(self, items):
        self.parent = {i: i for i in items}

    def find(self, x):
        while self.parent[x] != x:
            self.parent[x] = self.parent[self.parent[x]]
            x = self.parent[x]
        return x

    def union(self, a, b):
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            self.parent[ra] = rb

    def components(self):
        groups: dict = {}
        for item in self.parent:
            groups.setdefault(self.find(item), []).append(item)
        return list(groups.values())


# --- Hex sampling --------------------------------------------------------------

# Offsets used to pack (q, r) into one non-negative int64 key for np.unique.
_KEY_BIAS = 1 << 20
_KEY_MUL = 1 << 21


def pack_key(q: np.ndarray, r: np.ndarray) -> np.ndarray:
    return (q.astype(np.int64) + _KEY_BIAS) * _KEY_MUL + (r.astype(np.int64) + _KEY_BIAS)


def unpack_key(key: int) -> tuple[int, int]:
    q = key // _KEY_MUL - _KEY_BIAS
    r = key % _KEY_MUL - _KEY_BIAS
    return int(q), int(r)


def lonlat_to_pixel(lon: float, lat: float, zoom: int) -> tuple[float, float]:
    n = 2 ** zoom
    x = (lon + 180.0) / 360.0 * n * dem.TILE_SIZE
    lat_rad = math.radians(lat)
    y = (1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi) / 2.0 * n * dem.TILE_SIZE
    return x, y


def build_hexes(mosaic: dem.Mosaic, layout: hexgrid.HexLayout, log=print):
    log("assigning pixels to hexes...")
    x_m, y_m = hexgrid.lonlat_to_meters(mosaic.lon, mosaic.lat)
    q_frac, r_frac = layout.meters_to_axial_frac_vec(x_m, y_m)
    q_int, r_int = hexgrid.cube_round_vec(q_frac, r_frac)
    keys = pack_key(q_int, r_int)

    unique_keys, inverse, counts_all = np.unique(keys.ravel(), return_inverse=True, return_counts=True)
    log(f"  {unique_keys.size} distinct hex cells touched by the mosaic")

    roughness = dem.local_roughness(mosaic.elevation)

    land_mask = ~mosaic.is_sea.ravel()
    elev_flat = mosaic.elevation.ravel()
    rough_flat = roughness.ravel()
    land_group = inverse[land_mask]
    land_elev = elev_flat[land_mask]
    land_rough = rough_flat[land_mask]

    land_counts = np.bincount(land_group, minlength=unique_keys.size)
    land_fraction = land_counts / counts_all

    candidate_idx = np.where(land_fraction >= constants.LAND_PIXEL_FRACTION_MIN)[0]
    log(f"  {candidate_idx.size} cells clear the land-fraction threshold ({constants.LAND_PIXEL_FRACTION_MIN})")

    order = np.argsort(land_group, kind="stable")
    sorted_group = land_group[order]
    sorted_elev = land_elev[order]
    sorted_rough = land_rough[order]
    bounds = np.searchsorted(sorted_group, np.arange(unique_keys.size + 1))

    hexes: dict[tuple[int, int], dict] = {}
    for idx in candidate_idx:
        key = int(unique_keys[idx])
        q, r = unpack_key(key)
        x_c, y_c = layout.axial_to_meters(q, r)
        lon_c, lat_c = hexgrid.meters_to_lonlat(x_c, y_c)

        # Center-pixel-is-land check (spec: "中心が陸地のヘクスを採用する").
        px, py = lonlat_to_pixel(lon_c, lat_c, mosaic.zoom)
        local_x = int(round(px)) - mosaic.x0 * dem.TILE_SIZE
        local_y = int(round(py)) - mosaic.y0 * dem.TILE_SIZE
        h, w = mosaic.is_sea.shape
        if not (0 <= local_y < h and 0 <= local_x < w):
            continue
        if mosaic.is_sea[local_y, local_x]:
            continue

        lo, hi = bounds[idx], bounds[idx + 1]
        elev_slice = sorted_elev[lo:hi]
        rough_slice = sorted_rough[lo:hi]
        if elev_slice.size == 0:
            continue
        median = float(np.median(elev_slice))
        p10 = float(np.percentile(elev_slice, 10))
        p90 = float(np.percentile(elev_slice, 90))
        relief = p90 - p10

        # "Flat median" (constants.py's doc): central tendency of only the
        # locally-flat pixels, if there are enough of them - otherwise the
        # whole-hex median (no flat patch found -> nothing to correct for).
        flat_pixels = elev_slice[rough_slice <= constants.FLAT_ROUGHNESS_M]
        flat_gate = max(constants.MIN_FLAT_SAMPLES, constants.MIN_FLAT_FRACTION * elev_slice.size)
        if flat_pixels.size >= flat_gate:
            flat_median = float(np.median(flat_pixels))
        else:
            flat_median = median

        t = terrain_mod.classify(flat_median, relief)
        hexes[(q, r)] = dict(
            q=q, r=r, lon=lon_c, lat=lat_c, x_m=x_c, y_m=y_c,
            median_elev=median, flat_median_elev=flat_median, relief=relief, terrain=t,
            land_fraction=float(land_fraction[idx]),
        )

    log(f"  {len(hexes)} hexes adopted (center-on-land + fraction threshold)")
    return hexes


def nearest_hex(hexes: dict, lon: float, lat: float) -> tuple[int, int]:
    x0, y0 = hexgrid.lonlat_to_meters(lon, lat)
    best_id, best_d2 = None, None
    for hid, h in hexes.items():
        d2 = (h["x_m"] - x0) ** 2 + (h["y_m"] - y0) ** 2
        if best_d2 is None or d2 < best_d2:
            best_id, best_d2 = hid, d2
    return best_id


def hex_id_str(hid: tuple[int, int]) -> str:
    q, r = hid
    return f"h{q}_{r}"


def nearest_pref_name(lon: float, lat: float) -> str:
    x0, y0 = hexgrid.lonlat_to_meters(lon, lat)
    best_name, best_d2 = None, None
    for name, (plat, plon) in CAPITALS.items():
        px, py = hexgrid.lonlat_to_meters(plon, plat)
        d2 = (px - x0) ** 2 + (py - y0) ** 2
        if best_d2 is None or d2 < best_d2:
            best_name, best_d2 = name, d2
    return best_name


def build_links(hexes: dict, log=print):
    """Ordinary geometric land-land adjacency links, one entry per
    unordered pair, kind decided by terrain.link_kind. Returns a dict
    {(a,b) sorted tuple of hex ids: kind_str}."""
    links: dict[tuple, str] = {}
    for hid, h in hexes.items():
        q, r = hid
        for dq, dr in hexgrid.AXIAL_DIRECTIONS:
            nid = (q + dq, r + dr)
            if nid not in hexes or nid <= hid:
                continue
            nb = hexes[nid]
            kind = terrain_mod.link_kind(h["terrain"], nb["terrain"], h["median_elev"], nb["median_elev"])
            links[(hid, nid)] = kind
    return links


def apply_named_links(hexes: dict, links: dict, uf: UnionFind, entries: list, log=print):
    """Locates each named entry (constants.CHOKEPOINTS / NAMED_SEA_ROUTES)
    and forces its link to the required kind.

    A chokepoint's two reference coordinates are real-world bank-to-bank
    points, often much closer together (Kanmon: <1km) than the hex spacing
    (40km) - so "nearest hex to point A" / "nearest hex to point B"
    independently can collapse onto the *same* hex (both banks fall inside
    one cell's footprint). The robust way to locate the actual crossing at
    hex resolution is to search the already-built adjacency graph for the
    existing land-land edge whose two hex centers straddle the strait's
    midpoint most closely - that edge is, by construction, the boundary
    between "the hex chain going into landmass A" and "the hex chain going
    into landmass B" nearest the real strait, regardless of how the two
    named endpoints individually happened to snap to hex centers. Only
    falls back to forcing a brand new link between the two nearest-hex
    endpoints when no existing edge is anywhere close (a genuinely
    disconnected strait crossing, e.g. if grid alignment left a gap)."""
    zones = {}
    max_edge_dist_m = 1.5 * (constants.SPACING_M)
    for cp in entries:
        a_lon, a_lat = cp["a"]
        b_lon, b_lat = cp["b"]
        mid_lon, mid_lat = (a_lon + b_lon) / 2.0, (a_lat + b_lat) / 2.0
        mid_x, mid_y = hexgrid.lonlat_to_meters(mid_lon, mid_lat)

        best_pair, best_d2 = None, None
        for (h1, h2) in links:
            ex = (hexes[h1]["x_m"] + hexes[h2]["x_m"]) / 2.0
            ey = (hexes[h1]["y_m"] + hexes[h2]["y_m"]) / 2.0
            d2 = (ex - mid_x) ** 2 + (ey - mid_y) ** 2
            if best_d2 is None or d2 < best_d2:
                best_pair, best_d2 = (h1, h2), d2

        if best_pair is not None and math.sqrt(best_d2) <= max_edge_dist_m:
            hid_a, hid_b = best_pair
            mode = "overrode existing edge"
        else:
            hid_a = nearest_hex(hexes, a_lon, a_lat)
            hid_b = nearest_hex(hexes, b_lon, b_lat)
            if hid_a == hid_b:
                log(f"  WARNING: chokepoint {cp['id']} resolved to the same hex on both sides ({hid_a}); skipping")
                continue
            mode = "forced new link (no nearby edge found)"

        pair = (hid_a, hid_b) if hid_a <= hid_b else (hid_b, hid_a)
        links[pair] = cp["kind"]
        if cp["zone"]:
            zones[pair] = cp["zone"]
        elif pair in zones:
            del zones[pair]
        uf.union(hid_a, hid_b)
        log(f"  chokepoint {cp['name']} ({cp['id']}): {hex_id_str(hid_a)} <-> {hex_id_str(hid_b)} "
            f"kind={cp['kind']} ({mode})")
    return zones


def bridge_islands(hexes: dict, links: dict, zones: dict, uf: UnionFind, log=print):
    max_gap_m = constants.MAX_STRAIT_GAP_KM * 1000.0
    bridge_n = 0
    while True:
        comps = uf.components()
        if len(comps) <= 1:
            break
        best = None  # (dist2, hid_a, hid_b)
        for i in range(len(comps)):
            for j in range(i + 1, len(comps)):
                for a in comps[i]:
                    ax, ay = hexes[a]["x_m"], hexes[a]["y_m"]
                    for b in comps[j]:
                        bx, by = hexes[b]["x_m"], hexes[b]["y_m"]
                        d2 = (ax - bx) ** 2 + (ay - by) ** 2
                        if best is None or d2 < best[0]:
                            best = (d2, a, b)
        dist_m = math.sqrt(best[0])
        if dist_m > max_gap_m:
            remaining = len(comps)
            log(f"  {remaining} components remain isolated (closest gap {dist_m/1000:.1f}km > {constants.MAX_STRAIT_GAP_KM}km)")
            break
        hid_a, hid_b = best[1], best[2]
        pair = (hid_a, hid_b) if hid_a <= hid_b else (hid_b, hid_a)
        zone_id = f"strait_{hex_id_str(hid_a)}_{hex_id_str(hid_b)}"
        links[pair] = "strait"
        zones[pair] = zone_id
        uf.union(hid_a, hid_b)
        bridge_n += 1
        log(f"  bridged {hex_id_str(hid_a)} <-> {hex_id_str(hid_b)} ({dist_m/1000:.1f}km) as new zone {zone_id}")
    log(f"  {bridge_n} auto-bridge strait links added")


def drop_disconnected(hexes: dict, links: dict, zones: dict, uf: UnionFind, log=print):
    """`crate::scenario::Scenario::validate` requires the *entire* region
    graph connected - stricter than docs/phase8-spec.md section 1's "それを
    超える成分は孤立を許す" (which envisions isolated components simply
    staying out of the link graph). Reconciled by dropping every hex not in
    the largest connected component from the output entirely - consistent
    with the same section's explicit allowance to lose small islands at the
    land-adoption step; this is that same allowance applied post-bridging
    instead of pre-adoption."""
    comps = uf.components()
    comps.sort(key=len, reverse=True)
    kept = set(comps[0])
    dropped_hexes = [hid for c in comps[1:] for hid in c]
    if dropped_hexes:
        log(f"  dropping {len(dropped_hexes)} hex(es) in {len(comps)-1} component(s) still disconnected "
            f"from the main landmass after bridging:")
        for c in comps[1:]:
            labels = sorted({nearest_pref_name(hexes[h]["lon"], hexes[h]["lat"]) for h in c})
            log(f"    {len(c)} hex(es) near {', '.join(labels)}: {[hex_id_str(h) for h in c]}")

    hexes2 = {hid: h for hid, h in hexes.items() if hid in kept}
    links2 = {(a, b): k for (a, b), k in links.items() if a in kept and b in kept}
    zones2 = {(a, b): z for (a, b), z in zones.items() if a in kept and b in kept}
    return hexes2, links2, zones2


# --- Stage 8B: population, capacity, terrain, port, infrastructure --------
# docs/phase8-spec.md section 2. See this module's own docstring for the
# capacity-share arithmetic; constants.py's Stage 8B block for every
# individual constant's own justification.

def flat_area_weight(h: dict) -> float:
    """A hex's population/Food weight: falls off smoothly with the
    *flat-patch* median elevation (`flat_median_elev` - terrain.py's
    basin-aware term, not raw median, for the same Kofu/Nagano-basin reason
    terrain classification itself avoids raw median) and with relief.
    Always strictly positive - a mountain hex still gets *some* weight, just
    much less than a plain."""
    elev = max(h["flat_median_elev"], 0.0)
    relief = max(h["relief"], 0.0)
    return (1.0 / (1.0 + elev / constants.POP_WEIGHT_ELEV_HALF_M)) * (
        1.0 / (1.0 + relief / constants.POP_WEIGHT_RELIEF_HALF_M)
    )


def assign_prefectures(hexes: dict) -> dict:
    """hex id -> nearest-prefectural-capital name (docs/phase8-spec.md
    section 2: "ヘクスがどの県に属するかは、県庁所在地の緯度経度からの最近傍
    で決める"). Same rule Stage 8A's `nearest_pref_name` already used for
    display labels; reused here to actually drive population."""
    return {hid: nearest_pref_name(h["lon"], h["lat"]) for hid, h in hexes.items()}


def distribute_population(hexes: dict, pref_of: dict, log=print) -> tuple[dict, dict]:
    """Population per hex (docs/phase8-spec.md section 2: prefecture total,
    `prefecture_population.POPULATION`, spread across that prefecture's
    hexes proportional to `flat_area_weight`) and the raw weight dict
    (reused, unnormalized, as Food's capacity weight below). Asserts each
    prefecture's hex populations sum back to exactly its known total -
    section 4's "各県の人口配分の合計が元の県人口に一致すること"."""
    weight = {hid: flat_area_weight(h) for hid, h in hexes.items()}
    by_pref: dict[str, list] = defaultdict(list)
    for hid, pref in pref_of.items():
        by_pref[pref].append(hid)

    population: dict[tuple, float] = {}
    for pref in sorted(by_pref):
        hids = by_pref[pref]
        total_w = sum(weight[h] for h in hids)
        target = prefecture_population.POPULATION[pref]
        for hid in hids:
            population[hid] = target * weight[hid] / total_w
        achieved = sum(population[h] for h in hids)
        assert abs(achieved - target) < 1e-2, (
            f"population distribution for {pref} sums to {achieved}, not {target}"
        )
    log(f"  population distributed across {len(hexes)} hexes over {len(by_pref)} prefectures "
        f"(every prefecture's hex total matches its known population exactly)")
    return population, weight


def coastal_info(hexes: dict) -> tuple[dict, dict]:
    """hex id -> (is this hex coastal, how many of its 6 lattice directions
    open onto water/off-map). A hex is coastal if part of its own footprint
    is sea (`land_fraction` short of 1.0) or if it geometrically borders at
    least one lattice cell with no adopted hex there at all."""
    coastal: dict[tuple, bool] = {}
    sea_dir_count: dict[tuple, int] = {}
    for hid, h in hexes.items():
        q, r = hid
        cnt = 0
        for dq, dr in hexgrid.AXIAL_DIRECTIONS:
            if (q + dq, r + dr) not in hexes:
                cnt += 1
        coastal[hid] = h["land_fraction"] < constants.COASTAL_LAND_FRACTION_MAX or cnt > 0
        sea_dir_count[hid] = cnt
    return coastal, sea_dir_count


def steel_factor(terrain: Terrain, is_coastal: bool) -> float:
    """docs/phase8-spec.md section 2: Steel "人口と、沿岸・平地であることに比
    例（臨海工業地帯）" - terrain term (flat land favored) times coastal term
    (coastal favored)."""
    if terrain in (Terrain.PLAIN, Terrain.URBAN):
        terrain_f = constants.STEEL_TERRAIN_FACTOR_FLAT
    elif terrain is Terrain.HILL:
        terrain_f = constants.STEEL_TERRAIN_FACTOR_HILL
    else:
        terrain_f = constants.STEEL_TERRAIN_FACTOR_MOUNTAIN
    coastal_f = constants.STEEL_COASTAL_FACTOR_COASTAL if is_coastal else constants.STEEL_COASTAL_FACTOR_INLAND
    return terrain_f * coastal_f


def solve_capacity_coefficients(hexes: dict, population: dict, weight: dict, coastal: dict, log=print) -> dict:
    """Solves every `*_COEF` so the resulting national total exactly hits
    this module's docstring targets (`constants.TARGET_INDUSTRY_TOTAL` split
    by `CAPACITY_SHARE_*`, `constants.TARGET_FOOD_CAPACITY_MULT`) - see
    the module docstring for the arithmetic behind the target shares
    themselves."""
    n_hexes = len(hexes)
    sum_pop = sum(population.values())
    sum_food_w = sum(weight.values())
    sum_steel_w = sum(population[hid] * steel_factor(hexes[hid]["terrain"], coastal[hid]) for hid in hexes)
    sum_machinery_w = sum(population[hid] ** constants.MACHINERY_DENSITY_EXPONENT for hid in hexes)
    sum_arms_w = sum(population[hid] ** constants.ARMS_DENSITY_EXPONENT for hid in hexes)

    total = constants.TARGET_INDUSTRY_TOTAL
    energy_target = total * constants.CAPACITY_SHARE_ENERGY
    energy_pop_target = energy_target * (1.0 - constants.ENERGY_AREA_SHARE)
    energy_area_target = energy_target * constants.ENERGY_AREA_SHARE
    food_target = constants.CIVILIAN_FOOD_DEMAND_PER_POP * prefecture_population.TOTAL_POPULATION * constants.TARGET_FOOD_CAPACITY_MULT

    coef = dict(
        food=food_target / sum_food_w,
        energy_pop=energy_pop_target / sum_pop,
        energy_area=energy_area_target / n_hexes,
        steel=total * constants.CAPACITY_SHARE_STEEL / sum_steel_w,
        machinery=total * constants.CAPACITY_SHARE_MACHINERY / sum_machinery_w,
        munitions=total * constants.CAPACITY_SHARE_MUNITIONS / sum_pop,
        arms=total * constants.CAPACITY_SHARE_ARMS / sum_arms_w,
    )
    log(f"  capacity coefficients solved against national targets: "
        f"food={food_target:.2f} energy={energy_target:.2f} "
        f"steel={total*constants.CAPACITY_SHARE_STEEL:.2f} "
        f"machinery={total*constants.CAPACITY_SHARE_MACHINERY:.2f} "
        f"munitions={total*constants.CAPACITY_SHARE_MUNITIONS:.2f} "
        f"arms={total*constants.CAPACITY_SHARE_ARMS:.2f} (industry_total={total:.2f})")
    return coef


def build_capacities(hexes: dict, population: dict, weight: dict, coastal: dict, coef: dict) -> dict:
    capacities = {}
    for hid, h in hexes.items():
        pop = population[hid]
        capacities[hid] = dict(
            food=coef["food"] * weight[hid],
            energy=coef["energy_pop"] * pop + coef["energy_area"],
            steel=coef["steel"] * pop * steel_factor(h["terrain"], coastal[hid]),
            machinery=coef["machinery"] * (pop ** constants.MACHINERY_DENSITY_EXPONENT),
            munitions=coef["munitions"] * pop,
            arms=coef["arms"] * (pop ** constants.ARMS_DENSITY_EXPONENT),
        )
    return capacities


def apply_urban_override(hexes: dict, population: dict, log=print) -> dict:
    """docs/phase8-spec.md section 1's terrain table, last row: promotes a
    densely-populated `Plain` hex to `Urban` (never `Hill`/`Mountain` -
    `constants.URBAN_POPULATION_MIN`'s doc). Returns hex id -> final
    `Terrain`."""
    terrain_final = {}
    promoted = []
    for hid, h in hexes.items():
        t = h["terrain"]
        if t is Terrain.PLAIN and population[hid] >= constants.URBAN_POPULATION_MIN:
            t = Terrain.URBAN
            promoted.append(hid)
        terrain_final[hid] = t
    log(f"  {len(promoted)} hex(es) promoted Plain -> Urban "
        f"(population >= {constants.URBAN_POPULATION_MIN}万人): "
        f"{sorted(hex_id_str(h) for h in promoted)}")
    return terrain_final


def build_ports(hexes: dict, population: dict, coastal: dict, sea_dir_count: dict) -> dict:
    ports = {}
    for hid in hexes:
        if not coastal[hid]:
            ports[hid] = 0.0
            continue
        v = (
            constants.PORT_BASE
            + constants.PORT_COAST_DIRECTION_COEF * sea_dir_count[hid]
            + constants.PORT_POP_SQRT_COEF * math.sqrt(max(population[hid], 0.0))
        )
        ports[hid] = min(v, constants.PORT_MAX)
    return ports


def build_infrastructure(hexes: dict, population: dict, terrain_final: dict) -> dict:
    infra = {}
    for hid in hexes:
        base = constants.INFRA_BASE_BY_TERRAIN[terrain_final[hid].value]
        v = base + constants.INFRA_POP_SQRT_COEF * math.sqrt(max(population[hid], 0.0))
        infra[hid] = min(max(v, constants.INFRA_MIN), constants.INFRA_MAX)
    return infra


# --- Stage 8B: factions and sea zones ---------------------------------------
# docs/phase8-spec.md section 3.

def resolve_pref_sea_zone(pref: str) -> str:
    """The sea zone a prefecture faces (`sea_zones.PREF_TO_SEA_ZONE`), or -
    for one of the 8 landlocked prefectures, which should never actually own
    a coastal hex but is handled anyway rather than assumed - the zone of
    whichever *other* prefecture with a known zone has the nearest capital."""
    zone = sea_zone_mod.PREF_TO_SEA_ZONE.get(pref)
    if zone is not None:
        return zone
    plat, plon = CAPITALS[pref]
    x0, y0 = hexgrid.lonlat_to_meters(plon, plat)
    best_name, best_d2 = None, None
    for name in sea_zone_mod.PREF_TO_SEA_ZONE:
        clat, clon = CAPITALS[name]
        cx, cy = hexgrid.lonlat_to_meters(clon, clat)
        d2 = (cx - x0) ** 2 + (cy - y0) ** 2
        if best_d2 is None or d2 < best_d2:
            best_name, best_d2 = name, d2
    return sea_zone_mod.PREF_TO_SEA_ZONE[best_name]


PREF_SEA_ZONE = {pref: resolve_pref_sea_zone(pref) for pref in CAPITALS}


def remap_zones_to_real_seas(zones: dict, pref_of: dict, log=print) -> dict:
    """Stage 8A's `apply_named_links`/`bridge_islands` tag every strait/
    bridge link with an ad-hoc zone id (the three named chokepoints'
    `constants.CHOKEPOINTS`/`NAMED_SEA_ROUTES` zone field, or a fabricated
    `strait_h..._h...` id per auto-bridged island). Stage 8B replaces the
    whole sea-zone set with 8 real seas (`sea_zones.SEA_ZONE_INFO`), so
    every one of those ad-hoc ids is remapped onto whichever real zone the
    lexicographically-first endpoint hex's own prefecture faces - for the
    three named chokepoints this reproduces (or improves on: "seikan" -> the
    real `hoppou`) the same zone `scenarios/japan47.json` already uses for
    the same crossings."""
    remapped = {}
    for (a, b), old_zone in zones.items():
        anchor = a if a <= b else b
        remapped[(a, b)] = PREF_SEA_ZONE[pref_of[anchor]]
    log(f"  remapped {len(zones)} strait/bridge link(s) from ad-hoc Stage 8A zone ids onto the 8 real sea zones")
    return remapped


def build_faction_defs(hexes: dict, pref_of: dict, population: dict, log=print):
    """8 regional factions (docs/phase8-spec.md section 3), grouped by
    `regions.PREF_TO_BLOCK`; each faction's capital is its own
    highest-population hex. Returns (factions_json, bloc_a_ids, bloc_b_ids)."""
    by_block: dict[str, list] = defaultdict(list)
    for hid, pref in pref_of.items():
        by_block[region_mod.PREF_TO_BLOCK[pref]].append(hid)

    factions_json = []
    bloc_a, bloc_b = [], []
    for block in sorted(by_block):
        faction_id, faction_name, side = region_mod.BLOCKS[block]
        hids = sorted(by_block[block])
        capital_hid = sorted(hids, key=lambda h: (-population[h], hex_id_str(h)))[0]
        pop_total = sum(population[h] for h in hids)
        factions_json.append({
            "id": faction_id,
            "name": faction_name,
            "capital": hex_id_str(capital_hid),
            "regions": [hex_id_str(h) for h in hids],
        })
        (bloc_a if side == "a" else bloc_b).append(faction_id)
        log(f"  faction {faction_id} ({faction_name}): {len(hids)} hexes, "
            f"population {pop_total:.1f}万人, capital {hex_id_str(capital_hid)}")
    return factions_json, bloc_a, bloc_b


def build_sea_zone_defs(hexes: dict, pref_of: dict, coastal: dict, remapped_link_zones: dict, log=print):
    zone_coast: dict[str, set] = {zid: set() for zid in sea_zone_mod.SEA_ZONE_INFO}
    for hid in hexes:
        if not coastal[hid]:
            continue
        zone_coast[PREF_SEA_ZONE[pref_of[hid]]].add(hid)
    for (a, b), zid in remapped_link_zones.items():
        zone_coast[zid].add(a)
        zone_coast[zid].add(b)

    zones_json = []
    for zid in sorted(sea_zone_mod.SEA_ZONE_INFO):
        name, adjacent = sea_zone_mod.SEA_ZONE_INFO[zid]
        coast = sorted(hex_id_str(h) for h in zone_coast[zid])
        assert coast, f"sea zone {zid} ended up with no coastal hexes"
        zones_json.append({"id": zid, "name": name, "coast": coast, "adjacent": adjacent})
        log(f"  sea zone {zid} ({name}): {len(coast)} coastal hexes")
    return zones_json


def build_scenario_json(
    hexes: dict,
    links: dict,
    remapped_link_zones: dict,
    pref_of: dict,
    population: dict,
    capacities: dict,
    terrain_final: dict,
    ports: dict,
    infra: dict,
    log=print,
) -> dict:
    ordered_ids = sorted(hexes.keys())
    id_of = {hid: hex_id_str(hid) for hid in ordered_ids}

    # Per-hex outgoing link list.
    out_links: dict[tuple[int, int], list[dict]] = {hid: [] for hid in ordered_ids}
    for (a, b), kind in links.items():
        zone = remapped_link_zones.get((a, b))
        out_links[a].append(dict(to=id_of[b], kind=kind, strait_zone=zone))
        out_links[b].append(dict(to=id_of[a], kind=kind, strait_zone=zone))
    for hid in out_links:
        out_links[hid].sort(key=lambda l: l["to"])  # deterministic order

    pref_label_count: dict[str, int] = {}
    region_json = []
    for hid in ordered_ids:
        pname = pref_of[hid]
        pref_label_count[pname] = pref_label_count.get(pname, 0) + 1
        name = f"{pname}{pref_label_count[pname]}"
        links_json = []
        for l in out_links[hid]:
            entry = {"to": l["to"], "kind": l["kind"]}
            if l["strait_zone"] is not None:
                entry["strait_zone"] = l["strait_zone"]
            links_json.append(entry)
        cap = capacities[hid]
        region_json.append({
            "id": id_of[hid],
            "name": name,
            "terrain": terrain_final[hid].value,
            "population": round(population[hid], 3),
            "capacity": {g: round(cap[g], 4) for g in ("food", "energy", "steel", "machinery", "munitions", "arms")},
            "infrastructure": round(infra[hid], 3),
            "port": round(ports[hid], 3),
            "links": links_json,
            "position": [round(hexes[hid]["x_m"] / 1000.0, 2), round(hexes[hid]["y_m"] / 1000.0, 2)],
        })

    coastal, sea_dir_count = coastal_info(hexes)
    sea_zones_json = build_sea_zone_defs(hexes, pref_of, coastal, remapped_link_zones, log)
    factions_json, bloc_a, bloc_b = build_faction_defs(hexes, pref_of, population, log)

    return {
        "regions": region_json,
        "sea_zones": sea_zones_json,
        "factions": factions_json,
        "diplomacy": {
            "blocs": [
                {"id": "alliance_a", "name": "甲軍事同盟", "factions": bloc_a},
                {"id": "alliance_b", "name": "乙軍事同盟", "factions": bloc_b},
            ]
        },
        # Conquest only - deliberately *not* `scenarios/japan47.json`'s
        # `Coalition`/`Domination` trio. `sim::victory_winners`'s `Coalition`
        # fires the instant the *alliance* graph over every still-alive
        # faction is one connected component (a chain of bilateral
        # alliances, not mutual peace with everyone) - with 8 factions and,
        # by this scenario's alternating-side design (regions.py's doc), a
        # war front at nearly every border, the heuristic AI's routine
        # treaty-seeking chains enough Alliance treaties across former
        # enemies to connect that whole graph well inside 720 days (day 148
        # in an early seed-1 trial run) - ending the run in a diplomatic
        # "everyone's allied" Coalition win with no faction eliminated and
        # no territory conquered, long before docs/phase8-spec.md section
        # 4's 720-day war measurement window completes. `Domination` at
        # japan47's 0.75 share is vulnerable to the same mechanism (a large
        # connected alliance component's *combined* territory, not real
        # conquest, can clear 75% of the map on its own). `Conquest`
        # (`alive.len() == 1`) has no such failure mode - it requires
        # actually eliminating 7 of 8 factions, unreachable in 720 days for
        # a population-balanced 8-faction map - and is the same declared
        # form `scenarios/mvp.json` already uses.
        "victory": [{"type": "conquest"}],
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None, help="output scenario JSON path")
    ap.add_argument("--cache-dir", required=True)
    ap.add_argument("--spacing-m", type=float, default=constants.SPACING_M)
    ap.add_argument("--zoom", type=int, default=constants.DEM_ZOOM)
    ap.add_argument("--dry-run", action="store_true", help="compute and report, but do not write the output file")
    args = ap.parse_args()

    layout = hexgrid.HexLayout(spacing_m=args.spacing_m)

    print(f"fetching/caching DEM tiles at zoom {args.zoom} ...")
    tile_range = dem.tile_range_for_japan(args.zoom)
    n_tiles = (tile_range.x1 - tile_range.x0 + 1) * (tile_range.y1 - tile_range.y0 + 1)
    print(f"  tile range x[{tile_range.x0},{tile_range.x1}] y[{tile_range.y0},{tile_range.y1}] = {n_tiles} tiles")
    mosaic = dem.build_mosaic(args.cache_dir, args.zoom, tile_range)

    hexes = build_hexes(mosaic, layout)

    counts = {}
    for h in hexes.values():
        counts[h["terrain"]] = counts.get(h["terrain"], 0) + 1
    print("terrain distribution:")
    for t in Terrain:
        n = counts.get(t, 0)
        print(f"  {t.value:8s} {n:4d} ({100.0*n/len(hexes):.1f}%)")

    links = build_links(hexes)
    uf = UnionFind(hexes.keys())
    for (a, b) in links:
        uf.union(a, b)
    comps_before = uf.components()
    print(f"land components before chokepoint/bridging: {len(comps_before)}")
    print(f"  sizes: {sorted((len(c) for c in comps_before), reverse=True)[:10]}")

    zones = apply_named_links(hexes, links, uf, constants.CHOKEPOINTS)
    zones.update(apply_named_links(hexes, links, uf, constants.NAMED_SEA_ROUTES))
    bridge_islands(hexes, links, zones, uf)

    comps_after = uf.components()
    print(f"land components after chokepoints/named routes/bridging: {len(comps_after)}")
    print(f"  sizes: {sorted((len(c) for c in comps_after), reverse=True)[:10]}")

    hexes, links, zones = drop_disconnected(hexes, links, zones, uf)

    print("Stage 8B: population, capacity, terrain, port, infrastructure, factions, sea zones ...")
    pref_of = assign_prefectures(hexes)
    population, weight = distribute_population(hexes, pref_of, log=print)
    coastal, sea_dir_count = coastal_info(hexes)
    coef = solve_capacity_coefficients(hexes, population, weight, coastal, log=print)
    capacities = build_capacities(hexes, population, weight, coastal, coef)
    terrain_final = apply_urban_override(hexes, population, log=print)
    final_counts: dict = {}
    for t in terrain_final.values():
        final_counts[t] = final_counts.get(t, 0) + 1
    print("final terrain distribution (after Urban override):")
    for t in Terrain:
        n = final_counts.get(t, 0)
        print(f"  {t.value:8s} {n:4d} ({100.0*n/len(hexes):.1f}%)")
    ports = build_ports(hexes, population, coastal, sea_dir_count)
    infra = build_infrastructure(hexes, population, terrain_final)
    remapped_link_zones = remap_zones_to_real_seas(zones, pref_of, log=print)

    scenario = build_scenario_json(
        hexes, links, remapped_link_zones, pref_of, population, capacities, terrain_final, ports, infra, log=print,
    )
    print(f"regions: {len(scenario['regions'])}, sea_zones: {len(scenario['sea_zones'])}, "
          f"factions: {len(scenario['factions'])}")

    total_pop = sum(population.values())
    print(f"national population: {total_pop:.1f}万人 (target {prefecture_population.TOTAL_POPULATION:.1f}万人)")
    for good in ("food", "energy", "steel", "machinery", "munitions", "arms"):
        total = sum(capacities[hid][good] for hid in hexes)
        print(f"national capacity[{good}]: {total:.2f}")
    industry_total_national = sum(
        capacities[hid][g] for hid in hexes for g in ("energy", "steel", "machinery", "munitions", "arms")
    )
    print(f"national industry_total (non-food): {industry_total_national:.2f}")
    print("per-faction industry_total / estimated unit_cap (crates/agents unit_cap = 3 + industry_total/5):")
    region_by_id = {r["id"]: r for r in scenario["regions"]}
    for fac in scenario["factions"]:
        fac_regions = [region_by_id[rid] for rid in fac["regions"]]
        it = sum(r["capacity"][g] for r in fac_regions for g in ("energy", "steel", "machinery", "munitions", "arms"))
        pop = sum(r["population"] for r in fac_regions)
        print(f"  {fac['id']:10s} pop={pop:8.1f} industry_total={it:8.2f} unit_cap~={3.0+it/5.0:5.2f}")

    if args.dry_run:
        print("--dry-run: not writing output")
        return

    out_path = args.out
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(scenario, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
