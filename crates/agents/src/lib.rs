//! A rule-based `Agent` for `archipelago-sim` (design.md §19 / mvp-spec.md §7):
//! set economic policy, reinforce weakened units, recruit where it's safe,
//! pick offensives by value-per-threat, and walk idle interior units toward
//! the front. No randomness of its own, so a run stays fully determined by
//! the simulation's seed.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;
use archipelago_sim::balance::{
    ARMS_INPUT_MACHINERY, ARMS_INPUT_STEEL, CIVILIAN_ENERGY_DEMAND_PER_POP,
    CIVILIAN_FOOD_DEMAND_PER_POP, CIVILIAN_RATION_MAX, COMBAT_SUPPLY_MULT,
    FOCUS_MARITIME_IMPORT_CAPACITY_MULT, IMPORT_PER_PORT, MACHINERY_INPUT_STEEL,
    MUNITIONS_INPUT_STEEL, MUTINY_THRESHOLD, PROTEST_THRESHOLD, REGIME_CHANGE_THRESHOLD,
    STRIKE_THRESHOLD, SUPPLY_NEED_PER_MANPOWER, UNIT_EQUIPMENT, UNIT_MANPOWER,
};
use archipelago_sim::construction::Project;
use archipelago_sim::diplomacy::{Stance, Treaty, TreatyTerm};
use archipelago_sim::focus::{self, NationalFocus};
use archipelago_sim::good::Good;
use archipelago_sim::group::Group;
use archipelago_sim::ids::{FactionId, RegionId, SeaZoneId, TransportLineId, UnitId};
use archipelago_sim::military::Unit;
use archipelago_sim::naval;
use archipelago_sim::observation::Observation;
use archipelago_sim::world::{Domain, Station, World};

pub mod composite;
pub mod human;
pub mod llm;
pub mod newspaper;

pub use composite::CompositeAgent;
pub use human::HumanAgent;

use llm::Doctrine;

/// Whether `unit` currently shares its station with an enemy of `faction` -
/// the domain-generic form of the land-only `world.has_enemy_units` /
/// sea-only `world.has_enemy_fleets` checks, for AI code that walks both
/// land units and fleets through `Observation::own_units`.
fn unit_contested(world: &World, unit: &Unit, faction: FactionId) -> bool {
    match unit.station {
        Station::Region(r) => world.has_enemy_units(r, faction),
        Station::Sea(z) => world.has_enemy_fleets(z, faction),
    }
}

/// Minimum unit-count headroom (as a multiple of a fresh unit's cost) a
/// faction keeps in reserve before it will spend on a new recruit.
const RECRUIT_STOCK_MARGIN: f32 = 1.5;
/// A unit below this fraction of full manpower/equipment gets reinforced.
const REINFORCE_THRESHOLD: f32 = 0.75;
/// Days of stockpiled Munitions below which the agent shifts the shared
/// Steel/Energy input toward Munitions instead of Machinery (design.md §9's
/// war/economy trade-off, now expressed as an `industry_priority` split).
const LOW_SUPPLY_DAYS: f32 = 20.0;
/// Munitions industry-priority weight used when the Munitions stockpile is
/// running low (Machinery gets `1.0 - this`).
const MUNITIONS_FOCUSED_WEIGHT: f32 = 0.7;
/// Machinery industry-priority weight used when Arms production is starved
/// on Machinery input specifically (Stage 2A's "detect the blocking
/// upstream good and raise its priority").
const MACHINERY_FOCUSED_WEIGHT: f32 = 0.7;
/// Even split used when neither side is under particular pressure.
const BALANCED_WEIGHT: f32 = 0.5;
/// Manpower pool above which conscription is throttled hard - hoarding
/// manpower has a real cost since it suppresses labour via `region.mobilized`.
const CONSCRIPTION_THROTTLE_MANPOWER: f32 = 25.0;
/// Fraction of `unit_cap` (own unit count / cap) above which the faction is
/// considered to have nowhere left to spend manpower: `recruit` stops
/// raising new units once the cap is reached, so a pool this large just
/// sits idle suppressing `labor_ratio` via `region.mobilized` for nothing.
/// Combined with `CONSCRIPTION_THROTTLE_MANPOWER` (an already-large
/// reserve), this is when the agent stops drafting entirely instead of
/// merely throttling it - the pool's own demobilization
/// (`balance::MANPOWER_DEMOBILIZATION_RATE`) handles bringing it back down.
const STOP_CONSCRIPTION_UNIT_CAP_FRACTION: f32 = 0.9;
/// `Faction::supply_ratio` below which `disband_excess` treats the
/// national supply network as failing to keep the fielded force fed - at
/// least half of nationwide demand going unserved this tick. One half of
/// the solvency check that replaced the old head-count margin; see that
/// function's own doc for why both this and `DISBAND_SOLVENCY_BUFFER_DAYS`
/// have to hold before anything gets stood down.
const DISBAND_SOLVENCY_SUPPLY_RATIO: f32 = 0.5;
/// Days of national Munitions buffer (`munitions_buffer_days`) below which
/// `disband_excess` treats a bad `supply_ratio` as a real shortfall rather
/// than routing noise from a cut-off front. Deliberately well below
/// `LOW_SUPPLY_DAYS` (20): `set_policy`'s `munitions_running_low` already
/// reacts to a thinning stockpile at that mark by leaning `industry_priority`
/// toward Munitions, so disbanding only kicks in once that has clearly
/// failed to keep up - a faction with a week or more of stock in the bank
/// is not insolvent no matter how bad this tick's delivery ratio looks.
const DISBAND_SOLVENCY_BUFFER_DAYS: f32 = 7.0;
/// Consecutive `HeuristicAgent::decide_for_llm` calls (this agent's own,
/// `period`-days apart - `HeuristicAgent::with_peace_disposition`'s
/// `PERIOD` == 4, so this is roughly 60 days) `Faction::stock[Munitions]`
/// must sit at or under `CHRONIC_LOW_MUNITIONS_FLOOR` in a row before
/// `disband_excess` will shed a unit from *within* `unit_cap`'s own floor,
/// rather than only when the head count already clears `cap`. A single low
/// tick is not enough on its own - a faction ramping up early in a war, or
/// shrugging off a border skirmish, dips through zero for a tick or two as
/// a matter of course, and trimming a real fighting unit over that is worse
/// than the zero-Munitions streak it would prevent. Checked this matters:
/// without this gate (trimming on the very first low tick), `mvp` seed 1's
/// own early-game dip started disbanding units it would otherwise have
/// kept, which cost it the war it currently wins outright (`Outcome::
/// Victory` regresses to `Stalemate`) and made its own worst insolvency
/// streak *worse* (534 days, up from 127) - fewer units meant less
/// territory meant less industry meant a deeper hole, the opposite of what
/// this fix is for. 15 ticks is comfortably past `mvp`'s own early
/// fluctuations (its worst streak recovers on its own well inside that
/// window) while still small next to `INSOLVENCY_STREAK_LIMIT_DAYS`'s
/// 250-day bar - the case this exists for on `scenarios/japan_hex.json`
/// (400+ day permanent streaks) blows past this threshold in the first
/// couple of weeks and starts shrinking almost immediately.
const CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM: u32 = 15;
/// Mirrors `apps/headless/tests/scenario_acceptance.rs`'s own
/// `MUNITIONS_INSOLVENT_FLOOR` (`<= 0.01` rather than `== 0.0`, so float
/// noise from a tick that nets out to a hair above zero doesn't reset a
/// real streak): the same "the stockpile has not meaningfully recovered"
/// reading that suite measures directly off `Faction::stock[Munitions]`,
/// used here instead of `munitions_insolvent`'s `supply_ratio` term for
/// gauging whether `unit_cap`'s floor is chronically unaffordable.
/// `supply_ratio` is a *delivered/demanded flow* ratio, not a stock level -
/// measured on `scenarios/japan_hex.json` seed 2's 四国連合: a single unit's
/// upkeep is covered ~53% by that tick's own trickle of production, so
/// `supply_ratio` sits comfortably above `DISBAND_SOLVENCY_SUPPLY_RATIO`
/// (0.5) and `munitions_insolvent` reads `false`, even though
/// `logistics::distribute_supply` drains that same tick's production
/// straight back out (it always serves demand up to whatever's in stock,
/// every tick) and `Faction::stock[Munitions]` itself never once leaves
/// zero. A flow ratio above 0.5 does not mean the stockpile is recovering;
/// only the stock itself can say that.
const CHRONIC_LOW_MUNITIONS_FLOOR: f32 = 0.01;
/// Stage 9D (docs/phase9-spec.md "4. AI"): a faction-owned `TransportLine`
/// below this `Condition` is worth funding a `Project::TransportLine` for
/// (`transport_repair_ai`) - well below `1.0` so a line merely nicked by a
/// skirmish isn't fought over every decision cycle, but comfortably above
/// zero so a line worth restoring is caught before it's a near-total loss.
const TRANSPORT_REPAIR_CONDITION_FLOOR: f32 = 0.5;
/// Stage 9D: an enemy-owned `TransportLine` at or below this `Condition` is
/// treated as already effectively cut - `transport_interdict_ai` skips it in
/// favor of a line still worth striking, rather than spending an order on a
/// route that has nothing meaningful left to lose.
const TRANSPORT_INTERDICT_MIN_CONDITION: f32 = 0.05;
/// `civilian_ration` used when Munitions/Arms are critically short and
/// stability can still absorb it (design.md §9's civilian/war trade-off):
/// squeeze civilian Food/Energy/Machinery delivery down to this fraction to
/// free up stock for the war economy, at the cost of raising `shortage`
/// (and therefore unrest) pressure.
const CIVILIAN_RATION_LOW: f32 = 0.7;
/// Stability floor below which the agent stops rationing and returns
/// `civilian_ration` to 1.0 even if Munitions/Arms are still short - unrest
/// is already a problem, so squeezing civilians further isn't worth it.
const RATION_STABILITY_FLOOR: f32 = 60.0;
/// Arms stockpile, in unit-equivalents of `UNIT_EQUIPMENT`, below which
/// Arms counts as "critically short" for rationing purposes (mirrors
/// `RECRUIT_STOCK_MARGIN`'s notion of a comfortable buffer).
const ARMS_LOW_UNIT_MARGIN: f32 = 1.5;
/// `Region::devastation` above which the agent's top build priority
/// (docs/phase2-spec.md Stage 2B) is repairing a damaged own region rather
/// than expanding capacity or infrastructure elsewhere.
const REPAIR_THRESHOLD: f32 = 0.35;
/// Minimum days of Arms-equivalent Machinery/Steel stock (see
/// `machinery_limited_arms_days` in `set_policy`) that must remain before
/// the agent will spend on construction at all - so building never eats
/// into the stockpile the war effort itself needs (Stage 2B: "Machinery /
/// Steel の在庫が軍需の余裕分を下回っている間は着工しない").
const BUILD_STOCK_RESERVE_DAYS: f32 = 15.0;
/// Stage 2C sea imports (docs/phase2-spec.md "Stage 2C": "shortage が出て
/// いるなら不足量を埋めるだけの輸入を要求し"): the agent requests an import
/// rate scaled by `Faction::shortage` (0 when unshortaged, up to the full
/// civilian Food/Energy need at `shortage == 1.0`) rather than always asking
/// for the theoretical maximum - `trade::tick_imports` would cap an
/// over-large request at port capacity/Machinery affordability anyway, but
/// asking only for what's actually missing keeps the request meaningful as
/// a diagnostic and avoids needlessly bidding away Machinery the war economy
/// might still need.
const IMPORT_REQUEST_SHORTAGE_SCALE: f32 = 1.5;
/// Machinery stockpile, in import-equivalents of a full day's Food+Energy
/// civilian need, below which the agent throttles its import request
/// (design.md §9-style trade-off: imports are worth less than keeping the
/// war economy's own Machinery reserve solvent) rather than bidding for
/// imports it can't really afford to keep paying for.
const IMPORT_MACHINERY_LOW_DAYS: f32 = 10.0;
/// Import request multiplier applied when Machinery is running low
/// (`IMPORT_MACHINERY_LOW_DAYS`).
const IMPORT_THROTTLE_WEIGHT: f32 = 0.4;
/// `unit.supply` / `unit.strength()` average below which the fleet is
/// considered pressured on that axis for `logistics_priority` purposes
/// (docs/phase2-spec.md: "部隊の平均 supply が低ければ Munitions 寄り、部隊の
/// 平均 strength が低ければ Arms 寄りにする").
const LOGISTICS_PRESSURE_THRESHOLD: f32 = 0.75;
/// Logistics-priority weight given to whichever good the fleet is pressured
/// on (the other gets `1.0 -` this); an even split when neither or both axes
/// are under pressure, so neither ever gets a fixed unconditional priority.
const LOGISTICS_FOCUSED_WEIGHT: f32 = 0.7;
/// Stage 2D naval AI (docs/phase2-spec.md "Stage 2D" AI section, point 1):
/// fleet count below which the agent keeps building fleets at a safe home
/// port, mirroring `unit_cap`'s role for land recruitment but as a small
/// fixed floor rather than one scaled off industry — a minimal navy, not a
/// second army.
const NAVY_MIN_FLEETS: f32 = 2.0;
/// Naval counterpart of `offensive()`'s land engagement margin: added to
/// the enemy power a target zone's `caution` threshold is judged against,
/// so a fleet won't engage a target at exact parity with nothing in
/// reserve.
const NAVY_ENGAGE_MARGIN: f32 = 0.6;

/// Stage 3A AI (docs/phase3-spec.md "AI" under "Stage 3A"): how far above a
/// political event's own threshold (`STRIKE_THRESHOLD`/`PROTEST_THRESHOLD`/
/// `MUTINY_THRESHOLD`/`REGIME_CHANGE_THRESHOLD`) the agent starts reacting -
/// it eases off *before* the event actually fires, not after, since waiting
/// for the event itself means the damage (a strike, a mutiny, a coup) has
/// already landed.
const POLITICAL_SUPPORT_MARGIN: f32 = 6.0;
/// `conscription` ceiling the agent imposes once Labor or Citizens support
/// is within `POLITICAL_SUPPORT_MARGIN` of triggering `Event::Strike`/
/// `Event::Protest` - overrides whatever `set_policy`'s manpower-driven
/// tiers would otherwise pick.
const POLITICAL_CONSCRIPTION_CEILING: f32 = 0.3;
/// `industry_priority` weight given to Munitions (the Arms-leaning side of
/// the shared Steel/Energy budget - see `GROUP_ARMS_LEAN_*` in
/// `balance.rs`) when Military support is under political pressure
/// (Machinery gets `1.0 -` this).
const POLITICAL_ARMS_LEAN_WEIGHT: f32 = 0.75;
/// `offensive()`/`naval_ops()` `caution` multiplier applied when `stability`
/// is within `POLITICAL_SUPPORT_MARGIN` of `REGIME_CHANGE_THRESHOLD`
/// (docs/phase3-spec.md: "軍事行動より内政を優先する（攻勢の caution を一時
/// 的に引き上げる）") - a higher `caution` makes the agent wait for a bigger
/// force-ratio edge before committing to a new offensive, without touching
/// units already under way.
const POLITICAL_CAUTION_BOOST: f32 = 1.35;

