//! The tradeable commodities in Stage 2A's production chain, and the fixed
//! index each one occupies in every `[f32; GOOD_COUNT]` array
//! (`Region::capacity`, `Faction::stock`, `Faction::industry_priority`).
//!
//! Always index those arrays through `Good::index()` — never derive an
//! index from hashing or from iterating a `HashMap`/`HashSet` — so the
//! encoding and every accumulation order stay fully deterministic.
//!
//! Stage 11A (docs/phase11-spec.md §2) splits the single `Arms` commodity
//! Phase 1 introduced into one good per land branch: `Infantry`, `Armour`
//! and `Artillery`. `Arms` is **re-read as `Infantry`**, not eliminated
//! outright: every existing consumer (land unit equipment, and — until
//! Stage 11B gives Naval/Air their own dedicated commodity — Sea/Air
//! equipment too) keeps drawing exactly the same stock/capacity numbers it
//! always has, under its new, narrower name. The alternative the spec
//! allowed (dropping `Arms` for three brand-new goods with no `Infantry`
//! survivor) would have meant re-deriving Sea/Air's supply from scratch
//! with no historical baseline to check it against - re-reading preserves
//! one, unambiguous meaning for the renamed slot instead
//! (docs/conventions.md's "同じ名前が2つの意味を持つと必ず食い違う": `Arms`
//! never meant "every branch's equipment" *and* "just infantry's" at once
//! in this codebase, and it still doesn't after the rename).
//!
//! `Armour` and `Artillery` are genuinely new: Stage 11A gives every
//! scenario region-varying production capacity for them (see
//! `tools/hexmap/build_scenario.py` and `scenarios/mvp.json`/`japan47.json`)
//! so the data exists and differs region to region, but **no unit type
//! draws on either stock yet** ("部隊種別は入れない" - that is Stage 11B).
//! Their production (`economy::tick_economy`) is deliberately decoupled
//! from the Machinery/Steel input `Infantry` already fully claims, so
//! introducing them cannot perturb a single existing number - see
//! `economy::tick_economy`'s own doc for why, and
//! `Region::industry_total`'s doc for the matching reason its own sum
//! excludes them for now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Good {
    Food,
    Energy,
    Steel,
    Machinery,
    Munitions,
    /// The historical `Arms` slot, re-read as land's infantry-branch
    /// equipment (this module's own doc has the full account of why this
    /// was a rename, not a replacement).
    Infantry,
    Armour,
    Artillery,
}

pub const GOOD_COUNT: usize = 8;

/// Every `Good`, in the fixed order that matches `Good::index()` and the
/// `[f32; GOOD_COUNT]` layout. Iterate this instead of hand-rolling a
/// `0..GOOD_COUNT` loop whenever the code needs the `Good` value itself.
pub const ALL_GOODS: [Good; GOOD_COUNT] = [
    Good::Food,
    Good::Energy,
    Good::Steel,
    Good::Machinery,
    Good::Munitions,
    Good::Infantry,
    Good::Armour,
    Good::Artillery,
];

impl Good {
    pub const fn index(self) -> usize {
        match self {
            Good::Food => 0,
            Good::Energy => 1,
            Good::Steel => 2,
            Good::Machinery => 3,
            Good::Munitions => 4,
            Good::Infantry => 5,
            Good::Armour => 6,
            Good::Artillery => 7,
        }
    }

    /// Short Japanese label, used by the headless console report.
    pub const fn label(self) -> &'static str {
        match self {
            Good::Food => "食料",
            Good::Energy => "エネルギー",
            Good::Steel => "鉄鋼",
            Good::Machinery => "機械",
            Good::Munitions => "軍需品",
            Good::Infantry => "歩兵装備",
            Good::Armour => "機甲装備",
            Good::Artillery => "砲兵装備",
        }
    }

    /// Lowercase English key, used by the headless `--json` output.
    pub const fn key(self) -> &'static str {
        match self {
            Good::Food => "food",
            Good::Energy => "energy",
            Good::Steel => "steel",
            Good::Machinery => "machinery",
            Good::Munitions => "munitions",
            Good::Infantry => "infantry",
            Good::Armour => "armour",
            Good::Artillery => "artillery",
        }
    }
}
