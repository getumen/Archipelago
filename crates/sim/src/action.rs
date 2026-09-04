//! Player/agent-facing commands and the validation that turns them into
//! world mutations. Invalid actions are rejected, never panicked on.

use crate::balance::{
    CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN, FOCUS_MARITIME_FLEET_COST_MULT, FOCUS_SWITCH_DAYS,
    IMPORT_PLAN_RATE_MAX, NL_PROPOSAL_TEXT_MAX_CHARS, UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG,
    UNIT_START_ORG_RATIO,
};
use crate::construction::{required_points, Construction, Project};
use crate::diplomacy::{self, Stance, Treaty, TreatyTerm};
use crate::focus::{self, NationalFocus};
use crate::good::Good;
use crate::ids::{FactionId, RegionId, UnitId};
use crate::logistics;
use crate::military::{fleet_move_required, move_required, Movement, Unit};
use crate::naval;
use crate::world::{Domain, Station, World};

/// Stage 4B (docs/phase4-spec.md "Stage 4B"): `RespondToNaturalLanguageProposal`
/// carries an owned `Vec<TreatyTerm>` and `ProposeInNaturalLanguage` an owned
/// `String`, so `Action` can no longer derive `Copy` - every caller that
/// used to copy an `Action` implicitly (`sim::Simulation::apply`'s old `for
/// &act in actions`) now clones it explicitly instead.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// A land unit's `to` must be `Station::Region`; a fleet's must be
    /// `Station::Sea` — `apply_move` validates the destination matches the
    /// moving unit's own domain and rejects it otherwise
    /// (`ActionError::NotAdjacent`).
    MoveUnit { unit: UnitId, to: Station },
    HoldUnit { unit: UnitId },
    /// Stands a unit down, reversing `RecruitUnit`: the unit is removed
    /// from play and its current manpower and equipment return to the
    /// faction's pools rather than vanishing, mirroring how real
    /// demobilisation returns people to the workforce and matériel to the
    /// depot. See `apply_disband`'s doc for exactly what is refunded, and
    /// why standing down is refused while the unit is under enemy contact.
    DisbandUnit { unit: UnitId },
    /// Stage 2D (docs/phase2-spec.md "艦隊"): `domain` picks land or sea.
    /// A fleet can only be built in an owned, uncontested region that has a
    /// port (`region.port > 0.0`); it launches into that port's lowest-id
    /// facing sea zone (`naval::home_zone`).
    RecruitUnit { region: RegionId, domain: Domain },
    ReinforceUnit { unit: UnitId },
    SetConscription(f32),
    SetIndustryPriority { good: Good, weight: f32 },
    SetCivilianRation(f32),
    Build { region: RegionId, project: Project },
    CancelBuild { region: RegionId },
    /// Stage 2C sea imports (docs/phase2-spec.md "1. 海上輸入"): set the
    /// desired daily import rate for `good`. Only `Food` and `Energy` are
    /// importable - any other good is rejected with `ActionError::InvalidValue`.
    SetImportPlan { good: Good, rate: f32 },
    /// Stage 2C per-commodity delivery (docs/phase2-spec.md "3. 品目別の
    /// 到達率"): set the priority weight `logistics::distribute_supply` uses
    /// to split contended regional throughput between Munitions and Arms
    /// delivery for `good`.
    SetLogisticsPriority { good: Good, weight: f32 },
    /// Stage 3B (docs/phase3-spec.md "条約"): queues a one-tick pending
    /// proposal, visible to `to` via `Observation`/`Diplomacy::pending`.
    /// Replaces any existing outgoing proposal from this faction to `to`
    /// rather than stacking a second one.
    ProposeTreaty { to: FactionId, treaty: Treaty },
    /// Resolves a pending proposal *from* `from` *to* this faction into an
    /// active treaty.
    AcceptTreaty { from: FactionId, treaty: Treaty },
    /// Turns down a pending proposal *from* `from` *to* this faction.
    RejectTreaty { from: FactionId, treaty: Treaty },
    /// Ends an active `Stance::Ceasefire` with `to` immediately
    /// (docs/phase3-spec.md: "いつでも DeclareWar で破棄できる"). Rejected
    /// against any other current stance - breaking `NonAggression` or
    /// `Alliance` goes through `BreakTreaty` instead (`NonAggression` carries
    /// a notice period; `Alliance` falls back to `Ceasefire`, not war).
    DeclareWar { to: FactionId },
    /// Ends an active treaty with `with` - see `diplomacy::break_treaty` for
    /// what happens per treaty kind. Rejected for `Treaty::Ceasefire`
    /// (use `DeclareWar`).
    BreakTreaty { with: FactionId, treaty: Treaty },
    /// Stage 3C (docs/phase3-spec.md "Stage 3C — 国家方針"): commits the
    /// faction to a new long-term posture, starting a real
    /// `balance::FOCUS_SWITCH_DAYS` transition during which neither the old
    /// focus's effects nor the new one's apply - see
    /// `apply_set_national_focus`'s doc for exactly how that keeps this
    /// action un-spammable.
    SetNationalFocus(NationalFocus),
    /// Stage 4B (docs/phase4-spec.md "Stage 4B — 自然言語外交"): queues a
    /// one-tick free-text proposal to `to`, visible via `Observation`/
    /// `Diplomacy::pending_nl` - the natural-language counterpart of
    /// `ProposeTreaty`. `to`'s own agent (LLM-backed or keyword-fallback,
    /// both in `archipelago-agents`) is responsible for interpreting `text`
    /// into `TreatyTerm`s and answering with
    /// `RespondToNaturalLanguageProposal` - this crate never parses `text`
    /// itself.
    ProposeInNaturalLanguage { to: FactionId, text: String },
    /// Resolves a pending natural-language proposal *from* `from` *to* this
    /// faction: `terms` is this faction's own interpretation of that
    /// proposal's text (produced entirely outside this crate) and `accept`
    /// is its accept/reject verdict. Every term is independently
    /// re-validated against the *current* world before anything happens -
    /// see `diplomacy::apply_treaty_terms` - so an interpretation that says
    /// "accept" can still produce no change at all.
    RespondToNaturalLanguageProposal { from: FactionId, terms: Vec<TreatyTerm>, accept: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionError {
    NotOwner,
    UnitDead,
    NotAdjacent,
    Pinned,
    RegionNotOwned,
    RegionContested,
    InsufficientManpower,
    InsufficientEquipment,
    InvalidValue,
    /// `Action::Build` on a region that already has a project in progress.
    AlreadyBuilding,
    /// `Action::CancelBuild` on a region with no project in progress.
    NoConstruction,
    /// Stage 2D: `Action::RecruitUnit { domain: Domain::Sea, .. }` against a
    /// region with no port (or, in principle, no facing sea zone at all).
    NoPort,
}

pub fn apply_action(
    world: &mut World,
    faction: FactionId,
    action: Action,
) -> Result<(), ActionError> {
    match action {
        Action::MoveUnit { unit, to } => apply_move(world, faction, unit, to),
        Action::HoldUnit { unit } => apply_hold(world, faction, unit),
        Action::DisbandUnit { unit } => apply_disband(world, faction, unit),
        Action::RecruitUnit { region, domain } => apply_recruit(world, faction, region, domain),
        Action::ReinforceUnit { unit } => apply_reinforce(world, faction, unit),
        Action::SetConscription(value) => apply_set_conscription(world, faction, value),
        Action::SetIndustryPriority { good, weight } => {
            apply_set_industry_priority(world, faction, good, weight)
        }
        Action::SetCivilianRation(value) => apply_set_civilian_ration(world, faction, value),
        Action::Build { region, project } => apply_build(world, faction, region, project),
        Action::CancelBuild { region } => apply_cancel_build(world, faction, region),
        Action::SetImportPlan { good, rate } => apply_set_import_plan(world, faction, good, rate),
        Action::SetLogisticsPriority { good, weight } => {
            apply_set_logistics_priority(world, faction, good, weight)
        }
        Action::ProposeTreaty { to, treaty } => apply_propose_treaty(world, faction, to, treaty),
        Action::AcceptTreaty { from, treaty } => apply_accept_treaty(world, faction, from, treaty),
        Action::RejectTreaty { from, treaty } => apply_reject_treaty(world, faction, from, treaty),
        Action::DeclareWar { to } => apply_declare_war(world, faction, to),
        Action::BreakTreaty { with, treaty } => apply_break_treaty(world, faction, with, treaty),
        Action::SetNationalFocus(focus) => apply_set_national_focus(world, faction, focus),
        Action::ProposeInNaturalLanguage { to, text } => apply_propose_nl(world, faction, to, text),
        Action::RespondToNaturalLanguageProposal { from, terms, accept } => {
            apply_respond_nl(world, faction, from, terms, accept)
        }
    }
}

fn owned_unit<'a>(
    world: &'a World,
    faction: FactionId,
    unit: UnitId,
) -> Result<&'a Unit, ActionError> {
    let unit = world.units.get(unit.index()).ok_or(ActionError::UnitDead)?;
    if !unit.alive {
        return Err(ActionError::UnitDead);
    }
    if unit.owner != faction {
        return Err(ActionError::NotOwner);
    }
    Ok(unit)
}

