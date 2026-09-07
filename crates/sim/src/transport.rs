//! The transport network, kept as its own layer separate from
//! `Region::links` (troop movement only) and `Region`'s own political/
//! economic fields. Stage 9A (docs/phase9-spec.md "1. 層の分離") added the
//! pure data here plus the type-level invariants `docs/conventions.md` §1
//! asks for (`Condition`, `Capacity`). Stage 9B (docs/phase9-spec.md "2. 補
//! 給を有限流量にする") is the model swap that makes `logistics::
//! recompute_supply` route actual, capacity-constrained flow over this
//! network instead of best-path bottleneck reachability over
//! `Region::links`, and adds this module's own two behaviours: `condition`'s
//! damage/repair cycle (`tick_transport_condition`) and a line's actual
//! per-tick ceiling once war damage is folded in (`TransportLine::
//! effective_capacity`). `Region::port` is now purely a capacity *magnitude*
//! (import volume, `Region::value`); every "does this region have a port at
//! all" check goes through `World::port_node`/`has_port_node` instead, so
//! blockade and import eligibility can never disagree with the transport
//! layer about which regions have one (`naval::is_port_blockaded`'s doc).

use crate::balance::{
    INFRA_DAMAGE_SHARE, LINE_CONDITION_DAMAGE_PER_TICK, LINE_CONDITION_REPAIR_PER_TICK,
    NODE_OPERATIONAL_THRESHOLD,
};
use crate::ids::{RegionId, TransportLineId, TransportNodeId};
use crate::world::World;

/// docs/phase9-spec.md "輸送ノード": what a `TransportNode` is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportNodeKind {
    /// A route junction with no role of its own beyond connecting lines.
    Junction,
    /// Pays out supply to the units stationed in its own region (Stage 9B).
    Depot,
    /// Faces open water - the source of truth for *whether* a region has a
    /// port at all (`World::has_port_node`, `naval::is_port_blockaded`'s
    /// doc) and, since Stage 9B, the injection point for that region's
    /// import volume (`Region::port * balance::PORT_SUPPLY_PER_PORT`,
    /// zeroed under blockade). `Region::port` still holds *how much* port
    /// capacity a region has (a magnitude only, never an existence flag) -
    /// `scenario::ScenarioError::PortNodeWithoutRegionPort`/
    /// `RegionPortWithoutPortNode` keep the two from disagreeing about
    /// which regions have a port: a `Port` node may only exist for a region
    /// that reports `port > 0.0`, and such a region must declare exactly
    /// that node.
    Port,
    /// Stage 10A (docs/phase10-spec.md "1. 基地"): the sole authority for
    /// *whether* a region has an airfield at all, and (from this stage on)
    /// the injection point for that airfield's own air-unit demand inside
    /// `compute_transport_flow` (`World::supply_air`, `crate::air`'s own
    /// module doc). Deliberately no `Region::airfield` counterpart the way
    /// `Region::port` shadows `Port` - `Port`'s own doc records that a
    /// magnitude living on both `Region` and the node drifted apart in
    /// practice (`scenario::ScenarioError::PortNodeWithoutRegionPort`/
    /// `RegionPortWithoutPortNode` exist only to police that drift); an
    /// `Airfield` node simply *is* the fact, with nothing on `Region` to
    /// disagree with it. `scenario::Scenario::validate` therefore enforces
    /// no scenario-wide "at least one Airfield" minimum - a scenario with
    /// none is legal, exactly like one with no `Port` node: `Domain::Air`
    /// recruitment just has nowhere to succeed there
    /// (`action::ActionError::NoAirfield`), the same fail-fast shape
    /// `Domain::Sea` recruitment already has against a portless region.
    Airfield,
}

impl TransportNodeKind {
    /// Lowercase English key used by `scenario`'s JSON schema, the same
    /// `Good::key()`/`Terrain::key()`/`LinkKind::key()` convention.
    pub const fn key(self) -> &'static str {
        match self {
            TransportNodeKind::Junction => "junction",
            TransportNodeKind::Depot => "depot",
            TransportNodeKind::Port => "port",
            TransportNodeKind::Airfield => "airfield",
        }
    }

    pub fn from_key(key: &str) -> Option<TransportNodeKind> {
        match key {
            "junction" => Some(TransportNodeKind::Junction),
            "depot" => Some(TransportNodeKind::Depot),
            "port" => Some(TransportNodeKind::Port),
            "airfield" => Some(TransportNodeKind::Airfield),
            _ => None,
        }
    }
}

