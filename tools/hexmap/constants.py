"""Every tunable threshold for the hex map generator, per docs/phase8-spec.md
section 1's "閾値は tools/ 内の定数にまとめる". Nothing here is a per-hex
override - only global knobs. Retune these, not the generated data, if the
map comes out wrong (docs/phase8-spec.md: "配置を手で捻じ曲げない").
"""

# --- Grid ---------------------------------------------------------------

# Center-to-center hex spacing in meters. docs/phase8-spec.md section 1:
# "辺間隔 40km を既定とする（陸地マス数の目標 250〜300）".
SPACING_M = 40_000.0

# GSI dem_png zoom level to fetch. z7 covers all of Japan in ~99 tiles at
# ~1km/px ground resolution near 36N - enough samples per 40km hex (~1500+
# land+sea pixels per hex) without fetching 3-4x more tiles than needed
# (z8 would be ~360 tiles for ~2x the linear resolution).
DEM_ZOOM = 7

# A hex is only adopted if its center pixel is land (not exactly (128,0,0))
# per spec, AND at least this fraction of the pixels assigned to it (by
# nearest-hex-center / exact hex Voronoi partition) are land. Below this,
# the hex is mostly open sea with a sliver of coastline or a small island -
# discarded (spec: "小島の取りこぼしは許容する").
LAND_PIXEL_FRACTION_MIN = 0.35

# --- Terrain classification ----------------------------------------------
# docs/phase8-spec.md section 1's table, in meters of elevation:
#   median elevation high              -> Mountain
#   median elevation medium, OR high relief -> Hill
#   median elevation low AND low relief     -> Plain
# "起伏は同一ヘクス内の標高の分散か、上位・下位分位差で測る" - relief here is
# the p90-p10 spread of land-pixel elevation within the hex.
#
# Calibrated against the actual sampled distribution for SPACING_M=40000,
# not chosen from memory of what Japan "should" look like - the whole point
# of this file existing is that these are the only numbers a human ever
# touches, and they get checked against the rendered map, not against a
# mental image of Japan.
#
# A plain median-over-the-whole-hex elevation is not enough by itself: a
# hex centered on the Kofu or Nagano basin floor (~250-400m) is otherwise
# >=80% steep Southern-Alps/foothill slope by *area*, so its raw median
# lands at 700-900m - higher than several bona fide mountain hexes
# elsewhere in Japan, and would misclassify the basin as Mountain (exactly
# the failure docs/phase8-spec.md section 1 calls out by name). What
# actually distinguishes "small flat basin ringed by huge mountains" from
# "uniformly steep terrain" is not the elevation *value* distribution but
# the local *roughness* (slope) distribution: a basin floor is a patch of
# genuinely flat pixels regardless of the mountains around it, while deep
# alpine terrain (Hodaka, Norikura) has no flat patch at all anywhere in
# the hex. So median_m below is computed only over pixels whose local
# roughness clears FLAT_ROUGHNESS_M ("the flat part of this hex, if any");
# when a hex has no such patch (MIN_FLAT_SAMPLES not met - true uniform
# highland), it falls back to the ordinary whole-hex median.
#
# A second failure mode pulls the other way, and is just as real: Tohoku's
# Ou range and Hokkaido's Hidaka range are chains of comparatively narrow,
# often-isolated volcanic peaks (Zao 1841m, Chokai 2236m, ...) rather than
# one broad contiguous massif like the Japan Alps - at a 40km hex, most of
# a hex centered on one of these peaks is ordinary lower countryside, so
# even the *whole-hex* (non-flat-filtered) median lands in the 140-450m
# range despite a genuine 1700-2200m summit inside it. A single global
# MOUNTAIN_MEDIAN_M can't simultaneously (a) sit low enough to catch these,
# and (b) sit high enough that Kofu/Nagano's flat-filtered median (which
# *should* be low, ~250-350m, once the basin floor is isolated from the
# slopes around it) stays under it - those are different pixel populations
# (whole-hex vs. flat-only), so the same threshold applies cleanly to both:
# 480m clears Zao/Chokai-scale isolated peaks (whole-hex median) while
# staying comfortably above Kofu/Nagano's actual basin-floor elevation
# (flat median).
MOUNTAIN_MEDIAN_M = 440.0
HILL_MEDIAN_M = 150.0
HILL_RELIEF_M = 350.0

