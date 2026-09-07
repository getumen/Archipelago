//! All tunable balance constants for the simulation, gathered in one place
//! so systems never carry magic numbers of their own.

pub const WORKFORCE_SHARE: f32 = 0.5;
pub const CONSCRIPT_RATE: f32 = 0.00035;

/// Daily fraction of the manpower pool (drafted conscripts not yet
/// assigned to a unit) that returns to the civilian workforce, independent
/// of policy. This is the demobilization half of the manpower-pool fix
/// (`economy::tick_economy`): `conscription` only throttles inflow, so
/// without an outflow a faction sitting on a large population could draft
/// forever and never give the labour back, even though `Region::mobilized`
/// (and therefore `labor_ratio`) is recomputed from the *current* pool
/// every tick. Draining the pool changes nothing about population or unit
/// manpower - it only shrinks what counts toward `mobilized`, so the
/// workforce comes back automatically on the next tick.
///
/// At this rate a pool being fed by continuous drafting settles at
/// `draft / MANPOWER_DEMOBILIZATION_RATE` instead of growing without
/// bound: e.g. a faction drafting at the AI's throttled 0.15 tier
/// (agents::CONSCRIPTION_THROTTLE_MANPOWER) off the *entire* map's
/// population (~12,280) settles around 32 (万人) - the same order of
/// magnitude as that throttle threshold, not the hundreds a one-way
/// accumulator produces.
pub const MANPOWER_DEMOBILIZATION_RATE: f32 = 0.02;

/// Stage 2A production chain (docs/phase2-spec.md "Stage 2A"): input goods
/// consumed per unit of output good produced, at the `Steel -> Machinery /
/// Munitions -> Arms` stage. `Food` and `Energy` have no inputs.
pub const STEEL_INPUT_ENERGY: f32 = 0.5;
pub const MACHINERY_INPUT_STEEL: f32 = 0.4;
pub const MACHINERY_INPUT_ENERGY: f32 = 0.3;
pub const MUNITIONS_INPUT_STEEL: f32 = 0.3;
pub const MUNITIONS_INPUT_ENERGY: f32 = 0.2;
pub const ARMS_INPUT_MACHINERY: f32 = 0.5;
pub const ARMS_INPUT_STEEL: f32 = 0.3;

/// Stage 3C playtest fix (the ninth defect of docs/phase3-spec.md §0's
/// shape, found post-Stage-3C): `economy::tick_economy`'s Step 0/1
/// `efficiency`/`stability_mult` terms used to apply the *same*
/// industrial-disorder multiplier to every commodity, `Food` included. That
/// equality is what let unrest and shortage close into a loop with no
/// floor: unrest rises -> efficiency falls -> Food output falls -> shortage
/// rises -> unrest rises, bottoming out at `efficiency`'s own 0.2 floor
/// times `stability_mult`'s own 0.6 floor (0.12x capacity) with nothing to
/// arrest the fall before that - nowhere near enough to feed a population,
/// and two whole surviving factions sat pinned at `shortage == 1.0` for
/// 220+ days in the seed-3/seed-5 playtests that found this.
///
/// `Food`'s potential (`economy::tick_economy`) is now scaled by its own
/// term instead of the shared `efficiency[region] * stability_mult` every
/// other commodity uses: `FOOD_EFFICIENCY_FLOOR + (1 -
/// FOOD_EFFICIENCY_FLOOR) * (efficiency[region] *
/// stability_mult).clamp(0, 1).powf(FOOD_EFFICIENCY_DAMPENING)`.
/// - At full efficiency/stability (`== 1.0`) this still reaches `1.0`, so a
///   calm, fully-staffed faction sees no change from before.
/// - `FOOD_EFFICIENCY_DAMPENING < 1.0` makes the term fall far more slowly
///   than a straight product does as either input drops (`x.powf(d)` sits
///   above `x` on `0..1` whenever `d < 1`), so a strike or a stability crash
///   no longer cuts the harvest by the same proportion it cuts industrial
///   output.
/// - `FOOD_EFFICIENCY_FLOOR` is an unconditional additive floor: even at the
///   combined worst case (`efficiency == 0.2`, `stability_mult == 0.6`) Food's
///   multiplier can't fall below it, so an owned, undevastated region always
///   produces a meaningful fraction of its Food capacity - the loop above
///   has a floor to land on instead of spiraling toward zero
///   (`food_output_survives_political_collapse`).
///
/// This is a property of the same two underlying multipliers every other
/// commodity already reads, not a special-cased number: a faction that lets
/// unrest decay and stability recover climbs `efficiency[region] *
/// stability_mult` back toward 1.0 exactly as it always did, and Food's own
/// multiplier climbs right back up with it - a faction driven to maximum
/// shortage is not stuck there once the fighting that caused it stops
/// (`collapsed_faction_can_recover`, the regression guard for this
/// absorbing state).
///
/// This does NOT touch `Region::effective_capacity`'s `* (1 - devastation)`
/// term (applied before any of this, unconditionally, to every commodity
/// including Food), `Event::RegimeChange`'s flat `REGIME_CHANGE_OUTPUT_MULT`
/// (a fixed-duration, self-resetting event rather than a feedback loop that
/// deliberately hits every commodity per its own doc), or
/// `NationalFocus::Technocracy`'s production bonus - physical destruction of
/// the land, and the deliberate all-commodity events that already carry
/// their own recovery guarantee, still bite Food exactly as hard as before
/// (`devastation_still_destroys_food`).
pub const FOOD_EFFICIENCY_FLOOR: f32 = 0.6;
pub const FOOD_EFFICIENCY_DAMPENING: f32 = 0.3;

/// docs/phase8-spec.md Fix 2's follow-up: the Stage 3C playtest fixed the
/// "shortage → unrest → lower efficiency → worse shortage" loop for `Food`
/// specifically (`FOOD_EFFICIENCY_FLOOR`'s doc), but left every other
/// commodity reading the exact same unprotected product it replaced -
/// `efficiency[region]` (its own 0.2 floor, Step 0 of `economy.rs`) times
/// `stability_output_mult(stability)` (its own 0.6 floor) - so a faction
/// whose political support has genuinely collapsed can still see Energy,
/// Steel, Machinery, Munitions and Arms output pinned at that product's
/// compound worst case, `0.2 * 0.6 = 0.12`x capacity, with the same
/// no-recovery shape `FOOD_EFFICIENCY_FLOOR`'s own doc describes: low
/// output keeps `faction.shortage`/`supply_ratio` bad, which keeps unrest's
/// target high (`politics::tick_politics`), which keeps `efficiency` and
/// `stability_output_mult` pinned at their own floors - conventions.md §6's
/// "状態には必ず回復経路を持たせる" applies here exactly as much as it did to
/// Food, `japan_hex.json`'s smaller factions included
/// (`tools/hexmap/constants.py`'s "Stage 8B" section names this same gap).
///
/// This is deliberately a **narrower** fix than `FOOD_EFFICIENCY_FLOOR`'s,
/// not a copy of it, for two reasons:
///
/// - **Scope.** `FOOD_EFFICIENCY_FLOOR` floors the *combined*
///   `efficiency[region] * stability_output_mult(stability)` product - it
///   protects Food from a single region's own war-caused unrest as much as
///   from national political collapse. That's right for Food (a starving
///   population doesn't care which of the two caused it), but wrong for
///   military-industrial output: "devastation must still hurt production
///   hard" (`devastation_still_destroys_food`'s doc makes the same call for
///   Food, just against `effective_capacity`'s separate `(1-devastation)`
///   term rather than this one) extends here to unrest, too - a specific
///   region under active contest should stay exactly as suppressed as
///   before. So `INDUSTRIAL_STABILITY_FLOOR` only floors the
///   `stability_output_mult` half of the product (`economy::tick_economy`'s
///   `industrial_stability_mult`) - the faction-wide, policy-recoverable
///   signal - and leaves `efficiency[region]`'s own independent 0.2 floor
///   (unrest, labor mobilization, infrastructure) completely untouched.
///   A region that's actively contested or devastated is hit exactly as
///   hard as it always was; what changes is that a faction can no longer be
///   *additionally* crushed by its own national stability cratering on top
///   of that.
/// - **Magnitude.** `0.8`, not `FOOD_EFFICIENCY_FLOOR`'s `0.6`-of-the-
///   combined-product: still deliberately short of Food's protection (Food
///   floors the *combined* signal and still lands near `0.81`x at the
///   realistic worst case, per `food_output_survives_political_collapse`;
///   this term leaves `efficiency`'s own independent 0.2 floor completely
///   exposed, so the compound worst case here is `0.2 * 0.8 = 0.16`x
///   capacity even after the floor applies), but anchored to
///   `stability_output_mult`'s own formula rather than to any particular
///   scenario's observed range: `stability_output_mult(50.0) == 0.8`, so a
///   faction whose *national* government has collapsed completely
///   (`stability == 0`) is, for the industrial half of the loop, never
///   treated worse than one whose government still commands half its usual
///   support - genuine collapse gets real headroom to climb back
///   (conventions.md §6), while a merely-strained faction (`stability >
///   50`, i.e. `stability_output_mult` already above this floor on its
///   own) sees no change. `scenarios/mvp.json` seed 1's 720-day run drives
///   a faction as low as `stability ≈ 49` (`stability_output_mult ≈
///   0.796`), just inside this floor's range, so this value does change
///   mvp's `--json` hash - `docs/conventions.md` §5 is explicit that a
///   genuine balance change is allowed to do that; the previous `0.7` was
///   chosen specifically to stay below mvp's observed minimum instead of
///   from this reasoning, which made the floor a no-op for every faction
///   any `HeuristicAgent` in this repository has ever actually produced.
pub const INDUSTRIAL_STABILITY_FLOOR: f32 = 0.8;