fn apply_move(
    world: &mut World,
    faction: FactionId,
    unit_id: UnitId,
    to: Station,
) -> Result<(), ActionError> {
    let unit = owned_unit(world, faction, unit_id)?;
    let from = unit.station;

    let pinned = match from {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    };
    if pinned {
        return Err(ActionError::Pinned);
    }

    let (required, strait_zone) = match (from, to) {
        (Station::Region(from_r), Station::Region(to_r)) => {
            let link = world.link_between(from_r, to_r).ok_or(ActionError::NotAdjacent)?;
            let dest = world.region(to_r);
            let hostile = dest.owner != faction;
            let required = move_required(link.kind, dest.terrain, hostile);
            // External code review fix (Stage 2D): a Strait link's crossing
            // time is throttled the same way its supply throughput is — by
            // the highest sea control any other faction holds in the zone
            // it passes through — but sea control is recomputed every tick,
            // so that factor must NOT be sampled once and baked into
            // `required` here (docs/phase2-spec.md "1. 海峡リンクの遮断":
            // "移動もこの係数で遅くなる" means the crossing tracks *current*
            // control throughout, not the control at the moment it was
            // ordered). `required` stays the control-independent travel
            // cost; `tick_movement` applies the live factor to progress
            // every tick via `strait_zone`.
            (required, link.strait_zone)
        }
        (Station::Sea(from_z), Station::Sea(to_z)) => {
            if !world.sea_zone(from_z).adjacent.contains(&to_z) {
                return Err(ActionError::NotAdjacent);
            }
            let enemy_control = world.sea_zone(to_z).enemy_control_max(faction);
            let hostile = enemy_control > world.sea_zone(to_z).control[faction.index()];
            (fleet_move_required(hostile), None)
        }
        // A land unit can never be ordered into a sea zone, nor a fleet
        // into a region — `fleet_cannot_enter_land` and its converse are
        // exactly this branch.
        _ => return Err(ActionError::NotAdjacent),
    };

    world.unit_mut(unit_id).movement = Some(Movement {
        from,
        to,
        progress: 0.0,
        required,
        retreat: false,
        strait_zone,
    });
    Ok(())
}

