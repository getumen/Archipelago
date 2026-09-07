//! Units, their movement across the map, and the combat/recovery/occupation
//! systems that resolve what happens to them each tick.

use crate::balance::{
    ATTRITION_MANPOWER, ATTRITION_ORG, ATTRITION_SUPPLY_THRESHOLD, BROKEN_LOSS_MULT,
    CAPTURE_UNREST, COMBAT_DAMAGE, DEVASTATION_ON_CAPTURE, DEVASTATION_PER_COMBAT_DAMAGE,
    EQUIPMENT_LOSS_PER_DAMAGE, EXPERIENCE_GAIN_PER_HIT, FLEET_MOVE_DAYS,
    FOCUS_DEFENSIVE_HOME_DEFENSE_MULT, FOCUS_DEFENSIVE_OFFENSE_PENALTY_MULT,
    FOCUS_MILITARY_ORG_CAP_MULT, MANPOWER_LOSS_PER_DAMAGE, MORALE_LOSS_PER_BROKEN_HIT, MORALE_REGEN,
    MUTINY_ORG_REGEN_MULT, OCCUPATION_DECAY, OCCUPATION_RATE, ORG_DAMAGE_MULT, ORG_MARCH_DRAIN,
    ORG_REGEN, STRAIT_CROSSING_FACTOR_FLOOR, UNIT_DEATH_MANPOWER, UNIT_EQUIPMENT, UNIT_MANPOWER,
    UNIT_ORG, WAR_SUPPORT_CAPTURE_GAIN, WAR_SUPPORT_LOSS_PENALTY,
};
use crate::diplomacy::Treaty;
use crate::event::Event;
use crate::focus::{self, NationalFocus};
use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::naval;
use crate::rng::Rng;
use crate::world::{OccupationKind, Region, Station, Terrain, World};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Movement {
    pub from: Station,
    pub to: Station,
    pub progress: f32,
    /// Control-independent travel cost only — link travel days × terrain ×
    /// the hostile-destination multiplier for a land move, or
    /// `fleet_move_required`'s equivalent for a fleet. Must never bake in a
    /// sea-control factor: control is recomputed every tick and is applied
    /// fresh each tick to the movement *progress* instead (see
    /// `strait_zone` and `tick_movement`) — External code review fix (Stage
    /// 2D): a `required` baked from a one-time control sample either let an
    /// order issued during an open strait ignore a blockade established
    /// afterwards, or, issued under near-total enemy control, produced an
    /// effectively infinite `required` that could never recover even once
    /// the blockade lifted.
    pub required: f32,
    pub retreat: bool,
    /// The `Strait` link's sea zone this crossing passes through, if any
    /// (`action::apply_move`'s land branch and `tick_recovery`'s retreat
    /// branch both set this from `Link::strait_zone`). `tick_movement`
    /// reads *current* sea control here every tick via
    /// `naval::strait_factor`, rather than trusting a value sampled once
    /// when the order was issued.
    pub strait_zone: Option<SeaZoneId>,
}

#[derive(Clone, Debug)]
pub struct Unit {
    pub id: UnitId,
    pub owner: FactionId,
    pub name: String,
    /// Stage 2D (docs/phase2-spec.md "艦隊"): replaces the Phase 1/2A-2C
    /// `location: RegionId`. A land unit is always `Station::Region`, a
    /// fleet always `Station::Sea` — see `Station`'s own doc for why every
    /// region-scoped system keeps working unmodified against this.
    pub station: Station,
    pub movement: Option<Movement>,
    pub manpower: f32,
    pub equipment: f32,
    pub organization: f32,
    pub morale: f32,
    pub supply: f32,
    /// Stage 2C per-commodity delivery (docs/phase2-spec.md "3. 品目別の
    /// 到達率"): the fraction of this unit's equipment gap the supply
    /// network can currently deliver, in `0..=1`, eased toward its target
    /// the same way `supply` is (`logistics::distribute_supply`,
    /// `SUPPLY_SMOOTHING`). Read `arms_budget`, not this ratio, when
    /// deciding how much equipment can still land this tick - see its doc.
    pub arms_delivery: f32,
    /// External code review fix (Stage 2C): the absolute equipment units
    /// still deliverable to this unit *this tick*, reset from scratch every
    /// time `logistics::distribute_supply` runs (`gap * arms_delivery` as
    /// of that moment) rather than eased/accumulated. `action::apply_reinforce`
    /// spends this down as it delivers equipment, so N `ReinforceUnit`
    /// actions against the same unit in one batch can never together
    /// deliver more than one tick's allowance - reapplying `arms_delivery`
    /// to the shrinking remainder each call (the pre-fix behaviour) let a
    /// large enough N fill almost the whole gap regardless of the ratio.
    pub arms_budget: f32,
    /// The station `arms_delivery`/`arms_budget` were last computed for
    /// (stamped from `station` by `distribute_supply`, which runs before
    /// movement each tick). `apply_reinforce` compares this against the
    /// unit's *current* `station` to detect a same-day move that has left
    /// the cached ratio describing a place the unit already left, and
    /// recomputes on the spot rather than trusting it - see
    /// `logistics::instantaneous_arms_delivery` (land) and
    /// `naval::instantaneous_fleet_arms_delivery` (sea).
    pub arms_delivery_station: Station,
    pub experience: f32,
    pub alive: bool,
}

