//! Supply network: how far production reaches the front, and how it is
//! rationed to units once there. This is design.md §8's core loop — cutting
//! a single corridor region starves everything behind it.
//!
//! Stage 9B (docs/phase9-spec.md "2. 補給を有限流量にする") replaced the
//! previous model - best-path bottleneck *reachability* over `Region::links`,
//! where a link's declared throughput was never actually consumed, so every
//! downstream region behind a shared chokepoint could independently draw up
//! to that chokepoint's full figure as if no sibling were also drawing on it
//! - with a real capacity-constrained *flow* over the transport network
//! (`crate::transport`) added in Stage 9A. `compute_transport_flow` is the
//! whole model; `recompute_supply` just takes its per-region-and-faction
//! delivered amount. See that function's own doc for the algorithm and why
//! it is deterministic and demand-proportional under contention.
//!
//! One formula never got migrated in Stage 9B: `distribute_supply`'s
//! "non-owner" branch, which supplies units standing in territory their own
//! faction does not (yet) own - an occupier mid-invasion, before
//! `military::tick_occupation` flips `Region::owner`. It kept projecting
//! `0.4 * world.supply[some neighbor]`, a formula written for the old
//! ceiling model, against the new *delivered* meaning of `world.supply` -
//! that neighbor is typically a quiet rear region with no units of its own
//! to draw the figure up, so the projection read near-zero no matter how
//! much the network could actually carry through it. Fixed by making an
//! occupier's demand a first-class citizen of `compute_transport_flow`
//! itself (`World::supply_by_faction`, that function's own doc) rather than
//! a downstream guess - see `distribute_supply`'s and
//! `land_unit_supply_avail`'s own doc for the two call sites this reached.
//!
//! `Region::links` (still present, unchanged) is movement-only from this
//! stage on - nothing here reads it any more.

use std::collections::{BinaryHeap, VecDeque};

use crate::balance::{
    ARMS_SUPPLY_NEED_PER_GAP, COMBAT_SUPPLY_MULT, INDUSTRY_SUPPLY_SHARE, PORT_SUPPLY_PER_PORT,
    SUPPLY_FLOW_EPSILON, SUPPLY_FLOW_ROUNDS, SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING,
    UNIT_EQUIPMENT,
};
use crate::good::Good;
use crate::ids::RegionId;
use crate::naval;
use crate::transport::TransportNodeKind;
use crate::world::{LinkKind, Region, World};

/// Per-(region, faction) Munitions/Arms upkeep demand this tick - shared by
/// `compute_transport_flow` (sizes each region's demand *sink*, so the flow
/// model never delivers more than its own units can use) and
/// `distribute_supply` (splits whatever the network actually delivered
/// between the two goods). Extracted so the two never carry two
/// independently-maintained copies of the same per-unit loop that could
/// silently drift apart.
fn region_demand(world: &World) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();
    let mut demand_munitions = vec![vec![0.0f32; n_factions]; n_regions];
    let mut demand_arms = vec![vec![0.0f32; n_factions]; n_regions];
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let Some(region) = unit.station.region() else {
            continue; // fleets are handled by the sea-zone pass below
        };
        let r = region.index();
        let f = unit.owner.index();
        let in_combat = world.has_enemy_units(region, unit.owner);
        let mult = if in_combat { COMBAT_SUPPLY_MULT } else { 1.0 };
        demand_munitions[r][f] += unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult;
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        demand_arms[r][f] += equipment_gap * ARMS_SUPPLY_NEED_PER_GAP;
    }
    (demand_munitions, demand_arms)
}

/// A region's own production injected into the transport network -
/// `balance::INDUSTRY_SUPPLY_SHARE` of `industry_total()`, unchanged in
/// value from the pre-Stage-9B `Region::supply_source_blockaded`'s own term.
fn production_source(region: &Region) -> f32 {
    region.industry_total() * INDUSTRY_SUPPLY_SHARE
}

/// A region's import injected into the transport network at its own `Port`
/// node(s), `0.0` under blockade - `balance::PORT_SUPPLY_PER_PORT` per point
/// of `Region::port`, unchanged in value from the pre-Stage-9B
/// `Region::supply_source_blockaded`'s own term.
fn import_source(region: &Region, blockaded: bool) -> f32 {
    if blockaded {
        0.0
    } else {
        region.port * PORT_SUPPLY_PER_PORT
    }
}

/// Recomputes `world.supply`/`world.supply_by_faction`/`world.port_capacity`:
/// the Munitions+Arms throughput that actually flowed to each region this
/// tick, from `compute_transport_flow` - see that function's own doc for the
/// algorithm - plus the structural port-reachability figure from
/// `compute_port_source_capacity` (that function's own doc, and
/// `World::port_capacity`'s own doc, for why this is a separate computation
/// rather than another reader of `flow.served`). `world.supply[r]` is kept
/// as the region's own owner's entry of `world.supply_by_faction[r]`,
/// unchanged in meaning, so every reader that only ever cared about a
/// region's own delivered amount (JSON/observation export, the API, the
/// headless report) needs no change; the full per-faction matrix exists for
/// `distribute_supply`'s and `land_unit_supply_avail`'s non-owner branches.
pub fn recompute_supply(world: &mut World) {
    let flow = compute_transport_flow(world);
    let port_capacity = compute_port_source_capacity(world);
    world.supply = (0..world.regions.len())
        .map(|r| flow.served[r][world.regions[r].owner.index()])
        .collect();
    world.supply_by_faction = flow.served;
    world.port_capacity = port_capacity;
}

/// One edge of `compute_transport_flow`'s internal graph: either a real
/// `TransportLine` traversal (`line`/`dir`, `dir == true` meaning `line.from
/// -> line.to`) - contending for that line's own `residual_line` budget,
/// *shared* with the opposite-direction edge over the same line, since a
/// `TransportLine` is one physical route (`dir` only ever picks which
/// virtual vertex the edge leads to/from and which half of `line_flow` a
/// commit is credited to - never a separate budget) - or a `Virtual`
/// hub/collector edge, which never has a capacity of its own (the *vertex*
/// it leads to or from is what carries a budget, if any - see
/// `compute_transport_flow`'s doc for why hubs are needed at all).
#[derive(Clone, Copy, Debug)]
enum EdgeKind {
    Line { line: usize, dir: bool },
    Virtual,
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    to: usize,
    kind: EdgeKind,
}

/// The per-(region, faction) delivered amount (`World::supply_by_faction`'s
/// meaning) plus, purely for read-only display reconstruction
/// (`supply_routes`, `supply_link_flows`), every real `TransportLine`'s
/// actual committed flow this tick in each direction and its own effective
/// ceiling.
struct TransportFlow {
    /// `[region][faction]` - see `World::supply_by_faction`'s own doc.
    served: Vec<Vec<f32>>,
    /// `[forward, backward]` committed flow per `world.transport_lines`
    /// index - `forward` is `line.from -> line.to`.
    line_flow: Vec<[f32; 2]>,
    line_capacity: Vec<f32>,
}