# Local roughness = (max - min) elevation in a 3x3-pixel window (~2-3km at
# DEM_ZOOM=7's ground resolution near Japan) - well under a 40km hex, fine
# enough to resolve a basin floor (Kofu ~15km, Nagano ~20km+ across)
# without being so fine that ordinary rolling terrain reads as "flat".
FLAT_ROUGHNESS_M = 25.0
# The "flat median" is only trusted over the whole-hex median once a
# *meaningful share* of the hex is actually flat - not just a handful of
# stray smooth pixels (a river bed, a lake edge) that would otherwise let
# a tiny, unrepresentative sample override the classification of a hex
# that is not actually basin-like. Needs both a floor (protects small
# hexes / avoids a single-digit sample count mattering at all) and a
# fraction (protects hexes with many pixels: Mt Zao's hex has 9 locally-
# flat pixels out of 1440 - 0.6% - a coastal/river sliver, not a basin).
MIN_FLAT_SAMPLES = 20
MIN_FLAT_FRACTION = 0.02

# --- Links -----------------------------------------------------------------

from terrain_kind import Terrain

RAIL_TERRAINS = (Terrain.PLAIN, Terrain.URBAN)
# Elevation difference (meters, hex median-to-median) above which an
# ordinary land link downgrades from Rail to Road even if neither endpoint
# is classified Mountain - a steep road between two Hill hexes, say.
ROAD_ELEV_DIFF_M = 350.0

# --- Island bridging --------------------------------------------------------

# Real-world straits up to this distance apart get an auto-generated
# `Strait` link joining their otherwise-disconnected land components
# (docs/phase8-spec.md: "接続する海上距離に上限を設け、それを超える成分は孤立
# を許す"). Measured hex-center-to-hex-center (not coastline-to-coastline),
# which runs ~40-50km higher than the true water gap since each hex center
# sits well inland of its own coast - e.g. Sado's true ~32km gap to Honshu
# comes out ~69km center-to-center. 90km comfortably catches real,
# moderately-populated islands close to the mainland (Sado, the Goto
# archipelago) without reaching Okinawa/Amami (~350-500km from Kyushu,
# handled separately below - not a "strait" by any reasonable reading, and
# a fake-width Strait link there would misrepresent the geography rather
# than approximate it). `crate::scenario::Scenario::validate` requires the
# *entire* region graph connected (no isolated regions at all, stricter
# than this spec section's "孤立を許す" describes) - `build_scenario.py`
# reconciles the two by dropping any hex whose component still isn't
# joined to the mainland after this bridging pass, rather than emitting a
# file `validate` would reject; see its own "still-isolated" report.
MAX_STRAIT_GAP_KM = 90.0

# --- Named chokepoints (docs/phase8-spec.md section 1, "保たねばならない
# チョークポイント" / design.md's Phase 2 intent) ---------------------------
# These are *not* terrain placed by memory - they are the well-known real
# coordinates of three specific, named straits that Phase 2's design
# deliberately hard-coded a mechanical distinction for (Kanmon is immune to
# naval blockade; Seikan/Setouchi are not). The generic hex classification
# has no way to know a link is *this* strait rather than *a* strait, so its
# identity is looked up by nearest-hex-to-known-coordinate, same as
# docs/phase8-spec.md section 2 (Stage 8B) does for prefecture capitals.
#
# Each entry: (name_for_zone_id, side_a_lonlat, side_b_lonlat, LinkKind key,
# sea zone id or None for Tunnel).
CHOKEPOINTS = [
    # Kanmon strait (関門海峡): Shimonoseki (Honshu) <-> Moji, Kitakyushu
    # (Kyushu). <1km wide - deliberately a Tunnel, immune to blockade.
    dict(id="kanmon", name="関門", a=(130.9413, 33.9573), b=(130.9614, 33.9486), kind="tunnel", zone=None),
    # Tsugaru strait (津軽海峡): Honshu (Aomori/Tappi) <-> Hokkaido
    # (Matsumae/Fukushima-cho). ~19.5km at its narrowest.
    dict(id="seikan", name="津軽海峡", a=(140.35, 41.25), b=(140.35, 41.55), kind="strait", zone="seikan"),
    # Seto Inland Sea crossing (瀬戸内海, Honshu <-> Shikoku): Kobe/Akashi
    # area <-> Naruto, Tokushima side, the historical Honshu-Shikoku
    # crossing corridor via Awaji.
    dict(id="setouchi", name="瀬戸内", a=(135.0, 34.65), b=(134.6, 34.2), kind="strait", zone="setouchi"),
]

