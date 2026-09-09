//! Notable happenings emitted by tick systems, for logging and UI.

use std::fmt;

use crate::diplomacy::{Treaty, TreatyTerm};
use crate::ids::{FactionId, RegionId, SeaZoneId, TransportLineId, TransportNodeId, UnitId};
use crate::transport::TransportNodeKind;
use crate::world::Station;

/// What a `NodeStruck` sortie actually achieved, so a reader can tell a
/// decisive raid from a wasted one.
///
/// Three cases, because a boolean collapsed two of them into a sentence that
/// was false either way (`codex review`, P2, on `Event::NodeStruck`'s own
/// doc): the strike can cross the operational threshold, leave a still-
/// working node still working, or land on something that was already rubble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrikeOutcome {
    /// The strike crossed `balance::NODE_OPERATIONAL_THRESHOLD` - this
    /// sortie is what took the node out.
    KnockedOut,
    /// The node absorbed the damage and is still working.
    StillOperational,
    /// The node was already below the threshold before this sortie. Nothing
    /// was achieved that had not already been achieved.
    AlreadyDown,
}

#[derive(Clone, Debug)]
pub enum Event {
    Battle {
        region: RegionId,
        factions: Vec<FactionId>,
        casualties: f32,
    },
    /// Stage 2D (docs/phase2-spec.md "3. 海戦"): the sea-domain counterpart
    /// of `Battle`, kept as its own variant rather than reusing `Battle`'s
    /// `region: RegionId` field, which a sea zone can't fill.
    NavalBattle {
        zone: SeaZoneId,
        factions: Vec<FactionId>,
        casualties: f32,
    },
    UnitDestroyed {
        unit: UnitId,
        station: Station,
        owner: FactionId,
    },
    RegionCaptured {
        region: RegionId,
        from: FactionId,
        to: FactionId,
    },
    FactionEliminated {
        faction: FactionId,
    },
    /// Stage 3A (docs/phase3-spec.md "政治イベント"): Labor support fell
    /// below `balance::STRIKE_THRESHOLD`, depressing industrial output for
    /// `balance::STRIKE_DAYS`.
    Strike {
        faction: FactionId,
    },
    /// Citizens support fell below `balance::PROTEST_THRESHOLD`: every
    /// owned region's unrest target is elevated for as long as this holds.
    Protest {
        faction: FactionId,
    },
    /// Military support fell below `balance::MUTINY_THRESHOLD`: unit
    /// organization recovery is reduced for as long as this holds.
    Mutiny {
        faction: FactionId,
    },
    /// Business support fell below `balance::CAPITAL_FLIGHT_THRESHOLD`:
    /// construction throughput and Machinery output are reduced for as long
    /// as this holds.
    CapitalFlight {
        faction: FactionId,
    },
    /// `stability` fell below `balance::REGIME_CHANGE_THRESHOLD`: policies
    /// reset to their scenario defaults, war support and every group's
    /// support reset to 50, and production is depressed for
    /// `balance::REGIME_CHANGE_DAYS`. Territory, units and stock are
    /// untouched (docs/phase3-spec.md "政権交代の扱い").
    RegimeChange {
        faction: FactionId,
    },
    /// `region`'s owner's LocalGovernment support fell below
    /// `balance::SEPARATISM_THRESHOLD` while the region sat occupied
    /// (`core != owner`) with no units present: it peacefully reverted from
    /// `from` back to its original `to` (== `region`'s `core`).
    Separatism {
        region: RegionId,
        from: FactionId,
        to: FactionId,
    },
    /// Stage 3B (docs/phase3-spec.md "Stage 3B — 外交関係と条約"):
    /// `Action::ProposeTreaty` queued a one-tick pending proposal.
    TreatyProposed {
        from: FactionId,
        to: FactionId,
        treaty: Treaty,
    },
    /// `Action::AcceptTreaty` resolved a pending proposal into an active
    /// treaty (a `Stance` change for `Ceasefire`/`NonAggression`/`Alliance`,
    /// a mutual grant for `MilitaryAccess`/`PortAccess`/`TradeAgreement`).
    TreatySigned {
        a: FactionId,
        b: FactionId,
        treaty: Treaty,
    },
    /// `Action::RejectTreaty` turned down a pending proposal.
    TreatyRejected {
        from: FactionId,
        to: FactionId,
        treaty: Treaty,
    },
    /// `Action::BreakTreaty` ended an active treaty - immediately for
    /// `Alliance`/`MilitaryAccess`/`PortAccess`/`TradeAgreement`, or (for
    /// `NonAggression`) the moment its notice period was served, not when
    /// the resulting war actually starts (see `Event::WarDeclared`).
    TreatyBroken {
        a: FactionId,
        b: FactionId,
        treaty: Treaty,
    },
    /// `a` and `b` are now at `Stance::War` - either `Action::DeclareWar`
    /// breaking a `Ceasefire` outright, or a `NonAggression` break's notice
    /// period running out.
    WarDeclared {
        a: FactionId,
        b: FactionId,
    },
    /// `faction`'s `Stance::Alliance` with one side of a fresh war pulled it
    /// into `into_war_with` too (docs/phase3-spec.md: "同盟国が攻撃されたら
    /// 自動参戦する").
    AllianceDragIn {
        faction: FactionId,
        into_war_with: FactionId,
    },
    /// Stage 4B (docs/phase4-spec.md "Stage 4B — 自然言語外交"):
    /// `Action::ProposeInNaturalLanguage` queued a one-tick natural-language
    /// proposal. `text` is carried for logging/newspaper purposes only -
    /// nothing in the simulation ever parses it (see `diplomacy.rs`'s
    /// `TreatyTerm` doc).
    NaturalLanguageProposed {
        from: FactionId,
        to: FactionId,
        text: String,
    },
    /// `Action::RespondToNaturalLanguageProposal` accepted the deal *and*
    /// every one of its interpreted `TreatyTerm`s validated - the deal
    /// actually took effect. `terms` is the recipient's own interpretation
    /// exactly as it was applied - carried here (Stage 7C,
    /// docs/phase7-spec.md "4. 外交画面": "解釈された TreatyTerm と可否を
    /// 表示する") purely so a display layer can show what was actually
    /// decided; nothing in this crate re-derives or guesses at it from
    /// anywhere else.
    NaturalLanguageAccepted {
        from: FactionId,
        to: FactionId,
        terms: Vec<TreatyTerm>,
    },
    /// `Action::RespondToNaturalLanguageProposal` turned the proposal down.
    /// `terms` is still the recipient's interpretation of the offer it
    /// rejected - see `NaturalLanguageAccepted`'s doc.
    NaturalLanguageRejected {
        from: FactionId,
        to: FactionId,
        terms: Vec<TreatyTerm>,
    },
    /// `Action::RespondToNaturalLanguageProposal` said "accept", but at
    /// least one interpreted `TreatyTerm` failed validation against the
    /// current board - the whole deal was discarded, nothing changed
    /// (docs/phase4-spec.md: "LLM が「受諾」と返しても...成立しない"). `terms`
    /// is what was attempted - see `NaturalLanguageAccepted`'s doc.
    NaturalLanguageTermsInvalid {
        from: FactionId,
        to: FactionId,
        terms: Vec<TreatyTerm>,
    },
    /// `Action::StrikeNode` (docs/phase10-spec.md "3. 阻止": "飛行場と港への
    /// 攻撃") actually landed - `action::apply_strike_node` has no `&mut
    /// Vec<Event>` of its own to write into (it runs inside `Simulation::
    /// apply`, not a tick system), so it queues this onto `World::
    /// action_log`, the same "action appliers push here" shape `Diplomacy::
    /// log`/`tick_diplomacy` already established, drained by `Simulation::
    /// step_timed` right alongside `Diplomacy::log`. Before this variant
    /// existed, a whole bombing campaign left nothing in the event log or
    /// newspaper at all - see this event's own defect writeup.
    /// `outcome` says what the sortie actually achieved. A bare boolean
    /// could not (`codex review`, P2): read as a *transition* it called a
    /// repeat raid on rubble "not knocked out", and every formatter renders
    /// that as "still operational" - the opposite of the truth; read as a
    /// *state* it claimed a fresh knockout on every wasted repeat. Three
    /// cases need three values.
    NodeStruck {
        attacker: FactionId,
        defender: FactionId,
        node: TransportNodeId,
        region: RegionId,
        node_kind: TransportNodeKind,
        outcome: StrikeOutcome,
    },
    /// `Action::InterdictLine`'s twin of `NodeStruck`, same reason it exists
    /// and the same `World::action_log` queuing path. `capacity_cut` is
    /// whether `line` actually had any `Condition` left to lose - `false`
    /// only when it was already fully severed (`Condition` at `0.0`) before
    /// this hit, so a reader can tell a raid that mattered from one thrown
    /// at an already-dead route.
    LineInterdicted {
        attacker: FactionId,
        defender: FactionId,
        line: TransportLineId,
        capacity_cut: bool,
    },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Battle {
                region,
                factions,
                casualties,
            } => {
                let ids: Vec<String> = factions.iter().map(|f| f.0.to_string()).collect();
                write!(
                    f,
                    "battle in region {} between factions [{}]: {:.2} manpower lost",
                    region.0,
                    ids.join(", "),
                    casualties
                )
            }
            Event::NavalBattle {
                zone,
                factions,
                casualties,
            } => {
                let ids: Vec<String> = factions.iter().map(|f| f.0.to_string()).collect();
                write!(
                    f,
                    "naval battle in sea zone {} between factions [{}]: {:.2} manpower lost",
                    zone.0,
                    ids.join(", "),
                    casualties
                )
            }
            Event::UnitDestroyed { unit, station, owner } => match station {
                Station::Region(region) => write!(
                    f,
                    "unit {} (faction {}) destroyed in region {}",
                    unit.0, owner.0, region.0
                ),
                Station::Sea(zone) => write!(
                    f,
                    "unit {} (faction {}) sunk in sea zone {}",
                    unit.0, owner.0, zone.0
                ),
                Station::Airfield(node) => write!(
                    f,
                    "unit {} (faction {}) destroyed at airfield node {}",
                    unit.0, owner.0, node.0
                ),
            },
            Event::RegionCaptured { region, from, to } => write!(
                f,
                "region {} captured by faction {} from faction {}",
                region.0, to.0, from.0
            ),
            Event::FactionEliminated { faction } => {
                write!(f, "faction {} eliminated", faction.0)
            }
            Event::Strike { faction } => {
                write!(f, "strike begins in faction {} (industrial output reduced)", faction.0)
            }
            Event::Protest { faction } => {
                write!(f, "protests begin in faction {} (unrest rising)", faction.0)
            }
            Event::Mutiny { faction } => write!(
                f,
                "military insubordination in faction {} (organization recovery reduced)",
                faction.0
            ),
            Event::CapitalFlight { faction } => write!(
                f,
                "capital flight in faction {} (construction and Machinery output reduced)",
                faction.0
            ),
            Event::RegimeChange { faction } => {
                write!(f, "regime change in faction {} (policies reset)", faction.0)
            }
            Event::Separatism { region, from, to } => write!(
                f,
                "region {} reverts from faction {} to faction {} via separatism",
                region.0, from.0, to.0
            ),
            Event::TreatyProposed { from, to, treaty } => write!(
                f,
                "faction {} proposes {} to faction {}",
                from.0,
                treaty.key(),
                to.0
            ),
            Event::TreatySigned { a, b, treaty } => write!(
                f,
                "faction {} and faction {} sign {}",
                a.0,
                b.0,
                treaty.key()
            ),
            Event::TreatyRejected { from, to, treaty } => write!(
                f,
                "faction {} rejects faction {}'s {} proposal",
                to.0,
                from.0,
                treaty.key()
            ),
            Event::TreatyBroken { a, b, treaty } => write!(
                f,
                "faction {} breaks {} with faction {}",
                a.0,
                treaty.key(),
                b.0
            ),
            Event::WarDeclared { a, b } => {
                write!(f, "faction {} declares war on faction {}", a.0, b.0)
            }
            Event::AllianceDragIn { faction, into_war_with } => write!(
                f,
                "faction {} is dragged into war with faction {} by an alliance",
                faction.0, into_war_with.0
            ),
            Event::NaturalLanguageProposed { from, to, text } => write!(
                f,
                "faction {} sends a natural-language proposal to faction {}: \"{}\"",
                from.0, to.0, text
            ),
            Event::NaturalLanguageAccepted { from, to, terms } => write!(
                f,
                "faction {} accepts faction {}'s natural-language proposal ({} term{})",
                to.0, from.0, terms.len(), if terms.len() == 1 { "" } else { "s" }
            ),
            Event::NaturalLanguageRejected { from, to, terms } => write!(
                f,
                "faction {} rejects faction {}'s natural-language proposal ({} term{})",
                to.0, from.0, terms.len(), if terms.len() == 1 { "" } else { "s" }
            ),
            Event::NaturalLanguageTermsInvalid { from, to, terms } => write!(
                f,
                "faction {} tried to accept faction {}'s natural-language proposal, but its terms no longer held ({} term{})",
                to.0, from.0, terms.len(), if terms.len() == 1 { "" } else { "s" }
            ),
            Event::NodeStruck { attacker, defender, node, region, node_kind, outcome } => write!(
                f,
                "faction {} strikes {} node {} in region {} (held by faction {}){}",
                attacker.0,
                node_kind.key(),
                node.0,
                region.0,
                defender.0,
                match outcome {
                    StrikeOutcome::KnockedOut => ", knocking it out of operation",
                    StrikeOutcome::StillOperational => ", but it stays operational",
                    StrikeOutcome::AlreadyDown => ", which was already out of operation",
                }
            ),
            Event::LineInterdicted { attacker, defender, line, capacity_cut } => write!(
                f,
                "faction {} interdicts transport line {} (held by faction {}){}",
                attacker.0,
                line.0,
                defender.0,
                if *capacity_cut { ", cutting its capacity" } else { ", but it was already severed" }
            ),
        }
    }
}
