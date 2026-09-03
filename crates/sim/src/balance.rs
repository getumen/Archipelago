//! All tunable balance constants for the simulation, gathered in one place
//! so systems never carry magic numbers of their own.

pub const WORKFORCE_SHARE: f32 = 0.5;
pub const CONSCRIPT_RATE: f32 = 0.00035;

/// Daily fraction of the manpower pool (drafted conscripts not yet
/// assigned to a unit) that returns to the civilian workforce, independent
/// of policy. This is the demobilization half of the manpower-pool fix
/// (`economy::tick_economy`): `conscription` only throttles inflow, so
/// without an outflow a faction sitting on a large population could draft
/// forever and never give the labour back, even though `Region::mobilized`
/// (and therefore `labor_ratio`) is recomputed from the *current* pool
/// every tick. Draining the pool changes nothing about population or unit
/// manpower - it only shrinks what counts toward `mobilized`, so the
/// workforce comes back automatically on the next tick.
///
/// At this rate a pool being fed by continuous drafting settles at
/// `draft / MANPOWER_DEMOBILIZATION_RATE` instead of growing without
/// bound: e.g. a faction drafting at the AI's throttled 0.15 tier
/// (agents::CONSCRIPTION_THROTTLE_MANPOWER) off the *entire* map's
/// population (~12,280) settles around 32 (万人) - the same order of
/// magnitude as that throttle threshold, not the hundreds a one-way
/// accumulator produces.
pub const MANPOWER_DEMOBILIZATION_RATE: f32 = 0.02;

/// Stage 2A production chain (docs/phase2-spec.md "Stage 2A"): input goods
/// consumed per unit of output good produced, at the `Steel -> Machinery /
/// Munitions -> Arms` stage. `Food` and `Energy` have no inputs.
pub const STEEL_INPUT_ENERGY: f32 = 0.5;
pub const MACHINERY_INPUT_STEEL: f32 = 0.4;
pub const MACHINERY_INPUT_ENERGY: f32 = 0.3;
pub const MUNITIONS_INPUT_STEEL: f32 = 0.3;
pub const MUNITIONS_INPUT_ENERGY: f32 = 0.2;
pub const ARMS_INPUT_MACHINERY: f32 = 0.5;
pub const ARMS_INPUT_STEEL: f32 = 0.3;

/// Civilian demand, per capita (population is tracked in 万人/"ten
/// thousands"), for the three commodities civilians draw on directly.
///
/// `CIVILIAN_ENERGY_DEMAND_PER_POP` and `CIVILIAN_MACHINERY_DEMAND_PER_POP`
/// are derived from the Stage 2A scenario capacity table
/// (docs/phase2-spec.md's region list, summed per faction), not guessed:
/// with civilian demand met first and industry taking the remainder (see
/// economy.rs), each faction's civilian Energy draw should land around a
/// quarter of its *national* Energy capacity, and its civilian Machinery
/// draw around a twentieth of its Machinery capacity, so every faction can
/// both feed its population and still fund a substantial share of its
/// Steel/Machinery/Munitions/Arms chain. Concretely, at the nation's Energy
/// capacity (10.0 for 東方連合) versus what running Steel+Machinery+
/// Munitions at full table capacity alone would draw (~6.2), a civilian
/// share near 25% (~2.5) leaves the chain's ~6.2 need covered with room to
/// spare; scaled by each faction's own population this comes out to
/// approximately:
/// - 東方連合 (pop 5690, Energy 10.0): demand ≈ 2.56 (25.6% of capacity)
/// - 中央同盟 (pop 4180, Energy 7.5): demand ≈ 1.88 (25.1% of capacity)
/// - 西方同盟 (pop 2410, Energy 5.3): demand ≈ 1.08 (20.5% of capacity)
///
/// which fixes the per-capita rate at 0.00045. The same table-driven method
/// (target ≈5-8% of national Machinery capacity, since Arms' Machinery
/// input is small relative to Machinery capacity) fixes Machinery at
/// 0.0001.
pub const CIVILIAN_FOOD_DEMAND_PER_POP: f32 = 0.0022;
pub const CIVILIAN_ENERGY_DEMAND_PER_POP: f32 = 0.00045;
pub const CIVILIAN_MACHINERY_DEMAND_PER_POP: f32 = 0.0001;

/// Valid range for `Faction::civilian_ration` (design.md §9's civilian/war
/// trade-off, `Action::SetCivilianRation`): the fraction of civilian Food/
/// Energy/Machinery demand the government actually delivers. Below 1.0 the
/// undelivered share is never drawn from stock, freeing it for industry, at
/// the cost of feeding `Faction::shortage` (and therefore unrest) exactly
/// as genuine scarcity would. Floored at 0.5 so rationing is a costly lever,
/// not a way to make civilian demand vanish.
pub const CIVILIAN_RATION_MIN: f32 = 0.5;
pub const CIVILIAN_RATION_MAX: f32 = 1.0;
pub const CIVILIAN_RATION_DEFAULT: f32 = 1.0;

