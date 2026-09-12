//! Phase 12 technology (docs/phase12-spec.md): a faction's civilian,
//! munitions and equipment research, expressed as coefficients on
//! *existing* values rather than a new simulation domain of its own (§0).
//!
//! Stage 12A ships only the shape and the daily progress accumulation -
//! `ResearchAxis`/`ResearchWeight`, `Faction::research_allocation`/
//! `research_progress`, and `tick_research` itself. **Nothing outside this
//! module reads `research_progress` yet** - Stage 12B is what wires each
//! axis into a real coefficient (civilian/munitions production,
//! `military::Branch`'s combat multiplier). Until then this module can only
//! ever change what a faction *knows*, never what it can *do*, which is
//! exactly what keeps Stage 12A's "3 シナリオの結果が変わらない" acceptance
//! bar true by construction rather than by careful arithmetic - see
//! `tick_research`'s own doc for the deliberate one-way-accumulator
//! exception this stage carries forward.
//!
//! Deliberately *not* built yet (docs/phase12-spec.md §1 "追いつきには接触
//! が要る"): the catch-up/diffusion mechanic that lets a lagging faction
//! close the gap against a contacted rival. That needs the contact rules
//! (trade/alliance/adjacency) this stage never touches, and belongs with
//! Stage 12B's effects, not this stage's plain accumulation.

use crate::balance::RESEARCH_RATE_PER_MACHINERY;
use crate::world::World;

/// One of the three technology axes docs/phase12-spec.md §0 settled on
/// (owner consultation, 2026-09-12) - each a coefficient on an *existing*
/// value, never a new simulation domain:
///
/// | axis | eventually feeds (Stage 12B) |
/// |---|---|
/// | `Civilian` | `economy`'s Food/Energy/Machinery production |
/// | `Munitions` | Munitions and equipment production |
/// | `Equipment` | `military::Branch`'s combat coefficient |
///
/// `Equipment` is shared by every `Branch` rather than split further into
/// one axis per branch: the spec's own table (§0) names exactly three
/// rows, and "兵科ごとの装備" ("equipment, per branch") describes what this
/// *one* axis's progress feeds into in Stage 12B, not a fourth/fifth axis of
/// its own - splitting it further is exactly the kind of unrequested
/// abstraction docs/conventions.md §1 requires asking about before
/// building, and the spec never asks for it.
///
/// A flat, wildcard-free enum - the same `Good`/`Layer`/`military::Branch`
/// convention - so a `match` over every axis fails to compile the moment a
/// variant is added, rather than silently dropping it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResearchAxis {
    Civilian,
    Munitions,
    Equipment,
}

pub const RESEARCH_AXIS_COUNT: usize = 3;

/// Every `ResearchAxis`, in `ResearchAxis::index()`'s fixed order - the
/// same `ALL_GOODS`/`ALL_BRANCHES` convention, so no reader ever depends on
/// enum declaration order or a `HashMap`/`HashSet` iteration order
/// (docs/conventions.md §5).
pub const ALL_RESEARCH_AXES: [ResearchAxis; RESEARCH_AXIS_COUNT] =
    [ResearchAxis::Civilian, ResearchAxis::Munitions, ResearchAxis::Equipment];

impl ResearchAxis {
    pub const fn index(self) -> usize {
        match self {
            ResearchAxis::Civilian => 0,
            ResearchAxis::Munitions => 1,
            ResearchAxis::Equipment => 2,
        }
    }

    /// Lowercase English key - `Good::key()`/`Layer::key()`'s convention,
    /// used by both action codecs and (once Stage 12C exists) the API schema.
    pub const fn key(self) -> &'static str {
        match self {
            ResearchAxis::Civilian => "civilian",
            ResearchAxis::Munitions => "munitions",
            ResearchAxis::Equipment => "equipment",
        }
    }

    pub fn from_key(key: &str) -> Option<ResearchAxis> {
        match key {
            "civilian" => Some(ResearchAxis::Civilian),
            "munitions" => Some(ResearchAxis::Munitions),
            "equipment" => Some(ResearchAxis::Equipment),
            _ => None,
        }
    }

    /// Japanese label - `military::Branch::label()`'s convention, for
    /// Stage 12C's policy panel.
    pub const fn label(self) -> &'static str {
        match self {
            ResearchAxis::Civilian => "民生技術",
            ResearchAxis::Munitions => "軍需技術",
            ResearchAxis::Equipment => "装備技術",
        }
    }
}

