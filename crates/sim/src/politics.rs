//! Unrest, domestic political `Group` support, `stability` (their
//! influence-weighted average), war support, the six Stage 3A political
//! events, and separatism — the political feedback loop that eventually
//! throttles a faction's economy (see economy.rs) and, per design.md §2,
//! can topple a government that is winning the war outright.
//!
//! docs/phase3-spec.md "Stage 3A — 国内政治勢力" is the spec this module
//! implements. Every situational contribution to a group's target support is
//! a bounded term (never a raw accumulation onto `support` itself), and
//! every event is designed to be recoverable — see the doc comments on the
//! relevant `balance.rs` constants for how each one specifically avoids
//! pinning at a boundary.

use crate::balance::{
    CAPITAL_FLIGHT_THRESHOLD, CIVILIAN_RATION_DEFAULT, CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN,
    GROUP_ADAPT_RATE, GROUP_ARMS_LEAN_BUSINESS_BONUS, GROUP_ARMS_LEAN_CITIZENS_PENALTY,
    GROUP_ARMS_LEAN_MILITARY_BONUS, GROUP_ARMS_STOCK_MARGIN, GROUP_ARMS_STOCK_MILITARY_BONUS,
    GROUP_CASUALTY_CITIZENS_PENALTY, GROUP_CASUALTY_GOVERNMENT_PENALTY,
    GROUP_CASUALTY_MILITARY_PENALTY, GROUP_CASUALTY_NORM, GROUP_CONSCRIPTION_CITIZENS_PENALTY,
    GROUP_CONSCRIPTION_LABOR_PENALTY, GROUP_CONSCRIPTION_MILITARY_BONUS,
    GROUP_DEVASTATION_BUSINESS_PENALTY, GROUP_DEVASTATION_LOCALGOV_PENALTY,
    GROUP_MACHINERY_GOOD_BUSINESS_BONUS, GROUP_RATION_CITIZENS_PENALTY, GROUP_RATION_LABOR_PENALTY,
    GROUP_RATION_MILITARY_BONUS, GROUP_SHORTAGE_CITIZENS_PENALTY, GROUP_SHORTAGE_GOVERNMENT_PENALTY,
    GROUP_SHORTAGE_LABOR_PENALTY, GROUP_SUPPORT_BASELINE, GROUP_TERRITORY_DELTA_CAP,
    GROUP_TERRITORY_GAIN_GOVERNMENT_BONUS, GROUP_TERRITORY_GAIN_MILITARY_BONUS,
    GROUP_TERRITORY_LOSS_GOVERNMENT_PENALTY, GROUP_TERRITORY_LOSS_MILITARY_PENALTY,
    GROUP_UNREST_GOVERNMENT_PENALTY, GROUP_UNREST_LOCALGOV_PENALTY, MUTINY_THRESHOLD,
    OCCUPIED_UNREST_FLOOR, PROTEST_THRESHOLD, PROTEST_UNREST_BONUS, REGIME_CHANGE_DAYS,
    REGIME_CHANGE_THRESHOLD, SEPARATISM_DECAY, SEPARATISM_GARRISON_MAX_SUPPRESSION,
    SEPARATISM_GARRISON_POP_NORM, SEPARATISM_RATE, SEPARATISM_THRESHOLD,
    SEPARATISM_UNREST_GARRISON_MULT, STRIKE_DAYS, STRIKE_THRESHOLD, UNIT_EQUIPMENT,
    UNREST_ADAPT_RATE, UNREST_SHORTAGE_PRESSURE, UNREST_SUPPLY_PRESSURE,
    WAR_SUPPORT_CASUALTY_MULT, WAR_SUPPORT_DRIFT,
};
use crate::event::Event;
use crate::good::Good;
use crate::group::{Group, GROUP_COUNT};
use crate::ids::FactionId;
use crate::scenario::{
    FACTION_CONSCRIPTION, FACTION_IMPORT_PLAN, FACTION_INDUSTRY_PRIORITY, FACTION_LOGISTICS_PRIORITY,
};
use crate::world::{OccupationKind, Region, World};

