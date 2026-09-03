//! Stage 2C sea imports (docs/phase2-spec.md "Stage 2C — 品目別物流・港湾容量・
//! 海上輸入", "1. 海上輸入") and Stage 3B `Treaty::TradeAgreement`/
//! `Treaty::PortAccess` (docs/phase3-spec.md "Stage 3B — 外交関係と条約"), all
//! resolved together in `tick_imports` because they share the same scarce
//! resource: an importing faction's own port capacity.
//!
//! The Stage 2A playtest found that a faction holding the nation's
//! industrial heartland cannot feed its own population at any efficiency -
//! the islands' urban core is not food self-sufficient, and until Stage 2C
//! there was no way to bring food in from outside the map. Stage 3B adds a
//! second source: an allied faction's own surplus, moved directly rather
//! than through the abstract world market.
//!
//! Runs before `economy::tick_economy` in `Simulation::step`, so imported/
//! traded Food/Energy/Machinery is in stock in time for that same day's
//! civilian ration. World-market imports are paid for out of *yesterday's*
//! Machinery stock; `TradeAgreement` flows cost nothing but the exporter's
//! own stock (design.md §5's "経済圏を構築する" is meant to be a real,
//! mutually profitable strategy, not a taxed one).
//!
//! Two independent contention points are resolved by ratio, never by a
//! fixed precedence (docs/phase3-spec.md §0 / balance.rs's Stage 3B
//! section):
//! 1. **Exporter side**: if a `TradeAgreement` exporter has multiple
//!    partners simultaneously drawing on the same good, their combined
//!    claim is capped at what that exporter can actually spare and every
//!    claim on it is scaled down by the same ratio if it doesn't fit.
//! 2. **Importer side**: an importer's world-market desire (Stage 2C's
//!    `import_plan`) and its `TradeAgreement` inflow (after step 1's
//!    scaling) both draw on the *same* port-capacity pool
//!    (`Treaty::PortAccess` can extend that pool with a partner's own
//!    ports). If the two wants together exceed capacity, both are scaled
//!    down by the same ratio - world-market imports are never served first
//!    and TradeAgreement given only the leftover, or vice versa.
//!
//! Capacity is kept per port region throughout, never collapsed into one
//! national number — Stage 2D blockades individual ports, and only a
//! per-port figure can express that a blockade of one port doesn't touch
//! another. `TradeAgreement` flows aren't tied to a specific port (they
//! aren't apportioned into any region's `import_flow`) - only the capacity
//! *ceiling* they share with world-market imports is per-port; the goods
//! themselves move faction-to-faction.

use crate::balance::{
    FOCUS_ECONOMIC_TRADE_FLOW_MULT, FOCUS_MARITIME_IMPORT_CAPACITY_MULT,
    IMPORT_COST_MACHINERY_PER_GOOD, IMPORT_PER_PORT, TRADE_FLOW_RATE_MAX,
    TRADE_SURPLUS_RESERVE_FRACTION,
};
use crate::diplomacy::Treaty;
use crate::focus::{self, NationalFocus};
use crate::good::Good;
use crate::ids::FactionId;
use crate::naval;
use crate::world::{Faction, World};

/// The three tradeable civilian goods `Treaty::TradeAgreement` moves - the
/// same three `Faction::shortage_by_good` tracks (Stage 2C's external code
/// review fix), since those are the only goods with a per-commodity deficit
/// signal to drive a "who's short" decision from.
const TRADE_GOODS: [Good; 3] = [Good::Food, Good::Energy, Good::Machinery];

/// One (importer, exporter, good) `TradeAgreement` claim, before and after
/// the two ratio-scaling passes `tick_imports` applies.
struct TradeFlow {
    importer: usize,
    exporter: usize,
    good: Good,
    amount: f32,
}

/// `NationalFocus::EconomicSphere`'s `TradeAgreement`-flow multiplier for
/// `faction` (see `tick_imports`'s call site doc): `FOCUS_ECONOMIC_
/// TRADE_FLOW_MULT` while the focus is active (post-transition; see
/// `focus::active`), `1.0` otherwise.
fn economic_sphere_mult(faction: &Faction) -> f32 {
    if focus::active(faction) == Some(NationalFocus::EconomicSphere) {
        FOCUS_ECONOMIC_TRADE_FLOW_MULT
    } else {
        1.0
    }
}