/// Stage 9B's flow model (docs/phase9-spec.md "2. 補給を有限流量にする"): a
/// fixed number of synchronous (Jacobi-style) proportional-flow rounds over
/// the transport network.
///
/// ## The graph
///
/// Real vertices are `world.transport_nodes`. Three virtual vertices are
/// added *per region* rather than reading a region's production/import
/// straight onto one of its real nodes, so a region with more than one node
/// of the relevant kind still injects its production/import exactly once,
/// not once per node:
///
/// - `Prod(r)`: budget `production_source(r)`, feeding every `Depot` node
///   `r` owns.
/// - `Import(r)`: budget `import_source(r)` (`0` under blockade), feeding
///   every `Port` node `r` owns.
/// - `Inbound(r)`: budget `Region::node_throughput(r)` - every
///   *cross*-region `TransportLine` arriving at one of `r`'s nodes is
///   rerouted through this collector first (then fans out, unlimited, to
///   every node `r` owns), so `r`'s own absorptive capacity binds
///   regardless of which of its nodes the flow nominally arrives at. A
///   line whose two ends share a region (e.g. a region's own depot<->port
///   line) bypasses this - `Inbound` only ever gates supply crossing *in*
///   from somewhere else, exactly like the pre-Stage-9B node-side cap only
///   ever gated a link's *relayed* contribution, never a region's own
///   `own_source`.
/// - `Demand(r)`: sink, target `region_demand`'s combined Munitions+Arms
///   figure for `r`'s owner - fed by every `Depot` node `r` owns.
///
/// A `TransportLine` becomes two directed edges *sharing one*
/// `effective_capacity` between them (P1 fix: not a separate full ceiling
/// per direction the way `world::Link`'s declared throughput works for
/// movement - a route's physical capacity does not double just because
/// both directions genuinely carry traffic in the same tick) - enabled only
/// when the *source* end's own region is not currently contested
/// (`World::has_enemy_units`), exactly the old model's relay-eligibility
/// rule generalized from region links to transport lines: a contested
/// region still receives whatever reaches it, but never relays onward.
/// *Which* faction(s) may actually traverse it in a given round is a
/// separate, per-hauling-faction question - see "Demand is keyed by
/// (region, faction)" below - no longer baked into this shared adjacency
/// list at all.
///
/// ## Demand is keyed by (region, faction), not region alone
///
/// `region_demand` has always carried every faction with alive units in a
/// region, not just its owner - `distribute_supply`'s per-unit final loop
/// needs that to ration a contested region's *own* units correctly. This
/// function used to collapse it back down to just the owner's row before
/// ever building `remaining_demand`, silently dropping any other faction's
/// entry - which is exactly where an occupier's demand (a faction with alive
/// units physically standing in a region it does not own, mid-invasion,
/// before `military::tick_occupation` flips `Region::owner`) used to fall
/// out of the model entirely. `distribute_supply`'s "non-owner" branch
/// patched over the gap downstream with `0.4 * world.supply[some neighbor]`,
/// which read near-zero whenever that neighbor had no units of its own to
/// draw the figure up, no matter how much the network could actually carry.
///
/// Fixed by keeping `remaining_demand`/`served` indexed by `[region][faction]`
/// end to end, so an occupier's row is just another candidate competing for
/// capacity on the same terms as everyone else's, every round - never a
/// fixed claim by virtue of being an occupier, an id, or an iteration
/// position.
///
/// ## Which lines an occupier may use
///
/// `controlled[r][f]` is `true` when faction `f` may draw on region `r`'s
/// transport nodes for its own network: either `world.region(r).owner == f`
/// (home territory), or `f` has an alive land unit standing in `r` right now
/// (occupied, whether or not `Region::owner` has caught up yet). A line
/// between `ra` and `rb` is usable to haul `f`'s supply, in a given
/// direction, exactly when *both* ends are `controlled[_][f]` -
/// generalizing the old "both ends share one owner" rule (the
/// `f == owner(ra) == owner(rb)` special case of this) to also cover the
/// boundary line an invader's home territory shares with the foreign region
/// it is standing in, and any further line between two regions it has
/// pushed into and holds. It never lets `f` draw on a *third* party's
/// production: `Prod(r)`/`Import(r)` below are only ever seeded from a
/// region whose actual `owner` is `f`, so an occupier reaches the frontier
/// exclusively through its own network's own output, never by helping
/// itself to the production of the region it is standing in and does not
/// own.
///
/// Because eligibility now depends on who is hauling, one shared
/// multi-source BFS can no longer answer "what can everyone reach" - the old
/// model's owner-partitioned subgraphs never overlapped; this one's do,
/// wherever an occupier's home network and the defender's home network both
/// reach toward the same contested region. Each round therefore runs one BFS
/// *per faction* (ascending id - fixed order, never a `HashMap`/`HashSet`),
/// restricted to that faction's own eligible edges, before pooling
/// candidates from every faction's pass into the one shared proportional-
/// scaling step below. For the ordinary case (a faction with nothing
/// occupied) this reproduces exactly the old single combined pass, since
/// `controlled[_][f]` reduces to plain ownership and the per-faction
/// subgraphs are disjoint again; an occupier just gets its own genuine
/// search through its own network for the regions where it is not the
/// owner.
///
/// ## Why this needs rounds at all
///
/// A single greedy pass (find each sink's shortest path, grant it in full,
/// move to the next sink in some order) would let whichever sink is
/// processed first claim a shared bottleneck's entire capacity - a fixed
/// priority decided purely by iteration order, exactly the shape
/// CLAUDE.md's「繰り返し踏んだ欠陥」lists first. Instead, every round:
///
/// 1. One BFS *per hauling faction* (multi-source, from every `Prod`/
///    `Import` vertex of that faction's own regions with remaining budget,
///    over that faction's own eligible edges only) finds, for every
///    under-served (region, faction) pair with that faction as the hauler,
///    its shortest still-usable path to a source - purely a function of
///    *this round's* residual state, recomputed from scratch (no cached
///    routing). Every faction's candidates are pooled into one list before
///    anything below runs.
/// 2. Every such pair's *desired* amount this round is computed from that
///    snapshot (bounded by its own remaining demand and every resource
///    - lines, `Inbound`, the source itself - the path touches).
/// 3. Only once every candidate's desired amount is known does anything
///    commit: for each contended resource, `scale = min(1, remaining_budget
///    / total_desired)`; each candidate's *granted* amount is its desired
///    amount times the smallest scale along its own path. This is what
///    makes an oversubscribed resource split proportional to demand and
///    independent of which candidate happened to be considered first (spec
///    §7 Stage 9B, criterion 3) - independent of region *or* faction -
///    every grant is computed from one shared snapshot, not sequentially
///    against a partially-already-spent one.
/// 4. Every resource's remaining budget only ever decreases by the amount
///    actually granted this round (a real shrinking budget, never a ratio
///    re-applied to a shrinking remainder - the other standing defect shape
///    this project keeps guarding against).
///
/// This never lets committed flow against any one resource exceed its
/// round-start budget: for a resource whose `scale < 1`, the sum of
/// `desired * scale` over every candidate using it is at most `scale *
/// total_desired == remaining_budget` by construction; for `scale == 1` the
/// sum is at most `total_desired <= remaining_budget` since it wasn't
/// oversubscribed. Repeating for `SUPPLY_FLOW_ROUNDS` fixed rounds (never a
/// float convergence test - docs/phase9-spec.md "2. 決定論") lets residual
/// capacity freed by one round's scaling get re-contended by whoever still
/// has unmet demand in the next, converging toward - though, since this
/// never cancels/reroutes already-committed flow the way full augmenting-
/// path max-flow does, not always reaching - the exact max flow. That is an
/// accepted, deliberate trade: an exact max-flow solver would also pick
/// arbitrarily among multiple optimal solutions when several exist, which
/// would violate the demand-proportional requirement this algorithm is
/// built around instead.
/// The static parts of the transport graph `compute_transport_flow` (flow
/// rounds) and `compute_port_source_capacity` (widest-path reachability)
/// both search: vertex adjacency, each line's own effective capacity, and
/// which `(region, faction)` pairs may use a region's transport nodes at all
/// (`controlled` - see `compute_transport_flow`'s own doc, "Which lines an
/// occupier may use"). Built once by `build_transport_graph` and shared by
/// both callers so they can never silently drift into two independently
/// -maintained copies of the same graph.
struct TransportGraph {
    n_nodes: usize,
    n_regions: usize,
    adj: Vec<Vec<Edge>>,
    line_capacity: Vec<f32>,
    line_regions: Vec<(usize, usize)>,
    controlled: Vec<Vec<bool>>,
}

