//! Stage 3B (docs/phase3-spec.md "Stage 3B — 外交関係と条約"): the `Stance`
//! matrix and asymmetric `opinion` between every pair of factions, the six
//! `Treaty` kinds, and the one-tick pending-proposal queue.
//!
//! Everything here is designed against the same failure shape docs/
//! phase3-spec.md §0 (and this project's eight fixed defects) warns about:
//! a machine agent hammering an action every tick looking for a exploit.
//! Concretely:
//! - `opinion` always drifts back toward `0` (`tick_diplomacy`), so no pair
//!   of factions can be driven to a permanently unrecoverable relationship -
//!   the same target-approach discipline `group_support`/`war_support` use.
//! - Re-proposing an already-active treaty is rejected outright by
//!   `action::apply_propose_treaty`; re-proposing the exact same treaty
//!   already pending in the same direction leaves that `PendingProposal`
//!   completely untouched (no TTL refresh, no second `Event::TreatyProposed`)
//!   rather than removing and recreating it - so spamming `ProposeTreaty`
//!   every tick has no effect beyond the first call. Proposing a *different*
//!   treaty to the same target still replaces the old proposal, since only
//!   one outgoing proposal per `(from, to)` pair is kept at a time.
//! - Accepting a proposal clears any other pending proposal for that exact
//!   (pair, treaty) in either direction, and `action::apply_accept_treaty`
//!   independently revalidates the treaty isn't already active before
//!   applying it - together these close off the crossed-bilateral-proposal
//!   shape where accepting each side's proposal in turn would otherwise pay
//!   `TREATY_ACCEPT_OPINION_BONUS` twice for one treaty.
//! - Breaking a treaty (or ending a `Ceasefire` via `DeclareWar`) starts a
//!   real, decrementing `TREATY_COOLDOWN_DAYS` cooldown on re-proposing that
//!   same (pair, treaty) combination - a break/reform/break cycle can't
//!   repeat faster than the cooldown allows, closing off farming
//!   `TREATY_ACCEPT_OPINION_BONUS` by rapid signing and breaking.
//! - `NonAggression`'s notice period is a real countdown
//!   (`PendingBreak::days_left`, ticked down once per day), not a flag
//!   re-checked against a shrinking remainder.

use crate::balance::{
    ALLIANCE_BREAK_OPINION_PENALTY, DECLARE_WAR_OPINION_PENALTY, FOCUS_ALLIANCE_OPINION_RECOVERY_MULT,
    MINOR_TREATY_BREAK_OPINION_PENALTY, NL_PROPOSAL_COOLDOWN_DAYS, NL_PROPOSAL_TTL_DAYS,
    NON_AGGRESSION_BREAK_OPINION_PENALTY, NON_AGGRESSION_NOTICE_DAYS, OPINION_DECAY_RATE,
    PROPOSAL_TTL_DAYS, TREATY_ACCEPT_OPINION_BONUS, TREATY_COOLDOWN_DAYS,
};
use crate::event::Event;
use crate::focus::{self, NationalFocus};
use crate::good::Good;
use crate::ids::{FactionId, RegionId};
use crate::world::World;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stance {
    War,
    Ceasefire,
    NonAggression,
    Alliance,
}