/// Civilian demand, per capita (population is tracked in 万人/"ten
/// thousands"), for the three commodities civilians draw on directly.
///
/// `CIVILIAN_ENERGY_DEMAND_PER_POP` and `CIVILIAN_MACHINERY_DEMAND_PER_POP`
/// are derived from the Stage 2A scenario capacity table
/// (docs/phase2-spec.md's region list, summed per faction), not guessed:
/// with civilian demand met first and industry taking the remainder (see
/// economy.rs), each faction's civilian Energy draw should land around a
/// quarter of its *national* Energy capacity, and its civilian Machinery
/// draw around a twentieth of its Machinery capacity, so every faction can
/// both feed its population and still fund a substantial share of its
/// Steel/Machinery/Munitions/Arms chain. Concretely, at the nation's Energy
/// capacity (10.0 for 東方連合) versus what running Steel+Machinery+
/// Munitions at full table capacity alone would draw (~6.2), a civilian
/// share near 25% (~2.5) leaves the chain's ~6.2 need covered with room to
/// spare; scaled by each faction's own population this comes out to
/// approximately:
/// - 東方連合 (pop 5690, Energy 10.0): demand ≈ 2.56 (25.6% of capacity)
/// - 中央同盟 (pop 4180, Energy 7.5): demand ≈ 1.88 (25.1% of capacity)
/// - 西方同盟 (pop 2410, Energy 5.3): demand ≈ 1.08 (20.5% of capacity)
///
/// which fixes the per-capita rate at 0.00045. The same table-driven method
/// (target ≈5-8% of national Machinery capacity, since Arms' Machinery
/// input is small relative to Machinery capacity) fixes Machinery at
/// 0.0001.
pub const CIVILIAN_FOOD_DEMAND_PER_POP: f32 = 0.0022;
pub const CIVILIAN_ENERGY_DEMAND_PER_POP: f32 = 0.00045;
pub const CIVILIAN_MACHINERY_DEMAND_PER_POP: f32 = 0.0001;

/// Valid range for `Faction::civilian_ration` (design.md §9's civilian/war
/// trade-off, `Action::SetCivilianRation`): the fraction of civilian Food/
/// Energy/Machinery demand the government actually delivers. Below 1.0 the
/// undelivered share is never drawn from stock, freeing it for industry, at
/// the cost of feeding `Faction::shortage` (and therefore unrest) exactly
/// as genuine scarcity would. Floored at 0.5 so rationing is a costly lever,
/// not a way to make civilian demand vanish.
pub const CIVILIAN_RATION_MIN: f32 = 0.5;
pub const CIVILIAN_RATION_MAX: f32 = 1.0;
pub const CIVILIAN_RATION_DEFAULT: f32 = 1.0;

pub const UNIT_MANPOWER: f32 = 1.0;
pub const UNIT_EQUIPMENT: f32 = 20.0;
pub const UNIT_ORG: f32 = 100.0;
pub const UNIT_START_ORG_RATIO: f32 = 0.4;

/// Stage 10A (docs/phase10-spec.md "4. 生産"): the `Good::Machinery` an
/// `action::apply_recruit`'s `Domain::Air` branch consumes on top of the
/// same `UNIT_MANPOWER`/`UNIT_EQUIPMENT`(-priced-in-Arms) cost every other
/// domain already pays - an airframe's own airframe-and-avionics cost, on
/// the same industrial input land/sea equipment is already priced in
/// (`Good::Arms`), rather than a new commodity (the spec explicitly rules
/// out adding `Fuel`: "新しい Good を追加しない"). Set to `UNIT_EQUIPMENT`'s
/// own order of magnitude - no scenario deploys a `Domain::Air` unit yet
/// (10A leaves that to 10B onward), so there is nothing to measure this
/// against in actual play; a real balance pass is Stage 10E's job, not
/// this one's, per docs/conventions.md's "測定してから直す" - this is a
/// first, disclosed placeholder, not a tuned constant.
pub const AIR_UNIT_MACHINERY_COST: f32 = 20.0;

/// Stage 10B (docs/phase10-spec.md "2. 制空権"): how far an airfield's own
/// committed air power reaches, measured as straight-line geographic
/// distance between `world::Region::position`s - never through the
/// transport network ("航空機は線路の上を飛ばない") - in the same unit
/// `scenarios/japan_hex.json`'s own `position` field actually carries:
/// kilometres (`tools/hexmap/build_scenario.py`'s `x_m / 1000.0`;
/// `tools/hexmap/hexgrid.py`'s `SPACING_M = 40_000.0` spaces that map's own
/// hexes 40km apart center-to-center, for scale).
///
/// Chosen from what the number itself means, not from any scenario's
/// outcome (docs/conventions.md's shared "測定してから直す" discipline,
/// applied here as "don't fit the constant to a result"): 300km sits at
/// the low end of a single-engine fighter's typical unrefuelled combat
/// radius (roughly 300-500km for many 20th-century designs, before drop
/// tanks or air-to-air refuelling) - the aircraft class actually contesting
/// airspace over a single region, as distinct from a long-range bomber's
/// much greater reach. `scenarios/mvp.json`/`japan47.json`'s own `position`
/// fields are an unscaled schematic layout with no physical unit at all
/// (`world::Region::position`'s own doc), so this constant is honestly
/// meaningless there - but no shipped scenario places a `Domain::Air` unit
/// yet (`AIR_UNIT_MACHINERY_COST`'s own doc discloses the same gap), so
/// nothing currently exercises that mismatch. A real balance pass - which
/// may mean rescaling the two schematic maps' own coordinates rather than
/// this constant - is Stage 10E's job, not this one's.
pub const AIR_OPERATING_RADIUS_KM: f32 = 300.0;

pub const SUPPLY_NEED_PER_MANPOWER: f32 = 1.0;
pub const COMBAT_SUPPLY_MULT: f32 = 2.5;

pub const COMBAT_DAMAGE: f32 = 8.0;
pub const ORG_DAMAGE_MULT: f32 = 2.0;
pub const MANPOWER_LOSS_PER_DAMAGE: f32 = 0.004;
pub const EQUIPMENT_LOSS_PER_DAMAGE: f32 = 0.12;
pub const BROKEN_LOSS_MULT: f32 = 3.0;
/// Morale lost per broken hit taken in combat (`tick_combat`/
/// `naval::tick_naval_combat`, multiplied by `BROKEN_LOSS_MULT` when the
/// hit breaks the unit's organization).
pub const MORALE_LOSS_PER_BROKEN_HIT: f32 = 0.01;
/// Experience gained per hit taken in combat, land or naval.
pub const EXPERIENCE_GAIN_PER_HIT: f32 = 0.0015;

pub const ORG_REGEN: f32 = 2.5;
pub const ORG_MARCH_DRAIN: f32 = 3.0;
pub const MORALE_REGEN: f32 = 0.02;
pub const ATTRITION_MANPOWER: f32 = 0.006;
pub const ATTRITION_ORG: f32 = 6.0;