fn apply_hold(world: &mut World, faction: FactionId, unit_id: UnitId) -> Result<(), ActionError> {
    owned_unit(world, faction, unit_id)?;
    world.unit_mut(unit_id).movement = None;
    Ok(())
}

/// `Action::DisbandUnit`. Before this, `RecruitUnit` spent manpower and
/// equipment to raise a unit and nothing ever gave a way to reverse it - a
/// faction whose territory (and therefore `agents::unit_cap`) shrank after
/// over-building had no path back to solvency at all. That is exactly the
/// one-way accumulator shape docs/conventions.md §6 warns against, and this
/// closes it.
///
/// Refunds the unit's *current* manpower and equipment - not the nominal
/// `UNIT_MANPOWER`/`UNIT_EQUIPMENT` a fresh recruit costs, so a unit that
/// took losses or was never fully reinforced gives back only what it
/// actually has - to `Faction::manpower` and `Faction::stock[Arms]`
/// respectively, the exact pools `apply_recruit` drew them from:
///
/// - Manpower goes back into the draft pool, not straight into the
///   civilian workforce. `region.mobilized`/`labor_ratio` are recomputed
///   every tick from `Faction::manpower` plus every living unit's manpower
///   (`economy::tick_economy`), so crediting the pool rather than
///   discarding the manpower keeps that identity honest, and
///   `MANPOWER_DEMOBILIZATION_RATE` - the same outflow that already keeps
///   the draft pool itself from being a one-way accumulator - drains it
///   back into `labor_ratio` over the following weeks exactly as it does
///   idle drafted conscripts. No new recovery mechanism is introduced;
///   disbanding just hands the existing one more to work with.
/// - Equipment goes back to `Good::Arms` stock outright - there is no
///   equivalent "pool with its own decay" to route it through; Arms is
///   already a plain stock every other system draws from and refills.
///
/// Rejected while the unit shares its station with an enemy
/// (`ActionError::RegionContested`, the same check and error
/// `apply_reinforce` uses for the same condition): a unit engaged with the
/// enemy cannot simply walk away and demobilise. This also closes an
/// exploit the refund above would otherwise open - without it, a faction
/// about to lose a unit in combat (which becomes an unrefunded casualty,
/// see `military::tick_combat`'s `Outcome::Destroyed`) could disband it the
/// instant before to cash out a full refund instead of losing it for
/// nothing.
fn apply_disband(world: &mut World, faction: FactionId, unit_id: UnitId) -> Result<(), ActionError> {
    let unit = owned_unit(world, faction, unit_id)?;
    let (station, manpower, equipment) = (unit.station, unit.manpower, unit.equipment);
    let pinned = match station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    };
    if pinned {
        return Err(ActionError::RegionContested);
    }

    world.faction_mut(faction).manpower += manpower;
    world.faction_mut(faction).stock[Good::Arms.index()] += equipment;
    world.unit_mut(unit_id).alive = false;
    Ok(())
}

