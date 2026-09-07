"""Real airport data (Stage 10A, docs/phase10-spec.md "1. 基地": airfield
placement must be derived, not guessed). Companion to `port_data.py`/
`rail_data.py`; see `rail_data.py`'s own doc for the overall "use what real
国土数値情報 data supports" rationale this repeats one domain further.

## Data source and its licence

国土数値情報（空港）C28-21, FY2021 edition (the current one at generation
time - `KsjTmplt-C28-2021.html` is the newest listed). Its own page states
`このデータの使用許諾条件: 商用可` - unlike `port_data.py`'s C02 (an older,
non-commercial, no-redistribution licence), C28 carries no such
restriction: the download site's own terms of use
(https://nlftp.mlit.go.jp/ksj/other/agreement.html) apply the government's
公共データ利用規約（第1.0版）(PDL 1.0) by default to any 国土数値情報
dataset that does not declare its own different licence - permissive,
attribution required, commercial use and redistribution of *derived* work
both allowed (this is a looser, differently-worded permission than
`rail_data.py`'s N02, which separately declares itself "オープンデータ
（CC BY 4.0）" outright; both permit what this module does).

    Source page:     https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-C28-2021.html
    Direct download: https://nlftp.mlit.go.jp/ksj/gml/data/C28/C28-21/C28-21_GML.zip
    Attribution:     出典：国土交通省 国土数値情報（空港データ）C28-21
                      https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-C28-2021.html

As with `port_data.py`, only a small, heavily-reduced *derived fact* per
hex - which of ~100 real, named airports is closest, and whether it is
close enough to trust - ever reaches the committed scenario; no geometry,
runway length, operating-hours or administrator field survives into
`scenarios/japan_hex.json`. C28's own licence does not require this
reduction the way C02's does, but the same discipline (raw survey data
cached under `--cache-dir`, never committed - `build_scenario.py`'s own
`.gitignore`-style handling) is kept anyway, for the same reason `dem.py`'s
raw elevation tiles never reach the repository either: a big third-party
GIS dataset does not belong in a game's source tree just because its
licence would tolerate it.

## What the schema supports

The zip ships pre-built GeoJSON for every layer (unlike C02's shapefile
-only C02, no `.shp`/`.dbf` parsing needed here). Two layers matter:

- `UTF-8/C28-21_Airport.geojson`: one polygon per airport (in one case,
  more than one - `福岡空港`/`熊本空港`/`稚内空港`/`紋別空港`/`鳥取空港` each
  have 2-4 records, evidently per administrative sub-area; they all
  resolve to essentially the same point once joined below, so this module
  never needs to deduplicate them itself - the nearest-hex match already
  collapses them). Carries `C28_004` (供用中/建設中/休止中 - only 供用中,
  "in service", is kept; one nationwide record is 休止中 in the FY2021
  edition), `C28_005` (name) and `C28_101` (a `#`-prefixed foreign key into
  the reference-point layer below - never its own coordinate).
- `UTF-8/C28-21_AirportReferencePoint.geojson`: one `Point` per airport,
  keyed by `C28_000` (matching `C28_101` with the leading `#` stripped) -
  this is the airport's own representative point MLIT itself designates,
  used directly rather than computing a polygon centroid.

`C28_003` (`InstallAirPortCd`, the airport's administrative management
category: 1-4 are `拠点空港` tiers by managing body, 5 `その他の空港`, 6
`共用空港` i.e. shared civil/military) is *not* used to grade a matched
hex's connecting line capacity the way `port_data.py`'s `C02_002` grades a
port's. That tier is a considered official *size* hierarchy (this module's
own sibling explains why using it is legitimate, not invented); C28_003 by
contrast is an *administrative management* classification - who runs the
airport, not how much traffic it can bear - and reordering categories 1-6
by assumed strategic value (a shared civil/military base is often a large
running strip, an "other" airport is often a private strip) would be this
module's own opinion, not MLIT's. `tools/hexmap/transport_real.py` prices
every matched airfield's connecting line at one flat baseline instead
(`AIRFIELD_LINK_CAPACITY`, shared with the mechanical placeholder
`tools/transport_network.py` uses for `mvp.json`/`japan47.json`) - the same
"no legitimate ordinal signal, so don't invent one" basis `ROAD_CAPACITY`
already uses for a rail/road link with no real classification behind it.
"""

from __future__ import annotations

import io
import os
import urllib.request
import zipfile

import numpy as np

import hexgrid
import json as jsonlib

C28_URL = "https://nlftp.mlit.go.jp/ksj/gml/data/C28/C28-21/C28-21_GML.zip"
C28_AIRPORT_MEMBER = "UTF-8/C28-21_Airport.geojson"
C28_REFPOINT_MEMBER = "UTF-8/C28-21_AirportReferencePoint.geojson"
USER_AGENT = "Archipelago-mapgen/0.1 (github.com/getumen/Archipelago; hex map generator for a strategy game; contact via repo issues)"