# A named long-haul sea route, not a narrow strait: Okinawa sits ~500km
# from Kyushu, far past MAX_STRAIT_GAP_KM, so the generic bridging pass
# never reaches it and it would otherwise be dropped by the
# still-isolated-after-bridging cleanup (see MAX_STRAIT_GAP_KM's comment).
# `scenarios/japan47.json` already treats this exact crossing (its
# kagoshima <-> okinawa link) as a single `Strait` in the `toshina` zone
# despite the real distance - same abstraction, same zone name, continued
# here at hex granularity rather than re-litigated. Processed by the same
# nearest-hex-to-known-coordinate lookup as CHOKEPOINTS (see
# build_scenario.py's apply_named_links), just not a narrow chokepoint in
# the blockade-immunity sense.
NAMED_SEA_ROUTES = [
    dict(id="kyushu_okinawa", name="鹿児島-沖縄航路", a=(130.558, 31.596), b=(127.681, 26.212), kind="strait", zone="toshina"),
]

# --- Stage 8B: population, capacity, port, infrastructure ------------------
# docs/phase8-spec.md section 2. All coefficients here are *derived*, not
# guessed: `build_scenario.py` computes the sum of each raw per-hex weight
# term across the actually-generated map, then solves each `*_COEF` so the
# resulting national total hits the target below - see that file's
# `solve_capacity_coefficients` and its module docstring for the full
# arithmetic (also reproduced in the Stage 8B report).

# A hex's population weight ("低標高かつ起伏の小さい面積", section 2) - and,
# unnormalized, its Food capacity weight ("平地面積に比例", same section) -
# both fall off smoothly with the hex's *flat-patch* median elevation
# (`flat_median_elev`, terrain.py's basin-aware term - not raw median, for
# the same Kofu/Nagano-basin reason terrain classification itself avoids
# raw median) and with relief. The half-weight points are deliberately tied
# to the already-calibrated terrain thresholds (constants.HILL_MEDIAN_M /
# HILL_RELIEF_M) rather than new arbitrary numbers: a hex right at the
# Plain/Hill boundary gets almost exactly half the weight of a hex at sea
# level with no relief.
POP_WEIGHT_ELEV_HALF_M = HILL_MEDIAN_M  # 150.0
POP_WEIGHT_RELIEF_HALF_M = HILL_RELIEF_M  # 350.0

# A hex counts as coastal if any DEM pixel assigned to it is sea (its land
# fraction is short of 1.0 - `LAND_PIXEL_FRACTION_MIN`'s adoption threshold
# already guarantees every kept hex is *mostly* land, so this only fires for
# a hex that is genuinely part land, part sea) or if it geometrically
# borders at least one lattice cell with no adopted hex at all (open sea or
# a hex dropped for being mostly water) - the union covers both "this hex's
# own footprint touches water" and "the map simply doesn't continue here",
# which between them are every reasonable notion of "coastal" the generator
# can see. Used for Steel's industrial-coast weighting and for port sizing.
COASTAL_LAND_FRACTION_MAX = 0.995

# Terrain reclassification (section 1's table, "人口密度が突出して高いもの"
# -> `Urban`): only ever promotes a `Plain` hex (never `Hill`/`Mountain` -
# every real Japanese conurbation this is meant to catch - Tokyo, Osaka,
# Nagoya, Fukuoka, Sapporo - sits on flat land, and `terrain.link_kind`'s
# `RAIL_TERRAINS` already treats `Plain`/`Urban` identically, so promoting
# only `Plain` hexes means every link kind `build_links` already computed
# stays correct without recomputing it after this pass). Calibrated against
# the actual generated population distribution (this file's own header
# rule: retune the constant, don't hand-place cities) to land on the
# hexes that are obviously real metro cores - see the Stage 8B report for
# the resulting hex list.
URBAN_POPULATION_MIN = 140.0