impl Unit {
    pub fn manpower_ratio(&self) -> f32 {
        self.manpower / UNIT_MANPOWER
    }

    pub fn equipment_ratio(&self) -> f32 {
        (self.equipment / UNIT_EQUIPMENT).min(1.0)
    }

    pub fn org_ratio(&self) -> f32 {
        self.organization / UNIT_ORG
    }

    /// Blended manpower/equipment fullness, used by AI to judge if a unit
    /// needs reinforcing.
    pub fn strength(&self) -> f32 {
        0.5 * self.manpower_ratio() + 0.5 * self.equipment_ratio()
    }

    pub fn combat_power(&self) -> f32 {
        self.manpower_ratio()
            * (0.35 + 0.65 * self.equipment_ratio())
            * (0.25 + 0.75 * self.org_ratio())
            * (0.50 + 0.50 * self.morale)
            * (0.35 + 0.65 * self.supply)
            * (1.00 + 0.35 * self.experience)
    }
}

/// Days required to traverse `link` into `dest`, `hostile` being true when
/// the destination is not friendly-owned (slows movement down). Land only —
/// see `fleet_move_required` for the sea-domain counterpart.
pub fn move_required(link_kind: crate::world::LinkKind, dest_terrain: Terrain, hostile: bool) -> f32 {
    link_kind.travel_days() * dest_terrain.move_cost() * if hostile { 1.5 } else { 1.0 }
}

/// Days required for a fleet to cross into an adjacent sea zone
/// (docs/phase2-spec.md Stage 2D doesn't spell out a travel-time formula for
/// sea-zone movement the way it does for land links — sea zones carry no
/// `LinkKind`/`Terrain` to derive one from — so this mirrors `move_required`'s
/// shape at the flat `FLEET_MOVE_DAYS` base instead: `hostile` (entering
/// waters some other faction currently holds more control of than this
/// faction does) applies the same 1.5x slowdown land crossings into
/// unfriendly territory get.
pub fn fleet_move_required(hostile: bool) -> f32 {
    FLEET_MOVE_DAYS * if hostile { 1.5 } else { 1.0 }
}

fn is_pinned(world: &World, unit: &Unit) -> bool {
    match unit.station {
        Station::Region(r) => world.has_enemy_units(r, unit.owner),
        Station::Sea(z) => world.has_enemy_fleets(z, unit.owner),
        // Stage 10A: no air-vs-air combat exists yet (that's 10B/10C), so
        // "pinned" for a grounded air unit means the same thing it would
        // for a land garrison sharing that airfield's own region - an
        // airfield sitting inside a region an enemy currently holds units
        // in is exactly as contested as the region itself.
        Station::Airfield(node) => world.has_enemy_units(world.transport_node(node).region, unit.owner),
    }
}