impl Stance {
    pub const fn index(self) -> usize {
        match self {
            Stance::War => 0,
            Stance::Ceasefire => 1,
            Stance::NonAggression => 2,
            Stance::Alliance => 3,
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Stance::War => "war",
            Stance::Ceasefire => "ceasefire",
            Stance::NonAggression => "non_aggression",
            Stance::Alliance => "alliance",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Treaty {
    Ceasefire,
    NonAggression,
    Alliance,
    /// 通行権 (docs/phase3-spec.md): lets the grantee's units sit in the
    /// grantor's territory without `military::tick_occupation` treating that
    /// presence as an invasion, independent of `Stance` - see
    /// `military_access_allows_transit_without_occupation`.
    MilitaryAccess,
    /// 港湾利用: the grantee's `trade::tick_imports`/`tick_trade_agreements`
    /// may count the grantor's own uncontested, unblockaded ports toward the
    /// grantee's import capacity.
    PortAccess,
    /// 貿易: `trade::tick_trade_agreements` moves each of `Food`/`Energy`/
    /// `Machinery` from whichever side has spare stock toward whichever side
    /// is short of it, contending with world-market imports for the
    /// importing side's port capacity on a shared ratio, never a fixed
    /// precedence (see `balance.rs`'s Stage 3B section).
    TradeAgreement,
}

pub const TREATY_COUNT: usize = 6;

pub const ALL_TREATIES: [Treaty; TREATY_COUNT] = [
    Treaty::Ceasefire,
    Treaty::NonAggression,
    Treaty::Alliance,
    Treaty::MilitaryAccess,
    Treaty::PortAccess,
    Treaty::TradeAgreement,
];

impl Treaty {
    pub const fn index(self) -> usize {
        match self {
            Treaty::Ceasefire => 0,
            Treaty::NonAggression => 1,
            Treaty::Alliance => 2,
            Treaty::MilitaryAccess => 3,
            Treaty::PortAccess => 4,
            Treaty::TradeAgreement => 5,
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Treaty::Ceasefire => "ceasefire",
            Treaty::NonAggression => "non_aggression",
            Treaty::Alliance => "alliance",
            Treaty::MilitaryAccess => "military_access",
            Treaty::PortAccess => "port_access",
            Treaty::TradeAgreement => "trade_agreement",
        }
    }

    /// Whether this treaty kind is one of the three mutually-exclusive
    /// `Stance`s (as opposed to the three additive `MilitaryAccess`/
    /// `PortAccess`/`TradeAgreement` grants, which coexist freely with any
    /// stance and with each other).
    pub const fn is_stance(self) -> bool {
        matches!(self, Treaty::Ceasefire | Treaty::NonAggression | Treaty::Alliance)
    }
}

/// Stage 4B (docs/phase4-spec.md "Stage 4B — 自然言語外交"): the structured
/// shape every natural-language proposal must collapse into before it can
/// touch the board. An LLM (or, for a `HeuristicAgent` recipient, keyword
/// extraction - both live in `archipelago-agents`, never in this crate)
/// interprets `Action::ProposeInNaturalLanguage`'s free-text `text` into a
/// `Vec<TreatyTerm>` plus an accept/reject verdict, then hands that back as
/// `Action::RespondToNaturalLanguageProposal` - the *only* thing this crate
/// ever validates or applies is that structured `Vec<TreatyTerm>`, never the
/// text itself (design.md §13: "LLM はゲームルールそのものを決定するもので
/// はない"). See `apply_treaty_terms` for the validation every term goes
/// through, and its doc for why the whole deal is atomic (all terms valid,
/// or none applied).
///
/// Every term reads as a concession *by the proposer* (`Action::
/// ProposeInNaturalLanguage`'s sender) toward the recipient, except `Sign`,
/// which is the same mutual grant/stance change `Action::AcceptTreaty`
/// already produces - so "offer to withdraw in exchange for port access"
/// (design.md §12's worked example) is exactly `[Withdraw{from: ...},
/// Sign(Treaty::PortAccess)]`: the withdrawal is the proposer's own
/// concession, and `PortAccess` being a symmetric grant is what satisfies
/// "recognize our port rights" without needing an asymmetric grant term of
/// its own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TreatyTerm {
    /// Sign `Treaty` between the proposer and the recipient - identical
    /// effect to `Action::AcceptTreaty` (stance change or mutual grant, plus
    /// `TREATY_ACCEPT_OPINION_BONUS` both ways), and gated by the exact same
    /// "not already active" / cooldown checks so this can never be used to
    /// dodge `TREATY_COOLDOWN_DAYS` by going through natural language
    /// instead of `Action::ProposeTreaty`.
    Sign(Treaty),
    /// The proposer withdraws from `from`, which must currently be foreign
    /// soil under the proposer's own control (`owner == proposer`, `core !=
    /// proposer`) - it reverts to `from`'s `core` owner, not necessarily the
    /// recipient of this proposal.
    Withdraw { from: RegionId },
    /// The proposer cedes `region`, which it must currently own, to the
    /// recipient outright.
    Cede { region: RegionId },
    /// The proposer transfers `amount` of `good` from its own stock to the
    /// recipient's.
    Deliver { good: Good, amount: f32 },
}

/// A natural-language proposal awaiting the target faction's own agent
/// (LLM-backed or keyword-fallback) to interpret it into `TreatyTerm`s and
/// answer via `Action::RespondToNaturalLanguageProposal`
/// (docs/phase4-spec.md "Stage 4B"). Deliberately not merged into
/// `PendingProposal` - a natural-language offer has no `Treaty` yet (that's
/// exactly what interpretation produces) and carries free text instead.
#[derive(Clone, PartialEq, Debug)]
pub struct PendingNlProposal {
    pub from: FactionId,
    pub to: FactionId,
    pub text: String,
    pub ttl: u32,
}

/// A proposal awaiting the target faction's `Action::AcceptTreaty`/
/// `RejectTreaty`, visible to `to` (and, for bookkeeping, `from`) through
/// `Observation::encode()` and `Diplomacy::pending`. Expires unanswered once
/// `ttl` reaches `0` (`tick_diplomacy`) — see `PROPOSAL_TTL_DAYS`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PendingProposal {
    pub from: FactionId,
    pub to: FactionId,
    pub treaty: Treaty,
    pub ttl: u32,
}

/// A `NonAggression` break in its notice period (`Action::BreakTreaty`,
/// docs/phase3-spec.md: "破棄には NON_AGGRESSION_NOTICE_DAYS の予告が要る") -
/// the pair stays at `Stance::NonAggression` (combat/occupation still
/// suppressed) until `days_left` counts down to `0`, at which point
/// `tick_diplomacy` actually flips the pair to `Stance::War`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PendingBreak {
    pub a: FactionId,
    pub b: FactionId,
    pub days_left: u32,
}

