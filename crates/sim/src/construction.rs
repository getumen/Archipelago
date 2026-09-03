//! Stage 2B (docs/phase2-spec.md "Stage 2B — インフラと建設・戦災"): the
//! single project a region can have in progress at a time, its daily
//! Machinery/Steel-funded advancement, its completion effects, and the
//! passive recovery that pulls `Region::devastation` back down between
//! battles. Combat and capture *raise* `devastation`; that half lives next
//! to the systems that cause it (`military::tick_combat`,
//! `military::tick_occupation`).

use crate::balance::{
    CAPACITY_STEP, CAPITAL_FLIGHT_CONSTRUCTION_MULT, CONSTRUCTION_MACHINERY_PER_POINT,
    CONSTRUCTION_RATE, CONSTRUCTION_REQUIRED_CAPACITY, CONSTRUCTION_REQUIRED_INFRASTRUCTURE,
    CONSTRUCTION_REQUIRED_PORT, CONSTRUCTION_REQUIRED_REPAIR, CONSTRUCTION_STEEL_PER_POINT,
    DEVASTATION_RECOVERY, FOCUS_DEFENSIVE_DEVASTATION_RECOVERY_MULT,
    FOCUS_TECHNOCRACY_CONSTRUCTION_RATE_MULT, INFRA_STEP, PORT_STEP, REPAIR_STEP,
};
use crate::focus::{self, NationalFocus};
use crate::good::Good;
use crate::world::{Region, World};

/// A region-improvement project (docs/phase2-spec.md Stage 2B). Only one
/// can be in progress per region (`Region::construction`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Project {
    Infrastructure,
    Port,
    Capacity(Good),
    Repair,
}

/// In-progress work on a region's `Project`: `invested` building points
/// funded so far, out of `required` to complete it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Construction {
    pub project: Project,
    pub invested: f32,
    pub required: f32,
}

/// Building points required to complete `project` — the `required` a fresh
/// `Construction` is created with (`action::apply_build`).
pub fn required_points(project: Project) -> f32 {
    match project {
        Project::Infrastructure => CONSTRUCTION_REQUIRED_INFRASTRUCTURE,
        Project::Port => CONSTRUCTION_REQUIRED_PORT,
        Project::Capacity(_) => CONSTRUCTION_REQUIRED_CAPACITY,
        Project::Repair => CONSTRUCTION_REQUIRED_REPAIR,
    }
}

/// Applies a completed project's one-time effect (docs/phase2-spec.md Stage
/// 2B's completion-effect table).
fn apply_completion(region: &mut Region, project: Project) {
    match project {
        Project::Infrastructure => region.infrastructure = (region.infrastructure + INFRA_STEP).min(1.0),
        Project::Port => region.port += PORT_STEP,
        Project::Capacity(good) => region.capacity[good.index()] += CAPACITY_STEP,
        Project::Repair => region.devastation = (region.devastation - REPAIR_STEP).max(0.0),
    }
}

/// Advances every region's in-progress construction by up to
/// `CONSTRUCTION_RATE` building points, paid for out of the owning
/// faction's national Machinery/Steel stock at
/// `CONSTRUCTION_MACHINERY_PER_POINT` / `CONSTRUCTION_STEEL_PER_POINT`. When
/// the stock can't fund the full rate, progress is scaled down to whatever
/// fraction it can fund instead of stalling outright. Regions are visited in
/// fixed index order so two projects under the same faction draw on the
/// shared stock deterministically.
pub fn tick_construction(world: &mut World) {
    let n = world.regions.len();
    for i in 0..n {
        let Some(mut constr) = world.regions[i].construction else {
            continue;
        };
        let owner = world.regions[i].owner;

        // Stage 3A (docs/phase3-spec.md "資本逃避": "建設速度と Machinery 生
        // 産に係数"): a faction under active capital flight builds slower -
        // read before the mutable borrow below. Stage 3C
        // `NationalFocus::Technocracy` ("建設速度＋") stacks multiplicatively
        // on top of that, the same way every other independent rate
        // multiplier here does.
        let mut rate = CONSTRUCTION_RATE;
        if world.faction(owner).capital_flight_active {
            rate *= CAPITAL_FLIGHT_CONSTRUCTION_MULT;
        }
        if focus::active(world.faction(owner)) == Some(NationalFocus::Technocracy) {
            rate *= FOCUS_TECHNOCRACY_CONSTRUCTION_RATE_MULT;
        }

        // Cap the attempted progress at what's actually left to invest, so a
        // completing tick doesn't buy (and pay for) more than the project
        // needs — its total cost must equal `required *
        // CONSTRUCTION_*_PER_POINT` exactly, regardless of how progress was
        // spread across ticks.
        let attempted = rate.min(constr.required - constr.invested);
        let machinery_cost = attempted * CONSTRUCTION_MACHINERY_PER_POINT;
        let steel_cost = attempted * CONSTRUCTION_STEEL_PER_POINT;

        let faction = world.faction_mut(owner);
        let machinery_ratio = if machinery_cost > 0.0 {
            (faction.stock[Good::Machinery.index()] / machinery_cost).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let steel_ratio = if steel_cost > 0.0 {
            (faction.stock[Good::Steel.index()] / steel_cost).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let funded_ratio = machinery_ratio.min(steel_ratio);

        faction.stock[Good::Machinery.index()] =
            (faction.stock[Good::Machinery.index()] - machinery_cost * funded_ratio).max(0.0);
        faction.stock[Good::Steel.index()] =
            (faction.stock[Good::Steel.index()] - steel_cost * funded_ratio).max(0.0);

        constr.invested += attempted * funded_ratio;
        if constr.invested >= constr.required {
            apply_completion(&mut world.regions[i], constr.project);
            world.regions[i].construction = None;
        } else {
            world.regions[i].construction = Some(constr);
        }
    }
}

/// Passively recovers `devastation` toward zero (docs/phase2-spec.md Stage
/// 2B's recovery formula), scaled down by the region's own unrest and its
/// owning faction's stability so an unruly occupied region barely rebuilds
/// on its own — `Project::Repair` is the explicit, faster alternative.
pub fn tick_devastation_recovery(world: &mut World) {
    for i in 0..world.regions.len() {
        if world.regions[i].devastation <= 0.0 {
            continue;
        }
        let owner = world.regions[i].owner;
        let stability = world.faction(owner).stability;
        // Stage 3C `NationalFocus::DefensivePosture` (docs/phase3-spec.md:
        // "戦災の回復速度＋"): applies to every one of this faction's regions
        // unconditionally (not just its own `core` soil) - a defense-minded
        // economy rebuilds faster everywhere, not only at home.
        let focus_mult = if focus::active(world.faction(owner)) == Some(NationalFocus::DefensivePosture)
        {
            FOCUS_DEFENSIVE_DEVASTATION_RECOVERY_MULT
        } else {
            1.0
        };
        let region = &mut world.regions[i];
        let recovery = DEVASTATION_RECOVERY
            * focus_mult
            * (1.0 - region.unrest / 100.0).clamp(0.0, 1.0)
            * (0.5 + 0.5 * stability / 100.0).clamp(0.0, 1.0);
        region.devastation = (region.devastation - recovery).max(0.0);
    }
}