/// Stage 3B AI (docs/phase3-spec.md "AI" under "Stage 3B": "相対戦力・
/// opinion・自国の不足・共通の敵の有無から決める"): the fraction of an
/// enemy's total military power this faction's own must fall to before it's
/// considered "surrounded by a stronger enemy" and offers/accepts
/// `Ceasefire`/`NonAggression` - "自分より強い勢力に囲まれているなら停戦・
/// 不可侵を受け入れやすい" - scaled by the agent's own `caution` (`outmatched
/// = own_power < enemy_power * PEACE_SEEK_BASE_RATIO * caution`). At the
/// least cautious agent's `caution` (~1.15) this stays a real "clearly
/// weaker" bar (~0.86x parity); at the most cautious (~1.45) it eases to
/// "not clearly stronger" (~1.09x parity) - "caution の高い AI ほど条約を選
/// 好する". Deliberately kept below 1.0 even at the least cautious tier so
/// two roughly-matched factions don't rush to blanket peace the moment they
/// meet regardless of `caution` - the played-out history should still
/// depend on how the actual balance of power develops.
const PEACE_SEEK_BASE_RATIO: f32 = 0.75;
/// `opinion` floor (of the proposer, from the responder's point of view)
/// below which the responder rejects a `Ceasefire`/`NonAggression` proposal
/// outright even while outmatched - an actively hostile relationship isn't
/// trusted just because the numbers say peace would help.
const PEACE_ACCEPT_MIN_OPINION: f32 = -50.0;
/// `opinion` floor to accept an `Alliance` proposal - alliances commit this
/// faction to someone else's wars, so they need a real track record of good
/// relations first (typically built up by an already-accepted `Ceasefire`/
/// `NonAggression`/`TradeAgreement`'s `TREATY_ACCEPT_OPINION_BONUS`).
const ALLIANCE_ACCEPT_MIN_OPINION: f32 = 25.0;
/// `opinion` floor to accept a `MilitaryAccess`/`PortAccess` grant - low
/// commitment, so only clearly hostile relationships refuse it.
const ACCESS_ACCEPT_MIN_OPINION: f32 = -10.0;
/// `opinion` floor to accept (or propose) a `TradeAgreement` - the lowest
/// bar of any treaty kind, since it's mutually beneficial and low-risk by
/// construction (`trade::tick_imports`'s exporter-side surplus cap means it
/// can never actually cost the exporter its own reserve).
const TRADE_ACCEPT_MIN_OPINION: f32 = -30.0;
/// `Faction::shortage_by_good` level (docs/phase3-spec.md: "食料が不足して
/// いるなら TradeAgreement を強く求める") past which the agent actively seeks
/// out a `TradeAgreement` partner rather than merely accepting one if offered.
const TRADE_SEEK_SHORTAGE_THRESHOLD: f32 = 0.1;

/// External code review fix C1 (docs/phase3-spec.md "AI" under "Stage 3B":
/// treaty variety - `Alliance`/`MilitaryAccess`/`PortAccess` were
/// implemented and unit-tested but the heuristic AI never had a reason to
/// actually propose any of them, so none of the three ever ran in a real
/// game): how many times a third faction's total military power must exceed
/// *both* this faction's own and a prospective ally's own before it counts
/// as "clearly the dominant threat" worth allying against - see
/// `dominant_threat`.
const ALLIANCE_THREAT_RATIO: f32 = 1.5;

/// External code review fix C2 (docs/phase3-spec.md §23: every seed should
/// produce a different history): the "am I outmatched" peace-seeking checks
/// below (proactive `Ceasefire` proposals and `evaluate_proposal`'s
/// Ceasefire/NonAggression acceptance) additionally require this faction to
/// have suffered at least this much real `Faction::casualties` before they
/// engage at all. Without this gate, "am I outmatched" is judged purely off
/// each side's starting army size - identical for every seed, since nothing
/// about the map or the initial deployment varies by seed - so the very
/// first opportunity (as early as day 0) always reaches the same verdict
/// and the same treaty gets signed on the same day in every run. Gating on
/// actual war losses instead means the decision waits for combat - whose
/// exact damage rolls are seeded RNG (`military::tick_combat`/
/// `naval::tick_naval_combat`'s `rng.range(0.85, 1.15)`) - to have actually
/// happened, so which pair first crosses this bar, and when, can differ
/// seed to seed without the simulation itself gaining any new randomness.
const MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING: f32 = 0.05;

/// Stage 3C AI (docs/phase3-spec.md "AI" under "Stage 3C": "外交提案の受諾さ
/// れやすさ＋"): subtracted from every opinion floor in `evaluate_proposal`
/// when the *responder* has `NationalFocus::AllianceNetwork` active - an
/// alliance-minded faction says yes to a wider range of relationships than
/// it otherwise would.
const FOCUS_ALLIANCE_ACCEPT_BONUS: f32 = 20.0;

/// Stage 3C AI (docs/phase3-spec.md "AI" under "Stage 3C": "領土の半分を失う
/// ... にのみ切り替える"): once a faction's currently-owned region count
/// falls to this fraction of its `core` (starting) region count or below, it
/// reactively switches to `NationalFocus::DefensivePosture` - a stable,
/// state-free trigger (no extra bookkeeping needed: `Region::core` never
/// changes, so "half of what I started with" is always derivable straight
/// from `World`).
const MAJOR_TERRITORY_LOSS_FRACTION: f32 = 0.5;

/// Stage 4A AI (docs/phase4-spec.md "Stage 4A": "Doctrine ... これにより
/// ... 「正面戦争を避ける」... 「海軍増強を優先する」... がそのまま表現でき
/// る"): caution multiplier `HeuristicAgent::decide_for_llm` applies on top
/// of `HeuristicAgent::caution` when a `Doctrine::posture` of `Offensive` is
/// in effect - below `1.0` so the agent commits to a fight at a smaller
/// force-ratio edge than its own un-advised `caution` alone would allow.
const DOCTRINE_OFFENSIVE_CAUTION_MULT: f32 = 0.85;
/// As `DOCTRINE_OFFENSIVE_CAUTION_MULT`, for `Posture::Defensive`. Land
/// `offensive()` is suppressed outright under this posture (see
/// `decide_for_llm`'s `allow_land_offense`), and this multiplier raises the
/// bar the still-active naval defense/blockade ops judge against.
const DOCTRINE_DEFENSIVE_CAUTION_MULT: f32 = 1.6;
/// As above, for `Posture::Consolidate` - also suppresses new offensives,
/// but less steeply risk-averse than `Defensive` in what it still allows.
const DOCTRINE_CONSOLIDATE_CAUTION_MULT: f32 = 1.3;
/// `Doctrine::caution_bias` (docs/phase4-spec.md "-1.0..1.0 慎重さの補正")
/// scales the posture multiplier above by up to this fraction either way.
/// Read only after an `is_finite()` check (`decide_for_llm`) - a `Doctrine`
/// can be built directly, bypassing `llm::parse_doctrine`'s own clamp, so a
/// non-finite bias must never reach this multiplication (see `llm.rs`'s
/// module doc on why every `Doctrine` field is treated as untrusted at the
/// point of use).
const DOCTRINE_CAUTION_BIAS_RANGE: f32 = 0.3;
/// `Doctrine::primary_target` (docs/phase4-spec.md "Doctrine"): score
/// multiplier `offensive()` applies to a candidate target owned by the
/// named faction, so the wrapped heuristic prefers that enemy's territory
/// over an equally-scored target elsewhere without being forced onto it
/// (a target still has to clear the usual force-ratio `caution` gate).
const DOCTRINE_PRIMARY_TARGET_SCORE_MULT: f32 = 1.5;

/// Stage 3B AI: total military power (land + fleets combined) a faction can
/// currently bring to bear, used by `diplomacy_ai` to judge "surrounded by a
/// stronger enemy".
fn total_military_power(world: &World, faction: FactionId) -> f32 {
    world
        .units
        .iter()
        .filter(|u| u.alive && u.owner == faction)
        .map(Unit::combat_power)
        .fold(0.0, |acc, p| acc + p)
}

/// Every living faction's `total_military_power`, indexed by `FactionId`,
/// computed in one `O(units)` pass over `world.units` instead of one
/// `O(units)` pass per faction.
///
/// Stage 6C (docs/phase6-spec.md "Stage 6C"): profiling `HeuristicAgent`
/// on japan47 found `diplomacy_ai` — not `advance_interior`'s BFS, the
/// risk docs/phase6-spec.md §0 named — was the largest single contributor
/// to the AI's per-faction growth (~7.5x per decision call versus mvp,
/// worse than either the 4.7x region-count or 2x faction-count ratio on
/// their own), because `dominant_threat` called `total_military_power`
/// once *per other living faction* to find the strongest one, and
/// `diplomacy_ai` itself called it again for every faction it's at war
/// with — an `O(factions × units)` cost every single `decide()` call, on
/// top of `evaluate_proposal`'s own already-small use. Precomputing every
/// faction's power once here and passing the array down turns that into
/// `O(units + factions)`, called once per `diplomacy_ai` invocation
/// instead of scattered across it.
fn power_by_faction(world: &World) -> Vec<f32> {
    let mut power = vec![0.0f32; world.factions.len()];
    for unit in &world.units {
        if unit.alive {
            power[unit.owner.index()] += unit.combat_power();
        }
    }
    power
}

/// Whether `faction` should accept a proposed `treaty` from `from`
/// (docs/phase3-spec.md "AI" under "Stage 3B"): relative power for the two
/// stance-easing treaties, an opinion floor for every kind (higher for the
/// bigger commitments), and the shared `Faction::shortage_by_good` signal
/// for `TradeAgreement`. `peace_disposition` is this faction's own
/// diplomatic knob (see `HeuristicAgent::peace_disposition`'s doc) -
/// deliberately independent of the faction's *military* `caution`, the way
/// C2's fix asks for a knob of its own rather than reusing one that already
/// serves a different purpose.
fn evaluate_proposal(
    faction: FactionId,
    peace_disposition: f32,
    world: &World,
    from: FactionId,
    treaty: Treaty,
) -> bool {
    let opinion = world.diplomacy.opinion(faction, from);
    // Stage 3C AI (docs/phase3-spec.md "AI" under "Stage 3C": "外交提案の受
    // 諾されやすさ＋"): every opinion floor below eases by this much when the
    // *responder* (this faction) has `NationalFocus::AllianceNetwork`
    // active - it says yes more readily across every treaty kind, not just
    // `Alliance` itself.
    let accept_bonus = if focus::active(world.faction(faction)) == Some(NationalFocus::AllianceNetwork)
    {
        FOCUS_ALLIANCE_ACCEPT_BONUS
    } else {
        0.0
    };
    match treaty {
        Treaty::Ceasefire | Treaty::NonAggression => {
            if opinion < PEACE_ACCEPT_MIN_OPINION - accept_bonus {
                return false;
            }
            // External code review fix C2: judged off starting army size
            // alone (`total_military_power`, identical for every seed at
            // day 0) this would reach the same verdict on the same day in
            // every run - see `MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING`'s doc.
            if world.faction(faction).casualties < MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING {
                return false;
            }
            let own_power = total_military_power(world, faction);
            let enemy_power = total_military_power(world, from);
            own_power < enemy_power * PEACE_SEEK_BASE_RATIO * peace_disposition
        }
        Treaty::Alliance => opinion >= ALLIANCE_ACCEPT_MIN_OPINION - accept_bonus,
        Treaty::MilitaryAccess | Treaty::PortAccess => opinion >= ACCESS_ACCEPT_MIN_OPINION - accept_bonus,
        Treaty::TradeAgreement => opinion >= TRADE_ACCEPT_MIN_OPINION - accept_bonus,
    }
}

/// External code review fix B2 (docs/phase4-spec.md "Stage 4B — 自然言語外
/// 交"): the answer a `HeuristicAgent` recipient gives *every* natural-
/// language proposal, regardless of its text - reject outright, with no
/// terms extracted. Replaces the old `keyword_interpret`, a hand-rolled
/// English/Japanese keyword matcher that approximated understanding a
/// proposal it never actually parsed; docs/conventions.md §3 (フォールバッ
/// ク原則禁止) rules that out, since a `HeuristicAgent` genuinely cannot
/// interpret free text and a keyword hit is not evidence that it did. The
/// honest answer is "no", not a guess dressed up as one - and it's the same
/// answer `keyword_interpret` itself already gave to any text with no
/// recognized keyword (`unparseable_proposal_is_rejected`), just applied
/// uniformly instead of only when the guesswork happened to miss.
///
/// `LlmAgent::interpret_nl` (`llm.rs`) reaches for this exact function too,
/// for the same reason, when its own backend fails or answers with
/// something that doesn't parse - see that function's own doc.
pub(crate) fn cannot_interpret_nl(_obs: &Observation, _from: FactionId, _text: &str) -> (Vec<TreatyTerm>, bool) {
    (Vec::new(), false)
}

/// Stage 3B AI (docs/phase3-spec.md "AI" under "Stage 3B"): responds to
/// every pending proposal addressed to this faction, then proactively
/// proposes `Ceasefire` to whichever enemy most outmatches it,
/// `TradeAgreement` to trade partners once a tradeable good runs short,
/// `Alliance` against a common dominant threat, `MilitaryAccess` when the
/// shortest route to a war target crosses neutral land, and `PortAccess`
/// when this faction's own port capacity is what's actually capping its
/// imports (External code review fix C1 - the last three were implemented
/// and unit-tested but the AI never had a reason to reach for any of them).
/// Every proposal here is gated by `Diplomacy::cooldown`/an already-
/// outstanding proposal, so a rejected or expired offer isn't immediately
/// re-spammed (see `balance::TREATY_COOLDOWN_DAYS` and `diplomacy.rs`'s
/// module doc for why that matters against a machine agent).
fn diplomacy_ai(faction: FactionId, peace_disposition: f32, obs: &Observation, actions: &mut Vec<Action>) {
    let world = obs.world;
    let n = world.factions.len();

    // Respond to every proposal currently addressed to us. Collected first
    // so the borrow of `world.diplomacy.pending` ends before `actions` (a
    // separate `Vec`) is written to.
    let incoming: Vec<(FactionId, Treaty)> = world
        .diplomacy
        .pending
        .iter()
        .filter(|p| p.to == faction)
        .map(|p| (p.from, p.treaty))
        .collect();
    for (from, treaty) in incoming {
        if evaluate_proposal(faction, peace_disposition, world, from, treaty) {
            actions.push(Action::AcceptTreaty { from, treaty });
        } else {
            actions.push(Action::RejectTreaty { from, treaty });
        }
    }

    // Stage 6C: one O(units) pass for every faction's power, reused by
    // `own_power` below, by `dominant_threat`'s scan over every other
    // faction, and by the at-war loop's `enemy_power` - see
    // `power_by_faction`'s doc for the O(factions × units) cost this
    // replaces.
    let power = power_by_faction(world);
    let own_power = power[faction.index()];
    let own_casualties = world.faction(faction).casualties;
    // Worst of Food/Energy/Machinery shortage - the same three goods
    // `Treaty::TradeAgreement` moves (`trade::TRADE_GOODS`).
    let shortage = world.faction(faction).shortage_by_good;
    let trade_seeking = [Good::Food, Good::Energy, Good::Machinery]
        .iter()
        .any(|g| shortage[g.index()] > TRADE_SEEK_SHORTAGE_THRESHOLD);
    // External code review fix C1: who (if anyone) is a common dominant
    // threat worth allying against - see `dominant_threat`'s doc.
    let threat = dominant_threat(&power, world, faction);

    // Proactive proposals, in ascending faction-id order for determinism -
    // at most one outstanding outgoing proposal per target already enforced
    // by `action::apply_propose_treaty` (a duplicate attempt is simply
    // rejected, not queued again), so no per-tick budget bookkeeping is
    // needed here beyond not re-proposing every single day regardless.
    for other_idx in 0..n {
        let other = FactionId(other_idx as u32);
        if other == faction || !world.factions[other_idx].alive {
            continue;
        }
        if world.diplomacy.find_pending(faction, other).is_some() {
            continue;
        }

        if world.diplomacy.is_at_war(faction, other) {
            // External code review fix C2: judged off starting army size
            // alone this reaches the same verdict on the same day in every
            // seed - see `MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING`'s doc.
            let enemy_power = power[other_idx];
            let outmatched = own_casualties >= MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING
                && own_power < enemy_power * PEACE_SEEK_BASE_RATIO * peace_disposition;
            let opinion_ok = world.diplomacy.opinion(faction, other) >= PEACE_ACCEPT_MIN_OPINION;
            if outmatched
                && opinion_ok
                && world.diplomacy.cooldown(faction, other, Treaty::Ceasefire) == 0
            {
                actions.push(Action::ProposeTreaty { to: other, treaty: Treaty::Ceasefire });
                continue;
            }
        } else if let Some((threat_faction, _)) = threat {
            // External code review fix C1 (docs/phase3-spec.md: "相対戦力・
            // opinion・自国の不足・共通の敵の有無から決める" - "共通の敵の有無"
            // specifically): `other` is already at peace with us and isn't
            // the threat itself, so it's a candidate to ally against that
            // threat with.
            if other != threat_faction
                && !world.diplomacy.has_treaty(faction, other, Treaty::Alliance)
                && world.diplomacy.opinion(faction, other) >= ALLIANCE_ACCEPT_MIN_OPINION
                && world.diplomacy.cooldown(faction, other, Treaty::Alliance) == 0
            {
                actions.push(Action::ProposeTreaty { to: other, treaty: Treaty::Alliance });
                continue;
            }
        }

        if trade_seeking
            && !world.diplomacy.has_treaty(faction, other, Treaty::TradeAgreement)
            && world.diplomacy.opinion(faction, other) >= TRADE_ACCEPT_MIN_OPINION
            && world.diplomacy.cooldown(faction, other, Treaty::TradeAgreement) == 0
        {
            actions.push(Action::ProposeTreaty { to: other, treaty: Treaty::TradeAgreement });
        }
    }

    military_access_seek(faction, world, actions);
    port_access_seek(faction, world, actions);
}