pub const OCCUPATION_RATE: f32 = 30.0;
pub const OCCUPATION_DECAY: f32 = 25.0;
pub const CAPTURE_UNREST: f32 = 45.0;
pub const OCCUPIED_UNREST_FLOOR: f32 = 15.0;

/// Daily rate at which `region.unrest` closes the gap toward its
/// shortage/supply-pressure target (a target-approach model, not a
/// decay-then-accumulate one, so unrest can recover once pressure eases).
pub const UNREST_ADAPT_RATE: f32 = 0.05;
/// Unrest-target contribution at `faction.shortage == 1.0` (civilian/food
/// shortfall). Not enumerated explicitly in the spec prose
/// ("無補給・物資不足で上昇"); added here so the effect has a named,
/// centralized constant instead of a magic number.
pub const UNREST_SHORTAGE_PRESSURE: f32 = 55.0;
/// Unrest-target contribution at `faction.supply_ratio == 0.0` (fully unsupplied).
pub const UNREST_SUPPLY_PRESSURE: f32 = 30.0;

/// Daily fraction by which `unit.supply` moves toward its computed target,
/// used by `logistics::distribute_supply` to avoid abrupt swings.
pub const SUPPLY_SMOOTHING: f32 = 0.35;

/// Daily linear step by which `war_support` creeps back toward its 50 baseline.
pub const WAR_SUPPORT_DRIFT: f32 = 0.05;
/// War-support loss multiplier applied to a day's manpower casualties.
pub const WAR_SUPPORT_CASUALTY_MULT: f32 = 5.0;
/// War-support swing on a region changing hands.
pub const WAR_SUPPORT_CAPTURE_GAIN: f32 = 3.0;
pub const WAR_SUPPORT_LOSS_PENALTY: f32 = 4.0;

/// Manpower floor below which a unit is considered destroyed.
pub const UNIT_DEATH_MANPOWER: f32 = 0.05;
/// Supply ratio below which unsupplied attrition kicks in.
pub const ATTRITION_SUPPLY_THRESHOLD: f32 = 0.25;

/// Stage 2B war-damage/reconstruction model (docs/phase2-spec.md "Stage
/// 2B — インフラと建設・戦災"): `Region::devastation` (0..1) makes holding
/// territory not the same as being able to use it.
///
/// How much of `devastation` bites into `Region::effective_infrastructure`
/// (which both `economy`'s efficiency term and `logistics`'s relay
/// propagation read instead of the raw field): `effective_infra =
/// infrastructure * (1 - devastation * INFRA_DAMAGE_SHARE)`. Kept below 1.0
/// so a fully devastated region's infrastructure is crippled, not zeroed —
/// `effective_capacity` (a flat `* (1 - devastation)`) already carries the
/// harsher, unscaled penalty for production itself.
pub const INFRA_DAMAGE_SHARE: f32 = 0.6;

/// `devastation` gained per point of raw combat damage dealt in a region
/// this tick (the sum of `military::tick_combat`'s per-side `dmg_side`,
/// before it's split across units and converted to casualties — the same
/// scale `COMBAT_DAMAGE` operates on). A single skirmish nudges devastation
/// up a little; a region that stays a front line for weeks grinds toward
/// fully devastated.
pub const DEVASTATION_PER_COMBAT_DAMAGE: f32 = 0.001;

/// One-time `devastation` spike applied the instant a region's owner
/// changes (looting, sabotage, the fighting that won it) — on top of
/// whatever combat damage already accrued during the occupation fight.
pub const DEVASTATION_ON_CAPTURE: f32 = 0.35;

/// Base daily fraction of `devastation` recovered, before the unrest/
/// stability scaling in `construction::tick_devastation_recovery`:
/// `recovery = DEVASTATION_RECOVERY * (1 - unrest/100) * (0.5 + 0.5 *
/// stability/100)`. An unruly occupied region (`unrest` near 100) recovers
/// almost nothing on its own.
pub const DEVASTATION_RECOVERY: f32 = 0.01;

/// Building-point throughput a region's construction project advances by
/// per day when fully funded (`construction::tick_construction`); the
/// actual rate is scaled down to whatever fraction of its Machinery/Steel
/// cost the faction's stock can cover that tick, so a starved project slows
/// rather than stalling outright.
pub const CONSTRUCTION_RATE: f32 = 2.0;
/// Machinery consumed, from the national stock, per building point of
/// progress funded.
pub const CONSTRUCTION_MACHINERY_PER_POINT: f32 = 0.5;
/// Steel consumed, from the national stock, per building point of progress funded.
pub const CONSTRUCTION_STEEL_PER_POINT: f32 = 1.0;

/// Building points required to complete each `Project` variant
/// (`construction::required_points`) — at `CONSTRUCTION_RATE` fully funded,
/// `Infrastructure` takes 50 days, `Port` 40, `Capacity` 30, `Repair` 20;
/// `Repair` is deliberately the cheapest so it's a real alternative to
/// passive `DEVASTATION_RECOVERY`, not a strictly worse one.
pub const CONSTRUCTION_REQUIRED_INFRASTRUCTURE: f32 = 100.0;
pub const CONSTRUCTION_REQUIRED_PORT: f32 = 80.0;
pub const CONSTRUCTION_REQUIRED_CAPACITY: f32 = 60.0;
pub const CONSTRUCTION_REQUIRED_REPAIR: f32 = 40.0;

/// Effect size of each completed project (`construction::apply_completion`),
/// per docs/phase2-spec.md Stage 2B's completion-effect table.
pub const INFRA_STEP: f32 = 0.15;
pub const PORT_STEP: f32 = 0.3;
pub const CAPACITY_STEP: f32 = 1.0;
pub const REPAIR_STEP: f32 = 0.3;

/// Stage 2C sea imports (docs/phase2-spec.md "Stage 2C — 品目別物流・港湾容量・
/// 海上輸入"): each owned, uncontested port region can pull `Food`/`Energy`
/// in from outside the map, capped per port node (never summed into one
/// national number — `trade::tick_imports` keeps every port's own
/// contribution, since Stage 2D blockades individual ports).
///
/// `port_capacity(region) = region.port * IMPORT_PER_PORT * (1 -
/// devastation)`. Scaled so a faction's full port line can plausibly close
/// the structural Food gap the Stage 2A playtest found (a faction sitting on
/// the Machinery hub but short on Food): e.g. 中央同盟 (信越・北陸/東海/近畿,
/// ports 0.5+1.2+1.3=3.0) gets a combined capacity of `3.0 * 3.0 = 9.0`
/// good/day against a civilian Food need on the order of ~9 (pop 4180 *
/// `CIVILIAN_FOOD_DEMAND_PER_POP`), enough headroom to close a production
/// shortfall without dwarfing domestic output.
pub const IMPORT_PER_PORT: f32 = 3.0;

/// Upper clamp on `Action::SetImportPlan`'s `rate` (docs/phase2-spec.md:
/// "rate は 0 以上、上限でクランプ" — out-of-range values are clamped, not
/// rejected, unlike an invalid `good`). Set comfortably above any faction's
/// realistic total port capacity (`IMPORT_PER_PORT` times the map's largest
/// port line) so it's a safety ceiling, not a routine constraint.
pub const IMPORT_PLAN_RATE_MAX: f32 = 50.0;

/// Machinery spent, from the importing faction's national stock, per unit of
/// Food/Energy actually imported (docs/phase2-spec.md: "輸入は無償ではない。
/// Machinery を輸出して支払う"). Kept low relative to `ARMS_INPUT_MACHINERY`/
/// `CONSTRUCTION_MACHINERY_PER_POINT` so a Machinery-rich, Food-poor faction
/// (the Stage 2A structural-famine case) can afford a meaningful import flow
/// out of ordinary production, not just an idle stockpile.
pub const IMPORT_COST_MACHINERY_PER_GOOD: f32 = 0.3;

