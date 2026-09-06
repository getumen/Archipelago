"""Real railway line data (Stage 9C, docs/phase9-spec.md "3. データ": "japan_
hex の路線は実データから導出する"), the transport-network counterpart of
`dem.py`'s elevation tiles - `docs/phase8-spec.md`'s "地形を推測で置かない"
applied here to routes instead of terrain, per Stage 9C's brief.

## Data source

国土数値情報（鉄道）N02, FY2024 edition (the most recent at generation time).
Per the dataset's own listing page, editions from FY2020 onward are licensed
"オープンデータ（CC_BY_4.0）" (open data, CC BY 4.0) - permissive, attribution
required, no non-commercial or no-redistribution restriction (contrast
`port_data.py`'s C02, which *is* restricted, and is handled differently for
exactly that reason - see that module's own doc).

    Source page:     https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-N02-2024.html
    Direct download: https://nlftp.mlit.go.jp/ksj/gml/data/N02/N02-24/N02-24_GML.zip
    Attribution:     出典：国土交通省 国土数値情報（鉄道データ）N02-24 (CC BY 4.0)
                      https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-N02-2024.html

We read only the `UTF-8/N02-24_RailroadSection.geojson` member the zip
already ships (MLIT's own GeoJSON re-encoding of the same shapefile) - no
shapefile/DBF parsing needed for this dataset, unlike `port_data.py`'s C02.

## What the schema supports, and what it doesn't

Each feature carries `N02_001` (line category code), `N02_003` (line name)
and `N02_004` (operator name). There is no double-track or electrification
attribute in this schema at all - Stage 9C's brief ("use what the data
supports") is why capacity tiers below use only category + name + a
length-derived trunk/branch split, not track count or electrification.

`N02_001` codes (verified against the actual FY2024 data, not the product
spec's prose, since a few codes needed disambiguating by example):

    11, 12  ordinary railway (11 = JR, 12 = non-JR: third-sector, private)
    13-25   cable car, aerial tramway, monorail/AGT, subway, streetcar and
            other intra-city or short special-purpose systems

Only 11/12 are kept - the rest are short, intra-city systems irrelevant to
a >=40km hex corridor (a subway line essentially never leaves the hex it
starts in).

Within {11, 12}, three tiers:

  - `shinkansen`: line name contains "新幹線". Unambiguous in this dataset -
    every Shinkansen line is filed under `N02_001 == "11"` with "新幹線" in
    its name (verified: 上越/九州/北海道/北陸/山陽/東北/東海道/西九州新幹線,
    exactly the 8 lines in service in FY2024, nothing else matches).
  - `trunk`: `N02_001 == "11"` (JR) and the line's *national total length*
    (summed across every same-named feature anywhere in Japan) is at least
    `TRUNK_LENGTH_THRESHOLD_M`. This dataset registers conventional JR main
    lines with "本" dropped from their name (e.g. "東海道線", not "東海道
    本線" - checked directly against the raw data), so there is no literal
    name string to match against without hardcoding a list from memory
    (exactly what Stage 9C forbids); total route length is the mechanical,
    data-only stand-in, and it recovers the right lines without any list:
    by length, the top of the table is 東海道線/山陰線/東北新幹線/東北線/
    山陽線/山陽新幹線/東海道新幹線/奥羽線/函館線/日豊線/中央線/北陸新幹線/
    紀勢線/根室線/常磐線/... - genuine trunk lines, not an artifact of the
    threshold (see this module's own `report()` for the measured
    distribution this repo's generation run actually saw).
  - `branch`: everything else in {11, 12} - short JR lines and every non-JR
    "12" line regardless of length (a long private line, e.g. 名古屋本線 at
    ~100km, is still a regional operator's own line, not part of the
    national JR trunk backbone this tier is meant to identify).

## How a tier becomes a hex-to-hex capacity signal

`build_scenario.py` already has a fixed hex adjacency graph (`links`, one
entry per geometrically-adjacent hex pair, Phase 8) - Stage 9C does not add
or remove edges from it, only asks "does a real railway of this tier cross
*this* pair's shared boundary". Each qualifying feature's LineString(s) are
interpolated at `SAMPLE_STEP_M` and every sample point assigned to its
nearest hex (`hexgrid.cube_round_vec`, the same nearest-hex-center technique
`build_scenario.py`'s own `build_hexes` uses for DEM pixels). Wherever two
consecutive samples land in different, adjacent hexes, that unordered hex
pair is credited with this feature's tier (keeping the best tier seen if
multiple qualifying lines cross the same pair). A pair with no real-rail
evidence at all simply gets no entry - `transport_real.py` reads that as
"no real railway here", not as a data gap to guess at.
"""