/// One node of the transport network (docs/phase9-spec.md "輸送ノード").
/// Always belongs to exactly one region (`region`) - a region may own many
/// nodes, or none at all (`scenario::Scenario::validate` never requires the
/// reverse).
#[derive(Clone, Debug)]
pub struct TransportNode {
    pub id: TransportNodeId,
    pub name: String,
    pub kind: TransportNodeKind,
    pub region: RegionId,
    /// Stage 10C (docs/phase10-spec.md "3. 阻止": "飛行場ノードと港ノードを
    /// 叩けること"): this node's own structural health, `TransportLine::
    /// condition`'s exact type and starting value (`Condition::FULL`, never
    /// scenario-authored - a node has no declared starting damage the way a
    /// `TransportLine` occasionally does). Lowered outright by `Action::
    /// StrikeNode` (`action::apply_strike_node`), recovered passively every
    /// tick by `tick_node_condition` below - the same damage/repair split
    /// `TransportLine::condition` already has, one level down. Read only
    /// through `operational` below (`balance::NODE_OPERATIONAL_THRESHOLD`'s
    /// own doc explains why this is a binary gate rather than a graded
    /// capacity multiplier) - never compared directly anywhere else, so
    /// that threshold has exactly one place to live. Both `logistics::
    /// TransportGraph::node_operational` (the supply graph) and `trade::
    /// tick_imports` (a struck port's own import capacity) go through this
    /// one method rather than each re-deriving the comparison, so the two
    /// "is this port working" facts can never drift apart (codex review
    /// P2, Stage 10C: `tick_imports` originally derived import capacity
    /// from `Region::port` alone and ignored this field entirely).
    pub condition: Condition,
}

impl TransportNode {
    /// Whether this node currently relays anything at all. A node carries
    /// no physical `Capacity` of its own (unlike a `TransportLine`) to
    /// scale down gradually, so this is the one binary fact every reader of
    /// `condition` must use instead of comparing against
    /// `NODE_OPERATIONAL_THRESHOLD` itself.
    pub fn operational(&self) -> bool {
        self.condition.get() > NODE_OPERATIONAL_THRESHOLD
    }
}

/// docs/phase9-spec.md "輸送路線": what a `TransportLine` carries.
/// Deliberately only three variants, distinct from the region-movement
/// `world::LinkKind`'s five - `Tunnel`/`Strait` are movement-layer
/// distinctions about *which region link* a unit can use; in the transport
/// layer a tunnel is simply a (low-capacity) `Rail` line and a strait a
/// `Sea` line between two `Port` nodes (see `tools/transport_network.py`'s
/// module doc for exactly how `mvp.json`/`japan47.json` map one to the
/// other, and `tools/hexmap/transport_real.py`'s doc for the same mapping
/// applied to `japan_hex.json`'s Stage 9C real-data-derived network - and
/// why that mapping is what keeps 関門/青函/瀬戸内 meaningful chokepoints in
/// the new layer too).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportLineKind {
    Rail,
    Road,
    /// Open water between two `Port` nodes.
    Sea,
}

impl TransportLineKind {
    pub const fn key(self) -> &'static str {
        match self {
            TransportLineKind::Rail => "rail",
            TransportLineKind::Road => "road",
            TransportLineKind::Sea => "sea",
        }
    }

    pub fn from_key(key: &str) -> Option<TransportLineKind> {
        match key {
            "rail" => Some(TransportLineKind::Rail),
            "road" => Some(TransportLineKind::Road),
            "sea" => Some(TransportLineKind::Sea),
            _ => None,
        }
    }
}

