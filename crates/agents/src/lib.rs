//! A rule-based `Agent` for `archipelago-sim` (design.md §19 / mvp-spec.md §7):
//! set economic policy, reinforce weakened units, recruit where it's safe,
//! pick offensives by value-per-threat, and walk idle interior units toward
//! the front. No randomness of its own, so a run stays fully determined by
//! the simulation's seed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{
    ARMS_INPUT_MACHINERY, ARMS_INPUT_STEEL, CIVILIAN_ENERGY_DEMAND_PER_POP,
    CIVILIAN_FOOD_DEMAND_PER_POP, CIVILIAN_RATION_MAX, COMBAT_SUPPLY_MULT, MACHINERY_INPUT_STEEL,
    MUNITIONS_INPUT_STEEL, MUTINY_THRESHOLD, PROTEST_THRESHOLD, REGIME_CHANGE_THRESHOLD,
    STRIKE_THRESHOLD, SUPPLY_NEED_PER_MANPOWER, UNIT_EQUIPMENT, UNIT_MANPOWER,
};
use archipelago_sim::construction::Project;
use archipelago_sim::good::Good;
use archipelago_sim::group::Group;
use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use archipelago_sim::military::Unit;
use archipelago_sim::naval;
use archipelago_sim::observation::Observation;
use archipelago_sim::world::{Domain, Station, World};

/// Whether `unit` currently shares its station with an enemy of `faction` -
/// the domain-generic form of the land-only `world.has_enemy_units` /
/// sea-only `world.has_enemy_fleets` checks, for AI code that walks both
/// land units and fleets through `Observation::own_units`.
fn unit_contested(world: &World, unit: &Unit, faction: FactionId) -> bool {
    match unit.station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    }
}

/// Minimum unit-count headroom (as a multiple of a fresh unit's cost) a
/// faction keeps in reserve before it will spend on a new recruit.
const RECRUIT_STOCK_MARGIN: f32 = 1.5;
/// A unit below this fraction of full manpower/equipment gets reinforced.
const REINFORCE_THRESHOLD: f32 = 0.75;
/// Days of stockpiled Munitions below which the agent shifts the shared
/// Steel/Energy input toward Munitions instead of Machinery (design.md §9's
/// war/economy trade-off, now expressed as an `industry_priority` split).
const LOW_SUPPLY_DAYS: f32 = 20.0;
/// Munitions industry-priority weight used when the Munitions stockpile is
/// running low (Machinery gets `1.0 - this`).
const MUNITIONS_FOCUSED_WEIGHT: f32 = 0.7;
/// Machinery industry-priority weight used when Arms production is starved
/// on Machinery input specifically (Stage 2A's "detect the blocking
/// upstream good and raise its priority").
const MACHINERY_FOCUSED_WEIGHT: f32 = 0.7;
/// Even split used when neither side is under particular pressure.
const BALANCED_WEIGHT: f32 = 0.5;
/// Manpower pool above which conscription is throttled hard - hoarding
/// manpower has a real cost since it suppresses labour via `region.mobilized`.
const CONSCRIPTION_THROTTLE_MANPOWER: f32 = 25.0;
/// Fraction of `unit_cap` (own unit count / cap) above which the faction is
/// considered to have nowhere left to spend manpower: `recruit` stops
/// raising new units once the cap is reached, so a pool this large just
/// sits idle suppressing `labor_ratio` via `region.mobilized` for nothing.
/// Combined with `CONSCRIPTION_THROTTLE_MANPOWER` (an already-large
/// reserve), this is when the agent stops drafting entirely instead of
/// merely throttling it - the pool's own demobilization
/// (`balance::MANPOWER_DEMOBILIZATION_RATE`) handles bringing it back down.
const STOP_CONSCRIPTION_UNIT_CAP_FRACTION: f32 = 0.9;
/// `civilian_ration` used when Munitions/Arms are critically short and
/// stability can still absorb it (design.md §9's civilian/war trade-off):
/// squeeze civilian Food/Energy/Machinery delivery down to this fraction to
/// free up stock for the war economy, at the cost of raising `shortage`
/// (and therefore unrest) pressure.
const CIVILIAN_RATION_LOW: f32 = 0.7;
/// Stability floor below which the agent stops rationing and returns
/// `civilian_ration` to 1.0 even if Munitions/Arms are still short - unrest
/// is already a problem, so squeezing civilians further isn't worth it.
const RATION_STABILITY_FLOOR: f32 = 60.0;
/// Arms stockpile, in unit-equivalents of `UNIT_EQUIPMENT`, below which
/// Arms counts as "critically short" for rationing purposes (mirrors
/// `RECRUIT_STOCK_MARGIN`'s notion of a comfortable buffer).
const ARMS_LOW_UNIT_MARGIN: f32 = 1.5;
/// `Region::devastation` above which the agent's top build priority
/// (docs/phase2-spec.md Stage 2B) is repairing a damaged own region rather
/// than expanding capacity or infrastructure elsewhere.
const REPAIR_THRESHOLD: f32 = 0.35;
/// Minimum days of Arms-equivalent Machinery/Steel stock (see
/// `machinery_limited_arms_days` in `set_policy`) that must remain before
/// the agent will spend on construction at all - so building never eats
/// into the stockpile the war effort itself needs (Stage 2B: "Machinery /
/// Steel の在庫が軍需の余裕分を下回っている間は着工しない").
const BUILD_STOCK_RESERVE_DAYS: f32 = 15.0;
/// Stage 2C sea imports (docs/phase2-spec.md "Stage 2C": "shortage が出て
/// いるなら不足量を埋めるだけの輸入を要求し"): the agent requests an import
/// rate scaled by `Faction::shortage` (0 when unshortaged, up to the full
/// civilian Food/Energy need at `shortage == 1.0`) rather than always asking
/// for the theoretical maximum - `trade::tick_imports` would cap an
/// over-large request at port capacity/Machinery affordability anyway, but
/// asking only for what's actually missing keeps the request meaningful as
/// a diagnostic and avoids needlessly bidding away Machinery the war economy
/// might still need.
const IMPORT_REQUEST_SHORTAGE_SCALE: f32 = 1.5;
/// Machinery stockpile, in import-equivalents of a full day's Food+Energy
/// civilian need, below which the agent throttles its import request
/// (design.md §9-style trade-off: imports are worth less than keeping the
/// war economy's own Machinery reserve solvent) rather than bidding for
/// imports it can't really afford to keep paying for.
const IMPORT_MACHINERY_LOW_DAYS: f32 = 10.0;
/// Import request multiplier applied when Machinery is running low
/// (`IMPORT_MACHINERY_LOW_DAYS`).
const IMPORT_THROTTLE_WEIGHT: f32 = 0.4;
/// `unit.supply` / `unit.strength()` average below which the fleet is
/// considered pressured on that axis for `logistics_priority` purposes
/// (docs/phase2-spec.md: "部隊の平均 supply が低ければ Munitions 寄り、部隊の
/// 平均 strength が低ければ Arms 寄りにする").
const LOGISTICS_PRESSURE_THRESHOLD: f32 = 0.75;
/// Logistics-priority weight given to whichever good the fleet is pressured
/// on (the other gets `1.0 -` this); an even split when neither or both axes
/// are under pressure, so neither ever gets a fixed unconditional priority.
const LOGISTICS_FOCUSED_WEIGHT: f32 = 0.7;
/// Stage 2D naval AI (docs/phase2-spec.md "Stage 2D" AI section, point 1):
/// fleet count below which the agent keeps building fleets at a safe home
/// port, mirroring `unit_cap`'s role for land recruitment but as a small
/// fixed floor rather than one scaled off industry — a minimal navy, not a
/// second army.
const NAVY_MIN_FLEETS: f32 = 2.0;
/// Naval counterpart of `offensive()`'s land engagement margin: added to
/// the enemy power a target zone's `caution` threshold is judged against,
/// so a fleet won't engage a target at exact parity with nothing in
/// reserve.
const NAVY_ENGAGE_MARGIN: f32 = 0.6;