# Target *national* total of the five non-Food capacities (`industry_total`
# in crates/sim terms), before any in-simulation efficiency multiplier.
# docs/phase8-spec.md section 2's acceptance bar is "全勢力が軍需品を継続的
# に生産できること" - `build_scenario.py`'s report derives this figure from
# `unit_cap = 3 + industry_total/5` (crates/agents/src/lib.rs) and
# `SUPPLY_NEED_PER_MANPOWER = 1.0` (crates/sim/src/balance.rs): see the
# module docstring there for the full derivation of both this total and the
# per-good shares below.
TARGET_INDUSTRY_TOTAL = 240.0

# Shares of TARGET_INDUSTRY_TOTAL, summing to 1.0. Two trial 720-day/seed-1
# runs (this file's git history has the full arithmetic of both) converged
# on this final split; the reasoning behind it:
#
# 1st attempt (Munitions 0.38, Steel 0.17, Energy 0.18): broke on the
# *external* condition (does Munitions output keep pace with the army's
# upkeep) because it assumed a calm faction's `eff ~ 0.7`, but
# `economy::tick_economy`'s per-region `efficiency` folds in `1 -
# unrest/150`, and `unrest` climbs toward `balance::UNREST_SUPPLY_PRESSURE`
# whenever `supply_ratio` is low (logistics.rs) - the same shortage-
# compounds-into-lower-output loop `balance::FOOD_EFFICIENCY_FLOOR`'s own
# doc describes being fixed *for Food specifically*, left un-floored for
# every other commodity. A faction that starts even briefly short of
# Munitions (ordinary given `COMBAT_SUPPLY_MULT = 2.5` on a map at constant
# border war, `regions.py`'s doc) drives its own `eff` toward the *floor*
# (`efficiency`'s 0.2 clamp times `stability_output_mult`'s 0.6 floor,
# ~0.12) instead of recovering - a trial run confirmed 6 of 8 factions
# converged on `munitions == 0` by day 400.
#
# 2nd attempt (Munitions 0.60, Steel 0.15, Energy 0.16): raised Munitions'
# share to cover a worse assumed `eff`, but broke the *internal* chain
# instead: Steel and Energy must each fund their own downstream consumers
# (`balance.rs`'s `*_INPUT_*` coefficients), and because every downstream
# good scales by the *same* per-region `eff` its own production does, `eff`
# cancels out of that condition entirely - it is a fixed ratio, not
# eff-dependent:
#
#     CAPACITY_SHARE_STEEL   >= 0.4*share_machinery + 0.3*share_munitions + 0.3*share_arms
#     CAPACITY_SHARE_ENERGY  >= 0.5*share_steel     + 0.3*share_machinery + 0.2*share_munitions
#
# (`MACHINERY_INPUT_STEEL`/`MUNITIONS_INPUT_STEEL`/`ARMS_INPUT_STEEL` and
# `STEEL_INPUT_ENERGY`/`MACHINERY_INPUT_ENERGY`/`MUNITIONS_INPUT_ENERGY`,
# crates/sim/src/balance.rs). At Munitions=0.60 the first inequality alone
# demands Steel >= 0.3*0.60 = 0.18 just from Munitions' own input draw, but
# Steel was only given 0.15 - Steel was structurally starved regardless of
# `eff`, which is exactly what the 2nd trial run showed (Steel stock ≈ 0 for
# every faction by day 100, dragging Munitions production down with it via
# its own Steel input even where Energy was healthy).
#
# Final split: fixes Machinery/Arms at a small but positive
# `share_machinery=0.05`/`share_arms=0.03` (Machinery still covers civilian
# demand and Arms' own input; Arms still funds recruiting/reinforcement),
# then solves the two chain inequalities above for the *largest* Munitions
# share that leaves 20% headroom on both (not exact equality - per-region
# `eff` isn't perfectly uniform across a faction's hexes in practice, so the
# ratio isn't exactly conserved either), which comes out to Munitions ≈
# 0.466; rounded down to 0.42 for further margin, with the resulting slack
# handed to Steel/Energy rather than spent on a larger Munitions share:
#
#     share_steel  = 1.2*(0.4*0.05 + 0.3*0.42 + 0.3*0.03) ≈ 0.19  (+ leftover slack -> 0.22)
#     share_energy = 1.2*(0.5*share_steel + 0.3*0.05 + 0.2*0.42) ≈ 0.23  (+ leftover slack -> 0.28)
#
# At this split the *external* condition (`CAPACITY_SHARE_MUNITIONS * eff >=
# 0.2 * SAFETY`, `build_scenario.py`'s module docstring) holds up to `eff *
# SAFETY <= 0.42/0.2 = 2.1` - e.g. `eff=0.6` tolerates `SAFETY<=3.5`, ample
# margin for a faction with only a fraction of its army `unit_contested` at
# `COMBAT_SUPPLY_MULT=2.5` at any one time. The one case this still doesn't
# fully cover is the smallest faction's `unit_cap`'s flat "+3" floor
# (`crates/agents/src/lib.rs`) relative to its own tiny `industry_total` -
# see the Stage 8B report for how Shikoku (the smallest faction) actually
# behaves under this final calibration.
CAPACITY_SHARE_ENERGY = 0.28
CAPACITY_SHARE_STEEL = 0.22
CAPACITY_SHARE_MACHINERY = 0.05
CAPACITY_SHARE_MUNITIONS = 0.42
CAPACITY_SHARE_ARMS = 0.03
assert abs(
    CAPACITY_SHARE_ENERGY + CAPACITY_SHARE_STEEL + CAPACITY_SHARE_MACHINERY
    + CAPACITY_SHARE_MUNITIONS + CAPACITY_SHARE_ARMS - 1.0
) < 1e-9