/// Every faction pair's diplomatic state (docs/phase3-spec.md "関係"/"条約").
/// `stance`/`military_access`/`port_access`/`trade_agreement` are symmetric
/// (mirrored on every write, like `SeaZone::control` is derived
/// symmetrically); `opinion` is deliberately not - the spec calls it "非対称
/// な感情", so `a`'s opinion of `b` can differ from `b`'s of `a`.
#[derive(Clone, Debug)]
pub struct Diplomacy {
    n: usize,
    stance: Vec<Stance>,
    opinion: Vec<f32>,
    military_access: Vec<bool>,
    port_access: Vec<bool>,
    trade_agreement: Vec<bool>,
    /// `cooldown[(a*n+b)*TREATY_COUNT + treaty.index()]`, mirrored on every
    /// write the same way the boolean grants are - days left before
    /// `Action::ProposeTreaty` will accept a new proposal for that (pair,
    /// treaty) combination again (see `balance::TREATY_COOLDOWN_DAYS`).
    cooldown: Vec<u32>,
    /// Proposals awaiting a response, in the order they were made (insertion
    /// order is already deterministic - every action is applied through a
    /// fixed per-tick, per-faction sequence). At most one outgoing proposal
    /// per ordered `(from, to)` pair at a time - see
    /// `action::apply_propose_treaty`.
    pub pending: Vec<PendingProposal>,
    pending_breaks: Vec<PendingBreak>,
    /// Stage 4B (docs/phase4-spec.md "Stage 4B"): outstanding natural-
    /// language proposals, in the order they were made - the free-text
    /// counterpart of `pending`. At most one outgoing proposal per ordered
    /// `(from, to)` pair at a time (`action::apply_propose_nl`), exactly
    /// like `pending`.
    pub pending_nl: Vec<PendingNlProposal>,
    /// `nl_cooldown[from.index() * n + to.index()]`: days left before
    /// `Action::ProposeInNaturalLanguage` will accept a new proposal from
    /// `from` to `to` again, set whenever an outstanding one is answered
    /// (accepted or rejected, `diplomacy::respond_nl`) or expires unanswered
    /// (`tick_diplomacy`) - the same "spend a real, decrementing budget"
    /// guard `Diplomacy::cooldown` gives ordinary treaty proposals, so this
    /// path can't be spammed to flood the event log or repeatedly re-roll
    /// the recipient's interpretation for free (docs/phase3-spec.md §0).
    /// Deliberately one-directional (unlike `cooldown`, which is mirrored):
    /// a natural-language proposal itself has no symmetric "treaty kind" to
    /// key off, and `pending_nl`/`find_pending_nl` are already ordered-pair
    /// specific in exactly the same way.
    nl_cooldown: Vec<u32>,
    /// Events emitted by action appliers the instant a treaty is proposed,
    /// accepted, rejected or broken (`action.rs`'s Stage 3B appliers push
    /// here, since `action::apply_action` has no `&mut Vec<Event>` of its
    /// own to write into). `tick_diplomacy` drains this into the day's
    /// `Simulation::step` event list first thing, so from the outside
    /// diplomacy events appear exactly like every other tick-system event.
    pub(crate) log: Vec<Event>,
}

impl Diplomacy {
    pub fn new(n: usize) -> Self {
        Diplomacy {
            n,
            stance: vec![Stance::War; n * n],
            opinion: vec![0.0; n * n],
            military_access: vec![false; n * n],
            port_access: vec![false; n * n],
            trade_agreement: vec![false; n * n],
            cooldown: vec![0u32; n * n * TREATY_COUNT],
            pending: Vec::new(),
            pending_breaks: Vec::new(),
            pending_nl: Vec::new(),
            nl_cooldown: vec![0u32; n * n],
            log: Vec::new(),
        }
    }

    fn idx(&self, a: FactionId, b: FactionId) -> usize {
        a.index() * self.n + b.index()
    }

    fn cooldown_idx(&self, a: FactionId, b: FactionId, treaty: Treaty) -> usize {
        self.idx(a, b) * TREATY_COUNT + treaty.index()
    }

    pub fn stance(&self, a: FactionId, b: FactionId) -> Stance {
        self.stance[self.idx(a, b)]
    }

    pub fn is_at_war(&self, a: FactionId, b: FactionId) -> bool {
        a != b && self.stance(a, b) == Stance::War
    }

    fn set_stance(&mut self, a: FactionId, b: FactionId, stance: Stance) {
        let i = self.idx(a, b);
        self.stance[i] = stance;
        let j = self.idx(b, a);
        self.stance[j] = stance;
    }

    /// `a`'s opinion of `b`, `-100..100`.
    pub fn opinion(&self, a: FactionId, b: FactionId) -> f32 {
        self.opinion[self.idx(a, b)]
    }

    /// Adjusts `a`'s opinion of `b` by `delta`, clamped to `-100..100` -
    /// never the sole guarantee against pinning (see `tick_diplomacy`'s
    /// decay), but a hard backstop against a single call overshooting the
    /// range.
    pub fn adjust_opinion(&mut self, a: FactionId, b: FactionId, delta: f32) {
        let i = self.idx(a, b);
        self.opinion[i] = (self.opinion[i] + delta).clamp(-100.0, 100.0);
    }

