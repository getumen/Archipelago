//! Stage 3C national focus (docs/phase3-spec.md "Stage 3C — 国家方針"): the
//! six long-term strategic postures a faction can commit to via
//! `Action::SetNationalFocus`, and the switch/transition bookkeeping every
//! system that applies a focus modifier shares.
//!
//! A focus is characterisation, not a victory condition (docs/phase3-spec.md:
//! "方針は勝利条件ではなく性格づけである。勝敗は従来どおり領土と生存で決まる
//! が、方針によって取れる戦略が変わる"): it changes which strategies are
//! cheap, not who wins. Every modifier a focus applies lives next to the
//! system it touches — `politics.rs` (group support), `military.rs` (unit
//! organization ceiling and home-soil/offense combat multipliers),
//! `trade.rs` (import capacity, `TradeAgreement` flow), `action.rs` (fleet
//! build cost), `construction.rs` (build speed, devastation recovery),
//! `economy.rs` (production efficiency) and `diplomacy.rs` (opinion
//! recovery) — each named in `balance.rs`'s Stage 3C section. This module
//! only owns the enum itself and the shared "is a focus currently in
//! effect" / "advance today's switch countdown" primitives.
//!
//! ## Why rapid switching can't be exploited (docs/phase3-spec.md §0)
//!
//! `Action::SetNationalFocus` starts a real, decrementing
//! `balance::FOCUS_SWITCH_DAYS` transition (`Faction::focus_transition_days`)
//! during which `active()` returns `None` — **neither** the abandoned focus's
//! effects **nor** the new one's apply. That "both suspended" rule, not just
//! "the new one is late," is what makes switching a strict cost with no
//! exploitable window:
//! - You can never hold two foci's benefits at once by switching back and
//!   forth faster than the transition — every switch, however brief the
//!   intent, pays the full blackout.
//! - `action::apply_set_national_focus` rejects retargeting a switch that's
//!   already under way (any different focus while
//!   `focus_transition_days > 0`), so the elapsed portion of a transition can
//!   never be redirected toward a different destination for free, and
//!   re-affirming the *same* target mid-transition is a pure no-op rather
//!   than a timer reset — so an agent hammering the action every tick gets
//!   exactly the same outcome, on exactly the same day, as a single call.
//! - `focus_transition_days` is a spent-down budget ticked once per day by
//!   `tick_national_focus`, never a ratio re-applied to a remainder.

use crate::world::{Faction, World};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NationalFocus {
    MilitaryUnification,
    EconomicSphere,
    AllianceNetwork,
    MaritimeTrade,
    Technocracy,
    DefensivePosture,
}

pub const FOCUS_COUNT: usize = 6;

/// Every `NationalFocus`, in the fixed order that matches `index()` — the
/// same enumeration convention `group::ALL_GROUPS`/`diplomacy::ALL_TREATIES`
/// use.
pub const ALL_FOCI: [NationalFocus; FOCUS_COUNT] = [
    NationalFocus::MilitaryUnification,
    NationalFocus::EconomicSphere,
    NationalFocus::AllianceNetwork,
    NationalFocus::MaritimeTrade,
    NationalFocus::Technocracy,
    NationalFocus::DefensivePosture,
];

impl NationalFocus {
    pub const fn index(self) -> usize {
        match self {
            NationalFocus::MilitaryUnification => 0,
            NationalFocus::EconomicSphere => 1,
            NationalFocus::AllianceNetwork => 2,
            NationalFocus::MaritimeTrade => 3,
            NationalFocus::Technocracy => 4,
            NationalFocus::DefensivePosture => 5,
        }
    }

    /// Lowercase English key, used by the headless `--json` output.
    pub const fn key(self) -> &'static str {
        match self {
            NationalFocus::MilitaryUnification => "military_unification",
            NationalFocus::EconomicSphere => "economic_sphere",
            NationalFocus::AllianceNetwork => "alliance_network",
            NationalFocus::MaritimeTrade => "maritime_trade",
            NationalFocus::Technocracy => "technocracy",
            NationalFocus::DefensivePosture => "defensive_posture",
        }
    }

    /// Short Japanese label, used by the headless console report.
    pub const fn label(self) -> &'static str {
        match self {
            NationalFocus::MilitaryUnification => "軍事的統一",
            NationalFocus::EconomicSphere => "経済圏の構築",
            NationalFocus::AllianceNetwork => "同盟の形成",
            NationalFocus::MaritimeTrade => "海上貿易国家",
            NationalFocus::Technocracy => "技術国家",
            NationalFocus::DefensivePosture => "防衛特化",
        }
    }
}

/// The focus currently in effect for `faction`, or `None` while a switch's
/// `FOCUS_SWITCH_DAYS` transition is still running. Every system that applies
/// a Stage 3C modifier reads this — never `Faction::national_focus`
/// directly — so a mid-transition faction gets neither its old focus's
/// effects nor its new one's (see this module's doc for why that's what
/// keeps rapid switching from ever being an advantage).
pub fn active(faction: &Faction) -> Option<NationalFocus> {
    if faction.focus_transition_days == 0 {
        Some(faction.national_focus)
    } else {
        None
    }
}

/// Once-a-day maintenance (`Simulation::step`, run early alongside
/// `diplomacy::tick_diplomacy`'s own countdowns): decrements every living
/// faction's in-progress focus-switch transition, a real spent-down budget
/// rather than a ratio re-applied to a remainder.
pub fn tick_national_focus(world: &mut World) {
    for faction in world.factions.iter_mut() {
        if faction.focus_transition_days > 0 {
            faction.focus_transition_days -= 1;
        }
    }
}
