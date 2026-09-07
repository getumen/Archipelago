//! Player/agent-facing commands and the validation that turns them into
//! world mutations. Invalid actions are rejected, never panicked on.

use crate::air;
use crate::balance::{
    AIR_OPERATING_RADIUS_KM, AIR_UNIT_MACHINERY_COST, CIVILIAN_RATION_MAX, CIVILIAN_RATION_MIN,
    FOCUS_MARITIME_FLEET_COST_MULT, FOCUS_SWITCH_DAYS, IMPORT_PLAN_RATE_MAX, LINE_INTERDICTION_DAMAGE,
    NL_PROPOSAL_TEXT_MAX_CHARS, NODE_STRIKE_DAMAGE, UNIT_EQUIPMENT, UNIT_MANPOWER, UNIT_ORG,
    UNIT_START_ORG_RATIO,
};
use crate::construction::{required_points, Construction, Project};
use crate::diplomacy::{self, Stance, Treaty, TreatyTerm};
use crate::focus::{self, NationalFocus};
use crate::good::Good;
use crate::ids::{FactionId, RegionId, TransportLineId, TransportNodeId, UnitId};
use crate::logistics;
use crate::military::{air_move_required, fleet_move_required, move_required, Movement, Unit};
use crate::naval;
use crate::transport::{Condition, TransportNodeKind};
use crate::world::{Domain, Station, World};

/// Stage 4B (docs/phase4-spec.md "Stage 4B"): `RespondToNaturalLanguageProposal`
/// carries an owned `Vec<TreatyTerm>` and `ProposeInNaturalLanguage` an owned
/// `String`, so `Action` can no longer derive `Copy` - every caller that
/// used to copy an `Action` implicitly (`sim::Simulation::apply`'s old `for
/// &act in actions`) now clones it explicitly instead.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// A land unit's `to` must be `Station::Region`; a fleet's must be
    /// `Station::Sea`; a squadron's must be `Station::Airfield` —
    /// `apply_move` validates the destination matches the moving unit's own
    /// domain and rejects it otherwise (`ActionError::NotAdjacent`). Stage
    /// 10 follow-up: unlike land/sea, a squadron's destination airfield must
    /// also be this faction's own, operational, and within
    /// `balance::AIR_OPERATING_RADIUS_KM` of its current field - see
    /// `apply_move`'s own `Station::Airfield` arm for the full account.
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
    /// Stage 9D (docs/phase9-spec.md "4. 行動": "路線の遮断・復旧に関わる行動"):
    /// a deliberate strike against one route of the transport network
    /// (`crate::transport`), lowering its `Condition` by
    /// `balance::LINE_INTERDICTION_DAMAGE` outright rather than waiting on
    /// `transport::tick_transport_condition`'s passive contested-region
    /// damage. Only valid against a line owned entirely by a faction this
    /// one is currently at war with (`apply_interdict_line`'s own doc) - the
    /// repair counterpart lives on `Build`'s own `Project::TransportLine`
    /// instead, since restoring a route is funded, gradual infrastructure
    /// work, not a one-shot strike.
    InterdictLine { line: TransportLineId },
    /// Stage 10C (docs/phase10-spec.md "3. 阻止": "飛行場ノードと港ノードを叩
    /// けること"): a deliberate strike against one `transport::TransportNode`
    /// - lowers its own `condition` by `balance::NODE_STRIKE_DAMAGE` outright,
    /// the node-level twin of `InterdictLine`'s line-level strike. Only valid
    /// against an `Airfield` or `Port` node (design.md §8's own "物流拠点";
    /// a `Depot`/`Junction` target is rejected - `ActionError::
    /// NodeNotStrikeable`) in a region this faction is currently at war with
    /// (`ActionError::NodeNotHostile`). Deliberately no locality requirement,
    /// the same as `InterdictLine`'s own doc: this is the one action stage
    /// 10C gives air power to demonstrate design.md §8's "敵は領土そのもの
    /// ではなく、物流拠点を攻撃することも可能" without requiring this crate to
    /// model an actual air-to-ground strike mission - air becomes this
    /// action's (and `InterdictLine`'s) principal user from 10D's AI onward,
    /// never its only legal one.
    StrikeNode { node: TransportNodeId },
}