    pub fn has_military_access(&self, a: FactionId, b: FactionId) -> bool {
        a != b && self.military_access[self.idx(a, b)]
    }

    pub fn has_port_access(&self, a: FactionId, b: FactionId) -> bool {
        a != b && self.port_access[self.idx(a, b)]
    }

    pub fn has_trade_agreement(&self, a: FactionId, b: FactionId) -> bool {
        a != b && self.trade_agreement[self.idx(a, b)]
    }

    pub fn has_treaty(&self, a: FactionId, b: FactionId, treaty: Treaty) -> bool {
        match treaty {
            Treaty::Ceasefire => self.stance(a, b) == Stance::Ceasefire,
            Treaty::NonAggression => self.stance(a, b) == Stance::NonAggression,
            Treaty::Alliance => self.stance(a, b) == Stance::Alliance,
            Treaty::MilitaryAccess => self.has_military_access(a, b),
            Treaty::PortAccess => self.has_port_access(a, b),
            Treaty::TradeAgreement => self.has_trade_agreement(a, b),
        }
    }

    fn set_bool_grant(&mut self, treaty: Treaty, a: FactionId, b: FactionId, value: bool) {
        let vec = match treaty {
            Treaty::MilitaryAccess => &mut self.military_access,
            Treaty::PortAccess => &mut self.port_access,
            Treaty::TradeAgreement => &mut self.trade_agreement,
            _ => return,
        };
        let n = self.n;
        vec[a.index() * n + b.index()] = value;
        vec[b.index() * n + a.index()] = value;
    }

    pub fn cooldown(&self, a: FactionId, b: FactionId, treaty: Treaty) -> u32 {
        self.cooldown[self.cooldown_idx(a, b, treaty)]
    }

    fn set_cooldown(&mut self, a: FactionId, b: FactionId, treaty: Treaty, days: u32) {
        let i = self.cooldown_idx(a, b, treaty);
        self.cooldown[i] = days;
        let j = self.cooldown_idx(b, a, treaty);
        self.cooldown[j] = days;
    }

    /// The index into `pending` of an outstanding `from -> to` proposal, if any.
    pub fn find_pending(&self, from: FactionId, to: FactionId) -> Option<usize> {
        self.pending.iter().position(|p| p.from == from && p.to == to)
    }

    /// The index into `pending_nl` of an outstanding `from -> to`
    /// natural-language proposal, if any.
    pub fn find_pending_nl(&self, from: FactionId, to: FactionId) -> Option<usize> {
        self.pending_nl.iter().position(|p| p.from == from && p.to == to)
    }

    /// Days left before `from` may propose a natural-language deal to `to`
    /// again (see `nl_cooldown`'s doc).
    pub fn nl_cooldown(&self, from: FactionId, to: FactionId) -> u32 {
        self.nl_cooldown[self.idx(from, to)]
    }

    fn set_nl_cooldown(&mut self, from: FactionId, to: FactionId, days: u32) {
        let i = self.idx(from, to);
        self.nl_cooldown[i] = days;
    }

    pub fn pending_break(&self, a: FactionId, b: FactionId) -> Option<PendingBreak> {
        self.pending_breaks
            .iter()
            .copied()
            .find(|pb| (pb.a == a && pb.b == b) || (pb.a == b && pb.b == a))
    }
}

/// Applies `Action::ProposeTreaty`'s validated request: replaces any
/// existing `from -> to` proposal *for a different treaty* (never stacks a
/// second one) and logs `Event::TreatyProposed` - but re-proposing the exact
/// same `treaty` that's already pending in that direction is a true no-op,
/// leaving the original `PendingProposal` (and its `ttl`) completely
/// untouched. External code review fix A3: the previous code unconditionally
/// removed-and-recreated on every call, so re-proposing the same pending
/// treaty every tick refreshed its TTL indefinitely (an agent could keep a
/// proposal alive forever just by resubmitting it) and re-logged
/// `Event::TreatyProposed` every time, flooding the event log - exactly the
/// "an optimiser spams the same action every tick" shape docs/phase3-spec.md
/// §0 and this module's own doc warn against. `apply_propose_treaty` already
/// rejects a proposal for a treaty already *active*; this only has to guard
/// the pending-but-not-yet-answered case.
pub(crate) fn propose(world: &mut World, from: FactionId, to: FactionId, treaty: Treaty) {
    if let Some(existing) = world.diplomacy.pending.iter().find(|p| p.from == from && p.to == to) {
        if existing.treaty == treaty {
            return;
        }
    }
    world.diplomacy.pending.retain(|p| !(p.from == from && p.to == to));
    world.diplomacy.pending.push(PendingProposal {
        from,
        to,
        treaty,
        ttl: PROPOSAL_TTL_DAYS,
    });
    world.diplomacy.log.push(Event::TreatyProposed { from, to, treaty });
}

