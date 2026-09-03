//! The six tradeable commodities in Stage 2A's production chain, and the
//! fixed index each one occupies in every `[f32; GOOD_COUNT]` array
//! (`Region::capacity`, `Faction::stock`, `Faction::industry_priority`).
//!
//! Always index those arrays through `Good::index()` — never derive an
//! index from hashing or from iterating a `HashMap`/`HashSet` — so the
//! encoding and every accumulation order stay fully deterministic.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Good {
    Food,
    Energy,
    Steel,
    Machinery,
    Munitions,
    Arms,
}

pub const GOOD_COUNT: usize = 6;

/// Every `Good`, in the fixed order that matches `Good::index()` and the
/// `[f32; GOOD_COUNT]` layout. Iterate this instead of hand-rolling a
/// `0..GOOD_COUNT` loop whenever the code needs the `Good` value itself.
pub const ALL_GOODS: [Good; GOOD_COUNT] = [
    Good::Food,
    Good::Energy,
    Good::Steel,
    Good::Machinery,
    Good::Munitions,
    Good::Arms,
];

impl Good {
    pub const fn index(self) -> usize {
        match self {
            Good::Food => 0,
            Good::Energy => 1,
            Good::Steel => 2,
            Good::Machinery => 3,
            Good::Munitions => 4,
            Good::Arms => 5,
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
            Good::Arms => "兵器",
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
            Good::Arms => "arms",
        }
    }
}