impl TransportGraph {
    fn prod(&self, r: usize) -> usize {
        self.n_nodes + r
    }
    fn import(&self, r: usize) -> usize {
        self.n_nodes + self.n_regions + r
    }
    fn inbound(&self, r: usize) -> usize {
        self.n_nodes + 2 * self.n_regions + r
    }
    fn demand(&self, r: usize) -> usize {
        self.n_nodes + 3 * self.n_regions + r
    }
    fn is_inbound(&self, v: usize) -> bool {
        (self.n_nodes + 2 * self.n_regions..self.n_nodes + 3 * self.n_regions).contains(&v)
    }
    fn n_vertices(&self) -> usize {
        self.n_nodes + 4 * self.n_regions
    }
    /// `f` may traverse `line`'s edge only when both of its regions are its
    /// own network - home territory or somewhere it physically occupies (see
    /// `compute_transport_flow`'s own doc, "Which lines an occupier may
    /// use").
    fn line_eligible_for(&self, line: usize, f: usize) -> bool {
        let (ra, rb) = self.line_regions[line];
        self.controlled[ra][f] && self.controlled[rb][f]
    }
}

fn build_transport_graph(world: &World) -> TransportGraph {
    let n_nodes = world.transport_nodes.len();
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();
    let n_vertices = n_nodes + 4 * n_regions;

    let inbound = |r: usize| n_nodes + 2 * n_regions + r;
    let prod = |r: usize| n_nodes + r;
    let import = |r: usize| n_nodes + n_regions + r;
    let demand = |r: usize| n_nodes + 3 * n_regions + r;

    let contested: Vec<bool> = world.regions.iter().map(|r| world.has_enemy_units(r.id, r.owner)).collect();

    // `controlled[r][f]` - see this function's own doc, "Which lines an
    // occupier may use". Fixed order (`world.regions` then `world.units`,
    // both plain `Vec`s, never a `HashMap`/`HashSet`).
    let mut controlled = vec![vec![false; n_factions]; n_regions];
    for region in &world.regions {
        controlled[region.id.index()][region.owner.index()] = true;
    }
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        if let Some(region) = unit.station.region() {
            controlled[region.index()][unit.owner.index()] = true;
        }
    }

    // Fixed-order adjacency build: every push below iterates a `Vec` in its
    // own stored order (`world.transport_lines`, then `world.transport_nodes`
    // twice) - never a `HashMap`/`HashSet`, so BFS neighbor order is a pure
    // function of world state.
    let mut adj: Vec<Vec<Edge>> = vec![Vec::new(); n_vertices];
    // Stage 9B: a `TransportLineKind::Sea` line additionally suffers
    // `naval::sea_line_factor` - the transport-network counterpart of the
    // old region-`Strait`-link throttle (docs/phase2-spec.md "1. 海峡リンク
    // の遮断"). A `Rail`/`Road` line (including the 中国—九州 corridor,
    // deliberately modeled as a low-capacity `Rail` rather than a `Sea`
    // line - `transport`'s own module doc) is never touched by this at all,
    // which is exactly what keeps it immune to sea control by construction
    // (`kanmon_tunnel_survives_blockade`) rather than by a special case.
    let line_capacity: Vec<f32> = world
        .transport_lines
        .iter()
        .map(|line| {
            let base = line.effective_capacity(world);
            if line.kind == crate::transport::TransportLineKind::Sea {
                let ra = world.transport_node(line.from).region;
                let rb = world.transport_node(line.to).region;
                let owner = world.region(ra).owner;
                base * naval::sea_line_factor(world, ra, rb, owner)
            } else {
                base
            }
        })
        .collect();

    // `(ra, rb)` region index per line, looked up once - read by every BFS
    // below (`TransportGraph::line_eligible_for`) rather than re-derived
    // from `world.transport_node` on every traversal.
    let line_regions: Vec<(usize, usize)> = world
        .transport_lines
        .iter()
        .map(|line| (world.transport_node(line.from).region.index(), world.transport_node(line.to).region.index()))
        .collect();

    for (i, &(ra, rb)) in line_regions.iter().enumerate() {
        let line = &world.transport_lines[i];
        let from_node = line.from.index();
        let to_node = line.to.index();
        // Stage 9B fix: no longer pre-filtered to "both ends share one
        // owner" here - a line whose ends belong to two different factions
        // is still structurally added (`contested` is the only gate that
        // belongs in this shared, faction-independent adjacency list);
        // *whether* it is actually usable for a given hauling faction is
        // answered fresh by `line_eligible_for`, inside that faction's own
        // BFS below.
        if !contested[ra] {
            let target = if ra == rb { to_node } else { inbound(rb) };
            adj[from_node].push(Edge { to: target, kind: EdgeKind::Line { line: i, dir: true } });
        }
        if !contested[rb] {
            let target = if ra == rb { from_node } else { inbound(ra) };
            adj[to_node].push(Edge { to: target, kind: EdgeKind::Line { line: i, dir: false } });
        }
    }
    for (n_idx, node) in world.transport_nodes.iter().enumerate() {
        adj[inbound(node.region.index())].push(Edge { to: n_idx, kind: EdgeKind::Virtual });
    }
    for (n_idx, node) in world.transport_nodes.iter().enumerate() {
        let r = node.region.index();
        match node.kind {
            TransportNodeKind::Depot => {
                adj[prod(r)].push(Edge { to: n_idx, kind: EdgeKind::Virtual });
                adj[n_idx].push(Edge { to: demand(r), kind: EdgeKind::Virtual });
            }
            TransportNodeKind::Port => {
                adj[import(r)].push(Edge { to: n_idx, kind: EdgeKind::Virtual });
            }
            TransportNodeKind::Junction => {}
        }
    }

    TransportGraph { n_nodes, n_regions, adj, line_capacity, line_regions, controlled }
}