pub const UNIT_MANPOWER: f32 = 1.0;
pub const UNIT_EQUIPMENT: f32 = 20.0;
pub const UNIT_ORG: f32 = 100.0;
pub const UNIT_START_ORG_RATIO: f32 = 0.4;

pub const SUPPLY_NEED_PER_MANPOWER: f32 = 1.0;
pub const COMBAT_SUPPLY_MULT: f32 = 2.5;
pub const PROJECTED_SUPPLY_FACTOR: f32 = 0.4;

pub const COMBAT_DAMAGE: f32 = 8.0;
pub const ORG_DAMAGE_MULT: f32 = 2.0;
pub const MANPOWER_LOSS_PER_DAMAGE: f32 = 0.004;
pub const EQUIPMENT_LOSS_PER_DAMAGE: f32 = 0.12;
pub const BROKEN_LOSS_MULT: f32 = 3.0;
/// Morale lost per broken hit taken in combat (`tick_combat`/
/// `naval::tick_naval_combat`, multiplied by `BROKEN_LOSS_MULT` when the
/// hit breaks the unit's organization).
pub const MORALE_LOSS_PER_BROKEN_HIT: f32 = 0.01;
/// Experience gained per hit taken in combat, land or naval.
pub const EXPERIENCE_GAIN_PER_HIT: f32 = 0.0015;

pub const ORG_REGEN: f32 = 2.5;
pub const ORG_MARCH_DRAIN: f32 = 3.0;
pub const MORALE_REGEN: f32 = 0.02;
pub const ATTRITION_MANPOWER: f32 = 0.006;
pub const ATTRITION_ORG: f32 = 6.0;

pub const OCCUPATION_RATE: f32 = 30.0;
pub const OCCUPATION_DECAY: f32 = 25.0;
pub const CAPTURE_UNREST: f32 = 45.0;
pub const OCCUPIED_UNREST_FLOOR: f32 = 15.0;

/// Daily rate at which `region.unrest` closes the gap toward its
/// shortage/supply-pressure target (a target-approach model, not a
/// decay-then-accumulate one, so unrest can recover once pressure eases).
pub const UNREST_ADAPT_RATE: f32 = 0.05;
/// Unrest-target contribution at `faction.shortage == 1.0` (civilian/food
/// shortfall). Not enumerated explicitly in the spec prose
/// ("無補給・物資不足で上昇"); added here so the effect has a named,
/// centralized constant instead of a magic number.
pub const UNREST_SHORTAGE_PRESSURE: f32 = 55.0;
/// Unrest-target contribution at `faction.supply_ratio == 0.0` (fully unsupplied).
pub const UNREST_SUPPLY_PRESSURE: f32 = 30.0;

/// Daily fraction by which `unit.supply` moves toward its computed target,
/// used by `logistics::distribute_supply` to avoid abrupt swings.
pub const SUPPLY_SMOOTHING: f32 = 0.35;

/// Daily linear step by which `war_support` creeps back toward its 50 baseline.
pub const WAR_SUPPORT_DRIFT: f32 = 0.05;
/// War-support loss multiplier applied to a day's manpower casualties.
pub const WAR_SUPPORT_CASUALTY_MULT: f32 = 5.0;
/// War-support swing on a region changing hands.
pub const WAR_SUPPORT_CAPTURE_GAIN: f32 = 3.0;
pub const WAR_SUPPORT_LOSS_PENALTY: f32 = 4.0;

/// Manpower floor below which a unit is considered destroyed.
pub const UNIT_DEATH_MANPOWER: f32 = 0.05;
/// Supply ratio below which unsupplied attrition kicks in.
pub const ATTRITION_SUPPLY_THRESHOLD: f32 = 0.25;

/// Stage 2B war-damage/reconstruction model (docs/phase2-spec.md "Stage
/// 2B — インフラと建設・戦災"): `Region::devastation` (0..1) makes holding
/// territory not the same as being able to use it.
///
/// How much of `devastation` bites into `Region::effective_infrastructure`
/// (which both `economy`'s efficiency term and `logistics`'s relay
/// propagation read instead of the raw field): `effective_infra =
/// infrastructure * (1 - devastation * INFRA_DAMAGE_SHARE)`. Kept below 1.0
/// so a fully devastated region's infrastructure is crippled, not zeroed —
/// `effective_capacity` (a flat `* (1 - devastation)`) already carries the
/// harsher, unscaled penalty for production itself.
pub const INFRA_DAMAGE_SHARE: f32 = 0.6;

/// `devastation` gained per point of raw combat damage dealt in a region
/// this tick (the sum of `military::tick_combat`'s per-side `dmg_side`,
/// before it's split across units and converted to casualties — the same
/// scale `COMBAT_DAMAGE` operates on). A single skirmish nudges devastation
/// up a little; a region that stays a front line for weeks grinds toward
/// fully devastated.
pub const DEVASTATION_PER_COMBAT_DAMAGE: f32 = 0.001;

/// One-time `devastation` spike applied the instant a region's owner
/// changes (looting, sabotage, the fighting that won it) — on top of
/// whatever combat damage already accrued during the occupation fight.
pub const DEVASTATION_ON_CAPTURE: f32 = 0.35;