/// Applies an accepted proposal's effect: a `Stance` treaty sets the stance
/// matrix, a grant treaty sets its boolean both ways; either way both
/// factions' opinion of each other improves and `Event::TreatySigned` is
/// logged.
///
/// External code review fix A2: also clears any *other* outstanding
/// `pending` proposal for this exact `(a, b, treaty)` pair, in either
/// direction - the crossed-bilateral case where `a` proposed this same
/// treaty to `b` while `b` independently proposed it to `a`. Without this,
/// accepting one direction leaves the reverse proposal sitting in the queue
/// looking perfectly valid; accepting *that* one later would call this
/// function a second time and pay `TREATY_ACCEPT_OPINION_BONUS` again for a
/// treaty that was already active - a free, repeatable way to farm the
/// signing bonus. `apply_accept_treaty`'s own `has_treaty` revalidation is a
/// second, independent backstop against the same shape (defense in depth:
/// this clears the stale proposal proactively, that guards against ever
/// acting on one that slipped through some other path).
pub(crate) fn accept(world: &mut World, a: FactionId, b: FactionId, treaty: Treaty) {
    if treaty.is_stance() {
        let stance = match treaty {
            Treaty::Ceasefire => Stance::Ceasefire,
            Treaty::NonAggression => Stance::NonAggression,
            Treaty::Alliance => Stance::Alliance,
            _ => unreachable!(),
        };
        world.diplomacy.set_stance(a, b, stance);
    } else {
        world.diplomacy.set_bool_grant(treaty, a, b, true);
    }
    world.diplomacy.adjust_opinion(a, b, TREATY_ACCEPT_OPINION_BONUS);
    world.diplomacy.adjust_opinion(b, a, TREATY_ACCEPT_OPINION_BONUS);
    world.diplomacy.pending.retain(|p| {
        !(p.treaty == treaty && ((p.from == a && p.to == b) || (p.from == b && p.to == a)))
    });
    world.diplomacy.log.push(Event::TreatySigned { a, b, treaty });
}

pub(crate) fn reject(world: &mut World, from: FactionId, to: FactionId, treaty: Treaty) {
    world.diplomacy.log.push(Event::TreatyRejected { from, to, treaty });
}

/// Applies `Action::ProposeInNaturalLanguage`'s validated request - the
/// free-text counterpart of `propose`. `action::apply_propose_nl` already
/// rejects a repeat attempt while one is outstanding or on cooldown, so this
/// only ever runs for a genuinely new proposal.
pub(crate) fn propose_nl(world: &mut World, from: FactionId, to: FactionId, text: String) {
    world.diplomacy.log.push(Event::NaturalLanguageProposed { from, to, text: text.clone() });
    world.diplomacy.pending_nl.push(PendingNlProposal { from, to, text, ttl: NL_PROPOSAL_TTL_DAYS });
}

/// Applies `Action::RespondToNaturalLanguageProposal`'s validated request -
/// `action::apply_respond_nl` has already consumed the matching
/// `PendingNlProposal` before calling in here. Always spends a fresh
/// `NL_PROPOSAL_COOLDOWN_DAYS` cooldown on this `(from, to)` pair regardless
/// of outcome (accepted, rejected, or accepted-but-invalid) - the same
/// "answering a proposal costs a real, decrementing slot" guard
/// `tick_diplomacy`'s expiry path also spends, so a proposer can't force a
/// tighter retry loop than a genuinely un-interested recipient would allow
/// just by getting an instant rejection instead of letting one expire.
///
/// `accept_deal == true` does **not** mean the deal takes effect - see
/// `apply_treaty_terms`'s doc: every term must independently validate
/// against the *current* world, so an interpretation (LLM or keyword) that
/// says "accept" but names an infeasible term (a region the proposer
/// doesn't hold, more of a good than it has in stock, ...) still produces no
/// change to the board, only `Event::NaturalLanguageTermsInvalid`.
pub(crate) fn respond_nl(
    world: &mut World,
    from: FactionId,
    to: FactionId,
    terms: &[TreatyTerm],
    accept_deal: bool,
) {
    world.diplomacy.set_nl_cooldown(from, to, NL_PROPOSAL_COOLDOWN_DAYS);
    if !accept_deal {
        world.diplomacy.log.push(Event::NaturalLanguageRejected { from, to, terms: terms.to_vec() });
        return;
    }
    if apply_treaty_terms(world, from, to, terms) {
        world.diplomacy.log.push(Event::NaturalLanguageAccepted { from, to, terms: terms.to_vec() });
    } else {
        world.diplomacy.log.push(Event::NaturalLanguageTermsInvalid { from, to, terms: terms.to_vec() });
    }
}