fn compute_transport_flow(world: &World) -> TransportFlow {
    let n_regions = world.regions.len();
    let n_lines = world.transport_lines.len();
    let n_factions = world.factions.len();

    let graph = build_transport_graph(world);
    let n_vertices = graph.n_vertices();
    let prod = |r: usize| graph.prod(r);
    let import = |r: usize| graph.import(r);
    let inbound = |r: usize| graph.inbound(r);
    let demand = |r: usize| graph.demand(r);
    let is_inbound = |v: usize| graph.is_inbound(v);
    let line_eligible_for = |line: usize, f: usize| graph.line_eligible_for(line, f);
    let adj = &graph.adj;
    let line_capacity = graph.line_capacity.clone();

    let blockaded: Vec<bool> = world.regions.iter().map(|r| naval::is_port_blockaded(world, r.id)).collect();

    let mut vertex_budget = vec![0.0f32; n_vertices];
    for r in 0..n_regions {
        let region = &world.regions[r];
        vertex_budget[prod(r)] = production_source(region);
        vertex_budget[import(r)] = import_source(region, blockaded[r]);
        vertex_budget[inbound(r)] = region.node_throughput();
    }
    // P1 fix: one shared budget per *undirected* line, not `[c, c]` (a full
    // `c` for each direction independently) - `TransportLine` is a single
    // physical route (its own doc: "`capacity`/`condition` are properties
    // of the physical route itself, not of one direction across it"), so a
    // tick's *combined* forward+backward flow must never exceed its own
    // `effective_capacity`, exactly like `docs/phase9-spec.md` §2's
    // `制約 各路線の capacity * condition を超えて流せない` - `[c, c]` let
    // supply genuinely routed both ways in the same tick (two independent
    // streams crossing the same trunk, e.g.
    // `tests::line_flow_never_doubles_under_two_way_traffic`) carry up to
    // `2 * capacity * condition`, silently doubling the one
    // resource this whole phase exists to make finite. Opposing directions
    // now draw down the *same* residual and are arbitrated exactly like any
    // other contended resource in this algorithm (`total_desired_line`/
    // `scale_line` below pool both directions' desired amount before
    // scaling, never a fixed priority for whichever direction is "forward").
    let mut residual_line: Vec<f32> = line_capacity.clone();

    // Per-(region, faction) demand - generalized from the pre-fix
    // region-only array, which collapsed every row down to just the
    // region's own owner before this point and silently dropped every other
    // faction's entry (this function's own doc, "Demand is keyed by
    // (region, faction), not region alone").
    let (demand_munitions, demand_arms) = region_demand(world);
    let mut remaining_demand: Vec<Vec<f32>> = (0..n_regions)
        .map(|r| (0..n_factions).map(|f| demand_munitions[r][f] + demand_arms[r][f]).collect())
        .collect();
    let mut served = vec![vec![0.0f32; n_factions]; n_regions];
    let mut line_flow_accum = vec![[0.0f32; 2]; n_lines];

    struct Candidate {
        region: usize,
        faction: usize,
        source_vertex: usize,
        lines: Vec<(usize, bool)>,
        inbounds: Vec<usize>,
        desired: f32,
    }

    // Reused across every faction's BFS, every round, rather than
    // reallocated `n_factions * SUPPLY_FLOW_ROUNDS` times - this function's
    // own doc already promises this whole pass is "cheap - a fixed pass,
    // not a search."
    let mut visited = vec![false; n_vertices];
    let mut parent: Vec<Option<(usize, EdgeKind)>> = vec![None; n_vertices];
    let mut queue: VecDeque<usize> = VecDeque::new();

    for _round in 0..SUPPLY_FLOW_ROUNDS {
        let mut candidates: Vec<Candidate> = Vec::new();

        // --- 1+2. one BFS per hauling faction (ascending id - fixed order),
        // each restricted to that faction's own eligible edges
        // (`line_eligible_for`) over this round's shared residual state,
        // gathering that faction's under-served, now-reachable (region,
        // faction) candidates. See this function's own doc, "Which lines an
        // occupier may use", for why this can no longer be one combined
        // multi-source BFS the way it was before this fix. ---
        for f in 0..n_factions {
            for v in visited.iter_mut() {
                *v = false;
            }
            for p in parent.iter_mut() {
                *p = None;
            }
            queue.clear();
            for r in 0..n_regions {
                if world.regions[r].owner.index() == f
                    && vertex_budget[prod(r)] > SUPPLY_FLOW_EPSILON
                    && !visited[prod(r)]
                {
                    visited[prod(r)] = true;
                    queue.push_back(prod(r));
                }
            }
            for r in 0..n_regions {
                if world.regions[r].owner.index() == f
                    && vertex_budget[import(r)] > SUPPLY_FLOW_EPSILON
                    && !visited[import(r)]
                {
                    visited[import(r)] = true;
                    queue.push_back(import(r));
                }
            }
            while let Some(u) = queue.pop_front() {
                for edge in &adj[u] {
                    let usable = match edge.kind {
                        // Shared budget (reachability doesn't depend on
                        // which direction this edge is traversed in -
                        // either direction draws down the same
                        // `residual_line`), *and* this line must actually be
                        // eligible for the faction whose BFS this is.
                        EdgeKind::Line { line, .. } => {
                            residual_line[line] > SUPPLY_FLOW_EPSILON && line_eligible_for(line, f)
                        }
                        EdgeKind::Virtual => true,
                    };
                    if !usable {
                        continue;
                    }
                    let v = edge.to;
                    if is_inbound(v) && vertex_budget[v] <= SUPPLY_FLOW_EPSILON {
                        continue;
                    }
                    if visited[v] {
                        continue;
                    }
                    visited[v] = true;
                    parent[v] = Some((u, edge.kind));
                    queue.push_back(v);
                }
            }

            for r in 0..n_regions {
                if remaining_demand[r][f] <= SUPPLY_FLOW_EPSILON {
                    continue;
                }
                let sink = demand(r);
                if !visited[sink] {
                    continue;
                }
                let mut cur = sink;
                let mut lines = Vec::new();
                let mut inbounds = Vec::new();
                while let Some((p, kind)) = parent[cur] {
                    if let EdgeKind::Line { line, dir } = kind {
                        lines.push((line, dir));
                    }
                    if is_inbound(p) {
                        inbounds.push(p);
                    }
                    cur = p;
                }
                let source_vertex = cur;
                // Deliberately *not* pre-capped by any resource's own current
                // budget/residual here - only by this candidate's own
                // remaining demand, the one thing that is never shared with
                // another candidate. Capping against a *shared* resource's
                // budget before the round's aggregate `total_desired`/`scale`
                // step below would make a large demand look artificially
                // small next to a tiny one sharing the same bottleneck (its
                // "desired" would already have been clipped down to the
                // resource's own size), biasing the split toward whichever
                // side happened to have the smaller demand instead of
                // splitting by the true demand ratio - exactly the "fixed
                // priority over a scarce resource" shape this stage exists
                // to avoid, just smuggled in through demand size instead of
                // id/iteration order. Leaving `desired` as the raw remaining
                // demand and letting `scale` (computed from the *true*
                // `total_desired` across every candidate sharing a resource)
                // do 100% of the throttling is what makes the split
                // proportional to demand regardless of how lopsided it is -
                // and, since candidates here span every hauling faction, not
                // just an occupier's own.
                let desired = remaining_demand[r][f];
                if desired <= SUPPLY_FLOW_EPSILON {
                    continue;
                }
                candidates.push(Candidate { region: r, faction: f, source_vertex, lines, inbounds, desired });
            }
        }
        if candidates.is_empty() {
            continue;
        }
        // Canonical `(region, faction)` order, not the `(faction, region)`
        // order the per-faction passes above happened to produce it in -
        // step 3's `total_desired_vertex`/`total_desired_line` sums are
        // order-sensitive float accumulation (never associative), so this
        // fixes one single deterministic order regardless of how many
        // factions have occupier candidates this round, and - for a region
        // with only its own owner's candidate, the ordinary case - matches
        // the single ascending-region order the pre-fix single-pass model
        // always summed in, isolating any behavioural difference to where
        // an occupier's candidate genuinely adds a new term rather than to
        // incidental reordering of terms that were already there.
        candidates.sort_by_key(|c| (c.region, c.faction));

        // --- 3. proportional scale per contended resource, from one shared snapshot ---
        let mut total_desired_vertex = vec![0.0f32; n_vertices];
        // P1 fix: pooled across *both* directions of a line - a candidate
        // crossing forward and one crossing backward in the same round are
        // contending for the same physical capacity, and must be scaled
        // down together (never a fixed priority for either direction) the
        // same way two candidates sharing a vertex already are.
        let mut total_desired_line = vec![0.0f32; n_lines];
        for c in &candidates {
            total_desired_vertex[c.source_vertex] += c.desired;
            for &ib in &c.inbounds {
                total_desired_vertex[ib] += c.desired;
            }
            for &(line, _dir) in &c.lines {
                total_desired_line[line] += c.desired;
            }
        }
        let scale_vertex = |v: usize, budget: &[f32]| -> f32 {
            let td = total_desired_vertex[v];
            if td > SUPPLY_FLOW_EPSILON {
                (budget[v] / td).min(1.0)
            } else {
                1.0
            }
        };
        let scale_line = |line: usize, residual: &[f32]| -> f32 {
            let td = total_desired_line[line];
            if td > SUPPLY_FLOW_EPSILON {
                (residual[line] / td).min(1.0)
            } else {
                1.0
            }
        };

        // --- 4a. every candidate's `granted` amount, computed purely from
        // the round-start `vertex_budget`/`residual_line` snapshot above -
        // deliberately a separate pass from 4b's commit below, computing
        // every `scale` *before* any of them are applied. Folding compute
        // and commit into one pass over `candidates` would let whichever
        // candidate happens to come first in the `Vec` partially spend a
        // shared resource before a later candidate's own `scale` is even
        // computed against it - a sequential read-then-write race against
        // the very state `scale_vertex`/`scale_line` read, and exactly the
        // Gauss-Seidel order-dependence this stage's whole design is meant
        // to avoid (`oversubscribed_allocation_is_proportional_and_order_
        // independent` pins this: it failed with `granted` computed and
        // applied in the same loop, because a later candidate's `scale_line`
        // read the earlier candidate's already-decremented `residual_line`
        // instead of the shared round-start value every candidate must see).
        let granted: Vec<f32> = candidates
            .iter()
            .map(|c| {
                let mut scale = scale_vertex(c.source_vertex, &vertex_budget);
                for &ib in &c.inbounds {
                    scale = scale.min(scale_vertex(ib, &vertex_budget));
                }
                for &(line, _dir) in &c.lines {
                    scale = scale.min(scale_line(line, &residual_line));
                }
                c.desired * scale
            })
            .collect();

        // --- 4b. commit: every resource shrinks by exactly what was granted ---
        for (c, &granted) in candidates.iter().zip(granted.iter()) {
            if granted <= 0.0 {
                continue;
            }
            served[c.region][c.faction] += granted;
            remaining_demand[c.region][c.faction] -= granted;
            vertex_budget[c.source_vertex] -= granted;
            for &ib in &c.inbounds {
                vertex_budget[ib] -= granted;
            }
            for &(line, dir) in &c.lines {
                // The shared budget shrinks regardless of which direction
                // consumed it; `line_flow_accum` keeps the per-direction
                // split, purely for `supply_routes`/`supply_link_flows`'s
                // read-only reconstruction below.
                residual_line[line] -= granted;
                let idx = usize::from(!dir);
                line_flow_accum[line][idx] += granted;
            }
        }
    }

    TransportFlow { served, line_flow: line_flow_accum, line_capacity }
}

