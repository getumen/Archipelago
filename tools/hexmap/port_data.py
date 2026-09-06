"""Real port data (Stage 9C, docs/phase9-spec.md "3. データ": "...港湾データ
から導出する"). Companion to `rail_data.py`; see that module's doc for the
overall Stage 9C rationale.

## Data source and its licence - why this module is careful about what it
## emits

国土数値情報（港湾）C02, FY2014 edition (the most recent; FY2006/2008/2014
are the only three C02 editions MLIT has published). Unlike `rail_data.py`'s
N02 (CC BY 4.0 since FY2020), C02 is licensed under the *older*,
non-commercial term: "出典・加工者等表示のうえ、原著作者等の許諾上、非商用
目的のみでの利用（ただし複製物の再配布を除く）が可能" - usable for
non-commercial purposes with attribution, **excluding redistribution of
copies** of the dataset itself.

    Source page:     https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-C02-v3_2.html
    Direct download: https://nlftp.mlit.go.jp/ksj/gml/data/C02/C02-14/C02-14_GML.zip
    Attribution:     出典：国土交通省 国土数値情報（港湾データ）C02-14
                      https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-C02-v3_2.html

This repository (an unpaid hobby/open-source project) qualifies as
non-commercial use, but "複製物の再配布" (redistribution of copies) is
explicitly excluded regardless of purpose - so the raw shapefile geometry
must never end up in `scenarios/japan_hex.json`, which *is* redistributed
(a public git repository). This module follows exactly the precedent
`dem.py` already set for GSI's own elevation tiles: the raw survey data
(here, the zip; there, the PNG tiles) is fetched once and cached on disk
under `--cache-dir` (never committed - see `build_scenario.py`'s own
`.gitignore`-style handling of that directory), and only a small,
heavily-reduced *derived fact* per hex - which of ~1000 real, named ports is
closest, its one-digit official classification code, and a name string -
reaches the committed scenario. That is not a copy of the dataset (no
geometry, no facility-length figures, no administrative fields survive into
the emitted JSON); it is the same kind of reduction `dem.py`'s raw elevation
tiles undergo before becoming one `Terrain` enum value per hex.

## What the schema supports

C02 has no shapefile-native GeoJSON export (unlike N02), so this module
parses the raw Point shapefile (`.shp`) and its attribute table (`.dbf`)
directly with `struct` - both formats are simple enough (a point shapefile
record is 20 fixed bytes; this dataset's DBF has only fixed-width character
fields) that adding a shapefile-library dependency for ~1000 points is not
worth it, consistent with this tool directory's existing "numpy + Pillow
only" footprint (docs/phase8-spec.md §0).

Each port point carries `C02_002`, "港湾種別（２）コード" - the official
classification under 港湾法 (verified against known real examples: 横浜/
東京/川崎/大阪/神戸 all come back "11"=国際戦略港湾; 名古屋/苫小牧/北九州/
博多 all come back "12"=国際拠点港湾 - exactly their real designations):

    11  国際戦略港湾 (international strategic port)   -  5 nationwide
    12  国際拠点港湾 (international hub port)          - 18 nationwide
    13  重要港湾 (major port)
    14  地方港湾 (local port)
    15  56条港湾 (a minor statutory category)
    99  その他 (unclassified)

`C02_011`/`C02_012` (breakwater/mooring-facility length, metres) also exist
in the schema but are not used: converting an unnormalized metre figure into
a game "capacity" number would be its own arbitrary tuning choice no less
than hand-picking a constant, so this module sticks to the *ordinal* official
classification, which is already MLIT's own considered judgement of a port's
relative importance.
"""

from __future__ import annotations

import io
import os
import struct
import urllib.request
import zipfile

import numpy as np

import hexgrid

C02_URL = "https://nlftp.mlit.go.jp/ksj/gml/data/C02/C02-14/C02-14_GML.zip"
C02_SHP_MEMBER = "C02-14_GML/C02-14-g_PortAndHarbor.shp"
C02_DBF_MEMBER = "C02-14_GML/C02-14-g_PortAndHarbor.dbf"
USER_AGENT = "Archipelago-mapgen/0.1 (github.com/getumen/Archipelago; hex map generator for a strategy game; contact via repo issues)"