/// Stage 3A AI (docs/phase3-spec.md "AI" under "Stage 3A"): how far above a
/// political event's own threshold (`STRIKE_THRESHOLD`/`PROTEST_THRESHOLD`/
/// `MUTINY_THRESHOLD`/`REGIME_CHANGE_THRESHOLD`) the agent starts reacting -
/// it eases off *before* the event actually fires, not after, since waiting
/// for the event itself means the damage (a strike, a mutiny, a coup) has
/// already landed.
const POLITICAL_SUPPORT_MARGIN: f32 = 6.0;
/// `conscription` ceiling the agent imposes once Labor or Citizens support
/// is within `POLITICAL_SUPPORT_MARGIN` of triggering `Event::Strike`/
/// `Event::Protest` - overrides whatever `set_policy`'s manpower-driven
/// tiers would otherwise pick.
const POLITICAL_CONSCRIPTION_CEILING: f32 = 0.3;
/// `industry_priority` weight given to Munitions (the Arms-leaning side of
/// the shared Steel/Energy budget - see `GROUP_ARMS_LEAN_*` in
/// `balance.rs`) when Military support is under political pressure
/// (Machinery gets `1.0 -` this).
const POLITICAL_ARMS_LEAN_WEIGHT: f32 = 0.75;
/// `offensive()`/`naval_ops()` `caution` multiplier applied when `stability`
/// is within `POLITICAL_SUPPORT_MARGIN` of `REGIME_CHANGE_THRESHOLD`
/// (docs/phase3-spec.md: "軍事行動より内政を優先する（攻勢の caution を一時
/// 的に引き上げる）") - a higher `caution` makes the agent wait for a bigger
/// force-ratio edge before committing to a new offensive, without touching
/// units already under way.
const POLITICAL_CAUTION_BOOST: f32 = 1.35;

/// Decides for one faction every `period` days (offset by faction id so the
/// three AIs don't all act on the same day), per mvp-spec.md §7.
pub struct HeuristicAgent {
    faction: FactionId,
    caution: f32,
    period: u32,
    offset: u32,
}

impl HeuristicAgent {
    /// `caution` is the force-ratio margin required before the agent will
    /// launch an offensive: it attacks only when
    /// `own_power >= caution * (defended_power + 0.6)`. Higher values make
    /// the agent more cautious (it waits for a bigger edge); ~1.0 means it
    /// will attack at parity. Values around 1.1-1.5 work well (the MVP
    /// scenario uses 1.15 / 1.30 / 1.45 for its three factions).
    pub fn new(faction: FactionId, caution: f32) -> Self {
        const PERIOD: u32 = 4;
        HeuristicAgent {
            faction,
            caution,
            period: PERIOD,
            offset: faction.0 % PERIOD,
        }
    }
}