/// Base daily fraction of `devastation` recovered, before the unrest/
/// stability scaling in `construction::tick_devastation_recovery`:
/// `recovery = DEVASTATION_RECOVERY * (1 - unrest/100) * (0.5 + 0.5 *
/// stability/100)`. An unruly occupied region (`unrest` near 100) recovers
/// almost nothing on its own.
pub const DEVASTATION_RECOVERY: f32 = 0.01;

/// Building-point throughput a region's construction project advances by
/// per day when fully funded (`construction::tick_construction`); the
/// actual rate is scaled down to whatever fraction of its Machinery/Steel
/// cost the faction's stock can cover that tick, so a starved project slows
/// rather than stalling outright.
pub const CONSTRUCTION_RATE: f32 = 2.0;
/// Machinery consumed, from the national stock, per building point of
/// progress funded.
pub const CONSTRUCTION_MACHINERY_PER_POINT: f32 = 0.5;
/// Steel consumed, from the national stock, per building point of progress funded.
pub const CONSTRUCTION_STEEL_PER_POINT: f32 = 1.0;

/// Building points required to complete each `Project` variant
/// (`construction::required_points`) — at `CONSTRUCTION_RATE` fully funded,
/// `Infrastructure` takes 50 days, `Port` 40, `Capacity` 30, `Repair` 20;
/// `Repair` is deliberately the cheapest so it's a real alternative to
/// passive `DEVASTATION_RECOVERY`, not a strictly worse one.
pub const CONSTRUCTION_REQUIRED_INFRASTRUCTURE: f32 = 100.0;
pub const CONSTRUCTION_REQUIRED_PORT: f32 = 80.0;
pub const CONSTRUCTION_REQUIRED_CAPACITY: f32 = 60.0;
pub const CONSTRUCTION_REQUIRED_REPAIR: f32 = 40.0;

/// Effect size of each completed project (`construction::apply_completion`),
/// per docs/phase2-spec.md Stage 2B's completion-effect table.
pub const INFRA_STEP: f32 = 0.15;
pub const PORT_STEP: f32 = 0.3;
pub const CAPACITY_STEP: f32 = 1.0;
pub const REPAIR_STEP: f32 = 0.3;

/// Stage 2C sea imports (docs/phase2-spec.md "Stage 2C — 品目別物流・港湾容量・
/// 海上輸入"): each owned, uncontested port region can pull `Food`/`Energy`
/// in from outside the map, capped per port node (never summed into one
/// national number — `trade::tick_imports` keeps every port's own
/// contribution, since Stage 2D blockades individual ports).
///
/// `port_capacity(region) = region.port * IMPORT_PER_PORT * (1 -
/// devastation)`. Scaled so a faction's full port line can plausibly close
/// the structural Food gap the Stage 2A playtest found (a faction sitting on
/// the Machinery hub but short on Food): e.g. 中央同盟 (信越・北陸/東海/近畿,
/// ports 0.5+1.2+1.3=3.0) gets a combined capacity of `3.0 * 3.0 = 9.0`
/// good/day against a civilian Food need on the order of ~9 (pop 4180 *
/// `CIVILIAN_FOOD_DEMAND_PER_POP`), enough headroom to close a production
/// shortfall without dwarfing domestic output.
pub const IMPORT_PER_PORT: f32 = 3.0;

/// Upper clamp on `Action::SetImportPlan`'s `rate` (docs/phase2-spec.md:
/// "rate は 0 以上、上限でクランプ" — out-of-range values are clamped, not
/// rejected, unlike an invalid `good`). Set comfortably above any faction's
/// realistic total port capacity (`IMPORT_PER_PORT` times the map's largest
/// port line) so it's a safety ceiling, not a routine constraint.
pub const IMPORT_PLAN_RATE_MAX: f32 = 50.0;

/// Machinery spent, from the importing faction's national stock, per unit of
/// Food/Energy actually imported (docs/phase2-spec.md: "輸入は無償ではない。
/// Machinery を輸出して支払う"). Kept low relative to `ARMS_INPUT_MACHINERY`/
/// `CONSTRUCTION_MACHINERY_PER_POINT` so a Machinery-rich, Food-poor faction
/// (the Stage 2A structural-famine case) can afford a meaningful import flow
/// out of ordinary production, not just an idle stockpile.
pub const IMPORT_COST_MACHINERY_PER_GOOD: f32 = 0.3;

/// Stage 2C node-side supply throughput cap (docs/phase2-spec.md "2. 港湾・
/// インフラによるノード側の上限"): `node_throughput(region) = NODE_BASE +
/// region.effective_infrastructure() * NODE_INFRA + region.port *
/// NODE_PORT`, applied as an extra `min()` term in
/// `logistics::recompute_supply`'s propagation alongside the existing link
/// `max_throughput()`. A region with no infrastructure and no port can still
/// relay a trickle (`NODE_BASE`); a fully-developed, high-port hub can relay
/// close to a Rail link's own ceiling (`LinkKind::Rail::max_throughput() ==
/// 25.0`), so the node cap bites mainly on devastated or underdeveloped
/// relay points, not on every link uniformly.
pub const NODE_BASE: f32 = 3.0;
pub const NODE_INFRA: f32 = 14.0;
pub const NODE_PORT: f32 = 4.0;