/// External code review fix C1: the strongest other faction that clearly
/// outclasses *both* `faction`'s own military power and its own -
/// "clearly the dominant threat" the way docs/phase3-spec.md's "共通の敵"
/// (common enemy) AI note calls for. `None` when nobody meets
/// `ALLIANCE_THREAT_RATIO` against `faction` itself, so a faction that
/// isn't actually threatened never goes looking for an ally.
///
/// `power` is `faction`'s and every other living faction's
/// `total_military_power`, precomputed once by `power_by_faction` - see
/// that function's doc for why this no longer recomputes it per faction.
fn dominant_threat(power: &[f32], world: &World, faction: FactionId) -> Option<(FactionId, f32)> {
    let own_power = power[faction.index()];
    let n = world.factions.len();
    let mut best: Option<(FactionId, f32)> = None;
    for idx in 0..n {
        let other = FactionId(idx as u32);
        if other == faction || !world.factions[idx].alive {
            continue;
        }
        let p = power[idx];
        if p <= own_power * ALLIANCE_THREAT_RATIO {
            continue;
        }
        match best {
            Some((_, best_power)) if p <= best_power => {}
            _ => best = Some((other, p)),
        }
    }
    best
}

/// Stage 3C AI (docs/phase3-spec.md "AI" under "Stage 3C": "初期状況（工業力
/// ・港湾・地理）から方針を選び"): scores every `NationalFocus` off this
/// faction's own opening geography/industry (no cross-faction comparison
/// needed for the score itself), then returns the highest-scoring one that
/// no *lower-`FactionId`* living faction has already actively chosen -
/// diversity without any extra bookkeeping, relying only on the existing
/// `HeuristicAgent::period`/`offset` scheduling (`decide`'s doc): with 3
/// factions and `period == 4`, faction 0 always makes this call on day 0,
/// faction 1 on day 1, faction 2 on day 2, so by the time a later faction
/// picks, every earlier one's real choice is already visible in `World`
/// (the scenario's shared `FACTION_NATIONAL_FOCUS_DEFAULT` never collides
/// with this check, since only a *lower* id ever counts as "already
/// chosen"). Falls back to the top score outright if every candidate is
/// somehow already taken (never happens with 3 factions and 6 foci).
///
/// Scores (all roughly `0..1`-ish so they compare meaningfully against each
/// other):
/// - `MilitaryUnification`: Arms capacity's share of this faction's own
///   industry total.
/// - `Technocracy`: Machinery capacity's share of industry total.
/// - `EconomicSphere`: Steel capacity's share of industry total (a
///   raw-materials/trade-goods orientation).
/// - `MaritimeTrade`: total port rating over industry total.
/// - `DefensivePosture`: average owned-region terrain `defense_bonus`,
///   minus the `1.0` a flat `Terrain::Plain` map would score.
/// - `AllianceNetwork`: how many *distinct* other factions border this one,
///   per owned region - a faction sandwiched between several neighbors
///   scores higher than one with a single front.
fn choose_opening_focus(faction: FactionId, world: &World) -> NationalFocus {
    let own_regions = world.regions_of(faction);
    if own_regions.is_empty() {
        return world.faction(faction).national_focus;
    }

    let industry_total: f32 = own_regions.iter().map(|&r| world.region(r).industry_total()).sum::<f32>().max(0.01);
    let arms: f32 = own_regions.iter().map(|&r| world.region(r).effective_capacity(Good::Arms)).sum();
    let machinery: f32 = own_regions
        .iter()
        .map(|&r| world.region(r).effective_capacity(Good::Machinery))
        .sum();
    let steel: f32 = own_regions.iter().map(|&r| world.region(r).effective_capacity(Good::Steel)).sum();
    let port: f32 = own_regions.iter().map(|&r| world.region(r).port).sum();
    let defense_avg: f32 =
        own_regions.iter().map(|&r| world.region(r).terrain.defense_bonus()).sum::<f32>() / own_regions.len() as f32;

    let mut neighbor_factions: BTreeSet<FactionId> = BTreeSet::new();
    for &r in &own_regions {
        for n in world.neighbors(r) {
            let owner = world.region(n).owner;
            if owner != faction {
                neighbor_factions.insert(owner);
            }
        }
    }
    let alliance_score = neighbor_factions.len() as f32 / own_regions.len() as f32;

    let mut ranked: Vec<(NationalFocus, f32)> = vec![
        (NationalFocus::MilitaryUnification, arms / industry_total),
        (NationalFocus::Technocracy, machinery / industry_total),
        (NationalFocus::EconomicSphere, steel / industry_total),
        (NationalFocus::MaritimeTrade, port / industry_total),
        (NationalFocus::DefensivePosture, defense_avg - 1.0),
        (NationalFocus::AllianceNetwork, alliance_score),
    ];
    // Deterministic tie-break: higher score first, `NationalFocus::index()`
    // as the fixed fallback order for an exact tie.
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.index().cmp(&b.0.index()))
    });

    for &(candidate, _) in &ranked {
        let taken = world.factions.iter().any(|f| {
            f.alive && f.id.0 < faction.0 && f.national_focus == candidate
        });
        if !taken {
            return candidate;
        }
    }
    ranked[0].0
}

/// External code review fix P1 (docs/phase3-spec.md "AI" under "Stage 3C":
/// "状況が大きく変わったとき（領土の半分を失う、同盟が成立するなど）にのみ
/// 切り替える"): the only two reactive triggers, resolved as a strict
/// priority order rather than two independent `if`s - an existential
/// territorial collapse matters more than a new alliance, and *only* the
/// winning trigger's target is ever compared against `current`.
///
/// Before this fix each trigger was checked independently, guarded only by
/// `current != <that trigger's target>`. That looks edge-triggered but
/// isn't: once the territory trigger fires and `current` becomes
/// `DefensivePosture`, the *still-true* alliance condition sees
/// `current != AllianceNetwork` and fires too (rejected while the
/// territory switch is transitioning, then accepted the moment it
/// finishes) - and vice versa the instant the alliance switch itself
/// finishes, since the territory condition is still true. Two permanently-
/// true conditions with no priority between them made the two targets
/// perpetually disagree about what `current` "should" be, so the faction
/// oscillated between them forever, spending most of the game in the
/// `focus::active() == None` transition blackout - strictly worse than
/// never reacting at all.
///
/// The fix makes only the highest-priority *currently-true* condition's
/// target ever count as "what we should be on": while the territory
/// trigger holds, the alliance condition is never even consulted, so a
/// faction already on `DefensivePosture` for that reason emits nothing more
/// no matter how long the alliance also stays true - a real switch happens
/// only when the *set of true conditions* changes (the winning trigger
/// clears, or a higher one newly arms), which is a genuine change of
/// situation, not a level that merely remains true.
///
/// Also refuses to even evaluate while a switch is already under way
/// (`focus_transition_days > 0`): `action::apply_set_national_focus`
/// rejects retargeting mid-transition anyway, so doing so here would only
/// ever produce actions in `decide()`'s output that the simulation is
/// certain to bounce.
fn major_change_focus(faction: FactionId, world: &World) -> Option<NationalFocus> {
    let f = world.faction(faction);
    if f.focus_transition_days > 0 {
        return None;
    }
    let current = f.national_focus;

    let core_regions = world.regions.iter().filter(|r| r.core == faction).count() as f32;
    let owned_regions = world.region_count(faction) as f32;
    let territory_collapse =
        core_regions > 0.0 && owned_regions <= core_regions * MAJOR_TERRITORY_LOSS_FRACTION;
    if territory_collapse {
        return (current != NationalFocus::DefensivePosture).then_some(NationalFocus::DefensivePosture);
    }

    let n = world.factions.len();
    let allied = (0..n).map(|i| FactionId(i as u32)).any(|other| {
        other != faction
            && world.factions[other.index()].alive
            && world.diplomacy.stance(faction, other) == Stance::Alliance
    });
    if allied {
        return (current != NationalFocus::AllianceNetwork).then_some(NationalFocus::AllianceNetwork);
    }

    None
}

/// Stage 3C AI entry point (docs/phase3-spec.md "AI" under "Stage 3C"):
/// picks an opening focus on this agent's very first `decide()` call, then
/// only ever reacts to `major_change_focus` afterward - never re-running the
/// opening heuristic, so a score that would merely have crept past another
/// focus's without any real change in circumstances is not a reason to
/// switch.
fn national_focus_ai(faction: FactionId, initialized: &mut bool, obs: &Observation, actions: &mut Vec<Action>) {
    let world = obs.world;
    if !*initialized {
        actions.push(Action::SetNationalFocus(choose_opening_focus(faction, world)));
        *initialized = true;
        return;
    }
    if let Some(focus) = major_change_focus(faction, world) {
        actions.push(Action::SetNationalFocus(focus));
    }
}

/// This faction's own usable import capacity, mirroring `trade::tick_imports`'s
/// `own_capacity` computation exactly (own, uncontested, unblockaded ports
/// only) - the figure `Treaty::PortAccess` extends when granted, so it's
/// also the figure that decides whether this faction's *own* capacity is
/// the binding constraint worth seeking a grant to relieve.
///
/// External code review fix P2: Stage 3C added `NationalFocus::MaritimeTrade`'s
/// `FOCUS_MARITIME_IMPORT_CAPACITY_MULT` to `tick_imports`'s own per-port
/// figure, but this mirror wasn't updated alongside it - so a faction
/// running `MaritimeTrade` had its own capacity underestimated by this
/// function (by the same margin the multiplier grants) and could seek out a
/// `PortAccess` grant it didn't actually need. Applying the same
/// `focus::active`-gated multiplier here, exactly as `tick_imports` does,
/// keeps the two in lockstep again.
fn own_port_capacity(faction: FactionId, world: &World) -> f32 {
    let maritime_mult = if focus::active(world.faction(faction)) == Some(NationalFocus::MaritimeTrade) {
        FOCUS_MARITIME_IMPORT_CAPACITY_MULT
    } else {
        1.0
    };
    world
        .regions
        .iter()
        .filter(|r| {
            r.owner == faction
                && r.port > 0.0
                && !world.has_enemy_units(r.id, faction)
                && !naval::is_port_blockaded(world, r.id)
        })
        .map(|r| r.port * IMPORT_PER_PORT * (1.0 - r.devastation) * maritime_mult)
        .sum()
}

/// External code review fix C1 (docs/phase3-spec.md: "自国の不足" - port
/// capacity specifically, per `Treaty::PortAccess`'s own doc): seeks a
/// `PortAccess` grant when this faction is actually short on a good
/// `Action::SetImportPlan` would want to import (mirroring
/// `set_trade_policy`'s own shortage signal) *and* its own port capacity
/// can't cover the import volume that shortage implies - i.e. its ports,
/// not its Machinery budget or the world market itself, are the binding
/// constraint. Targets whichever other faction has the most spare capacity
/// of its own to lend, among those with a decent-enough relationship.
fn port_access_seek(faction: FactionId, world: &World, actions: &mut Vec<Action>) {
    let f = world.faction(faction);
    let shortage_seeking = f.shortage_by_good[Good::Food.index()] > TRADE_SEEK_SHORTAGE_THRESHOLD
        || f.shortage_by_good[Good::Energy.index()] > TRADE_SEEK_SHORTAGE_THRESHOLD;
    if !shortage_seeking {
        return;
    }

    let total_pop: f32 = world.regions.iter().filter(|r| r.owner == faction).map(|r| r.population).sum();
    let desired_import = (total_pop * CIVILIAN_FOOD_DEMAND_PER_POP
        + total_pop * CIVILIAN_ENERGY_DEMAND_PER_POP)
        * IMPORT_REQUEST_SHORTAGE_SCALE;
    if own_port_capacity(faction, world) >= desired_import {
        return; // capacity isn't what's binding - nothing a grant would fix.
    }

    let n = world.factions.len();
    let mut best: Option<(FactionId, f32)> = None;
    for idx in 0..n {
        let grantor = FactionId(idx as u32);
        if grantor == faction || !world.factions[idx].alive {
            continue;
        }
        if world.diplomacy.has_treaty(grantor, faction, Treaty::PortAccess) {
            continue;
        }
        if world.diplomacy.find_pending(faction, grantor).is_some() {
            continue;
        }
        if world.diplomacy.cooldown(faction, grantor, Treaty::PortAccess) > 0 {
            continue;
        }
        if world.diplomacy.opinion(faction, grantor) < ACCESS_ACCEPT_MIN_OPINION {
            continue;
        }
        let capacity = own_port_capacity(grantor, world);
        if capacity <= 0.0 {
            continue;
        }
        match best {
            Some((_, best_cap)) if capacity <= best_cap => {}
            _ => best = Some((grantor, capacity)),
        }
    }
    if let Some((grantor, _)) = best {
        actions.push(Action::ProposeTreaty { to: grantor, treaty: Treaty::PortAccess });
    }
}

/// External code review fix C1 (docs/phase3-spec.md: "MilitaryAccess" -
/// "相手領を通過できる"): for every faction this one is at war with, checks
/// whether the shortest land route from this faction's capital to that
/// enemy's capital crosses a third faction's territory - if so, and that
/// third faction isn't itself hostile, seeks transit rights from it rather
/// than never being able to reach a war target with no direct shared
/// border at all.
fn military_access_seek(faction: FactionId, world: &World, actions: &mut Vec<Action>) {
    let n = world.factions.len();
    let capital = world.faction(faction).capital;
    for idx in 0..n {
        let enemy = FactionId(idx as u32);
        if enemy == faction || !world.factions[idx].alive {
            continue;
        }
        if !world.diplomacy.is_at_war(faction, enemy) {
            continue;
        }
        let enemy_capital = world.faction(enemy).capital;
        let Some(path) = land_path(world, capital, enemy_capital) else { continue };
        // The first leg and last leg of the route are our own departure and
        // the enemy's own soil - only a region strictly in between, owned
        // by neither side, means the route actually crosses foreign land.
        let blocker = path
            .iter()
            .skip(1)
            .take(path.len().saturating_sub(2))
            .map(|&r| world.region(r).owner)
            .find(|&owner| owner != faction && owner != enemy);
        let Some(blocker) = blocker else { continue };
        if world.diplomacy.is_at_war(faction, blocker) {
            continue; // can't ask a hostile party for transit rights.
        }
        if world.diplomacy.has_treaty(faction, blocker, Treaty::MilitaryAccess) {
            continue;
        }
        if world.diplomacy.find_pending(faction, blocker).is_some() {
            continue;
        }
        if world.diplomacy.cooldown(faction, blocker, Treaty::MilitaryAccess) > 0 {
            continue;
        }
        if world.diplomacy.opinion(faction, blocker) >= ACCESS_ACCEPT_MIN_OPINION {
            actions.push(Action::ProposeTreaty { to: blocker, treaty: Treaty::MilitaryAccess });
        }
    }
}

