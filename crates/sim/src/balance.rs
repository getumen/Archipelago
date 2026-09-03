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

/// Daily rate at which `stability` closes the gap to its target value.
pub const STABILITY_ADAPT_RATE: f32 = 0.02;

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