/// Stage 2C per-commodity delivery (docs/phase2-spec.md "3. 品目別の到達率"):
/// converts a unit's equipment gap (`UNIT_EQUIPMENT - unit.equipment`) into
/// an Arms delivery-flow demand on the same regional throughput Munitions
/// upkeep already contends for, on a comparable scale to
/// `SUPPLY_NEED_PER_MANPOWER` - a unit at its full `UNIT_EQUIPMENT` (20.0)
/// gap wants a flow of `20.0 * 0.1 == 2.0`, in the same order of magnitude
/// as one unit's peacetime (1.0) to in-combat (2.5) Munitions demand, so
/// `Faction::logistics_priority` has real contention to split rather than
/// one side dwarfing the other by construction.
pub const ARMS_SUPPLY_NEED_PER_GAP: f32 = 0.1;

/// Stage 2D (docs/phase2-spec.md "Stage 2D — 海軍・制海権・海上封鎖"): days for
/// a fleet to cross into an adjacent sea zone at full speed (no `LinkKind`
/// exists for sea-zone adjacency to derive a figure from) — set between
/// `LinkKind::Road::travel_days()` (3.0) and `LinkKind::Sea::travel_days()`
/// (6.0), the fastest and slowest land-facing figures, since open-ocean
/// fleet transit is neither as quick as a road march nor as slow as cargo
/// crossing a sea link end-to-end.
pub const FLEET_MOVE_DAYS: f32 = 4.0;

/// Naval combat's analogue of `COMBAT_DAMAGE` (docs/phase2-spec.md "3. 海戦":
/// "地形補正はなく、代わりに NAVAL_DAMAGE を用いる"). Kept equal to
/// `COMBAT_DAMAGE` — sea zones have no terrain to apply a defense bonus
/// through, so naval combat's damage scale doesn't need to be re-tuned
/// independently of land's; it only needs its own named constant so the
/// systems that use it don't share a single knob across both domains.
pub const NAVAL_DAMAGE: f32 = 8.0;

/// Sea-control threshold (docs/phase2-spec.md "2. 港の封鎖", "1. 海峡リンクの
/// 遮断") past which a faction's presence in a sea zone counts as a real
/// blockade of the ports/straits touching it, rather than a token patrol
/// that happens to have inflicted a little damage. Set well above "any
/// nonzero control" so a handful of skirmishing fleets can't flip a port's
/// import on and off; a faction needs a clear majority of the zone's naval
/// power to choke it.
pub const BLOCKADE_CONTROL_THRESHOLD: f32 = 0.6;

// ---------------------------------------------------------------------------
// Stage 3A — 国内政治勢力 (docs/phase3-spec.md "Stage 3A"): the seven
// `Group`s' support, `stability`'s redefinition as their influence-weighted
// average, and the six political events. Every situational contribution
// below is a *bounded* term added to a group's target support, never a raw
// accumulation onto `support` itself — the same target-approach discipline
// `UNREST_ADAPT_RATE` already established, now applied to something with
// far more simultaneous inputs.
// ---------------------------------------------------------------------------

/// Daily rate at which each `Faction::group_support[g]` closes the gap to
/// its freshly recomputed target (`politics::tick_politics`) — the same
/// target-approach shape as `UNREST_ADAPT_RATE`, so a
/// group's support can always recover once the pressure driving it down
/// eases, and never pins at 0 or 100 the way a raw accumulator would.
pub const GROUP_ADAPT_RATE: f32 = 0.04;

/// The target every group's support gravitates to before any situational
/// contribution is added (docs/phase3-spec.md "支持の更新": `target[g] = 50 +
/// Σ...`). Also `Faction::stability`'s value immediately after
/// `Event::RegimeChange` resets every group to this baseline (a uniform
/// reset makes the influence-weighted `stability` land exactly here too,
/// since `Faction::group_influence` always sums to 1.0).
pub const GROUP_SUPPORT_BASELINE: f32 = 50.0;

/// `conscription` (0..1) contribution: Military gains, Labor and Citizens
/// lose, scaled linearly by the policy's own value.
pub const GROUP_CONSCRIPTION_MILITARY_BONUS: f32 = 12.0;
pub const GROUP_CONSCRIPTION_LABOR_PENALTY: f32 = 8.0;
pub const GROUP_CONSCRIPTION_CITIZENS_PENALTY: f32 = 6.0;

/// `civilian_ration` being low (docs/phase3-spec.md: "civilian_ration が低
/// い") contribution, driven by how far the ration sits below
/// `CIVILIAN_RATION_MAX` relative to its full `CIVILIAN_RATION_MIN..MAX`
/// range (0 at full ration, 1 at the floor) — Military gains, Citizens lose
/// heavily (the spec's "−−"), Labor loses moderately.
pub const GROUP_RATION_MILITARY_BONUS: f32 = 8.0;
pub const GROUP_RATION_CITIZENS_PENALTY: f32 = 16.0;
pub const GROUP_RATION_LABOR_PENALTY: f32 = 6.0;