impl Agent for HeuristicAgent {
    fn name(&self) -> &str {
        "HeuristicAgent"
    }

    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        if obs.world.day % self.period != self.offset {
            return Vec::new();
        }

        let mut actions = Vec::new();
        set_policy(self.faction, obs, &mut actions);
        set_trade_policy(self.faction, obs, &mut actions);
        set_logistics_priority(obs, &mut actions);
        reinforce(self.faction, obs, &mut actions);
        recruit(self.faction, obs, &mut actions);
        naval_recruit(self.faction, obs, &mut actions);
        build(self.faction, obs, &mut actions);

        // Stage 3A AI (docs/phase3-spec.md: "stability が REGIME_CHANGE_THRESHOLD
        // に近いときは、軍事行動より内政を優先する"): raise the force-ratio
        // bar for launching a *new* offensive when the government is close
        // to falling, rather than spending what's left of its support on a
        // war it might not survive to finish.
        let stability = obs.world.faction(self.faction).stability;
        let caution = if stability < REGIME_CHANGE_THRESHOLD + POLITICAL_SUPPORT_MARGIN {
            self.caution * POLITICAL_CAUTION_BOOST
        } else {
            self.caution
        };
        offensive(self.faction, caution, obs, &mut actions);
        naval_ops(self.faction, caution, obs, &mut actions);

        let already_moved: BTreeSet<UnitId> = actions
            .iter()
            .filter_map(|a| match a {
                Action::MoveUnit { unit, .. } => Some(*unit),
                _ => None,
            })
            .collect();
        advance_interior(self.faction, obs, &already_moved, &mut actions);

        actions
    }
}

/// Unit-count ceiling the agent recruits toward (mvp-spec.md §7.3): scales
/// with industrial base so a stronger economy can support a bigger army.
/// Shared by `recruit` (which stops raising new units at this cap) and
/// `set_policy` (which uses proximity to this cap to decide whether a large
/// manpower pool still has somewhere to go).
fn unit_cap(faction: FactionId, obs: &Observation) -> f32 {
    3.0 + obs.world.industry_total(faction) / 5.0
}

/// Count of `obs.own_units()` in a given domain — `unit_cap`/`recruit`'s
/// land army sizing must not be diluted by fleets sharing the same
/// `own_units()` list Stage 2D introduced (`naval_recruit` has its own,
/// separate `NAVY_MIN_FLEETS` floor).
fn own_unit_count(obs: &Observation, domain: Domain) -> usize {
    obs.own_units()
        .into_iter()
        .filter(|&u| obs.world.unit(u).station.domain() == domain)
        .count()
}

fn set_policy(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let manpower = f.manpower;
    let at_unit_cap = own_unit_count(obs, Domain::Land) as f32
        >= unit_cap(faction, obs) * STOP_CONSCRIPTION_UNIT_CAP_FRACTION;
    let conscription: f32 = if manpower < 5.0 {
        0.9
    } else if manpower > CONSCRIPTION_THROTTLE_MANPOWER {
        // A large pool with no unit-cap headroom left to spend it on is
        // just dead weight suppressing `labor_ratio` - stop drafting
        // entirely and let demobilization drain it back to the workforce.
        // Otherwise keep the existing hard throttle: there's still room to
        // grow the army, so a trickle of conscription is worth its cost.
        if at_unit_cap { 0.0 } else { 0.15 }
    } else {
        0.6
    };

    // Stage 3A AI (docs/phase3-spec.md: "Labor か Citizens が閾値に近づいたら
    // civilian_ration を戻し、conscription を下げる"): react before
    // `Event::Strike`/`Event::Protest` actually fires, not after.
    let labor_near = f.group_support[Group::Labor.index()] < STRIKE_THRESHOLD + POLITICAL_SUPPORT_MARGIN;
    let citizens_near =
        f.group_support[Group::Citizens.index()] < PROTEST_THRESHOLD + POLITICAL_SUPPORT_MARGIN;
    let political_squeeze = labor_near || citizens_near;
    let conscription = if political_squeeze {
        conscription.min(POLITICAL_CONSCRIPTION_CEILING)
    } else {
        conscription
    };
    actions.push(Action::SetConscription(conscription));

    let daily_demand: f32 = obs
        .own_units()
        .into_iter()
        .map(|unit_id| {
            let unit = obs.world.unit(unit_id);
            let mult = if unit_contested(obs.world, unit, faction) {
                COMBAT_SUPPLY_MULT
            } else {
                1.0
            };
            unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult
        })
        .sum();
    let munitions = f.stock[Good::Munitions.index()];
    let munitions_running_low = daily_demand > 0.0 && munitions / daily_demand < LOW_SUPPLY_DAYS;

    // Detect whether Arms production is bottlenecked on Machinery input
    // specifically (rather than Steel): if the Machinery stock funds fewer
    // days of Arms output than the Steel stock does, Machinery is the
    // blocking upstream good, and raising its share of the shared
    // Steel/Energy input (at Munitions' expense) is what relieves it.
    let machinery_limited_arms_days = f.stock[Good::Machinery.index()] / ARMS_INPUT_MACHINERY;
    let steel_limited_arms_days = f.stock[Good::Steel.index()] / ARMS_INPUT_STEEL;
    let arms_blocked_on_machinery = machinery_limited_arms_days < steel_limited_arms_days;

    // Stage 3A AI (docs/phase3-spec.md: "Military が低ければ Arms 寄りに
    // industry_priority を振る"): a Military group nearing `Event::Mutiny`
    // overrides the ordinary Munitions-stockpile/Machinery-bottleneck
    // reasoning above - keeping the army happy takes priority over either.
    let military_near =
        f.group_support[Group::Military.index()] < MUTINY_THRESHOLD + POLITICAL_SUPPORT_MARGIN;
    let (machinery_weight, munitions_weight) = if military_near {
        (1.0 - POLITICAL_ARMS_LEAN_WEIGHT, POLITICAL_ARMS_LEAN_WEIGHT)
    } else if munitions_running_low {
        (1.0 - MUNITIONS_FOCUSED_WEIGHT, MUNITIONS_FOCUSED_WEIGHT)
    } else if arms_blocked_on_machinery {
        (MACHINERY_FOCUSED_WEIGHT, 1.0 - MACHINERY_FOCUSED_WEIGHT)
    } else {
        (BALANCED_WEIGHT, BALANCED_WEIGHT)
    };
    actions.push(Action::SetIndustryPriority { good: Good::Machinery, weight: machinery_weight });
    actions.push(Action::SetIndustryPriority { good: Good::Munitions, weight: munitions_weight });

    // Civilian rationing (design.md §9): when Munitions or Arms are
    // critically short and stability can still absorb the unrest cost,
    // divert some civilian Food/Energy/Machinery delivery to the war
    // economy. Ease off (back to full delivery) once stability drops too
    // far or the stockpiles have recovered - rationing further at that
    // point just compounds the unrest it caused.
    let arms_low = f.stock[Good::Arms.index()] < UNIT_EQUIPMENT * ARMS_LOW_UNIT_MARGIN;
    let ration = if (munitions_running_low || arms_low) && f.stability > RATION_STABILITY_FLOOR {
        CIVILIAN_RATION_LOW
    } else {
        1.0
    };
    // Stage 3A AI: restoring full rationing takes priority over the war
    // economy's own appetite once Labor/Citizens support is under political
    // pressure - the same override `conscription` above gets.
    let ration = if political_squeeze { CIVILIAN_RATION_MAX } else { ration };
    actions.push(Action::SetCivilianRation(ration));
}