/// One of the four decision domains every `Action` belongs to (design.md
/// §14/§22: Human/Heuristic/LLM/RL agents interchangeable, and users writing
/// their own). A `Layer` is a coarser routing key than the concrete `Action`
/// variant - an external RL policy, or an internal `archipelago-agents`
/// `CompositeAgent`, decides per layer rather than per variant, so this
/// needs to partition every current *and future* `Action` variant into
/// exactly one bucket. `Action::layer` is where that partition is enforced;
/// this type intentionally carries no behaviour of its own.
///
/// The four boundaries, and the judgement calls behind them:
///
/// - **`Military`** — orders that move, raise, or stand down force
///   structure: `MoveUnit`/`HoldUnit`/`DisbandUnit`/`ReinforceUnit`, and
///   `RecruitUnit` alongside them even though it spends `Faction::manpower`
///   and `Good::Arms` (economic resources, same as `ReinforceUnit`). The
///   decision `RecruitUnit` represents — how large the army is, and where —
///   is inseparable from the other four force-structure actions: a policy
///   that could march and reinforce units but never raise or retire one
///   couldn't meaningfully "be the military" at all. Resource cost was
///   deliberately not used as the classifying property, or `ReinforceUnit`
///   (uncontroversially military) would have to move to `Economy` too.
/// - **`Economy`** — national resource policy: `SetConscription`,
///   `SetCivilianRation`, `SetIndustryPriority`, `SetLogisticsPriority`,
///   `SetImportPlan`, plus `Build`/`CancelBuild`. Construction is design.md
///   §9's own economic system listing "建設" alongside food/steel/energy as
///   one of the industries a national economy runs, and every project it
///   funds (`crate::construction::Project`) draws on the same
///   Machinery/Steel stock `SetIndustryPriority` allocates between goods —
///   a separate "infrastructure" layer would isolate two variants that
///   share every input and every constraint with the rest of economic
///   planning, for no distinct policy surface of their own.
/// - **`GrandStrategy`** — `SetNationalFocus` alone, deliberately not folded
///   into `Economy` or `Military` even though a given focus's effects land
///   on one of them (or on diplomacy): a `NationalFocus` is the one
///   decision that reshapes multipliers *across* every other layer at once
///   (military caution, economic priorities, treaty-seeking — see
///   `crate::focus::active`'s call sites throughout `archipelago-agents`),
///   so it sits above all three rather than inside any one of them. It is
///   also the lowest-frequency, highest-leverage action in the whole
///   vocabulary — exactly the shape an RL curriculum wants to isolate into
///   its own tiny (six-focus) action space rather than bury inside a larger
///   `Economy` or `Military` one.
/// - **`Diplomacy`** — every treaty and natural-language action:
///   `ProposeTreaty`/`AcceptTreaty`/`RejectTreaty`/`DeclareWar`/
///   `BreakTreaty`/`ProposeInNaturalLanguage`/
///   `RespondToNaturalLanguageProposal`. None of these touch a unit,
///   resource, or region directly; every one of them mutates only
///   `World::diplomacy` state.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Layer {
    Military,
    Economy,
    GrandStrategy,
    Diplomacy,
}

pub const LAYER_COUNT: usize = 4;

/// Every `Layer`, in a fixed order - iterate this instead of hand-rolling
/// one, the same convention `good::ALL_GOODS`/`diplomacy::ALL_TREATIES`/
/// `focus::ALL_FOCI` already follow, so nothing here ever depends on a
/// `HashMap`/`HashSet` iteration order (docs/conventions.md §5).
pub const ALL_LAYERS: [Layer; LAYER_COUNT] =
    [Layer::Military, Layer::Economy, Layer::GrandStrategy, Layer::Diplomacy];

impl Layer {
    pub const fn index(self) -> usize {
        match self {
            Layer::Military => 0,
            Layer::Economy => 1,
            Layer::GrandStrategy => 2,
            Layer::Diplomacy => 3,
        }
    }

    /// Lowercase English key, for the API `/schema` response and the
    /// Python env - mirrors `Good::key`/`Treaty::key`/`NationalFocus::key`.
    pub const fn key(self) -> &'static str {
        match self {
            Layer::Military => "military",
            Layer::Economy => "economy",
            Layer::GrandStrategy => "grand_strategy",
            Layer::Diplomacy => "diplomacy",
        }
    }
}

impl Action {
    /// Which `Layer` this action belongs to - see `Layer`'s own doc for the
    /// boundaries and the reasoning behind each judgement call.
    ///
    /// Exhaustive with no wildcard arm: a new `Action` variant fails to
    /// compile here until it is explicitly placed in a layer, rather than
    /// silently landing nowhere (or everywhere) the way a catch-all arm
    /// would let it - this is what makes the classification total and
    /// compiler-checked rather than a convention someone has to remember.
    pub fn layer(&self) -> Layer {
        match self {
            Action::MoveUnit { .. }
            | Action::HoldUnit { .. }
            | Action::DisbandUnit { .. }
            | Action::ReinforceUnit { .. }
            | Action::RecruitUnit { .. }
            // `InterdictLine` (Stage 9D): a wartime strike against the
            // enemy's own capability, the same category `RecruitUnit`
            // itself argues for above ("inseparable from the other
            // force-structure actions") - this is inseparable from the
            // rest of conducting the war, not a standing economic policy
            // (`Economy`'s own boundary is national resource *policy*,
            // which this isn't: it's a one-shot combat-like act against a
            // specific enemy target, the same shape `MoveUnit` into
            // contact already has). `StrikeNode` (Stage 10C) is
            // `InterdictLine`'s own node-level twin, the same reasoning
            // applies verbatim.
            | Action::InterdictLine { .. }
            | Action::StrikeNode { .. } => Layer::Military,

            Action::SetConscription(_)
            | Action::SetCivilianRation(_)
            | Action::SetIndustryPriority { .. }
            | Action::SetLogisticsPriority { .. }
            | Action::SetImportPlan { .. }
            | Action::Build { .. }
            | Action::CancelBuild { .. } => Layer::Economy,

            Action::SetNationalFocus(_) => Layer::GrandStrategy,

            Action::ProposeTreaty { .. }
            | Action::AcceptTreaty { .. }
            | Action::RejectTreaty { .. }
            | Action::DeclareWar { .. }
            | Action::BreakTreaty { .. }
            | Action::ProposeInNaturalLanguage { .. }
            | Action::RespondToNaturalLanguageProposal { .. } => Layer::Diplomacy,
        }
    }