/// `World::port_capacity`'s own computation: for every region with a `Port`
/// transport node and every faction, the widest-path (max-min-capacity)
/// source capacity the transport network structurally offers that port -
/// the largest bottleneck over any usable path from that faction's own
/// `Prod`/`Import` sources, entirely independent of `region_demand` (this
/// module's own top-of-file doc, and `World::port_capacity`'s own doc, for
/// why `compute_transport_flow`'s demand-bounded `served` cannot answer this
/// question: a region with no demand candidate never gets one, regardless of
/// how much capacity would reach it if asked).
///
/// Shares `build_transport_graph` with `compute_transport_flow` (same
/// `line_capacity`/eligibility/`controlled`), but solves a different
/// problem on it: a widest path (maximum bottleneck path) rather than a
/// contended multi-commodity flow, since nothing here is rationed against a
/// competing demand - there is deliberately no demand term in this
/// computation at all. Every edge weight (a line's capacity, or an
/// unbounded `Virtual` hop) is non-negative, so - exactly like Dijkstra's
/// shortest-path algorithm - a greatest-bottleneck-first priority queue
/// finalizes each vertex's optimal value the moment it is first popped and
/// never needs to revisit it: termination is "the queue is empty", a
/// well-ordered structural fact, never a float-magnitude comparison against
/// an epsilon (docs/phase9-spec.md "2. 決定論"'s "打ち切り条件を反復回数で
/// 固定する" is about the *other* algorithm in this module,
/// `compute_transport_flow`'s contended proportional-flow rounds, which
/// cannot terminate this way since a round's outcome truly does depend on
/// every candidate sharing a resource, not a single monotone best-first
/// order). `MaxOrd` breaks ties in the priority queue by `f32::total_cmp`
/// (never `NaN` here - every capacity is a finite, non-negative float or
/// `f32::INFINITY`) so the heap's internal order is a pure function of
/// `World` state, not of insertion timing.
fn compute_port_source_capacity(world: &World) -> Vec<Vec<f32>> {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let graph = build_transport_graph(world);
    let n_vertices = graph.n_vertices();

    let blockaded: Vec<bool> = world.regions.iter().map(|r| naval::is_port_blockaded(world, r.id)).collect();
    let mut vertex_budget = vec![0.0f32; n_vertices];
    for r in 0..n_regions {
        let region = &world.regions[r];
        vertex_budget[graph.prod(r)] = production_source(region);
        vertex_budget[graph.import(r)] = import_source(region, blockaded[r]);
        vertex_budget[graph.inbound(r)] = region.node_throughput();
    }

    // Every `Port` node's own vertex index, grouped by region - `result`
    // below only ever reports a value for these, but the widest-path search
    // itself runs over the full graph (a port's own capacity can be relayed
    // through any node, not only reached directly from `Import`).
    let mut port_nodes_by_region: Vec<Vec<usize>> = vec![Vec::new(); n_regions];
    for (n_idx, node) in world.transport_nodes.iter().enumerate() {
        if node.kind == TransportNodeKind::Port {
            port_nodes_by_region[node.region.index()].push(n_idx);
        }
    }
    if port_nodes_by_region.iter().all(Vec::is_empty) {
        return vec![vec![0.0f32; n_factions]; n_regions];
    }

    let mut result = vec![vec![0.0f32; n_factions]; n_regions];
    for f in 0..n_factions {
        let mut widest = vec![0.0f32; n_vertices];
        let mut finalized = vec![false; n_vertices];
        let mut heap: BinaryHeap<(MaxOrd, usize)> = BinaryHeap::new();
        for r in 0..n_regions {
            if world.regions[r].owner.index() != f {
                continue;
            }
            for v in [graph.prod(r), graph.import(r)] {
                if vertex_budget[v] > 0.0 {
                    widest[v] = vertex_budget[v];
                    heap.push((MaxOrd(vertex_budget[v]), v));
                }
            }
        }
        while let Some((MaxOrd(cap), u)) = heap.pop() {
            if finalized[u] || cap < widest[u] {
                continue; // a stale, since-superseded queue entry
            }
            finalized[u] = true;
            for edge in &graph.adj[u] {
                let line_cap = match edge.kind {
                    EdgeKind::Line { line, .. } => {
                        if !graph.line_eligible_for(line, f) {
                            continue;
                        }
                        graph.line_capacity[line]
                    }
                    EdgeKind::Virtual => f32::INFINITY,
                };
                let mut candidate = widest[u].min(line_cap);
                if graph.is_inbound(edge.to) {
                    candidate = candidate.min(vertex_budget[edge.to]);
                }
                if candidate > widest[edge.to] {
                    widest[edge.to] = candidate;
                    heap.push((MaxOrd(candidate), edge.to));
                }
            }
        }
        for (r, nodes) in port_nodes_by_region.iter().enumerate() {
            result[r][f] = nodes.iter().map(|&n_idx| widest[n_idx]).fold(0.0f32, f32::max);
        }
    }
    result
}

/// A total order over `f32` for `compute_port_source_capacity`'s
/// greatest-first `BinaryHeap` - `f32::total_cmp` rather than
/// `partial_cmp().unwrap()` so this can never panic, though every value it
/// is ever built from (a capacity, or `f32::INFINITY`) is already never
/// `NaN`.
#[derive(Clone, Copy, PartialEq, Debug)]
struct MaxOrd(f32);

impl Eq for MaxOrd {}