# Mirrors `balance::CIVILIAN_FOOD_DEMAND_PER_POP` (crates/sim/src/balance.rs)
# - duplicated here (not imported; `tools/` has no dependency on `crates/`)
# purely to size TARGET_FOOD_CAPACITY below against actual civilian demand.
CIVILIAN_FOOD_DEMAND_PER_POP = 0.0022

# Mirrors `scenario::UNITS_PER_FACTION` (crates/sim/src/scenario.rs) and
# `balance::SUPPLY_NEED_PER_MANPOWER` / `balance::UNIT_MANPOWER`
# (crates/sim/src/balance.rs) - duplicated here for the same "no crates/
# dependency" reason as `CIVILIAN_FOOD_DEMAND_PER_POP` above, purely so
# `build_scenario.py`'s own report can check each faction's starting
# Munitions capacity against what its starting army actually needs, in the
# engine's own units, without guessing at the numbers by hand.
UNITS_PER_FACTION = 3
SUPPLY_NEED_PER_MANPOWER = 1.0
UNIT_MANPOWER = 1.0

# Target national Food capacity: docs/phase8-spec.md section 2's Food row
# ("平地面積に比例") is independent of TARGET_INDUSTRY_TOTAL. Sized to
# `balance::CIVILIAN_FOOD_DEMAND_PER_POP` (0.0022/capita) times the national
# population (12495万人, prefecture_population.TOTAL_POPULATION) times a
# buffer factor, the same "target a comfortable multiple of civilian demand"
# approach `balance.rs`'s own civilian-demand constants use - 2.5x mirrors
# `scenarios/japan47.json`'s own national Food/demand ratio (58.5 capacity
# against a ~27.5/day demand, i.e. ~2.1x, rounded up slightly since Food's
# own `food_efficiency` multiplier - balance.rs's `FOOD_EFFICIENCY_FLOOR`/
# `FOOD_EFFICIENCY_DAMPENING` - rarely reaches 1.0 in practice).
TARGET_FOOD_CAPACITY_MULT = 2.5

# Energy is "薄く比例（全国に分散）" (section 2): most of each hex's Energy
# capacity still scales with its population, but a fixed share is instead
# spread flat across every land hex regardless of population, so a sparse
# hex still contributes something (unlike Steel, which is still zero in an
# empty hex - Munitions gets the same treatment as Energy, just under its
# own share below).
ENERGY_AREA_SHARE = 0.2