/// Stage 2C sea imports (docs/phase2-spec.md "Stage 2C" AI section): request
/// enough Food/Energy import to close whatever share of civilian demand
/// `Faction::shortage_by_good` says is currently missing *for that specific
/// commodity* (external code review fix - the aggregate `Faction::shortage`
/// is the worst of Food/Energy/Machinery and can be nonzero from Machinery
/// alone, which used to make this request both Food and Energy at full
/// scale even when one of them was perfectly well-stocked), throttled back
/// when the Machinery that pays for it is itself running low.
fn set_trade_policy(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let total_pop: f32 = obs.own_regions().iter().map(|&r| obs.world.region(r).population).sum();
    let food_need = total_pop * CIVILIAN_FOOD_DEMAND_PER_POP;
    let energy_need = total_pop * CIVILIAN_ENERGY_DEMAND_PER_POP;

    let machinery_days = if food_need + energy_need > 0.0 {
        f.stock[Good::Machinery.index()] / (food_need + energy_need)
    } else {
        f32::INFINITY
    };
    let throttle = if machinery_days < IMPORT_MACHINERY_LOW_DAYS {
        IMPORT_THROTTLE_WEIGHT
    } else {
        1.0
    };

    // External code review fix (Stage 2C): scale each commodity's request
    // off *that commodity's own* deficit (`shortage_by_good`), not the
    // aggregate `shortage` (the worst of Food/Energy/Machinery). The old
    // code requested both Food and Energy proportional to full demand
    // whenever aggregate shortage was nonzero, even when only one was
    // actually short - the two plans then competed for the same port
    // capacity and the same Machinery payment, crowding out the commodity
    // that genuinely needed the import with surplus of the one that didn't.
    let food_scale = f.shortage_by_good[Good::Food.index()] * IMPORT_REQUEST_SHORTAGE_SCALE * throttle;
    let energy_scale = f.shortage_by_good[Good::Energy.index()] * IMPORT_REQUEST_SHORTAGE_SCALE * throttle;
    actions.push(Action::SetImportPlan { good: Good::Food, rate: food_need * food_scale });
    actions.push(Action::SetImportPlan { good: Good::Energy, rate: energy_need * energy_scale });
}

/// Stage 2C per-commodity delivery (docs/phase2-spec.md "Stage 2C" AI
/// section): shift `logistics_priority` toward Munitions when the fleet's
/// average `supply` is under pressure, toward Arms when its average
/// `strength()` is - an even split when neither (or both) axis is
/// pressured, so neither good gets a fixed unconditional priority.
fn set_logistics_priority(obs: &Observation, actions: &mut Vec<Action>) {
    let units = obs.own_units();
    if units.is_empty() {
        return;
    }

    let mut supply_sum = 0.0f32;
    let mut strength_sum = 0.0f32;
    for &unit_id in &units {
        let unit = obs.world.unit(unit_id);
        supply_sum += unit.supply;
        strength_sum += unit.strength();
    }
    let avg_supply = supply_sum / units.len() as f32;
    let avg_strength = strength_sum / units.len() as f32;

    let low_supply = avg_supply < LOGISTICS_PRESSURE_THRESHOLD;
    let low_strength = avg_strength < LOGISTICS_PRESSURE_THRESHOLD;
    let (munitions_weight, arms_weight) = match (low_supply, low_strength) {
        (true, false) => (LOGISTICS_FOCUSED_WEIGHT, 1.0 - LOGISTICS_FOCUSED_WEIGHT),
        (false, true) => (1.0 - LOGISTICS_FOCUSED_WEIGHT, LOGISTICS_FOCUSED_WEIGHT),
        _ => (0.5, 0.5),
    };
    actions.push(Action::SetLogisticsPriority { good: Good::Munitions, weight: munitions_weight });
    actions.push(Action::SetLogisticsPriority { good: Good::Arms, weight: arms_weight });
}

