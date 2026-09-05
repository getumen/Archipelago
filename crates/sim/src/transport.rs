//! Stage 9A (docs/phase9-spec.md "1. 層の分離"): the transport network,
//! kept as its own layer separate from `Region::links` (troop movement) and
//! `world::World::supply` (still computed by `logistics::recompute_supply`
//! exactly as before). Nothing reads these types yet - Stage 9B is the
//! model swap that makes supply flow over this network instead of
//! `Region::links`. Today this module is pure data plus the type-level
//! invariants `docs/conventions.md` §1 asks for (`Condition`, `Capacity`):
//! everything else about *how* the network behaves (finite capacity
//! contention, `condition` damage/repair) is Stage 9B's job, not this
//! one's.

use crate::ids::{RegionId, TransportNodeId};

/// docs/phase9-spec.md "輸送ノード": what a `TransportNode` is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportNodeKind {
    /// A route junction with no role of its own beyond connecting lines.
    Junction,
    /// Pays out supply to the units stationed in its own region (Stage 9B).
    Depot,
    /// Faces open water. Stage 9A keeps `Region::port` as the sole source of
    /// truth for *how much* port capacity a region has and whether it can be
    /// blockaded (`naval::is_port_blockaded` is unchanged) - see this
    /// module's own doc and `scenario::ScenarioError::PortNodeWithoutRegionPort`
    /// for the one consistency rule Stage 9A enforces between the two: a
    /// `Port` node may only exist for a region that already reports
    /// `port > 0.0`, so the new layer can never claim a port the old one
    /// doesn't also know about. Stage 9B/9C is expected to make this node,
    /// not `Region::port`, the thing blockade actually keys off - at that
    /// point the region-level field either gets removed or becomes a
    /// derived read of the node.
    Port,
}

impl TransportNodeKind {
    /// Lowercase English key used by `scenario`'s JSON schema, the same
    /// `Good::key()`/`Terrain::key()`/`LinkKind::key()` convention.
    pub const fn key(self) -> &'static str {
        match self {
            TransportNodeKind::Junction => "junction",
            TransportNodeKind::Depot => "depot",
            TransportNodeKind::Port => "port",
        }
    }

    pub fn from_key(key: &str) -> Option<TransportNodeKind> {
        match key {
            "junction" => Some(TransportNodeKind::Junction),
            "depot" => Some(TransportNodeKind::Depot),
            "port" => Some(TransportNodeKind::Port),
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
}

/// docs/phase9-spec.md "輸送路線": what a `TransportLine` carries.
/// Deliberately only three variants, distinct from the region-movement
/// `world::LinkKind`'s five - `Tunnel`/`Strait` are movement-layer
/// distinctions about *which region link* a unit can use; in the transport
/// layer a tunnel is simply a (low-capacity) `Rail` line and a strait a
/// `Sea` line between two `Port` nodes (see `tools/transport_network.py`'s
/// module doc for exactly how the three shipped scenarios map one to the
/// other, and why that mapping is what keeps 関門/青函/瀬戸内 meaningful
/// chokepoints in the new layer too).
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
/// itself, not of one direction across it - Stage 9B decides how a
/// direction-specific flow is drawn against that shared capacity each tick;
/// nothing in Stage 9A reads either field at all.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TransportLine {
    pub from: TransportNodeId,
    pub to: TransportNodeId,
    pub kind: TransportLineKind,
    pub capacity: Capacity,
    pub condition: Condition,
}