/// `industry_priority` leaning toward war matériel (docs/phase3-spec.md:
/// "industry_priority が Arms 寄り"). `industry_priority` has no direct
/// `Arms` weight to read (Arms output is capped by leftover Machinery/Steel
/// stock, not a contended input share — see `economy::tick_economy`), so
/// this reads the closest real signal: how much more of the shared Steel/
/// Energy budget is steered toward `Munitions` (war matériel) than toward
/// `Machinery` (civilian/industrial goods), clamped to `0..1` since only a
/// *positive* lean toward war production should count. Military and
/// Business gain, Citizens lose.
pub const GROUP_ARMS_LEAN_MILITARY_BONUS: f32 = 6.0;
pub const GROUP_ARMS_LEAN_BUSINESS_BONUS: f32 = 5.0;
pub const GROUP_ARMS_LEAN_CITIZENS_PENALTY: f32 = 5.0;

/// `Faction::shortage` (0..1) contribution: Citizens lose heavily (the
/// spec's "−−"), Labor and Government lose moderately.
pub const GROUP_SHORTAGE_CITIZENS_PENALTY: f32 = 20.0;
pub const GROUP_SHORTAGE_LABOR_PENALTY: f32 = 10.0;
pub const GROUP_SHORTAGE_GOVERNMENT_PENALTY: f32 = 8.0;

/// Average owned-region `unrest` (0..100, read as a `0..1` fraction)
/// contribution: LocalGovernment loses heavily (the spec's "−−", it answers
/// for local order directly), Government loses moderately.
pub const GROUP_UNREST_LOCALGOV_PENALTY: f32 = 22.0;
pub const GROUP_UNREST_GOVERNMENT_PENALTY: f32 = 10.0;

/// Average owned-region `devastation` (0..1) contribution: LocalGovernment
/// and Business both lose — war damage is a local-administration and an
/// economic problem before it's a national-government one.
pub const GROUP_DEVASTATION_LOCALGOV_PENALTY: f32 = 10.0;
pub const GROUP_DEVASTATION_BUSINESS_PENALTY: f32 = 12.0;

/// Normalizer for the day's manpower casualties (docs/phase3-spec.md: "そ
/// の日の戦死が多い"), in the same 万人/day units `WAR_SUPPORT_CASUALTY_MULT`
/// already uses — the daily loss that maxes out this contribution's `0..1`
/// factor. Set to half a fresh unit's full `UNIT_MANPOWER` (1.0) so a single
/// hard-fought battle's losses are already a meaningful jolt, not something
/// that needs a multi-unit wipeout to register.
pub const GROUP_CASUALTY_NORM: f32 = 0.5;
pub const GROUP_CASUALTY_MILITARY_PENALTY: f32 = 10.0;
pub const GROUP_CASUALTY_CITIZENS_PENALTY: f32 = 8.0;
pub const GROUP_CASUALTY_GOVERNMENT_PENALTY: f32 = 6.0;

/// Normalizer for the day's net region-count change (docs/phase3-spec.md:
/// "領土を得た"/"領土を失った"): the single-region flip that already maxes
/// out the `0..1` gain/loss factor — territory rarely changes hands faster
/// than one region at a time in a single tick, so this is a ceiling, not a
/// typical case.
pub const GROUP_TERRITORY_DELTA_CAP: f32 = 1.0;
pub const GROUP_TERRITORY_GAIN_MILITARY_BONUS: f32 = 6.0;
pub const GROUP_TERRITORY_GAIN_GOVERNMENT_BONUS: f32 = 6.0;
pub const GROUP_TERRITORY_LOSS_MILITARY_PENALTY: f32 = 8.0;
/// The spec's "−−" for a territorial loss: Government answers for losing
/// ground more harshly than Military does.
pub const GROUP_TERRITORY_LOSS_GOVERNMENT_PENALTY: f32 = 14.0;

/// `stock[Arms]` being ample (docs/phase3-spec.md: "stock[Arms] が潤沢")
/// contribution: normalized against a multiple of `UNIT_EQUIPMENT` (a full
/// unit's equipment draw) so the term reads as "how many fresh units' worth
/// of Arms are sitting in reserve," capped at `0..1`.
pub const GROUP_ARMS_STOCK_MARGIN: f32 = 3.0;
pub const GROUP_ARMS_STOCK_MILITARY_BONUS: f32 = 6.0;

/// Machinery production running well (docs/phase3-spec.md: "生産（Machinery）
/// が好調") contribution: `Faction::machinery_output_ratio` (today's actual
/// Machinery output over its input-unconstrained potential, already `0..1`)
/// scaled straight through — Business gains when the chain isn't
/// input-starved.
pub const GROUP_MACHINERY_GOOD_BUSINESS_BONUS: f32 = 6.0;