fn apply_recruit(
    world: &mut World,
    faction: FactionId,
    region_id: RegionId,
    domain: Domain,
) -> Result<(), ActionError> {
    let region = world
        .regions
        .get(region_id.index())
        .ok_or(ActionError::RegionNotOwned)?;
    if region.owner != faction {
        return Err(ActionError::RegionNotOwned);
    }
    if world.has_enemy_units(region_id, faction) {
        return Err(ActionError::RegionContested);
    }

    // Stage 2D (docs/phase2-spec.md "艦隊": "艦隊は港のある自領地域でのみ建造
    // できる"): a fleet needs a port to launch from; a land unit doesn't
    // care about `region.port` at all.
    let station = match domain {
        Domain::Land => Station::Region(region_id),
        Domain::Sea => {
            if region.port <= 0.0 {
                return Err(ActionError::NoPort);
            }
            let zone = naval::home_zone(world, region_id).ok_or(ActionError::NoPort)?;
            Station::Sea(zone)
        }
    };

    // Stage 3C `NationalFocus::MaritimeTrade` (docs/phase3-spec.md: "艦隊の
    // 建造コスト −"): only a `Domain::Sea` recruit's Arms *cost* is
    // discounted - the fleet's own `equipment` stat below still starts at
    // the normal `UNIT_EQUIPMENT`, so this is cheaper shipbuilding, not a
    // weaker fleet.
    let f = world.faction(faction);
    let equipment_cost = if domain == Domain::Sea
        && focus::active(f) == Some(NationalFocus::MaritimeTrade)
    {
        UNIT_EQUIPMENT * FOCUS_MARITIME_FLEET_COST_MULT
    } else {
        UNIT_EQUIPMENT
    };
    if f.manpower < UNIT_MANPOWER {
        return Err(ActionError::InsufficientManpower);
    }
    if f.stock[Good::Arms.index()] < equipment_cost {
        return Err(ActionError::InsufficientEquipment);
    }

    world.faction_mut(faction).manpower -= UNIT_MANPOWER;
    world.faction_mut(faction).stock[Good::Arms.index()] -= equipment_cost;

    let id = UnitId(world.units.len() as u32);
    let kind = match domain {
        Domain::Land => "Corps",
        Domain::Sea => "Fleet",
    };
    let name = format!("{} {} {}", world.faction(faction).name, kind, id.0);
    world.units.push(Unit {
        id,
        owner: faction,
        name,
        station,
        movement: None,
        manpower: UNIT_MANPOWER,
        equipment: UNIT_EQUIPMENT,
        organization: UNIT_ORG * UNIT_START_ORG_RATIO,
        morale: 1.0,
        supply: 1.0,
        arms_delivery: 1.0,
        arms_budget: 0.0,
        arms_delivery_station: station,
        experience: 0.0,
        alive: true,
    });
    Ok(())
}

