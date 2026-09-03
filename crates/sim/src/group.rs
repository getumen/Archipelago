//! Stage 3A (docs/phase3-spec.md "Stage 3A — 国内政治勢力", design.md §11):
//! the seven domestic political factions every `Faction` tracks support and
//! influence for, and the fixed index each one occupies in
//! `Faction::group_support`/`Faction::group_influence`.
//!
//! Always index those arrays through `Group::index()` — never derive an
//! index from hashing or from iterating a `HashMap`/`HashSet` — so the
//! encoding and every accumulation order stay fully deterministic (the same
//! rule `Good::index()` documents for the commodity arrays).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Government,
    LocalGovernment,
    Bureaucracy,
    Military,
    Business,
    Labor,
    Citizens,
}

pub const GROUP_COUNT: usize = 7;

/// Every `Group`, in the fixed order that matches `Group::index()` and the
/// `[f32; GROUP_COUNT]` layout. Iterate this instead of hand-rolling a
/// `0..GROUP_COUNT` loop whenever the code needs the `Group` value itself.
pub const ALL_GROUPS: [Group; GROUP_COUNT] = [
    Group::Government,
    Group::LocalGovernment,
    Group::Bureaucracy,
    Group::Military,
    Group::Business,
    Group::Labor,
    Group::Citizens,
];

impl Group {
    pub const fn index(self) -> usize {
        match self {
            Group::Government => 0,
            Group::LocalGovernment => 1,
            Group::Bureaucracy => 2,
            Group::Military => 3,
            Group::Business => 4,
            Group::Labor => 5,
            Group::Citizens => 6,
        }
    }

    /// Short Japanese label, used by the headless console report.
    pub const fn label(self) -> &'static str {
        match self {
            Group::Government => "中央政府",
            Group::LocalGovernment => "地方政府",
            Group::Bureaucracy => "官僚",
            Group::Military => "軍部",
            Group::Business => "財界",
            Group::Labor => "労働者",
            Group::Citizens => "市民",
        }
    }

    /// Lowercase English key, used by the headless `--json` output.
    pub const fn key(self) -> &'static str {
        match self {
            Group::Government => "government",
            Group::LocalGovernment => "local_government",
            Group::Bureaucracy => "bureaucracy",
            Group::Military => "military",
            Group::Business => "business",
            Group::Labor => "labor",
            Group::Citizens => "citizens",
        }
    }
}