/// Breadth-first shortest path (region ids, both ends included) from `from`
/// to `to` over the *full* region graph, unrestricted by ownership - the
/// land-domain counterpart of `zone_path_next`'s sea BFS, but returning the
/// whole route (not just the next hop) since `military_access_seek` needs
/// to inspect every region along the way, not just step toward it.
fn land_path(world: &World, from: RegionId, to: RegionId) -> Option<Vec<RegionId>> {
    if from == to {
        return Some(vec![from]);
    }
    let n = world.regions.len();
    let mut visited = vec![false; n];
    let mut prev: Vec<Option<RegionId>> = vec![None; n];
    let mut queue = VecDeque::new();
    visited[from.index()] = true;
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        if current == to {
            break;
        }
        let mut neighbors: Vec<RegionId> = world.neighbors(current).collect();
        neighbors.sort_by_key(|r| r.0);
        for next in neighbors {
            if !visited[next.index()] {
                visited[next.index()] = true;
                prev[next.index()] = Some(current);
                queue.push_back(next);
            }
        }
    }

    if !visited[to.index()] {
        return None;
    }
    let mut path = vec![to];
    let mut step = to;
    while let Some(p) = prev[step.index()] {
        path.push(p);
        step = p;
    }
    path.reverse();
    Some(path)
}

/// Decides for one faction every `period` days (offset by faction id so the
/// three AIs don't all act on the same day), per mvp-spec.md §7.
pub struct HeuristicAgent {
    faction: FactionId,
    caution: f32,
    /// External code review fix C2 (docs/phase3-spec.md §23: every seed
    /// should produce a different history): each faction's own diplomatic
    /// disposition, the way `caution` already differentiates each faction's
    /// *military* aggressiveness - a deliberately separate knob, not a
    /// reuse of `caution`, since a faction can be militarily bold but
    /// diplomatically quick to make peace, or the reverse. Multiplies
    /// `PEACE_SEEK_BASE_RATIO` in the outmatched checks `diplomacy_ai`/
    /// `evaluate_proposal` run: below `1.0` this faction sues for peace/
    /// alliance sooner (it counts as "outmatched" at a smaller power
    /// deficit) than one above `1.0`. Set once at construction, never
    /// randomized - the seed-to-seed variety this is meant to help produce
    /// comes from combining it with the seeded RNG's actual combat outcomes
    /// (see `MIN_WAR_CASUALTIES_FOR_PEACE_SEEKING`), not from this knob
    /// itself varying.
    peace_disposition: f32,
    period: u32,
    offset: u32,
    /// Stage 3C AI (docs/phase3-spec.md "AI" under "Stage 3C"): whether this
    /// agent has already made its opening `NationalFocus` choice - `false`
    /// until the first `decide()` call issues one, after which
    /// `national_focus_ai` only ever reacts to `major_change_focus`. Not
    /// derived from `World` (unlike everything else this AI reads) because
    /// nothing in `World` distinguishes "still holding the scenario's shared
    /// default" from "deliberately chose that same focus" - see
    /// `choose_opening_focus`'s doc for how that same ambiguity is avoided
    /// for the *other* factions' choices instead.
    focus_initialized: bool,
    /// Consecutive `decide_for_llm` calls (this agent's own, `period`-days
    /// apart) in a row where `Faction::stock[Munitions]` has read at or
    /// under `CHRONIC_LOW_MUNITIONS_FLOOR` - `disband_excess`'s own doc
    /// explains why a single low tick isn't enough to shed a unit from
    /// *within* `unit_cap`'s floor, and `CHRONIC_LOW_MUNITIONS_FLOOR`'s doc
    /// explains why this reads the stock directly rather than
    /// `munitions_insolvent`. Reset to `0` the moment a call reads the stock
    /// above that floor, so a faction that recovers even briefly has to
    /// build the streak back up from scratch rather than carrying a stale
    /// near-threshold count into its next rough patch.
    chronic_insolvency_ticks: u32,
}

/// Default per-faction caution spread (mvp-spec.md §7 suggests this
/// spread): the force-ratio margin required before `HeuristicAgent`
/// launches an offensive, indexed by faction id. Shared by every caller
/// that wants "the same default AI `apps/headless --agent heuristic` uses"
/// without picking its own tuning - `apps/headless`'s `build_agents` and
/// `archipelago-api`'s uncontrolled-faction agents both go through
/// `default_heuristic_agent` below, so both produce byte-identical default
/// play for the same seed (docs/phase5-spec.md's `api_run_matches_headless`).
///
/// Stage 6C (docs/phase6-spec.md "Stage 6C" item 4): extended from 3 to 8
/// entries so japan47's 6 factions each get a genuinely distinct value
/// instead of `default_heuristic_agent`'s old flat `(1.25, 1.0)` fallback
/// for every faction index beyond 2 - with every faction past the third
/// behaving identically, no consistent asymmetry could ever develop
/// between them. This is deliberately an *AI tuning* value, not a
/// `balance.rs` constant (per this file's own module-level split from
/// `crates/sim`'s: "バランス定数は balance.rs、AI のチューニング値は
/// crates/agents"), so extending it changes no shared simulation constant
/// - and appending past index 2 rather than editing indices `0..3` leaves
/// mvp (3 factions, indices `0..3` only) exactly as before: its `--json`
/// hash for seed 1 is unchanged by this edit.
///
/// This alone does not make japan47 resolve decisively within a 720-day
/// run - measured (see docs/phase6-spec.md "Stage 6C" item 4's write-up):
/// with this spread every seed 1-5 still ends in `Outcome::Stalemate` at
/// day 720, same as before, though territory swings noticeably harder
/// (e.g. seed 2's largest faction ends day 720 owning 18/47 regions
/// against a flat-fallback baseline's max of 13/47, and keeps climbing
/// well past day 720 - re-run to day 6000, the same seed reaches 4 of 6
/// factions actually eliminated). The elimination bar itself (`Outcome::
/// Victory` needs `alive.len() == 1`, i.e. every *other* faction's
/// `region_count` at zero) is what won't fit in 720 days here: at the time
/// this was measured, japan47 started all 6 factions in `Diplomacy::new`'s
/// unconditional mutual War (same as mvp), so a leader had to grind down
/// five separate rivals instead of mvp's two, and the pace that finishes
/// territory conquest is set by `unit_cap`'s industry-driven army growth
/// and `balance.rs`'s combat/occupation constants - both shared with mvp
/// and therefore off limits for this fix (retuning either changes mvp's
/// byte-identical `--json` hash too). Confirmed empirically, not by
/// inspection alone: `COMBAT_DAMAGE` at up to 10x, `OCCUPATION_RATE`/
/// `OCCUPATION_DECAY` at 3x/5x, `unit_cap`'s divisor from 1/3 to 5x, and
/// this caution spread pushed to even more extreme values, were each tried
/// in isolation against 20-50 seeds; none changed the day-720 outcome
/// type.
///
/// `scenarios/japan47.json` now declares its own starting blocs
/// (`archipelago_sim::scenario::DiplomacyDef`, docs/future-work.md "japan47
/// が 720 日で決着しない") - two 3-faction alliances (東日本/西日本) instead
/// of a six-way free-for-all - which was the scenario-scoped mechanism this
/// note used to describe as out of scope. Re-measured with the blocs in
/// place: seeds 1-5 *still* end in `Outcome::Stalemate` at day 720, and
/// territory barely moves at all in that window (a single contested
/// border between the two blocs, instead of six-way chaos, is a much
/// narrower front). Worse, this alone can never reach `Outcome::Victory` no
/// matter how long a run goes: `HeuristicAgent` never issues `Action::
/// DeclareWar` or `Action::BreakTreaty` against anyone, so two factions
/// that start (or end up) allied never fight each other, and `alive.len()`
/// can never drop to `1` while an allied pair both still hold territory.
/// Blocs shorten *whom* a leader has to eliminate, not whether the last
/// step (turning on one's own ally) can ever happen under default play -
/// that gap was a victory-condition question. It's since been closed from
/// the other direction, not this one: `archipelago_sim::world::
/// VictoryCondition::Coalition` makes victory reachable the instant every
/// survivor belongs to one mutually-allied group, without requiring a bloc
/// to ever turn on itself - see `scenarios/japan47.json`'s `victory`
/// declaration and `Simulation::outcome`'s doc. `HeuristicAgent` still never
/// issues `DeclareWar`/`BreakTreaty` against an ally, so the mechanism this
/// paragraph describes (a bloc eliminating its own members) remains
/// unimplemented - it just no longer needs to be, now that `Coalition`/
/// `Domination` give a bloc a way to win without it.
pub const DEFAULT_CAUTION: [f32; 8] = [1.15, 1.30, 1.45, 0.80, 1.90, 0.95, 1.70, 2.10];

/// Default per-faction diplomatic disposition spread, paired with
/// `DEFAULT_CAUTION` - see `HeuristicAgent::with_peace_disposition`'s doc
/// for what the knob does, and `DEFAULT_CAUTION`'s doc for why this was
/// extended past its original 3 entries.
pub const DEFAULT_PEACE_DISPOSITION: [f32; 8] = [1.05, 0.80, 1.15, 0.70, 1.25, 0.90, 1.10, 1.35];

/// Builds the default `HeuristicAgent` for faction index `i`, using
/// `DEFAULT_CAUTION`/`DEFAULT_PEACE_DISPOSITION`.
///
/// External code review fix B5: this used to fall back to `(1.25, 1.0)` -
/// the middle of both spreads - for any faction index past the table, a
/// made-up personality no scenario actually asked for and indistinguishable
/// from every other faction past the table (exactly the "every faction past
/// index 2 behaves identically" bug this file's own Stage 6C doc above
/// describes fixing for indices 3-7 - the untouched fallback just moved the
/// same problem to index 8+). A scenario with more factions than either
/// table defines is a scenario this default tuning genuinely doesn't cover;
/// per docs/conventions.md §3 (フォールバック原則禁止) that's a hard failure
/// here, not a silent guess.
pub fn default_heuristic_agent(i: usize) -> HeuristicAgent {
    let caution = *DEFAULT_CAUTION.get(i).unwrap_or_else(|| {
        panic!(
            "faction {i} has no default AI personality: DEFAULT_CAUTION only covers {} factions - \
             this scenario has more factions than the default AI tuning table supports; extend \
             DEFAULT_CAUTION/DEFAULT_PEACE_DISPOSITION or give this scenario its own AI setup",
            DEFAULT_CAUTION.len(),
        )
    });
    let peace_disposition = *DEFAULT_PEACE_DISPOSITION.get(i).unwrap_or_else(|| {
        panic!(
            "faction {i} has no default AI personality: DEFAULT_PEACE_DISPOSITION only covers {} \
             factions - this scenario has more factions than the default AI tuning table supports; \
             extend DEFAULT_CAUTION/DEFAULT_PEACE_DISPOSITION or give this scenario its own AI setup",
            DEFAULT_PEACE_DISPOSITION.len(),
        )
    });
    HeuristicAgent::with_peace_disposition(FactionId(i as u32), caution, peace_disposition)
}

impl HeuristicAgent {
    /// `caution` is the force-ratio margin required before the agent will
    /// launch an offensive: it attacks only when
    /// `own_power >= caution * (defended_power + 0.6)`. Higher values make
    /// the agent more cautious (it waits for a bigger edge); ~1.0 means it
    /// will attack at parity. Values around 1.1-1.5 work well (the MVP
    /// scenario uses 1.15 / 1.30 / 1.45 for its three factions).
    ///
    /// `peace_disposition` defaults to `1.0` (no bias, matching this
    /// function's pre-C2 behaviour) - use `with_peace_disposition` to give
    /// factions differing diplomatic dispositions.
    pub fn new(faction: FactionId, caution: f32) -> Self {
        Self::with_peace_disposition(faction, caution, 1.0)
    }

    /// As `new`, but with an explicit `peace_disposition` (see the field's
    /// doc) instead of the neutral default.
    pub fn with_peace_disposition(faction: FactionId, caution: f32, peace_disposition: f32) -> Self {
        const PERIOD: u32 = 4;
        HeuristicAgent {
            faction,
            caution,
            peace_disposition,
            period: PERIOD,
            offset: faction.0 % PERIOD,
            focus_initialized: false,
            chronic_insolvency_ticks: 0,
        }
    }

    /// The faction this agent decides for.
    pub fn faction(&self) -> FactionId {
        self.faction
    }

    /// Stage 4A entry point (docs/phase4-spec.md "LlmAgent"): everything
    /// `Agent::decide` does, with `doctrine` (from `llm::LlmAgent`, or
    /// `None`) steering a handful of choices. `doctrine == None` takes
    /// exactly the same branches, in exactly the same order, as the plain
    /// `Agent::decide` below - this is what lets a backend that never
    /// produces a `Doctrine` (every call failing, or before the first
    /// consult) produce a run byte-identical to `HeuristicAgent` used
    /// directly (see `llm_failure_falls_back_to_heuristic`).
    pub(crate) fn decide_for_llm(&mut self, obs: &Observation, doctrine: Option<&Doctrine>) -> Vec<Action> {
        if obs.world.day % self.period != self.offset {
            return Vec::new();
        }

        let mut actions = Vec::new();

        // Stage 4A (docs/phase4-spec.md "Doctrine"): a named `focus` takes
        // over from the heuristic's own opening/reactive focus AI outright
        // rather than racing it. Safe to push unconditionally every time
        // this doctrine is in effect - `apply_set_national_focus` already
        // makes re-affirming the current focus a no-op and rejects
        // retargeting mid-transition, so this can never be spammed into an
        // advantage.
        match doctrine.and_then(|d| d.focus) {
            Some(focus) => actions.push(Action::SetNationalFocus(focus)),
            None => national_focus_ai(self.faction, &mut self.focus_initialized, obs, &mut actions),
        }

        set_policy(self.faction, obs, &mut actions);
        set_trade_policy(self.faction, obs, &mut actions);
        set_logistics_priority(obs, &mut actions);
        diplomacy_ai(self.faction, self.peace_disposition, obs, &mut actions);

        if let Some(doc) = doctrine {
            seek_doctrine_treaties(self.faction, obs, doc, &mut actions);
        }

        reinforce(self.faction, obs, &mut actions);
        self.chronic_insolvency_ticks = if obs.world.faction(self.faction).stock[Good::Munitions.index()]
            <= CHRONIC_LOW_MUNITIONS_FLOOR
        {
            self.chronic_insolvency_ticks.saturating_add(1)
        } else {
            0
        };
        recruit(self.faction, self.chronic_insolvency_ticks, obs, &mut actions);
        disband_excess(self.faction, self.chronic_insolvency_ticks, obs, &mut actions);
        naval_recruit(self.faction, self.chronic_insolvency_ticks, obs, &mut actions);
        disband_excess_naval(self.faction, self.chronic_insolvency_ticks, obs, &mut actions);
        build(self.faction, obs, &mut actions);
        transport_repair_ai(self.faction, obs, &mut actions);

        // Stage 3A AI (docs/phase3-spec.md: "stability が REGIME_CHANGE_THRESHOLD
        // に近いときは、軍事行動より内政を優先する"): raise the force-ratio
        // bar for launching a *new* offensive when the government is close
        // to falling, rather than spending what's left of its support on a
        // war it might not survive to finish.
        let stability = obs.world.faction(self.faction).stability;
        let mut caution = if stability < REGIME_CHANGE_THRESHOLD + POLITICAL_SUPPORT_MARGIN {
            self.caution * POLITICAL_CAUTION_BOOST
        } else {
            self.caution
        };

        // Stage 4A: `Posture`/`caution_bias` reshape the same `caution`
        // knob `offensive()`/`naval_ops()` already read, and `Defensive`/
        // `Consolidate` additionally suppress *new* offensives outright
        // (still-in-flight orders from a previous tick are untouched -
        // there simply are none, since `offensive()` is the only source of
        // attack orders). `avoid`/`primary_target` only ever affect target
        // *selection*, never bypass the caution gate itself.
        let (avoid, primary_target, allow_offense): (&[FactionId], Option<FactionId>, bool) = match doctrine
        {
            None => (&[], None, true),
            Some(doc) => {
                let bias = if doc.caution_bias.is_finite() { doc.caution_bias.clamp(-1.0, 1.0) } else { 0.0 };
                let (posture_mult, allow_offense) = match doc.posture {
                    llm::Posture::Offensive => (DOCTRINE_OFFENSIVE_CAUTION_MULT, true),
                    llm::Posture::Defensive => (DOCTRINE_DEFENSIVE_CAUTION_MULT, false),
                    llm::Posture::Consolidate => (DOCTRINE_CONSOLIDATE_CAUTION_MULT, false),
                };
                caution *= posture_mult * (1.0 + bias * DOCTRINE_CAUTION_BIAS_RANGE);
                (doc.avoid.as_slice(), doc.primary_target, allow_offense)
            }
        };

        if allow_offense {
            offensive(self.faction, caution, obs, avoid, primary_target, &mut actions);
            transport_interdict_ai(self.faction, obs, &mut actions);
        }
        naval_ops(self.faction, caution, obs, allow_offense, &mut actions);

        let already_moved: BTreeSet<UnitId> = actions
            .iter()
            .filter_map(|a| match a {
                Action::MoveUnit { unit, .. } => Some(*unit),
                _ => None,
            })
            .collect();
        advance_interior(self.faction, obs, &already_moved, &mut actions);

        actions
    }
}

