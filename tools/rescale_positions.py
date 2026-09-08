#!/usr/bin/env python3
"""Rescales `scenarios/mvp.json` and `scenarios/japan47.json`'s region
`position` fields onto the same real kilometre scale `scenarios/japan_hex
.json` already uses, so `balance::AIR_OPERATING_RADIUS_KM = 300` (a
straight-line kilometre figure, `crates/sim/src/air.rs`'s
`geographic_distance`) means the same thing on every shipped map.

Why this exists (docs/phase10-spec.md gap, `world::Region::position`'s own
doc, `scenario::parse_position`'s own doc): Stage 10B made `position`
simulation-affecting for the first time, but `mvp.json`/`japan47.json`
still carried the unscaled, hand-placed layout `docs/phase7-spec.md`
"地域の座標" introduced back when the field's own doc said "座標はシミュ
レーションに一切影響しない". Measuring it (see this repo's Phase 10 gap
report) showed the effect is not cosmetic: at the old schematic scale, on
average 68%/75% of mvp/japan47's own regions already sit inside one 300km
radius of any given region - AI-flown air superiority there is close to
global reach, doing none of the work `AIR_OPERATING_RADIUS_KM`'s own doc
says the constant is for. `japan_hex.json`'s equivalent figure is 25%.

Both maps are stylised, hand-authored groupings/prefectures - not derived
from a DEM the way `japan_hex` is (`tools/hexmap/build_scenario.py`) - so
there is no pixel data to re-derive a placement from. But every region
still names real places (`RESCALE - grounded in prefecture-capital
coordinates` below), so the fix this module makes is: stop inventing a
scale by eye (docs/conventions.md §3, the standing "don't place things by
memory" rule from Phase 8/9C) and instead project each region's *real*
component prefecture(s) through the exact same equirectangular +
fixed-cos(lat) projection `tools/hexmap/hexgrid.py` already uses to build
`japan_hex.json` (`hexgrid.lonlat_to_meters`, divided by 1000 - identical
to `build_scenario.py`'s own `x_m / 1000.0`). That makes all three
scenarios' coordinates the same physical unit, not merely each internally
self-consistent.

Sources, all already in this tree or independently verifiable - never a
coordinate chosen by eye:

  - `tools/hexmap/prefecture_capitals.py`'s `CAPITALS` table (lat/lon of
    all 47 prefectural capitals) - the same table `build_scenario.py`
    already uses to assign hexes to prefectures.
  - `japan47.json`: one region per prefecture already (its own `name`
    field, e.g. "青森県"), so its position is simply that prefecture's own
    capital, projected.
  - `mvp.json`: each of its ten regions is a named grouping of real
    prefectures (東北, 関東, 中部...), so its position is the *unweighted
    centroid* of its member prefectures' own capitals, projected. The
    prefecture membership per mvp region (`MVP_REGION_PREFECTURES` below)
    is fixed by ordinary, independently-checkable Japanese regional
    groupings, not invented for this map:
      - 北東北/南東北: the standard tourism/regional-development "北東北
        三県" (青森・岩手・秋田) / "南東北三県" (宮城・山形・福島) split.
      - 関東: the standard 1都6県.
      - 信越・北陸: 中部地方's standard 3-way subdivision's 北陸4県
        (新潟・富山・石川・福井) plus 甲信2県 (山梨・長野) - "信越" in the
        region's own name is 信濃(長野)+越後(新潟); 甲斐(山梨) joins them
        here because Chubu's 3-way split leaves it no home in "東海"
        below and none of mvp's other 9 regions.
      - 東海: 中部地方's remaining 3 prefectures (岐阜・静岡・愛知) plus
        三重 - this is exactly the Japan Meteorological Agency's own
        「東海地方」 forecast region (岐阜県・静岡県・愛知県・三重県),
        an independently citable standard, not this project's own call.
      - 近畿: the standard "2府4県" core (滋賀・京都・大阪・兵庫・奈良・
        和歌山) - the ambiguous 8th Chubu/Kinki prefecture (三重) is
        already placed under 東海 above, so it is not double-counted here.
      - 中国/四国: the standard 5県/4県.
      - 九州: the standard 7県 plus 沖縄 (mvp has no separate Okinawa
        region).
    Every one of the 47 prefectural capitals is used in `MVP_REGION_
    PREFECTURES` exactly once - `_check_mvp_coverage` below fails loudly
    (docs/conventions.md §3, fail-fast) if that ever stops being true.

`scenarios/japan_hex.json` is untouched by this script - it already carries
real kilometre coordinates and Stage 10 explicitly must not touch it.

Usage:
    python3 tools/rescale_positions.py --write   # rewrite both files in place
    python3 tools/rescale_positions.py           # print the new position table, change nothing
    python3 tools/rescale_positions.py --check   # exit nonzero if either file's committed
                                                  # positions differ from what this script derives
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "hexmap"))

import hexgrid  # noqa: E402  (path insert must run first)
from prefecture_capitals import CAPITALS  # noqa: E402

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MVP_PATH = os.path.join(REPO_ROOT, "scenarios", "mvp.json")
JAPAN47_PATH = os.path.join(REPO_ROOT, "scenarios", "japan47.json")

# mvp.json's ten regions as groupings of real prefectures - see this
# module's own doc above for the citable source of every grouping.
MVP_REGION_PREFECTURES: dict[str, list[str]] = {
    "hokkaido": ["北海道"],
    "kita_tohoku": ["青森", "岩手", "秋田"],
    "minami_tohoku": ["宮城", "山形", "福島"],
    "kanto": ["茨城", "栃木", "群馬", "埼玉", "千葉", "東京", "神奈川"],
    "shinetsu_hokuriku": ["新潟", "富山", "石川", "福井", "山梨", "長野"],
    "tokai": ["岐阜", "静岡", "愛知", "三重"],
    "kinki": ["滋賀", "京都", "大阪", "兵庫", "奈良", "和歌山"],
    "chugoku": ["鳥取", "島根", "岡山", "広島", "山口"],
    "shikoku": ["徳島", "香川", "愛媛", "高知"],
    "kyushu": ["福岡", "佐賀", "長崎", "熊本", "大分", "宮崎", "鹿児島", "沖縄"],
}


def _check_mvp_coverage() -> None:
    used = [p for prefs in MVP_REGION_PREFECTURES.values() for p in prefs]
    if len(used) != len(set(used)):
        dupes = sorted({p for p in used if used.count(p) > 1})
        raise SystemExit(f"MVP_REGION_PREFECTURES lists a prefecture more than once: {dupes}")
    missing = sorted(set(CAPITALS) - set(used))
    extra = sorted(set(used) - set(CAPITALS))
    if missing or extra:
        raise SystemExit(f"MVP_REGION_PREFECTURES does not exactly cover CAPITALS: missing={missing} extra={extra}")


def capital_km(prefecture: str) -> tuple[float, float]:
    """A prefecture's capital, projected into the same (x_m/1000, y_m/1000)
    kilometre plane `tools/hexmap/build_scenario.py` writes `japan_hex.json`
    positions in - same origin, same fixed-cos(lat) longitude correction."""
    lat, lon = CAPITALS[prefecture]
    x_m, y_m = hexgrid.lonlat_to_meters(lon, lat)
    return x_m / 1000.0, y_m / 1000.0


def mvp_position(region_id: str) -> tuple[float, float]:
    prefs = MVP_REGION_PREFECTURES[region_id]
    xs, ys = zip(*(capital_km(p) for p in prefs))
    return sum(xs) / len(xs), sum(ys) / len(ys)


def _capital_key_for_prefecture_name(name: str) -> str:
    """japan47.json region names carry the prefecture's own administrative
    suffix (都/道/府/県, e.g. "青森県", "東京都", "北海道" itself has none) -
    `CAPITALS` keys never do. Strips exactly one trailing suffix character
    when present."""
    if name in CAPITALS:
        return name
    if name[-1] in "都道府県" and name[:-1] in CAPITALS:
        return name[:-1]
    raise SystemExit(f"japan47 region name {name!r} does not match any CAPITALS entry")


def japan47_position(region_name: str) -> tuple[float, float]:
    return capital_km(_capital_key_for_prefecture_name(region_name))


def _splice_position(text: str, region_id: str, xy: tuple[float, float]) -> str:
    """Replaces only the `position` array's two numbers for one region,
    reusing whatever bracket layout (inline `[x, y]` or one-number-per-line)
    that region's own `position` field already used - so the diff is the
    changed numbers alone, never an incidental reformat of a file this
    script does not otherwise own the style of."""
    x, y = round(xy[0], 2), round(xy[1], 2)
    pattern = re.compile(
        r'("id":\s*"' + re.escape(region_id) + r'".*?"position":\s*\[)([^\]]*)(\])',
        re.DOTALL,
    )

    def replace(m: re.Match) -> str:
        prefix, body, suffix = m.group(1), m.group(2), m.group(3)
        if "\n" in body:
            indent = re.search(r"\n(\s*)\S", body).group(1)
            closing_indent = indent[:-2] if indent.endswith("  ") else indent
            return f"{prefix}\n{indent}{x},\n{indent}{y}\n{closing_indent}{suffix}"
        return f"{prefix}{x}, {y}{suffix}"

    new_text, n = pattern.subn(replace, text, count=1)
    if n != 1:
        raise SystemExit(f"expected exactly one `position` field for region {region_id!r}, found {n}")
    return new_text


def rescale_file(path: str, position_fn) -> str:
    with open(path, encoding="utf-8") as f:
        text = f.read()
    data = json.loads(text)
    for region in data["regions"]:
        xy = position_fn(region)
        text = _splice_position(text, region["id"], xy)
    return text


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--write", action="store_true", help="rewrite mvp.json/japan47.json in place")
    ap.add_argument("--check", action="store_true", help="exit nonzero if either file's committed positions are stale")
    args = ap.parse_args()

    _check_mvp_coverage()

    mvp_text = rescale_file(MVP_PATH, lambda r: mvp_position(r["id"]))
    japan47_text = rescale_file(JAPAN47_PATH, lambda r: japan47_position(r["name"]))

    if args.check:
        stale = []
        for path, new_text in ((MVP_PATH, mvp_text), (JAPAN47_PATH, japan47_text)):
            with open(path, encoding="utf-8") as f:
                current = f.read()
            if current != new_text:
                stale.append(path)
        if stale:
            print(f"stale positions in: {', '.join(stale)}", file=sys.stderr)
            sys.exit(1)
        print("mvp.json and japan47.json positions match the derivation exactly.")
        return

    if not args.write:
        for region_id in MVP_REGION_PREFECTURES:
            print("mvp", region_id, mvp_position(region_id))
        data = json.loads(japan47_text)
        for region in data["regions"]:
            print("japan47", region["id"], region["position"])
        return

    with open(MVP_PATH, "w", encoding="utf-8") as f:
        f.write(mvp_text)
    with open(JAPAN47_PATH, "w", encoding="utf-8") as f:
        f.write(japan47_text)
    print(f"wrote rescaled positions into {MVP_PATH} and {JAPAN47_PATH}")


if __name__ == "__main__":
    main()