/// `day_casualties`/`region_delta` are indexed like `World::factions`:
/// manpower lost this tick, and the net change in owned region count this
/// tick (from `military::tick_occupation`, positive on net gain).
pub fn tick_politics(
    world: &mut World,
    day_casualties: &[f32],
    region_delta: &[i32],
    events: &mut Vec<Event>,
) {
    let n = world.factions.len();

    let shortage: Vec<f32> = world.factions.iter().map(|f| f.shortage).collect();
    let supply_ratio: Vec<f32> = world.factions.iter().map(|f| f.supply_ratio).collect();
    // Stage 3A Event::Protest (docs/phase3-spec.md "デモ"): read *yesterday's*
    // Citizens support (this tick hasn't updated `group_support` yet) to
    // decide whether each faction's owned regions get an extra unrest-target
    // bump today — the same one-tick lag every other Stage 3A event effect
    // has on the systems that consume it.
    let protesting: Vec<bool> = world
        .factions
        .iter()
        .map(|f| f.group_support[Group::Citizens.index()] < PROTEST_THRESHOLD)
        .collect();

    for region in world.regions.iter_mut() {
        let floor = if region.core == region.owner {
            0.0
        } else {
            OCCUPIED_UNREST_FLOOR
        };
        let f = region.owner.index();
        let mut pressure = UNREST_SHORTAGE_PRESSURE * shortage[f]
            + UNREST_SUPPLY_PRESSURE * (1.0 - supply_ratio[f].clamp(0.0, 1.0));
        if protesting[f] {
            pressure += PROTEST_UNREST_BONUS;
        }
        let target = (floor + pressure).min(100.0);
        region.unrest += (target - region.unrest) * UNREST_ADAPT_RATE;
        region.unrest = region.unrest.clamp(0.0, 100.0);
    }

    let mut avg_unrest = vec![0.0f32; n];
    let mut avg_devastation = vec![0.0f32; n];
    let mut region_count = vec![0u32; n];
    for region in &world.regions {
        let f = region.owner.index();
        avg_unrest[f] += region.unrest;
        avg_devastation[f] += region.devastation;
        region_count[f] += 1;
    }
    for f in 0..n {
        if region_count[f] > 0 {
            avg_unrest[f] /= region_count[f] as f32;
            avg_devastation[f] /= region_count[f] as f32;
        }
    }

    let arms_stock: Vec<f32> = world.factions.iter().map(|f| f.stock[Good::Arms.index()]).collect();
    let machinery_ratio: Vec<f32> = world.factions.iter().map(|f| f.machinery_output_ratio).collect();

    for f_idx in 0..n {
        if !world.factions[f_idx].alive {
            continue;
        }

        // ---- Group support: target-approach model (docs/phase3-spec.md
        // "支持の更新"). Every contribution below is a bounded term (its own
        // `0..1` driving factor times a named `balance.rs` magnitude), added
        // to a 50-baseline target that `group_support` closes the gap to at
        // `GROUP_ADAPT_RATE` per day — never accumulated onto `support`
        // directly, so no group can pin at 0 or 100 without a path back.
        let conscription = world.factions[f_idx].conscription;
        let ration = world.factions[f_idx].civilian_ration;
        let ration_deficit =
            ((CIVILIAN_RATION_MAX - ration) / (CIVILIAN_RATION_MAX - CIVILIAN_RATION_MIN))
                .clamp(0.0, 1.0);
        let arms_lean = (world.factions[f_idx].industry_priority[Good::Munitions.index()]
            - world.factions[f_idx].industry_priority[Good::Machinery.index()])
            .clamp(0.0, 1.0);
        let shortage_f = world.factions[f_idx].shortage.clamp(0.0, 1.0);
        let unrest_f = (avg_unrest[f_idx] / 100.0).clamp(0.0, 1.0);
        let devastation_f = avg_devastation[f_idx].clamp(0.0, 1.0);
        let casualties_f = (day_casualties[f_idx] / GROUP_CASUALTY_NORM).clamp(0.0, 1.0);
        let gained_f = (region_delta[f_idx].max(0) as f32 / GROUP_TERRITORY_DELTA_CAP).clamp(0.0, 1.0);
        let lost_f =
            ((-region_delta[f_idx]).max(0) as f32 / GROUP_TERRITORY_DELTA_CAP).clamp(0.0, 1.0);
        let arms_stock_f =
            (arms_stock[f_idx] / (UNIT_EQUIPMENT * GROUP_ARMS_STOCK_MARGIN)).clamp(0.0, 1.0);
        let machinery_f = machinery_ratio[f_idx].clamp(0.0, 1.0);

        let mut target = [GROUP_SUPPORT_BASELINE; GROUP_COUNT];
        target[Group::Military.index()] += GROUP_CONSCRIPTION_MILITARY_BONUS * conscription;
        target[Group::Labor.index()] -= GROUP_CONSCRIPTION_LABOR_PENALTY * conscription;
        target[Group::Citizens.index()] -= GROUP_CONSCRIPTION_CITIZENS_PENALTY * conscription;

        target[Group::Military.index()] += GROUP_RATION_MILITARY_BONUS * ration_deficit;
        target[Group::Citizens.index()] -= GROUP_RATION_CITIZENS_PENALTY * ration_deficit;
        target[Group::Labor.index()] -= GROUP_RATION_LABOR_PENALTY * ration_deficit;

        target[Group::Military.index()] += GROUP_ARMS_LEAN_MILITARY_BONUS * arms_lean;
        target[Group::Business.index()] += GROUP_ARMS_LEAN_BUSINESS_BONUS * arms_lean;
        target[Group::Citizens.index()] -= GROUP_ARMS_LEAN_CITIZENS_PENALTY * arms_lean;

        target[Group::Citizens.index()] -= GROUP_SHORTAGE_CITIZENS_PENALTY * shortage_f;
        target[Group::Labor.index()] -= GROUP_SHORTAGE_LABOR_PENALTY * shortage_f;
        target[Group::Government.index()] -= GROUP_SHORTAGE_GOVERNMENT_PENALTY * shortage_f;

        target[Group::LocalGovernment.index()] -= GROUP_UNREST_LOCALGOV_PENALTY * unrest_f;
        target[Group::Government.index()] -= GROUP_UNREST_GOVERNMENT_PENALTY * unrest_f;

        target[Group::LocalGovernment.index()] -= GROUP_DEVASTATION_LOCALGOV_PENALTY * devastation_f;
        target[Group::Business.index()] -= GROUP_DEVASTATION_BUSINESS_PENALTY * devastation_f;

        target[Group::Military.index()] -= GROUP_CASUALTY_MILITARY_PENALTY * casualties_f;
        target[Group::Citizens.index()] -= GROUP_CASUALTY_CITIZENS_PENALTY * casualties_f;
        target[Group::Government.index()] -= GROUP_CASUALTY_GOVERNMENT_PENALTY * casualties_f;

        target[Group::Military.index()] +=
            GROUP_TERRITORY_GAIN_MILITARY_BONUS * gained_f - GROUP_TERRITORY_LOSS_MILITARY_PENALTY * lost_f;
        target[Group::Government.index()] += GROUP_TERRITORY_GAIN_GOVERNMENT_BONUS * gained_f
            - GROUP_TERRITORY_LOSS_GOVERNMENT_PENALTY * lost_f;

        target[Group::Military.index()] += GROUP_ARMS_STOCK_MILITARY_BONUS * arms_stock_f;
        target[Group::Business.index()] += GROUP_MACHINERY_GOOD_BUSINESS_BONUS * machinery_f;

        let faction = &mut world.factions[f_idx];
        for g in 0..GROUP_COUNT {
            let t = target[g].clamp(0.0, 100.0);
            faction.group_support[g] += (t - faction.group_support[g]) * GROUP_ADAPT_RATE;
            faction.group_support[g] = faction.group_support[g].clamp(0.0, 100.0);
        }

        // ---- Stability (docs/phase3-spec.md "安定度の再定義"): the
        // influence-weighted average of group support, replacing Phase 1's
        // independent target-approach variable. Political policy now feeds
        // straight into `economy::tick_economy`'s `stability_mult`, closing
        // the loop design.md §2 asks for.
        let mut stability = 0.0f32;
        for g in 0..GROUP_COUNT {
            stability += faction.group_influence[g] * faction.group_support[g];
        }
        faction.stability = stability.clamp(0.0, 100.0);

        // ---- War support (unchanged from Phase 1/2). ----
        faction.war_support -= day_casualties[f_idx] * WAR_SUPPORT_CASUALTY_MULT;
        if faction.war_support > 50.0 {
            faction.war_support = (faction.war_support - WAR_SUPPORT_DRIFT).max(50.0);
        } else if faction.war_support < 50.0 {
            faction.war_support = (faction.war_support + WAR_SUPPORT_DRIFT).min(50.0);
        }
        faction.war_support = faction.war_support.clamp(0.0, 100.0);
    }

    apply_political_events(world, events);
    tick_separatism(world, events);
}