/// Stage 2C node-side supply throughput cap (docs/phase2-spec.md "2. 港湾・
/// インフラによるノード側の上限"): `node_throughput(region) = NODE_BASE +
/// region.effective_infrastructure() * NODE_INFRA + region.port *
/// NODE_PORT`, applied as an extra `min()` term in
/// `logistics::recompute_supply`'s propagation alongside the existing link
/// `max_throughput()`. A region with no infrastructure and no port can still
/// relay a trickle (`NODE_BASE`); a fully-developed, high-port hub can relay
/// close to a Rail link's own ceiling (`LinkKind::Rail::max_throughput() ==
/// 25.0`), so the node cap bites mainly on devastated or underdeveloped
/// relay points, not on every link uniformly.
pub const NODE_BASE: f32 = 3.0;
pub const NODE_INFRA: f32 = 14.0;
pub const NODE_PORT: f32 = 4.0;

/// Stage 2C per-commodity delivery (docs/phase2-spec.md "3. 品目別の到達率"):
/// converts a unit's equipment gap (`UNIT_EQUIPMENT - unit.equipment`) into
/// an Arms delivery-flow demand on the same regional throughput Munitions
/// upkeep already contends for, on a comparable scale to
/// `SUPPLY_NEED_PER_MANPOWER` - a unit at its full `UNIT_EQUIPMENT` (20.0)
/// gap wants a flow of `20.0 * 0.1 == 2.0`, in the same order of magnitude
/// as one unit's peacetime (1.0) to in-combat (2.5) Munitions demand, so
/// `Faction::logistics_priority` has real contention to split rather than
/// one side dwarfing the other by construction.
pub const ARMS_SUPPLY_NEED_PER_GAP: f32 = 0.1;

/// Stage 2D (docs/phase2-spec.md "Stage 2D — 海軍・制海権・海上封鎖"): days for
/// a fleet to cross into an adjacent sea zone at full speed (no `LinkKind`
/// exists for sea-zone adjacency to derive a figure from) — set between
/// `LinkKind::Road::travel_days()` (3.0) and `LinkKind::Sea::travel_days()`
/// (6.0), the fastest and slowest land-facing figures, since open-ocean
/// fleet transit is neither as quick as a road march nor as slow as cargo
/// crossing a sea link end-to-end.
pub const FLEET_MOVE_DAYS: f32 = 4.0;

/// Naval combat's analogue of `COMBAT_DAMAGE` (docs/phase2-spec.md "3. 海戦":
/// "地形補正はなく、代わりに NAVAL_DAMAGE を用いる"). Kept equal to
/// `COMBAT_DAMAGE` — sea zones have no terrain to apply a defense bonus
/// through, so naval combat's damage scale doesn't need to be re-tuned
/// independently of land's; it only needs its own named constant so the
/// systems that use it don't share a single knob across both domains.
pub const NAVAL_DAMAGE: f32 = 8.0;

/// Sea-control threshold (docs/phase2-spec.md "2. 港の封鎖", "1. 海峡リンクの
/// 遮断") past which a faction's presence in a sea zone counts as a real
/// blockade of the ports/straits touching it, rather than a token patrol
/// that happens to have inflicted a little damage. Set well above "any
/// nonzero control" so a handful of skirmishing fleets can't flip a port's
/// import on and off; a faction needs a clear majority of the zone's naval
/// power to choke it.
pub const BLOCKADE_CONTROL_THRESHOLD: f32 = 0.6;

// ---------------------------------------------------------------------------
// Stage 3A — 国内政治勢力 (docs/phase3-spec.md "Stage 3A"): the seven
// `Group`s' support, `stability`'s redefinition as their influence-weighted
// average, and the six political events. Every situational contribution
// below is a *bounded* term added to a group's target support, never a raw
// accumulation onto `support` itself — the same target-approach discipline
// `UNREST_ADAPT_RATE` already established, now applied to something with
// far more simultaneous inputs.
// ---------------------------------------------------------------------------

/// Daily rate at which each `Faction::group_support[g]` closes the gap to
/// its freshly recomputed target (`politics::tick_politics`) — the same
/// target-approach shape as `UNREST_ADAPT_RATE`, so a
/// group's support can always recover once the pressure driving it down
/// eases, and never pins at 0 or 100 the way a raw accumulator would.
pub const GROUP_ADAPT_RATE: f32 = 0.04;

/// The target every group's support gravitates to before any situational
/// contribution is added (docs/phase3-spec.md "支持の更新": `target[g] = 50 +
/// Σ...`). Also `Faction::stability`'s value immediately after
/// `Event::RegimeChange` resets every group to this baseline (a uniform
/// reset makes the influence-weighted `stability` land exactly here too,
/// since `Faction::group_influence` always sums to 1.0).
pub const GROUP_SUPPORT_BASELINE: f32 = 50.0;

/// `conscription` (0..1) contribution: Military gains, Labor and Citizens
/// lose, scaled linearly by the policy's own value.
pub const GROUP_CONSCRIPTION_MILITARY_BONUS: f32 = 12.0;
pub const GROUP_CONSCRIPTION_LABOR_PENALTY: f32 = 8.0;
pub const GROUP_CONSCRIPTION_CITIZENS_PENALTY: f32 = 6.0;

/// `civilian_ration` being low (docs/phase3-spec.md: "civilian_ration が低
/// い") contribution, driven by how far the ration sits below
/// `CIVILIAN_RATION_MAX` relative to its full `CIVILIAN_RATION_MIN..MAX`
/// range (0 at full ration, 1 at the floor) — Military gains, Citizens lose
/// heavily (the spec's "−−"), Labor loses moderately.
pub const GROUP_RATION_MILITARY_BONUS: f32 = 8.0;
pub const GROUP_RATION_CITIZENS_PENALTY: f32 = 16.0;
pub const GROUP_RATION_LABOR_PENALTY: f32 = 6.0;

/// `industry_priority` leaning toward war matériel (docs/phase3-spec.md:
/// "industry_priority が Arms 寄り"). `industry_priority` has no direct
/// `Arms` weight to read (Arms output is capped by leftover Machinery/Steel
/// stock, not a contended input share — see `economy::tick_economy`), so
/// this reads the closest real signal: how much more of the shared Steel/
/// Energy budget is steered toward `Munitions` (war matériel) than toward
/// `Machinery` (civilian/industrial goods), clamped to `0..1` since only a
/// *positive* lean toward war production should count. Military and
/// Business gain, Citizens lose.
pub const GROUP_ARMS_LEAN_MILITARY_BONUS: f32 = 6.0;
pub const GROUP_ARMS_LEAN_BUSINESS_BONUS: f32 = 5.0;
pub const GROUP_ARMS_LEAN_CITIZENS_PENALTY: f32 = 5.0;

/// `Faction::shortage` (0..1) contribution: Citizens lose heavily (the
/// spec's "−−"), Labor and Government lose moderately.
pub const GROUP_SHORTAGE_CITIZENS_PENALTY: f32 = 20.0;
pub const GROUP_SHORTAGE_LABOR_PENALTY: f32 = 10.0;
pub const GROUP_SHORTAGE_GOVERNMENT_PENALTY: f32 = 8.0;

/// Average owned-region `unrest` (0..100, read as a `0..1` fraction)
/// contribution: LocalGovernment loses heavily (the spec's "−−", it answers
/// for local order directly), Government loses moderately.
pub const GROUP_UNREST_LOCALGOV_PENALTY: f32 = 22.0;
pub const GROUP_UNREST_GOVERNMENT_PENALTY: f32 = 10.0;

/// Average owned-region `devastation` (0..1) contribution: LocalGovernment
/// and Business both lose — war damage is a local-administration and an
/// economic problem before it's a national-government one.
pub const GROUP_DEVASTATION_LOCALGOV_PENALTY: f32 = 10.0;
pub const GROUP_DEVASTATION_BUSINESS_PENALTY: f32 = 12.0;

/// Normalizer for the day's manpower casualties (docs/phase3-spec.md: "そ
/// の日の戦死が多い"), in the same 万人/day units `WAR_SUPPORT_CASUALTY_MULT`
/// already uses — the daily loss that maxes out this contribution's `0..1`
/// factor. Set to half a fresh unit's full `UNIT_MANPOWER` (1.0) so a single
/// hard-fought battle's losses are already a meaningful jolt, not something
/// that needs a multi-unit wipeout to register.
pub const GROUP_CASUALTY_NORM: f32 = 0.5;
pub const GROUP_CASUALTY_MILITARY_PENALTY: f32 = 10.0;
pub const GROUP_CASUALTY_CITIZENS_PENALTY: f32 = 8.0;
pub const GROUP_CASUALTY_GOVERNMENT_PENALTY: f32 = 6.0;