fn apply_reinforce(
    world: &mut World,
    faction: FactionId,
    unit_id: UnitId,
) -> Result<(), ActionError> {
    let unit = owned_unit(world, faction, unit_id)?;
    let pinned = match unit.station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    };
    if pinned {
        return Err(ActionError::RegionContested);
    }

    // External code review fix (Stage 2C; Stage 2D extends it to fleets):
    // `arms_delivery`/`arms_budget` are stamped by
    // `logistics::distribute_supply`, which runs once a tick *before*
    // movement. If this unit has moved since that stamp
    // (`arms_delivery_station != station`), the cached numbers describe a
    // place it has already left - trust them and a unit could finish
    // marching (or sailing) out of a well-supplied place into a cut-off one
    // and still reinforce at the old, high ratio. Recompute fresh for the
    // *current* station on the spot instead of trying to invalidate/track
    // the cache from `military::tick_movement` (deriving on demand here is
    // the simpler thing to reason about: one call site, no extra
    // bookkeeping needed anywhere movement happens), then stamp the
    // refreshed numbers back onto the unit so a second `ReinforceUnit`
    // against it later in this same batch sees the already-fresh,
    // already-being-spent budget rather than recomputing - and
    // re-granting - it again.
    if unit.arms_delivery_station != unit.station {
        let (ratio, budget) = match unit.station.domain() {
            Domain::Land => logistics::instantaneous_arms_delivery(world, unit_id),
            Domain::Sea => naval::instantaneous_fleet_arms_delivery(world, unit_id),
        };
        let unit = world.unit_mut(unit_id);
        unit.arms_delivery = ratio;
        unit.arms_budget = budget;
        unit.arms_delivery_station = unit.station;
    }

    let unit = world.unit(unit_id);
    let need_manpower = (UNIT_MANPOWER - unit.manpower).max(0.0);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
    // External code review fix (Stage 2C): `arms_budget` is a real
    // allowance that gets spent down below, not a ratio re-applied to
    // whatever gap remains - the pre-fix `need_equipment * arms_delivery`
    // let repeated `ReinforceUnit` actions in one batch compound past a
    // single tick's delivery allowance (each call recomputed the ratio
    // against the now-smaller remaining gap instead of a shrinking budget).
    let deliverable_equipment = need_equipment.min(unit.arms_budget.max(0.0));

    let f = world.faction(faction);
    let fill_manpower = need_manpower.min(f.manpower);
    let fill_equipment = deliverable_equipment.min(f.stock[Good::Arms.index()]);

    world.faction_mut(faction).manpower -= fill_manpower;
    world.faction_mut(faction).stock[Good::Arms.index()] -= fill_equipment;
    let unit = world.unit_mut(unit_id);
    unit.manpower += fill_manpower;
    unit.equipment += fill_equipment;
    unit.arms_budget = (unit.arms_budget - fill_equipment).max(0.0);
    Ok(())
}

fn apply_set_conscription(
    world: &mut World,
    faction: FactionId,
    value: f32,
) -> Result<(), ActionError> {
    if !(0.0..=1.0).contains(&value) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).conscription = value;
    Ok(())
}

fn apply_set_industry_priority(
    world: &mut World,
    faction: FactionId,
    good: Good,
    weight: f32,
) -> Result<(), ActionError> {
    if !(0.0..=1.0).contains(&weight) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).industry_priority[good.index()] = weight;
    Ok(())
}

fn apply_set_civilian_ration(
    world: &mut World,
    faction: FactionId,
    value: f32,
) -> Result<(), ActionError> {
    if !(CIVILIAN_RATION_MIN..=CIVILIAN_RATION_MAX).contains(&value) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).civilian_ration = value;
    Ok(())
}