/// Tops up under-strength units (land or fleet) sitting safely in friendly,
/// uncontested territory/waters. A land unit's station must additionally be
/// its own faction's region (matches the pre-Stage-2D check exactly); a sea
/// zone has no owner, so a fleet only needs to be uncontested.
fn reinforce(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let mut units = obs.own_units();
    units.sort_by_key(|u| u.0);

    for unit_id in units {
        let unit = obs.world.unit(unit_id);
        let safe = match unit.station {
            Station::Region(r) => {
                obs.world.region(r).owner == faction && !obs.world.has_enemy_units(r, faction)
            }
            Station::Sea(z) => !obs.world.has_enemy_fleets(z, faction),
        };
        if !safe {
            continue;
        }
        if unit.strength() < REINFORCE_THRESHOLD {
            actions.push(Action::ReinforceUnit { unit: unit_id });
        }
    }
}

/// Raises a new corps at the capital, or failing that the safest, most
/// industrious region held, as long as the faction can afford it and isn't
/// already well-manned relative to its industrial base.
fn recruit(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let cap = unit_cap(faction, obs);
    if own_unit_count(obs, Domain::Land) as f32 >= cap {
        return;
    }
    if f.manpower < UNIT_MANPOWER * RECRUIT_STOCK_MARGIN
        || f.stock[Good::Arms.index()] < UNIT_EQUIPMENT * RECRUIT_STOCK_MARGIN
    {
        return;
    }

    let capital = f.capital;
    let region = if is_safe_own_region(faction, obs, capital) {
        Some(capital)
    } else {
        safe_own_regions(faction, obs)
            .into_iter()
            .fold(None, |best: Option<(RegionId, f32)>, r| {
                let industry = obs.world.region(r).industry_total();
                match best {
                    Some((_, best_industry)) if industry <= best_industry => best,
                    _ => Some((r, industry)),
                }
            })
            .map(|(r, _)| r)
    };

    if let Some(region) = region {
        actions.push(Action::RecruitUnit { region, domain: Domain::Land });
    }
}

fn is_safe_own_region(faction: FactionId, obs: &Observation, region: RegionId) -> bool {
    obs.world.region(region).owner == faction && !obs.world.has_enemy_units(region, faction)
}

/// Own regions with no enemy units present, in ascending id order.
fn safe_own_regions(faction: FactionId, obs: &Observation) -> Vec<RegionId> {
    let mut regions: Vec<RegionId> = obs
        .own_regions()
        .into_iter()
        .filter(|&r| !obs.world.has_enemy_units(r, faction))
        .collect();
    regions.sort_by_key(|r| r.0);
    regions
}

/// The best own, uncontested port region to operate a navy out of - highest
/// `port` value, ties broken toward the lowest region id - or `None` if the
/// faction holds no safe port at all. Shared by `naval_recruit` (where to
/// build) and `home_zone` (where an idle fleet with nothing else to do
/// returns to).
fn best_own_port_region(faction: FactionId, obs: &Observation) -> Option<RegionId> {
    safe_own_regions(faction, obs)
        .into_iter()
        .filter(|&r| obs.world.region(r).port > 0.0)
        .fold(None, |best: Option<(RegionId, f32)>, r| {
            let port = obs.world.region(r).port;
            match best {
                Some((_, best_port)) if port <= best_port => best,
                _ => Some((r, port)),
            }
        })
        .map(|(r, _)| r)
}

/// The sea zone the faction's main port faces (docs/phase2-spec.md Stage 2D
/// AI point 4: "自国の主要港の海域"), if it holds a safe port at all.
fn home_zone(faction: FactionId, obs: &Observation) -> Option<SeaZoneId> {
    naval::home_zone(obs.world, best_own_port_region(faction, obs)?)
}

/// Sea zones touching a port region matching `own` (`true`: this faction's
/// own regions; `false`: any other faction's) — regardless of whether that
/// region is currently contested, since a blockade target or a defense
/// target is about the port's *owner*, not today's fighting there.
fn port_zones(faction: FactionId, obs: &Observation, own: bool) -> BTreeSet<SeaZoneId> {
    let mut zones = BTreeSet::new();
    for region in &obs.world.regions {
        let matches_owner = if own { region.owner == faction } else { region.owner != faction };
        if matches_owner && region.port > 0.0 {
            zones.extend(obs.world.zones_touching(region.id));
        }
    }
    zones
}