/// Normalizer for the day's net region-count change (docs/phase3-spec.md:
/// "領土を得た"/"領土を失った"): the single-region flip that already maxes
/// out the `0..1` gain/loss factor — territory rarely changes hands faster
/// than one region at a time in a single tick, so this is a ceiling, not a
/// typical case.
pub const GROUP_TERRITORY_DELTA_CAP: f32 = 1.0;
pub const GROUP_TERRITORY_GAIN_MILITARY_BONUS: f32 = 6.0;
pub const GROUP_TERRITORY_GAIN_GOVERNMENT_BONUS: f32 = 6.0;
pub const GROUP_TERRITORY_LOSS_MILITARY_PENALTY: f32 = 8.0;
/// The spec's "−−" for a territorial loss: Government answers for losing
/// ground more harshly than Military does.
pub const GROUP_TERRITORY_LOSS_GOVERNMENT_PENALTY: f32 = 14.0;

/// `stock[Arms]` being ample (docs/phase3-spec.md: "stock[Arms] が潤沢")
/// contribution: normalized against a multiple of `UNIT_EQUIPMENT` (a full
/// unit's equipment draw) so the term reads as "how many fresh units' worth
/// of Arms are sitting in reserve," capped at `0..1`.
pub const GROUP_ARMS_STOCK_MARGIN: f32 = 3.0;
pub const GROUP_ARMS_STOCK_MILITARY_BONUS: f32 = 6.0;

/// Machinery production running well (docs/phase3-spec.md: "生産（Machinery）
/// が好調") contribution: `Faction::machinery_output_ratio` (today's actual
/// Machinery output over its input-unconstrained potential, already `0..1`)
/// scaled straight through — Business gains when the chain isn't
/// input-starved.
pub const GROUP_MACHINERY_GOOD_BUSINESS_BONUS: f32 = 6.0;

/// Stage 3A political events (docs/phase3-spec.md "政治イベント"): support
/// thresholds below which each event triggers, and the effect sizes/
/// durations each one applies. Every one of these is designed to be
/// recoverable — a live condition re-evaluated every tick (Protest, Mutiny,
/// CapitalFlight, Separatism) ends the instant support crosses back above
/// threshold, and a fixed-duration event (Strike, RegimeChange) always ends
/// on its own after its day count, even if it can start again right after.

/// Labor support threshold for `Event::Strike`.
pub const STRIKE_THRESHOLD: f32 = 38.0;
/// Fixed duration `Event::Strike` depresses industrial output for, once
/// triggered — a real strike doesn't end the instant Labor support ticks
/// back over the threshold; it runs its course.
pub const STRIKE_DAYS: u32 = 15;
/// Multiplier applied to every non-`Food` commodity's potential output
/// while a strike is active (`economy::tick_economy`) — `Food` is exempted
/// because a labor strike is an industrial-workforce action, not a farming
/// one.
pub const STRIKE_OUTPUT_MULT: f32 = 0.7;

/// Citizens support threshold for `Event::Protest`.
pub const PROTEST_THRESHOLD: f32 = 38.0;
/// Extra unrest-target pressure (`politics::tick_politics`, same units as
/// `UNREST_SHORTAGE_PRESSURE`/`UNREST_SUPPLY_PRESSURE`) applied to every
/// region a faction under active protest owns, for as long as Citizens
/// support stays below `PROTEST_THRESHOLD`.
pub const PROTEST_UNREST_BONUS: f32 = 15.0;

/// Military support threshold for `Event::Mutiny`.
pub const MUTINY_THRESHOLD: f32 = 32.0;
/// Multiplier applied to `ORG_REGEN` (`military::tick_recovery`) for every
/// unit owned by a faction under active mutiny, for as long as Military
/// support stays below `MUTINY_THRESHOLD`.
pub const MUTINY_ORG_REGEN_MULT: f32 = 0.4;

/// Business support threshold for `Event::CapitalFlight`.
pub const CAPITAL_FLIGHT_THRESHOLD: f32 = 32.0;
/// Multiplier applied to a faction's construction throughput
/// (`construction::tick_construction`) while capital flight is active.
pub const CAPITAL_FLIGHT_CONSTRUCTION_MULT: f32 = 0.5;
/// Multiplier applied to Machinery's potential output
/// (`economy::tick_economy`) while capital flight is active.
pub const CAPITAL_FLIGHT_MACHINERY_MULT: f32 = 0.7;

/// `stability` threshold for `Event::RegimeChange`.
pub const REGIME_CHANGE_THRESHOLD: f32 = 40.0;
/// Fixed duration the post-coup production disruption lasts, and the
/// cooldown before a *new* regime change can trigger for the same faction —
/// consecutive collapses are possible if the underlying squeeze continues,
/// but never faster than once every `REGIME_CHANGE_DAYS`.
pub const REGIME_CHANGE_DAYS: u32 = 30;
/// Multiplier applied to *every* commodity's potential output (including
/// `Food` — unlike `STRIKE_OUTPUT_MULT`, a change of government disrupts the
/// whole economy, not just industrial labor) while the post-coup disruption
/// is in effect.
pub const REGIME_CHANGE_OUTPUT_MULT: f32 = 0.75;

/// LocalGovernment support threshold for `Event::Separatism` (checked
/// against the *occupying* faction's own LocalGovernment support, for each
/// region it holds where `core != owner`).
pub const SEPARATISM_THRESHOLD: f32 = 42.0;
/// Daily progress (docs/phase3-spec.md: "occupation が core 勢力に向かって進
/// む"), added to `Region::occupation` toward reverting to `Region::core`,
/// while separatism is active in an occupied region with no units of any
/// faction physically present (see `politics::tick_separatism`). Kept below
/// `OCCUPATION_RATE` — this is a political drift, not a military conquest.
pub const SEPARATISM_RATE: f32 = 4.0;
/// Daily recovery of that same progress once LocalGovernment support climbs
/// back above `SEPARATISM_THRESHOLD` — the same recoverability every other
/// Stage 3A event guarantees.
pub const SEPARATISM_DECAY: f32 = 4.0;

/// External code review fix (Stage 3A, Fix 2/3): a garrison the owner keeps
/// stationed in a region under active separatism no longer vetoes the drift
/// outright (the old behaviour let holding *any* garrison there freeze the
/// meter completely, which — combined with Fix 3's playtested absorbing
/// state, docs/phase3-spec.md §0 — meant an over-extended faction could hold
/// a hostile, starving region forever for free). Instead
/// `politics::tick_separatism` scales `SEPARATISM_RATE` down by how strong
/// that garrison is *relative to the region's population and unrest* — a
/// small garrison in a big, restless region barely slows the drift; a large
/// one in a small, calm region can suppress it close to (but never all the
/// way to) a standstill. `garrison_power` is the owner's own units'
/// `combat_power()` summed in the region (zero when none are present, which
/// is exactly the old fully-unsuppressed case: `SEPARATISM_GARRISON_POP_NORM`
/// and the unrest term below both multiply a zero garrison to zero
/// suppression, so `separatist_returns_occupied_region`'s no-garrison timing
/// is unchanged).
///
/// `suppression = (garrison_power / (population * (1 +
/// unrest/100 * SEPARATISM_UNREST_GARRISON_MULT) * SEPARATISM_GARRISON_POP_NORM))
/// .clamp(0, SEPARATISM_GARRISON_MAX_SUPPRESSION)`, and the effective daily
/// rate is `SEPARATISM_RATE * (1 - suppression)`. Sized so a couple of
/// full-strength units (`combat_power` on the order of 0.5-1.0 each) meaningfully
/// suppress separatism in one of the map's smaller regions (population in the
/// low hundreds) but barely register against one of its largest urban
/// centers (population in the thousands) — holding down a big, hostile
/// population takes a correspondingly bigger garrison, with the manpower/
/// supply cost that implies (design.md §9's trade-off, not a free lever).
pub const SEPARATISM_GARRISON_POP_NORM: f32 = 0.002;
/// How much each point of regional `unrest` (0..100) raises the effective
/// population a garrison must suppress, at `unrest == 100` scaling it up by
/// this fraction (docs/phase3-spec.md's target-approach discipline: a
/// restless population is harder to hold down than a calm one of the same
/// size).
pub const SEPARATISM_UNREST_GARRISON_MULT: f32 = 1.0;
/// Ceiling on how much a garrison can suppress the separatist drift rate —
/// never 1.0, so holding a region against separatism is always eventually
/// contested by *something*, per Fix 3's "slow, not stop" mandate and the
/// project's standing rule against absorbing states (docs/phase3-spec.md §0).
pub const SEPARATISM_GARRISON_MAX_SUPPRESSION: f32 = 0.85;

