"""Prefecture population, in 万人 (ten-thousands), keyed the same way
`prefecture_capitals.CAPITALS` is (Japanese name, no `都`/`道`/`府`/`県`
suffix).

Stage 8B (docs/phase8-spec.md section 2): "都道府県別の人口（万人）を既知の表
として持ち、各県のヘクスへ配分する". This is the *national* prefecture-level
census figure - independently verifiable against any standard population
table - which `build_scenario.py` then spreads across that prefecture's
hexes weighted by low, flat land (`nearest_pref_name` decides which hexes
belong to which prefecture; the weighting itself lives in
`build_scenario.py`, not here).

These are the same 47 values already used by `scenarios/japan47.json`'s
per-region `population` field (one prefecture == one region there), kept as
an independent table rather than parsed from that file at generation time
so this generator has no runtime dependency on another scenario's JSON -
the two are expected to (and do, per this module's own `TOTAL_POPULATION`
sanity check) sum to the same national total, 12495 万人 (~1.25 億, in the
right ballpark for Japan's actual population).
"""

from __future__ import annotations

POPULATION: dict[str, float] = {
    "北海道": 520.0,
    "青森": 120.0,
    "岩手": 118.0,
    "宮城": 230.0,
    "秋田": 93.0,
    "山形": 105.0,
    "福島": 180.0,
    "茨城": 285.0,
    "栃木": 190.0,
    "群馬": 190.0,
    "埼玉": 735.0,
    "千葉": 628.0,
    "東京": 1400.0,
    "神奈川": 923.0,
    "新潟": 215.0,
    "富山": 102.0,
    "石川": 111.0,
    "福井": 75.0,
    "山梨": 80.0,
    "長野": 202.0,
    "岐阜": 195.0,
    "静岡": 360.0,
    "愛知": 750.0,
    "三重": 172.0,
    "滋賀": 141.0,
    "京都": 254.0,
    "大阪": 878.0,
    "兵庫": 540.0,
    "奈良": 130.0,
    "和歌山": 90.0,
    "鳥取": 55.0,
    "島根": 65.0,
    "岡山": 186.0,
    "広島": 278.0,
    "山口": 132.0,
    "徳島": 70.0,
    "香川": 93.0,
    "愛媛": 130.0,
    "高知": 68.0,
    "福岡": 510.0,
    "佐賀": 80.0,
    "長崎": 128.0,
    "熊本": 171.0,
    "大分": 111.0,
    "宮崎": 105.0,
    "鹿児島": 155.0,
    "沖縄": 146.0,
}

TOTAL_POPULATION = sum(POPULATION.values())
assert TOTAL_POPULATION == 12495.0, f"unexpected national total: {TOTAL_POPULATION}"
