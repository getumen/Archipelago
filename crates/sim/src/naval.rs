//! Stage 2D (docs/phase2-spec.md "Stage 2D — 海軍・制海権・海上封鎖"): sea
//! zones, sea control, the three blockade effects (strait throttling, port
//! closure, naval combat), and fleet supply. Deliberately kept as its own
//! module rather than folded into `military`/`logistics`/`trade`: regions
//! and sea zones are different topologies (owned land graph vs. unowned open
//! water), and forcing them through one generic function would cost more in
//! readability than the small amount of structural duplication this module
//! accepts instead. Where the *pool* being split really is shared with land
//! (Munitions/Arms delivery, the national stock they draw from) this module
//! is combined into the same `logistics::distribute_supply` pass rather than
//! drawing on it after the fact — see that function's doc.

use crate::balance::{
    ARMS_SUPPLY_NEED_PER_GAP, BLOCKADE_CONTROL_THRESHOLD, BROKEN_LOSS_MULT, COMBAT_SUPPLY_MULT,
    EQUIPMENT_LOSS_PER_DAMAGE, EXPERIENCE_GAIN_PER_HIT, MANPOWER_LOSS_PER_DAMAGE,
    MORALE_LOSS_PER_BROKEN_HIT, NAVAL_DAMAGE, ORG_DAMAGE_MULT, PROJECTED_SUPPLY_FACTOR,
    SUPPLY_NEED_PER_MANPOWER, SUPPLY_SMOOTHING, UNIT_EQUIPMENT,
};
use crate::event::Event;
use crate::good::Good;
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::military::CombatReport;
use crate::rng::Rng;
use crate::world::World;

/// Recomputes every sea zone's `control` from the `combat_power` of
/// whichever fleets are currently in it (docs/phase2-spec.md "制海権":
/// `control[f] = power[f] / sum(power[*])`, all zero if nobody's there).
/// Must run before anything reads `control` this tick — `Simulation::step`
/// calls it first, from fleet positions as they stood at the end of the
/// previous tick's movement, the same "snapshot before this tick's changes"
/// convention `recompute_supply`'s `contested` and `trade::tick_imports`'s
/// `contested` already follow for land.
pub fn tick_sea_control(world: &mut World) {
    let n_factions = world.factions.len();
    for zone_idx in 0..world.sea_zones.len() {
        let zone_id = SeaZoneId(zone_idx as u32);
        let mut power = vec![0.0f32; n_factions];
        for faction in &world.factions {
            power[faction.id.index()] = world.zone_power(zone_id, faction.id);
        }
        let total: f32 = power.iter().fold(0.0, |acc, &p| acc + p);
        let control = if total > 0.0 {
            power.iter().map(|&p| p / total).collect()
        } else {
            vec![0.0f32; n_factions]
        };
        world.sea_zones[zone_idx].control = control;
    }
}

/// docs/phase2-spec.md "2. 港の封鎖": a port is blockaded when some faction
/// currently at `Stance::War` with its owner holds at least
/// `BLOCKADE_CONTROL_THRESHOLD` control in any sea zone the region faces.
/// Judged per port region, never aggregated — a blockade of one port must
/// never affect another (a region with no `Port` transport node can't be
/// blockaded; every MVP region has one, but the guard keeps the function
/// meaningful on a map that doesn't). External code review fix A1: reads
/// `World::hostile_control_max` rather than the raw `SeaZone::enemy_control_max`
/// - a faction at `Ceasefire`/`NonAggression`/`Alliance` with the owner,
/// however dominant its fleet presence, must not blockade its own treaty
/// partner's port.
///
/// Stage 9B: keyed off `World::has_port_node` rather than `Region::port >
/// 0.0` - the transport layer's `Port` node is now the source of truth for
/// *whether* a region has a port at all (`Region::port`'s own doc); the two
/// can never disagree since `scenario::Scenario::validate` keeps them in
/// lockstep, but this is the one place that fact is actually acted on
/// rather than merely enforced.
pub fn is_port_blockaded(world: &World, region: RegionId) -> bool {
    let owner = world.region(region).owner;
    if !world.has_port_node(region) {
        return false;
    }
    world
        .zones_touching(region)
        .into_iter()
        .any(|z| world.hostile_control_max(z, owner) >= BLOCKADE_CONTROL_THRESHOLD)
}