// ---------------------------------------------------------------------------
// Stage 3B — 外交関係と条約 (docs/phase3-spec.md "Stage 3B"): `Stance`, the
// asymmetric `opinion` matrix, the six `Treaty` kinds, the one-tick
// pending-proposal queue, and `TradeAgreement`'s surplus-to-deficit flow.
// Per §0's carried-forward rules: `opinion` moves toward a target (0) rather
// than accumulating one-way, so no pair of factions can reach a diplomatic
// state neither can ever recover from; every treaty-affecting action is
// either idempotent (re-proposing/re-accepting an already-active treaty is
// rejected outright) or metered by a real, decrementing cooldown, never a
// ratio re-applied to a shrinking remainder - see `diplomacy.rs`'s module
// doc for how each action is made abuse-resistant against a reward
// optimiser spamming it every tick.
// ---------------------------------------------------------------------------

/// Daily rate at which `Diplomacy::opinion[a][b]` closes the gap toward its
/// 0 baseline (the same target-approach shape as `GROUP_ADAPT_RATE`/
/// `UNREST_ADAPT_RATE`) - every opinion swing from a treaty being signed,
/// broken, or a war being declared is a bounded one-time delta, not a
/// permanent shift, so no relationship can be driven to -100 and pinned
/// there forever.
pub const OPINION_DECAY_RATE: f32 = 0.03;

/// Opinion gained, both directions, when a proposed treaty is accepted
/// (`action::apply_accept_treaty`).
pub const TREATY_ACCEPT_OPINION_BONUS: f32 = 15.0;

/// Opinion lost, both directions, when `Action::DeclareWar` breaks a
/// `Ceasefire` (docs/phase3-spec.md: "いつでも DeclareWar で破棄できる") - the
/// single largest diplomatic penalty, since ending a truce outright is the
/// most hostile bilateral act short of the war itself.
pub const DECLARE_WAR_OPINION_PENALTY: f32 = 40.0;

/// Notice period (days) `Action::BreakTreaty` on a `NonAggression` pact must
/// serve before the pair actually reverts to `Stance::War`
/// (docs/phase3-spec.md: "破棄には NON_AGGRESSION_NOTICE_DAYS の予告が要る") -
/// tracked per pair in `Diplomacy::pending_breaks` and counted down by
/// `diplomacy::tick_diplomacy`; combat and occupation stay suppressed for
/// the whole notice period, since the stance itself doesn't change until it
/// elapses.
pub const NON_AGGRESSION_NOTICE_DAYS: u32 = 10;
/// Opinion lost, both directions, the moment notice is served (not when the
/// war actually starts `NON_AGGRESSION_NOTICE_DAYS` later) - breaking the
/// pact is the diplomatic act; the war that follows is `DECLARE_WAR`'s own
/// betrayal-scale penalty, not repeated here.
pub const NON_AGGRESSION_BREAK_OPINION_PENALTY: f32 = 20.0;

/// Opinion lost, both directions, when `Action::BreakTreaty` ends an
/// `Alliance` (immediate: stance falls back to `Ceasefire`, not war - an
/// alliance ending is a diplomatic rupture, not a declaration of war).
pub const ALLIANCE_BREAK_OPINION_PENALTY: f32 = 30.0;

/// Opinion lost, both directions, when `Action::BreakTreaty` cancels a
/// `MilitaryAccess`/`PortAccess`/`TradeAgreement` grant - the smallest
/// break penalty, since these are working arrangements, not the core
/// war/peace relationship.
pub const MINOR_TREATY_BREAK_OPINION_PENALTY: f32 = 10.0;

/// Days a (pair, `Treaty` kind) combination is locked out of
/// `Action::ProposeTreaty` after that treaty was broken (`BreakTreaty`) or,
/// for `Ceasefire` specifically, after `Action::DeclareWar` ends one - the
/// abuse-resistance guard named in docs/phase3-spec.md §0: without a real,
/// decrementing cooldown a reward optimiser could cycle
/// propose-accept-break every tick to keep re-harvesting
/// `TREATY_ACCEPT_OPINION_BONUS`, or declare war and instantly re-propose
/// `Ceasefire` to dodge `NonAggression`'s notice period in spirit.
pub const TREATY_COOLDOWN_DAYS: u32 = 20;

/// How many days a `PendingProposal` survives, once created, before
/// `diplomacy::tick_diplomacy` expires it unanswered
/// (docs/phase3-spec.md: "提案は 1 tick 保留され、相手の応答を待つ"). Not a
/// spendable resource of its own - re-proposing after expiry costs nothing
/// beyond the normal cooldown/duplicate-proposal rules, so a genuinely
/// interested counterpart is never permanently locked out by one missed day.
///
/// External code review fix (C1/C2 re-audit): kept above the bare "1 tick"
/// the spec line names, because a literal 1-day hold combined with
/// `archipelago-agents`' `HeuristicAgent` (each faction only decides once
/// every `period` days, offset by faction id so they don't all act the same
/// day) makes some ordered pairs structurally unable to ever answer a
/// proposal in time - not a rare edge case: with `period == 4` and 3
/// factions at offsets `0`/`1`/`2`, a proposal from the offset-`2` faction
/// to the offset-`0` one needs 2 days before that target's next turn, but a
/// 1-day TTL is already gone by then, so *every* proposal in that direction
/// (and the reverse) silently expired unanswered, regardless of how
/// favorable its terms were. `3` comfortably covers the worst gap any
/// small, offset-staggered set of agents can produce against this same
/// period without over-extending how long a stale offer lingers.
pub const PROPOSAL_TTL_DAYS: u32 = 3;

/// Fraction of a `TradeAgreement` exporter's own stock of a tradeable good
/// (`Food`/`Energy`/`Machinery`) that is reserved for its own domestic use
/// before any of it counts as exportable surplus
/// (`trade::tick_trade_agreements`): `surplus = stock * (1 -
/// TRADE_SURPLUS_RESERVE_FRACTION)`. A fraction of current stock rather than
/// a flat number so the reserve scales sensibly across goods that start at
/// very different stockpile levels (`scenario::FACTION_STOCK`).
pub const TRADE_SURPLUS_RESERVE_FRACTION: f32 = 0.5;

/// Ceiling on how much of one tradeable good one `TradeAgreement` partner
/// can pull from another in a single day, at the importer's `shortage_by_good`
/// fully saturated (`== 1.0`) - kept on the same order as `IMPORT_PER_PORT`
/// (world-market imports' own per-port ceiling) so a trade partnership is a
/// meaningful substitute for (or complement to) the world market, not a
/// dominant or negligible one.
pub const TRADE_FLOW_RATE_MAX: f32 = 6.0;

/// External code review-style fix, applied up front (docs/phase3-spec.md §0:
/// "複数の主体が奪い合う量は必ず比率で按分する"): `TradeAgreement` inflow and
/// world-market import inflow for the same importing faction both draw on
/// the *same* pooled port capacity (`trade::tick_imports` /
/// `trade::tick_trade_agreements`'s shared `capacity[f]`, which
/// `Treaty::PortAccess` can extend with a partner's own ports). Neither is
/// computed first and the other given only the leftover - both wanted
/// amounts are summed, and if they exceed capacity both are scaled down by
/// the same ratio, `capacity / (world_wanted + trade_wanted)`. No named
/// constant is needed for the split itself; `trade::tick_imports` and
/// `trade::tick_trade_agreements` both implement this rule.

/// External code review fix (Stage 2D): floor on `naval::strait_factor`
/// (`1 - enemy_control_max`) when it is applied to a crossing's per-tick
/// movement progress in `military::tick_movement`. `strait_factor` is
/// already `.clamp(0.0, 1.0)`, so this is not a divide-by-zero guard — it
/// exists so a unit mid-crossing under a total (or near-total) blockade
/// still creeps forward at a slow trickle each tick instead of making
/// literally zero progress, keeping the per-tick math finite and away from
/// the degenerate all-progress-happens-in-one-instant edge a bare `* 0.0`
/// would produce every tick it's fully blockaded.
pub const STRAIT_CROSSING_FACTOR_FLOOR: f32 = 0.05;