from __future__ import annotations

import json
import math
import os
import urllib.request
import zipfile
from collections import defaultdict

import numpy as np

import hexgrid

N02_URL = "https://nlftp.mlit.go.jp/ksj/gml/data/N02/N02-24/N02-24_GML.zip"
N02_GEOJSON_MEMBER = "UTF-8/N02-24_RailroadSection.geojson"
USER_AGENT = "Archipelago-mapgen/0.1 (github.com/getumen/Archipelago; hex map generator for a strategy game; contact via repo issues)"

# Ordinary-railway category codes this module keeps (see module doc).
RAIL_CATEGORY_CODES = {"11", "12"}
JR_CATEGORY_CODE = "11"

# A JR (code 11) line whose *national* total length is at least this is
# `trunk` tier - see module doc for why length, not name, and this repo's
# generation report for the measured distribution this threshold actually
# separates (a comfortable gap between genuine main lines and everything
# shorter, not a value hand-picked for a desired game outcome).
TRUNK_LENGTH_THRESHOLD_M = 250_000.0

# Interpolation step along each line, comfortably finer than a hex's own
# ~40km spacing so a straight stretch can never skip over an intermediate
# hex undetected.
SAMPLE_STEP_M = 5_000.0

TIER_RANK = {"branch": 1, "trunk": 2, "shinkansen": 3}


def _cache_path(cache_dir: str) -> str:
    return os.path.join(cache_dir, "geodata", "N02-24_GML.zip")


def fetch_n02_zip_bytes(cache_dir: str, log=print) -> bytes:
    """Downloads-and-caches N02-24_GML.zip, the same on-disk-cache-first
    discipline `dem.py`'s `fetch_tile_bytes` uses for GSI tiles - re-runs of
    `build_scenario.py` never re-fetch once the zip is cached."""
    path = _cache_path(cache_dir)
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    log(f"  fetching {N02_URL} ...")
    req = urllib.request.Request(N02_URL, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=120) as resp:
        data = resp.read()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    return data


def _flatten_parts(geometry: dict) -> list[list[list[float]]]:
    t = geometry["type"]
    if t == "LineString":
        return [geometry["coordinates"]]
    if t == "MultiLineString":
        return geometry["coordinates"]
    return []


def _segment_length_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    # Planar approximation consistent with hexgrid's own projection (a fixed
    # cos(lat) correction), plenty accurate for length ranking and sampling
    # at this scale.
    dx = (lon2 - lon1) * 111_320.0 * math.cos(math.radians((lat1 + lat2) / 2.0))
    dy = (lat2 - lat1) * 111_320.0
    return math.hypot(dx, dy)