/// Stage 4A (docs/phase4-spec.md "Doctrine"): proposes whichever of
/// `Doctrine::seek_treaties` this faction doesn't already have active or
/// pending, and isn't on cooldown for. Every `FactionId` is re-validated
/// against the *current* `World` right here, regardless of whether
/// `llm::parse_doctrine` already sanitized it at parse time - a `Doctrine`
/// can also be constructed directly (bypassing the parser entirely, as
/// `llm_cannot_produce_invalid_actions` does on purpose), and a stale or
/// out-of-range target must never reach `Diplomacy`'s direct indexing
/// (`has_treaty`/`cooldown`/`find_pending` all index by `FactionId` with no
/// bounds check of their own - see `diplomacy.rs`).
fn seek_doctrine_treaties(faction: FactionId, obs: &Observation, doctrine: &Doctrine, actions: &mut Vec<Action>) {
    let world = obs.world;
    let n = world.factions.len();
    for &(to, treaty) in &doctrine.seek_treaties {
        if to == faction || to.index() >= n || !world.factions[to.index()].alive {
            continue;
        }
        if world.diplomacy.has_treaty(faction, to, treaty) {
            continue;
        }
        if world.diplomacy.find_pending(faction, to).is_some() {
            continue;
        }
        if world.diplomacy.cooldown(faction, to, treaty) > 0 {
            continue;
        }
        actions.push(Action::ProposeTreaty { to, treaty });
    }
}

impl Agent for HeuristicAgent {
    fn name(&self) -> &str {
        "HeuristicAgent"
    }

    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        let mut actions = self.decide_for_llm(obs, None);
        // External code review fix B2 (docs/phase4-spec.md "Stage 4B"): a
        // plain HeuristicAgent recipient has no LLM to reach for, so it
        // answers every pending proposal with `cannot_interpret_nl`'s honest
        // "no" rather than approximating an understanding via keywords.
        // Deliberately outside `decide_for_llm`, which `LlmAgent` also calls
        // directly for its wrapped fallback (see that struct's `decide`)
        // with its *own* LLM-based interpretation instead - putting this
        // here keeps the two from double-answering the same pending
        // proposal.
        llm::respond_to_pending_nl_proposals(obs, |from, text| cannot_interpret_nl(obs, from, text), &mut actions);
        actions
    }
}

/// Unit-count ceiling the agent recruits toward: a flat floor plus a slope
/// on the faction's whole industrial base (mvp-spec.md §7.3), not derived
/// from any single good's capacity. `3.0` guarantees every faction can hold
/// its `scenarios::UNITS_PER_FACTION` starting army regardless of how its
/// economy is shaped; `industry_total / 5.0` lets a faction grow its force
/// as it industrializes, using `World::industry_total` - the sum of every
/// non-Food good's `effective_capacity` across owned regions, the same
/// devastation-adjusted "war-relevant industry" figure `choose_opening_focus`
/// and `recruit`'s own region-picking already read - rather than any one
/// good in isolation.
///
/// A cap keyed to Munitions capacity alone was tried and reverted: at a
/// headroom tight enough to matter it floored every faction below its own
/// fixed 3-unit start, freezing `scenarios/mvp.json` - the scenario whose
/// economy this AI is actually tuned against - into a 720-day stalemate at
/// starting strength instead of the decisive war it produces under this
/// formula. `scenarios/japan_hex.json`'s small factions being unable to
/// feed even their starting three units is real, but it is a scenario
/// authoring problem (see `tools/hexmap/build_scenario.py`'s Munitions
/// calibration), not one this shared, mvp-tuned cap should be bent to fix.
///
/// Shared by `recruit` (which stops raising new units at this cap) and
/// `set_policy` (which uses proximity to this cap to decide whether a large
/// manpower pool still has somewhere to go).
fn unit_cap(faction: FactionId, obs: &Observation) -> f32 {
    3.0 + obs.world.industry_total(faction) / 5.0
}

/// National daily Munitions demand across every unit (land and sea) this
/// faction currently fields - `SUPPLY_NEED_PER_MANPOWER` per unit of
/// manpower, `balance::COMBAT_SUPPLY_MULT` higher for anything in contact
/// with the enemy. Mirrors exactly what `logistics::distribute_supply`
/// sums into that faction's `total_demand` this same tick, so comparing it
/// against `Faction::stock[Munitions]` gives "how many days would the
/// current stockpile cover at today's draw". Shared by `set_policy`'s
/// industry-priority nudge and `disband_excess`'s solvency check below -
/// both care about the same national Munitions economy, not a land-only
/// slice of it.
fn munitions_daily_demand(faction: FactionId, obs: &Observation) -> f32 {
    obs.own_units()
        .into_iter()
        .map(|unit_id| {
            let unit = obs.world.unit(unit_id);
            let mult =
                if unit_contested(obs.world, unit, faction) { COMBAT_SUPPLY_MULT } else { 1.0 };
            unit.manpower * SUPPLY_NEED_PER_MANPOWER * mult
        })
        .sum()
}

/// Days of national Munitions stock (`Faction::stock[Munitions]`) the
/// current daily draw (`munitions_daily_demand`) would last -
/// `f32::INFINITY` when there is no draw at all (an empty stockpile isn't
/// a problem if nothing is drawing on it). The buffer half of
/// `munitions_insolvent`'s solvency check.
fn munitions_buffer_days(faction: FactionId, obs: &Observation) -> f32 {
    let demand = munitions_daily_demand(faction, obs);
    if demand <= 0.0 {
        f32::INFINITY
    } else {
        obs.world.faction(faction).stock[Good::Munitions.index()] / demand
    }
}

/// Whether `faction`'s economy is currently failing to feed the force it
/// already fields: `Faction::supply_ratio` (the delivered/demanded ratio
/// `logistics::distribute_supply` computes each tick) below
/// `DISBAND_SOLVENCY_SUPPLY_RATIO` *and* `munitions_buffer_days` below
/// `DISBAND_SOLVENCY_BUFFER_DAYS` - see `disband_excess`'s doc for why both
/// must hold (this is what tells a genuinely failing economy apart from a
/// front that's merely cut off).
///
/// Shared by `disband_excess` (stop shedding once this reads false) and
/// `recruit` (stop growing while this reads true) - without the second use,
/// `recruit`'s own gate (`own_unit_count >= cap`) has nothing to say about
/// Munitions at all, so a structurally insolvent faction sitting one unit
/// below a fractional `cap` would have its next `RecruitUnit` immediately
/// undo `disband_excess`'s last cut, which would trigger again next tick,
/// forever - the exact oscillation this refinement is supposed to avoid,
/// just relocated from "unit-count margin" to "recruit and disband
/// disagreeing about the same faction in the same breath". Gating both on
/// the same signal is what keeps them in agreement.
fn munitions_insolvent(faction: FactionId, obs: &Observation) -> bool {
    let f = obs.world.faction(faction);
    f.supply_ratio < DISBAND_SOLVENCY_SUPPLY_RATIO
        && munitions_buffer_days(faction, obs) < DISBAND_SOLVENCY_BUFFER_DAYS
}

/// Count of `obs.own_units()` in a given domain — `unit_cap`/`recruit`'s
/// land army sizing must not be diluted by fleets sharing the same
/// `own_units()` list Stage 2D introduced (`naval_recruit` has its own,
/// separate `NAVY_MIN_FLEETS` floor).
fn own_unit_count(obs: &Observation, domain: Domain) -> usize {
    obs.own_units()
        .into_iter()
        .filter(|&u| obs.world.unit(u).station.domain() == domain)
        .count()
}

fn set_policy(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let manpower = f.manpower;
    let at_unit_cap = own_unit_count(obs, Domain::Land) as f32
        >= unit_cap(faction, obs) * STOP_CONSCRIPTION_UNIT_CAP_FRACTION;
    let conscription: f32 = if manpower < 5.0 {
        0.9
    } else if manpower > CONSCRIPTION_THROTTLE_MANPOWER {
        // A large pool with no unit-cap headroom left to spend it on is
        // just dead weight suppressing `labor_ratio` - stop drafting
        // entirely and let demobilization drain it back to the workforce.
        // Otherwise keep the existing hard throttle: there's still room to
        // grow the army, so a trickle of conscription is worth its cost.
        if at_unit_cap { 0.0 } else { 0.15 }
    } else {
        0.6
    };

    // Stage 3A AI (docs/phase3-spec.md: "Labor か Citizens が閾値に近づいたら
    // civilian_ration を戻し、conscription を下げる"): react before
    // `Event::Strike`/`Event::Protest` actually fires, not after.
    let labor_near = f.group_support[Group::Labor.index()] < STRIKE_THRESHOLD + POLITICAL_SUPPORT_MARGIN;
    let citizens_near =
        f.group_support[Group::Citizens.index()] < PROTEST_THRESHOLD + POLITICAL_SUPPORT_MARGIN;
    let political_squeeze = labor_near || citizens_near;
    let conscription = if political_squeeze {
        conscription.min(POLITICAL_CONSCRIPTION_CEILING)
    } else {
        conscription
    };
    actions.push(Action::SetConscription(conscription));

    let daily_demand = munitions_daily_demand(faction, obs);
    let munitions = f.stock[Good::Munitions.index()];
    let munitions_running_low = daily_demand > 0.0 && munitions / daily_demand < LOW_SUPPLY_DAYS;

    // Detect whether Arms production is bottlenecked on Machinery input
    // specifically (rather than Steel): if the Machinery stock funds fewer
    // days of Arms output than the Steel stock does, Machinery is the
    // blocking upstream good, and raising its share of the shared
    // Steel/Energy input (at Munitions' expense) is what relieves it.
    let machinery_limited_arms_days = f.stock[Good::Machinery.index()] / ARMS_INPUT_MACHINERY;
    let steel_limited_arms_days = f.stock[Good::Steel.index()] / ARMS_INPUT_STEEL;
    let arms_blocked_on_machinery = machinery_limited_arms_days < steel_limited_arms_days;

    // Stage 3A AI (docs/phase3-spec.md: "Military が低ければ Arms 寄りに
    // industry_priority を振る"): a Military group nearing `Event::Mutiny`
    // overrides the ordinary Munitions-stockpile/Machinery-bottleneck
    // reasoning above - keeping the army happy takes priority over either.
    let military_near =
        f.group_support[Group::Military.index()] < MUTINY_THRESHOLD + POLITICAL_SUPPORT_MARGIN;
    let (machinery_weight, munitions_weight) = if military_near {
        (1.0 - POLITICAL_ARMS_LEAN_WEIGHT, POLITICAL_ARMS_LEAN_WEIGHT)
    } else if munitions_running_low {
        (1.0 - MUNITIONS_FOCUSED_WEIGHT, MUNITIONS_FOCUSED_WEIGHT)
    } else if arms_blocked_on_machinery {
        (MACHINERY_FOCUSED_WEIGHT, 1.0 - MACHINERY_FOCUSED_WEIGHT)
    } else {
        (BALANCED_WEIGHT, BALANCED_WEIGHT)
    };
    actions.push(Action::SetIndustryPriority { good: Good::Machinery, weight: machinery_weight });
    actions.push(Action::SetIndustryPriority { good: Good::Munitions, weight: munitions_weight });

    // Civilian rationing (design.md §9): when Munitions or Arms are
    // critically short and stability can still absorb the unrest cost,
    // divert some civilian Food/Energy/Machinery delivery to the war
    // economy. Ease off (back to full delivery) once stability drops too
    // far or the stockpiles have recovered - rationing further at that
    // point just compounds the unrest it caused.
    let arms_low = f.stock[Good::Arms.index()] < UNIT_EQUIPMENT * ARMS_LOW_UNIT_MARGIN;
    let ration = if (munitions_running_low || arms_low) && f.stability > RATION_STABILITY_FLOOR {
        CIVILIAN_RATION_LOW
    } else {
        1.0
    };
    // Stage 3A AI: restoring full rationing takes priority over the war
    // economy's own appetite once Labor/Citizens support is under political
    // pressure - the same override `conscription` above gets.
    let ration = if political_squeeze { CIVILIAN_RATION_MAX } else { ration };
    actions.push(Action::SetCivilianRation(ration));
}

/// Stage 2C sea imports (docs/phase2-spec.md "Stage 2C" AI section): request
/// enough Food/Energy import to close whatever share of civilian demand
/// `Faction::shortage_by_good` says is currently missing *for that specific
/// commodity* (external code review fix - the aggregate `Faction::shortage`
/// is the worst of Food/Energy/Machinery and can be nonzero from Machinery
/// alone, which used to make this request both Food and Energy at full
/// scale even when one of them was perfectly well-stocked), throttled back
/// when the Machinery that pays for it is itself running low.
fn set_trade_policy(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let total_pop: f32 = obs.own_regions().iter().map(|&r| obs.world.region(r).population).sum();
    let food_need = total_pop * CIVILIAN_FOOD_DEMAND_PER_POP;
    let energy_need = total_pop * CIVILIAN_ENERGY_DEMAND_PER_POP;

    let machinery_days = if food_need + energy_need > 0.0 {
        f.stock[Good::Machinery.index()] / (food_need + energy_need)
    } else {
        f32::INFINITY
    };
    let throttle = if machinery_days < IMPORT_MACHINERY_LOW_DAYS {
        IMPORT_THROTTLE_WEIGHT
    } else {
        1.0
    };

    // External code review fix (Stage 2C): scale each commodity's request
    // off *that commodity's own* deficit (`shortage_by_good`), not the
    // aggregate `shortage` (the worst of Food/Energy/Machinery). The old
    // code requested both Food and Energy proportional to full demand
    // whenever aggregate shortage was nonzero, even when only one was
    // actually short - the two plans then competed for the same port
    // capacity and the same Machinery payment, crowding out the commodity
    // that genuinely needed the import with surplus of the one that didn't.
    let food_scale = f.shortage_by_good[Good::Food.index()] * IMPORT_REQUEST_SHORTAGE_SCALE * throttle;
    let energy_scale = f.shortage_by_good[Good::Energy.index()] * IMPORT_REQUEST_SHORTAGE_SCALE * throttle;
    actions.push(Action::SetImportPlan { good: Good::Food, rate: food_need * food_scale });
    actions.push(Action::SetImportPlan { good: Good::Energy, rate: energy_need * energy_scale });
}