/// Stage 3A political events (docs/phase3-spec.md "政治イベント"): support
/// thresholds below which each event triggers, and the effect sizes/
/// durations each one applies. Every one of these is designed to be
/// recoverable — a live condition re-evaluated every tick (Protest, Mutiny,
/// CapitalFlight, Separatism) ends the instant support crosses back above
/// threshold, and a fixed-duration event (Strike, RegimeChange) always ends
/// on its own after its day count, even if it can start again right after.

/// Labor support threshold for `Event::Strike`.
pub const STRIKE_THRESHOLD: f32 = 38.0;
/// Fixed duration `Event::Strike` depresses industrial output for, once
/// triggered — a real strike doesn't end the instant Labor support ticks
/// back over the threshold; it runs its course.
pub const STRIKE_DAYS: u32 = 15;
/// Multiplier applied to every non-`Food` commodity's potential output
/// while a strike is active (`economy::tick_economy`) — `Food` is exempted
/// because a labor strike is an industrial-workforce action, not a farming
/// one.
pub const STRIKE_OUTPUT_MULT: f32 = 0.7;

/// Citizens support threshold for `Event::Protest`.
pub const PROTEST_THRESHOLD: f32 = 38.0;
/// Extra unrest-target pressure (`politics::tick_politics`, same units as
/// `UNREST_SHORTAGE_PRESSURE`/`UNREST_SUPPLY_PRESSURE`) applied to every
/// region a faction under active protest owns, for as long as Citizens
/// support stays below `PROTEST_THRESHOLD`.
pub const PROTEST_UNREST_BONUS: f32 = 15.0;

/// Military support threshold for `Event::Mutiny`.
pub const MUTINY_THRESHOLD: f32 = 32.0;
/// Multiplier applied to `ORG_REGEN` (`military::tick_recovery`) for every
/// unit owned by a faction under active mutiny, for as long as Military
/// support stays below `MUTINY_THRESHOLD`.
pub const MUTINY_ORG_REGEN_MULT: f32 = 0.4;

/// Business support threshold for `Event::CapitalFlight`.
pub const CAPITAL_FLIGHT_THRESHOLD: f32 = 32.0;
/// Multiplier applied to a faction's construction throughput
/// (`construction::tick_construction`) while capital flight is active.
pub const CAPITAL_FLIGHT_CONSTRUCTION_MULT: f32 = 0.5;
/// Multiplier applied to Machinery's potential output
/// (`economy::tick_economy`) while capital flight is active.
pub const CAPITAL_FLIGHT_MACHINERY_MULT: f32 = 0.7;

/// `stability` threshold for `Event::RegimeChange`.
pub const REGIME_CHANGE_THRESHOLD: f32 = 40.0;
/// Fixed duration the post-coup production disruption lasts, and the
/// cooldown before a *new* regime change can trigger for the same faction —
/// consecutive collapses are possible if the underlying squeeze continues,
/// but never faster than once every `REGIME_CHANGE_DAYS`.
pub const REGIME_CHANGE_DAYS: u32 = 30;
/// Multiplier applied to *every* commodity's potential output (including
/// `Food` — unlike `STRIKE_OUTPUT_MULT`, a change of government disrupts the
/// whole economy, not just industrial labor) while the post-coup disruption
/// is in effect.
pub const REGIME_CHANGE_OUTPUT_MULT: f32 = 0.75;

/// LocalGovernment support threshold for `Event::Separatism` (checked
/// against the *occupying* faction's own LocalGovernment support, for each
/// region it holds where `core != owner`).
pub const SEPARATISM_THRESHOLD: f32 = 42.0;
/// Daily progress (docs/phase3-spec.md: "occupation が core 勢力に向かって進
/// む"), added to `Region::occupation` toward reverting to `Region::core`,
/// while separatism is active in an occupied region with no units of any
/// faction physically present (see `politics::tick_separatism`). Kept below
/// `OCCUPATION_RATE` — this is a political drift, not a military conquest.
pub const SEPARATISM_RATE: f32 = 4.0;
/// Daily recovery of that same progress once LocalGovernment support climbs
/// back above `SEPARATISM_THRESHOLD` — the same recoverability every other
/// Stage 3A event guarantees.
pub const SEPARATISM_DECAY: f32 = 4.0;