/// Triggers/advances the five non-separatism Stage 3A political events
/// (docs/phase3-spec.md "政治イベント"). Strike and RegimeChange are
/// fixed-duration: once triggered they run their own course via a countdown
/// (`strike_days`/`regime_change_days`) and can only trigger again after it
/// reaches zero. Protest, Mutiny and CapitalFlight are live conditions with
/// no timer, re-evaluated fresh every tick from `group_support` — the
/// stored `*_active` flags exist only so other systems can read "is this in
/// effect" without depending on `crate::group`, and so an `Event` is logged
/// only on the rising edge rather than every tick the condition holds.
fn apply_political_events(world: &mut World, events: &mut Vec<Event>) {
    for f_idx in 0..world.factions.len() {
        if !world.factions[f_idx].alive {
            continue;
        }
        let faction_id = FactionId(f_idx as u32);
        let support = world.factions[f_idx].group_support;

        if world.factions[f_idx].strike_days > 0 {
            world.factions[f_idx].strike_days -= 1;
        } else if support[Group::Labor.index()] < STRIKE_THRESHOLD {
            world.factions[f_idx].strike_days = STRIKE_DAYS;
            events.push(Event::Strike { faction: faction_id });
        }

        let protest_now = support[Group::Citizens.index()] < PROTEST_THRESHOLD;
        if protest_now && !world.factions[f_idx].protest_active {
            events.push(Event::Protest { faction: faction_id });
        }
        world.factions[f_idx].protest_active = protest_now;

        let mutiny_now = support[Group::Military.index()] < MUTINY_THRESHOLD;
        if mutiny_now && !world.factions[f_idx].mutiny_active {
            events.push(Event::Mutiny { faction: faction_id });
        }
        world.factions[f_idx].mutiny_active = mutiny_now;

        let capital_flight_now = support[Group::Business.index()] < CAPITAL_FLIGHT_THRESHOLD;
        if capital_flight_now && !world.factions[f_idx].capital_flight_active {
            events.push(Event::CapitalFlight { faction: faction_id });
        }
        world.factions[f_idx].capital_flight_active = capital_flight_now;

        if world.factions[f_idx].regime_change_days > 0 {
            world.factions[f_idx].regime_change_days -= 1;
        } else if world.factions[f_idx].stability < REGIME_CHANGE_THRESHOLD {
            // docs/phase3-spec.md "政権交代の扱い": policies reset to their
            // scenario defaults, war_support resets to 50, every group's
            // support resets to the 50 baseline (which, since
            // `group_influence` always sums to 1.0, makes the freshly
            // recomputed `stability` land exactly on 50 too) - but territory,
            // units and stock are untouched. This is a penalty for the
            // policy stance the faction built up, not a board-destroying one.
            let faction = &mut world.factions[f_idx];
            faction.conscription = FACTION_CONSCRIPTION;
            faction.civilian_ration = CIVILIAN_RATION_DEFAULT;
            faction.industry_priority = FACTION_INDUSTRY_PRIORITY;
            faction.logistics_priority = FACTION_LOGISTICS_PRIORITY;
            faction.import_plan = FACTION_IMPORT_PLAN;
            faction.war_support = 50.0;
            faction.group_support = [GROUP_SUPPORT_BASELINE; GROUP_COUNT];
            faction.stability = GROUP_SUPPORT_BASELINE;
            faction.regime_change_days = REGIME_CHANGE_DAYS;
            // External code review fix (Stage 3A, Fix 1): `protest_active`/
            // `mutiny_active`/`capital_flight_active` were computed earlier
            // this same tick from *pre-reset* support and would otherwise
            // survive the reset unchanged, keeping their (now stale) live
            // condition in effect for one extra day — `economy`,
            // `construction` and `military::tick_recovery` all run before
            // `tick_politics` gets to recompute them fresh tomorrow. Every
            // one of these thresholds sits below `GROUP_SUPPORT_BASELINE`
            // (50), so a uniform reset to baseline always clears all three
            // outright rather than needing them individually recomputed.
            faction.protest_active = false;
            faction.mutiny_active = false;
            faction.capital_flight_active = false;
            events.push(Event::RegimeChange { faction: faction_id });
        }
    }
}