/// docs/phase2-spec.md "1. 海峡リンクの遮断": the throughput/speed multiplier
/// a `Strait` link crossing sea zone `zone` suffers, from `faction`'s point
/// of view (the faction relaying supply, or moving, across the link) —
/// `1 - ` the highest control any *other* faction holds there. A link with
/// no `strait_zone` (every non-`Strait` link, and the 中国—九州 `Tunnel`
/// deliberately) is unaffected: this function is only ever called for links
/// that do carry one.
pub fn strait_factor(world: &World, zone: SeaZoneId, faction: FactionId) -> f32 {
    (1.0 - world.sea_zone(zone).enemy_control_max(faction)).clamp(0.0, 1.0)
}

/// Stage 9B: the sea-control throttle a `transport::TransportLineKind::Sea`
/// line suffers, mirroring `strait_factor` exactly (`1 -` the highest
/// control any *other* faction than `owner` holds) but derived from the
/// line's own two endpoint regions rather than a stored `strait_zone` - the
/// zone(s) actually crossed are whichever sea zone both regions' coastlines
/// share (`World::zones_touching`), read fresh every call (never cached),
/// so a front that shifts after a route was chosen throttles it the very
/// same tick (CLAUDE.md「繰り返し踏んだ欠陥」: "発令時点の値を焼き込まない").
/// `1.0` (no throttle) if the two regions share no common sea zone at all -
/// every same-owner `Sea` transport line connects two regions that do, in
/// every shipped scenario, but this stays honest about the general case
/// rather than assuming it.
pub fn sea_line_factor(world: &World, region_a: RegionId, region_b: RegionId, owner: FactionId) -> f32 {
    let zones_b = world.zones_touching(region_b);
    world
        .zones_touching(region_a)
        .into_iter()
        .filter(|z| zones_b.contains(z))
        .map(|z| strait_factor(world, z, owner))
        .fold(1.0f32, f32::min)
}

/// docs/phase2-spec.md "3. 海戦": resolves combat in every sea zone held by
/// two or more surviving factions' fleets, mirroring land `tick_combat`'s
/// resolution formula exactly but with `NAVAL_DAMAGE` in place of
/// `COMBAT_DAMAGE` and no terrain defense bonus (sea zones have no
/// terrain) — kept as a separate function from land's rather than a shared
/// generic one, since the "who defends" concept land needs solely to apply
/// its terrain bonus has nothing to attach to here.
///
/// External code review fix A1: stance-aware exactly like land
/// `military::tick_combat` — factions merely sharing a zone isn't enough, at
/// least one *pair* present must actually be at `Stance::War`, and each
/// side's damage dealt/received only ever comes from sides it's actually at
/// war with. Before this fix every other faction's fleet always counted as
/// hostile here regardless of `Stance`, so a `Ceasefire`/`NonAggression`/
/// `Alliance` partner's fleets still fought every tick.
pub fn tick_naval_combat(world: &mut World, rng: &mut Rng, events: &mut Vec<Event>) -> CombatReport {
    let mut fought = vec![false; world.units.len()];
    let mut casualties = vec![0.0f32; world.factions.len()];

    for zone_idx in 0..world.sea_zones.len() {
        let zone_id = SeaZoneId(zone_idx as u32);
        let mut factions_present: Vec<FactionId> =
            world.fleets_in(zone_id).map(|u| u.owner).collect::<Vec<_>>();
        factions_present.sort_by_key(|f| f.0);
        factions_present.dedup();
        if factions_present.len() < 2 {
            continue;
        }
        let any_war = factions_present
            .iter()
            .enumerate()
            .any(|(i, &a)| factions_present[i + 1..].iter().any(|&b| world.diplomacy.is_at_war(a, b)));
        if !any_war {
            continue;
        }

        let power: Vec<f32> = factions_present
            .iter()
            .map(|&f| world.zone_power(zone_id, f))
            .collect();

        let mut battle_casualties = 0.0f32;
        for (side_idx, &side_faction) in factions_present.iter().enumerate() {
            let enemy_power: f32 = factions_present
                .iter()
                .enumerate()
                .filter(|&(other_idx, &other_faction)| {
                    other_idx != side_idx && world.diplomacy.is_at_war(side_faction, other_faction)
                })
                .map(|(other_idx, _)| power[other_idx])
                .sum();
            if enemy_power <= 0.0 {
                continue;
            }
            let dmg_side = enemy_power * NAVAL_DAMAGE * rng.range(0.85, 1.15);
            let side_power = power[side_idx];
            if side_power <= 0.0 {
                continue;
            }

            let unit_ids: Vec<UnitId> = world
                .fleets_in(zone_id)
                .filter(|u| u.owner == side_faction)
                .map(|u| u.id)
                .collect();

            for unit_id in unit_ids {
                fought[unit_id.index()] = true;
                let unit = world.unit_mut(unit_id);
                let raw_power = unit.combat_power();
                let share = raw_power / side_power;
                let dmg = dmg_side * share;

                unit.organization = (unit.organization - dmg * ORG_DAMAGE_MULT).max(0.0);
                let broken = if unit.organization <= 0.0 {
                    BROKEN_LOSS_MULT
                } else {
                    1.0
                };
                let manpower_loss = (dmg * MANPOWER_LOSS_PER_DAMAGE * broken).min(unit.manpower);
                unit.manpower -= manpower_loss;
                unit.equipment =
                    (unit.equipment - dmg * EQUIPMENT_LOSS_PER_DAMAGE * broken).max(0.0);
                unit.morale = (unit.morale - MORALE_LOSS_PER_BROKEN_HIT * broken).max(0.0);
                unit.experience = (unit.experience + EXPERIENCE_GAIN_PER_HIT).min(1.0);

                casualties[side_faction.index()] += manpower_loss;
                battle_casualties += manpower_loss;
                world.faction_mut(side_faction).casualties += manpower_loss;
            }
        }

        events.push(Event::NavalBattle {
            zone: zone_id,
            factions: factions_present.clone(),
            casualties: battle_casualties,
        });
    }

    CombatReport { fought, casualties }
}