impl PartialOrd for MaxOrd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MaxOrd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Rations each faction's stockpiled Munitions across its units, region by
/// region, and eases every unit's `supply` ratio toward its funded target.
///
/// Stage 2C (docs/phase2-spec.md "3. 品目別の到達率") widens this from
/// Munitions alone to Munitions *and* Arms delivery, contending for the same
/// regional throughput (`avail`, from `world.supply`) rather than each
/// getting their own. The split is controlled by
/// `Faction::logistics_priority[Munitions]/[Arms]` — never a hardcoded
/// order, per the standing rule against giving one consumer a fixed first
/// claim on a shared resource. Within a region/faction cell, each good's
/// priority-weighted share is a first pass; whatever either good's share
/// goes unused (because its own demand is smaller than its share — Arms
/// demand is usually zero outside active reinforcement) is then handed to
/// the other in proportion to its remaining unmet demand, so idle Arms
/// throughput doesn't sit ring-fenced away from Munitions units that could
/// actually use it, and vice versa.
///
/// Munitions delivery still drains `Faction::stock[Munitions]` exactly as
/// before (`unit.supply`, the arrival ratio units consume upkeep from). Arms
/// delivery does *not* touch `Faction::stock[Arms]` here — it only computes
/// `unit.arms_delivery`, the arrival-rate ceiling `action::apply_reinforce`
/// applies on top of its own separate stock check, so a faction with a full
/// armory still can't instantly re-equip a unit the network can't reach.
///
/// Stage 2D (docs/phase2-spec.md "艦隊の補給"): fleets draw on the exact
/// same national `Faction::stock[Munitions]` pool land units do, so their
/// demand (`naval::fleet_demand_and_avail`, per sea zone rather than per
/// region) is folded into this same pass and shares the same
/// stock-limited `scale[f]` — computed once, from land's and sea's combined
/// `total_served`/`total_demand` — before either domain's stock is
/// deducted. Running the sea pass afterward against whatever land left in
/// stock would give land an unconditional first claim on the shared pool,
/// exactly the hardcoded-precedence defect shape this project keeps finding
/// and fixing (see this function's own doc above for the land-side version
/// of the same rule).
///
/// Stage 9B: `demand_munitions`/`demand_arms` now come from the same
/// `region_demand` helper `compute_transport_flow` sizes its sinks from,
/// rather than an independently-maintained copy of the same per-unit loop.
/// `avail[r][f]` for the owning faction is `world.supply[r]` exactly as
/// before — only that value's *meaning* changed (Stage 9B's transport flow,
/// already demand-bounded, rather than a pure network ceiling).
///
/// The non-owner case — units caught in territory their own faction does
/// not (yet) own, an occupier mid-invasion — used to fall back to a
/// downstream projection, `0.4 * world.supply[some neighbor]`, guessing at
/// what the network might carry rather than asking it: that neighbor is
/// typically a quiet rear region with no units of its own to draw the
/// figure up, so the guess read near-zero regardless of how much capacity
/// actually reached the frontier (this module's own top-of-file doc has the
/// measured consequence). Fixed by giving that demand a real seat in
/// `compute_transport_flow` itself — `World::supply_by_faction[r][f]` is
/// exactly `avail[r][f]` for a non-owner `f` now, no separate formula at
/// all: it already went through the identical proportional-scaling rounds
/// every other candidate did, competing for the same capacity on the same
/// terms, `compute_transport_flow`'s own doc has the full account.
pub fn distribute_supply(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let (demand_munitions, demand_arms) = region_demand(world);

    let mut avail = vec![vec![0.0f32; n_factions]; n_regions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            if demand_munitions[r][f] <= 0.0 && demand_arms[r][f] <= 0.0 {
                continue;
            }
            avail[r][f] = if world.regions[r].owner.index() == f {
                world.supply[r]
            } else {
                world.supply_by_faction[r][f]
            };
        }
    }

    let mut served_munitions = vec![vec![0.0f32; n_factions]; n_regions];
    let mut served_arms = vec![vec![0.0f32; n_factions]; n_regions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            if avail[r][f] <= 0.0 {
                continue;
            }
            let faction = &world.factions[f];
            let w_munitions = faction.logistics_priority[Good::Munitions.index()];
            let w_arms = faction.logistics_priority[Good::Arms.index()];
            let (m, a) =
                split_munitions_arms(avail[r][f], w_munitions, w_arms, demand_munitions[r][f], demand_arms[r][f]);
            served_munitions[r][f] = m;
            served_arms[r][f] = a;
        }
    }

    // Stage 2D: the sea-zone counterpart of the region-indexed arrays above,
    // split by the same priority weights via the same shared helper.
    let (demand_munitions_zone, demand_arms_zone, avail_zone) = naval::fleet_demand_and_avail(world);
    let n_zones = world.sea_zones.len();
    let mut served_munitions_zone = vec![vec![0.0f32; n_factions]; n_zones];
    let mut served_arms_zone = vec![vec![0.0f32; n_factions]; n_zones];
    for z in 0..n_zones {
        for f in 0..n_factions {
            if avail_zone[z][f] <= 0.0 {
                continue;
            }
            let faction = &world.factions[f];
            let w_munitions = faction.logistics_priority[Good::Munitions.index()];
            let w_arms = faction.logistics_priority[Good::Arms.index()];
            let (m, a) = split_munitions_arms(
                avail_zone[z][f],
                w_munitions,
                w_arms,
                demand_munitions_zone[z][f],
                demand_arms_zone[z][f],
            );
            served_munitions_zone[z][f] = m;
            served_arms_zone[z][f] = a;
        }
    }

    let mut total_served = vec![0.0f32; n_factions];
    let mut total_demand = vec![0.0f32; n_factions];
    for r in 0..n_regions {
        for f in 0..n_factions {
            total_served[f] += served_munitions[r][f];
            total_demand[f] += demand_munitions[r][f];
        }
    }
    for z in 0..n_zones {
        for f in 0..n_factions {
            total_served[f] += served_munitions_zone[z][f];
            total_demand[f] += demand_munitions_zone[z][f];
        }
    }

    let mut scale = vec![1.0f32; n_factions];
    for faction in world.factions.iter_mut() {
        let f = faction.id.index();
        let munitions = faction.stock[Good::Munitions.index()];
        scale[f] = if total_served[f] > 0.0 {
            (munitions / total_served[f]).min(1.0)
        } else {
            1.0
        };
        faction.stock[Good::Munitions.index()] = (munitions - total_served[f] * scale[f]).max(0.0);
        faction.supply_ratio = if total_demand[f] > 0.0 {
            (total_served[f] * scale[f] / total_demand[f]).min(1.0)
        } else {
            1.0
        };
    }

    for unit in world.units.iter_mut() {
        if !unit.alive {
            continue;
        }
        let Some(region) = unit.station.region() else {
            continue; // fleets are finished off by naval::apply_fleet_supply below
        };
        let r = region.index();
        let f = unit.owner.index();
        let target_munitions = if demand_munitions[r][f] > 0.0 {
            served_munitions[r][f] / demand_munitions[r][f] * scale[f]
        } else {
            1.0
        };
        unit.supply =
            (unit.supply + (target_munitions - unit.supply) * SUPPLY_SMOOTHING).clamp(0.0, 1.0);

        let target_arms = if demand_arms[r][f] > 0.0 {
            (served_arms[r][f] / demand_arms[r][f]).min(1.0)
        } else {
            1.0
        };
        unit.arms_delivery = (unit.arms_delivery + (target_arms - unit.arms_delivery) * SUPPLY_SMOOTHING)
            .clamp(0.0, 1.0);

        // External code review fix (Stage 2C): reset the unit's per-tick
        // Arms delivery *budget* here, once, from scratch - the absolute
        // equipment `arms_delivery`'s freshly-eased ratio allows against
        // *this instant's* gap, not accumulated or rolled over from
        // before. `action::apply_reinforce` spends this down as it
        // delivers equipment; stamping `arms_delivery_station` alongside it
        // records which station this budget was computed for, so a unit
        // that moves before its next `ReinforceUnit` action is detected as
        // stale and recomputed on the spot instead of trusted -
        // `logistics::instantaneous_arms_delivery`.
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        unit.arms_budget = equipment_gap * unit.arms_delivery;
        unit.arms_delivery_station = unit.station;
    }

    naval::apply_fleet_supply(
        world,
        &served_munitions_zone,
        &demand_munitions_zone,
        &served_arms_zone,
        &demand_arms_zone,
        &scale,
    );
}