# A hex's port-bearing status is entirely Phase 8's own (`Region::port > 0`,
# derived from population/coastline, unrelated to this module) - Stage 9C
# only refines that hex's Port node/line *capacity*. This is the search
# radius for "the nearest real, named port to this hex's centre" (both
# directions: hex->nearest-port for capacity, and port->nearest-hex for
# identifying which hex a specific major port like 横浜 corresponds to).
# Calibrated against this repo's own generation run: at 50km every
# port-bearing hex except 2 (both remote Hokkaido coastline with sparse
# registered ports) found a real match; the unmatched pair is reported, not
# silently guessed at (`transport_real.py`'s own log output).
MATCH_RADIUS_M = 50_000.0

# Tighter radius for the reverse direction - naming a specific hex after a
# specific *major* (tier 11/12) port by name, only when confident. All 23
# real tier-11/12 ports matched within 30km in this repo's own generation
# run (see the module report); a looser radius here would risk mislabeling
# a hex after a port that actually belongs to a neighboring one.
NAME_MATCH_RADIUS_M = 30_000.0


def _cache_path(cache_dir: str) -> str:
    return os.path.join(cache_dir, "geodata", "C02-14_GML.zip")


def fetch_c02_zip_bytes(cache_dir: str, log=print) -> bytes:
    path = _cache_path(cache_dir)
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    log(f"  fetching {C02_URL} ...")
    req = urllib.request.Request(C02_URL, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = resp.read()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    return data


def _read_shp_points(data: bytes) -> list[tuple[float, float]]:
    """Minimal ESRI Shapefile reader, Point (shape type 1) records only -
    exactly what C02's PortAndHarbor layer contains. Main file header is
    100 bytes; each record is an 8-byte record header (record number,
    content length in 16-bit words, both big-endian) followed by a
    little-endian int32 shape type and, for Point, two little-endian
    float64s (x, y)."""
    (file_len_words,) = struct.unpack(">i", data[24:28])
    file_len_bytes = file_len_words * 2
    pos = 100
    points: list[tuple[float, float]] = []
    while pos < file_len_bytes:
        _rec_num, content_len_words = struct.unpack(">ii", data[pos : pos + 8])
        pos += 8
        content_len_bytes = content_len_words * 2
        (shape_type,) = struct.unpack("<i", data[pos : pos + 4])
        if shape_type != 1:
            raise ValueError(f"expected Point (shape type 1), got {shape_type} - C02 schema must have changed")
        x, y = struct.unpack("<dd", data[pos + 4 : pos + 20])
        points.append((x, y))
        pos += content_len_bytes
    return points


def _read_dbf(data: bytes, encoding: str = "cp932") -> list[dict[str, str]]:
    """Minimal xBase (.dbf) reader, character fields only - all of C02's
    fields (`C02_001`..`C02_013`) are type 'C'. `cp932` (a superset of
    Shift-JIS) matches the encoding MLIT ships this dataset's DBF in."""
    (num_records,) = struct.unpack("<I", data[4:8])
    (header_len,) = struct.unpack("<H", data[8:10])
    (record_len,) = struct.unpack("<H", data[10:12])
    fields: list[tuple[str, int]] = []
    pos = 32
    while data[pos] != 0x0D:
        name = data[pos : pos + 11].split(b"\x00")[0].decode("ascii")
        length = data[pos + 16]
        fields.append((name, length))
        pos += 32
    records = []
    rec_start = header_len
    for _ in range(num_records):
        raw = data[rec_start : rec_start + record_len]
        rec_start += record_len
        off = 1  # byte 0 is the deletion flag
        values: dict[str, str] = {}
        for name, length in fields:
            values[name] = raw[off : off + length].decode(encoding).strip()
            off += length
        records.append(values)
    return records


class PortIndex:
    """All ~1000 real C02 ports, projected into the same metre space
    `hexgrid` uses, ready for nearest-neighbour queries in either
    direction."""

    def __init__(self, xy: np.ndarray, tier: np.ndarray, name: list[str]):
        self.xy = xy  # (N, 2) float64
        self.tier = tier  # (N,) int, one of {11,12,13,14,15,99}
        self.name = name  # length-N list[str]

    def nearest_to(self, x_m: float, y_m: float) -> tuple[int, float]:
        """Index of, and distance (m) to, the real port nearest a point."""
        d = np.hypot(self.xy[:, 0] - x_m, self.xy[:, 1] - y_m)
        i = int(np.argmin(d))
        return i, float(d[i])


def load_port_index(cache_dir: str, log=print) -> PortIndex:
    data = fetch_c02_zip_bytes(cache_dir, log=log)
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        shp = zf.read(C02_SHP_MEMBER)
        dbf = zf.read(C02_DBF_MEMBER)
    points = _read_shp_points(shp)
    records = _read_dbf(dbf)
    if len(points) != len(records):
        raise ValueError(f"C02 shp/dbf record count mismatch: {len(points)} vs {len(records)}")

    xy = np.array([hexgrid.lonlat_to_meters(lon, lat) for lon, lat in points])
    tier = np.array([int(r["C02_002"]) for r in records])
    name = [r["C02_005"] for r in records]
    log(f"  C02 port: loaded {len(points)} real ports")
    return PortIndex(xy, tier, name)


def match_hex_port_tiers(
    port_index: PortIndex,
    hex_meters: dict[str, tuple[float, float]],
    port_hex_ids: set[str],
    log=print,
) -> dict[str, int]:
    """For every hex id in `port_hex_ids` (Phase 8's own `Region::port > 0`
    set - this module never decides which hexes have a port, only how big),
    the classification code (`C02_002`) of the nearest real port within
    `MATCH_RADIUS_M`. A hex with no real port that close is omitted - the
    caller falls back to the least-important real tier (14, 地方港湾) for
    it, an explicit, logged, disclosed default (not a silent invention of a
    specific real port that doesn't exist)."""
    result: dict[str, int] = {}
    unmatched: list[str] = []
    for hid in sorted(port_hex_ids):
        x, y = hex_meters[hid]
        i, dist = port_index.nearest_to(x, y)
        if dist <= MATCH_RADIUS_M:
            result[hid] = int(port_index.tier[i])
        else:
            unmatched.append(hid)
    log(f"  C02 port: matched {len(result)}/{len(port_hex_ids)} port-bearing hexes to a real port within {MATCH_RADIUS_M/1000:.0f}km")
    if unmatched:
        log(f"    unmatched (no real port that close - using 地方港湾/14 baseline): {unmatched}")
    return result


def match_major_port_names(
    port_index: PortIndex,
    hex_meters: dict[str, tuple[float, float]],
    port_hex_ids: set[str],
    log=print,
) -> dict[str, str]:
    """For every real tier-11/12 (国際戦略港湾/国際拠点港湾) port, the
    nearest port-bearing hex within `NAME_MATCH_RADIUS_M`, so that hex's
    Port node can be labelled with the real port's name (Stage 9C
    verification: "主要港（横浜・名古屋・神戸・北九州・苫小牧）がノードとし
    て存在すること"). Where two real major ports land on the same hex (e.g.
    大阪 and 堺泉北 both nearest the same hex), the higher-tier (numerically
    lower `C02_002`) one wins; ties keep the closer one."""
    hex_list = sorted(port_hex_ids)
    hex_xy = np.array([hex_meters[h] for h in hex_list])

    best: dict[str, tuple[int, float, str]] = {}  # hid -> (tier, dist, name)
    for i in range(len(port_index.name)):
        tier = int(port_index.tier[i])
        if tier not in (11, 12):
            continue
        d = np.hypot(hex_xy[:, 0] - port_index.xy[i, 0], hex_xy[:, 1] - port_index.xy[i, 1])
        j = int(np.argmin(d))
        dist = float(d[j])
        if dist > NAME_MATCH_RADIUS_M:
            continue
        hid = hex_list[j]
        prior = best.get(hid)
        if prior is None or (tier, dist) < (prior[0], prior[1]):
            best[hid] = (tier, dist, port_index.name[i])

    result = {hid: name for hid, (_tier, _dist, name) in best.items()}
    log(f"  C02 port: {len(result)} hex(es) identified as a real major (tier 11/12) port by name: {sorted(result.items())}")
    return result