/// Stage 2C per-commodity delivery (docs/phase2-spec.md "Stage 2C" AI
/// section): shift `logistics_priority` toward Munitions when the fleet's
/// average `supply` is under pressure, toward Arms when its average
/// `strength()` is - an even split when neither (or both) axis is
/// pressured, so neither good gets a fixed unconditional priority.
fn set_logistics_priority(obs: &Observation, actions: &mut Vec<Action>) {
    let units = obs.own_units();
    if units.is_empty() {
        return;
    }

    let mut supply_sum = 0.0f32;
    let mut strength_sum = 0.0f32;
    for &unit_id in &units {
        let unit = obs.world.unit(unit_id);
        supply_sum += unit.supply;
        strength_sum += unit.strength();
    }
    let avg_supply = supply_sum / units.len() as f32;
    let avg_strength = strength_sum / units.len() as f32;

    let low_supply = avg_supply < LOGISTICS_PRESSURE_THRESHOLD;
    let low_strength = avg_strength < LOGISTICS_PRESSURE_THRESHOLD;
    let (munitions_weight, arms_weight) = match (low_supply, low_strength) {
        (true, false) => (LOGISTICS_FOCUSED_WEIGHT, 1.0 - LOGISTICS_FOCUSED_WEIGHT),
        (false, true) => (1.0 - LOGISTICS_FOCUSED_WEIGHT, LOGISTICS_FOCUSED_WEIGHT),
        _ => (0.5, 0.5),
    };
    actions.push(Action::SetLogisticsPriority { good: Good::Munitions, weight: munitions_weight });
    actions.push(Action::SetLogisticsPriority { good: Good::Arms, weight: arms_weight });
}

/// Tops up under-strength units (land or fleet) sitting safely in friendly,
/// uncontested territory/waters. A land unit's station must additionally be
/// its own faction's region (matches the pre-Stage-2D check exactly); a sea
/// zone has no owner, so a fleet only needs to be uncontested.
fn reinforce(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let mut units = obs.own_units();
    units.sort_by_key(|u| u.0);

    for unit_id in units {
        let unit = obs.world.unit(unit_id);
        let safe = match unit.station {
            Station::Region(r) => {
                obs.world.region(r).owner == faction && !obs.world.has_enemy_units(r, faction)
            }
            Station::Sea(z) => !obs.world.has_enemy_fleets(z, faction),
        };
        if !safe {
            continue;
        }
        if unit.strength() < REINFORCE_THRESHOLD {
            actions.push(Action::ReinforceUnit { unit: unit_id });
        }
    }
}

/// Raises a new corps at the capital, or failing that the safest, most
/// industrious region held, as long as the faction can afford it and isn't
/// already well-manned relative to its industrial base.
fn recruit(faction: FactionId, chronic_insolvency_ticks: u32, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let cap = unit_cap(faction, obs);
    if own_unit_count(obs, Domain::Land) as f32 >= cap {
        return;
    }
    if f.manpower < UNIT_MANPOWER * RECRUIT_STOCK_MARGIN
        || f.stock[Good::Arms.index()] < UNIT_EQUIPMENT * RECRUIT_STOCK_MARGIN
    {
        return;
    }
    // Stage 9D (docs/phase9-spec.md "4. AI"): the transport network is now a
    // finite, capacity-constrained resource (Stage 9B), and `unit_cap` above
    // has no notion of it at all - it is derived purely from industry, so a
    // faction can keep recruiting straight through a network that's already
    // failing to deliver to the force it has. Measured on `scenarios/mvp.json`
    // seed 1 before this gate: reaching `Outcome::Conquest` on day 265 with
    // 10 units at `supply_ratio` 0.500 (pre-Stage-9B) regressed, after
    // Stage 9B alone, to a 720-day stalemate with 15 units at `supply_ratio`
    // 0.095 - the AI kept adding units the network could not feed at all.
    //
    // The rule: stop recruiting the instant nationwide delivered/demanded
    // supply already reads below `DISBAND_SOLVENCY_SUPPLY_RATIO` (reusing
    // the same threshold `munitions_insolvent` already uses for "the network
    // is failing to serve at least half of demand" - not a new tuning
    // constant), *without* also requiring `munitions_buffer_days` to have
    // run out the way `munitions_insolvent` does for `disband_excess`. A
    // faction can carry a comfortable Munitions stockpile (say, built up
    // before the front thinned out) while its *flow* ratio is already
    // failing - `munitions_insolvent` alone would keep waving new recruits
    // through right up until that stockpile is actually drained, by which
    // point the network was already unable to carry the existing force.
    // Recruiting more mouths only makes a network that already can't feed
    // half of demand relay less to everyone, never more - so this checks
    // the flow signal on its own, strictly ahead of stock depletion.
    if f.supply_ratio < DISBAND_SOLVENCY_SUPPLY_RATIO {
        return;
    }
    // Munitions solvency gate (`munitions_insolvent`'s own doc): a faction
    // that can't feed the force it already has must not add another mouth
    // to feed, and must not immediately rebuild whatever `disband_excess`
    // last cut for exactly that reason.
    if munitions_insolvent(faction, obs) {
        return;
    }
    // Same chronic-stock gate `disband_excess` uses for its own within-`cap`
    // trim (`CHRONIC_LOW_MUNITIONS_FLOOR`'s doc): `munitions_insolvent`
    // alone isn't enough here, because it can read "solvent" purely from
    // `supply_ratio` while the stock itself has been stuck at the floor for
    // as long as `disband_excess` has been actively cutting - without this,
    // measured on `scenarios/japan_hex.json` seed 2's 四国連合: this
    // function rebuilt, in the very same tick, exactly the unit
    // `disband_excess` had just cut for chronic insolvency (`supply_ratio`
    // for one unit alone sat at ~0.53-0.58, above `DISBAND_SOLVENCY_
    // SUPPLY_RATIO`), forever netting to zero change and permanently
    // pinning `Faction::stock[Munitions]` at zero - the same one-way
    // absorbing state, just re-entered every single tick instead of once.
    if chronic_insolvency_ticks > CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM {
        return;
    }

    let capital = f.capital;
    let region = if is_safe_own_region(faction, obs, capital) {
        Some(capital)
    } else {
        safe_own_regions(faction, obs)
            .into_iter()
            .fold(None, |best: Option<(RegionId, f32)>, r| {
                let industry = obs.world.region(r).industry_total();
                match best {
                    Some((_, best_industry)) if industry <= best_industry => best,
                    _ => Some((r, industry)),
                }
            })
            .map(|(r, _)| r)
    };

    if let Some(region) = region {
        actions.push(Action::RecruitUnit { region, domain: Domain::Land });
    }
}

/// Disband-defect fix (docs/conventions.md §6's one-way accumulator shape,
/// measured on `scenarios/japan_hex.json`: a faction stuck at zero
/// Munitions, unable to feed an army sized for territory it no longer
/// holds): `recruit` above stops *growing* the land force at `unit_cap`,
/// but nothing ever shrank it back down once `unit_cap` itself fell - a
/// faction's industry (and therefore its cap) can drop after losing
/// regions while every unit it already raised stays on the books forever.
///
/// `total <= cap` is only the *precondition* for having anything to trim -
/// mirrors the boundary `recruit` stops growing at, so there is nothing to
/// shed back below it. Whether to actually act on an overshoot is a
/// separate, solvency-driven question: a first cut of this fix gated it on
/// `total` clearing `cap` by a flat 25% margin, which watches the wrong
/// quantity - measured on `scenarios/japan_hex.json` day 720, a faction
/// holding five times another's territory can field the same handful of
/// units as that starved faction and read as "solvent" by pure head count,
/// while a faction sitting at 36.5% `supply_ratio` with an empty Munitions
/// stock never crossed the margin and starved forever. What actually
/// determines whether a force is sustainable is whether the *economy* can
/// feed it, which `Faction::supply_ratio` and `Faction::stock[Munitions]`
/// say directly - so `munitions_insolvent` (its own doc has the two
/// thresholds and why both must hold) reads those instead, and this is
/// what tells a genuinely failing economy apart from a front that's merely
/// cut off: a besieged region's demand goes unserved -
/// `logistics::distribute_supply`'s `avail[r][f]` is zero for it - which
/// drags `supply_ratio` down exactly like real insolvency does, but the
/// Munitions that would have gone to that front is never drawn from
/// `Faction::stock` in the first place (nothing was delivered), so the
/// national buffer stays intact. A faction can therefore read as badly
/// undersupplied on `supply_ratio` alone and still correctly read as
/// solvent here, because the second, independent signal - the stockpile
/// behind the whole force, not just the cut-off piece of it - says the
/// economy is fine and this is a logistics problem, not an economic one.
///
/// `munitions_insolvent`'s two thresholds are themselves the hysteresis
/// this fix's old flat margin used to provide, now hung off the quantity
/// that actually matters: both sit below `LOW_SUPPLY_DAYS` (20), the point
/// at which `set_policy`'s `munitions_running_low` already leans
/// `industry_priority` toward Munitions - that policy gets first chance to
/// fix a thinning stockpile before this one starts cutting force size, so a
/// stockpile dipping for a tick or two (a border skirmish, a moment of
/// devastation) doesn't immediately reverse itself into a disband; only a
/// sustained, two-signal shortfall crosses this. When it does, stands the
/// *weakest* (`Unit::combat_power`, ascending) uncontested units down,
/// exactly enough to bring the force back to `cap` - never more than the
/// measured overshoot, and never a unit under enemy contact (the same
/// refusal `action::apply_disband` itself enforces, so this never queues an
/// order the simulation would reject anyway).
///
/// That alone isn't sufficient for the hysteresis to hold, though: `cap` is
/// a continuous, industry-derived value, and `total` an integer, so a
/// structurally insolvent faction sitting exactly one unit above a
/// fractional `cap` gets that one unit trimmed back down *below* `cap` -
/// and `recruit`'s own gate (`own_unit_count >= cap`) would then see
/// headroom and immediately rebuild it, without ever checking whether the
/// stockpile that made this function cut in the first place has recovered.
/// Measured on `scenarios/japan_hex.json` seed 1: one faction cycled
/// through exactly this every recruit/disband tick from day 182 to day 718
/// - over 500 days of build-then-cut with no change in outcome, purely
/// because the two functions disagreed about the same faction's solvency.
/// `recruit` therefore checks `munitions_insolvent` too now, and stops
/// growing while it holds - `unit_cap` still sets *how big* the force is
/// allowed to get (recruit's own boundary, untouched here), but whether
/// it's currently allowed to move *toward* that size is the same solvency
/// question this function asks about shrinking it. Expressing both in the
/// same terms is what keeps them from fighting each other.
///
/// This is what keeps the war on japan_hex from going quiet: a faction
/// actively fighting for territory it can't yet feed only ever loses its
/// *weakest* rear units down to what its economy can carry, never units
/// engaged at the front.
///
/// `unit_cap`'s own `3.0` floor was originally meant to stop this from
/// talking a faction down toward zero at all - "a faction able to keep
/// fighting, not one disarmed into irrelevance". Measured false on
/// `scenarios/japan_hex.json`: a handful of factions reduced to a few
/// regions have a national Munitions *potential*
/// (`economy::tick_economy`'s `pot[Munitions]` - capacity-bounded, set by
/// how much Munitions-producing land they still hold, not by any input
/// budget - no amount of spare Steel/Energy share can raise it) below what
/// even a single unit's upkeep draws. `total` then never clears `cap`, so
/// the trim above never fires, and the floor that was meant to guarantee a
/// fighting force instead pins the faction into exactly the one-way
/// absorbing state docs/conventions.md §6 warns against - just expressed as
/// permanent zero Munitions instead of a ballooning head count, and with no
/// recovery path at all: `recruit`'s own `munitions_insolvent` gate already
/// stops it from making the shortfall worse, but nothing before this ever
/// made it better. Once the stockpile itself is genuinely, *persistently*
/// stuck at the floor even inside `cap` (`chronic_insolvency_ticks` past
/// `CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM` - tracked off
/// `Faction::stock[Munitions]` directly, not `munitions_insolvent`; see
/// `CHRONIC_LOW_MUNITIONS_FLOOR`'s doc for why `supply_ratio` can't be
/// trusted for this specific read, and the ticks constant's own doc for why
/// a single low tick must not be enough here), this now sheds one more unit
/// anyway - gradual (one at a time, not a wipeout), since `decide` reruns
/// this every time the agent acts, so the force keeps shrinking - possibly
/// to zero land units, for a faction whose production can't cover even one
/// - until demand finally clears whatever production is left, whatever
/// size `cap`'s floor claimed should have been enough. The same
/// `recruit`/`munitions_insolvent` hysteresis that already stops the
/// over-cap case above from oscillating stops this from immediately
/// rebuilding what it just shed.
///
/// Land-only, mirroring `unit_cap`/`recruit`'s own land-only scope - fleets
/// are sized by `NAVY_MIN_FLEETS` instead, a different mechanism this fix
/// deliberately leaves untouched.
fn disband_excess(faction: FactionId, chronic_insolvency_ticks: u32, obs: &Observation, actions: &mut Vec<Action>) {
    let cap = unit_cap(faction, obs);
    let land_units: Vec<UnitId> = obs
        .own_units()
        .into_iter()
        .filter(|&u| obs.world.unit(u).station.domain() == Domain::Land)
        .collect();
    let total = land_units.len() as f32;
    if total <= 0.0 {
        return;
    }

    // Trim toward `cap` in one pass when a head count clearing it is the
    // overshoot (the original shape this fix covers - territory/industry
    // lost after the army was raised for a bigger economy) *and*
    // `munitions_insolvent` agrees the economy genuinely can't keep up
    // (that function's own doc: the two-signal check that tells a real
    // shortfall apart from a merely cut-off front). Once already at or
    // under `cap`, that same two-signal check is the wrong instrument -
    // `CHRONIC_LOW_MUNITIONS_FLOOR`'s doc has the measured case where
    // `supply_ratio` alone reads "fine" forever while the stock itself
    // never leaves zero - so this instead reads `Faction::stock[Munitions]`
    // directly, over a streak long enough to rule out routine noise
    // (`CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM`'s doc): only once that
    // streak has run long enough to call it chronic does the floor itself
    // count as unaffordable, and even then only one unit is shed per call,
    // leaving the next tick's fresh read to decide whether another cut is
    // still warranted.
    //
    // The two branches are combined with `max`, not `if`/`else if`: a
    // faction sitting a fraction of a unit above `cap` (e.g. `total == 5`,
    // `cap == 4.9`) rounds `total - cap` down to `0`, so the over-cap branch
    // alone can compute "nothing to trim" even while `munitions_insolvent`
    // reads true - which used to suppress the chronic branch entirely
    // whenever it happened to share the same `if` (an `else if` never runs
    // once the first arm's condition is true, regardless of what it
    // actually computed). Measured on `scenarios/japan_hex.json` seed 2's
    // 近畿府: exactly this rounding zeroed the over-cap branch every tick for
    // 250+ consecutive days while genuinely chronic, because `total` sat a
    // hair above `cap` the whole time.
    let over_cap_excess = if total > cap && munitions_insolvent(faction, obs) {
        (total - cap).round().max(0.0) as usize
    } else {
        0
    };
    let chronic_excess = if chronic_insolvency_ticks > CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM { 1 } else { 0 };
    let excess = over_cap_excess.max(chronic_excess);
    if excess == 0 {
        return;
    }

    let mut eligible: Vec<UnitId> =
        land_units.into_iter().filter(|&u| !unit_contested(obs.world, obs.world.unit(u), faction)).collect();
    // Weakest combat power first; ties broken by unit id for determinism
    // (`f32::partial_cmp` alone can't order NaN, which `combat_power`
    // should never produce, but the tie-break also keeps equal-strength
    // ties from depending on `own_units()`'s incoming order).
    eligible.sort_by(|&a, &b| {
        let power_a = obs.world.unit(a).combat_power();
        let power_b = obs.world.unit(b).combat_power();
        power_a.partial_cmp(&power_b).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0))
    });

    for &unit in eligible.iter().take(excess) {
        actions.push(Action::DisbandUnit { unit });
    }
}

fn is_safe_own_region(faction: FactionId, obs: &Observation, region: RegionId) -> bool {
    obs.world.region(region).owner == faction && !obs.world.has_enemy_units(region, faction)
}