/// The only place a `Vec<TreatyTerm>` ever touches the board
/// (docs/phase4-spec.md "Stage 4B": "自然言語のまま盤面に効くことは決してな
/// い"). All-or-nothing on purpose - the same atomicity
/// `action::apply_accept_treaty` gets from being one `Action`, extended to a
/// whole natural-language deal so a recipient (or a hostile LLM response)
/// can never cherry-pick "accept the feasible half of the deal, discard the
/// rest" by bundling one valid term with one invalid one. Returns whether
/// the deal was applied.
///
/// External code review fix: the previous implementation validated every
/// term against the *unchanged* world and only afterwards applied all of
/// them, so a batch could pass validation and still produce impossible
/// state - two `Deliver` terms that each individually fit inside the
/// proposer's stock but together overdraw it (leaving a negative stock), or
/// two identical `Sign(treaty)` terms that each individually see the treaty
/// as not-yet-active and so both "pass", paying
/// `TREATY_ACCEPT_OPINION_BONUS` twice for one signing. Since the terms come
/// from an LLM's (or a hostile actor's) interpretation of free text, this is
/// exactly the "an optimiser composes a batch that individually validates
/// but collectively cheats" shape docs/phase3-spec.md §0 and design.md
/// §15/§18 warn about. Fixed in two parts:
///
/// 1. An exact-duplicate `Sign(treaty)` is collapsed to its first occurrence
///    before validation - `Sign` is a pure state-flag grant (like
///    `propose`'s existing "re-proposing an already-pending treaty is a
///    no-op" idiom), so re-asserting one this same batch already applied is
///    idempotent, not a second concession worth a second bonus. Every other
///    term kind (`Withdraw`/`Cede`/`Deliver`) is a real transfer with its
///    own cost, so a literal repeat of one of those is deliberately left
///    alone here and instead falls through to cumulative validation below.
/// 2. The (deduplicated) batch is then validated and applied term-by-term
///    against a scratch clone of the world, so a term that only becomes
///    infeasible *because of an earlier term in this same batch* (two
///    `Deliver`s that individually fit but together overdraw; a `Cede` of a
///    region an earlier `Withdraw` already gave away; ...) is caught before
///    anything real changes. The real `world` is reassigned from the scratch
///    copy only if every term validated and applied cleanly - on any
///    failure `world` is returned untouched, preserving the same
///    all-or-nothing guarantee the old validate-then-apply-all shape
///    intended but didn't actually enforce cumulatively.
pub(crate) fn apply_treaty_terms(
    world: &mut World,
    proposer: FactionId,
    recipient: FactionId,
    terms: &[TreatyTerm],
) -> bool {
    if terms.is_empty() {
        return false; // an "accept" with nothing to accept is not a deal.
    }

    let mut deduped: Vec<TreatyTerm> = Vec::with_capacity(terms.len());
    for &term in terms {
        let already_signed = matches!(term, TreatyTerm::Sign(_)) && deduped.contains(&term);
        if !already_signed {
            deduped.push(term);
        }
    }

    let mut scratch = world.clone();
    for &term in &deduped {
        if !term_is_valid(&scratch, proposer, recipient, term) {
            return false;
        }
        apply_term(&mut scratch, proposer, recipient, term);
    }
    *world = scratch;
    true
}

/// Read-only feasibility check for one `TreatyTerm`, against the world as it
/// stands *right now* - never against whatever the proposer's situation was
/// when the natural-language text was first sent (which may be days stale
/// by the time the recipient answers). See `TreatyTerm`'s doc for what each
/// variant means and which side (`proposer`/`recipient`) it acts on.
fn term_is_valid(world: &World, proposer: FactionId, recipient: FactionId, term: TreatyTerm) -> bool {
    match term {
        TreatyTerm::Sign(treaty) => {
            !world.diplomacy.has_treaty(proposer, recipient, treaty)
                && world.diplomacy.cooldown(proposer, recipient, treaty) == 0
        }
        TreatyTerm::Withdraw { from } => world.regions.get(from.index()).is_some_and(|r| {
            r.owner == proposer && r.core != proposer && !world.has_enemy_units(from, proposer)
        }),
        TreatyTerm::Cede { region } => world.regions.get(region.index()).is_some_and(|r| {
            r.owner == proposer && !world.has_enemy_units(region, proposer)
        }),
        TreatyTerm::Deliver { good, amount } => {
            amount.is_finite() && amount >= 0.0 && world.faction(proposer).stock[good.index()] >= amount
        }
    }
}

/// Applies one already-validated `TreatyTerm`. Never called directly by an
/// action applier - only through `apply_treaty_terms`, after every term in
/// the batch has passed `term_is_valid`.
fn apply_term(world: &mut World, proposer: FactionId, recipient: FactionId, term: TreatyTerm) {
    match term {
        TreatyTerm::Sign(treaty) => accept(world, proposer, recipient, treaty),
        TreatyTerm::Withdraw { from } => {
            let core = world.region(from).core;
            transfer_region(world, from, core);
        }
        TreatyTerm::Cede { region } => transfer_region(world, region, recipient),
        TreatyTerm::Deliver { good, amount } => {
            world.faction_mut(proposer).stock[good.index()] -= amount;
            world.faction_mut(recipient).stock[good.index()] += amount;
        }
    }
}