/// Advances in-progress movement, resolving arrivals. Units whose current
/// station holds enemy forces are pinned and make no progress unless
/// retreating. Handles both land units and fleets identically — `Movement`
/// and `Unit::station` are both `Station`-typed, so arrival is just
/// `unit.station = to` regardless of domain.
pub fn tick_movement(world: &mut World) {
    struct Progress {
        id: UnitId,
        arrived: bool,
        new_progress: f32,
    }

    let mut updates = Vec::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let Some(mv) = unit.movement else { continue };
        let pinned = !mv.retreat && is_pinned(world, unit);
        if pinned {
            continue;
        }
        let mut step = 0.5 + 0.5 * unit.supply;
        // External code review fix (Stage 2D): the CURRENT sea-control
        // factor for this crossing's strait, resampled every tick rather
        // than baked into `required` once at order time — see `Movement`'s
        // doc. Floored (`STRAIT_CROSSING_FACTOR_FLOOR`) so a total blockade
        // still lets progress creep forward instead of hard-freezing at
        // exactly zero, and so a lifted blockade always resumes normal
        // speed rather than the crossing having been left needing an
        // effectively infinite `required` to ever finish.
        if let Some(zone) = mv.strait_zone {
            let factor = naval::strait_factor(world, zone, unit.owner).max(STRAIT_CROSSING_FACTOR_FLOOR);
            step *= factor;
        }
        let new_progress = mv.progress + step;
        updates.push(Progress {
            id: unit.id,
            arrived: new_progress >= mv.required,
            new_progress,
        });
    }

    for update in updates {
        let unit = world.unit_mut(update.id);
        unit.organization = (unit.organization - ORG_MARCH_DRAIN).max(0.0);
        if update.arrived {
            let to = unit.movement.unwrap().to;
            unit.station = to;
            unit.movement = None;
        } else {
            unit.movement.as_mut().unwrap().progress = update.new_progress;
        }
    }
}

pub struct CombatReport {
    /// Indexed like `World::units`: true for units that fought this tick.
    pub fought: Vec<bool>,
    /// Indexed like `World::factions`: manpower lost this tick.
    pub casualties: Vec<f32>,
}

/// Resolves combat in every region held by two or more surviving factions.
pub fn tick_combat(world: &mut World, rng: &mut Rng, events: &mut Vec<Event>) -> CombatReport {
    let mut fought = vec![false; world.units.len()];
    let mut casualties = vec![0.0; world.factions.len()];

    for region_idx in 0..world.regions.len() {
        let region_id = RegionId(region_idx as u32);
        let mut factions_present: Vec<FactionId> = world
            .units_in(region_id)
            .map(|u| u.owner)
            .collect::<Vec<_>>();
        factions_present.sort_by_key(|f| f.0);
        factions_present.dedup();
        if factions_present.len() < 2 {
            continue;
        }
        // Stage 3B (docs/phase3-spec.md "Ceasefire": "戦闘・占領が発生しな
        // い"): two or more factions merely sharing a region isn't enough -
        // at least one *pair* of them must actually be at `Stance::War`, or
        // this is peaceful coexistence (a `Ceasefire`/`NonAggression`/
        // `Alliance` partner's units passing through), not a battle. The
        // default scenario starts every pair at War, so this is a no-op
        // there — every existing test keeps its prior behaviour.
        let any_war = factions_present
            .iter()
            .enumerate()
            .any(|(i, &a)| factions_present[i + 1..].iter().any(|&b| world.diplomacy.is_at_war(a, b)));
        if !any_war {
            continue;
        }

        let region = world.region(region_id);
        let defender = pick_defender(world, region, &factions_present);
        let defense_bonus = region.terrain.defense_bonus();
        let region_core = region.core;

        // Stage 3C `NationalFocus::DefensivePosture` (docs/phase3-spec.md:
        // "自領での防御補正＋"/"攻勢時の補正−"): a per-side multiplier layered
        // on top of terrain's `defense_bonus` - extra defense only when this
        // side is both the defender *and* fighting on its own `core`
        // territory (never on merely-held/occupied land), a penalty on
        // offense whenever this side is present but is *not* the defender
        // here. Computed once per side up front, the same way
        // `defense_bonus` itself is a single per-region constant for the
        // whole battle.
        let side_mult: Vec<f32> = factions_present
            .iter()
            .map(|&f| combat_posture_mult(world, f, f == defender, region_core == f))
            .collect();

        // Effective power per side, in the same order as `factions_present`.
        let power: Vec<f32> = factions_present
            .iter()
            .enumerate()
            .map(|(i, &f)| {
                let base = world.region_power(region_id, f);
                let terrain_mult = if f == defender { defense_bonus } else { 1.0 };
                base * terrain_mult * side_mult[i]
            })
            .collect();

        let mut battle_casualties = 0.0f32;
        // Raw damage dealt in this region today, summed across every side
        // regardless of who inflicts or receives it — the physical
        // destruction that feeds `Region::devastation`, independent of the
        // manpower/equipment casualties it also causes.
        let mut region_damage = 0.0f32;
        for (side_idx, &side_faction) in factions_present.iter().enumerate() {
            // Stage 3B: only power from sides actually at war with this one
            // counts as its "enemy" - a faction present but at peace with
            // `side_faction` neither deals nor takes damage from it, even in
            // a region where a third pair *is* at war (mixed-stance battles
            // are possible once treaties diverge factions' relationships).
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
            let dmg_side = enemy_power * COMBAT_DAMAGE * rng.range(0.85, 1.15);
            region_damage += dmg_side;
            let side_power = power[side_idx];
            if side_power <= 0.0 {
                continue;
            }

            let unit_ids: Vec<UnitId> = world
                .units_in(region_id)
                .filter(|u| u.owner == side_faction)
                .map(|u| u.id)
                .collect();

            for unit_id in unit_ids {
                fought[unit_id.index()] = true;
                let unit = world.unit_mut(unit_id);
                let terrain_mult = if side_faction == defender { defense_bonus } else { 1.0 };
                let raw_power = unit.combat_power() * terrain_mult * side_mult[side_idx];
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
                unit.equipment = (unit.equipment - dmg * EQUIPMENT_LOSS_PER_DAMAGE * broken).max(0.0);
                unit.morale = (unit.morale - MORALE_LOSS_PER_BROKEN_HIT * broken).max(0.0);
                unit.experience = (unit.experience + EXPERIENCE_GAIN_PER_HIT).min(1.0);

                casualties[side_faction.index()] += manpower_loss;
                battle_casualties += manpower_loss;
                world.faction_mut(side_faction).casualties += manpower_loss;
            }
        }

        let devastated = world.region_mut(region_id);
        devastated.devastation =
            (devastated.devastation + region_damage * DEVASTATION_PER_COMBAT_DAMAGE).min(1.0);

        events.push(Event::Battle {
            region: region_id,
            factions: factions_present.clone(),
            casualties: battle_casualties,
        });
    }

    CombatReport { fought, casualties }
}