/// External code review fix (Stage 3A, Fix 2/3): a garrison the owner keeps
/// stationed in a region under active separatism no longer vetoes the drift
/// outright (the old behaviour let holding *any* garrison there freeze the
/// meter completely, which — combined with Fix 3's playtested absorbing
/// state, docs/phase3-spec.md §0 — meant an over-extended faction could hold
/// a hostile, starving region forever for free). Instead
/// `politics::tick_separatism` scales `SEPARATISM_RATE` down by how strong
/// that garrison is *relative to the region's population and unrest* — a
/// small garrison in a big, restless region barely slows the drift; a large
/// one in a small, calm region can suppress it close to (but never all the
/// way to) a standstill. `garrison_power` is the owner's own units'
/// `combat_power()` summed in the region (zero when none are present, which
/// is exactly the old fully-unsuppressed case: `SEPARATISM_GARRISON_POP_NORM`
/// and the unrest term below both multiply a zero garrison to zero
/// suppression, so `separatist_returns_occupied_region`'s no-garrison timing
/// is unchanged).
///
/// `suppression = (garrison_power / (population * (1 +
/// unrest/100 * SEPARATISM_UNREST_GARRISON_MULT) * SEPARATISM_GARRISON_POP_NORM))
/// .clamp(0, SEPARATISM_GARRISON_MAX_SUPPRESSION)`, and the effective daily
/// rate is `SEPARATISM_RATE * (1 - suppression)`. Sized so a couple of
/// full-strength units (`combat_power` on the order of 0.5-1.0 each) meaningfully
/// suppress separatism in one of the map's smaller regions (population in the
/// low hundreds) but barely register against one of its largest urban
/// centers (population in the thousands) — holding down a big, hostile
/// population takes a correspondingly bigger garrison, with the manpower/
/// supply cost that implies (design.md §9's trade-off, not a free lever).
pub const SEPARATISM_GARRISON_POP_NORM: f32 = 0.002;
/// How much each point of regional `unrest` (0..100) raises the effective
/// population a garrison must suppress, at `unrest == 100` scaling it up by
/// this fraction (docs/phase3-spec.md's target-approach discipline: a
/// restless population is harder to hold down than a calm one of the same
/// size).
pub const SEPARATISM_UNREST_GARRISON_MULT: f32 = 1.0;
/// Ceiling on how much a garrison can suppress the separatist drift rate —
/// never 1.0, so holding a region against separatism is always eventually
/// contested by *something*, per Fix 3's "slow, not stop" mandate and the
/// project's standing rule against absorbing states (docs/phase3-spec.md §0).
pub const SEPARATISM_GARRISON_MAX_SUPPRESSION: f32 = 0.85;

// ---------------------------------------------------------------------------
// Stage 3B — 外交関係と条約 (docs/phase3-spec.md "Stage 3B"): `Stance`, the
// asymmetric `opinion` matrix, the six `Treaty` kinds, the one-tick
// pending-proposal queue, and `TradeAgreement`'s surplus-to-deficit flow.
// Per §0's carried-forward rules: `opinion` moves toward a target (0) rather
// than accumulating one-way, so no pair of factions can reach a diplomatic
// state neither can ever recover from; every treaty-affecting action is
// either idempotent (re-proposing/re-accepting an already-active treaty is
// rejected outright) or metered by a real, decrementing cooldown, never a
// ratio re-applied to a shrinking remainder - see `diplomacy.rs`'s module
// doc for how each action is made abuse-resistant against a reward
// optimiser spamming it every tick.
// ---------------------------------------------------------------------------

/// Daily rate at which `Diplomacy::opinion[a][b]` closes the gap toward its
/// 0 baseline (the same target-approach shape as `GROUP_ADAPT_RATE`/
/// `UNREST_ADAPT_RATE`) - every opinion swing from a treaty being signed,
/// broken, or a war being declared is a bounded one-time delta, not a
/// permanent shift, so no relationship can be driven to -100 and pinned
/// there forever.
pub const OPINION_DECAY_RATE: f32 = 0.03;

/// Opinion gained, both directions, when a proposed treaty is accepted
/// (`action::apply_accept_treaty`).
pub const TREATY_ACCEPT_OPINION_BONUS: f32 = 15.0;

/// Opinion lost, both directions, when `Action::DeclareWar` breaks a
/// `Ceasefire` (docs/phase3-spec.md: "いつでも DeclareWar で破棄できる") - the
/// single largest diplomatic penalty, since ending a truce outright is the
/// most hostile bilateral act short of the war itself.
pub const DECLARE_WAR_OPINION_PENALTY: f32 = 40.0;

/// Notice period (days) `Action::BreakTreaty` on a `NonAggression` pact must
/// serve before the pair actually reverts to `Stance::War`
/// (docs/phase3-spec.md: "破棄には NON_AGGRESSION_NOTICE_DAYS の予告が要る") -
/// tracked per pair in `Diplomacy::pending_breaks` and counted down by
/// `diplomacy::tick_diplomacy`; combat and occupation stay suppressed for
/// the whole notice period, since the stance itself doesn't change until it
/// elapses.
pub const NON_AGGRESSION_NOTICE_DAYS: u32 = 10;
/// Opinion lost, both directions, the moment notice is served (not when the
/// war actually starts `NON_AGGRESSION_NOTICE_DAYS` later) - breaking the
/// pact is the diplomatic act; the war that follows is `DECLARE_WAR`'s own
/// betrayal-scale penalty, not repeated here.
pub const NON_AGGRESSION_BREAK_OPINION_PENALTY: f32 = 20.0;

/// Opinion lost, both directions, when `Action::BreakTreaty` ends an
/// `Alliance` (immediate: stance falls back to `Ceasefire`, not war - an
/// alliance ending is a diplomatic rupture, not a declaration of war).
pub const ALLIANCE_BREAK_OPINION_PENALTY: f32 = 30.0;