// ---------------------------------------------------------------------------
// Stage 3C — 国家方針 (docs/phase3-spec.md "Stage 3C — 国家方針"): the six
// `NationalFocus` variants and their per-focus modifiers. Every modifier
// below is read only through `focus::active()`, which returns `None` for a
// faction mid-`FOCUS_SWITCH_DAYS` transition — see `focus.rs`'s module doc
// for why that (and `action::apply_set_national_focus`'s no-retarget-mid-
// transition rule) is what keeps `Action::SetNationalFocus` un-spammable.
// ---------------------------------------------------------------------------

/// Days a `NationalFocus` switch takes to settle (docs/phase3-spec.md:
/// "変更には FOCUS_SWITCH_DAYS の移行期間があり、その間は効果が出ない") - a
/// real, decrementing `Faction::focus_transition_days` counted down once a
/// day by `focus::tick_national_focus`, during which `focus::active` returns
/// `None` for this faction (neither the abandoned focus's effects nor the
/// new one's apply). Kept on the same order as `STRIKE_DAYS`/
/// `TREATY_COOLDOWN_DAYS` - long enough that a focus is a real commitment,
/// short enough that a faction reacting to a genuine crisis (see
/// `archipelago-agents`' major-change switching) isn't locked out for an
/// unreasonable fraction of a 720-day run.
pub const FOCUS_SWITCH_DAYS: u32 = 20;

/// `NationalFocus::MilitaryUnification` (docs/phase3-spec.md: "Military 支持
/// ＋、部隊の組織率上限＋、Citizens 支持 −"): `politics::tick_politics`
/// group-support target contributions, on the same order as
/// `GROUP_ARMS_LEAN_MILITARY_BONUS`/`GROUP_ARMS_LEAN_CITIZENS_PENALTY`.
pub const FOCUS_MILITARY_SUPPORT_BONUS: f32 = 8.0;
pub const FOCUS_MILITARY_CITIZENS_PENALTY: f32 = 6.0;
/// Multiplier on `UNIT_ORG` used as the organization ceiling
/// `military::tick_recovery` clamps every unit of a MilitaryUnification
/// faction to, in place of the plain `UNIT_ORG` every other faction's units
/// are capped at - raising what `Unit::organization` (and therefore
/// `Unit::org_ratio`/`Unit::combat_power`, both still dividing by the fixed
/// `UNIT_ORG`) can actually reach, rather than changing the ratio formula
/// itself.
pub const FOCUS_MILITARY_ORG_CAP_MULT: f32 = 1.15;

/// `NationalFocus::EconomicSphere` (docs/phase3-spec.md: "Business 支持 ＋、
/// TradeAgreement の流量 ＋"): group-support bonus, and the multiplier
/// `trade::tick_imports` applies to `TRADE_FLOW_RATE_MAX` for a
/// `TradeAgreement` flow where either side has this focus active (the higher
/// of the two, so one economically-focused partner is enough to grow the
/// flow - never double-counted when both sides have it).
pub const FOCUS_ECONOMIC_BUSINESS_SUPPORT_BONUS: f32 = 8.0;
pub const FOCUS_ECONOMIC_TRADE_FLOW_MULT: f32 = 1.5;

/// `NationalFocus::AllianceNetwork` (docs/phase3-spec.md: "外交提案の受諾さ
/// れやすさ＋、opinion の回復＋"): multiplier `diplomacy::tick_diplomacy`
/// applies to `OPINION_DECAY_RATE` for `a`'s opinion of `b` specifically
/// while that opinion is negative (i.e. "recovery" toward neutral, not a
/// faster erosion of an already-good relationship) when `a` has this focus
/// active. The acceptance-ease half of the effect is an AI-side knob
/// (`archipelago-agents`'s `FOCUS_ALLIANCE_ACCEPT_BONUS`), not a sim number.
pub const FOCUS_ALLIANCE_OPINION_RECOVERY_MULT: f32 = 1.6;

/// `NationalFocus::MaritimeTrade` (docs/phase3-spec.md: "港湾の輸入容量 ＋、
/// 艦隊の建造コスト −"): multiplier `trade::tick_imports` applies to a
/// MaritimeTrade faction's own per-port import capacity, and the multiplier
/// `action::apply_recruit` applies to `UNIT_EQUIPMENT`'s Arms cost when
/// building a `Domain::Sea` unit under this focus (land recruits are
/// unaffected - this discounts fleets specifically, not army equipment in
/// general).
pub const FOCUS_MARITIME_IMPORT_CAPACITY_MULT: f32 = 1.3;
pub const FOCUS_MARITIME_FLEET_COST_MULT: f32 = 0.75;

/// `NationalFocus::Technocracy` (docs/phase3-spec.md: "Bureaucracy 支持 ＋、
/// 建設速度 ＋、生産効率 ＋"): group-support bonus, the multiplier
/// `construction::tick_construction` applies to `CONSTRUCTION_RATE` (stacks
/// multiplicatively with `CAPITAL_FLIGHT_CONSTRUCTION_MULT` the same way
/// every other independent rate multiplier in that function does), and the
/// multiplier `economy::tick_economy` applies to every commodity's potential
/// output alongside `stability_mult`/`regime_change_mult`.
pub const FOCUS_TECHNOCRACY_BUREAUCRACY_SUPPORT_BONUS: f32 = 8.0;
pub const FOCUS_TECHNOCRACY_CONSTRUCTION_RATE_MULT: f32 = 1.3;
pub const FOCUS_TECHNOCRACY_PRODUCTION_MULT: f32 = 1.1;

/// `NationalFocus::DefensivePosture` (docs/phase3-spec.md: "自領での防御補正
/// ＋、戦災の回復速度 ＋、攻勢時の補正 −"): `military::tick_combat`'s
/// per-side power multiplier, layered on top of `Terrain::defense_bonus`,
/// when this faction is the region's defender *and* the region is its own
/// `core` territory (never on merely-occupied land — "自領" is home soil
/// specifically); the offense penalty applies whenever this faction is
/// present in a battle but is *not* the defender there, regardless of whose
/// territory it is. `construction::tick_devastation_recovery`'s multiplier
/// applies unconditionally to a DefensivePosture faction's own
/// `Region::devastation` recovery, home soil or not - rebuilding faster
/// everywhere is the whole point of a defense-oriented economy.
pub const FOCUS_DEFENSIVE_HOME_DEFENSE_MULT: f32 = 1.25;
pub const FOCUS_DEFENSIVE_DEVASTATION_RECOVERY_MULT: f32 = 1.5;
pub const FOCUS_DEFENSIVE_OFFENSE_PENALTY_MULT: f32 = 0.85;

// ---------------------------------------------------------------------------
// Stage 4B — 自然言語外交 (docs/phase4-spec.md "Stage 4B — 自然言語外交"):
// `Action::ProposeInNaturalLanguage`/`RespondToNaturalLanguageProposal` and
// the `TreatyTerm`s an interpretation (LLM or keyword fallback, both in
// `archipelago-agents`) collapses into. Per docs/phase3-spec.md §0's
// carried-forward rule about a 1-tick allowance being a spent budget, never
// a ratio reapplied to a remainder: answering a natural-language proposal
// (accepted, rejected, or left to expire) spends a real, decrementing
// `NL_PROPOSAL_COOLDOWN_DAYS` on that `(from, to)` pair, the same shape
// `TREATY_COOLDOWN_DAYS` already gives ordinary treaty proposals - see
// `diplomacy.rs`'s module doc for the exact exploit shapes this closes.
// ---------------------------------------------------------------------------

/// How many days a `PendingNlProposal` survives, once created, before
/// `diplomacy::tick_diplomacy` expires it unanswered - the free-text
/// counterpart of `PROPOSAL_TTL_DAYS`, kept at the same value for the same
/// reason (`PROPOSAL_TTL_DAYS`'s own doc: it must outlive the worst gap a
/// small, offset-staggered set of `HeuristicAgent`s can leave between a
/// proposal landing and the target's next turn).
pub const NL_PROPOSAL_TTL_DAYS: u32 = 3;