/// `NationalFocus::DefensivePosture`'s combat multiplier for `faction` in
/// this battle (see `tick_combat`'s call site doc): `FOCUS_DEFENSIVE_
/// HOME_DEFENSE_MULT` when defending its own `core` soil, `FOCUS_DEFENSIVE_
/// OFFENSE_PENALTY_MULT` when present but not the defender, `1.0` otherwise
/// (including a DefensivePosture faction defending merely-held/occupied
/// land, or one whose focus isn't active - mid-transition per
/// `focus::active` - at all).
fn combat_posture_mult(world: &World, faction: FactionId, is_defender: bool, is_core: bool) -> f32 {
    if focus::active(world.faction(faction)) != Some(NationalFocus::DefensivePosture) {
        return 1.0;
    }
    if is_defender {
        if is_core {
            FOCUS_DEFENSIVE_HOME_DEFENSE_MULT
        } else {
            1.0
        }
    } else {
        FOCUS_DEFENSIVE_OFFENSE_PENALTY_MULT
    }
}

fn pick_defender(world: &World, region: &Region, present: &[FactionId]) -> FactionId {
    if present.contains(&region.owner) {
        return region.owner;
    }
    let mut best = present[0];
    let mut best_power = world.region_power(region.id, best);
    for &f in &present[1..] {
        let p = world.region_power(region.id, f);
        if p > best_power || (p == best_power && f.0 < best.0) {
            best = f;
            best_power = p;
        }
    }
    best
}