/// Splits `total_avail` throughput between Munitions and Arms demand by
/// `logistics_priority` weight (falling back to an even split if both
/// weights are zero), then hands back to the other good whatever share
/// either good's priority left unused, in proportion to its own remaining
/// unmet demand — so a good with no demand this tick doesn't ring-fence
/// throughput a hungry demand could actually use, and neither good ever
/// gets a hardcoded first claim on the leftover. Shared by `distribute_supply`'s
/// land (per-region) and sea (per-zone, via `naval::fleet_demand_and_avail`)
/// passes — the pool being split is the same contention in both cases, only
/// the place it's keyed by differs.
fn split_munitions_arms(
    total_avail: f32,
    w_munitions: f32,
    w_arms: f32,
    demand_munitions: f32,
    demand_arms: f32,
) -> (f32, f32) {
    let w_munitions = w_munitions.max(0.0);
    let w_arms = w_arms.max(0.0);
    let w_sum = w_munitions + w_arms;
    let (share_munitions_frac, share_arms_frac) = if w_sum > 0.0 {
        (w_munitions / w_sum, w_arms / w_sum)
    } else {
        (0.5, 0.5)
    };
    let share_munitions = total_avail * share_munitions_frac;
    let share_arms = total_avail * share_arms_frac;

    let served_m1 = demand_munitions.min(share_munitions);
    let served_a1 = demand_arms.min(share_arms);

    let leftover = (share_munitions - served_m1) + (share_arms - served_a1);
    let remaining_m = demand_munitions - served_m1;
    let remaining_a = demand_arms - served_a1;
    let remaining_total = remaining_m + remaining_a;
    let (extra_m, extra_a) = if remaining_total > 0.0 && leftover > 0.0 {
        let extra = leftover.min(remaining_total);
        (extra * remaining_m / remaining_total, extra * remaining_a / remaining_total)
    } else {
        (0.0, 0.0)
    };

    (served_m1 + extra_m, served_a1 + extra_a)
}

/// External code review fix (Stage 2C): recomputes a single land unit's Arms
/// delivery ratio and per-tick budget from its *current* region, for
/// `action::apply_reinforce` to call when the unit's cached
/// `Unit::arms_delivery_station` no longer matches `Unit::station` -
/// `distribute_supply` above only runs once a tick, before movement, so a
/// unit that has since moved carries numbers stamped from a region it has
/// already left. Land only — `apply_reinforce` calls
/// `naval::instantaneous_fleet_arms_delivery` instead for a fleet.
///
/// Mirrors `distribute_supply`'s per-region Arms throughput share, but
/// treats `unit`'s own equipment gap as the *only* Arms demand in its
/// region rather than re-running the full multi-unit contention pass
/// above. That is exact when no other same-faction unit shares the region,
/// and otherwise a deliberately conservative simplification kept simple on
/// purpose: it is a single self-contained calculation for one unit at
/// action-apply time, not a second copy of the batched, multi-unit
/// leftover-redistribution logic above. It never grants more of the
/// region's throughput than a from-scratch pass would give this unit
/// alone, and the national Arms stock check in `apply_reinforce` still
/// bounds what actually leaves the armory regardless.
///
/// The transport network's current delivery reaching `unit`'s own land
/// station, for its own faction - `0.0` if nothing at all currently reaches
/// it. Extracted from `instantaneous_arms_delivery` (below) so
/// `action::apply_reinforce` can ask this same reachability question for
/// *manpower* replenishment too (that function's own doc has the full
/// account of why manpower needs it) - independent of whether the unit
/// happens to have an equipment gap at all, unlike
/// `instantaneous_arms_delivery` itself, which never reaches this
/// computation once `need_equipment <= 0`. Land only, mirroring that
/// function's own scope; see `naval::fleet_unit_supply_avail` for the
/// sea-domain counterpart.
///
/// The non-owner case (an occupier standing in territory its faction does
/// not own) used to fall back to the same `0.4 * a neighbor's world.supply`
/// projection `distribute_supply`'s own non-owner branch did — see that
/// function's own doc for why that read near-zero regardless of the
/// network's real capacity. Fixed the same way: `World::supply_by_faction`
/// already carries this exact (region, faction) figure, computed by the
/// same flow `distribute_supply` itself reads, so both call sites now agree
/// by construction rather than by two independently-maintained copies of
/// the same formula. The owner case still reads `world.supply[r]` directly,
/// exactly as before this fix, rather than `world.supply_by_faction[r][owner]`
/// (always numerically the same after a real `recompute_supply` - `World`'s
/// own doc - but tests that drive `world.supply` directly without going
/// through a full tick must keep working unchanged).
pub fn land_unit_supply_avail(world: &World, unit_id: crate::ids::UnitId) -> f32 {
    let unit = world.unit(unit_id);
    let region = unit
        .station
        .region()
        .expect("land_unit_supply_avail is land-only; callers must route fleets to naval::fleet_unit_supply_avail");
    let r = region.index();
    let faction = unit.owner;

    if world.regions[r].owner == faction {
        world.supply[r]
    } else {
        world.supply_by_faction[r][faction.index()]
    }
}

/// Returns `(ratio, budget)`: `ratio` in `0..=1`, `budget` the absolute
/// equipment units deliverable this instant (`gap * ratio`).
pub fn instantaneous_arms_delivery(world: &World, unit_id: crate::ids::UnitId) -> (f32, f32) {
    let unit = world.unit(unit_id);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
    if need_equipment <= 0.0 {
        return (1.0, 0.0);
    }

    let avail = land_unit_supply_avail(world, unit_id);
    if avail <= 0.0 {
        return (0.0, 0.0);
    }

    let faction_ref = &world.factions[unit.owner.index()];
    let w_munitions = faction_ref.logistics_priority[Good::Munitions.index()].max(0.0);
    let w_arms = faction_ref.logistics_priority[Good::Arms.index()].max(0.0);
    let w_sum = w_munitions + w_arms;
    let share_arms_frac = if w_sum > 0.0 { w_arms / w_sum } else { 0.5 };
    let share_arms = avail * share_arms_frac;

    let demand_arms = need_equipment * ARMS_SUPPLY_NEED_PER_GAP;
    let ratio = (share_arms / demand_arms).min(1.0);
    (ratio, need_equipment * ratio)
}

// ---------------------------------------------------------------------
// Stage 7C (docs/phase7-spec.md "1. 補給網の可視化"): read-only display
// support. Nothing below this line is ever called from `Simulation::step`
// or any tick system, and nothing here writes to `World`. Stage 9B: both
// functions now re-run `compute_transport_flow` fresh (it is a pure
// function of `World`) rather than re-deriving their answer purely from
// `world.supply`, since - unlike the old monotone-relaxation model - a
// flow computed this way cannot be reconstructed from its own totals alone.
// ---------------------------------------------------------------------

/// A same-owner line's actual relayed throughput this tick against its own
/// ceiling (`TransportLine::effective_capacity`). `flow` is clamped into
/// `0.0..=capacity` at construction, so this type can never represent
/// "flowing more than its own cap allows" - `ratio`/`is_saturated` are
/// derived from the two stored numbers, never cached separately, so they
/// can't drift out of sync with them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LinkThroughput {
    flow: f32,
    capacity: f32,
}

/// How close to its own ceiling counts as "stuck there" (docs/phase7-spec.md
/// "1.": "上限に張り付いているリンクを明示する") - guards against float
/// noise in the flow allocation's own arithmetic, not a design threshold of
/// its own.
const SATURATION_EPSILON: f32 = 1e-3;

