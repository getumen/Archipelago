//! All tunable balance constants for the simulation, gathered in one place
//! so systems never carry magic numbers of their own.

pub const WORKFORCE_SHARE: f32 = 0.5;
pub const INDUSTRY_OUTPUT_PER_POINT: f32 = 1.10;
pub const FOOD_OUTPUT_PER_POINT: f32 = 0.9;
pub const CIVILIAN_DEMAND_PER_POP: f32 = 0.0023;
pub const FOOD_DEMAND_PER_POP: f32 = 0.0022;
pub const CONSCRIPT_RATE: f32 = 0.00035;

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