/// Regenerates organization/morale for units not in combat, applies
/// unsupplied attrition, and routs or destroys broken units.
pub fn tick_recovery(world: &mut World, fought: &[bool], events: &mut Vec<Event>) {
    struct Delta {
        id: UnitId,
        organization: f32,
        morale: f32,
        manpower: f32,
    }

    // Stage 3A (docs/phase3-spec.md "軍部の不服従": "全部隊の組織率回復に係
    // 数"): read once per faction, keyed by index like `World::factions`, so
    // `tick_recovery` doesn't need to know about `crate::group` at all -
    // `politics::tick_politics` owns deciding whether mutiny is active.
    let mutiny: Vec<bool> = world.factions.iter().map(|f| f.mutiny_active).collect();
    // Stage 3C `NationalFocus::MilitaryUnification` (docs/phase3-spec.md:
    // "部隊の組織率上限＋"): the ceiling `organization` is clamped to below,
    // per faction - `UNIT_ORG` normally, raised while this focus is active
    // (post-transition; see `focus::active`). `Unit::org_ratio`/
    // `combat_power` still divide by the fixed `UNIT_ORG`, so a unit held at
    // the raised ceiling reads as an organization ratio above 1.0 there -
    // this only widens what `organization` itself can reach.
    let org_cap: Vec<f32> = world
        .factions
        .iter()
        .map(|f| {
            if focus::active(f) == Some(NationalFocus::MilitaryUnification) {
                UNIT_ORG * FOCUS_MILITARY_ORG_CAP_MULT
            } else {
                UNIT_ORG
            }
        })
        .collect();

    let mut deltas = Vec::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        let is_fighting = fought[unit.id.index()];
        let marching = unit.movement.is_some();

        let mut organization = unit.organization;
        let mut morale = unit.morale;
        let mut manpower = unit.manpower;

        if !is_fighting && !marching {
            // A fleet at sea has no region infrastructure to draw on; ships
            // maintain themselves at the same baseline a fully-developed
            // land region's infrastructure factor (1.0) would give a unit,
            // rather than getting a land-specific bonus/penalty they have no
            // way to earn or suffer.
            let infra = match unit.station {
                Station::Region(r) => world.region(r).effective_infrastructure(),
                Station::Sea(_) => 1.0,
                // Stage 10A: an airfield sits inside a region (unlike open
                // water), so it draws the same regional infrastructure
                // bonus/penalty a land unit stationed there would, rather
                // than the sea-only flat baseline.
                Station::Airfield(node) => world.region(world.transport_node(node).region).effective_infrastructure(),
            };
            let mutiny_mult = if mutiny[unit.owner.index()] { MUTINY_ORG_REGEN_MULT } else { 1.0 };
            organization += ORG_REGEN * (0.3 + 0.7 * unit.supply) * (0.6 + 0.4 * infra) * mutiny_mult;
        }
        if !is_fighting {
            morale += MORALE_REGEN * unit.supply;
        }
        if unit.supply < ATTRITION_SUPPLY_THRESHOLD {
            manpower -= ATTRITION_MANPOWER * (1.0 - unit.supply / ATTRITION_SUPPLY_THRESHOLD);
            organization -= ATTRITION_ORG;
        }

        deltas.push(Delta {
            id: unit.id,
            organization: organization.clamp(0.0, org_cap[unit.owner.index()]),
            morale: morale.clamp(0.0, 1.0),
            manpower: manpower.max(0.0),
        });
    }

    for d in deltas {
        let unit = world.unit_mut(d.id);
        let owner = unit.owner;
        let attrition_loss = (unit.manpower - d.manpower).max(0.0);
        unit.organization = d.organization;
        unit.morale = d.morale;
        unit.manpower = d.manpower;
        if attrition_loss > 0.0 {
            world.faction_mut(owner).casualties += attrition_loss;
        }
    }

    enum Outcome {
        Retreat {
            to: Station,
            required: f32,
            strait_zone: Option<SeaZoneId>,
        },
        Destroyed,
    }

    let mut outcomes = Vec::new();
    for unit in &world.units {
        if !unit.alive {
            continue;
        }
        if unit.manpower <= UNIT_DEATH_MANPOWER {
            // A unit this depleted is dead outright - it must not linger as
            // a phantom retreater that still blocks occupation and counts
            // in presence checks for one extra tick.
            outcomes.push((unit.id, Outcome::Destroyed));
        } else if unit.organization <= 0.0 && unit.movement.is_none() && is_pinned(world, unit) {
            // Stage 2D: a broken fleet with no sea zone left to fall back
            // into is sunk here exactly the way a broken land unit with no
            // region to retreat into is destroyed - the same mechanism, not
            // a special case, which is what "艦隊は退却先の海域がなければ撃沈
            // される" (docs/phase2-spec.md Stage 2D) falls out of.
            if let Some(dest) = retreat_candidate(world, unit) {
                // Mirrors `action::apply_move`'s land branch: `required`
                // holds only the control-independent travel cost, and a
                // retreat across a `Strait` link's zone carries that zone
                // through as `strait_zone` so `tick_movement` throttles its
                // *progress* by the current sea-control factor each tick,
                // the same as an ordered move does.
                let (required, strait_zone) = match (unit.station, dest) {
                    (Station::Region(from), Station::Region(to)) => {
                        let link = world.link_between(from, to).unwrap();
                        (
                            move_required(link.kind, world.region(to).terrain, false) * 0.5,
                            link.strait_zone,
                        )
                    }
                    (Station::Sea(_), Station::Sea(_)) => (fleet_move_required(false) * 0.5, None),
                    _ => unreachable!("a unit's retreat candidates are always its own domain"),
                };
                outcomes.push((
                    unit.id,
                    Outcome::Retreat {
                        to: dest,
                        required,
                        strait_zone,
                    },
                ));
            } else {
                outcomes.push((unit.id, Outcome::Destroyed));
            }
        }
    }

    for (id, outcome) in outcomes {
        match outcome {
            Outcome::Retreat {
                to,
                required,
                strait_zone,
            } => {
                let unit = world.unit_mut(id);
                unit.movement = Some(Movement {
                    from: unit.station,
                    to,
                    progress: 0.0,
                    required,
                    retreat: true,
                    strait_zone,
                });
            }
            Outcome::Destroyed => {
                let unit = world.unit_mut(id);
                let (station, owner, residual_manpower) = (unit.station, unit.owner, unit.manpower);
                unit.alive = false;
                if residual_manpower > 0.0 {
                    world.faction_mut(owner).casualties += residual_manpower;
                }
                events.push(Event::UnitDestroyed {
                    unit: id,
                    station,
                    owner,
                });
            }
        }
    }
}