impl LinkThroughput {
    fn new(flow: f32, capacity: f32) -> Self {
        let capacity = capacity.max(0.0);
        LinkThroughput { flow: flow.clamp(0.0, capacity), capacity }
    }

    pub fn flow(self) -> f32 {
        self.flow
    }

    pub fn capacity(self) -> f32 {
        self.capacity
    }

    /// `flow / capacity`, `0.0` for a link with no capacity at all.
    pub fn ratio(self) -> f32 {
        if self.capacity > 0.0 {
            self.flow / self.capacity
        } else {
            0.0
        }
    }

    /// A chokepoint: this link's flow has reached its own ceiling, so
    /// widening whatever's upstream of it would do nothing - the ceiling
    /// itself is what's holding supply back.
    pub fn is_saturated(self) -> bool {
        self.capacity > 0.0 && self.flow >= self.capacity - SATURATION_EPSILON
    }
}

/// Which same-owner neighbor region (if any) a region's delivered supply is
/// mostly attributable to this tick - the "供給がどの経路で来ているか"
/// docs/phase7-spec.md asks the overlay to reconstruct, adapted for Stage
/// 9B: reconstructed from `compute_transport_flow`'s own committed
/// cross-region line flow, grouped by the neighbor region on the other end,
/// rather than replayed from a cached path (there is none to cache — see
/// `compute_transport_flow`'s doc, recomputed fresh every call).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SupplySource {
    /// This region's own production/import base is at least as large as any
    /// single neighbor's committed relay into it this tick.
    Own,
    /// This neighbor's committed relay into the region is what its
    /// delivered supply is mostly attributable to.
    Relay(RegionId),
}

/// One region's reconstructed place in the supply network, alongside the
/// raw `contested`/`blockaded` reads that shape it - a region that can't
/// currently relay onward (`contested`) still shows here with whatever it
/// receives.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SupplyRegionRoute {
    pub region: RegionId,
    /// `world.supply[region]` - repeated here so a caller can read
    /// everything about one region off a single value.
    pub cap: f32,
    pub contested: bool,
    pub blockaded: bool,
    pub own_source: f32,
    pub source: SupplySource,
}

/// Reconstructs every region's `SupplyRegionRoute`, from a fresh
/// `compute_transport_flow` run - see this module's own doc for why this
/// (unlike the pre-Stage-9B version) has to recompute the flow rather than
/// merely re-read `world.supply`.
pub fn supply_routes(world: &World) -> Vec<SupplyRegionRoute> {
    let flow = compute_transport_flow(world);
    let n = world.regions.len();
    let contested: Vec<bool> = (0..n).map(|i| world.has_enemy_units(world.regions[i].id, world.regions[i].owner)).collect();
    let blockaded: Vec<bool> = (0..n).map(|i| naval::is_port_blockaded(world, world.regions[i].id)).collect();

    // Every cross-region line's committed flow, grouped by the region it
    // fed - fixed order (ascending `world.transport_lines` index, then
    // ascending contributing-region id via the sort below), never a
    // `HashMap`/`HashSet`.
    let mut incoming: Vec<Vec<(RegionId, f32)>> = vec![Vec::new(); n];
    for (i, line) in world.transport_lines.iter().enumerate() {
        let ra = world.transport_node(line.from).region;
        let rb = world.transport_node(line.to).region;
        if ra == rb {
            continue;
        }
        let [fwd, bwd] = flow.line_flow[i];
        if fwd > 0.0 {
            incoming[rb.index()].push((ra, fwd));
        }
        if bwd > 0.0 {
            incoming[ra.index()].push((rb, bwd));
        }
    }

    (0..n)
        .map(|j| {
            let region = &world.regions[j];
            let own_source = production_source(region) + import_source(region, blockaded[j]);

            let mut by_neighbor = incoming[j].clone();
            by_neighbor.sort_by_key(|(r, _)| r.0);
            let mut merged: Vec<(RegionId, f32)> = Vec::new();
            for (r, v) in by_neighbor {
                if let Some(entry) = merged.iter_mut().find(|(er, _)| *er == r) {
                    entry.1 += v;
                } else {
                    merged.push((r, v));
                }
            }
            let best = merged.into_iter().fold(None::<(RegionId, f32)>, |acc, (r, v)| match acc {
                Some((br, bv)) if bv >= v => Some((br, bv)),
                _ => Some((r, v)),
            });

            let source = match best {
                Some((r, v)) if v > own_source => SupplySource::Relay(r),
                _ => SupplySource::Own,
            };

            SupplyRegionRoute {
                region: RegionId(j as u32),
                cap: flow.served[j][world.regions[j].owner.index()],
                contested: contested[j],
                blockaded: blockaded[j],
                own_source,
                source,
            }
        })
        .collect()
}

/// One directed, cross-region line's throughput this tick, aggregated to
/// region granularity (several `TransportLine`s can connect the same two
/// regions - e.g. a `Rail` line between their depots and a `Sea` line
/// between their ports - and are summed here, matching `supply_routes`'s
/// own per-neighbor aggregation) - `from` is the relaying source region,
/// `to` the region it feeds. `kind` is read off `Region::links` for display
/// only (`world::LinkKind`, distinct from `transport::TransportLineKind`)
/// and is `None` when no region link happens to connect the same pair -
/// purely cosmetic, never read by anything that affects simulation outcome.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SupplyLinkFlow {
    pub from: RegionId,
    pub to: RegionId,
    pub kind: Option<LinkKind>,
    pub throughput: LinkThroughput,
}

/// Every cross-region pair with at least one committed or capacity-bearing
/// transport line between them this tick, aggregated exactly as
/// `SupplyLinkFlow`'s own doc describes. Ascending `(from, to)` order
/// (`RegionId`'s own `Ord`), never a `HashMap`/`HashSet`.
/// Stage 9D (docs/phase9-spec.md "4. 観測ベクトル"): every `TransportLine`'s
/// own current committed flow this tick, `[forward + backward]` summed into
/// one magnitude per `world.transport_lines` index (fixed order, matching
/// `ids::TransportLineId`) - what `Observation::encode()` reports per line
/// alongside its `capacity`/`condition`. Unlike `supply_routes`/
/// `supply_link_flows` above, this is *not* aggregated to region-pair
/// granularity: an RL agent reasoning about the network layer itself needs
/// the individual route's own number, not several routes between the same
/// two regions folded together. Re-runs `compute_transport_flow` fresh, for
/// the same reason those two functions already do (this module's own doc).
pub fn transport_line_flows(world: &World) -> Vec<f32> {
    compute_transport_flow(world).line_flow.iter().map(|&[fwd, bwd]| fwd + bwd).collect()
}

pub fn supply_link_flows(world: &World) -> Vec<SupplyLinkFlow> {
    let flow = compute_transport_flow(world);
    let mut pairs: std::collections::BTreeMap<(RegionId, RegionId), (f32, f32)> = std::collections::BTreeMap::new();
    for (i, line) in world.transport_lines.iter().enumerate() {
        let ra = world.transport_node(line.from).region;
        let rb = world.transport_node(line.to).region;
        if ra == rb {
            continue;
        }
        let cap = flow.line_capacity[i];
        let [fwd, bwd] = flow.line_flow[i];
        let entry_fwd = pairs.entry((ra, rb)).or_insert((0.0, 0.0));
        entry_fwd.0 += fwd;
        entry_fwd.1 += cap;
        let entry_bwd = pairs.entry((rb, ra)).or_insert((0.0, 0.0));
        entry_bwd.0 += bwd;
        entry_bwd.1 += cap;
    }

    pairs
        .into_iter()
        .map(|((from, to), (flow_sum, cap_sum))| SupplyLinkFlow {
            from,
            to,
            kind: world.link_between(from, to).map(|l| l.kind),
            throughput: LinkThroughput::new(flow_sum, cap_sum),
        })
        .collect()
}