/// Days a `(from, to)` pair is locked out of a fresh
/// `Action::ProposeInNaturalLanguage` after their last one was answered
/// (accepted, rejected, or resolved as terms-invalid) or expired unanswered
/// (`diplomacy::respond_nl`/`tick_diplomacy`) - the abuse-resistance guard
/// this whole section's doc names: without it, a proposer could re-submit a
/// slightly-reworded natural-language text every single day, each attempt
/// re-triggering interpretation (a real backend call, for an `LlmAgent`
/// recipient) and, on every lucky "accept", another
/// `TREATY_ACCEPT_OPINION_BONUS`-equivalent payout via a `Sign` term. Kept
/// close to `TREATY_COOLDOWN_DAYS` so natural-language diplomacy isn't a
/// structurally cheaper way to farm the same reward `ProposeTreaty` already
/// guards.
pub const NL_PROPOSAL_COOLDOWN_DAYS: u32 = 20;

/// Ceiling on a `Action::ProposeInNaturalLanguage`'s `text` length, in
/// characters - `action::apply_propose_nl` rejects anything longer outright.
/// Purely a defence-in-depth bound against a pathologically large string
/// reaching `Diplomacy::pending_nl`/the event log (and, for an `LlmAgent`
/// recipient, the prompt built from it) - ordinary proposals, in any
/// language, sit far below this.
pub const NL_PROPOSAL_TEXT_MAX_CHARS: usize = 500;

// ---------------------------------------------------------------------------
// Stage 9B (docs/phase9-spec.md "2. 補給を有限流量にする"): the transport-
// network flow model that replaces best-path bottleneck reachability.
// ---------------------------------------------------------------------------

/// Share of a region's `industry_total()` injected into the transport
/// network as its own supply source - extracted, unchanged in value, from
/// the pre-Stage-9B `Region::supply_source_blockaded`'s inline `* 0.5`, so
/// swapping the routing model alone doesn't also silently change how much a
/// region can inject to begin with.
pub const INDUSTRY_SUPPLY_SHARE: f32 = 0.5;

/// Supply injected into the transport network per point of `Region::port`,
/// at that region's own `Port` node(s), zeroed when the port is blockaded
/// (`naval::is_port_blockaded`) - extracted, unchanged in value, from the
/// pre-Stage-9B `Region::supply_source_blockaded`'s inline `* 4.0`.
pub const PORT_SUPPLY_PER_PORT: f32 = 4.0;

/// Fixed number of synchronous (Jacobi-style: every round's allocation is
/// computed from the *previous* round's residual state and committed only
/// once the whole round has been evaluated) proportional-flow rounds
/// `logistics::recompute_supply` runs before stopping - never a float
/// convergence test, so the same seed always runs exactly this many rounds
/// (docs/phase9-spec.md "2. 決定論": "打ち切り条件を反復回数で固定する").
/// The network is sparse (japan_hex: 468 nodes / 853 lines, roughly planar),
/// so this is cheap - a fixed `SUPPLY_FLOW_ROUNDS * lines` pass, not a search.
/// Chosen well above the map's own diameter in *rounds* (each round's BFS
/// already spans an entire source-to-sink path in one pass; rounds are only
/// needed to re-shake residual capacity between competing sinks after a
/// commit, not to cross more hops) so repeated contention between sinks
/// sharing a chokepoint has room to settle before the cutoff.
///
/// **Measured, not guessed.** japan_hex seed 1, 720 days, mean
/// `supply_ratio` over the surviving factions: 24 rounds → 0.3223,
/// 48 → 0.3254, 96 → 0.3254. 48 and 96 agree to every printed digit for
/// every faction, so the scheme has fully settled by 48 and the cutoff is
/// not what limits how well a faction is supplied — 24 lands within about
/// 1% of that. Raising it buys a fraction of a percent for twice the work,
/// so it stays at 24. **Do not raise this hoping to fix a starving
/// faction**: the measurement above says the round count is not the cause.
pub const SUPPLY_FLOW_ROUNDS: usize = 24;

/// Floor below which a flow-graph edge/vertex budget is treated as
/// exhausted for this tick - guards the proportional-scaling division
/// (`granted / total_desired`) against float noise turning an already-spent
/// resource into a divide-by-near-zero.
pub const SUPPLY_FLOW_EPSILON: f32 = 1e-4;

/// Daily `Condition` lost by a `TransportLine` touching a contested region
/// (`World::has_enemy_units` true for either endpoint's region) - war damage
/// interdicting the line, docs/phase9-spec.md "輸送路線": "戦災・遮断で下がり".
pub const LINE_CONDITION_DAMAGE_PER_TICK: f32 = 0.03;

/// Daily `Condition` recovered by a `TransportLine` touching no contested
/// region - the required recovery path (docs/phase9-spec.md "輸送路線":
/// "回復経路を持つ"; CLAUDE.md's「繰り返し踏んだ欠陥」: "状態には必ず回復経路
/// を持たせる"). Slower than the damage rate, so a line that spent a long
/// siege near zero takes a real stretch of peace to fully recover, not one
/// quiet tick.
pub const LINE_CONDITION_REPAIR_PER_TICK: f32 = 0.015;

/// Stage 9D (docs/phase9-spec.md "4. 行動"): `Condition` lost in one
/// `Action::InterdictLine` - a deliberate, targeted strike, so meaningfully
/// larger than a single day of `LINE_CONDITION_DAMAGE_PER_TICK`'s passive
/// contested-region wear, but well short of severing a healthy line outright
/// in one action - repeated interdiction (or ongoing contest at an endpoint)
/// is what actually cuts a route, not one order.
pub const LINE_INTERDICTION_DAMAGE: f32 = 0.2;

/// Building points required to complete `construction::Project::
/// TransportLine` (`construction::required_points`) - between `Repair`
/// (`CONSTRUCTION_REQUIRED_REPAIR`, the cheapest existing project) and
/// `Capacity` (`CONSTRUCTION_REQUIRED_CAPACITY`): restoring a route is real
/// infrastructure work, not the free, everyday drip `LINE_CONDITION_REPAIR_
/// PER_TICK` already does for free once a region stops being contested, but
/// it is not a bigger undertaking than rebuilding the region's own
/// devastation.
pub const CONSTRUCTION_REQUIRED_TRANSPORT_LINE: f32 = 40.0;

/// Effect of one completed `Project::TransportLine`
/// (`construction::apply_completion`): raises the targeted line's
/// `Condition` by this much (capped at `Condition::FULL`) - the same
/// step-not-full-reset shape `REPAIR_STEP` already uses for region
/// devastation, so a badly damaged line still needs more than one
/// completed project to fully recover.
pub const TRANSPORT_LINE_REPAIR_STEP: f32 = 0.3;

/// Stage 10C (docs/phase10-spec.md "3. 阻止": "飛行場ノードと港ノードを叩ける
/// こと"): the `transport::TransportNode::condition` a node must stay above
/// to keep relaying anything at all (`logistics::TransportGraph::
/// node_operational`) - a binary open/closed fact, not a graded throughput
/// cut, because unlike a `TransportLine` a node carries no physical
/// `Capacity` of its own to scale down in the first place (its own edges are
/// unconstrained hubs - see `logistics::build_transport_graph`'s own doc);
/// "half its structural integrity gone" is where a real facility (a runway,
/// a set of quays and cranes) stops functioning as a whole rather than
/// merely slower, the same all-or-nothing character `naval::
/// is_port_blockaded`'s own threshold already treats a port's usability as
/// having, one level down (a specific fraction of *this* node's own health,
/// not of enemy control over the water it faces).
pub const NODE_OPERATIONAL_THRESHOLD: f32 = 0.5;

/// Stage 10C: `Condition` lost in one `Action::StrikeNode` - large enough
/// that a single successful strike against a fully healthy node
/// (`transport::TransportNode::condition` starts at `Condition::FULL`)
/// crosses `NODE_OPERATIONAL_THRESHOLD` outright (`1.0 - 0.6 = 0.4 < 0.5`)
/// and closes it on the spot, deliberately unlike `LINE_INTERDICTION_DAMAGE`
/// (which never alone severs a healthy route): design.md §8's whole case for
/// this stage is that a single strike against one concentrated point target
/// - one airfield, one port - can decisively cut supply without occupying
/// the region it sits in, not merely wear it down the way repeated raids
/// along an entire spread-out route must.
pub const NODE_STRIKE_DAMAGE: f32 = 0.6;