/// Own regions with no enemy units present, in ascending id order.
fn safe_own_regions(faction: FactionId, obs: &Observation) -> Vec<RegionId> {
    let mut regions: Vec<RegionId> = obs
        .own_regions()
        .into_iter()
        .filter(|&r| !obs.world.has_enemy_units(r, faction))
        .collect();
    regions.sort_by_key(|r| r.0);
    regions
}

/// The best own, uncontested port region to operate a navy out of - highest
/// `port` value, ties broken toward the lowest region id - or `None` if the
/// faction holds no safe port at all. Shared by `naval_recruit` (where to
/// build) and `home_zone` (where an idle fleet with nothing else to do
/// returns to).
fn best_own_port_region(faction: FactionId, obs: &Observation) -> Option<RegionId> {
    safe_own_regions(faction, obs)
        .into_iter()
        .filter(|&r| obs.world.region(r).port > 0.0)
        .fold(None, |best: Option<(RegionId, f32)>, r| {
            let port = obs.world.region(r).port;
            match best {
                Some((_, best_port)) if port <= best_port => best,
                _ => Some((r, port)),
            }
        })
        .map(|(r, _)| r)
}

/// The sea zone the faction's main port faces (docs/phase2-spec.md Stage 2D
/// AI point 4: "自国の主要港の海域"), if it holds a safe port at all.
fn home_zone(faction: FactionId, obs: &Observation) -> Option<SeaZoneId> {
    naval::home_zone(obs.world, best_own_port_region(faction, obs)?)
}

/// Sea zones touching a port region matching `own` (`true`: this faction's
/// own regions; `false`: any other faction's) — regardless of whether that
/// region is currently contested, since a blockade target or a defense
/// target is about the port's *owner*, not today's fighting there.
fn port_zones(faction: FactionId, obs: &Observation, own: bool) -> BTreeSet<SeaZoneId> {
    let mut zones = BTreeSet::new();
    for region in &obs.world.regions {
        let matches_owner = if own { region.owner == faction } else { region.owner != faction };
        if matches_owner && region.port > 0.0 {
            zones.extend(obs.world.zones_touching(region.id));
        }
    }
    zones
}

/// Stage 2D naval AI (docs/phase2-spec.md "Stage 2D" AI section, point 1):
/// keeps at least `NAVY_MIN_FLEETS` fleets in being, built at the faction's
/// best safe port, the same affordability gate `recruit` uses for land units.
///
/// `chronic_insolvency_ticks` gate: the same one `recruit` now carries (see
/// that function's own doc) - without it, this rebuilds, in the same tick,
/// exactly the fleet `disband_excess_naval` just cut for chronic
/// insolvency, since `NAVY_MIN_FLEETS` is a flat constant with no
/// industry-derived cap of its own to fall below in the first place, unlike
/// land's `unit_cap`. Measured on `scenarios/japan_hex.json` seed 2's
/// 中部同盟: once every land unit was gone, its last fleet(s) alone kept
/// national Munitions demand just above zero forever, pinning
/// `Faction::stock[Munitions]` at zero for 400+ consecutive days even
/// though the faction held territory and was otherwise functioning.
fn naval_recruit(faction: FactionId, chronic_insolvency_ticks: u32, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    if own_unit_count(obs, Domain::Sea) as f32 >= NAVY_MIN_FLEETS {
        return;
    }
    if f.manpower < UNIT_MANPOWER * RECRUIT_STOCK_MARGIN
        || f.stock[Good::Arms.index()] < UNIT_EQUIPMENT * RECRUIT_STOCK_MARGIN
    {
        return;
    }
    // Stage 9D: the same network-aware gate `recruit` carries (see its own
    // doc for the full reasoning) - a fleet draws on the exact same
    // national Munitions demand a land unit does, so a network already
    // failing to serve half of demand must not get another mouth to feed
    // here either.
    if f.supply_ratio < DISBAND_SOLVENCY_SUPPLY_RATIO {
        return;
    }
    // Only blocks once `disband_excess_naval` would actually be cutting
    // (that function's own doc: gated on the land force already being
    // gone) - otherwise this would refuse to rebuild toward
    // `NAVY_MIN_FLEETS` during an ordinary early-game dip that never
    // touches an existing fleet at all, for no benefit.
    if own_unit_count(obs, Domain::Land) == 0
        && chronic_insolvency_ticks > CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM
    {
        return;
    }
    if let Some(region) = best_own_port_region(faction, obs) {
        actions.push(Action::RecruitUnit { region, domain: Domain::Sea });
    }
}

/// The sea-domain mirror of `disband_excess` - see that function's own doc
/// for the full account of why a chronically-unaffordable floor needs a
/// shrink path at all. `NAVY_MIN_FLEETS` is already a flat constant (unlike
/// `unit_cap`, it never falls with lost territory), so in practice fleet
/// count never exceeds it under normal play and only the within-floor,
/// chronic-stock branch ever fires here - the over-floor branch is kept
/// anyway so this reads the same way `disband_excess` does and stays
/// correct if that ever changes.
fn disband_excess_naval(faction: FactionId, chronic_insolvency_ticks: u32, obs: &Observation, actions: &mut Vec<Action>) {
    let sea_units: Vec<UnitId> = obs
        .own_units()
        .into_iter()
        .filter(|&u| obs.world.unit(u).station.domain() == Domain::Sea)
        .collect();
    let total = sea_units.len() as f32;
    if total <= 0.0 {
        return;
    }

    // The within-floor branch only kicks in once the land force is already
    // gone (`own_unit_count(.., Domain::Land) == 0`) - land units are more
    // numerous and cheaper to raise back, so `disband_excess`'s own chronic
    // trim gets first claim on relieving the economy; only once that's
    // exhausted its own headroom is a fleet - a proportionally much bigger
    // cut, at `NAVY_MIN_FLEETS` == 2 - fair game. Checked this matters:
    // without it, `mvp` seed 1's own early-game dip (well inside land's
    // 15-tick chronic window, so `disband_excess` itself never trims
    // anything there) still cost a fleet the moment this branch fired
    // unconditionally, which regressed `mvp` from `Outcome::Victory` to
    // `Outcome::Stalemate` - the same class of over-eager trim
    // `CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM`'s own doc already measured
    // once for land, just proportionally worse here since one fleet is half
    // of `mvp`'s entire navy.
    // Combined with `max`, not `if`/`else if` - see `disband_excess`'s own
    // doc for why an `else if` here can silently swallow a genuinely
    // chronic case whenever `total` sits a fraction of a fleet above
    // `NAVY_MIN_FLEETS`.
    let land_is_gone = own_unit_count(obs, Domain::Land) == 0;
    let over_cap_excess = if total > NAVY_MIN_FLEETS && munitions_insolvent(faction, obs) {
        (total - NAVY_MIN_FLEETS).round().max(0.0) as usize
    } else {
        0
    };
    let chronic_excess =
        if land_is_gone && chronic_insolvency_ticks > CHRONIC_INSOLVENCY_TICKS_FOR_FLOOR_TRIM { 1 } else { 0 };
    let excess = over_cap_excess.max(chronic_excess);
    if excess == 0 {
        return;
    }

    let mut eligible: Vec<UnitId> =
        sea_units.into_iter().filter(|&u| !unit_contested(obs.world, obs.world.unit(u), faction)).collect();
    eligible.sort_by(|&a, &b| {
        let power_a = obs.world.unit(a).combat_power();
        let power_b = obs.world.unit(b).combat_power();
        power_a.partial_cmp(&power_b).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0))
    });

    for &unit in eligible.iter().take(excess) {
        actions.push(Action::DisbandUnit { unit });
    }
}

/// Stage 2D naval AI (docs/phase2-spec.md "Stage 2D" AI section, points
/// 2-4): moves idle fleets, grouped by their current zone, toward whichever
/// of three priorities applies -
/// 1. (point 2) clear enemy fleets from a zone touching one of this
///    faction's own ports, if reachable and the force ratio (`caution`,
///    the same margin `offensive()` uses for land) favors attacking;
/// 2. (point 3) otherwise, contest a zone touching an *enemy* port under
///    the same force-ratio gate — sea denial against the faction most
///    dependent on that water;
/// 3. (point 4) otherwise, head back to the faction's main home port zone,
///    where it stays once there (no target found next time it's idle).
/// Stage 4A addition: `allow_offense` gates point 3 (contesting an enemy
/// port zone) only - point 2 (defending an own port zone under threat)
/// always runs regardless, since a `Doctrine::posture` of `Defensive`/
/// `Consolidate` means "don't start fights", not "don't defend". When
/// `false`, `enemy_port_zones` is left empty so `blockade_target` below
/// never finds anything to propose.
fn naval_ops(
    faction: FactionId,
    caution: f32,
    obs: &Observation,
    allow_offense: bool,
    actions: &mut Vec<Action>,
) {
    let own_port_zones = port_zones(faction, obs, true);
    let enemy_port_zones = if allow_offense { port_zones(faction, obs, false) } else { BTreeSet::new() };

    let mut idle_by_zone: BTreeMap<SeaZoneId, Vec<UnitId>> = BTreeMap::new();
    for unit_id in obs.own_units() {
        let unit = obs.world.unit(unit_id);
        if unit.movement.is_some() {
            continue;
        }
        if let Some(zone) = unit.station.sea_zone() {
            idle_by_zone.entry(zone).or_default().push(unit_id);
        }
    }
    if idle_by_zone.is_empty() {
        return;
    }

    let home = home_zone(faction, obs);

    for (&zone, fleets) in &idle_by_zone {
        let reachable_zones = |candidates: &BTreeSet<SeaZoneId>| -> Vec<SeaZoneId> {
            candidates
                .iter()
                .copied()
                .filter(|&z| z == zone || obs.world.sea_zone(zone).adjacent.contains(&z))
                .collect()
        };

        // Point 2: defend an own port zone under threat - the most
        // dangerous reachable one (highest enemy power) is the most urgent.
        let defend_target = reachable_zones(&own_port_zones)
            .into_iter()
            .map(|z| (z, obs.enemy_zone_power(z)))
            .filter(|&(_, danger)| danger > 0.0)
            .fold(None, |best: Option<(SeaZoneId, f32)>, (z, danger)| {
                match best {
                    Some((_, best_danger)) if danger <= best_danger => best,
                    _ => Some((z, danger)),
                }
            })
            .map(|(z, _)| z);

        // Point 3: contest an enemy port zone - same `value / (1 +
        // enemy_power)` scoring `offensive()` uses for land targets, so the
        // agent prefers a valuable, weakly-held zone over a strongly
        // defended one.
        let blockade_target = reachable_zones(&enemy_port_zones)
            .into_iter()
            .fold(None, |best: Option<(SeaZoneId, f32)>, z| {
                let score = zone_value(obs, z) / (1.0 + obs.enemy_zone_power(z));
                match best {
                    Some((_, best_score)) if score <= best_score => best,
                    _ => Some((z, score)),
                }
            })
            .map(|(z, _)| z);

        let target = defend_target.or(blockade_target);

        if let Some(target) = target {
            if target == zone {
                continue; // already there; naval combat resolves it this tick
            }
            let own_power = obs.own_zone_power(zone);
            let enemy_power = obs.enemy_zone_power(target);
            if own_power >= caution * (enemy_power + NAVY_ENGAGE_MARGIN) {
                for &unit_id in fleets {
                    actions.push(Action::MoveUnit { unit: unit_id, to: Station::Sea(target) });
                }
            }
            continue;
        }

        // Point 4: nothing to do here - head back toward the main home port.
        if let Some(home) = home {
            if zone != home {
                if let Some(next) = zone_path_next(obs.world, zone, home) {
                    for &unit_id in fleets {
                        actions.push(Action::MoveUnit { unit: unit_id, to: Station::Sea(next) });
                    }
                }
            }
        }
    }
}

/// Rough strategic value of a sea zone for blockade targeting: the highest
/// `Region::value()` among the ports it touches.
fn zone_value(obs: &Observation, zone: SeaZoneId) -> f32 {
    obs.world
        .sea_zone(zone)
        .coast
        .iter()
        .map(|&r| obs.world.region(r).value())
        .fold(0.0f32, f32::max)
}

/// Breadth-first next hop from `from` toward `to` over the sea-zone
/// adjacency graph — the zone-domain counterpart of
/// `Observation::path_next` (which is region/land-graph specific).
fn zone_path_next(world: &World, from: SeaZoneId, to: SeaZoneId) -> Option<SeaZoneId> {
    if from == to {
        return None;
    }
    let n = world.sea_zones.len();
    let mut visited = vec![false; n];
    let mut prev = vec![None; n];
    let mut queue = VecDeque::new();
    visited[from.index()] = true;
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        if current == to {
            break;
        }
        let mut neighbors: Vec<SeaZoneId> = world.sea_zone(current).adjacent.clone();
        neighbors.sort_by_key(|z| z.0);
        for next in neighbors {
            if !visited[next.index()] {
                visited[next.index()] = true;
                prev[next.index()] = Some(current);
                queue.push_back(next);
            }
        }
    }

    if !visited[to.index()] {
        return None;
    }
    let mut step = to;
    while let Some(p) = prev[step.index()] {
        if p == from {
            return Some(step);
        }
        step = p;
    }
    None
}

/// Build priorities (docs/phase2-spec.md Stage 2B):
/// 1. Repair an own, uncontested, sufficiently devastated region.
/// 2. Otherwise, add `Capacity` for whichever good is the production
///    chain's structural bottleneck, at a safe, high-infrastructure region.
/// 3. Otherwise, raise `Infrastructure` at a front region.
///
/// Gated behind `BUILD_STOCK_RESERVE_DAYS`: while Machinery/Steel stock is
/// below the war effort's own short-term reserve, the agent does not start
/// or continue directing new resources into construction (an already
/// in-progress project run by `construction::tick_construction` still
/// slows down instead of stalling - this gate only stops *new* orders).
fn build(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let f = obs.world.faction(faction);
    let machinery_days = f.stock[Good::Machinery.index()] / ARMS_INPUT_MACHINERY;
    let steel_days = f.stock[Good::Steel.index()] / ARMS_INPUT_STEEL;
    if machinery_days < BUILD_STOCK_RESERVE_DAYS || steel_days < BUILD_STOCK_RESERVE_DAYS {
        return;
    }

    if let Some(region) = repair_target(faction, obs) {
        actions.push(Action::Build { region, project: Project::Repair });
        return;
    }

    if let Some(good) = bottleneck_good(obs) {
        if let Some(region) = safest_high_infra_region(faction, obs) {
            actions.push(Action::Build { region, project: Project::Capacity(good) });
            return;
        }
    }

    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);
    let region = front.into_iter().find(|&r| {
        obs.world.region(r).construction.is_none() && !obs.world.has_enemy_units(r, faction)
    });
    if let Some(region) = region {
        actions.push(Action::Build { region, project: Project::Infrastructure });
    }
}

/// The most devastated own, uncontested region with no project already
/// running, if any is past `REPAIR_THRESHOLD`.
fn repair_target(faction: FactionId, obs: &Observation) -> Option<RegionId> {
    let mut candidates = obs.own_regions();
    candidates.sort_by_key(|r| r.0);
    candidates
        .into_iter()
        .filter(|&r| {
            let region = obs.world.region(r);
            region.devastation > REPAIR_THRESHOLD
                && region.construction.is_none()
                && !obs.world.has_enemy_units(r, faction)
        })
        .fold(None, |best: Option<(RegionId, f32)>, r| {
            let d = obs.world.region(r).devastation;
            match best {
                Some((_, best_d)) if d <= best_d => best,
                _ => Some((r, d)),
            }
        })
        .map(|(r, _)| r)
}