/// Peacefully hands `region_id` to `new_owner`, outside of combat -
/// mirrors the ownership-change bookkeeping `military::tick_occupation`
/// does on a violent capture (occupation/occupier/occupation_kind cleared,
/// any in-progress project discarded since it belonged to the previous
/// owner) but deliberately does *not* add the devastation/unrest spike a
/// captured-by-force region gets: this is a negotiated transfer, not a
/// battle.
fn transfer_region(world: &mut World, region_id: RegionId, new_owner: FactionId) {
    let region = world.region_mut(region_id);
    region.owner = new_owner;
    region.occupation = 0.0;
    region.occupier = None;
    region.occupation_kind = None;
    region.construction = None;
}

/// `Action::DeclareWar`: only ever used to break an active `Ceasefire`
/// (`action::apply_declare_war` rejects every other current stance) -
/// immediate, no notice. Clears every grant treaty between the pair (they
/// stop making sense once the two are shooting at each other again), drags
/// in each side's current allies (docs/phase3-spec.md: "同盟国が攻撃された
/// ら自動参戦する", a single non-recursive pass over both sides' allies so
/// the cascade stays bounded), and locks `Treaty::Ceasefire` on cooldown so
/// the pair can't instantly re-propose peace.
pub(crate) fn declare_war(world: &mut World, a: FactionId, b: FactionId, events: &mut Vec<Event>) {
    start_war(world, a, b, events);
}

/// Shared by `declare_war` and `tick_diplomacy`'s `NonAggression`-notice
/// expiry - the actual mechanics of two factions going to war, independent
/// of how they got there.
fn start_war(world: &mut World, a: FactionId, b: FactionId, events: &mut Vec<Event>) {
    world.diplomacy.set_stance(a, b, Stance::War);
    for treaty in [Treaty::MilitaryAccess, Treaty::PortAccess, Treaty::TradeAgreement] {
        world.diplomacy.set_bool_grant(treaty, a, b, false);
    }
    world.diplomacy.adjust_opinion(a, b, -DECLARE_WAR_OPINION_PENALTY);
    world.diplomacy.adjust_opinion(b, a, -DECLARE_WAR_OPINION_PENALTY);
    world.diplomacy.set_cooldown(a, b, Treaty::Ceasefire, TREATY_COOLDOWN_DAYS);
    events.push(Event::WarDeclared { a, b });

    let n = world.factions.len();
    let allies_of = |world: &World, x: FactionId, exclude: FactionId| -> Vec<FactionId> {
        (0..n)
            .map(|i| FactionId(i as u32))
            .filter(|&y| y != x && y != exclude && world.diplomacy.stance(x, y) == Stance::Alliance)
            .collect()
    };
    let a_allies = allies_of(world, a, b);
    let b_allies = allies_of(world, b, a);
    for ally in a_allies {
        if !world.diplomacy.is_at_war(ally, b) {
            drag_into_war(world, ally, b, events);
        }
    }
    for ally in b_allies {
        if !world.diplomacy.is_at_war(ally, a) {
            drag_into_war(world, ally, a, events);
        }
    }
}

/// The alliance-drag-in half of `start_war`: sets the pair straight to War
/// (no notice - an ally honoring its pact isn't the one who broke a treaty)
/// and logs it as its own event so a log reader can tell an original
/// declaration from an ally being pulled in.
fn drag_into_war(world: &mut World, a: FactionId, b: FactionId, events: &mut Vec<Event>) {
    world.diplomacy.set_stance(a, b, Stance::War);
    for treaty in [Treaty::MilitaryAccess, Treaty::PortAccess, Treaty::TradeAgreement] {
        world.diplomacy.set_bool_grant(treaty, a, b, false);
    }
    world.diplomacy.adjust_opinion(a, b, -DECLARE_WAR_OPINION_PENALTY * 0.5);
    world.diplomacy.adjust_opinion(b, a, -DECLARE_WAR_OPINION_PENALTY * 0.5);
    events.push(Event::AllianceDragIn { faction: a, into_war_with: b });
}

/// `Action::BreakTreaty`, dispatched by treaty kind - see each arm's doc.
pub(crate) fn break_treaty(world: &mut World, a: FactionId, b: FactionId, treaty: Treaty) {
    match treaty {
        Treaty::Ceasefire => unreachable!("apply_break_treaty rejects Ceasefire before calling in"),
        Treaty::NonAggression => {
            // Notice period: the pair stays at NonAggression (still no
            // combat/occupation) until `tick_diplomacy` counts the pending
            // break down to zero and actually starts the war.
            world.diplomacy.pending_breaks.push(PendingBreak {
                a,
                b,
                days_left: NON_AGGRESSION_NOTICE_DAYS,
            });
            world.diplomacy.adjust_opinion(a, b, -NON_AGGRESSION_BREAK_OPINION_PENALTY);
            world.diplomacy.adjust_opinion(b, a, -NON_AGGRESSION_BREAK_OPINION_PENALTY);
        }
        Treaty::Alliance => {
            // Immediate but not war - a lapsed alliance is a rupture, not
            // an attack.
            world.diplomacy.set_stance(a, b, Stance::Ceasefire);
            world.diplomacy.adjust_opinion(a, b, -ALLIANCE_BREAK_OPINION_PENALTY);
            world.diplomacy.adjust_opinion(b, a, -ALLIANCE_BREAK_OPINION_PENALTY);
            world.diplomacy.set_cooldown(a, b, Treaty::Alliance, TREATY_COOLDOWN_DAYS);
        }
        Treaty::MilitaryAccess | Treaty::PortAccess | Treaty::TradeAgreement => {
            world.diplomacy.set_bool_grant(treaty, a, b, false);
            world.diplomacy.adjust_opinion(a, b, -MINOR_TREATY_BREAK_OPINION_PENALTY);
            world.diplomacy.adjust_opinion(b, a, -MINOR_TREATY_BREAK_OPINION_PENALTY);
            world.diplomacy.set_cooldown(a, b, treaty, TREATY_COOLDOWN_DAYS);
        }
    }
    world.diplomacy.log.push(Event::TreatyBroken { a, b, treaty });
}