    /// The existing unit this action orders, if it orders one at all - a
    /// finer routing key than `Layer`, for a caller that needs to route
    /// *within* `Military` rather than merely to it (`archipelago-agents`'
    /// `HumanAgent` per-unit delegation is the one user today: a delegated
    /// unit's `MoveUnit`/`HoldUnit`/`DisbandUnit`/`ReinforceUnit` should
    /// reach the wrapped agent, but nothing else should, unit-targeted or
    /// not).
    ///
    /// `RecruitUnit` returns `None` alongside every non-`Military` action -
    /// it *creates* a unit rather than commanding one that already exists,
    /// so there is no existing `UnitId` to route by (see `Layer::Military`'s
    /// own doc for why it is still classified `Military`). Exhaustive for
    /// the same reason `layer` is: a future action that does target a unit
    /// must be added here explicitly rather than silently falling through a
    /// wildcard into "does not target a unit".
    pub fn target_unit(&self) -> Option<UnitId> {
        match self {
            Action::MoveUnit { unit, .. }
            | Action::HoldUnit { unit }
            | Action::DisbandUnit { unit }
            | Action::ReinforceUnit { unit } => Some(*unit),

            Action::RecruitUnit { .. }
            | Action::SetConscription(_)
            | Action::SetCivilianRation(_)
            | Action::SetIndustryPriority { .. }
            | Action::SetLogisticsPriority { .. }
            | Action::SetImportPlan { .. }
            | Action::Build { .. }
            | Action::CancelBuild { .. }
            | Action::SetNationalFocus(_)
            | Action::ProposeTreaty { .. }
            | Action::AcceptTreaty { .. }
            | Action::RejectTreaty { .. }
            | Action::DeclareWar { .. }
            | Action::BreakTreaty { .. }
            | Action::ProposeInNaturalLanguage { .. }
            | Action::RespondToNaturalLanguageProposal { .. }
            | Action::InterdictLine { .. }
            | Action::StrikeNode { .. } => None,
        }
    }
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
    /// Stage 10A: `Action::RecruitUnit { domain: Domain::Air, .. }`'s own
    /// `Good::Machinery` cost (`balance::AIR_UNIT_MACHINERY_COST`) exceeds
    /// the faction's current stock - the airframe-specific sibling of
    /// `InsufficientEquipment`'s Arms check.
    InsufficientMachinery,
    InvalidValue,
    /// `Action::Build` on a region that already has a project in progress.
    AlreadyBuilding,
    /// `Action::CancelBuild` on a region with no project in progress.
    NoConstruction,
    /// Stage 2D: `Action::RecruitUnit { domain: Domain::Sea, .. }` against a
    /// region with no port (or, in principle, no facing sea zone at all).
    NoPort,
    /// Stage 10A: `Action::RecruitUnit { domain: Domain::Air, .. }` against
    /// a region with no `transport::TransportNodeKind::Airfield` node - the
    /// air-domain sibling of `NoPort`. Stage 10 follow-up: `apply_move`'s
    /// own `Station::Airfield` arm reuses this for a redeploy order naming a
    /// `TransportNodeId` that isn't genuinely an `Airfield`-kind node, or
    /// names one that is currently wrecked (`transport::TransportNode::
    /// operational` false) - the same "no usable airfield" fact, not a
    /// second error for what is the same experience from either action.
    NoAirfield,
    /// Stage 9D: `Action::InterdictLine`/`Action::Build`'s
    /// `Project::TransportLine` named a `TransportLineId` past the end of
    /// `World::transport_lines` - no such route exists.
    InvalidLine,
    /// Stage 9D: `Action::Build`'s `Project::TransportLine` named a line
    /// with an endpoint this faction doesn't own - only a faction's own
    /// route can be invested in.
    LineNotOwned,
    /// Stage 9D: `Action::InterdictLine` named a line that isn't owned
    /// entirely by a single faction this one is currently at war with (a
    /// line with mixed ownership, one owned by the acting faction itself,
    /// or one whose owner isn't a current war opponent).
    LineNotHostile,
    /// Stage 10C: `Action::StrikeNode` named a `TransportNodeId` past the
    /// end of `World::transport_nodes` - `InvalidLine`'s node-level twin.
    /// Stage 10 follow-up: `Action::MoveUnit`'s own `Station::Airfield` arm
    /// reuses this for the identical out-of-bounds case on its `to` target.
    InvalidNode,
    /// Stage 10C: `Action::StrikeNode` named a node whose `kind` isn't
    /// `Airfield` or `Port` (design.md §8's own "物流拠点") - a `Depot`/
    /// `Junction` is not a legal strike target.
    NodeNotStrikeable,
    /// Stage 10C: `Action::StrikeNode` named a node whose own region isn't
    /// owned by a faction this one is currently at war with (its own
    /// region, or one at peace) - `LineNotHostile`'s node-level twin.
    NodeNotHostile,
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
        Action::InterdictLine { line } => apply_interdict_line(world, faction, line),
        Action::StrikeNode { node } => apply_strike_node(world, faction, node),
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
        // Stage 10A: same rule `military::is_pinned`'s `Station::Airfield`
        // arm already uses - an airfield is exactly as contested as the
        // region it sits inside.
        Station::Airfield(node) => world.has_enemy_units(world.transport_node(node).region, faction),
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
        // Stage 10 follow-up (docs/phase10-spec.md "1. 基地": a squadron's
        // location is a `Station::Airfield`, so its own move is airfield-to-
        // airfield, not region/sea-zone adjacency): a squadron flies, so
        // "adjacent" means geographic reach, not a graph edge — reuses the
        // exact `AIR_OPERATING_RADIUS_KM` reach `air::tick_air_superiority`
        // already gives a *stationary* squadron's committed power, rather
        // than inventing a second, unmeasured "ferry range" constant (see
        // `balance::AIR_MOVE_DAYS`'s own doc for why). A squadron that could
        // never contest the sky over its own destination in the first place
        // has no business being told it can fly there.
        //
        // Ownership, not adjacency, is what stands in for "friendly
        // territory" here: unlike a land unit (which can be ordered into a
        // hostile-owned region — that is how an invasion happens — with
        // `move_required`'s `hostile` multiplier slowing it down) or a fleet
        // (which can enter contested water), no mechanism in this crate lets
        // a squadron fight its way onto a foreign airfield. `apply_recruit`'s
        // own `Domain::Air` arm already requires a fresh squadron's home
        // field to be this faction's own operational airfield
        // (`ActionError::NoAirfield`); a redeploying squadron is held to the
        // identical requirement, reusing `ActionError::RegionNotOwned` (the
        // same "not this faction's to use" fact `apply_build`/`apply_recruit`
        // already spend that error on) for a destination owned by someone
        // else, and `ActionError::NoAirfield` for a destination that either
        // isn't genuinely an `Airfield`-kind node or is currently wrecked
        // (`transport::TransportNode::operational`) — "no usable airfield
        // there" is the same fact `apply_recruit`'s own doc already reads
        // "no airfield" and "the airfield is rubble" as, not two different
        // errors for what a recruiting or redeploying faction experiences as
        // one and the same thing: nowhere to land right now.
        //
        // No live in-transit factor to sample (unlike a `Strait` link's
        // `strait_zone`, this arm's `strait_zone` output is always `None`):
        // a strait is a shared chokepoint with its own persistent
        // `SeaZoneId` that `tick_movement` re-reads every tick a crossing is
        // under way. A flight between two airfields has no such standing
        // entity in this data model to re-read — `Region::air_superiority`
        // is defined over *regions*, not over the line between two
        // airfields, and inventing a flight-path-vs-region intersection test
        // to throttle a redeployment mid-flight would be exactly the kind of
        // unrequested new mechanism docs/conventions.md §1 requires asking
        // about before building, for an interception mechanic Phase 10 never
        // specified in the first place (10C's own interdiction is about
        // *stationary* squadrons projecting power over regions, never about
        // catching another squadron en route). The origin's own contested-
        // ness is still covered — the shared `pinned` check above already
        // refuses to let a squadron leave an airfield under enemy ground
        // contact, the same as it does for every other domain.
        (Station::Airfield(from_node), Station::Airfield(to_node)) => {
            // `codex review` (P2): a move to the field the squadron already
            // sits on passes every check below - operational, owned, zero
            // kilometres - and would start a one-day `Movement` to nowhere.
            // Repeating the order keeps the squadron perpetually in transit,
            // bleeding organization in `tick_movement` and never being
            // anywhere, which is precisely the sort of loop an optimiser is
            // expected to find (docs/mvp-spec.md §5's own premise). Land and
            // sea get this for free: a region is never adjacent to itself,
            // nor a sea zone to itself.
            if from_node == to_node {
                return Err(ActionError::NotAdjacent);
            }
            let dest = world.transport_nodes.get(to_node.index()).ok_or(ActionError::InvalidNode)?;
            if dest.kind != TransportNodeKind::Airfield || !dest.operational() {
                return Err(ActionError::NoAirfield);
            }
            if world.region(dest.region).owner != faction {
                return Err(ActionError::RegionNotOwned);
            }
            let from_pos = world.region(world.transport_node(from_node).region).position;
            let to_pos = world.region(dest.region).position;
            if air::geographic_distance(from_pos, to_pos) > AIR_OPERATING_RADIUS_KM {
                return Err(ActionError::NotAdjacent);
            }
            (air_move_required(), None)
        }
        // A land unit can never be ordered into a sea zone or an airfield,
        // nor a fleet into a region or an airfield, nor a squadron into a
        // region or a sea zone — `fleet_cannot_enter_land` and its
        // converse(s) are exactly this branch.
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
        // Stage 10A: same rule `military::is_pinned`'s `Station::Airfield`
        // arm already uses - an airfield is exactly as contested as the
        // region it sits inside.
        Station::Airfield(node) => world.has_enemy_units(world.transport_node(node).region, faction),
    };
    if pinned {
        return Err(ActionError::RegionContested);
    }

    world.faction_mut(faction).manpower += manpower;
    world.faction_mut(faction).stock[Good::Arms.index()] += equipment;
    // Stage 10A (`codex review`, P2): `apply_recruit` charges
    // `AIR_UNIT_MACHINERY_COST` on top of manpower and Arms for
    // `Domain::Air`, so disband has to hand the airframe back too or a
    // recruit/disband cycle silently destroys Machinery - a decrease with
    // no way back, which is the mirror image of CLAUDE.md's 「一方通行の
    // アキュムレータを作らない。増える量には戻る経路を持たせる」 and a
    // break with the disband contract every other domain already keeps.
    //
    // Refunded in proportion to the squadron's remaining equipment, the
    // same way `equipment` itself is returned rather than the full
    // recruitment charge: a squadron ground down to nothing has no airframes
    // left to recover, so a full refund would turn attrition into a way of
    // manufacturing Machinery out of losses.
    //
    // This refund is honest by construction, not by a second ledger:
    // `apply_reinforce` now charges `Good::Machinery` for an air unit's
    // equipment at exactly this same `AIR_UNIT_MACHINERY_COST /
    // UNIT_EQUIPMENT` rate whenever it delivers any (see its own doc) - so
    // the equipment this refund is proportional to can never have been
    // replaced with Arms alone. Before that fix, a damaged squadron could be
    // refilled with nothing but Arms and disbanded here for a full refund it
    // never paid back in - a repeatable Arms -> Machinery converter with no
    // cost on the Arms side, exactly the kind of one-way accumulator
    // CLAUDE.md's own record of this project's repeat defects warns against.
    if matches!(station, Station::Airfield(_)) {
        let intact = (equipment / UNIT_EQUIPMENT).clamp(0.0, 1.0);
        world.faction_mut(faction).stock[Good::Machinery.index()] += AIR_UNIT_MACHINERY_COST * intact;
    }
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
    // care whether the region has one at all. Stage 9B: "has a port" is
    // `World::has_port_node`'s question, not `region.port > 0.0`'s (see
    // `Region::port`'s own doc).
    let station = match domain {
        Domain::Land => Station::Region(region_id),
        Domain::Sea => {
            // Stage 10C (codex review P2, same survey as the `Domain::Air`
            // arm below): a port node wrecked by `Action::StrikeNode` must
            // refuse a new fleet exactly as it already refuses one with no
            // port at all - reusing `ActionError::NoPort` rather than a
            // fresh variant, since "no port node" and "the port node is
            // currently rubble" both cash out to the same fact from a
            // recruiting faction's point of view: there is nowhere here to
            // launch a fleet from right now. Gated on the same shared
            // `World::port_node_operational` `trade::tick_imports` already
            // uses, never a second, differently-shaped check.
            if !world.port_node_operational(region_id) {
                return Err(ActionError::NoPort);
            }
            let zone = naval::home_zone(world, region_id).ok_or(ActionError::NoPort)?;
            Station::Sea(zone)
        }
        // Stage 10A (docs/phase10-spec.md "1. 基地"): an air unit needs an
        // `Airfield` node to be based at, the air-domain sibling of
        // `Domain::Sea`'s port requirement above - `World::airfield_node`
        // is the sole authority for whether `region_id` has one at all
        // (`transport::TransportNodeKind::Airfield`'s own doc).
        //
        // Stage 10C (codex review P2): existence alone isn't enough any
        // more than it is for `Domain::Sea` above - a node wrecked by
        // `Action::StrikeNode` must refuse a fresh squadron the same way a
        // missing node does, via the exact `TransportNode::operational`
        // gate `air::node_air_power`/`logistics`'s supply graph already use
        // (never a second, independently-derived "does this airfield work"
        // check). Reuses `ActionError::NoAirfield` rather than a new
        // variant - "no airfield" and "the airfield is currently rubble"
        // are the same fact to a recruiting faction: nowhere to base a
        // squadron right now.
        Domain::Air => {
            // `codex review` (P2): a region may declare several airfields
            // and `logistics::build_transport_graph` gates each one's vertex
            // independently, so "can this region base a squadron" has to ask
            // every airfield, not just the lowest-id one - otherwise
            // striking the first grounded a region that still had a working
            // field, and striking a later one changed nothing. Base the new
            // squadron at an airfield that is actually standing.
            let node = world
                .transport_nodes
                .iter()
                .find(|n| {
                    n.region == region_id
                        && n.kind == crate::transport::TransportNodeKind::Airfield
                        && n.operational()
                })
                .ok_or(ActionError::NoAirfield)?;
            Station::Airfield(node.id)
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
    // Stage 10A (docs/phase10-spec.md "4. 生産"): an air unit's own
    // `Good::Machinery` cost, on top of the `UNIT_MANPOWER`/Arms cost every
    // domain already pays - the spec explicitly rules out a new commodity
    // ("新しい Good を追加しない"), so the airframe itself is priced in an
    // existing industrial input instead of a fourth recruit-cost good.
    let machinery_cost = if domain == Domain::Air { AIR_UNIT_MACHINERY_COST } else { 0.0 };
    if f.manpower < UNIT_MANPOWER {
        return Err(ActionError::InsufficientManpower);
    }
    if f.stock[Good::Arms.index()] < equipment_cost {
        return Err(ActionError::InsufficientEquipment);
    }
    if f.stock[Good::Machinery.index()] < machinery_cost {
        return Err(ActionError::InsufficientMachinery);
    }

    world.faction_mut(faction).manpower -= UNIT_MANPOWER;
    world.faction_mut(faction).stock[Good::Arms.index()] -= equipment_cost;
    world.faction_mut(faction).stock[Good::Machinery.index()] -= machinery_cost;

    let id = UnitId(world.units.len() as u32);
    let kind = match domain {
        Domain::Land => "Corps",
        Domain::Sea => "Fleet",
        Domain::Air => "Squadron",
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
        // Stage 10A: same rule `military::is_pinned`'s `Station::Airfield`
        // arm already uses - an airfield is exactly as contested as the
        // region it sits inside.
        Station::Airfield(node) => world.has_enemy_units(world.transport_node(node).region, faction),
    };
    if pinned {
        return Err(ActionError::RegionContested);
    }

    // Stage 9D fix (docs/conventions.md §6's "状態には必ず回復経路を持たせる"):
    // unlike equipment (gated below by `arms_budget`, itself derived from
    // the transport network), manpower reinforcement used to refill
    // straight from the faction's national manpower pool with no reference
    // to the network at all. `military::tick_recovery`'s own attrition
    // (`ATTRITION_MANPOWER`, driven by `unit.supply`) is the recovery path
    // that is supposed to eventually stand down a unit stranded beyond
    // every supply route - but an unconstrained manpower refill here undid
    // every tick's attrition loss the moment `strength()` dipped low enough
    // to trigger `HeuristicAgent::reinforce_weak_units`, well before manpower
    // ever neared `UNIT_DEATH_MANPOWER`, forever. Measured on
    // `scenarios/japan47.json` seed 1: several factions read `supply_ratio
    // == 0.0` with hundreds of units of *national* Munitions stock never
    // reaching their own cut-off fronts, and the same disconnect applies to
    // manpower - a conscript can no more march down a severed line than a
    // shell can ride one. Gated on the same reachability question
    // `instantaneous_arms_delivery`/`instantaneous_fleet_arms_delivery`
    // already ask for equipment (extracted as `land_unit_supply_avail`/
    // `naval::fleet_unit_supply_avail` so both can share it) - a binary
    // "does the network deliver anything at all to this unit's current
    // station", not a ratio, so repeated `ReinforceUnit` actions in one
    // batch can't compound past what an already-connected front could
    // deliver today (nothing changes there at all: `network_reachable` is
    // simply `true`). Once it reads `false`, manpower stops being an
    // avenue back to full strength and `tick_recovery`'s attrition is left
    // to run its course - the recovery path this project's own convention
    // requires.
    //
    // `codex review` P1 fix: asked *before* the arms-delivery recompute
    // below, not after. Both `naval::fleet_unit_supply_avail` and
    // `logistics::land_unit_supply_avail` detect staleness the same way -
    // comparing `Unit::arms_delivery_station` against the unit's current
    // `station` (see each function's own doc) and falling back to a fresh
    // flow re-run when they differ - but the arms-delivery block below
    // re-stamps `arms_delivery_station` onto the current station the moment
    // it runs. Asking `network_reachable` afterward would see "not stale"
    // and trust the still-unrefreshed `world.supply_sea`/`world.supply`
    // cache instead, silently undoing the fix for this exact call, in
    // either domain alike. `land_unit_supply_avail` used to be exempt from
    // this ordering trap only because it never consulted
    // `arms_delivery_station` at all - which was itself the land-side half
    // of this same staleness defect, left unfixed; now that it does, the
    // order above matters for land too, and is already correct for it since
    // `network_reachable` is computed once, ahead of both domains' recompute
    // blocks, not per-domain.
    let network_reachable = match unit.station.domain() {
        Domain::Land => logistics::land_unit_supply_avail(world, unit_id) > 0.0,
        Domain::Sea => naval::fleet_unit_supply_avail(world, unit_id) > 0.0,
        Domain::Air => air::air_unit_supply_avail(world, unit_id) > 0.0,
    };

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
        let domain = unit.station.domain();
        let (ratio, budget) = match domain {
            Domain::Land => logistics::instantaneous_arms_delivery(world, unit_id),
            Domain::Sea => naval::instantaneous_fleet_arms_delivery(world, unit_id),
            Domain::Air => air::instantaneous_air_arms_delivery(world, unit_id),
        };
        // `codex review` P1 fix (second round): claim the exact throughput
        // `instantaneous_arms_delivery`/`instantaneous_fleet_arms_delivery`'s
        // own `avail` just drew from this tick's shared, decreasing network-
        // capacity leftover (`logistics::SupplyLeftover`) - never a second,
        // fresh full-capacity recompute, which is what let N arrivals in one
        // tick each independently draw a whole tick's own capacity all over
        // again. Kept as an explicit, separate step from the peeks above
        // (`network_reachable`, and `instantaneous_arms_delivery`'s own
        // internal read) rather than folded into either of them, so asking
        // "is this reachable at all" never itself spends anything - only
        // this one call does, and this branch runs at most once per unit per
        // tick (the `arms_delivery_station` guard above), so it can never
        // double-spend for the same arrival - see `commit_instantaneous_
        // land_grant`'s own doc for the full account, including why the
        // order multiple *different* arriving units get processed in here is
        // not a "fixed priority" in the sense this project's conventions
        // forbid.
        match domain {
            Domain::Land => {
                logistics::commit_instantaneous_land_grant(world, unit_id);
            }
            Domain::Sea => {
                logistics::commit_instantaneous_sea_grant(world, unit_id);
            }
            Domain::Air => {
                logistics::commit_instantaneous_air_grant(world, unit_id);
            }
        }
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
    let is_air = unit.station.domain() == Domain::Air;

    let f = world.faction(faction);
    let fill_manpower = if network_reachable { need_manpower.min(f.manpower) } else { 0.0 };
    let mut fill_equipment = deliverable_equipment.min(f.stock[Good::Arms.index()]);
    // Stage 10A exploit fix (`codex review`, P2): an air unit's equipment
    // *is* its airframes, so replacing it must cost `Good::Machinery`, not
    // only `Good::Arms` - the same industrial input `apply_recruit` prices a
    // fresh airframe in (`AIR_UNIT_MACHINERY_COST` per `UNIT_EQUIPMENT`),
    // charged here at that same rate for whatever fraction of the gap is
    // actually delivered. Without this, `apply_disband`'s equipment-
    // proportional Machinery refund (see its own doc) turned combat losses
    // into a free Arms -> Machinery converter: damage a squadron, refill it
    // with nothing but abundant Arms, then disband it for a full Machinery
    // refund it never paid back in.
    //
    // Bounded by the faction's actual Machinery stock exactly the way
    // `fill_equipment` is already bounded by its Arms stock two lines above
    // - the same shape, not a second one: a hard cap on a real, currently-
    // held stock, checked once per call against whatever `fill_equipment`
    // already is (never a ratio re-applied to a shrinking remainder). This
    // keeps N `ReinforceUnit` calls in one batch bounded exactly like the
    // existing Arms/`arms_budget` caps: each call spends the stock down for
    // real, so the total delivered - and the total Machinery charged for it
    // - can never exceed what a single tick's `arms_budget` and starting
    // stock allow between them, in either domain's currency. Running out of
    // Machinery mid-reinforcement behaves exactly like running out of Arms
    // already does above: `fill_equipment` (and therefore the equipment
    // actually delivered) is simply capped down to what can be paid for,
    // never rejected with an `ActionError` and never delivered without
    // being paid for - the same partial-fill shape this function already
    // uses for every other resource, not a new one invented for this good.
    let machinery_cost = if is_air {
        let machinery_per_equipment = AIR_UNIT_MACHINERY_COST / UNIT_EQUIPMENT;
        let affordable_equipment = f.stock[Good::Machinery.index()] / machinery_per_equipment;
        fill_equipment = fill_equipment.min(affordable_equipment.max(0.0));
        fill_equipment * machinery_per_equipment
    } else {
        0.0
    };

    world.faction_mut(faction).manpower -= fill_manpower;
    world.faction_mut(faction).stock[Good::Arms.index()] -= fill_equipment;
    if is_air {
        world.faction_mut(faction).stock[Good::Machinery.index()] -= machinery_cost;
    }
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

    // Stage 9D (docs/phase9-spec.md "4. 行動"): a `Project::TransportLine`
    // must be hosted at one of its own two endpoint regions - so an agent
    // can't invest a distant region's Machinery/Steel into an unrelated
    // route - and the line must be entirely this faction's own (both
    // endpoints), the same "own network only" boundary
    // `logistics::compute_transport_flow` itself enforces for whether a
    // line is usable at all.
    if let Project::TransportLine(line_id) = project {
        let line = world.transport_lines.get(line_id.index()).ok_or(ActionError::InvalidLine)?;
        let ra = world.transport_node(line.from).region;
        let rb = world.transport_node(line.to).region;
        if region_id != ra && region_id != rb {
            return Err(ActionError::InvalidValue);
        }
        if world.region(ra).owner != faction || world.region(rb).owner != faction {
            return Err(ActionError::LineNotOwned);
        }
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
/// `Action::InterdictLine` (docs/phase9-spec.md "4. 行動"). Valid only
/// against a line owned entirely by one other faction (both endpoint
/// regions share the same owner, distinct from `faction`) that `faction` is
/// currently at war with — a line straddling two different owners (a
/// contested front) or already fully this faction's own is rejected, the
/// same way `apply_declare_war`/`apply_break_treaty` reject a target that
/// isn't in the state their action assumes. No locality requirement (no
/// need for `faction` to already hold a region near either endpoint):
/// docs/phase9-spec.md's whole case for this layer is that a *route*, not
/// merely a region, is a legitimate strategic target in its own right, so
/// this is deliberately as unconstrained by geography as `ProposeTreaty`
/// already is by it.
fn apply_interdict_line(
    world: &mut World,
    faction: FactionId,
    line: TransportLineId,
) -> Result<(), ActionError> {
    let existing = world.transport_lines.get(line.index()).ok_or(ActionError::InvalidLine)?;
    let owner_a = world.region(world.transport_node(existing.from).region).owner;
    let owner_b = world.region(world.transport_node(existing.to).region).owner;
    if owner_a != owner_b || owner_a == faction {
        return Err(ActionError::LineNotHostile);
    }
    if !world.diplomacy.is_at_war(faction, owner_a) {
        return Err(ActionError::LineNotHostile);
    }

    let next = (existing.condition.get() - LINE_INTERDICTION_DAMAGE).max(0.0);
    world.transport_lines[line.index()].condition =
        Condition::new(next).expect("clamped into 0.0..=1.0 above");
    Ok(())
}

/// `Action::StrikeNode` (docs/phase10-spec.md "3. 阻止": "飛行場と港への攻撃")
/// - `apply_interdict_line`'s node-level twin. Valid only against an
/// `Airfield` or `Port` node (`ActionError::NodeNotStrikeable` otherwise -
/// design.md §8 names ports and airfields, not every transport node, as
/// legitimate strike targets) whose own region is owned by a faction
/// `faction` is currently at war with, distinct from `faction` itself
/// (`ActionError::NodeNotHostile`) - the same "hostile and not your own"
/// shape `apply_interdict_line` checks per line endpoint, collapsed to one
/// region here since a node (unlike a line) has only one. No locality
/// requirement, for the same reason `apply_interdict_line` has none: see
/// `Action::StrikeNode`'s own doc.
///
/// **Air superiority now gates and prices this strike** (design.md §8's own
/// framing - "敵は...物流拠点を攻撃することも可能" - was never meant to read
/// as a free action regardless of who holds the sky over the target):
///
/// - `air::air_superiority_factor` scales `NODE_STRIKE_DAMAGE` itself down
///   to whatever fraction of the target's airspace the defender does *not*
///   hold - contested air degrades the strike, air the defender fully holds
///   lets essentially nothing through, `1.0` (today's undegraded behavior)
///   when nobody contests the sky there at all. Read fresh against `Region::
///   air_superiority` right here, at resolution time, never sampled once
///   when the order was queued (CLAUDE.md「繰り返し踏んだ欠陥」: "発令時点の
///   値を焼き込まない") - the exact same function `air_line_factor` already
///   uses for `InterdictLine`'s throughput throttle, not a second notion of
///   who controls the air.
/// - `air::apply_strike_losses` is the missing counterplay: whatever of
///   `faction`'s own air units can currently reach the target region take
///   losses proportional to that same defender's hostile share, whether or
///   not the strike itself accomplished anything. Sending squadrons into
///   contested skies costs squadrons even on the ticks the bombs miss.
///
/// Applied for *every* strikeable node kind, `Port` included: design.md §8
/// never restricts "物流拠点を攻撃する" to airfield targets, and Phase 10's
/// air force is this action's principal user (10D's own AI, `air_strike_ai`)
/// regardless of whether the node struck happens to be a runway or a quay.
fn apply_strike_node(
    world: &mut World,
    faction: FactionId,
    node: TransportNodeId,
) -> Result<(), ActionError> {
    let existing = world.transport_nodes.get(node.index()).ok_or(ActionError::InvalidNode)?;
    if existing.kind != TransportNodeKind::Airfield && existing.kind != TransportNodeKind::Port {
        return Err(ActionError::NodeNotStrikeable);
    }
    let owner = world.region(existing.region).owner;
    if owner == faction || !world.diplomacy.is_at_war(faction, owner) {
        return Err(ActionError::NodeNotHostile);
    }
    let region = existing.region;

    // Collected *before* anything is mutated: the sortie's own bases are
    // part of what this strike changes (`air::strike_origin_regions`), and
    // after the losses below some of those squadrons may no longer reach.
    let refresh_origins = air::strike_origin_regions(world, region, faction);

    let factor = air::air_superiority_factor(world, region, faction);
    let next = (existing.condition.get() - NODE_STRIKE_DAMAGE * factor).max(0.0);
    world.transport_nodes[node.index()].condition =
        Condition::new(next).expect("clamped into 0.0..=1.0 above");

    // This sortie is judged against the defence it actually flew into -
    // `factor` above was read before the node took any damage, so
    // `1.0 - factor` is that same pre-strike hostile share. Grounding the
    // field does not retroactively spare the bombers that grounded it
    // (`codex review`, P1: refreshing before this line let a strike that
    // faced and beat a full defence take zero losses for it).
    air::apply_strike_losses(world, region, faction, 1.0 - factor);

    // Only now, with both the node's condition and the attacker's own
    // squadrons already mutated, is the cached air picture refreshed.
    //
    // `Region::air_superiority` is a per-tick cache that
    // `air::tick_air_superiority` rebuilds once a day, but a whole batch of
    // actions resolves between two ticks. Grounding this airfield changed
    // who can fly over everything within its reach
    // (`air::units_reaching` refuses to fly from a non-operational node),
    // and the losses just taken changed how much power the attacker still
    // projects. Without this, every later action in the same batch would be
    // judged against the picture from before both - CLAUDE.md's
    // 「発令時点の値を焼き込まない」, one batch deep.
    air::refresh_air_superiority_near(world, &refresh_origins);
    Ok(())
}

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