/// docs/phase2-spec.md "艦隊": a fleet's best source of supply — the
/// highest `world.supply[region]` among the zone's coastal regions this
/// faction owns, has a port in, and doesn't currently contest with the
/// enemy. `0.0` if no such port faces this zone at all.
fn best_facing_port_supply(world: &World, zone: SeaZoneId, faction: FactionId) -> f32 {
    world
        .sea_zone(zone)
        .coast
        .iter()
        .filter(|&&r| {
            let region = world.region(r);
            region.owner == faction && world.has_port_node(r) && !world.has_enemy_units(r, faction)
        })
        .map(|&r| world.supply[r.index()])
        .fold(0.0f32, f32::max)
}

/// The lowest-id sea zone a region faces, if any — the "home water" a fleet
/// built at that port is launched into (`action::apply_recruit`) and the
/// zone the AI treats as that port's own for naval purposes.
pub fn home_zone(world: &World, region: RegionId) -> Option<SeaZoneId> {
    world.zones_touching(region).into_iter().next()
}

/// Per-zone, per-faction Munitions/Arms demand and available throughput for
/// fleets, mirroring `logistics::distribute_supply`'s per-region arrays but
/// keyed by `SeaZoneId`. Pure (no mutation) — `logistics::distribute_supply`
/// folds the result into the very same national-stock scaling pass land's
/// own demand goes through, rather than letting land spend the shared
/// Munitions stock first and sea take whatever's left (that exact
/// hardcoded-precedence shape is the standing defect class this project
/// keeps finding and fixing).
pub fn fleet_demand_and_avail(world: &World) -> (Vec<Vec<f32>>, Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let n_zones = world.sea_zones.len();
    let n_factions = world.factions.len();

    let mut demand_munitions = vec![vec![0.0f32; n_factions]; n_zones];
    let mut demand_arms = vec![vec![0.0f32; n_factions]; n_zones];
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let Some(zone) = unit.station.sea_zone() else {
            continue;
        };
        let zi = zone.index();
        let f = unit.owner.index();
        let in_combat = world.has_enemy_fleets(zone, unit.owner);
        let mult = if in_combat {
            COMBAT_SUPPLY_MULT
        } else {
            1.0
        };
        demand_munitions[zi][f] += unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult;
        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        demand_arms[zi][f] += equipment_gap * ARMS_SUPPLY_NEED_PER_GAP;
    }

    let mut avail = vec![vec![0.0f32; n_factions]; n_zones];
    for zi in 0..n_zones {
        let zone_id = SeaZoneId(zi as u32);
        for f in 0..n_factions {
            if demand_munitions[zi][f] <= 0.0 && demand_arms[zi][f] <= 0.0 {
                continue;
            }
            let faction_id = FactionId(f as u32);
            avail[zi][f] = best_facing_port_supply(world, zone_id, faction_id) * PROJECTED_SUPPLY_FACTOR;
        }
    }

    (demand_munitions, demand_arms, avail)
}