def load_rail_features(cache_dir: str, log=print) -> list[dict]:
    """Returns every kept (category 11/12) feature as
    `{"tier": "shinkansen"|"trunk"|"branch", "name": str, "parts": [[[lon,
    lat], ...], ...]}` - one entry per raw GeoJSON feature, `parts` already
    split out of any MultiLineString."""
    data = fetch_n02_zip_bytes(cache_dir, log=log)
    with zipfile.ZipFile(__import__("io").BytesIO(data)) as zf:
        with zf.open(N02_GEOJSON_MEMBER) as f:
            geojson = json.load(f)

    raw_features = [
        feat for feat in geojson["features"] if feat["properties"]["N02_001"] in RAIL_CATEGORY_CODES
    ]

    # Pass 1: national total length per JR (code 11) line name, for the
    # trunk/branch split - see module doc.
    jr_length_by_name: dict[str, float] = defaultdict(float)
    for feat in raw_features:
        props = feat["properties"]
        if props["N02_001"] != JR_CATEGORY_CODE:
            continue
        for part in _flatten_parts(feat["geometry"]):
            for i in range(len(part) - 1):
                (lon1, lat1), (lon2, lat2) = part[i], part[i + 1]
                jr_length_by_name[props["N02_003"]] += _segment_length_m(lon1, lat1, lon2, lat2)

    features = []
    for feat in raw_features:
        props = feat["properties"]
        name = props["N02_003"]
        if "新幹線" in name:
            tier = "shinkansen"
        elif props["N02_001"] == JR_CATEGORY_CODE and jr_length_by_name[name] >= TRUNK_LENGTH_THRESHOLD_M:
            tier = "trunk"
        else:
            tier = "branch"
        features.append({"tier": tier, "name": name, "parts": _flatten_parts(feat["geometry"])})

    log(
        f"  N02 rail: kept {len(features)} features (categories 11/12); "
        f"tiers: shinkansen={sum(1 for f in features if f['tier']=='shinkansen')} "
        f"trunk={sum(1 for f in features if f['tier']=='trunk')} "
        f"branch={sum(1 for f in features if f['tier']=='branch')}"
    )
    return features


def build_rail_crossings(
    features: list[dict],
    hex_meters: dict[str, tuple[float, float]],
    layout: hexgrid.HexLayout,
    log=print,
) -> dict[frozenset, tuple[str, str]]:
    """For every hex pair a real qualifying rail line crosses, the best
    (`TIER_RANK`-highest) tier seen and the name of the line that produced
    it: `{frozenset({hex_a, hex_b}): (tier, line_name)}`. Only pairs where
    *both* hexes are keys of `hex_meters` (i.e. both are real land hexes in
    this scenario) are recorded - a real line's endpoints elsewhere (open
    sea, a dropped islet) are simply not representable in this scenario's
    graph and are dropped, not guessed at."""
    crossings: dict[frozenset, tuple[str, str]] = {}
    best_rank: dict[frozenset, int] = {}

    for feat in features:
        tier = feat["tier"]
        rank = TIER_RANK[tier]
        for part in feat["parts"]:
            if len(part) < 2:
                continue
            lons: list[float] = []
            lats: list[float] = []
            for i in range(len(part) - 1):
                (lon1, lat1), (lon2, lat2) = part[i], part[i + 1]
                seg_len = _segment_length_m(lon1, lat1, lon2, lat2)
                n_steps = max(1, math.ceil(seg_len / SAMPLE_STEP_M))
                for k in range(n_steps):
                    t = k / n_steps
                    lons.append(lon1 + (lon2 - lon1) * t)
                    lats.append(lat1 + (lat2 - lat1) * t)
            lons.append(part[-1][0])
            lats.append(part[-1][1])

            x, y = hexgrid.lonlat_to_meters(np.array(lons), np.array(lats))
            qf, rf = layout.meters_to_axial_frac_vec(x, y)
            qi, ri = hexgrid.cube_round_vec(qf, rf)
            hids = [f"h{q}_{r}" for q, r in zip(qi.tolist(), ri.tolist())]

            prev = hids[0]
            for h in hids[1:]:
                if h != prev and prev in hex_meters and h in hex_meters:
                    key = frozenset((prev, h))
                    if best_rank.get(key, 0) < rank:
                        best_rank[key] = rank
                        crossings[key] = (tier, feat["name"])
                prev = h

    log(f"  N02 rail: {len(crossings)} scenario hex-pairs have a real rail crossing")
    return crossings