# Munitions ("人口密度に比例" per section 2) additionally gets its own flat
# per-hex floor, the same mechanism as `ENERGY_AREA_SHARE` above: a fixed
# share of the *national Munitions target* (not of `TARGET_INDUSTRY_TOTAL` -
# this redistributes Munitions' own existing share, it does not add to it)
# is spread evenly across every land hex regardless of population; the
# remainder still scales with population as before.
#
# Why Munitions specifically, and not Steel/Machinery/Arms too: a large,
# sparsely-populated faction's `unit_cap` peacetime floor
# (`crates/agents/src/lib.rs`: `3 + industry_total/5`, `industry_total`
# summing all five non-Food goods) is satisfied trivially by area alone once
# any capacity exists, but *feeding* those 3 starting units
# (`scenarios::UNITS_PER_FACTION`) is a Munitions-only draw
# (`SUPPLY_NEED_PER_MANPOWER * UNIT_MANPOWER` per unit,
# `logistics::distribute_supply`) - Steel/Machinery/Arms shortfalls degrade
# output quality (`economy::tick_economy`'s input chain) but don't zero a
# unit's supply outright the way Munitions does. A population-only Munitions
# formula makes a faction's *territory* worthless for the one good its
# starting army actually needs to survive - see build_scenario.py's module
# docstring for the arithmetic this share was calibrated against
# (`scenarios/japan_hex.json`'s 北海道方面軍: 61 hexes, only ~4% of national
# population).
MUNITIONS_AREA_SHARE = 0.65

# Machinery/Arms concentrate super-linearly in dense hexes ("人口密度に強く
# 比例（都市圏に集中）" / "人口密度に比例、Machinery より集中" - section 2):
# capacity_h ∝ population_h ** exponent. Arms' exponent is the larger of the
# two so the same population difference concentrates it harder, per the
# spec's explicit ordering.
MACHINERY_DENSITY_EXPONENT = 1.6
ARMS_DENSITY_EXPONENT = 2.0

# Steel ("人口と、沿岸・平地であることに比例（臨海工業地帯）" - section 2):
# capacity_h ∝ population_h * steel_factor_h, where steel_factor_h multiplies
# a terrain term (flat land favored) by a coastal term (coastal favored) -
# a coastal flat hex (a real 臨海工業地帯 site) gets the full weight; an
# inland mountain hex gets the smallest.
STEEL_TERRAIN_FACTOR_FLAT = 1.0
STEEL_TERRAIN_FACTOR_HILL = 0.5
STEEL_TERRAIN_FACTOR_MOUNTAIN = 0.2
STEEL_COASTAL_FACTOR_COASTAL = 1.0
STEEL_COASTAL_FACTOR_INLAND = 0.35

# Port size (section 2: "規模は接する海岸線の長さと人口による"): a coastal
# hex gets a flat base, plus a term for how much of its 6-direction lattice
# boundary actually opens onto open water (0..6, `sea_dir_count` in
# build_scenario.py - a peninsula-tip hex scores higher than a hex with a
# single narrow inlet), plus a term in sqrt(population) (diminishing
# returns, so one enormous metro hex doesn't dwarf every other port by a
# factor equal to its raw population ratio). Range chosen to land in the
# same 0.4-1.5 order of magnitude `scenarios/japan47.json`'s ports occupy.
PORT_BASE = 0.3
PORT_COAST_DIRECTION_COEF = 0.07
PORT_POP_SQRT_COEF = 0.045
PORT_MAX = 1.6

# Infrastructure: a per-terrain base (Urban highest, Mountain lowest) plus a
# sqrt(population) term (a busier hex has more built-up road/rail bed),
# clamped to the same broad range `scenarios/japan47.json` uses (0.45-1.0).
INFRA_BASE_BY_TERRAIN = {"plain": 0.5, "hill": 0.4, "mountain": 0.3, "urban": 0.75}
INFRA_POP_SQRT_COEF = 0.018
INFRA_MIN = 0.3
INFRA_MAX = 1.0
