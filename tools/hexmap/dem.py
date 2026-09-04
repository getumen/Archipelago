"""GSI (Geospatial Information Authority of Japan) elevation tile fetching
and mosaic assembly.

docs/phase8-spec.md section 0: elevation tiles come from
    https://cyberjapandata.gsi.go.jp/xyz/dem_png/{z}/{x}/{y}.png
decoded as
    elevation_m = (R*65536 + G*256 + B) * 0.01, values >= 2**23 are negative
    (subtract 2**24 first). Sea is exactly (128, 0, 0).

Tiles are standard XYZ/slippy-map tiles in Web Mercator. This module fetches
the tiles that cover Japan at a fixed zoom, caches them on disk (so re-runs
never re-fetch), and assembles them into one big mosaic in pixel space with
helper functions to convert between pixel indices and (lon, lat).
"""

from __future__ import annotations

import math
import os
import time
import urllib.error
import urllib.request
from dataclasses import dataclass

import numpy as np
from PIL import Image

TILE_URL = "https://cyberjapandata.gsi.go.jp/xyz/dem_png/{z}/{x}/{y}.png"
TILE_SIZE = 256
USER_AGENT = "Archipelago-mapgen/0.1 (github.com/getumen/Archipelago; hex map generator for a strategy game; contact via repo issues)"
FETCH_DELAY_SECONDS = 0.15

# Japan's rough latitude band, per docs/phase8-spec.md section 1: "日本の緯度帯
# （北緯 24〜46 度）"; longitude band widened slightly on both sides to cover
# Okinawa (~122E) through eastern Hokkaido (~146E) with a small margin so no
# hex near the edge of the band gets starved of tile data.
LON_MIN, LON_MAX = 122.0, 146.5
LAT_MIN, LAT_MAX = 23.5, 46.5


def lonlat_to_tile_frac(lon: float, lat: float, zoom: int) -> tuple[float, float]:
    """Continuous (fractional) tile coordinates for a (lon, lat) at `zoom`."""
    n = 2 ** zoom
    x = (lon + 180.0) / 360.0 * n
    lat_rad = math.radians(lat)
    y = (1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi) / 2.0 * n
    return x, y


def pixel_to_lonlat(gx: np.ndarray, gy: np.ndarray, zoom: int) -> tuple[np.ndarray, np.ndarray]:
    """Vectorized inverse of the slippy-tile pixel mapping: global pixel
    indices (gx, gy) at `zoom` (each tile is TILE_SIZE px) -> (lon, lat) in
    degrees."""
    n = 2 ** zoom
    total_px = n * TILE_SIZE
    lon = gx / total_px * 360.0 - 180.0
    y_frac = gy / total_px
    lat = np.degrees(np.arctan(np.sinh(np.pi * (1.0 - 2.0 * y_frac))))
    return lon, lat


@dataclass(frozen=True)
class TileRange:
    zoom: int
    x0: int
    x1: int  # inclusive
    y0: int
    y1: int  # inclusive

    @property
    def width_px(self) -> int:
        return (self.x1 - self.x0 + 1) * TILE_SIZE

    @property
    def height_px(self) -> int:
        return (self.y1 - self.y0 + 1) * TILE_SIZE


def tile_range_for_japan(zoom: int) -> TileRange:
    x0f, y0f = lonlat_to_tile_frac(LON_MIN, LAT_MAX, zoom)
    x1f, y1f = lonlat_to_tile_frac(LON_MAX, LAT_MIN, zoom)
    return TileRange(zoom=zoom, x0=math.floor(x0f), x1=math.floor(x1f), y0=math.floor(y0f), y1=math.floor(y1f))


def _cache_path(cache_dir: str, zoom: int, x: int, y: int) -> str:
    return os.path.join(cache_dir, f"{zoom}", str(x), f"{y}.png")


def fetch_tile_bytes(cache_dir: str, zoom: int, x: int, y: int) -> bytes | None:
    """Returns raw PNG bytes for tile (zoom, x, y), using the on-disk cache
    first. Returns None for a tile GSI has no data for (404) - fine, it
    means that tile is entirely outside GSI's covered area (e.g. open
    ocean far from any land); we treat it as all-sea when that happens."""
    path = _cache_path(cache_dir, zoom, x, y)
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    url = TILE_URL.format(z=zoom, x=x, y=y)
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = resp.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path + ".missing", "w") as f:
                f.write("404\n")
            return None
        raise
    finally:
        time.sleep(FETCH_DELAY_SECONDS)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    return data