/// Applies the already-split, already-national-stock-scaled per-zone Arms/
/// Munitions service back onto each fleet's `supply`/`arms_delivery`/
/// `arms_budget`, exactly mirroring `logistics::distribute_supply`'s final
/// per-unit loop for land units.
pub fn apply_fleet_supply(
    world: &mut World,
    served_munitions: &[Vec<f32>],
    demand_munitions: &[Vec<f32>],
    served_arms: &[Vec<f32>],
    demand_arms: &[Vec<f32>],
    scale: &[f32],
) {
    for unit in world.units.iter_mut() {
        if !unit.alive {
            continue;
        }
        let Some(zone) = unit.station.sea_zone() else {
            continue;
        };
        let zi = zone.index();
        let f = unit.owner.index();

        let target_munitions = if demand_munitions[zi][f] > 0.0 {
            served_munitions[zi][f] / demand_munitions[zi][f] * scale[f]
        } else {
            1.0
        };
        unit.supply = (unit.supply + (target_munitions - unit.supply) * SUPPLY_SMOOTHING)
            .clamp(0.0, 1.0);

        let target_arms = if demand_arms[zi][f] > 0.0 {
            (served_arms[zi][f] / demand_arms[zi][f]).min(1.0)
        } else {
            1.0
        };
        unit.arms_delivery = (unit.arms_delivery
            + (target_arms - unit.arms_delivery) * SUPPLY_SMOOTHING)
            .clamp(0.0, 1.0);

        let equipment_gap = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
        unit.arms_budget = equipment_gap * unit.arms_delivery;
        unit.arms_delivery_station = unit.station;
    }
}

/// `logistics::instantaneous_arms_delivery`'s sea-domain counterpart — the
/// same "a unit that moved since the last `distribute_supply` pass can't be
/// trusted to reinforce off a stale cached ratio" fix, for a fleet that has
/// moved to a different zone since the tick's supply pass ran.
/// A blockaded port and the sea zone(s) actually responsible for it - Stage
/// 7C (docs/phase7-spec.md "2. 制海権と封鎖": "どの海域の制海権が原因かを
/// 結ぶ"). `causes` reuses the exact same per-zone threshold check
/// `is_port_blockaded` itself folds together with `.any()`, so this can
/// never disagree with it about *whether* a port is blockaded - only add
/// the "which zone(s)" detail on top.
#[derive(Clone, PartialEq, Debug)]
pub struct PortBlockade {
    pub region: RegionId,
    pub causes: Vec<SeaZoneId>,
}

/// Every currently-blockaded port, in ascending `RegionId` order (fixed
/// iteration order for determinism, though this is read-only display data
/// that never touches `World`).
pub fn blockaded_ports(world: &World) -> Vec<PortBlockade> {
    world
        .regions
        .iter()
        .filter(|r| is_port_blockaded(world, r.id))
        .map(|r| {
            let causes = world
                .zones_touching(r.id)
                .into_iter()
                .filter(|&z| world.hostile_control_max(z, r.owner) >= BLOCKADE_CONTROL_THRESHOLD)
                .collect();
            PortBlockade { region: r.id, causes }
        })
        .collect()
}

pub fn instantaneous_fleet_arms_delivery(world: &World, unit_id: UnitId) -> (f32, f32) {
    let unit = world.unit(unit_id);
    let need_equipment = (UNIT_EQUIPMENT - unit.equipment).max(0.0);
    if need_equipment <= 0.0 {
        return (1.0, 0.0);
    }
    let Some(zone) = unit.station.sea_zone() else {
        return (1.0, 0.0);
    };
    let faction = unit.owner;

    let avail = best_facing_port_supply(world, zone, faction) * PROJECTED_SUPPLY_FACTOR;
    if avail <= 0.0 {
        return (0.0, 0.0);
    }

    let faction_ref = &world.factions[faction.index()];
    let w_munitions = faction_ref.logistics_priority[Good::Munitions.index()].max(0.0);
    let w_arms = faction_ref.logistics_priority[Good::Arms.index()].max(0.0);
    let w_sum = w_munitions + w_arms;
    let share_arms_frac = if w_sum > 0.0 { w_arms / w_sum } else { 0.5 };
    let share_arms = avail * share_arms_frac;

    let demand_arms = need_equipment * ARMS_SUPPLY_NEED_PER_GAP;
    let ratio = (share_arms / demand_arms).min(1.0);
    (ratio, need_equipment * ratio)
}