# The search radius for "this real airport belongs to this hex" (existence,
# unlike `port_data.py`'s `MATCH_RADIUS_M`/`NAME_MATCH_RADIUS_M`, which
# both refine an *already-yes* hex - Phase 8 never decided which hexes have
# an airfield, this module is the sole source of that decision). Calibrated
# against this repo's own generation run: at 50km, 82 of 107 in-service
# airports (into 64 distinct hexes - several real airports, e.g. Osaka's
# 伊丹/関西/神戸, share one hex) find a real match; every miss beyond that
# is a small, genuinely remote-island airport (奥尻, 種子島, 対馬, the
# entire 南西諸島 chain from 奄美 south) whose own hex either sits outside
# this map's 289-hex land grid or is tens to hundreds of km from the
# nearest one - a real absence, not a matching-radius artifact (the next
# miss past 50km is already 58.8km; misses then climb steadily past 400km).
MATCH_RADIUS_M = 50_000.0


class AirportIndex:
    """All real, in-service C28 airports, projected into the same metre
    space `hexgrid` uses, ready for nearest-hex queries."""

    def __init__(self, xy: np.ndarray, name: list[str]):
        self.xy = xy  # (N, 2) float64
        self.name = name  # length-N list[str]


def _cache_path(cache_dir: str) -> str:
    return os.path.join(cache_dir, "geodata", "C28-21_GML.zip")


def fetch_c28_zip_bytes(cache_dir: str, log=print) -> bytes:
    path = _cache_path(cache_dir)
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    log(f"  fetching {C28_URL} ...")
    req = urllib.request.Request(C28_URL, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = resp.read()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    return data


def load_airport_index(cache_dir: str, log=print) -> AirportIndex:
    data = fetch_c28_zip_bytes(cache_dir, log=log)
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        airport_geojson = jsonlib.loads(zf.read(C28_AIRPORT_MEMBER).decode("utf-8"))
        refpoint_geojson = jsonlib.loads(zf.read(C28_REFPOINT_MEMBER).decode("utf-8"))

    refpoint_by_id: dict[str, tuple[float, float]] = {}
    for feature in refpoint_geojson["features"]:
        rid = feature["properties"]["C28_000"]
        lon, lat = feature["geometry"]["coordinates"]
        refpoint_by_id[rid] = (lon, lat)

    xy: list[tuple[float, float]] = []
    name: list[str] = []
    skipped_no_refpoint = 0
    for feature in airport_geojson["features"]:
        props = feature["properties"]
        if props["C28_004"] != "供用中":
            continue  # 建設中/休止中 - not a usable airfield today
        ref_id = props["C28_101"].lstrip("#")
        coord = refpoint_by_id.get(ref_id)
        if coord is None:
            skipped_no_refpoint += 1
            continue
        lon, lat = coord
        xy.append(hexgrid.lonlat_to_meters(lon, lat))
        name.append(props["C28_005"])

    if skipped_no_refpoint:
        log(f"  C28 airport: {skipped_no_refpoint} in-service record(s) had no matching reference point - skipped")
    log(f"  C28 airport: loaded {len(xy)} real in-service airports")
    return AirportIndex(np.array(xy), name)


def match_airfield_hexes(
    airport_index: AirportIndex,
    hex_meters: dict[str, tuple[float, float]],
    log=print,
) -> dict[str, str]:
    """For every real in-service airport within `MATCH_RADIUS_M` of some
    hex's centre, that hex's id -> the nearest such airport's own name
    (first-seen wins if two airports are both closest to the same hex -
    ties are vanishingly rare at this radius and the identity of which
    *name* labels the hex is cosmetic; which hexes get an `Airfield` node
    at all is the only fact `transport_real.py` actually acts on). Hexes
    with no real airport that close simply get no entry - never a
    least-important-tier fallback the way `port_data.py`'s hex-side match
    does, since unlike a port's *size*, there is no such thing as a
    reasonable default airfield to invent."""
    hex_ids = list(hex_meters.keys())
    hex_xy = np.array([hex_meters[h] for h in hex_ids])

    matched: dict[str, str] = {}
    unmatched: list[tuple[str, float, str]] = []
    for i in range(len(airport_index.name)):
        d = np.hypot(hex_xy[:, 0] - airport_index.xy[i, 0], hex_xy[:, 1] - airport_index.xy[i, 1])
        j = int(np.argmin(d))
        dist = float(d[j])
        if dist <= MATCH_RADIUS_M:
            hid = hex_ids[j]
            if hid not in matched:
                matched[hid] = airport_index.name[i]
        else:
            unmatched.append((airport_index.name[i], dist / 1000.0, hex_ids[j]))

    log(f"  C28 airport: matched {len(matched)} hex(es) to a real airfield within {MATCH_RADIUS_M / 1000.0:.0f}km")
    if unmatched:
        names = ", ".join(f"{n} ({d:.0f}km from {h})" for n, d, h in sorted(unmatched, key=lambda x: x[1])[:5])
        log(f"  C28 airport: {len(unmatched)} real airport(s) too far from any hex to match (nearest 5: {names}, ...)")

    return matched