/// Once-a-day diplomacy maintenance (`Simulation::step`, called early -
/// before combat, so a `NonAggression` notice expiring today already stops
/// suppressing combat/occupation for the rest of *today's* tick):
/// 1. Drains `Diplomacy::log` (proposals/accepts/rejects/breaks recorded by
///    `action.rs` the instant they happened) into the day's event list.
/// 2. Counts down and expires unanswered `PendingProposal`s.
/// 3. Counts down `PendingBreak`s, starting the war the instant one reaches zero.
/// 4. Counts down every (pair, treaty) cooldown.
/// 5. Decays every `opinion` entry a fraction of the way back toward `0`.
pub fn tick_diplomacy(world: &mut World, events: &mut Vec<Event>) {
    events.append(&mut world.diplomacy.log);

    // Expire proposals whose ttl already reached 0 on a previous day, then
    // decrement everything still outstanding - so a proposal created today
    // (ttl == PROPOSAL_TTL_DAYS) survives through tomorrow's tick_diplomacy
    // call before it can expire, giving every faction's turn a fair chance
    // to see and answer it via `Observation`, regardless of iteration order
    // within the day it was made.
    world.diplomacy.pending.retain(|p| p.ttl > 0);
    for p in world.diplomacy.pending.iter_mut() {
        p.ttl -= 1;
    }

    // Stage 4B: the same expiry shape as `pending` above, but an expired
    // natural-language proposal additionally spends `NL_PROPOSAL_COOLDOWN_DAYS`
    // on its `(from, to)` pair (`diplomacy::respond_nl`'s doc) - letting an
    // unanswered proposal expire for free would otherwise be a *cheaper* way
    // to retry than getting an explicit rejection back.
    let expired_nl: Vec<(FactionId, FactionId)> = world
        .diplomacy
        .pending_nl
        .iter()
        .filter(|p| p.ttl == 0)
        .map(|p| (p.from, p.to))
        .collect();
    world.diplomacy.pending_nl.retain(|p| p.ttl > 0);
    for p in world.diplomacy.pending_nl.iter_mut() {
        p.ttl -= 1;
    }
    for (from, to) in expired_nl {
        world.diplomacy.set_nl_cooldown(from, to, NL_PROPOSAL_COOLDOWN_DAYS);
    }

    for c in world.diplomacy.nl_cooldown.iter_mut() {
        if *c > 0 {
            *c -= 1;
        }
    }

    let mut resolved_breaks = Vec::new();
    for pb in world.diplomacy.pending_breaks.iter_mut() {
        if pb.days_left > 0 {
            pb.days_left -= 1;
        }
        if pb.days_left == 0 {
            resolved_breaks.push((pb.a, pb.b));
        }
    }
    world.diplomacy.pending_breaks.retain(|pb| pb.days_left > 0);
    for (a, b) in resolved_breaks {
        start_war(world, a, b, events);
    }

    for c in world.diplomacy.cooldown.iter_mut() {
        if *c > 0 {
            *c -= 1;
        }
    }

    let n = world.factions.len();
    // Stage 3C `NationalFocus::AllianceNetwork` (docs/phase3-spec.md:
    // "opinion の回復＋"): read once per faction up front, the same way
    // `mutiny`/`org_cap` are precomputed elsewhere - `a`'s own focus speeds
    // up how fast a *negative* opinion of `b` climbs back toward neutral.
    // Deliberately only while `opinion[i] < 0.0` ("recovery," not a faster
    // decay of an opinion that's already positive) so the focus never erodes
    // a hard-won good relationship faster.
    let alliance_mult: Vec<f32> = world
        .factions
        .iter()
        .map(|f| {
            if focus::active(f) == Some(NationalFocus::AllianceNetwork) {
                FOCUS_ALLIANCE_OPINION_RECOVERY_MULT
            } else {
                1.0
            }
        })
        .collect();
    for a_idx in 0..n {
        for b_idx in 0..n {
            if a_idx == b_idx {
                continue;
            }
            let i = a_idx * n + b_idx;
            let base = world.diplomacy.opinion[i];
            let rate = if base < 0.0 {
                OPINION_DECAY_RATE * alliance_mult[a_idx]
            } else {
                OPINION_DECAY_RATE
            };
            world.diplomacy.opinion[i] -= base * rate;
        }
    }
}