/// Opinion lost, both directions, when `Action::BreakTreaty` cancels a
/// `MilitaryAccess`/`PortAccess`/`TradeAgreement` grant - the smallest
/// break penalty, since these are working arrangements, not the core
/// war/peace relationship.
pub const MINOR_TREATY_BREAK_OPINION_PENALTY: f32 = 10.0;

/// Days a (pair, `Treaty` kind) combination is locked out of
/// `Action::ProposeTreaty` after that treaty was broken (`BreakTreaty`) or,
/// for `Ceasefire` specifically, after `Action::DeclareWar` ends one - the
/// abuse-resistance guard named in docs/phase3-spec.md §0: without a real,
/// decrementing cooldown a reward optimiser could cycle
/// propose-accept-break every tick to keep re-harvesting
/// `TREATY_ACCEPT_OPINION_BONUS`, or declare war and instantly re-propose
/// `Ceasefire` to dodge `NonAggression`'s notice period in spirit.
pub const TREATY_COOLDOWN_DAYS: u32 = 20;

/// How many days a `PendingProposal` survives, once created, before
/// `diplomacy::tick_diplomacy` expires it unanswered
/// (docs/phase3-spec.md: "提案は 1 tick 保留され、相手の応答を待つ"). Not a
/// spendable resource of its own - re-proposing after expiry costs nothing
/// beyond the normal cooldown/duplicate-proposal rules, so a genuinely
/// interested counterpart is never permanently locked out by one missed day.
///
/// External code review fix (C1/C2 re-audit): kept above the bare "1 tick"
/// the spec line names, because a literal 1-day hold combined with
/// `archipelago-agents`' `HeuristicAgent` (each faction only decides once
/// every `period` days, offset by faction id so they don't all act the same
/// day) makes some ordered pairs structurally unable to ever answer a
/// proposal in time - not a rare edge case: with `period == 4` and 3
/// factions at offsets `0`/`1`/`2`, a proposal from the offset-`2` faction
/// to the offset-`0` one needs 2 days before that target's next turn, but a
/// 1-day TTL is already gone by then, so *every* proposal in that direction
/// (and the reverse) silently expired unanswered, regardless of how
/// favorable its terms were. `3` comfortably covers the worst gap any
/// small, offset-staggered set of agents can produce against this same
/// period without over-extending how long a stale offer lingers.
pub const PROPOSAL_TTL_DAYS: u32 = 3;

/// Fraction of a `TradeAgreement` exporter's own stock of a tradeable good
/// (`Food`/`Energy`/`Machinery`) that is reserved for its own domestic use
/// before any of it counts as exportable surplus
/// (`trade::tick_trade_agreements`): `surplus = stock * (1 -
/// TRADE_SURPLUS_RESERVE_FRACTION)`. A fraction of current stock rather than
/// a flat number so the reserve scales sensibly across goods that start at
/// very different stockpile levels (`scenario::FACTION_STOCK`).
pub const TRADE_SURPLUS_RESERVE_FRACTION: f32 = 0.5;

/// Ceiling on how much of one tradeable good one `TradeAgreement` partner
/// can pull from another in a single day, at the importer's `shortage_by_good`
/// fully saturated (`== 1.0`) - kept on the same order as `IMPORT_PER_PORT`
/// (world-market imports' own per-port ceiling) so a trade partnership is a
/// meaningful substitute for (or complement to) the world market, not a
/// dominant or negligible one.
pub const TRADE_FLOW_RATE_MAX: f32 = 6.0;

/// External code review-style fix, applied up front (docs/phase3-spec.md §0:
/// "複数の主体が奪い合う量は必ず比率で按分する"): `TradeAgreement` inflow and
/// world-market import inflow for the same importing faction both draw on
/// the *same* pooled port capacity (`trade::tick_imports` /
/// `trade::tick_trade_agreements`'s shared `capacity[f]`, which
/// `Treaty::PortAccess` can extend with a partner's own ports). Neither is
/// computed first and the other given only the leftover - both wanted
/// amounts are summed, and if they exceed capacity both are scaled down by
/// the same ratio, `capacity / (world_wanted + trade_wanted)`. No named
/// constant is needed for the split itself; `trade::tick_imports` and
/// `trade::tick_trade_agreements` both implement this rule.

/// External code review fix (Stage 2D): floor on `naval::strait_factor`
/// (`1 - enemy_control_max`) when it is applied to a crossing's per-tick
/// movement progress in `military::tick_movement`. `strait_factor` is
/// already `.clamp(0.0, 1.0)`, so this is not a divide-by-zero guard — it
/// exists so a unit mid-crossing under a total (or near-total) blockade
/// still creeps forward at a slow trickle each tick instead of making
/// literally zero progress, keeping the per-tick math finite and away from
/// the degenerate all-progress-happens-in-one-instant edge a bare `* 0.0`
/// would produce every tick it's fully blockaded.
pub const STRAIT_CROSSING_FACTOR_FLOOR: f32 = 0.05;