/// Stage 2D naval AI (docs/phase2-spec.md "Stage 2D" AI section, point 1):
/// keeps at least `NAVY_MIN_FLEETS` fleets in being, built at the faction's
/// best safe port, the same affordability gate `recruit` uses for land units.
fn naval_recruit(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    if own_unit_count(obs, Domain::Sea) as f32 >= NAVY_MIN_FLEETS {
        return;
    }
    if f.manpower < UNIT_MANPOWER * RECRUIT_STOCK_MARGIN
        || f.stock[Good::Arms.index()] < UNIT_EQUIPMENT * RECRUIT_STOCK_MARGIN
    {
        return;
    }
    if let Some(region) = best_own_port_region(faction, obs) {
        actions.push(Action::RecruitUnit { region, domain: Domain::Sea });
    }
}

/// Stage 2D naval AI (docs/phase2-spec.md "Stage 2D" AI section, points
/// 2-4): moves idle fleets, grouped by their current zone, toward whichever
/// of three priorities applies -
/// 1. (point 2) clear enemy fleets from a zone touching one of this
///    faction's own ports, if reachable and the force ratio (`caution`,
///    the same margin `offensive()` uses for land) favors attacking;
/// 2. (point 3) otherwise, contest a zone touching an *enemy* port under
///    the same force-ratio gate — sea denial against the faction most
///    dependent on that water;
/// 3. (point 4) otherwise, head back to the faction's main home port zone,
///    where it stays once there (no target found next time it's idle).
fn naval_ops(faction: FactionId, caution: f32, obs: &Observation, actions: &mut Vec<Action>) {
    let own_port_zones = port_zones(faction, obs, true);
    let enemy_port_zones = port_zones(faction, obs, false);

    let mut idle_by_zone: BTreeMap<SeaZoneId, Vec<UnitId>> = BTreeMap::new();
    for unit_id in obs.own_units() {
        let unit = obs.world.unit(unit_id);
        if unit.movement.is_some() {
            continue;
        }
        if let Some(zone) = unit.station.sea_zone() {
            idle_by_zone.entry(zone).or_default().push(unit_id);
        }
    }
    if idle_by_zone.is_empty() {
        return;
    }

    let home = home_zone(faction, obs);

    for (&zone, fleets) in &idle_by_zone {
        let reachable_zones = |candidates: &BTreeSet<SeaZoneId>| -> Vec<SeaZoneId> {
            candidates
                .iter()
                .copied()
                .filter(|&z| z == zone || obs.world.sea_zone(zone).adjacent.contains(&z))
                .collect()
        };

        // Point 2: defend an own port zone under threat - the most
        // dangerous reachable one (highest enemy power) is the most urgent.
        let defend_target = reachable_zones(&own_port_zones)
            .into_iter()
            .map(|z| (z, obs.enemy_zone_power(z)))
            .filter(|&(_, danger)| danger > 0.0)
            .fold(None, |best: Option<(SeaZoneId, f32)>, (z, danger)| {
                match best {
                    Some((_, best_danger)) if danger <= best_danger => best,
                    _ => Some((z, danger)),
                }
            })
            .map(|(z, _)| z);

        // Point 3: contest an enemy port zone - same `value / (1 +
        // enemy_power)` scoring `offensive()` uses for land targets, so the
        // agent prefers a valuable, weakly-held zone over a strongly
        // defended one.
        let blockade_target = reachable_zones(&enemy_port_zones)
            .into_iter()
            .fold(None, |best: Option<(SeaZoneId, f32)>, z| {
                let score = zone_value(obs, z) / (1.0 + obs.enemy_zone_power(z));
                match best {
                    Some((_, best_score)) if score <= best_score => best,
                    _ => Some((z, score)),
                }
            })
            .map(|(z, _)| z);

        let target = defend_target.or(blockade_target);

        if let Some(target) = target {
            if target == zone {
                continue; // already there; naval combat resolves it this tick
            }
            let own_power = obs.own_zone_power(zone);
            let enemy_power = obs.enemy_zone_power(target);
            if own_power >= caution * (enemy_power + NAVY_ENGAGE_MARGIN) {
                for &unit_id in fleets {
                    actions.push(Action::MoveUnit { unit: unit_id, to: Station::Sea(target) });
                }
            }
            continue;
        }

        // Point 4: nothing to do here - head back toward the main home port.
        if let Some(home) = home {
            if zone != home {
                if let Some(next) = zone_path_next(obs.world, zone, home) {
                    for &unit_id in fleets {
                        actions.push(Action::MoveUnit { unit: unit_id, to: Station::Sea(next) });
                    }
                }
            }
        }
    }
}

/// Rough strategic value of a sea zone for blockade targeting: the highest
/// `Region::value()` among the ports it touches.
fn zone_value(obs: &Observation, zone: SeaZoneId) -> f32 {
    obs.world
        .sea_zone(zone)
        .coast
        .iter()
        .map(|&r| obs.world.region(r).value())
        .fold(0.0f32, f32::max)
}

/// Breadth-first next hop from `from` toward `to` over the sea-zone
/// adjacency graph — the zone-domain counterpart of
/// `Observation::path_next` (which is region/land-graph specific).
fn zone_path_next(world: &World, from: SeaZoneId, to: SeaZoneId) -> Option<SeaZoneId> {
    if from == to {
        return None;
    }
    let n = world.sea_zones.len();
    let mut visited = vec![false; n];
    let mut prev = vec![None; n];
    let mut queue = VecDeque::new();
    visited[from.index()] = true;
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        if current == to {
            break;
        }
        let mut neighbors: Vec<SeaZoneId> = world.sea_zone(current).adjacent.clone();
        neighbors.sort_by_key(|z| z.0);
        for next in neighbors {
            if !visited[next.index()] {
                visited[next.index()] = true;
                prev[next.index()] = Some(current);
                queue.push_back(next);
            }
        }
    }

    if !visited[to.index()] {
        return None;
    }
    let mut step = to;
    while let Some(p) = prev[step.index()] {
        if p == from {
            return Some(step);
        }
        step = p;
    }
    None
}