/// `Action::SetImportPlan` (docs/phase2-spec.md "1. 海上輸入"): only `Food`
/// and `Energy` are importable - any other good is rejected outright. A
/// valid good's `rate` is clamped to `0.0..=IMPORT_PLAN_RATE_MAX` rather than
/// rejected, per the spec's "rate は 0 以上、上限でクランプ".
fn apply_set_import_plan(
    world: &mut World,
    faction: FactionId,
    good: Good,
    rate: f32,
) -> Result<(), ActionError> {
    if good != Good::Food && good != Good::Energy {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).import_plan[good.index()] = rate.clamp(0.0, IMPORT_PLAN_RATE_MAX);
    Ok(())
}

/// `Action::SetLogisticsPriority` (docs/phase2-spec.md "3. 品目別の到達率"):
/// same validation shape as `apply_set_industry_priority` - only the
/// `Munitions`/`Arms` weights are ever read by `logistics::distribute_supply`.
fn apply_set_logistics_priority(
    world: &mut World,
    faction: FactionId,
    good: Good,
    weight: f32,
) -> Result<(), ActionError> {
    if !(0.0..=1.0).contains(&weight) {
        return Err(ActionError::InvalidValue);
    }
    world.faction_mut(faction).logistics_priority[good.index()] = weight;
    Ok(())
}

/// `Action::Build` (docs/phase2-spec.md Stage 2B): own region, not
/// contested, no project already running.
fn apply_build(
    world: &mut World,
    faction: FactionId,
    region_id: RegionId,
    project: Project,
) -> Result<(), ActionError> {
    let region = world
        .regions
        .get(region_id.index())
        .ok_or(ActionError::RegionNotOwned)?;
    if region.owner != faction {
        return Err(ActionError::RegionNotOwned);
    }
    if world.has_enemy_units(region_id, faction) {
        return Err(ActionError::RegionContested);
    }
    if region.construction.is_some() {
        return Err(ActionError::AlreadyBuilding);
    }

    world.region_mut(region_id).construction = Some(Construction {
        project,
        invested: 0.0,
        required: required_points(project),
    });
    Ok(())
}

/// `Action::CancelBuild` (docs/phase2-spec.md Stage 2B): resources already
/// invested are forfeited — the `Construction` is simply discarded, not
/// refunded.
fn apply_cancel_build(
    world: &mut World,
    faction: FactionId,
    region_id: RegionId,
) -> Result<(), ActionError> {
    let region = world
        .regions
        .get(region_id.index())
        .ok_or(ActionError::RegionNotOwned)?;
    if region.owner != faction {
        return Err(ActionError::RegionNotOwned);
    }
    if region.construction.is_none() {
        return Err(ActionError::NoConstruction);
    }

    world.region_mut(region_id).construction = None;
    Ok(())
}

/// `Action::ProposeTreaty` (docs/phase3-spec.md "条約"). Rejects a
/// self-target, a dead/unknown target, a treaty already active between the
/// pair, and a (pair, treaty) combination still on cooldown after a recent
/// break - each of these keeps `ProposeTreaty` from being a free, repeatable
/// no-op an optimiser could spam for no reason. A *duplicate* outgoing
/// proposal (same treaty already pending to the same target) is allowed
/// through here but has no additional effect - `diplomacy::propose` replaces
/// rather than stacks it.
fn apply_propose_treaty(
    world: &mut World,
    faction: FactionId,
    to: FactionId,
    treaty: Treaty,
) -> Result<(), ActionError> {
    if to == faction {
        return Err(ActionError::InvalidValue);
    }
    if world.factions.get(to.index()).is_none_or(|f| !f.alive) {
        return Err(ActionError::InvalidValue);
    }
    if world.diplomacy.has_treaty(faction, to, treaty) {
        return Err(ActionError::InvalidValue);
    }
    if world.diplomacy.cooldown(faction, to, treaty) > 0 {
        return Err(ActionError::InvalidValue);
    }
    diplomacy::propose(world, faction, to, treaty);
    Ok(())
}

