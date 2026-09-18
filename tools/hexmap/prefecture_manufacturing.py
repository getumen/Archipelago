"""Prefecture manufacturing output (製造品出荷額等), in 百万円, keyed the
same way `prefecture_population.POPULATION` is (Japanese name, no
`都`/`道`/`府`/`県` suffix).

Added to fix a structural defect measured in Stage 12D (docs/phase12-spec.md
§Stage 12D, "孤立した勢力が技術で詰まないことを測る"): 北海道方面軍 was left
permanently behind in research at japan_hex day 720, reproducibly across
seeds. Research rate is proportional to `Faction::machinery_output`, and
before this table existed `build_scenario.py` derived each hex's Machinery
(and, by explicit reuse, Aircraft) capacity from
`population_h ** MACHINERY_DENSITY_EXPONENT` (1.6) - a guess read off the
design document's prose ("人口密度に強く比例（都市圏に集中）") with no real
data behind the exponent or the population proxy itself.

Checked against this exact statistic, that guess does not hold:

  - the real log-log slope of prefectural 製造品出荷額等 against this
    module's own `prefecture_population.POPULATION` is ~0.989 (essentially
    proportional, not super-linear)
  - R² of that fit is only ~0.657 - population is a weak proxy for
    manufacturing even at the right exponent (東京 is 1st in population but
    13th in manufacturing; 愛知 is 4th in population and 1st in
    manufacturing by a wide margin)
  - the exponent-1.6 proxy produced an 85x faction-level Machinery spread
    against a 12x population spread; the real per-prefecture figures used
    here produce roughly a 12x spread instead

So this table replaces the guess with the actual statistic, the same way
`prefecture_population.POPULATION` already replaced a guessed population
curve, and the same way this project sources terrain (国土地理院 elevation
tiles), rail (N02-24) and ports (C02-14) from real data rather than placing
them by memory (docs/phase8-spec.md, CLAUDE.md's licence section).

Provenance, stated plainly:

  - statistic: 製造品出荷額等 (manufactured goods shipment value), from the
    工業統計調査 (Census of Manufacture, 経済産業省)
  - year: 2013 (the year the source aggregation reports; not re-verified
    against a different survey year)
  - these 47 values were **transcribed from a third-party aggregation of
    that statistic**, not pulled from e-Stat directly. A future refresh
    against e-Stat's own 経済センサス‐活動調査 (Economic Census for Business
    Activity) figures, ideally for a more recent year, would be a real
    improvement - both a more authoritative source and a more current one.
    Nothing here should be read as e-Stat-verified.

`build_scenario.py` spreads each prefecture's total across that
prefecture's hexes using the same weighting `distribute_population` already
uses for `prefecture_population.POPULATION` (`nearest_pref_name` decides
hex-to-prefecture membership; the weighting itself lives in
`build_scenario.py`, not here) - see `distribute_manufacturing` there.

Licence: 経済産業省 government statistics (工業統計調査), usable
commercially with attribution - unlike the port data (C02-14, 非商用),
this table adds no non-commercial constraint (CLAUDE.md's licence section
has the fuller comparison against rail/port/airport data).
"""

from __future__ import annotations

MANUFACTURING: dict[str, float] = {
    "北海道": 6385147.0,
    "青森": 1520298.0,
    "岩手": 2267151.0,
    "宮城": 3726535.0,
    "秋田": 1106465.0,
    "山形": 2395796.0,
    "福島": 4762508.0,
    "茨城": 10901331.0,
    "栃木": 8179507.0,
    "群馬": 7722701.0,
    "埼玉": 11787702.0,
    "千葉": 13003297.0,
    "東京": 7851824.0,
    "神奈川": 17226142.0,
    "新潟": 4405065.0,
    "富山": 3331418.0,
    "石川": 2424273.0,
    "福井": 1830135.0,
    "山梨": 1985155.0,
    "長野": 5112535.0,
    "岐阜": 4797431.0,
    "静岡": 15699131.0,
    "愛知": 42001844.0,
    "三重": 10409249.0,
    "滋賀": 6435202.0,
    "京都": 4560516.0,
    "大阪": 16024460.0,
    "兵庫": 14026866.0,
    "奈良": 1848195.0,
    "和歌山": 2972305.0,
    "鳥取": 655290.0,
    "島根": 1004306.0,
    "岡山": 7673681.0,
    "広島": 8555642.0,
    "山口": 6797922.0,
    "徳島": 1712207.0,
    "香川": 2283571.0,
    "愛媛": 4067759.0,
    "高知": 521768.0,
    "福岡": 8193015.0,
    "佐賀": 1652804.0,
    "長崎": 1627820.0,
    "熊本": 2385012.0,
    "大分": 4382787.0,
    "宮崎": 1447591.0,
    "鹿児島": 1802491.0,
    "沖縄": 628279.0,
}

TOTAL_MANUFACTURING = sum(MANUFACTURING.values())
# In 百万円; also expressed in 兆円 (1兆円 = 1,000,000百万円) so it can be
# eyeballed against any standard table of Japan's manufacturing shipment
# value without doing the arithmetic by hand - it should land near 290兆円.
TOTAL_MANUFACTURING_TRILLION_YEN = TOTAL_MANUFACTURING / 1_000_000.0
assert TOTAL_MANUFACTURING == 292_092_129.0, f"unexpected national total: {TOTAL_MANUFACTURING}"
assert 280.0 < TOTAL_MANUFACTURING_TRILLION_YEN < 300.0, (
    f"national manufacturing total {TOTAL_MANUFACTURING_TRILLION_YEN:.1f}兆円 is outside the "
    "expected ballpark (~290兆円) - check for a transcription error"
)