/// Build priorities (docs/phase2-spec.md Stage 2B):
/// 1. Repair an own, uncontested, sufficiently devastated region.
/// 2. Otherwise, add `Capacity` for whichever good is the production
///    chain's structural bottleneck, at a safe, high-infrastructure region.
/// 3. Otherwise, raise `Infrastructure` at a front region.
///
/// Gated behind `BUILD_STOCK_RESERVE_DAYS`: while Machinery/Steel stock is
/// below the war effort's own short-term reserve, the agent does not start
/// or continue directing new resources into construction (an already
/// in-progress project run by `construction::tick_construction` still
/// slows down instead of stalling - this gate only stops *new* orders).
fn build(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let machinery_days = f.stock[Good::Machinery.index()] / ARMS_INPUT_MACHINERY;
    let steel_days = f.stock[Good::Steel.index()] / ARMS_INPUT_STEEL;
    if machinery_days < BUILD_STOCK_RESERVE_DAYS || steel_days < BUILD_STOCK_RESERVE_DAYS {
        return;
    }

    if let Some(region) = repair_target(faction, obs) {
        actions.push(Action::Build { region, project: Project::Repair });
        return;
    }

    if let Some(good) = bottleneck_good(obs) {
        if let Some(region) = safest_high_infra_region(faction, obs) {
            actions.push(Action::Build { region, project: Project::Capacity(good) });
            return;
        }
    }

    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);
    let region = front.into_iter().find(|&r| {
        obs.world.region(r).construction.is_none() && !obs.world.has_enemy_units(r, faction)
    });
    if let Some(region) = region {
        actions.push(Action::Build { region, project: Project::Infrastructure });
    }
}

/// The most devastated own, uncontested region with no project already
/// running, if any is past `REPAIR_THRESHOLD`.
fn repair_target(faction: FactionId, obs: &Observation) -> Option<RegionId> {
    let mut candidates = obs.own_regions();
    candidates.sort_by_key(|r| r.0);
    candidates
        .into_iter()
        .filter(|&r| {
            let region = obs.world.region(r);
            region.devastation > REPAIR_THRESHOLD
                && region.construction.is_none()
                && !obs.world.has_enemy_units(r, faction)
        })
        .fold(None, |best: Option<(RegionId, f32)>, r| {
            let d = obs.world.region(r).devastation;
            match best {
                Some((_, best_d)) if d <= best_d => best,
                _ => Some((r, d)),
            }
        })
        .map(|(r, _)| r)
}

/// The good whose *national* effective capacity structurally can't fund
/// what downstream production needs from it - `Steel` first, since both
/// `Machinery` and `Munitions` (and `Arms`, via `Steel`) draw on it, then
/// `Machinery` for `Arms` specifically. `None` when nothing owned is
/// structurally starved this way.
fn bottleneck_good(obs: &Observation) -> Option<Good> {
    let mut steel_cap = 0.0f32;
    let mut machinery_cap = 0.0f32;
    let mut munitions_cap = 0.0f32;
    let mut arms_cap = 0.0f32;
    for r in obs.own_regions() {
        let region = obs.world.region(r);
        steel_cap += region.effective_capacity(Good::Steel);
        machinery_cap += region.effective_capacity(Good::Machinery);
        munitions_cap += region.effective_capacity(Good::Munitions);
        arms_cap += region.effective_capacity(Good::Arms);
    }

    let steel_needed = machinery_cap * MACHINERY_INPUT_STEEL
        + munitions_cap * MUNITIONS_INPUT_STEEL
        + arms_cap * ARMS_INPUT_STEEL;
    if steel_cap < steel_needed {
        return Some(Good::Steel);
    }

    let machinery_needed = arms_cap * ARMS_INPUT_MACHINERY;
    if machinery_cap < machinery_needed {
        return Some(Good::Machinery);
    }

    None
}

/// The safest place to expand capacity: an own, uncontested, non-front
/// region with no project running, preferring the highest infrastructure so
/// the new capacity is actually usable at good efficiency; falls back to
/// any safe own region without a project if every own region is on the front.
fn safest_high_infra_region(faction: FactionId, obs: &Observation) -> Option<RegionId> {
    let front: BTreeSet<RegionId> = obs.front_regions().into_iter().collect();
    let mut own = obs.own_regions();
    own.sort_by_key(|r| r.0);

    let interior_best = own
        .iter()
        .copied()
        .filter(|r| {
            !front.contains(r)
                && obs.world.region(*r).construction.is_none()
                && !obs.world.has_enemy_units(*r, faction)
        })
        .fold(None, |best: Option<(RegionId, f32)>, r| {
            let infra = obs.world.region(r).infrastructure;
            match best {
                Some((_, best_infra)) if infra <= best_infra => best,
                _ => Some((r, infra)),
            }
        })
        .map(|(r, _)| r);
    if interior_best.is_some() {
        return interior_best;
    }

    own.into_iter().find(|&r| {
        obs.world.region(r).construction.is_none() && !obs.world.has_enemy_units(r, faction)
    })
}