/// `Action::AcceptTreaty`: `from` must have an outstanding proposal of
/// exactly this `treaty` to this faction. Consumes it (removed from
/// `Diplomacy::pending` here, before `diplomacy::accept` applies the
/// effect) so it can never be accepted twice.
///
/// External code review fix A2: also revalidates the treaty isn't already
/// active between the pair before applying it - a crossed-bilateral
/// proposal (`a` proposes to `b` while `b` independently proposes the same
/// treaty to `a`) would otherwise let accepting the second one re-apply
/// `diplomacy::accept` (and its `TREATY_ACCEPT_OPINION_BONUS`) for a treaty
/// that accepting the first already activated. `diplomacy::accept` itself
/// now clears the reverse-direction proposal the instant a treaty activates
/// (see its doc), so this check is a backstop rather than the only guard -
/// but a `PendingProposal` predating that fix, or reaching this some other
/// way, must still never be actable on twice.
fn apply_accept_treaty(
    world: &mut World,
    faction: FactionId,
    from: FactionId,
    treaty: Treaty,
) -> Result<(), ActionError> {
    let idx = world
        .diplomacy
        .pending
        .iter()
        .position(|p| p.from == from && p.to == faction && p.treaty == treaty)
        .ok_or(ActionError::InvalidValue)?;
    if world.diplomacy.has_treaty(faction, from, treaty) {
        return Err(ActionError::InvalidValue);
    }
    world.diplomacy.pending.remove(idx);
    diplomacy::accept(world, faction, from, treaty);
    Ok(())
}

/// `Action::RejectTreaty`: same lookup as `AcceptTreaty`, but simply
/// discards the proposal instead of applying it.
fn apply_reject_treaty(
    world: &mut World,
    faction: FactionId,
    from: FactionId,
    treaty: Treaty,
) -> Result<(), ActionError> {
    let idx = world
        .diplomacy
        .pending
        .iter()
        .position(|p| p.from == from && p.to == faction && p.treaty == treaty)
        .ok_or(ActionError::InvalidValue)?;
    world.diplomacy.pending.remove(idx);
    diplomacy::reject(world, from, faction, treaty);
    Ok(())
}

/// `Action::DeclareWar` (docs/phase3-spec.md "Ceasefire": "いつでも
/// DeclareWar で破棄できる"): only valid against a current `Stance::Ceasefire`
/// - already `War` is a no-op the action layer refuses rather than silently
/// accepting, and `NonAggression`/`Alliance` must go through `BreakTreaty`
/// (the former for its notice period, the latter because breaking an
/// alliance is a rupture, not automatically a declaration of war).
fn apply_declare_war(world: &mut World, faction: FactionId, to: FactionId) -> Result<(), ActionError> {
    if to == faction || world.factions.get(to.index()).is_none_or(|f| !f.alive) {
        return Err(ActionError::InvalidValue);
    }
    if world.diplomacy.stance(faction, to) != Stance::Ceasefire {
        return Err(ActionError::InvalidValue);
    }
    let mut events = Vec::new();
    diplomacy::declare_war(world, faction, to, &mut events);
    world.diplomacy.log.extend(events);
    Ok(())
}

/// `Action::BreakTreaty`: `with` must currently hold exactly the treaty
/// being broken (rejects breaking something not actually active, and
/// `Treaty::Ceasefire` outright - see `Action::DeclareWar`'s doc).
fn apply_break_treaty(
    world: &mut World,
    faction: FactionId,
    with: FactionId,
    treaty: Treaty,
) -> Result<(), ActionError> {
    if with == faction || treaty == Treaty::Ceasefire {
        return Err(ActionError::InvalidValue);
    }
    if !world.diplomacy.has_treaty(faction, with, treaty) {
        return Err(ActionError::InvalidValue);
    }
    if treaty == Treaty::NonAggression && world.diplomacy.pending_break(faction, with).is_some() {
        // Already serving notice - breaking it twice must not restart (or
        // extend) the countdown.
        return Err(ActionError::InvalidValue);
    }
    diplomacy::break_treaty(world, faction, with, treaty);
    Ok(())
}