/// The nearest (lowest-id) safe fallback station for a broken unit, if any:
/// for a land unit, an owned neighboring region with no enemy present; for a
/// fleet, an adjacent sea zone with no enemy fleet present (sea zones have
/// no owner, so "safe" for a fleet means simply uncontested).
///
/// Stage 10A: an air unit has no such fallback yet - relocating a broken
/// squadron to a different airfield is an operational-radius/movement
/// question docs/phase10-spec.md leaves to a later stage (10A ships no
/// `MoveUnit` support for `Domain::Air` at all - `action::apply_move`'s own
/// doc). A broken, pinned, immobile air unit is therefore destroyed outright
/// exactly like a land unit with no safe neighboring region to fall back
/// into, never invented a retreat path it has no data-backed way to earn.
fn retreat_candidate(world: &World, unit: &Unit) -> Option<Station> {
    match unit.station {
        Station::Region(from) => {
            let mut candidates: Vec<RegionId> = world
                .neighbors(from)
                .filter(|&r| {
                    world.region(r).owner == unit.owner && !world.has_enemy_units(r, unit.owner)
                })
                .collect();
            candidates.sort_by_key(|r| r.0);
            candidates.first().map(|&r| Station::Region(r))
        }
        Station::Sea(from) => {
            let mut candidates: Vec<crate::ids::SeaZoneId> = world
                .sea_zone(from)
                .adjacent
                .iter()
                .copied()
                .filter(|&z| !world.has_enemy_fleets(z, unit.owner))
                .collect();
            candidates.sort_by_key(|z| z.0);
            candidates.first().map(|&z| Station::Sea(z))
        }
        Station::Airfield(_) => None,
    }
}