/// A research allocation weight, `0.0..=1.0` - the bounded-quantity pattern
/// `transport::Condition`/`transport::Capacity`/`world::AirSuperiority`
/// already establish (docs/conventions.md §1: "ビジネスロジックはなるべく
/// 型で実装する... 不正な値をそもそも構築できなくする"). `Faction::
/// industry_priority`/`logistics_priority` share this exact same
/// "per-slot weight, split proportionally, no fixed order" shape but store
/// it as a raw, only-validated-at-the-action-boundary `f32` - they predate
/// this being applied as literally as it can be, and docs/conventions.md
/// §1's own "既存コードへの適用" is explicit that this isn't retrofitted
/// onto them wholesale. New code doesn't have to repeat their shape,
/// though, so this is its own type rather than a fourth raw `[f32; N]`.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct ResearchWeight(f32);

// `Copy` (derived above) is what makes `[ResearchWeight(..); N]`'s
// array-repeat syntax legal below - the same reason `Condition`/`Capacity`/
// `AirSuperiority` are all `Copy` too.

impl ResearchWeight {
    pub const ZERO: ResearchWeight = ResearchWeight(0.0);

    pub fn new(value: f32) -> Option<ResearchWeight> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Some(ResearchWeight(value))
        } else {
            None
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// `Faction::research_allocation`'s starting value: an even three-way
/// split, the same "no axis favoured before any agent or player has made a
/// choice" starting point `scenario::FACTION_INDUSTRY_PRIORITY`'s even
/// split establishes for its own contended goods. Unlike that constant,
/// this one's exact starting numbers cannot change Stage 12A's own
/// behaviour either way - nothing outside this module reads
/// `research_progress` yet (this module's own doc) - so there is no
/// balance judgement being smuggled in here; it exists purely so a fresh
/// faction's allocation is a valid, meaningful `ResearchWeight` from day
/// one (an explicit "nobody has expressed a preference yet" rather than
/// leaning on `tick_research`'s own zero-sum fallback, which would produce
/// the identical even split anyway - see that function's doc).
pub(crate) const FACTION_RESEARCH_ALLOCATION_DEFAULT: [ResearchWeight; RESEARCH_AXIS_COUNT] =
    [ResearchWeight(1.0 / 3.0); RESEARCH_AXIS_COUNT];

/// Daily research progress (docs/phase12-spec.md §0 "ただし速度は経済に依存
/// する"): each axis's `research_progress` grows by this tick's absolute
/// Machinery production (`Faction::machinery_output` -
/// `economy::tick_economy`'s own figure, deliberately *not* the
/// `machinery_output_ratio` ratio, which stays near its ceiling even after
/// a faction loses territory since potential shrinks right alongside actual
/// output - reading the ratio would hide exactly the "losing an industrial
/// region slows research" causality the spec asks for) times the faction's
/// own labour availability this tick (population-weighted
/// `Region::labor_ratio()` over every region it currently owns - drafting
/// manpower away, per the spec's own "徴兵で労働力を削れば... 遅くなる",
/// depresses this independently of whatever `machinery_output` already
/// reflects). The combined rate is split across the three axes by
/// `research_allocation` - falling back to an even three-way split when
/// every weight is `0.0` - the exact same "shared input, proportional
/// share, never a fixed order" mechanism `economy::tick_economy` already
/// uses for `industry_priority` (docs/conventions.md §6).
///
/// A faction with no territory (`total_pop == 0.0`, e.g. every region lost
/// or reassigned) gets `labour == 0.0` and therefore makes zero progress
/// this tick regardless of any stale `machinery_output` value left over
/// from before it lost its last region - see
/// `faction_with_no_territory_makes_no_research_progress`.
///
/// **`research_progress` only ever grows.** This is a deliberate,
/// documented exception to docs/conventions.md §6's "一方通行のアキュムレ
/// ータを作らない" - see `Faction::research_progress`'s own doc (in
/// `world.rs`) for the full reasoning (docs/phase12-spec.md §1) and why a
/// future "fix" adding a cap here would be undoing an intentional design
/// decision, not closing a gap.
pub fn tick_research(world: &mut World) {
    let n = world.factions.len();
    let mut total_pop = vec![0.0f32; n];
    let mut labor_weighted = vec![0.0f32; n];
    for region in &world.regions {
        let f = region.owner.index();
        total_pop[f] += region.population;
        labor_weighted[f] += region.labor_ratio() * region.population;
    }

    for faction in world.factions.iter_mut() {
        if !faction.alive {
            continue;
        }
        let f = faction.id.index();
        let labor = if total_pop[f] > 0.0 { labor_weighted[f] / total_pop[f] } else { 0.0 };
        let rate = faction.machinery_output * labor * RESEARCH_RATE_PER_MACHINERY;

        let weight_sum: f32 = faction.research_allocation.iter().map(|w| w.get()).sum();
        for axis in ALL_RESEARCH_AXES {
            let share = if weight_sum > 0.0 {
                faction.research_allocation[axis.index()].get() / weight_sum
            } else {
                1.0 / RESEARCH_AXIS_COUNT as f32
            };
            faction.research_progress[axis.index()] += rate * share;
        }
    }
}