def decode_tile(data: bytes) -> tuple[np.ndarray, np.ndarray]:
    """PNG bytes -> (elevation_m float32 array, is_sea bool array), both
    (TILE_SIZE, TILE_SIZE). Sea pixels get elevation 0.0 (irrelevant; masked
    out by is_sea everywhere elevation is used)."""
    img = Image.open(__import__("io").BytesIO(data)).convert("RGB")
    a = np.asarray(img).astype(np.int64)
    r, g, b = a[:, :, 0], a[:, :, 1], a[:, :, 2]
    is_sea = (r == 128) & (g == 0) & (b == 0)
    raw = r * 65536 + g * 256 + b
    elev = raw.astype(np.float64) * 0.01
    elev = np.where(elev >= (2 ** 23) * 0.01, elev - (2 ** 24) * 0.01, elev)
    elev = np.where(is_sea, 0.0, elev).astype(np.float32)
    return elev, is_sea


def local_roughness(elevation: np.ndarray) -> np.ndarray:
    """(max - min) elevation in each pixel's 3x3 neighborhood - a cheap
    local-slope proxy used to tell "flat valley/basin floor" apart from
    "steep mountainside" independent of absolute elevation (see
    constants.py's FLAT_ROUGHNESS_M doc for why median elevation alone
    can't make that distinction)."""
    h, w = elevation.shape
    padded = np.pad(elevation, 1, mode="edge")
    local_max = elevation.copy()
    local_min = elevation.copy()
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            shifted = padded[1 + dy:1 + dy + h, 1 + dx:1 + dx + w]
            local_max = np.maximum(local_max, shifted)
            local_min = np.minimum(local_min, shifted)
    return local_max - local_min


@dataclass
class Mosaic:
    zoom: int
    x0: int
    y0: int
    elevation: np.ndarray  # (H, W) float32
    is_sea: np.ndarray  # (H, W) bool
    lon: np.ndarray  # (H, W) float64, per-pixel center longitude
    lat: np.ndarray  # (H, W) float64, per-pixel center latitude


def build_mosaic(cache_dir: str, zoom: int, tile_range: TileRange, log=print) -> Mosaic:
    width = tile_range.width_px
    height = tile_range.height_px
    elevation = np.zeros((height, width), dtype=np.float32)
    is_sea = np.ones((height, width), dtype=bool)  # default: treat missing tiles as sea

    n_tiles = (tile_range.x1 - tile_range.x0 + 1) * (tile_range.y1 - tile_range.y0 + 1)
    done = 0
    for tx in range(tile_range.x0, tile_range.x1 + 1):
        for ty in range(tile_range.y0, tile_range.y1 + 1):
            data = fetch_tile_bytes(cache_dir, zoom, tx, ty)
            done += 1
            if data is not None:
                tile_elev, tile_sea = decode_tile(data)
                ox = (tx - tile_range.x0) * TILE_SIZE
                oy = (ty - tile_range.y0) * TILE_SIZE
                elevation[oy:oy + TILE_SIZE, ox:ox + TILE_SIZE] = tile_elev
                is_sea[oy:oy + TILE_SIZE, ox:ox + TILE_SIZE] = tile_sea
            if done % 20 == 0 or done == n_tiles:
                log(f"  dem tiles {done}/{n_tiles}")

    # Per-pixel (lon, lat) of the pixel *center* (+0.5 offset).
    gx = np.arange(width, dtype=np.float64) + tile_range.x0 * TILE_SIZE + 0.5
    gy = np.arange(height, dtype=np.float64) + tile_range.y0 * TILE_SIZE + 0.5
    gx2, gy2 = np.meshgrid(gx, gy)
    lon, lat = pixel_to_lonlat(gx2, gy2, zoom)

    return Mosaic(zoom=zoom, x0=tile_range.x0, y0=tile_range.y0, elevation=elevation, is_sea=is_sea, lon=lon, lat=lat)