/// Stage 9D (docs/phase9-spec.md "4. AI": "自勢力の補給が細っている路線を認識
/// して優先度を上げる"): the worst-condition line this faction owns outright
/// (both endpoint regions its own territory) below `TRANSPORT_REPAIR_
/// CONDITION_FLOOR`, if it can actually be invested in right now - hosted at
/// whichever of its two endpoint regions isn't already mid-construction and
/// isn't under enemy contact (`action::apply_build`'s own requirements).
/// `None` when no owned line is damaged enough to bother with, or none of
/// the damaged ones has an eligible host region this tick.
///
/// `codex review` (P2): host eligibility is part of *choosing*, not a filter
/// applied afterwards. Picking the single worst line first and only then
/// asking whether it can be hosted made the whole AI sit idle whenever that
/// one line happened to be mid-construction or under enemy contact at both
/// ends - every other damaged line went unrepaired until the worst one
/// freed up. Worst *repairable* is the actual intent.
fn transport_repair_target(faction: FactionId, obs: &Observation) -> Option<(RegionId, TransportLineId)> {
    let world = obs.world;
    world
        .transport_lines
        .iter()
        .filter_map(|line| {
            let ra = world.transport_node(line.from).region;
            let rb = world.transport_node(line.to).region;
            if world.region(ra).owner != faction
                || world.region(rb).owner != faction
                || line.condition.get() >= TRANSPORT_REPAIR_CONDITION_FLOOR
            {
                return None;
            }
            let region = [ra, rb]
                .into_iter()
                .find(|&r| world.region(r).construction.is_none() && !world.has_enemy_units(r, faction))?;
            Some((region, line))
        })
        .fold(None, |best: Option<(RegionId, &archipelago_sim::transport::TransportLine)>, candidate| match best {
            Some((_, b)) if b.condition.get() <= candidate.1.condition.get() => best,
            _ => Some(candidate),
        })
        .map(|(region, line)| (region, line.id))
}

/// Stage 9D AI: funds restoring the network's own worst-damaged own route
/// (`transport_repair_target`) via `Build`'s `Project::TransportLine` -
/// called alongside `build` (`decide_for_llm`), independently of it, so a
/// tick that has nothing else worth building still keeps the network itself
/// in mind. Shares the region-level "one project at a time" constraint
/// `action::apply_build` enforces with every other `Project`, so this can
/// occasionally lose out to `build`'s own choice for the same region in the
/// same tick - a harmless, self-correcting no-op the next cycle re-evaluates
/// fresh, the same way `recruit`/`disband_excess` already tolerate losing to
/// each other on a given tick.
fn transport_repair_ai(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    if let Some((region, line)) = transport_repair_target(faction, obs) {
        actions.push(Action::Build { region, project: Project::TransportLine(line) });
    }
}

/// Stage 9D (docs/phase9-spec.md "4. AI"): while at war, the most valuable
/// route (highest current `TransportLine::effective_capacity`) owned
/// entirely by an enemy faction and not already reduced to near nothing
/// (`TRANSPORT_INTERDICT_MIN_CONDITION`) - picked by capacity rather than by
/// front proximity, since docs/phase9-spec.md's whole case for this layer is
/// that a *route* is now a legitimate target in its own right
/// (`Action::InterdictLine`'s own doc: no locality requirement). Called only
/// when `offensive`'s own `allow_offense` gate is open (`decide_for_llm`),
/// the same doctrine-respecting condition every other offensive act shares.
fn transport_interdict_ai(faction: FactionId, obs: &Observation, actions: &mut Vec<Action>) {
    let world = obs.world;
    let best = world
        .transport_lines
        .iter()
        .filter(|line| {
            let ra = world.transport_node(line.from).region;
            let rb = world.transport_node(line.to).region;
            let owner_a = world.region(ra).owner;
            let owner_b = world.region(rb).owner;
            owner_a == owner_b
                && owner_a != faction
                && world.diplomacy.is_at_war(faction, owner_a)
                && line.condition.get() > TRANSPORT_INTERDICT_MIN_CONDITION
        })
        .fold(None, |best: Option<&archipelago_sim::transport::TransportLine>, line| {
            let cap = line.effective_capacity(world);
            match best {
                Some(b) if b.effective_capacity(world) >= cap => best,
                _ => Some(line),
            }
        });
    if let Some(line) = best {
        actions.push(Action::InterdictLine { line: line.id });
    }
}

/// The good whose *national* effective capacity structurally can't fund
/// what downstream production needs from it - `Steel` first, since both
/// `Machinery` and `Munitions` (and `Arms`, via `Steel`) draw on it, then
/// `Machinery` for `Arms` specifically. `None` when nothing owned is
/// structurally starved this way.
fn bottleneck_good(obs: &Observation) -> Option<Good> {
    let mut steel_cap = 0.0f32;
    let mut machinery_cap = 0.0f32;
    let mut munitions_cap = 0.0f32;
    let mut arms_cap = 0.0f32;
    for r in obs.own_regions() {
        let region = obs.world.region(r);
        steel_cap += region.effective_capacity(Good::Steel);
        machinery_cap += region.effective_capacity(Good::Machinery);
        munitions_cap += region.effective_capacity(Good::Munitions);
        arms_cap += region.effective_capacity(Good::Arms);
    }

    let steel_needed = machinery_cap * MACHINERY_INPUT_STEEL
        + munitions_cap * MUNITIONS_INPUT_STEEL
        + arms_cap * ARMS_INPUT_STEEL;
    if steel_cap < steel_needed {
        return Some(Good::Steel);
    }

    let machinery_needed = arms_cap * ARMS_INPUT_MACHINERY;
    if machinery_cap < machinery_needed {
        return Some(Good::Machinery);
    }

    None
}

/// The safest place to expand capacity: an own, uncontested, non-front
/// region with no project running, preferring the highest infrastructure so
/// the new capacity is actually usable at good efficiency; falls back to
/// any safe own region without a project if every own region is on the front.
fn safest_high_infra_region(faction: FactionId, obs: &Observation) -> Option<RegionId> {
    let front: BTreeSet<RegionId> = obs.front_regions().into_iter().collect();
    let mut own = obs.own_regions();
    own.sort_by_key(|r| r.0);

    let interior_best = own
        .iter()
        .copied()
        .filter(|r| {
            !front.contains(r)
                && obs.world.region(*r).construction.is_none()
                && !obs.world.has_enemy_units(*r, faction)
        })
        .fold(None, |best: Option<(RegionId, f32)>, r| {
            let infra = obs.world.region(r).infrastructure;
            match best {
                Some((_, best_infra)) if infra <= best_infra => best,
                _ => Some((r, infra)),
            }
        })
        .map(|(r, _)| r);
    if interior_best.is_some() {
        return interior_best;
    }

    own.into_iter().find(|&r| {
        obs.world.region(r).construction.is_none() && !obs.world.has_enemy_units(r, faction)
    })
}

/// From every uncontested own region, picks the best adjacent target
/// (highest `value / (1 + enemy_power)`) and, if the garrison is strong
/// enough relative to it, sends units in - leaving one unit behind as a
/// garrison when attacking out of a front region with a defended target.
/// Units already moving toward that same target fill part of that quota
/// without a fresh order (see `is_en_route_here` below).
/// Stage 4A additions (docs/phase4-spec.md "Doctrine"): `avoid` drops any
/// candidate target owned by one of those factions before scoring even
/// starts (a hard exclusion, not a penalty - `HeuristicAgent::decide` itself
/// always passes `&[]` here, so this is behaviourally a no-op for every
/// existing caller); `primary_target`, when set, multiplies a candidate's
/// score by `DOCTRINE_PRIMARY_TARGET_SCORE_MULT` when it's owned by that
/// faction - a preference, not a requirement, so a much better-scoring
/// target elsewhere can still win. Neither parameter is ever used to index
/// `World` - only compared against a target region's own (always-valid)
/// `owner` - so an out-of-range or otherwise invalid `FactionId` in either
/// is harmless here by construction (see `llm.rs`'s module doc).
fn offensive(
    faction: FactionId,
    caution: f32,
    obs: &Observation,
    avoid: &[FactionId],
    primary_target: Option<FactionId>,
    actions: &mut Vec<Action>,
) {
    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);

    for region in safe_own_regions(faction, obs) {
        let mut targets: Vec<RegionId> = obs
            .world
            .neighbors(region)
            .filter(|&n| {
                let owner = obs.world.region(n).owner;
                owner != faction && !avoid.contains(&owner)
            })
            .collect();
        targets.sort_by_key(|r| r.0);
        if targets.is_empty() {
            continue;
        }

        let best_target = targets
            .into_iter()
            .fold(None, |best: Option<(RegionId, f32)>, t| {
                let value = obs.world.region(t).value();
                let mut score = value / (1.0 + obs.enemy_power(t));
                if primary_target.is_some() && primary_target == Some(obs.world.region(t).owner) {
                    score *= DOCTRINE_PRIMARY_TARGET_SCORE_MULT;
                }
                match best {
                    Some((_, best_score)) if score <= best_score => best,
                    _ => Some((t, score)),
                }
            })
            .map(|(t, _)| t);
        let Some(target) = best_target else { continue };

        let enemy_power = obs.enemy_power(target);
        let terrain_bonus = obs.world.region(target).terrain.defense_bonus();
        let own_power = obs.own_power(region);
        if own_power < caution * (enemy_power * terrain_bonus + 0.6) {
            continue;
        }

        let is_front = front.binary_search(&region).is_ok();
        let target_undefended = enemy_power <= 0.0;
        let mut present: Vec<UnitId> = obs
            .world
            .units_in(region)
            .filter(|u| u.owner == faction)
            .map(|u| u.id)
            .collect();
        present.sort_by_key(|u| u.0);

        // Garrison sizing counts every unit physically here, whether idle
        // or already departing.
        let send_capacity = if is_front && !target_undefended {
            present.len().saturating_sub(1)
        } else {
            present.len()
        };

        let is_en_route_here = |u: &UnitId| {
            matches!(obs.world.unit(*u).movement, Some(mv) if mv.to == Station::Region(target))
        };

        // A unit already under way toward `target` occupies one of the
        // capacity slots without needing a fresh order - re-issuing
        // `MoveUnit` for it would reset `Movement::progress` to zero, and
        // since the agent re-plans every 4 days while a hostile crossing
        // can take longer than that, the attack would never land. A unit
        // moving toward somewhere else still counts as idle here and can be
        // redirected - that's a deliberate change of target, not an
        // accident of re-planning.
        let already_en_route = present.iter().filter(|u| is_en_route_here(u)).count();
        let new_orders = send_capacity.saturating_sub(already_en_route);

        let idle: Vec<UnitId> = present.into_iter().filter(|u| !is_en_route_here(u)).collect();
        for &unit_id in idle.iter().take(new_orders) {
            actions.push(Action::MoveUnit { unit: unit_id, to: Station::Region(target) });
        }
    }
}

/// Walks units that aren't already at (or ordered toward) the front one
/// step closer, via `Observation::path_next`, toward whichever front region
/// is nearest by raw map distance.
///
/// Stage 6C (docs/phase6-spec.md "Stage 6C"): this used to run a fresh
/// `map_distances` (a full, unconstrained `O(regions)` BFS) *per idle unit*,
/// picking whichever front minimized `distances[front]`. That was written
/// for mvp's 10 regions and handful of units; at 47 regions with a unit
/// count that grows with industry (`unit_cap`), it was a candidate for the
/// dominant cost in `HeuristicAgent::decide` docs/phase6-spec.md §0 flagged
/// ("AI の探索が破綻する") — one BFS per unit, when the map only has a
/// handful of front regions. Profiling `HeuristicAgent` on japan47 showed
/// it in fact isn't the *biggest* single contributor (`diplomacy_ai`'s
/// `O(factions × units)` cost, fixed by `power_by_faction`, was larger) —
/// but on any tick with at least one idle unit to walk forward, this is
/// still real BFS-per-unit cost worth removing, and on most ticks
/// (determined empirically: idle-unit-needing-a-move ticks are rare -
/// most units are already at the front, already moving, or in combat)
/// there's nothing to walk at all, so the fix below must stay just as
/// cheap as the original on *those* ticks - no BFS paid for that never
/// gets used.
///
/// Since the map's links are always bidirectional (`scenario` validation
/// requires it), `distance(location, f) == distance(f, location)`, so the
/// exact same nearest-front-by-id-on-ties value can be had by running
/// `map_distances` once *per front region* (bounded by how many of this
/// faction's own regions border an enemy — typically a handful, and never
/// more than `own_regions().len()`) instead of once per idle unit — but
/// only once at least one unit has actually cleared every other
/// disqualifying check below (already moving, not on land, already at the
/// front, contested). Collecting that eligible list first, and returning
/// before ever touching `map_distances` if it's empty, is what keeps a
/// quiet tick's cost at zero, same as before this change. This is the
/// identical formula for the eligible units, just with the outer loop
/// swapped from "one BFS per unit" to "one BFS per front", so it produces
/// byte-identical results while scaling with border length instead of
/// army size. A small `path_next` cache below is the same trick for the
/// second BFS `Observation::path_next` runs per unit — units idling in the
/// same region heading to the same nearest front (common right after a
/// recruitment wave) now share one BFS instead of repeating it.
fn advance_interior(
    faction: FactionId,
    obs: &Observation,
    already_moved: &BTreeSet<UnitId>,
    actions: &mut Vec<Action>,
) {
    let mut front = obs.front_regions();
    front.sort_by_key(|r| r.0);
    if front.is_empty() {
        return;
    }

    let mut units = obs.own_units();
    units.sort_by_key(|u| u.0);

    // Only the units that actually need a move order - filtering this
    // first (no BFS touched yet) is what keeps a tick with nothing to walk
    // exactly as cheap as before this change.
    let eligible: Vec<(UnitId, RegionId)> = units
        .into_iter()
        .filter(|unit_id| !already_moved.contains(unit_id))
        .filter_map(|unit_id| {
            let unit = obs.world.unit(unit_id);
            if unit.movement.is_some() {
                return None;
            }
            // Fleets are steered by `naval_ops`, not this land-only walk
            // toward the region front.
            let location = unit.station.region()?;
            if front.binary_search(&location).is_ok() {
                return None;
            }
            if obs.world.has_enemy_units(location, faction) {
                return None;
            }
            Some((unit_id, location))
        })
        .collect();
    if eligible.is_empty() {
        return;
    }

    // One BFS per front region (see doc above), reused by every eligible
    // unit below instead of one BFS per unit.
    let front_distances: Vec<Vec<u32>> =
        front.iter().map(|&f| map_distances(obs.world, f)).collect();

    let mut path_cache: HashMap<(RegionId, RegionId), Option<RegionId>> = HashMap::new();

    for (unit_id, location) in eligible {
        // Exactly `map_distances(obs.world, location)[f]` for each front
        // `f` (undirected graph, so `distance(location, f) ==
        // distance(f, location)`) - same argmin, same ascending-front-id
        // tie-break as the original per-unit computation, just read from
        // the precomputed per-front tables instead of a fresh per-unit BFS.
        let nearest_front = front
            .iter()
            .copied()
            .enumerate()
            .fold(None, |best: Option<(RegionId, u32)>, (fi, f)| {
                let d = front_distances[fi][location.index()];
                match best {
                    Some((_, best_d)) if d >= best_d => best,
                    _ => Some((f, d)),
                }
            })
            .map(|(f, _)| f);
        let Some(nearest_front) = nearest_front else { continue };

        let next = *path_cache
            .entry((location, nearest_front))
            .or_insert_with(|| obs.path_next(location, nearest_front));
        if let Some(next) = next {
            actions.push(Action::MoveUnit { unit: unit_id, to: Station::Region(next) });
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod llm_tests;

/// Breadth-first hop count from `from` to every region, over the full map
/// graph (not restricted to friendly territory - this is just "how far
/// away is the front", not a route the unit is forced to take).
fn map_distances(world: &World, from: RegionId) -> Vec<u32> {
    let mut dist = vec![u32::MAX; world.regions.len()];
    dist[from.index()] = 0;
    let mut queue = VecDeque::new();
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        let mut neighbors: Vec<RegionId> = world.neighbors(current).collect();
        neighbors.sort_by_key(|r| r.0);
        for next in neighbors {
            if dist[next.index()] == u32::MAX {
                dist[next.index()] = dist[current.index()] + 1;
                queue.push_back(next);
            }
        }
    }

    dist
}