/// From every uncontested own region, picks the best adjacent target
/// (highest `value / (1 + enemy_power)`) and, if the garrison is strong
/// enough relative to it, sends units in - leaving one unit behind as a
/// garrison when attacking out of a front region with a defended target.
/// Units already moving toward that same target fill part of that quota
/// without a fresh order (see `is_en_route_here` below).
fn offensive(faction: FactionId, caution: f32, obs: &Observation, actions: &mut Vec<Action>) {
    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);

    for region in safe_own_regions(faction, obs) {
        let mut targets: Vec<RegionId> = obs
            .world
            .neighbors(region)
            .filter(|&n| obs.world.region(n).owner != faction)
            .collect();
        targets.sort_by_key(|r| r.0);
        if targets.is_empty() {
            continue;
        }

        let best_target = targets
            .into_iter()
            .fold(None, |best: Option<(RegionId, f32)>, t| {
                let value = obs.world.region(t).value();
                let score = value / (1.0 + obs.enemy_power(t));
                match best {
                    Some((_, best_score)) if score <= best_score => best,
                    _ => Some((t, score)),
                }
            })
            .map(|(t, _)| t);
        let Some(target) = best_target else { continue };

        let enemy_power = obs.enemy_power(target);
        let terrain_bonus = obs.world.region(target).terrain.defense_bonus();
        let own_power = obs.own_power(region);
        if own_power < caution * (enemy_power * terrain_bonus + 0.6) {
            continue;
        }

        let is_front = front.binary_search(&region).is_ok();
        let target_undefended = enemy_power <= 0.0;
        let mut present: Vec<UnitId> = obs
            .world
            .units_in(region)
            .filter(|u| u.owner == faction)
            .map(|u| u.id)
            .collect();
        present.sort_by_key(|u| u.0);

        // Garrison sizing counts every unit physically here, whether idle
        // or already departing.
        let send_capacity = if is_front && !target_undefended {
            present.len().saturating_sub(1)
        } else {
            present.len()
        };

        let is_en_route_here = |u: &UnitId| {
            matches!(obs.world.unit(*u).movement, Some(mv) if mv.to == Station::Region(target))
        };

        // A unit already under way toward `target` occupies one of the
        // capacity slots without needing a fresh order - re-issuing
        // `MoveUnit` for it would reset `Movement::progress` to zero, and
        // since the agent re-plans every 4 days while a hostile crossing
        // can take longer than that, the attack would never land. A unit
        // moving toward somewhere else still counts as idle here and can be
        // redirected - that's a deliberate change of target, not an
        // accident of re-planning.
        let already_en_route = present.iter().filter(|u| is_en_route_here(u)).count();
        let new_orders = send_capacity.saturating_sub(already_en_route);

        let idle: Vec<UnitId> = present.into_iter().filter(|u| !is_en_route_here(u)).collect();
        for &unit_id in idle.iter().take(new_orders) {
            actions.push(Action::MoveUnit { unit: unit_id, to: Station::Region(target) });
        }
    }
}

/// Walks units that aren't already at (or ordered toward) the front one
/// step closer, via `Observation::path_next`, toward whichever front region
/// is nearest by raw map distance.
fn advance_interior(
    faction: FactionId,
    obs: &Observation,
    already_moved: &BTreeSet<UnitId>,
    actions: &mut Vec<Action>,
) {
    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);
    if front.is_empty() {
        return;
    }

    let mut units = obs.own_units();
    units.sort_by_key(|u| u.0);

    for unit_id in units {
        if already_moved.contains(&unit_id) {
            continue;
        }
        let unit = obs.world.unit(unit_id);
        if unit.movement.is_some() {
            continue;
        }
        // Fleets are steered by `naval_ops`, not this land-only walk toward
        // the region front.
        let Some(location) = unit.station.region() else {
            continue;
        };
        if front.binary_search(&location).is_ok() {
            continue;
        }
        if obs.world.has_enemy_units(location, faction) {
            continue;
        }

        let distances = map_distances(obs.world, location);
        let nearest_front = front
            .iter()
            .copied()
            .fold(None, |best: Option<(RegionId, u32)>, f| {
                let d = distances[f.index()];
                match best {
                    Some((_, best_d)) if d >= best_d => best,
                    _ => Some((f, d)),
                }
            })
            .map(|(f, _)| f);
        let Some(nearest_front) = nearest_front else { continue };

        if let Some(next) = obs.path_next(location, nearest_front) {
            actions.push(Action::MoveUnit { unit: unit_id, to: Station::Region(next) });
        }
    }
}

#[cfg(test)]
mod tests;

/// Breadth-first hop count from `from` to every region, over the full map
/// graph (not restricted to friendly territory - this is just "how far
/// away is the front", not a route the unit is forced to take).
fn map_distances(world: &World, from: RegionId) -> Vec<u32> {
    let mut dist = vec![u32::MAX; world.regions.len()];
    dist[from.index()] = 0;
    let mut queue = VecDeque::new();
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        let mut neighbors: Vec<RegionId> = world.neighbors(current).collect();
        neighbors.sort_by_key(|r| r.0);
        for next in neighbors {
            if dist[next.index()] == u32::MAX {
                dist[next.index()] = dist[current.index()] + 1;
                queue.push_back(next);
            }
        }
    }

    dist
}