pub fn tick_imports(world: &mut World) {
    let n_regions = world.regions.len();
    let n_factions = world.factions.len();

    let contested: Vec<bool> = (0..n_regions)
        .map(|i| {
            let region = &world.regions[i];
            world.has_enemy_units(region.id, region.owner)
        })
        .collect();

    for region in world.regions.iter_mut() {
        region.import_flow = 0.0;
    }

    // Step 1: per-port capacity (own, uncontested, unblockaded regions
    // only), and each faction's own total across its own ports.
    let mut port_capacity = vec![0.0f32; n_regions];
    let mut own_capacity = vec![0.0f32; n_factions];
    for i in 0..n_regions {
        // Stage 2D (docs/phase2-spec.md "2. 港の封鎖"): a blockaded port
        // imports nothing, independent of (and in addition to) land contest
        // — judged per port, so a blockade of one port never touches
        // another's `import_flow`.
        if contested[i] || naval::is_port_blockaded(world, world.regions[i].id) {
            continue;
        }
        let region = &world.regions[i];
        // Stage 3C `NationalFocus::MaritimeTrade` (docs/phase3-spec.md: "港
        // 湾の輸入容量＋"): the owner's own ports rate higher while this
        // focus is active - a `Treaty::PortAccess` grantee drawing on those
        // ports benefits too, since it's genuinely a better port, not a
        // per-recipient discount.
        let maritime_mult = if focus::active(&world.factions[region.owner.index()])
            == Some(NationalFocus::MaritimeTrade)
        {
            FOCUS_MARITIME_IMPORT_CAPACITY_MULT
        } else {
            1.0
        };
        let cap = region.port * IMPORT_PER_PORT * (1.0 - region.devastation) * maritime_mult;
        if cap > 0.0 {
            port_capacity[i] = cap;
            own_capacity[region.owner.index()] += cap;
        }
    }

    // Stage 3B `Treaty::PortAccess` (docs/phase3-spec.md "PortAccess"):
    // every other faction that has granted this one port access extends its
    // usable capacity with that faction's own (already contest/blockade
    // filtered) port total. This is capacity the grantee draws on *in
    // addition to*, not instead of, the grantor's own use of the same rated
    // figure - modelling true multi-tenant port throughput is out of scope
    // here (see this module's doc).
    let mut total_capacity = own_capacity.clone();
    for f in 0..n_factions {
        let grantee = FactionId(f as u32);
        for g in 0..n_factions {
            if g == f {
                continue;
            }
            let grantor = FactionId(g as u32);
            if world.diplomacy.has_treaty(grantor, grantee, Treaty::PortAccess) {
                total_capacity[f] += own_capacity[g];
            }
        }
    }

    // Step 2 (Stage 3B contention point 1, "exporter side"): every
    // TradeAgreement claim, each individually capped by that exporter's own
    // spare stock (`TRADE_SURPLUS_RESERVE_FRACTION` reserved for itself),
    // in fixed (importer, exporter, good) order for determinism.
    let mut flows: Vec<TradeFlow> = Vec::new();
    for imp in 0..n_factions {
        if !world.factions[imp].alive {
            continue;
        }
        let importer_id = FactionId(imp as u32);
        for exp in 0..n_factions {
            if exp == imp || !world.factions[exp].alive {
                continue;
            }
            let exporter_id = FactionId(exp as u32);
            if !world.diplomacy.has_treaty(importer_id, exporter_id, Treaty::TradeAgreement) {
                continue;
            }
            for &good in &TRADE_GOODS {
                let deficit = world.factions[imp].shortage_by_good[good.index()].clamp(0.0, 1.0);
                if deficit <= 0.0 {
                    continue;
                }
                let surplus = (world.factions[exp].stock[good.index()]
                    * (1.0 - TRADE_SURPLUS_RESERVE_FRACTION))
                    .max(0.0);
                // Stage 3C `NationalFocus::EconomicSphere` (docs/phase3-
                // spec.md: "TradeAgreement の流量＋"): either side having this
                // focus active grows the flow ceiling - the higher of the
                // two multipliers, so a partnership with one economically-
                // focused side is never double-counted when both are.
                let flow_mult = economic_sphere_mult(&world.factions[imp])
                    .max(economic_sphere_mult(&world.factions[exp]));
                let want = (TRADE_FLOW_RATE_MAX * flow_mult * deficit).min(surplus);
                if want > 0.0 {
                    flows.push(TradeFlow { importer: imp, exporter: exp, good, amount: want });
                }
            }
        }
    }

    // If several importers draw on the same exporter's same good at once,
    // their combined claim can exceed what step 2 individually allowed for
    // (each claim was capped against the exporter's *undiminished* stock) -
    // scale every claim on that (exporter, good) down by the same ratio
    // rather than serving whichever importer happened to be processed
    // first.
    for exp in 0..n_factions {
        for &good in &TRADE_GOODS {
            let surplus = (world.factions[exp].stock[good.index()]
                * (1.0 - TRADE_SURPLUS_RESERVE_FRACTION))
                .max(0.0);
            let total_claim: f32 = flows
                .iter()
                .filter(|f| f.exporter == exp && f.good == good)
                .map(|f| f.amount)
                .sum();
            if total_claim > surplus && total_claim > 0.0 {
                let scale = surplus / total_claim;
                for f in flows.iter_mut().filter(|f| f.exporter == exp && f.good == good) {
                    f.amount *= scale;
                }
            }
        }
    }

    // Step 3 (Stage 3B contention point 2, "importer side"): each
    // importer's world-market want (Stage 2C, already capped by Machinery
    // affordability) and its now exporter-fair-shared TradeAgreement want
    // share the same port-capacity ceiling - summed and scaled together,
    // never one served in full before the other sees what's left.
    for f in 0..n_factions {
        if !world.factions[f].alive || total_capacity[f] <= 0.0 {
            continue;
        }

        let desired_food = world.factions[f].import_plan[Good::Food.index()].max(0.0);
        let desired_energy = world.factions[f].import_plan[Good::Energy.index()].max(0.0);
        let world_desired = desired_food + desired_energy;
        let machinery_stock = world.factions[f].stock[Good::Machinery.index()];
        let affordable = if IMPORT_COST_MACHINERY_PER_GOOD > 0.0 {
            machinery_stock / IMPORT_COST_MACHINERY_PER_GOOD
        } else {
            f32::INFINITY
        };
        let world_want = world_desired.min(affordable).max(0.0);

        let trade_want: f32 = flows.iter().filter(|fl| fl.importer == f).map(|fl| fl.amount).sum();

        let total_want = world_want + trade_want;
        if total_want <= 0.0 {
            continue;
        }
        let capacity_scale = if total_want > total_capacity[f] {
            total_capacity[f] / total_want
        } else {
            1.0
        };

        let world_actual = world_want * capacity_scale;
        if world_actual > 0.0 && world_desired > 0.0 {
            let good_split = world_actual / world_desired;
            let actual_food = desired_food * good_split;
            let actual_energy = desired_energy * good_split;

            world.factions[f].stock[Good::Food.index()] += actual_food;
            world.factions[f].stock[Good::Energy.index()] += actual_energy;
            world.factions[f].stock[Good::Machinery.index()] =
                (world.factions[f].stock[Good::Machinery.index()]
                    - world_actual * IMPORT_COST_MACHINERY_PER_GOOD)
                    .max(0.0);

            // Step 4 (unchanged from Stage 2C): apportion the world-market
            // flow across this faction's *own* ports by each port's share
            // of its own capacity - a PortAccess-granted foreign port's
            // contribution isn't attributed to any single region's
            // `import_flow` (see this module's doc).
            if own_capacity[f] > 0.0 {
                for i in 0..n_regions {
                    if world.regions[i].owner.index() == f && port_capacity[i] > 0.0 {
                        world.regions[i].import_flow =
                            world_actual * (port_capacity[i] / own_capacity[f]);
                    }
                }
            }
        }

        for flow in flows.iter().filter(|fl| fl.importer == f) {
            let actual = flow.amount * capacity_scale;
            if actual <= 0.0 {
                continue;
            }
            world.factions[flow.importer].stock[flow.good.index()] += actual;
            world.factions[flow.exporter].stock[flow.good.index()] -= actual;
        }
    }
}