/// `Action::SetNationalFocus` (docs/phase3-spec.md "Stage 3C — 国家方針").
/// Two rules keep this un-spammable (docs/phase3-spec.md §0: "SetNational-
/// Focus がファーム/回避に使われないこと"), together closing off every shape
/// rapid repeated calls could exploit:
/// - Setting the *same* focus that's already current — whether it's already
///   active or a switch to it is already under way — is a pure no-op: it
///   neither starts a new transition nor resets/extends one in progress. An
///   agent that calls this every tick with the same target pays the
///   transition exactly once, on the same schedule as a single call.
/// - Requesting a *different* focus while a switch is already under way
///   (`focus_transition_days > 0`) is rejected outright. The agent must let
///   the current transition finish before redirecting it - without this, an
///   agent could keep retargeting the switch and never actually settle on
///   anything, or attempt to reuse a transition already partway elapsed
///   toward a different destination for free.
///
/// Because `focus::active` treats *any* faction with `focus_transition_days
/// > 0` as having no focus in effect (neither the abandoned one nor the new
/// one - see `focus.rs`'s module doc), there is additionally no window in
/// which switching, however rapidly, ever nets a bonus: every switch pays
/// the full `FOCUS_SWITCH_DAYS` blackout, unconditionally.
fn apply_set_national_focus(
    world: &mut World,
    faction: FactionId,
    focus: NationalFocus,
) -> Result<(), ActionError> {
    let f = world.faction_mut(faction);
    if focus == f.national_focus {
        return Ok(());
    }
    if f.focus_transition_days > 0 {
        return Err(ActionError::InvalidValue);
    }
    f.national_focus = focus;
    f.focus_transition_days = FOCUS_SWITCH_DAYS;
    Ok(())
}

/// `Action::ProposeInNaturalLanguage` (docs/phase4-spec.md "Stage 4B").
/// Rejects a self-target, a dead/unknown target, an empty or oversized
/// `text`, and - the abuse-resistance guard, mirroring
/// `apply_propose_treaty`'s own - a repeat attempt while one is already
/// outstanding from this faction to `to` or still on
/// `NL_PROPOSAL_COOLDOWN_DAYS` cooldown from the last one being answered or
/// expiring. Unlike `apply_propose_treaty`, a duplicate attempt here is
/// rejected outright rather than silently accepted as a no-op: there is no
/// "same treaty, so it's harmless to no-op" concept for free text, and
/// rejecting gives a caller a clear signal that this attempt did nothing.
fn apply_propose_nl(
    world: &mut World,
    faction: FactionId,
    to: FactionId,
    text: String,
) -> Result<(), ActionError> {
    if to == faction {
        return Err(ActionError::InvalidValue);
    }
    if world.factions.get(to.index()).is_none_or(|f| !f.alive) {
        return Err(ActionError::InvalidValue);
    }
    if text.trim().is_empty() || text.chars().count() > NL_PROPOSAL_TEXT_MAX_CHARS {
        return Err(ActionError::InvalidValue);
    }
    if world.diplomacy.find_pending_nl(faction, to).is_some() {
        return Err(ActionError::InvalidValue);
    }
    if world.diplomacy.nl_cooldown(faction, to) > 0 {
        return Err(ActionError::InvalidValue);
    }
    diplomacy::propose_nl(world, faction, to, text);
    Ok(())
}

/// `Action::RespondToNaturalLanguageProposal`: `from` must have an
/// outstanding natural-language proposal to this faction. Consumes it
/// (removed from `Diplomacy::pending_nl` here, before `diplomacy::respond_nl`
/// applies the verdict) so it can never be answered twice - the same shape
/// `apply_accept_treaty`/`apply_reject_treaty` already use for `pending`.
/// Whether the deal actually takes effect is entirely
/// `diplomacy::apply_treaty_terms`'s call, not this function's - see its doc.
fn apply_respond_nl(
    world: &mut World,
    faction: FactionId,
    from: FactionId,
    terms: Vec<TreatyTerm>,
    accept: bool,
) -> Result<(), ActionError> {
    let idx = world
        .diplomacy
        .find_pending_nl(from, faction)
        .ok_or(ActionError::InvalidValue)?;
    world.diplomacy.pending_nl.remove(idx);
    diplomacy::respond_nl(world, from, faction, &terms, accept);
    Ok(())
}