/// Stage 3A separatism (docs/phase3-spec.md "地方独立運動"): in any owned
/// region where `core != owner`, once the owner's LocalGovernment support
/// falls below `SEPARATISM_THRESHOLD`, that region's `occupation` meter
/// starts advancing toward its `core` faction — a peaceful, political
/// reversion, expressed through the same `occupation`/`occupier` fields
/// `military::tick_occupation` uses for a real invasion, but owned by this
/// function instead while `Region::occupation_kind == Some(Separatist)` (see
/// `OccupationKind`'s doc for how the two avoid corrupting each other's
/// progress).
///
/// A *foreign* force present (any unit not owned by the current `owner` —
/// a real invasion, whether by `core`'s own military or a third faction)
/// hands the region's meter to `military::tick_occupation` exclusively:
/// that's a military conquest in progress, which political drift must not
/// interfere with or be overtaken by.
///
/// The owner's own garrison, if any, no longer vetoes the drift outright
/// (External code review fix, Fix 2/3 — docs/phase3-spec.md §0's absorbing-
/// state rule: a garrison that can freeze this meter forever for free is
/// exactly the kind of fixed-priority veto §0 warns against). Instead it
/// scales `SEPARATISM_RATE` down by the garrison's strength relative to the
/// region's population and unrest — see `balance::SEPARATISM_GARRISON_*`.
/// With no garrison at all the scaling factor is 1.0, so an empty region
/// still drifts at the full, unsuppressed rate exactly as before.
///
/// Recoverable in both directions: it advances while the condition holds and
/// decays back to zero the moment it doesn't, the same target-style
/// guarantee every other Stage 3A event gets.
pub(crate) fn tick_separatism(world: &mut World, events: &mut Vec<Event>) {
    for i in 0..world.regions.len() {
        let region_id = world.regions[i].id;
        let (core, owner) = {
            let r = &world.regions[i];
            (r.core, r.owner)
        };
        if core == owner {
            continue;
        }

        let mut foreign_present = false;
        let mut garrison_power = 0.0f32;
        for unit in world.units_in(region_id) {
            if unit.owner == owner {
                garrison_power += unit.combat_power();
            } else {
                foreign_present = true;
            }
        }
        if foreign_present {
            // A real invasion (by `core`'s own military or a third faction)
            // owns this region's meter until it's resolved — see
            // `military::tick_occupation`'s handling of `OccupationKind`.
            continue;
        }
        if world.regions[i].occupation_kind == Some(OccupationKind::Military) {
            // A previous real invasion's progress, still decaying toward
            // zero now that the invader has left, owns this region's meter
            // until it fully clears — separatism must not overwrite it.
            continue;
        }

        let condition_met = world.faction(core).alive
            && world.faction(owner).group_support[Group::LocalGovernment.index()]
                < SEPARATISM_THRESHOLD;

        let region = &mut world.regions[i];
        if condition_met {
            let rate = separatist_rate(region, garrison_power);
            region.occupier = Some(core);
            region.occupation_kind = Some(OccupationKind::Separatist);
            region.occupation = (region.occupation + rate).min(100.0);
            if region.occupation >= 100.0 {
                region.owner = core;
                region.occupation = 0.0;
                region.occupier = None;
                region.occupation_kind = None;
                events.push(Event::Separatism { region: region_id, from: owner, to: core });
            }
        } else if region.occupier == Some(core) {
            region.occupation = (region.occupation - SEPARATISM_DECAY).max(0.0);
            if region.occupation == 0.0 {
                region.occupier = None;
                region.occupation_kind = None;
            }
        }
    }
}

/// `balance::SEPARATISM_GARRISON_*`'s scaling formula: how fast separatism
/// advances today, given `garrison_power` (the owner's own units'
/// `combat_power()` summed in the region, `0.0` when none are present).
/// Always in `(SEPARATISM_RATE * (1 - SEPARATISM_GARRISON_MAX_SUPPRESSION),
/// SEPARATISM_RATE]` — never fully vetoed, and exactly `SEPARATISM_RATE` at
/// `garrison_power == 0.0` (the fully-unsuppressed case every prior test
/// already exercises).
fn separatist_rate(region: &Region, garrison_power: f32) -> f32 {
    let population = region.population.max(0.01);
    let unrest_factor = (region.unrest / 100.0).clamp(0.0, 1.0);
    let effective_pop = population * (1.0 + unrest_factor * SEPARATISM_UNREST_GARRISON_MULT);
    let suppression = (garrison_power / (effective_pop * SEPARATISM_GARRISON_POP_NORM))
        .clamp(0.0, SEPARATISM_GARRISON_MAX_SUPPRESSION);
    SEPARATISM_RATE * (1.0 - suppression)
}