/// Advances occupation progress in every region and flips ownership once it
/// reaches 100.
pub fn tick_occupation(world: &mut World, events: &mut Vec<Event>) {
    let n = world.regions.len();
    let mut present_factions: Vec<Vec<FactionId>> = Vec::with_capacity(n);
    for i in 0..n {
        let region_id = RegionId(i as u32);
        let mut factions: Vec<FactionId> = world.units_in(region_id).map(|u| u.owner).collect();
        factions.sort_by_key(|f| f.0);
        factions.dedup();
        present_factions.push(factions);
    }

    for i in 0..n {
        let region_id = RegionId(i as u32);
        let owner = world.region(region_id).owner;
        let present = &present_factions[i];

        // Stage 3B (docs/phase3-spec.md "Ceasefire": "占領は発生しない",
        // "MilitaryAccess": "占領は発生しない"): a foreign faction present
        // only counts as a potential occupier if it's actually at war with
        // `owner` *and* hasn't been granted transit rights here. A faction
        // at peace (or holding `MilitaryAccess`) can sit in `owner`'s
        // territory indefinitely without that presence ever starting an
        // occupation - the default all-War scenario makes every present
        // foreign faction eligible exactly as before, so existing tests are
        // unaffected.
        let eligible: Vec<FactionId> = present
            .iter()
            .copied()
            .filter(|&f| {
                f != owner
                    && world.diplomacy.is_at_war(owner, f)
                    && !world.diplomacy.has_treaty(owner, f, Treaty::MilitaryAccess)
            })
            .collect();

        if present.contains(&owner) || eligible.is_empty() {
            let region = world.region_mut(region_id);
            // Stage 3A separatism (docs/phase3-spec.md "地方独立運動",
            // `politics::tick_separatism`): a core-faction reversion in
            // progress is a *political* drift, not "nobody is contesting
            // this region" in the ordinary military sense decay here
            // represents — it must not be eaten by that decay just because
            // no foreign army is physically present (or only the owner's own
            // garrison is), or a political force with no troops to send
            // could never overcome it. External code review fix (Fix 2/3):
            // recognized via `OccupationKind` rather than the structural
            // `occupier == core != owner` check this used to use — that
            // structural check couldn't tell a genuine separatist marker
            // apart from a military occupier that happened to *also* be
            // `core` (see `OccupationKind`'s doc), which let a real invasion
            // by `core` silently inherit separatist progress instead of
            // starting fresh below. `politics.rs` owns this region's
            // occupation meter — including scaling its own rate down for a
            // present owner garrison, see `politics::separatist_rate` — for
            // as long as `occupation_kind` says `Separatist`.
            let separatist_advance = region.occupation_kind == Some(OccupationKind::Separatist);
            if !separatist_advance {
                region.occupation = (region.occupation - OCCUPATION_DECAY).max(0.0);
                if region.occupation == 0.0 {
                    region.occupier = None;
                    region.occupation_kind = None;
                }
            }
            continue;
        }

        let occupier = eligible[0];
        let region = world.region_mut(region_id);
        if region.occupier != Some(occupier) || region.occupation_kind != Some(OccupationKind::Military) {
            // A newly arrived occupier starts from zero: progress earned by
            // a previous occupying faction — or by political separatism
            // drifting toward the same faction (External code review fix,
            // Fix 2: an actual invading army must never get a head start off
            // a political marker just because it happens to share the
            // target faction) — must not carry over to this one.
            region.occupation = 0.0;
        }
        region.occupier = Some(occupier);
        region.occupation_kind = Some(OccupationKind::Military);
        region.occupation += OCCUPATION_RATE;

        if region.occupation >= 100.0 {
            region.owner = occupier;
            region.occupation = 0.0;
            region.occupier = None;
            region.occupation_kind = None;
            region.unrest += CAPTURE_UNREST;
            // A region changing hands is a war-damage spike of its own
            // (looting, sabotage, the fighting that won it) on top of
            // whatever combat damage already accrued, and any project the
            // previous owner had underway does not carry over.
            region.devastation = (region.devastation + DEVASTATION_ON_CAPTURE).min(1.0);
            region.construction = None;
            events.push(Event::RegionCaptured {
                region: region_id,
                from: owner,
                to: occupier,
            });
            let occupier_support =
                (world.faction(occupier).war_support + WAR_SUPPORT_CAPTURE_GAIN).clamp(0.0, 100.0);
            world.faction_mut(occupier).war_support = occupier_support;
            let owner_support =
                (world.faction(owner).war_support - WAR_SUPPORT_LOSS_PENALTY).clamp(0.0, 100.0);
            world.faction_mut(owner).war_support = owner_support;
        }
    }
}