/// A route's health, `0.0..=1.0` (docs/phase9-spec.md "`condition` は 0〜1
/// の健全度"). docs/conventions.md §1 ("ビジネスロジックはなるべく型で実装
/// する... 不正な値をそもそも構築できなくする"): only ever constructed
/// through `new`, so a `Condition` a Stage 9B system holds is proof its
/// value already lies in range - no call site needs to re-clamp or
/// re-check it, the same discipline `world::DominationShare` already
/// applies to a scenario-declared threshold. Stage 9B is what actually
/// *moves* this value over time (war damage lowers it, repair raises it
/// back - docs/phase9-spec.md "回復経路を持つ", one of the standing defect
/// classes CLAUDE.md's「繰り返し踏んだ欠陥」warns against reintroducing);
/// this type only fixes what a legal value looks like, once, regardless of
/// how many places later read or write it.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct Condition(f32);

impl Condition {
    /// A freshly built or fully repaired route.
    pub const FULL: Condition = Condition(1.0);

    pub fn new(value: f32) -> Option<Condition> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Some(Condition(value))
        } else {
            None
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// Upper bound on throughput per tick, before `condition` scales it down -
/// non-negative and finite (docs/phase9-spec.md "輸送路線": "`capacity` は
/// 1 tick に通せる量の上限"). docs/conventions.md §1, the same discipline
/// `Condition` and `world::DominationShare` already apply to their own
/// scenario-declared fields: only ever constructed through `new`, so a
/// negative value (which Stage 9B's `capacity * condition` flow-limit
/// computation would otherwise silently propagate into) can never reach a
/// `TransportLine` at all, regardless of how many places later read it.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct Capacity(f32);

impl Capacity {
    pub fn new(value: f32) -> Option<Capacity> {
        if value.is_finite() && value >= 0.0 {
            Some(Capacity(value))
        } else {
            None
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// One route of the transport network (docs/phase9-spec.md "輸送路線").
/// Undirected: `capacity`/`condition` are properties of the physical route
/// itself, not of one direction across it - `logistics::recompute_supply`
/// draws flow against this same shared ceiling each tick regardless of
/// which direction it travels, so a line whose forward and backward
/// traffic both route genuinely (two independent streams crossing the same
/// trunk) still never carries more than one `effective_capacity` combined
/// (`line_flow_never_doubles_under_two_way_traffic` pins this) - unlike
/// `world::Link`'s declared-per-direction throughput for movement, which
/// *is* a separate full ceiling per direction.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TransportLine {
    /// Stage 9D (docs/phase9-spec.md "4. 行動"): this line's own stable
    /// address, matching its index in `World::transport_lines` - the same
    /// "id equals declaration order" convention `TransportNode::id` already
    /// follows (`ids::TransportLineId`'s own doc).
    pub id: TransportLineId,
    pub from: TransportNodeId,
    pub to: TransportNodeId,
    pub kind: TransportLineKind,
    pub capacity: Capacity,
    pub condition: Condition,
}

impl TransportLine {
    /// Stage 9B (docs/phase9-spec.md "2. 補給を有限流量にする"): this line's
    /// actual per-tick throughput ceiling, before `logistics::
    /// recompute_supply`'s flow allocation ever contends for it -
    /// `capacity * condition`, further attenuated by war damage
    /// (`Region::devastation`) at *both* endpoint regions, the transport-
    /// network counterpart of `Region::effective_infrastructure` reading
    /// through devastation rather than the raw field. A devastated relay
    /// point cripples a line running through it exactly the way it cripples
    /// that region's own production (`devastation_reduces_supply_throughput`
    /// pins this) - reuses `INFRA_DAMAGE_SHARE` rather than a near-duplicate
    /// constant, since it is the same "how much of devastation actually
    /// bites into infrastructure-like throughput" share `Region::
    /// effective_infrastructure` already applies.
    pub fn effective_capacity(&self, world: &World) -> f32 {
        let from_region = world.transport_node(self.from).region;
        let to_region = world.transport_node(self.to).region;
        let health = |r: RegionId| 1.0 - world.region(r).devastation * INFRA_DAMAGE_SHARE;
        (self.capacity.get() * self.condition.get() * health(from_region) * health(to_region)).max(0.0)
    }
}

/// Stage 9B (docs/phase9-spec.md "輸送路線": "戦災・遮断で下がり、回復経路を
/// 持つ"; CLAUDE.md's「繰り返し踏んだ欠陥」: "状態には必ず回復経路を持たせる"):
/// every line touching a currently-contested region (`World::
/// has_enemy_units` true for either endpoint's own region) loses
/// `LINE_CONDITION_DAMAGE_PER_TICK`; every other line recovers
/// `LINE_CONDITION_REPAIR_PER_TICK` back toward `Condition::FULL`. Reads
/// `has_enemy_units` off unit positions as they stood at the top of this
/// tick, the same "snapshot before today's changes" convention
/// `logistics::recompute_supply`'s own `contested` already follows, so a
/// front line that has just been cleared this same tick still counts as
/// contested for today's damage and only starts recovering tomorrow.
///
/// Fixed iteration order (`world.transport_lines`'s own `Vec` order, never a
/// `HashMap`/`HashSet`) and no branch depends on anything but each line's
/// own two endpoint regions, so this is trivially order-independent -
/// nothing here reads or writes any other line's state.
pub fn tick_transport_condition(world: &mut World) {
    let contested: Vec<bool> = world
        .regions
        .iter()
        .map(|r| world.has_enemy_units(r.id, r.owner))
        .collect();

    for i in 0..world.transport_lines.len() {
        let line = world.transport_lines[i];
        let from_region = world.transport_node(line.from).region;
        let to_region = world.transport_node(line.to).region;
        let damaged = contested[from_region.index()] || contested[to_region.index()];
        let delta = if damaged {
            -LINE_CONDITION_DAMAGE_PER_TICK
        } else {
            LINE_CONDITION_REPAIR_PER_TICK
        };
        let next = (line.condition.get() + delta).clamp(0.0, 1.0);
        world.transport_lines[i].condition =
            Condition::new(next).expect("clamped into 0.0..=1.0 above");
    }
}

/// Stage 10C (docs/phase10-spec.md "3. 阻止"): every `TransportNode`'s own
/// `condition` recovers `LINE_CONDITION_REPAIR_PER_TICK` per tick, capped at
/// `Condition::FULL` - the required recovery path (CLAUDE.md「繰り返し踏んだ
/// 欠陥」: "状態には必ず回復経路を持たせる。入ったら出られない状態を作らない"):
/// a struck airfield or port left alone eventually repairs past `balance::
/// NODE_OPERATIONAL_THRESHOLD` and reopens on its own, with no further
/// action required from anyone.
///
/// Deliberately no *damage* branch here, unlike `tick_transport_condition`'s
/// line-level twin: a node only ever loses `condition` through a deliberate
/// `Action::StrikeNode`, never merely by its own region being contested - a
/// region can sit under total enemy air superiority (or ground contact)
/// without any strike ever having been ordered against its airfield/port
/// specifically, and this stage's whole design (docs/phase10-spec.md "3.
/// 阻止") treats striking a node as the deliberate act that makes it a
/// target, not a passive consequence of the front moving near it - passive
/// erosion of *lines* through a contested region is already
/// `tick_transport_condition`'s own job.
///
/// Reuses `LINE_CONDITION_REPAIR_PER_TICK` rather than a second, separately
/// -tuned magnitude: this is the same kind of "0..1 health value with no
/// active damage this tick recovers toward `FULL`" quantity `TransportLine::
/// condition` already models at this exact rate, and conventions ask that an
/// existing, already-measured constant be reused instead of inventing an
/// unmeasured twin for a mechanism that means the same thing.
///
/// Fixed iteration order (`world.transport_nodes`'s own `Vec` order) - no
/// branch reads any other node's state, so this is trivially order-
/// independent.
pub fn tick_node_condition(world: &mut World) {
    for i in 0..world.transport_nodes.len() {
        let next = (world.transport_nodes[i].condition.get() + LINE_CONDITION_REPAIR_PER_TICK).min(1.0);
        world.transport_nodes[i].condition = Condition::new(next).expect("clamped into 0.0..=1.0 above");
    }
}
