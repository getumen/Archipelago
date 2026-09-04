"""Terrain classification from per-hex elevation samples.

docs/phase8-spec.md section 1: classify from the *distribution* of
elevation samples inside a hex, not a single mean - a plain mean flattens a
basin ringed by mountains (Kofu, Nagano) into "Mountain", and flattens the
slopes around it into something too tame. Central tendency (median, robust
to a skewed within-hex distribution) and relief (p90-p10 spread) are used
together.
"""

from __future__ import annotations

import constants
from terrain_kind import Terrain


def classify(median_m: float, relief_m: float) -> Terrain:
    if median_m >= constants.MOUNTAIN_MEDIAN_M:
        return Terrain.MOUNTAIN
    if median_m >= constants.HILL_MEDIAN_M or relief_m >= constants.HILL_RELIEF_M:
        return Terrain.HILL
    return Terrain.PLAIN


def link_kind(terrain_a: Terrain, terrain_b: Terrain, median_a: float, median_b: float) -> str:
    """docs/phase8-spec.md section 1: "リンク種別は両端の地形から決める。平地
    同士は Rail、山地が絡めば Road、標高差が大きければ Road"."""
    if terrain_a in constants.RAIL_TERRAINS and terrain_b in constants.RAIL_TERRAINS:
        return "rail"
    if terrain_a is Terrain.MOUNTAIN or terrain_b is Terrain.MOUNTAIN:
        return "road"
    if abs(median_a - median_b) >= constants.ROAD_ELEV_DIFF_M:
        return "road"
    return "rail"
