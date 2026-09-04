"""Hex grid geometry: a pointy-top axial hex lattice laid over Japan using a
simple equirectangular projection with a fixed-latitude cos() correction
(docs/phase8-spec.md section 1: "投影は簡易でよい。緯度経度を等距離円筒に近似
し、日本の緯度帯（北緯 24〜46 度）で経度方向を cos(lat) 補正する").

All neighbor distances in this lattice are exactly `spacing_m` apart by
construction (redblobgames-style axial coordinates), which is what makes
"is this hex a geometric neighbor of that hex" a simple lookup over 6 fixed
axial offsets rather than a nearest-neighbor search.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy as np

# Reference latitude for the fixed cos() longitude correction - the middle
# of Japan's latitude band, so the single scale factor used for the whole
# grid is least wrong at the center and only mildly wrong at the ends
# (Wakkanai ~45.5N, Yonaguni ~24.5N).
REF_LAT = 35.0
LON0 = 130.0  # arbitrary origin, only affects the constant offset of x_m
LAT0 = 35.0  # arbitrary origin, only affects the constant offset of y_m

M_PER_DEG_LAT = 111320.0
M_PER_DEG_LON_AT_REF = 111320.0 * math.cos(math.radians(REF_LAT))

# The 6 axial neighbor offsets for a pointy-top hex lattice (redblobgames
# "Hexagonal Grids" convention).
AXIAL_DIRECTIONS = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)]


def lonlat_to_meters(lon, lat):
    x_m = (lon - LON0) * M_PER_DEG_LON_AT_REF
    y_m = (lat - LAT0) * M_PER_DEG_LAT
    return x_m, y_m


def meters_to_lonlat(x_m, y_m):
    lon = LON0 + x_m / M_PER_DEG_LON_AT_REF
    lat = LAT0 + y_m / M_PER_DEG_LAT
    return lon, lat


@dataclass(frozen=True)
class HexLayout:
    spacing_m: float

    @property
    def size(self) -> float:
        """Center-to-vertex distance. `spacing_m` is the fixed center-to-
        center distance between any two axial-adjacent hexes, which for a
        regular pointy-top hex lattice equals size * sqrt(3)."""
        return self.spacing_m / math.sqrt(3.0)

    def axial_to_meters(self, q, r):
        size = self.size
        x = size * math.sqrt(3.0) * (q + r / 2.0)
        y = size * 1.5 * r
        return x, y

    def axial_to_meters_vec(self, q: np.ndarray, r: np.ndarray):
        size = self.size
        x = size * math.sqrt(3.0) * (q + r / 2.0)
        y = size * 1.5 * r
        return x, y

    def meters_to_axial_frac(self, x, y):
        size = self.size
        q = (math.sqrt(3.0) / 3.0 * x - 1.0 / 3.0 * y) / size
        r = (2.0 / 3.0 * y) / size
        return q, r

    def meters_to_axial_frac_vec(self, x: np.ndarray, y: np.ndarray):
        size = self.size
        q = (math.sqrt(3.0) / 3.0 * x - 1.0 / 3.0 * y) / size
        r = (2.0 / 3.0 * y) / size
        return q, r


def cube_round_vec(q: np.ndarray, r: np.ndarray):
    """Vectorized cube-coordinate rounding to the nearest hex (redblobgames
    "Hex Rounding") - this is exactly what makes nearest-hex-center
    assignment of a raster of pixels produce the exact hexagonal Voronoi
    partition of the lattice, not an approximation."""
    x = q
    z = r
    y = -x - z
    rx = np.round(x)
    ry = np.round(y)
    rz = np.round(z)

    x_diff = np.abs(rx - x)
    y_diff = np.abs(ry - y)
    z_diff = np.abs(rz - z)

    x_is_max = (x_diff > y_diff) & (x_diff > z_diff)
    y_is_max = (~x_is_max) & (y_diff > z_diff)
    # else z_is_max

    rx_fixed = np.where(x_is_max, -ry - rz, rx)
    rz_fixed = np.where((~x_is_max) & (~y_is_max), -rx - ry, rz)
    # ry not needed further; q = rx_fixed, r = rz_fixed
    return rx_fixed.astype(np.int64), rz_fixed.astype(np.int64)


def axial_distance(a: tuple[int, int], b: tuple[int, int]) -> int:
    aq, ar = a
    bq, br = b
    ax, az = aq, ar
    ay = -ax - az
    bx, bz = bq, br
    by = -bx - bz
    return max(abs(ax - bx), abs(ay - by), abs(az - bz))
