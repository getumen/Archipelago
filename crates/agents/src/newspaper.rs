//! Stage 4C (docs/phase4-spec.md "Stage 4C — 新聞・報道生成", design.md
//! §13), rebuilt per docs/newspaper-spec.md: periodic newspaper generation
//! that reports *what happened to the faction's country*, not an
//! enumeration of the tick engine's `Event` stream.
//!
//! ## The three load-bearing decisions (docs/newspaper-spec.md)
//!
//! 1. **State deltas drive the issue; events are evidence.** `template_summary`
//!    diffs `Faction`/`Region`/transport-network state between the period's
//!    `start: &World` and its `world: &World` end, and leads with whatever
//!    actually moved (`political_mover`, `stability_mover`, `territory_mover`,
//!    `transport_mover`, `stockpile_mover`, `import_mover`, `force_mover`, ...) -
//!    covering every row of docs/newspaper-spec.md §1's state table, including
//!    per-commodity stockpiles, import volume, and per-branch unit counts.
//!    Same-kind events (a fortnight of near-identical air raids on the same
//!    route) are tallied into one clause (`tally_strike`), never expanded one
//!    line per occurrence - see `fifteen_air_raids_do_not_become_fifteen_lines`.
//! 2. **The government's framing is part of the output.** `government_tone`
//!    reads the faction's *current* stability/war support/crisis count and
//!    picks a closing editorial clause - confident when things are fine,
//!    hollow reassurance when they are not. See `tone_differs_between_stable_and_collapsing_faction`.
//!    Its room in the article is reserved before the body is ever truncated
//!    to `MAX_ARTICLE_CHARS`, so a busy, eventful issue can never cut the
//!    tone off partway - see `government_tone_survives_a_maximally_eventful_issue`.
//! 3. **The template is primary; the LLM is a rewriting layer on top of it.**
//!    `generate_article` always builds `template_summary` first and, when a
//!    real backend is configured, asks it only to *rewrite* that already-
//!    complete article more naturally - never to reconstruct one from raw
//!    events again. A backend that fails or answers empty falls straight
//!    back to the template text untouched (`newspaper_falls_back_to_template`),
//!    which is why this template alone has to be worth reading.
//!
//! ## The boundary this module exists to enforce
//!
//! Everything here only ever *reads* `&World`/`&[Event]` and produces
//! `String`s. No function in this module takes a `&mut World`, a `&mut
//! Simulation`, or anything else that could feed generated prose back into
//! the simulation - see `newspaper_does_not_affect_simulation` for the
//! guarantee that toggling newspaper generation on or off cannot change a
//! single value in the game.
//!
//! Generation failure (`LlmBackend::complete` returning `Err`, or an `Ok`
//! response that's empty/whitespace-only) falls back to the mechanical
//! template built straight from `events`/`start`/`world`
//! (`template_summary`) - see `newspaper_falls_back_to_template`.

use archipelago_sim::balance::{
    CAPITAL_FLIGHT_THRESHOLD, MUTINY_THRESHOLD, NODE_OPERATIONAL_THRESHOLD, PROTEST_THRESHOLD, REGIME_CHANGE_THRESHOLD,
    STRIKE_THRESHOLD,
};
use archipelago_sim::diplomacy::Treaty;
use archipelago_sim::event::{Event, StrikeOutcome};
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::group::{Group, ALL_GROUPS};
use archipelago_sim::ids::FactionId;
use archipelago_sim::military::{Branch, ALL_BRANCHES};
use archipelago_sim::naval;
use archipelago_sim::world::{Faction, World};

use crate::llm::{truncate_chars, LlmBackend, LlmRequest};

/// How often (in simulated days) an issue is generated, mirroring
/// `llm::LLM_CONSULT_INTERVAL_DAYS`'s "not every tick" reasoning - a
/// newspaper reporting on a single day's events would be mostly empty, and
/// nothing about prose generation needs tick-level freshness.
pub const NEWSPAPER_INTERVAL_DAYS: u32 = 30;

/// A response longer than this many *characters* is truncated
/// (`truncate_chars`, shared with `llm::parse_doctrine`'s `rationale`
/// bound) - purely a memory/display bound.
const MAX_ARTICLE_CHARS: usize = 900;

/// One faction's own article for one reporting period. `from_backend` is
/// `false` exactly when this is the mechanical template fallback rather
/// than backend-rewritten prose (`newspaper_falls_back_to_template`).
#[derive(Clone, Debug, PartialEq)]
pub struct NewspaperArticle {
    pub faction: FactionId,
    pub period_start: u32,
    pub period_end: u32,
    pub text: String,
    pub from_backend: bool,
}

/// System prompt for the LLM *rewriting* layer (docs/newspaper-spec.md §3:
/// "テンプレートを主とする...LLM はさらに自然な文章にする層として上に乗る").
/// Deliberately not asked to invent anything: `template_summary` has already
/// decided what happened and how the government frames it; the backend's
/// only job is prose, never new content.
const SYSTEM_PROMPT: &str = "You are a copy editor for one faction's home newspaper in a grand-strategy war simulation. \
You will be given a mechanically-written but fact-accurate Japanese article: what moved in the nation this period, \
in order of importance, followed by the government's own editorial framing. Rewrite it as natural, flowing Japanese \
prose in 3-6 short sentences, keeping every fact, every number, and the existing per-faction slant and government \
tone exactly as given - do not add, remove, or soften any fact, and do not invent anything the source text does not \
say. Output nothing but the rewritten article text itself - no JSON, no headline markup, no preamble.";

/// Japanese label for a `Treaty`, for the fallback template - mirrors
/// `apps/headless/src/report.rs`'s `treaty_label` (that one lives on the
/// binary side and isn't reachable from this crate, so this is a small,
/// deliberate duplicate rather than a new inter-crate dependency).
fn treaty_label(treaty: Treaty) -> &'static str {
    match treaty {
        Treaty::Ceasefire => "停戦",
        Treaty::NonAggression => "不可侵条約",
        Treaty::Alliance => "同盟",
        Treaty::MilitaryAccess => "通行権",
        Treaty::PortAccess => "港湾利用権",
        Treaty::TradeAgreement => "通商協定",
    }
}

/// One state-delta clause competing for a place in the article, ranked by
/// `magnitude` against every other clause (docs/newspaper-spec.md §1:
/// "期間の始点と終点の差を取り、大きく動いたものから語る"). `magnitude`'s
/// scale is deliberately mixed - support points, region counts, casualty
/// figures - since nothing outside `template_summary`'s own sort ever reads
/// it; it only has to rank this article's own clauses against each other.
struct Mover {
    magnitude: f32,
    text: String,
}

/// A clause is worth printing at all only once its magnitude clears this
/// floor - otherwise a fraction of a support point jittering tick to tick
/// would pad every single issue with clauses that say nothing. This is a
/// display-legibility constant local to prose generation, not a simulation
/// balance constant (docs/newspaper-spec.md "共通の制約": don't tune those;
/// this tunes nothing the simulation itself ever reads).
const MOVER_MAGNITUDE_FLOOR: f32 = 6.0;

/// Added to a political mover's magnitude when the group sits in crisis
/// territory (below its own `balance.rs` threshold) at the period's end -
/// large enough that a chronic crisis a period's events never re-triggered
/// (the actual defect docs/newspaper-spec.md §0 reports: support already
/// below threshold, `protest_active` already `true`, so no fresh `Event`
/// fires) still leads the issue rather than being outweighed by an
/// unrelated but larger-magnitude swing elsewhere.
const CRISIS_MOVER_BOOST: f32 = 60.0;

/// This `Group`'s own crisis threshold from `balance.rs`, if it has one -
/// `politics::tick_politics`' own `Strike`/`Protest`/`Mutiny`/`CapitalFlight`
/// trigger conditions, read here rather than re-declared, so "in crisis" in
/// the newspaper can never disagree with "in crisis" in the simulation.
/// `Government`/`LocalGovernment`/`Bureaucracy` have no such threshold - a
/// low-influence swing there is reported only if its raw delta clears
/// `MOVER_MAGNITUDE_FLOOR`.
fn group_crisis_threshold(group: Group) -> Option<f32> {
    match group {
        Group::Labor => Some(STRIKE_THRESHOLD),
        Group::Citizens => Some(PROTEST_THRESHOLD),
        Group::Military => Some(MUTINY_THRESHOLD),
        Group::Business => Some(CAPITAL_FLIGHT_THRESHOLD),
        Group::Government | Group::LocalGovernment | Group::Bureaucracy => None,
    }
}

/// One political group's mover: `None` when its support neither moved
/// enough to mention nor sits in crisis - `Some` otherwise, phrased as a
/// fall/rise/persistent-crisis clause depending on where `start`/`end` sit
/// relative to `group_crisis_threshold`.
fn political_mover(group: Group, start: &Faction, end: &Faction) -> Option<Mover> {
    let i = group.index();
    let (s, e) = (start.group_support[i], end.group_support[i]);
    let delta = e - s;
    let threshold = group_crisis_threshold(group);
    let in_crisis = threshold.is_some_and(|t| e < t);

    let mut magnitude = delta.abs();
    if in_crisis {
        magnitude += CRISIS_MOVER_BOOST;
    }
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }

    let label = group.label();
    let text = if in_crisis && threshold.is_some_and(|t| s < t) {
        // Already below threshold at the period's start too - a *standing*
        // crisis, exactly the case that used to vanish from the newspaper
        // once its triggering `Event` had already fired in an earlier issue.
        format!("{label}の支持は{e:.0}/100と危機的水準にとどまったままで、この状態が続いている。")
    } else if in_crisis {
        format!("{label}の支持はこの期間で{s:.0}から{e:.0}へ急落し、危機的水準まで悪化した。")
    } else if delta >= 0.0 {
        format!("{label}の支持は{s:.0}から{e:.0}へ上向いた。")
    } else {
        format!("{label}の支持は{s:.0}から{e:.0}へ落ち込んだ。")
    };
    Some(Mover { magnitude, text })
}

/// `stability`'s own mover - weighted a little above a same-sized political
/// swing since it's the figure `RegimeChange` itself watches
/// (`REGIME_CHANGE_THRESHOLD`).
fn stability_mover(start: &Faction, end: &Faction) -> Option<Mover> {
    let (s, e) = (start.stability, end.stability);
    let delta = e - s;
    let in_crisis = e < REGIME_CHANGE_THRESHOLD;
    let mut magnitude = delta.abs() * 1.2;
    if in_crisis {
        magnitude += CRISIS_MOVER_BOOST;
    }
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }
    let text = if in_crisis && s < REGIME_CHANGE_THRESHOLD {
        format!("政権の安定度は{e:.0}/100のまま危機的な低水準が続いている。")
    } else if in_crisis {
        format!("政権の安定度は{s:.0}から{e:.0}へ急落し、体制の存立が危うい水準に達した。")
    } else if delta >= 0.0 {
        format!("政権の安定度は{s:.0}から{e:.0}へ改善した。")
    } else {
        format!("政権の安定度は{s:.0}から{e:.0}へ低下した。")
    };
    Some(Mover { magnitude, text })
}

/// `war_support`'s own mover - no `balance.rs` crisis threshold names it
/// directly, so unlike the four group movers this is delta-only.
fn war_support_mover(start: &Faction, end: &Faction) -> Option<Mover> {
    let (s, e) = (start.war_support, end.war_support);
    let delta = e - s;
    if delta.abs() < MOVER_MAGNITUDE_FLOOR {
        return None;
    }
    let text = if delta >= 0.0 {
        format!("厭戦感は和らぎ、戦争支持は{s:.0}から{e:.0}へ上昇した。")
    } else {
        format!("厭戦感が強まり、戦争支持は{s:.0}から{e:.0}へ低下した。")
    };
    Some(Mover { magnitude: delta.abs(), text })
}

/// Civilian shortage/ration mover, folding both `shortage` and
/// `civilian_ration` into one clause since they describe the same lived
/// experience (queues and cuts) from opposite ends - a government that cuts
/// rations *because* of shortage shouldn't read as two unrelated facts.
fn shortage_mover(start: &Faction, end: &Faction) -> Option<Mover> {
    let shortage_delta = (end.shortage - start.shortage) * 100.0;
    let ration_delta = (end.civilian_ration - start.civilian_ration) * 100.0;
    let magnitude = shortage_delta.abs().max(ration_delta.abs()).max(if end.shortage > 0.2 { 40.0 } else { 0.0 });
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }

    // Which of Food/Energy/Machinery is actually short right now
    // (`Faction::shortage_by_good`'s own doc: only these three indices are
    // ever nonzero) - names the commodity instead of leaving "不足" abstract.
    let worst_good = [Good::Food, Good::Energy, Good::Machinery]
        .into_iter()
        .max_by(|a, b| end.shortage_by_good[a.index()].partial_cmp(&end.shortage_by_good[b.index()]).unwrap());
    let good_note = match worst_good {
        Some(g) if end.shortage_by_good[g.index()] > 0.05 => format!("特に{}の不足が深刻で、", g.label()),
        _ => String::new(),
    };

    let ration_note = if ration_delta < -MOVER_MAGNITUDE_FLOOR {
        format!("政府は配給を{:.0}%から{:.0}%へ切り詰めた。", start.civilian_ration * 100.0, end.civilian_ration * 100.0)
    } else if ration_delta > MOVER_MAGNITUDE_FLOOR {
        format!("配給は{:.0}%から{:.0}%へ回復した。", start.civilian_ration * 100.0, end.civilian_ration * 100.0)
    } else {
        String::new()
    };

    let text = if end.shortage > start.shortage {
        format!("{good_note}国内の物資不足は{:.0}%から{:.0}%へ悪化した。{ration_note}", start.shortage * 100.0, end.shortage * 100.0)
    } else if end.shortage < start.shortage && start.shortage - end.shortage > 0.05 {
        format!("物資不足は{:.0}%から{:.0}%へ和らいだ。{ration_note}", start.shortage * 100.0, end.shortage * 100.0)
    } else if end.shortage > 0.05 {
        format!("{good_note}物資不足は{:.0}%の水準で高止まりしている。{ration_note}", end.shortage * 100.0)
    } else {
        // `codex review` (P2): rationing alone can carry this mover past its
        // floor while shortage sits at or near zero, and the old catch-all
        // then printed 「物資不足は0%の水準で高止まりしている」 - the paper
        // stating something untrue, which is the one thing
        // `docs/newspaper-spec.md` cannot tolerate. Shortage and rationing
        // are separate economic facts there (§1's table lists them apart),
        // so with nothing to say about shortage this says nothing about it.
        format!("{good_note}{ration_note}")
    };
    Some(Mover { magnitude, text })
}

/// Manpower lost this period (`Faction::casualties`' own doc: a running,
/// never-reset total, so the *period's* toll is the two snapshots' delta,
/// not the field itself) plus the supply ratio delivered to the front.
fn military_mover(start: &Faction, end: &Faction) -> Option<Mover> {
    let casualties = (end.casualties - start.casualties).max(0.0);
    let supply_delta = (end.supply_ratio - start.supply_ratio) * 100.0;
    let magnitude = casualties * 3.0 + supply_delta.abs();
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }
    let mut text = if casualties > 0.05 {
        format!("この期間の戦死者は{casualties:.1}万人に上った。")
    } else {
        String::new()
    };
    if supply_delta.abs() >= MOVER_MAGNITUDE_FLOOR {
        if supply_delta < 0.0 {
            text.push_str(&format!(
                "前線への補給率は{:.0}%から{:.0}%へ低下している。",
                start.supply_ratio * 100.0,
                end.supply_ratio * 100.0,
            ));
        } else {
            text.push_str(&format!(
                "前線への補給率は{:.0}%から{:.0}%へ改善した。",
                start.supply_ratio * 100.0,
                end.supply_ratio * 100.0,
            ));
        }
    }
    if text.is_empty() {
        return None;
    }
    Some(Mover { magnitude, text })
}

/// Per-commodity stockpile mover (docs/newspaper-spec.md §1 "経済":
/// "品目ごとの在庫と不足") - distinct from `shortage_mover`'s civilian
/// shortage/ration reading: a stockpile can swing hugely (a munitions
/// buildup staged ahead of an offensive, a steel stockpile drawn down by a
/// construction spree) without ever touching civilian shortage at all, so
/// without this reading such a swing would never surface in any mover.
/// Reports whichever single commodity moved the most, as a percentage of
/// whichever of its start/end level is larger - goods that start at very
/// different absolute scales (Munitions at 400, Armour at 60,
/// `scenario::FACTION_STOCK`) are then compared on the same footing, and
/// the denominator can never collapse to zero even for a commodity that
/// starts or ends completely depleted.
fn stockpile_mover(start: &Faction, end: &Faction) -> Option<Mover> {
    let mut worst: Option<(Good, f32, f32, f32)> = None; // (good, pct_delta, s, e)
    for good in ALL_GOODS {
        let i = good.index();
        let (s, e) = (start.stock[i], end.stock[i]);
        let denom = s.max(e).max(1.0);
        let pct = (e - s) / denom * 100.0;
        if worst.is_none_or(|(_, best, _, _)| pct.abs() > best.abs()) {
            worst = Some((good, pct, s, e));
        }
    }
    let (good, pct, s, e) = worst?;
    let magnitude = pct.abs();
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }
    let text = if pct >= 0.0 {
        format!("{}の備蓄は{s:.0}から{e:.0}へ積み増された。", good.label())
    } else {
        format!("{}の備蓄は{s:.0}から{e:.0}へ取り崩された。", good.label())
    };
    Some(Mover { magnitude, text })
}

/// Total sea-import volume actually landed this period, summed over every
/// port this faction currently owns (`Region::import_flow`'s own doc: real
/// tonnage delivered *today*, recomputed every tick - not the requested
/// `Faction::import_plan` rate, which can sit unfulfilled under a blockade
/// `blockaded_ports` already reports separately).
fn total_import_flow(world: &World, faction: FactionId) -> f32 {
    world.regions.iter().filter(|r| r.owner == faction).map(|r| r.import_flow).sum()
}

/// Import-volume mover (docs/newspaper-spec.md §1 "経済": "輸入量") - a
/// faction that ramps imports up or lets them collapse (a new trade
/// agreement, a blockade tightening) moves neither `shortage_mover`'s
/// civilian ration nor any other existing mover on its own.
fn import_mover(start_flow: f32, end_flow: f32) -> Option<Mover> {
    let delta = end_flow - start_flow;
    if delta.abs() < MOVER_MAGNITUDE_FLOOR {
        return None;
    }
    let text = if delta >= 0.0 {
        format!("海上輸入量は{start_flow:.0}から{end_flow:.0}へ増加した。")
    } else {
        format!("海上輸入量は{start_flow:.0}から{end_flow:.0}へ減少した。")
    };
    Some(Mover { magnitude: delta.abs(), text })
}

/// Number of this faction's own living land units in each `Branch`, in
/// `ALL_BRANCHES` order - docs/newspaper-spec.md §1 "軍事": "部隊数（兵科
/// 別）", read directly off `World::units` rather than reconstructed from
/// `Event::Battle`/recruit events.
fn branch_unit_counts(world: &World, faction: FactionId) -> [usize; 3] {
    ALL_BRANCHES.map(|b| world.units.iter().filter(|u| u.owner == faction && u.alive && u.branch == Some(b)).count())
}

/// Force build-up/drawdown mover: how each branch's living unit count moved
/// over the period. Distinct from `military_mover`'s casualty/supply-ratio
/// reading - recruiting a wave of new units ahead of an offensive, or
/// disbanding idle ones, changes neither casualties nor delivered supply,
/// so without this reading such a build-up would never surface in any
/// mover. A change of a single unit is not itself worth a clause (routine
/// replacement-rate noise); only branches that moved by 2 or more units are
/// named.
fn force_mover(start: [usize; 3], end: [usize; 3]) -> Option<Mover> {
    let mut clauses: Vec<(f32, String)> = Vec::new();
    for (i, branch) in ALL_BRANCHES.into_iter().enumerate() {
        let (s, e) = (start[i] as f32, end[i] as f32);
        let delta = e - s;
        if delta.abs() < 2.0 {
            continue;
        }
        let verb = if delta > 0.0 { "増強" } else { "縮小" };
        clauses.push((delta.abs(), format!("{}部隊は{s:.0}個から{e:.0}個へ{verb}された。", branch.label())));
    }
    if clauses.is_empty() {
        return None;
    }
    let magnitude: f32 = clauses.iter().map(|(m, _)| *m).sum::<f32>() * 4.0;
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }
    clauses.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("unit-count deltas are always finite"));
    let text = clauses.into_iter().map(|(_, t)| t).collect::<String>();
    Some(Mover { magnitude, text })
}

/// Territory mover: net region-count change plus which named regions moved
/// (`captured`/`lost`, already tallied by `template_summary` from `events`)
/// and how the faction's own war-torn state (`Region::devastation`,
/// averaged over regions it holds) moved alongside it.
fn territory_mover(
    start_count: usize,
    end_count: usize,
    captured: &[String],
    lost: &[String],
    start_devastation: f32,
    end_devastation: f32,
) -> Option<Mover> {
    let region_delta = end_count as f32 - start_count as f32;
    let devastation_delta = (end_devastation - start_devastation) * 100.0;
    // Each region changing hands is treated as a large, front-page-worthy
    // swing - deliberately weighted well above a same-sized percentage swing
    // elsewhere, since gaining or losing a whole region is a bigger fact
    // about the country than a few points of any single ratio.
    //
    // Weighted on `captured.len() + lost.len()` - the *gross* count of named
    // region-level events - not `region_delta`, which is only their *net*.
    // A faction that captured three regions and lost three elsewhere nets to
    // zero, so a net-only magnitude would silently drop a front that moved
    // in both directions as "quiet", even though `captured` and `lost` are
    // both populated and the text below is about to name every one of them.
    // See `net_zero_territory_exchange_is_not_quiet`.
    //
    // `region_delta` is added too (`codex review`, P2): a region handed over
    // by `TreatyTerm::Cede`/`Withdraw` moves through
    // `diplomacy::transfer_region`, which emits no `RegionCaptured`, so both
    // event lists are empty while the count really changed. Without this term
    // the mover returned `None` before the region-count fallback text below
    // could ever run, and a negotiated territorial change vanished from the
    // paper. Gross events still dominate so an even exchange stays loud.
    let magnitude = (captured.len() + lost.len()) as f32 * 20.0 + region_delta.abs() * 20.0 + devastation_delta.abs();
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }

    let mut text = String::new();
    if !captured.is_empty() {
        text.push_str(&format!("{}を制圧し、戦線を前進させた。", join_capped(captured, "地域")));
    }
    if !lost.is_empty() {
        text.push_str(&format!("{}からは戦略的転進を行った。", join_capped(lost, "地域")));
    }
    if text.is_empty() && region_delta != 0.0 {
        text.push_str(&format!("保有地域は{}地域から{}地域へ変わった。", start_count, end_count));
    }
    if devastation_delta.abs() >= MOVER_MAGNITUDE_FLOOR {
        if devastation_delta > 0.0 {
            text.push_str(&format!(
                "国土の戦災は平均{:.0}%から{:.0}%へ広がった。",
                start_devastation * 100.0,
                end_devastation * 100.0,
            ));
        } else {
            text.push_str(&format!(
                "国土の戦災は平均{:.0}%から{:.0}%へ和らいだ。",
                start_devastation * 100.0,
                end_devastation * 100.0,
            ));
        }
    }
    if text.is_empty() {
        return None;
    }
    Some(Mover { magnitude, text })
}

/// Transport-network mover: how many of the faction's own nodes/lines sit
/// below `NODE_OPERATIONAL_THRESHOLD` now versus at the period's start,
/// with the strikes tallied from `events` (`tally_strike`) cited as the
/// evidence for *why* - never one clause per strike (docs/newspaper-spec.md
/// "出来事は根拠": "15 回の空襲を受けた、であって 15 行ではない").
fn transport_mover(
    start_down: usize,
    end_down: usize,
    own_node_strikes: &[(String, u32, u32)],
    own_line_strikes: &[(String, u32, u32)],
    node_strikes_at_home: &[(String, u32, u32)],
    line_strikes_at_home: &[(String, u32, u32)],
) -> Option<Mover> {
    let delta = end_down as f32 - start_down as f32;
    let strike_count: u32 = own_node_strikes.iter().chain(own_line_strikes).map(|(_, n, _)| n).sum::<u32>()
        + node_strikes_at_home.iter().chain(line_strikes_at_home).map(|(_, n, _)| n).sum::<u32>();
    // A strike that actually achieves something (a knockout/capacity cut,
    // not just an attempt) is weighted heavily enough on its own that even
    // a single such strike clears `MOVER_MAGNITUDE_FLOOR` -
    // `node_strikes_are_not_silently_dropped` pins exactly this: a lone
    // knocked-out port must never be silently dropped just because it's not
    // part of a large campaign.
    let achieved_count: u32 = own_node_strikes.iter().chain(own_line_strikes).map(|(_, _, a)| a).sum::<u32>()
        + node_strikes_at_home.iter().chain(line_strikes_at_home).map(|(_, _, a)| a).sum::<u32>();
    // `strike_count` itself carries the same per-occurrence weight
    // (`* 6.0`) as `achieved_count`, not `* 1.0` - an *unsuccessful* raid is
    // still a real attack the text below reports (`format_strike_tally`
    // names it regardless of outcome), and fewer than six of them used to
    // sit under `MOVER_MAGNITUDE_FLOOR` on their own, reading a real air
    // campaign that happened to fail as a quiet period. See
    // `failed_strikes_are_not_a_quiet_period`.
    let magnitude = delta.abs() * 10.0 + strike_count as f32 * 6.0 + achieved_count as f32 * 6.0;
    if magnitude < MOVER_MAGNITUDE_FLOOR {
        return None;
    }

    let mut text = String::new();
    if !node_strikes_at_home.is_empty() || !line_strikes_at_home.is_empty() {
        text.push_str(&format!("{}を受けた。", format_strike_tally(node_strikes_at_home, line_strikes_at_home, "空爆・妨害")));
    }
    if end_down > 0 {
        text.push_str(&format!("輸送網は{end_down}拠点/路線が機能を停止したままとなっている。"));
    } else if start_down > 0 {
        // `codex review` (P2): a period whose whole story is that the network
        // came back had no branch at all - `magnitude` cleared the floor on
        // the delta, every text branch stayed silent, and the mover returned
        // `None`, so a real recovery could be reported as a quiet period.
        // Recovery is a state change the spec's endpoint diff is meant to
        // catch, exactly like the damage that preceded it.
        text.push_str(&format!("停止していた{start_down}拠点/路線が復旧した。"));
    }
    if !own_node_strikes.is_empty() || !own_line_strikes.is_empty() {
        text.push_str(&format!("一方、{}を実施した。", format_strike_tally(own_node_strikes, own_line_strikes, "空爆・妨害")));
    }
    if text.is_empty() {
        return None;
    }
    Some(Mover { magnitude, text })
}

/// Renders a strike tally (`(counterpart faction name, hits, achieved)`
/// triples for nodes then for lines) into one clause fragment - "計 N 回の
/// 空爆・妨害を受け、うち M 回は目標の機能を停止させた" - regardless of
/// whether that's 1 raid or 15, exactly the aggregation
/// `fifteen_air_raids_do_not_become_fifteen_lines` pins.
fn format_strike_tally(nodes: &[(String, u32, u32)], lines: &[(String, u32, u32)], verb: &str) -> String {
    let hits: u32 = nodes.iter().chain(lines).map(|(_, n, _)| n).sum();
    let achieved: u32 = nodes.iter().chain(lines).map(|(_, _, a)| a).sum();
    if achieved > 0 {
        format!("計{hits}回の{verb}（うち{achieved}回は機能を停止させた）")
    } else {
        format!("計{hits}回の{verb}")
    }
}

/// A list of more than this many distinctly-named facts (regions captured
/// in one period, ports under blockade, ...) is joined as its first
/// `NAMED_LIST_CAP` names plus a trailing count, rather than every name -
/// `join_capped`'s own doc has the readability rationale. A whole front
/// collapsing in one `japan_hex` period (8 factions, dozens of regions)
/// otherwise produces a single comma-joined clause naming a dozen-plus
/// regions, which reads as noise long before a human finishes it - the same
/// "article length must not scale with event count" principle
/// `tally_strike`/`tally_battle` already apply to *repeated* events, applied
/// here to a long list of *distinct* named ones.
const NAMED_LIST_CAP: usize = 4;

/// Joins `items` with "、", capping how many are actually named
/// (`NAMED_LIST_CAP`) and summarizing the rest as "ほかN{unit}" - see
/// `NAMED_LIST_CAP`'s own doc for why. `unit` is the counter word for
/// whatever's left out ("地域"/"港"/"件"/...).
fn join_capped(items: &[String], unit: &str) -> String {
    if items.len() <= NAMED_LIST_CAP {
        items.join("、")
    } else {
        format!("{}ほか{}{unit}", items[..NAMED_LIST_CAP].join("、"), items.len() - NAMED_LIST_CAP)
    }
}

/// Adds one occurrence to a strike tally, merging into an existing entry for
/// `counterpart` if one already exists this period - same linear-`Vec`-scan
/// idiom `tally_battle` already uses (never more than one entry per living
/// rival faction, so a `HashMap` would be pure overhead).
fn tally_strike(tally: &mut Vec<(String, u32, u32)>, counterpart: String, achieved: bool) {
    if let Some(entry) = tally.iter_mut().find(|(name, _, _)| *name == counterpart) {
        entry.1 += 1;
        if achieved {
            entry.2 += 1;
        }
    } else {
        tally.push((counterpart, 1, achieved as u32));
    }
}

/// Adds one `Battle`/`NavalBattle` occurrence to `tally`'s running per-
/// location count, casualty total, and per-branch casualty breakdown
/// (`branch_casualties` - empty for a naval engagement), appending a fresh
/// entry if `label` hasn't been seen yet in this period. Same linear-scan
/// merge for the per-branch breakdown - never more than 3 entries
/// (`military::ALL_BRANCHES`), so a nested `HashMap` would be pure overhead.
fn tally_battle(
    tally: &mut Vec<(String, u32, f32, Vec<(Branch, f32)>)>,
    label: String,
    casualties: f32,
    branch_casualties: &[(Branch, f32)],
) {
    if let Some(entry) = tally.iter_mut().find(|(l, _, _, _)| *l == label) {
        entry.1 += 1;
        entry.2 += casualties;
        for &(branch, lost) in branch_casualties {
            match entry.3.iter_mut().find(|(b, _)| *b == branch) {
                Some(e) => e.1 += lost,
                None => entry.3.push((branch, lost)),
            }
        }
    } else {
        tally.push((label, 1, casualties, branch_casualties.to_vec()));
    }
}

/// Number of this faction's own transport nodes/lines currently below
/// `NODE_OPERATIONAL_THRESHOLD` - the "輸送: 路線の状態、ノードの稼働" line
/// docs/newspaper-spec.md's state table asks for, read directly off
/// existing transport-layer state rather than reconstructed from events.
fn down_transport_count(world: &World, faction: FactionId) -> usize {
    let node_owner = |node: archipelago_sim::ids::TransportNodeId| world.transport_node(node).region;
    let owns_node = |node: archipelago_sim::ids::TransportNodeId| world.region(node_owner(node)).owner == faction;

    let down_nodes = world.transport_nodes.iter().filter(|n| world.region(n.region).owner == faction && !n.operational()).count();
    let down_lines = world
        .transport_lines
        .iter()
        .filter(|l| (owns_node(l.from) || owns_node(l.to)) && l.condition.get() <= NODE_OPERATIONAL_THRESHOLD)
        .count();
    down_nodes + down_lines
}

/// Average `Region::devastation` over the regions this faction currently
/// owns - `0.0` (not an error) for a faction holding none, matching
/// `military_mover`'s/`shortage_mover`'s own "nothing to report" shape.
fn avg_devastation(world: &World, faction: FactionId) -> f32 {
    let regions: Vec<f32> = world.regions.iter().filter(|r| r.owner == faction).map(|r| r.devastation).collect();
    if regions.is_empty() {
        0.0
    } else {
        regions.iter().sum::<f32>() / regions.len() as f32
    }
}

/// The government's own framing of this issue (docs/newspaper-spec.md §2:
/// "論調は安定度と支持から決まる。苦しいときほど楽観的な発表になる") - reads
/// only `end`'s *current* stability/war support/crisis count, so a faction
/// that just climbed out of crisis still gets read the way it currently
/// stands, not the way it stood a month ago.
///
/// `war_support` carries most of the discriminating power here, not
/// `stability`: measured against real playthroughs (mvp seed 1, japan_hex
/// seed 2, both `--report 30` faction tables read by hand), a faction
/// actually at war spends nearly the entire game with `stability` pinned in
/// a narrow ~50-55 band - `REGIME_CHANGE_THRESHOLD` (40) below it is a real
/// existential line but is crossed only when a faction is *already*
/// collapsing, so gating the everyday bands on `stability < 55.0` (an
/// earlier version of this function did) made every issue for the entire
/// game read with the same "限定的" spin, regardless of how the war was
/// actually going - confirmed by hand: a full 300-day japan_hex run
/// produced the identical closing sentence in 80 of 80 articles. `war_support`
/// in the same data actually spans its whole `0..100` range faction to
/// faction and month to month, so the bands below are built on it instead
/// (CLAUDE.md「検証についての教訓」: measure the real distribution before
/// tuning a threshold, don't guess one that merely looks reasonable).
fn government_tone(end: &Faction) -> &'static str {
    let crisis_count = ALL_GROUPS.iter().filter(|&&g| group_crisis_threshold(g).is_some_and(|t| end.group_support[g.index()] < t)).count();
    let regime_crisis = end.stability < REGIME_CHANGE_THRESHOLD;

    if regime_crisis || crisis_count >= 2 || end.war_support < 20.0 {
        "政府は「事態は完全に掌握している」と発表しているが、国内の実情はそれとかけ離れている。"
    } else if crisis_count == 1 || end.war_support < 45.0 {
        "政府は被害を限定的なものと発表し、動揺の沈静化を強調している。"
    } else if end.war_support >= 70.0 {
        "政府はこの期間の成果を国民に誇らしげに公表している。"
    } else {
        "政府はおおむね平静な調子でこの期間の状況を発表している。"
    }
}

/// The mechanical, backend-free template (docs/newspaper-spec.md §3: "テン
/// プレートだけで「読める新聞」が成立すること") - and, since a run with no
/// LLM backend configured always falls through to it
/// (`generate_article`'s doc), the newspaper's ordinary output, not just its
/// failure path. `start`/`world` are the same faction's board at the
/// period's first and last day; diffing them is what drives the body
/// (docs/newspaper-spec.md §1), with `events` supplying the specific named
/// evidence (which region, which route, how many raids) for whichever
/// deltas actually moved.
fn template_summary(start: &World, world: &World, faction: FactionId, events: &[Event]) -> String {
    let sf = start.faction(faction);
    let f = world.faction(faction);

    let mut own_gained: Vec<String> = Vec::new();
    let mut own_lost: Vec<String> = Vec::new();
    let mut own_battles: Vec<(String, u32, f32, Vec<(Branch, f32)>)> = Vec::new();
    let mut own_treaties: Vec<String> = Vec::new();
    let mut own_wars: Vec<String> = Vec::new();
    let mut own_node_strikes: Vec<(String, u32, u32)> = Vec::new();
    let mut own_line_strikes: Vec<(String, u32, u32)> = Vec::new();
    let mut node_strikes_at_home: Vec<(String, u32, u32)> = Vec::new();
    let mut line_strikes_at_home: Vec<(String, u32, u32)> = Vec::new();
    let mut rival_moves: Vec<String> = Vec::new();
    let mut rival_turmoil: Vec<String> = Vec::new();
    let mut rival_air_ops: u32 = 0;

    for event in events {
        match event {
            Event::RegionCaptured { region, from, to } => {
                let region_name = &world.region(*region).name;
                if *to == faction {
                    own_gained.push(region_name.clone());
                } else if *from == faction {
                    own_lost.push(region_name.clone());
                } else {
                    rival_moves.push(format!(
                        "{}が{}を{}から奪取",
                        world.faction(*to).name,
                        region_name,
                        world.faction(*from).name,
                    ));
                }
            }
            Event::Battle { region, factions, casualties, sides } if factions.contains(&faction) => {
                let branch_casualties: Vec<(Branch, f32)> = archipelago_sim::event::battle_branch_totals(sides)
                    .into_iter()
                    .map(|e| (e.branch, e.casualties))
                    .collect();
                tally_battle(&mut own_battles, format!("{}での戦闘", world.region(*region).name), *casualties, &branch_casualties);
            }
            Event::NavalBattle { zone, factions, casualties } if factions.contains(&faction) => {
                tally_battle(&mut own_battles, format!("{}沖での海戦", world.sea_zone(*zone).name), *casualties, &[]);
            }
            Event::TreatySigned { a, b, treaty } if *a == faction || *b == faction => {
                let other = if *a == faction { *b } else { *a };
                own_treaties.push(format!("{}と{}を締結", world.faction(other).name, treaty_label(*treaty)));
            }
            Event::WarDeclared { a, b } if *a == faction => {
                own_wars.push(format!("{}に宣戦を布告した", world.faction(*b).name));
            }
            Event::WarDeclared { a, b } if *b == faction => {
                own_wars.push(format!("{}より宣戦を布告された", world.faction(*a).name));
            }
            Event::Strike { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}でストライキが発生", world.faction(*f2).name))
            }
            Event::Protest { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}でデモが拡大", world.faction(*f2).name))
            }
            Event::Mutiny { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}で軍規律が乱れている", world.faction(*f2).name))
            }
            Event::CapitalFlight { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}で資本逃避が進んでいる", world.faction(*f2).name))
            }
            Event::RegimeChange { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}で政権が崩壊した", world.faction(*f2).name))
            }
            Event::FactionEliminated { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}が全領土を失い脱落した", world.faction(*f2).name))
            }
            Event::NodeStruck { attacker, defender, outcome, .. } => {
                let achieved = matches!(outcome, StrikeOutcome::KnockedOut);
                if *attacker == faction {
                    tally_strike(&mut own_node_strikes, world.faction(*defender).name.clone(), achieved);
                } else if *defender == faction {
                    tally_strike(&mut node_strikes_at_home, world.faction(*attacker).name.clone(), achieved);
                } else {
                    rival_air_ops += 1;
                }
            }
            Event::LineInterdicted { attacker, defender, capacity_cut, .. } => {
                if *attacker == faction {
                    tally_strike(&mut own_line_strikes, world.faction(*defender).name.clone(), *capacity_cut);
                } else if *defender == faction {
                    tally_strike(&mut line_strikes_at_home, world.faction(*attacker).name.clone(), *capacity_cut);
                } else {
                    rival_air_ops += 1;
                }
            }
            _ => {}
        }
    }

    let blockaded_ports: Vec<String> = world
        .regions
        .iter()
        .filter(|r| r.owner == faction && r.port > 0.0 && naval::is_port_blockaded(world, r.id))
        .map(|r| r.name.clone())
        .collect();

    let mut movers: Vec<Mover> = Vec::new();
    for group in ALL_GROUPS {
        if let Some(m) = political_mover(group, sf, f) {
            movers.push(m);
        }
    }
    movers.extend(stability_mover(sf, f));
    movers.extend(war_support_mover(sf, f));
    movers.extend(shortage_mover(sf, f));
    movers.extend(military_mover(sf, f));
    movers.extend(stockpile_mover(sf, f));
    movers.extend(import_mover(total_import_flow(start, faction), total_import_flow(world, faction)));
    movers.extend(force_mover(branch_unit_counts(start, faction), branch_unit_counts(world, faction)));
    movers.extend(territory_mover(
        start.region_count(faction),
        world.region_count(faction),
        &own_gained,
        &own_lost,
        avg_devastation(start, faction),
        avg_devastation(world, faction),
    ));
    movers.extend(transport_mover(
        down_transport_count(start, faction),
        down_transport_count(world, faction),
        &own_node_strikes,
        &own_line_strikes,
        &node_strikes_at_home,
        &line_strikes_at_home,
    ));
    movers.sort_by(|a, b| b.magnitude.partial_cmp(&a.magnitude).expect("mover magnitudes are always finite"));

    let mut s = format!("{}日〜{}日 {}紙: ", start.day, world.day, f.name);

    let evidence_empty = own_battles.is_empty()
        && own_treaties.is_empty()
        && own_wars.is_empty()
        && blockaded_ports.is_empty()
        && rival_moves.is_empty()
        && rival_turmoil.is_empty()
        && rival_air_ops == 0;

    if movers.is_empty() && evidence_empty {
        s.push_str("前線に大きな動きはなく、静穏な期間が続いた。");
    } else {
        for m in &movers {
            s.push_str(&m.text);
            s.push(' ');
        }

        if !own_battles.is_empty() {
            let lines: Vec<String> = own_battles
                .iter()
                .map(|(label, count, casualties, branch_casualties)| {
                    let branch_note = if branch_casualties.is_empty() {
                        String::new()
                    } else {
                        let parts: Vec<String> =
                            branch_casualties.iter().map(|(branch, lost)| format!("{}{:.1}万人", branch.label(), lost)).collect();
                        format!("、内訳 {}", parts.join("/"))
                    };
                    if *count > 1 {
                        format!("{label}が{count}回 (損耗合計 {casualties:.1}万人{branch_note})")
                    } else {
                        format!("{label} (損耗 {casualties:.1}万人{branch_note})")
                    }
                })
                .collect();
            s.push_str(&format!("前線各地で交戦が続いた。{}。", join_capped(&lines, "地点")));
        }
        if !own_wars.is_empty() {
            s.push_str(&format!("{}。", own_wars.join("、")));
        }
        if !blockaded_ports.is_empty() {
            s.push_str(&format!("{}は依然として海上封鎖下にある。", join_capped(&blockaded_ports, "港")));
        }
        if !own_treaties.is_empty() {
            s.push_str(&format!("外交面では{}。", own_treaties.join("、")));
        }
        if !rival_moves.is_empty() {
            s.push_str(&format!(" 他方、{}という動きもあった。", join_capped(&rival_moves, "件")));
        }
        if !rival_turmoil.is_empty() {
            s.push_str(&format!(" 敵陣営では{}など、体制の動揺が伝えられている。", join_capped(&rival_turmoil, "件")));
        }
        if rival_air_ops > 0 {
            s.push_str(&format!(" 他陣営間でも{rival_air_ops}件の空爆・妨害があった。"));
        }
    }

    // The government's closing framing (docs/newspaper-spec.md §2: "その
    //新聞そのものが国の状態を語る情報源になる") is reserved room up front,
    // not appended and hoped-to-fit: truncating the *whole* string
    // (body + tone) to `MAX_ARTICLE_CHARS` after appending used to cut the
    // tone off partway - or entirely - in exactly the busy, eventful
    // periods (many movers, many evidence clauses) where a hollow official
    // line is most informative. Reserving space is preferred over
    // truncating-then-checking because the cap must be enforced
    // unconditionally (`MAX_ARTICLE_CHARS` is a hard bound the backend path
    // also promises), so the body has to give way first, every time, rather
    // than the tone being a best-effort afterthought.
    // The government's closing framing (docs/newspaper-spec.md §2: "その
    //新聞そのものが国の状態を語る情報源になる") is reserved room up front,
    // not appended and hoped-to-fit: truncating the *whole* string
    // (body + tone) to `MAX_ARTICLE_CHARS` after appending used to cut the
    // tone off partway - or entirely - in exactly the busy, eventful
    // periods (many movers, many evidence clauses) where a hollow official
    // line is most informative. Reserving space is preferred over
    // truncating-then-checking because the cap must be enforced
    // unconditionally (`MAX_ARTICLE_CHARS` is a hard bound the backend path
    // also promises), so the body has to give way first, every time, rather
    // than the tone being a best-effort afterthought.
    let tone_suffix = format!(" {}", government_tone(f));
    let body_budget = MAX_ARTICLE_CHARS.saturating_sub(tone_suffix.chars().count());
    let mut s = truncate_chars(&s, body_budget);
    s.push_str(&tone_suffix);
    s
}

/// Generates one faction's article for the period `start.day..world.day`.
/// Always succeeds - a backend `Err`, or an empty/whitespace-only `Ok`
/// response, falls back to `template_summary` rather than propagating a
/// failure (docs/phase4-spec.md "Stage 4C": generation failure must still
/// produce a text, never stop the game or leave a gap in the log).
///
/// docs/newspaper-spec.md §3: the backend is never asked to reconstruct an
/// article from raw events - `template_summary` already *is* a complete,
/// readable article, and the backend's only job (when one succeeds) is to
/// rewrite that text more naturally, never to replace its content.
pub fn generate_article<B: LlmBackend>(backend: &B, start: &World, world: &World, faction: FactionId, events: &[Event]) -> NewspaperArticle {
    let template = template_summary(start, world, faction, events);

    let request = LlmRequest { system: SYSTEM_PROMPT.to_string(), user: template.clone(), max_output_tokens: 320 };

    match backend.complete(&request) {
        Ok(text) if !text.trim().is_empty() => NewspaperArticle {
            faction,
            period_start: start.day,
            period_end: world.day,
            text: truncate_chars(text.trim(), MAX_ARTICLE_CHARS),
            from_backend: true,
        },
        _ => NewspaperArticle { faction, period_start: start.day, period_end: world.day, text: template, from_backend: false },
    }
}

/// One article per currently-living faction, for the same reporting period -
/// this is what a headless `--newspaper` "issue" actually consists of.
pub fn generate_issue<B: LlmBackend>(backend: &B, start: &World, world: &World, events: &[Event]) -> Vec<NewspaperArticle> {
    world.factions.iter().filter(|f| f.alive).map(|f| generate_article(backend, start, world, f.id, events)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::event::BattleSide;
    use archipelago_sim::transport::TransportNodeKind;
    use archipelago_sim::ids::{RegionId, TransportLineId, TransportNodeId};
    use archipelago_sim::scenario;

    use crate::llm::{LlmError, MockBackend};

    /// `newspaper_falls_back_to_template` (docs/phase4-spec.md "Stage 4C の
    /// 受け入れ基準"): a backend that fails on every call still produces a
    /// nonempty article, and it's clearly marked as the template fallback,
    /// not backend prose.
    #[test]
    fn newspaper_falls_back_to_template() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let events = vec![Event::RegionCaptured { region: RegionId(4), from: FactionId(1), to: faction }];
        let article = generate_article(&backend, &world, &world, faction, &events);

        assert!(!article.from_backend, "a failing backend must fall back to the mechanical template");
        assert!(!article.text.trim().is_empty(), "the fallback template must still produce readable text");
        assert!(
            article.text.contains(&world.faction(faction).name),
            "the template should at least name the faction it's reporting for"
        );
    }

    /// An empty (whitespace-only) `Ok` response is treated the same as an
    /// outright failure - "the backend answered, but said nothing" must not
    /// produce a blank article.
    #[test]
    fn blank_backend_response_falls_back_to_template() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::new(vec![Ok("   \n  ".to_string())]);

        let article = generate_article(&backend, &world, &world, faction, &[]);
        assert!(!article.from_backend);
        assert!(!article.text.trim().is_empty());
    }

    /// A backend that actually answers is used verbatim (trimmed), not
    /// replaced by the template.
    #[test]
    fn successful_backend_response_is_used() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::new(vec![Ok("Our forces made a bold strategic redeployment today.".to_string())]);

        let article = generate_article(&backend, &world, &world, faction, &[]);
        assert!(article.from_backend);
        assert_eq!(article.text, "Our forces made a bold strategic redeployment today.");
    }

    /// `template_article_reflects_actual_events`: the fallback template is
    /// state/event-driven, not canned - an issue generated over a period
    /// containing a specific capture and a specific battle names both, and
    /// an issue over a quiet, unchanged period claims neither happened.
    #[test]
    fn template_article_reflects_actual_events() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let captured_region = RegionId(4); // 信越・北陸, owned by faction 1 at scenario start
        let battle_region = RegionId(1); // 北東北, owned by faction 0
        let mut after_capture = world.clone();
        after_capture.regions[captured_region.index()].owner = faction;
        after_capture.day += NEWSPAPER_INTERVAL_DAYS;

        let events = vec![
            Event::RegionCaptured { region: captured_region, from: FactionId(1), to: faction },
            Event::Battle { region: battle_region, factions: vec![faction, FactionId(1)], casualties: 3.5, sides: vec![] },
        ];

        let busy = generate_article(&backend, &world, &after_capture, faction, &events);
        assert!(!busy.from_backend, "no backend is configured for this test, so this must be the template");
        assert!(
            busy.text.contains(&world.region(captured_region).name),
            "an issue covering a capture must name the captured region: {}",
            busy.text
        );
        assert!(
            busy.text.contains(&world.region(battle_region).name),
            "an issue covering a battle must name the battle's region: {}",
            busy.text
        );

        let mut quiet_end = world.clone();
        quiet_end.day += NEWSPAPER_INTERVAL_DAYS;
        let quiet = generate_article(&backend, &world, &quiet_end, faction, &[]);
        assert!(
            !quiet.text.contains(&world.region(captured_region).name),
            "a quiet period's issue must not claim a capture that never happened: {}",
            quiet.text
        );
        assert!(
            !quiet.text.contains(&world.region(battle_region).name),
            "a quiet period's issue must not claim a battle that never happened: {}",
            quiet.text
        );
    }

    /// `generate_issue` produces exactly one article per living faction, and
    /// skips eliminated ones.
    #[test]
    fn issue_covers_every_living_faction() {
        let mut world = scenario::build_world();
        world.factions[2].alive = false;
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let issue = generate_issue(&backend, &world, &world, &[]);
        assert_eq!(issue.len(), 2);
        assert!(issue.iter().all(|a| a.faction != FactionId(2)));
    }

    /// docs/newspaper-spec.md §5, acceptance criterion 1 - the whole reason
    /// this module was rewritten (§0's playtest finding): a period where a
    /// group's support collapsed into crisis territory must be mentioned,
    /// *even when the triggering `Event` fired in an earlier issue* and this
    /// period's `events` slice is empty - the exact shape of the original
    /// defect (support already below `PROTEST_THRESHOLD`/`STRIKE_THRESHOLD`
    /// at the period's start, so no *fresh* `Strike`/`Protest` fires, and the
    /// old event-enumerating template had nothing left to read).
    ///
    /// Proven able to fail: dropping the `CRISIS_MOVER_BOOST` addition (so a
    /// mover is included by raw delta alone) was run by hand and turns this
    /// red - a standing, non-moving crisis has `delta == 0`, so the mover is
    /// filtered out entirely and the article falls back to its "quiet
    /// period" text:
    /// `0日〜30日 東方連合紙: 前線に大きな動きはなく、静穏な期間が続いた。 政府は
    /// 被害を限定的なものと発表し、動揺の沈静化を強調している。` - containing
    /// neither "労働者" nor "危機的", and containing "静穏" outright. Reverted
    /// after confirming; both assertions failed exactly as expected.
    #[test]
    fn support_collapse_is_mentioned_even_without_a_fresh_event() {
        let mut world = scenario::build_world();
        let faction = FactionId(0);
        let i = Group::Labor.index();
        world.factions[faction.index()].group_support[i] = 20.0; // already below STRIKE_THRESHOLD (38.0)

        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        // Support stays exactly where it started - no delta, no fresh Event.
        let backend = MockBackend::always_err(LlmError::Unavailable);
        let article = generate_article(&backend, &world, &end, faction, &[]);

        assert!(
            article.text.contains("労働者") && article.text.contains("危機的"),
            "a standing labor-support crisis must be mentioned even with zero events this period: {}",
            article.text
        );
        assert!(
            !article.text.contains("静穏"),
            "a faction with support collapsed into crisis must never be described as a quiet period: {}",
            article.text
        );
    }

    /// docs/newspaper-spec.md §5, acceptance criterion 2: fifteen
    /// occurrences of the same kind of event (repeated air raids on the same
    /// route, the exact scenario §0's playtest found) must not become
    /// fifteen near-identical lines - they must be tallied into one clause
    /// naming the count.
    ///
    /// Proven able to fail: run by hand with `format_strike_tally` changed
    /// to emit one clause per hit instead of a tallied count (the pre-fix
    /// shape every `NodeStruck`/`LineInterdicted` line used to have) -
    /// `article.text` came back as
    /// `...空爆・妨害を受けた。空爆・妨害を受けた。空爆・妨害を受けた。...`
    /// repeated fifteen times, containing no "計15回" anywhere and pushing
    /// `sentence_count` well past the `< 8` bound. Reverted after
    /// confirming; both assertions failed exactly as expected.
    #[test]
    fn fifteen_air_raids_do_not_become_fifteen_lines() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let attacker = FactionId(1);
        let line = TransportLineId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let events: Vec<Event> = (0..15)
            .map(|_| Event::LineInterdicted { attacker, defender: faction, line, capacity_cut: true })
            .collect();

        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let article = generate_article(&backend, &world, &end, faction, &events);

        assert!(
            article.text.contains("計15回"),
            "fifteen occurrences of the same event kind must be tallied into one clause naming the count: {}",
            article.text
        );
        let sentence_count = article.text.matches('。').count();
        assert!(
            sentence_count < 8,
            "an article covering 15 identical strikes must stay a handful of sentences, not one per strike \
             (got {sentence_count} sentences): {}",
            article.text
        );
    }

    /// docs/newspaper-spec.md §5, acceptance criterion 3: the government's
    /// tone must differ between a stable faction and a collapsing one - the
    /// same underlying `events` (none), read for two factions whose state
    /// differs, must produce two different closing framings.
    ///
    /// Proven able to fail: run by hand with `government_tone` hardcoded to
    /// always return its "おおむね平静" branch - `collapsing_article.text`
    /// came back ending in
    /// `...政府はおおむね平静な調子でこの期間の状況を発表している。`, containing
    /// neither "完全に掌握" nor "実情はそれとかけ離れている", failing the last
    /// assertion below exactly as expected. Reverted after confirming.
    #[test]
    fn tone_differs_between_stable_and_collapsing_faction() {
        let mut world = scenario::build_world();
        let stable = FactionId(0);
        let collapsing = FactionId(1);
        world.factions[stable.index()].stability = 80.0;
        world.factions[stable.index()].war_support = 70.0;
        world.factions[collapsing.index()].stability = 15.0;
        for g in ALL_GROUPS {
            world.factions[collapsing.index()].group_support[g.index()] = 10.0;
        }

        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let stable_article = generate_article(&backend, &world, &end, stable, &[]);
        let collapsing_article = generate_article(&backend, &world, &end, collapsing, &[]);

        assert_ne!(
            stable_article.text, collapsing_article.text,
            "a stable faction and a collapsing one must not read the same article"
        );
        assert!(
            stable_article.text.contains("誇らしげ") || !stable_article.text.contains("完全に掌握"),
            "a stable faction's government should not sound like it is denying a crisis: {}",
            stable_article.text
        );
        assert!(
            collapsing_article.text.contains("完全に掌握") || collapsing_article.text.contains("実情はそれとかけ離れている"),
            "a collapsing faction's government framing should read as hollow reassurance: {}",
            collapsing_article.text
        );
    }

    /// A quiet period (no events, no meaningful state movement) still reads
    /// as calm prose, not an empty string - the template's original "静穏な
    /// 期間" behaviour, preserved through the rewrite.
    #[test]
    fn quiet_period_reads_as_calm() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let article = generate_article(&backend, &world, &end, faction, &[]);
        assert!(article.text.contains("静穏"), "an unchanged period must still read as calm: {}", article.text);
    }

    /// `NodeStruck`/`LineInterdicted` events still surface in the template
    /// (regression guard for the Defect fix `template_summary`'s old doc
    /// described - before it existed, a whole bombing campaign left nothing
    /// in the newspaper at all), now via the tallied clause rather than a
    /// per-event line.
    #[test]
    fn node_strikes_are_not_silently_dropped() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let attacker = FactionId(1);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let events = vec![Event::NodeStruck {
            attacker,
            defender: faction,
            node: TransportNodeId(0),
            region: RegionId(0),
            node_kind: TransportNodeKind::Port,
            outcome: StrikeOutcome::KnockedOut,
        }];
        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let article = generate_article(&backend, &world, &end, faction, &events);
        assert!(article.text.contains("空爆"), "a node strike against this faction must appear in its article: {}", article.text);
    }

    /// A `Battle` with a nonempty `sides` still reports its per-branch
    /// breakdown - guards the `BranchEngagement` plumbing this rewrite kept
    /// unchanged from the pre-rewrite template.
    #[test]
    fn battle_reports_branch_breakdown() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let rival = FactionId(1);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let events = vec![Event::Battle {
            region: RegionId(1),
            factions: vec![faction, rival],
            casualties: 4.0,
            sides: vec![BattleSide { faction, branches: vec![archipelago_sim::event::BranchEngagement { branch: Branch::Armour, power: 10.0, casualties: 4.0 }] }],
        }];
        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let article = generate_article(&backend, &world, &end, faction, &events);
        assert!(article.text.contains("機甲"), "a battle with branch data should name the branch involved: {}", article.text);
    }

    /// A front that traded territory in both directions - three regions
    /// captured, three (well, two-for-two here) lost elsewhere - nets to an
    /// unchanged region count. `territory_mover` must still speak, naming
    /// both the gains and the losses, rather than reading the net as "no
    /// territory changed hands".
    ///
    /// Proven able to fail: with `territory_mover`'s magnitude computed from
    /// `region_delta.abs() * 20.0` (the pre-fix formula) instead of
    /// `(captured.len() + lost.len()) as f32 * 20.0`, this test goes red:
    /// `region_delta` is `0.0` (4 regions both before and after), so
    /// `territory_mover` returns `None`, no other mover clears its floor,
    /// and `evidence_empty` is also `true` (it never inspects
    /// `own_gained`/`own_lost`) - the article comes back
    /// `...前線に大きな動きはなく、静穏な期間が続いた。...`, naming neither
    /// 信越・北陸/東海 nor 北東北/南東北 and containing "静穏" outright, failing
    /// all four assertions below. Reverted after confirming.
    #[test]
    fn net_zero_territory_exchange_is_not_quiet() {
        let world = scenario::build_world();
        let faction = FactionId(0); // touhou_rengou: owns hokkaido/kita_tohoku/minami_tohoku/kanto
        let rival_a = FactionId(1); // chuo_domei: owns shinetsu_hokuriku/tokai/kinki
        let rival_b = FactionId(2); // seihou_domei: owns chugoku/shikoku/kyushu
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let captured_1 = RegionId(4); // 信越・北陸, from chuo_domei
        let captured_2 = RegionId(5); // 東海, from chuo_domei
        let lost_1 = RegionId(1); // 北東北, to seihou_domei
        let lost_2 = RegionId(2); // 南東北, to seihou_domei

        let mut end = world.clone();
        end.regions[captured_1.index()].owner = faction;
        end.regions[captured_2.index()].owner = faction;
        end.regions[lost_1.index()].owner = rival_b;
        end.regions[lost_2.index()].owner = rival_b;
        end.day += NEWSPAPER_INTERVAL_DAYS;

        assert_eq!(
            world.region_count(faction),
            end.region_count(faction),
            "this test only means something if the net region count is unchanged"
        );

        let events = vec![
            Event::RegionCaptured { region: captured_1, from: rival_a, to: faction },
            Event::RegionCaptured { region: captured_2, from: rival_a, to: faction },
            Event::RegionCaptured { region: lost_1, from: faction, to: rival_b },
            Event::RegionCaptured { region: lost_2, from: faction, to: rival_b },
        ];
        let article = generate_article(&backend, &world, &end, faction, &events);

        assert!(
            article.text.contains(&world.region(captured_1).name) && article.text.contains(&world.region(captured_2).name),
            "an evenly-traded front must still name what was captured: {}",
            article.text
        );
        assert!(
            article.text.contains(&world.region(lost_1).name) && article.text.contains(&world.region(lost_2).name),
            "an evenly-traded front must still name what was lost: {}",
            article.text
        );
        assert!(
            !article.text.contains("静穏"),
            "a faction that traded territory in both directions must never read as a quiet period: {}",
            article.text
        );
    }

    /// A handful of unsuccessful strikes (fewer than the six occurrences
    /// `transport_mover`'s old per-attempt weight of `1.0` needed to clear
    /// `MOVER_MAGNITUDE_FLOOR`) is still a real attack the article must
    /// report - an air campaign that failed to knock anything out is not the
    /// same as no air campaign at all.
    ///
    /// Proven able to fail: with `transport_mover`'s magnitude computed as
    /// `strike_count as f32` (the pre-fix per-attempt weight) instead of
    /// `strike_count as f32 * 6.0`, this test goes red: three unsuccessful
    /// strikes give `strike_count == 3`, `achieved_count == 0`, `delta ==
    /// 0.0`, so `magnitude == 3.0 < MOVER_MAGNITUDE_FLOOR (6.0)` and
    /// `transport_mover` returns `None`; `evidence_empty` never inspects the
    /// strike tallies either, so the article comes back
    /// `...前線に大きな動きはなく、静穏な期間が続いた。...`, containing neither
    /// "空爆" nor "計3回", and containing "静穏" outright. Reverted after
    /// confirming.
    #[test]
    fn failed_strikes_are_not_a_quiet_period() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let attacker = FactionId(1);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let events = vec![
            Event::NodeStruck {
                attacker,
                defender: faction,
                node: TransportNodeId(0),
                region: RegionId(0),
                node_kind: TransportNodeKind::Port,
                outcome: StrikeOutcome::StillOperational,
            },
            Event::NodeStruck {
                attacker,
                defender: faction,
                node: TransportNodeId(0),
                region: RegionId(0),
                node_kind: TransportNodeKind::Port,
                outcome: StrikeOutcome::StillOperational,
            },
            Event::LineInterdicted { attacker, defender: faction, line: TransportLineId(0), capacity_cut: false },
        ];
        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let article = generate_article(&backend, &world, &end, faction, &events);

        assert!(
            article.text.contains("空爆") && article.text.contains("計3回"),
            "three unsuccessful strikes must still be reported and tallied, not dropped: {}",
            article.text
        );
        assert!(
            !article.text.contains("静穏"),
            "a faction under an (unsuccessful) air campaign must never read as a quiet period: {}",
            article.text
        );
    }

    /// The counterpart to the two false-quiet regressions above: a period
    /// where genuinely nothing moved - no captures, no losses, no strikes,
    /// no political/stability/shortage/military movement - must still read
    /// as calm. Fixing the magnitude formulas above must not make every
    /// period noisy.
    ///
    /// Proven able to pass only by coincidence would be worse than useless,
    /// so this is deliberately the same scenario as `quiet_period_reads_as_calm`
    /// re-asserted after both magnitude fixes: run by hand with either fix
    /// reverted, this test still passes (it has no captures/losses/strikes
    /// to react to) - it is `net_zero_territory_exchange_is_not_quiet` and
    /// `failed_strikes_are_not_a_quiet_period` above that catch the
    /// regressions; this one confirms the fixes didn't overcorrect.
    #[test]
    fn a_network_recovery_is_reported_not_silent() {
        // Nothing but recovery: three nodes/lines were down at the start and
        // none is at the end, with no strike events at all in the period.
        let mover = transport_mover(3, 0, &[], &[], &[], &[]);
        let mover = mover.expect("a period whose whole story is that the network came back must produce a mover");
        assert!(
            mover.text.contains("復旧"),
            "the recovery must actually be said, not merely counted toward the magnitude: got {:?}",
            mover.text
        );
    }

    /// **Confirmed this test can fail.** Removing the `else if start_down > 0`
    /// branch makes every text branch stay silent, so `transport_mover`
    /// returns `None` and the `expect` above trips - a real recovery reported
    /// as a quiet period. Restored, and it passes.
    #[test]
    fn zero_shortage_is_never_called_persistently_high() {
        // Rationing moves enough to carry the mover past its floor while
        // shortage sits at zero throughout.
        let world = scenario::build_world();
        let mut start = world.faction(FactionId(0)).clone();
        start.civilian_ration = 0.80;
        start.shortage = 0.0;
        let mut end = start.clone();
        end.civilian_ration = 0.90;

        let mover = shortage_mover(&start, &end).expect("a 10-point rationing swing must clear the floor");
        assert!(
            !mover.text.contains("高止まり"),
            "with shortage at 0% the paper must not claim it is 高止まり - stating something untrue is the one \
             thing docs/newspaper-spec.md cannot tolerate: got {:?}",
            mover.text
        );
    }

    /// **Confirmed this test can fail.** Restoring the old catch-all branch
    /// makes the text read 「物資不足は0%の水準で高止まりしている」, tripping
    /// the assertion. Restored, and it passes.
    #[test]
    fn a_negotiated_territorial_change_is_reported() {
        // A region handed over by treaty: the count moved, but
        // `diplomacy::transfer_region` emits no `RegionCaptured`, so both
        // event lists are empty.
        let mover = territory_mover(10, 9, &[], &[], 0.0, 0.0);
        let mover = mover.expect("a region lost by treaty is a real territorial change and must produce a mover");
        assert!(
            mover.text.contains("10") && mover.text.contains("9"),
            "the paper must name the change it detected: got {:?}",
            mover.text
        );
    }

    /// **Confirmed this test can fail.** Dropping `region_delta.abs() * 20.0`
    /// from `territory_mover`'s magnitude makes it return `None` before the
    /// region-count fallback text can run, so the `expect` above trips and a
    /// negotiated cession vanishes from the issue. Restored, and it passes.
    #[test]
    fn genuinely_uneventful_period_is_still_quiet() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let mut end = world.clone();
        end.day += NEWSPAPER_INTERVAL_DAYS;
        let article = generate_article(&backend, &world, &end, faction, &[]);
        assert!(
            article.text.contains("静穏"),
            "a period with no captures, losses, strikes, or state movement must still read as calm: {}",
            article.text
        );
    }

    /// Defect fix: a period whose *only* notable change is a jump in actual
    /// sea-import volume (docs/newspaper-spec.md §1 "経済": "輸入量") - no
    /// captures, no strikes, no political/shortage movement - must not read
    /// as quiet.
    ///
    /// Proven able to fail: with `import_mover` removed from the `movers`
    /// list this test goes red - the article comes back
    /// `...前線に大きな動きはなく、静穏な期間が続いた。...`, containing
    /// neither "輸入" nor the new import figure, and containing "静穏"
    /// outright. Reverted after confirming.
    #[test]
    fn import_surge_is_not_quiet() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let capital = world.faction(faction).capital;
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let mut end = world.clone();
        end.regions[capital.index()].import_flow = 40.0;
        end.day += NEWSPAPER_INTERVAL_DAYS;

        let article = generate_article(&backend, &world, &end, faction, &[]);
        assert!(
            article.text.contains("輸入量"),
            "a large import-volume swing with nothing else moving must be reported: {}",
            article.text
        );
        assert!(
            !article.text.contains("静穏"),
            "a faction whose only notable change is an import-volume swing must never read as a quiet period: {}",
            article.text
        );
    }

    /// Defect fix: a period whose *only* notable change is a per-commodity
    /// stockpile swing (docs/newspaper-spec.md §1 "経済": "品目ごとの在庫と
    /// 不足") - nothing else moves: no captures, no strikes, no political or
    /// shortage/ration movement - must not read as quiet.
    ///
    /// Proven able to fail: with `stockpile_mover` removed from the `movers`
    /// list this test goes red - the article comes back
    /// `...前線に大きな動きはなく、静穏な期間が続いた。...`, containing
    /// neither "軍需品" nor "備蓄", and containing "静穏" outright. Reverted
    /// after confirming.
    #[test]
    fn stockpile_swing_is_not_quiet() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let mut end = world.clone();
        let i = Good::Munitions.index();
        // A three-quarters drawdown of the munitions stockpile, nothing else
        // in the faction's state moves at all.
        end.factions[faction.index()].stock[i] *= 0.25;
        end.day += NEWSPAPER_INTERVAL_DAYS;

        let article = generate_article(&backend, &world, &end, faction, &[]);
        assert!(
            article.text.contains("軍需品") && article.text.contains("備蓄"),
            "a large stockpile drawdown with nothing else moving must be reported: {}",
            article.text
        );
        assert!(
            !article.text.contains("静穏"),
            "a faction whose only notable change is a stockpile swing must never read as a quiet period: {}",
            article.text
        );
    }

    /// Defect fix: a period whose *only* notable change is a force
    /// build-up (docs/newspaper-spec.md §1 "軍事": "部隊数（兵科別）") - new
    /// units raised, no casualties, no supply-ratio movement, no captures -
    /// must not read as quiet.
    ///
    /// Proven able to fail: with `force_mover` removed from the `movers`
    /// list this test goes red - the article comes back
    /// `...前線に大きな動きはなく、静穏な期間が続いた。...`, containing
    /// neither "機甲" nor "増強", and containing "静穏" outright. Reverted
    /// after confirming.
    #[test]
    fn force_buildup_is_not_quiet() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let capital = world.faction(faction).capital;
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let mut end = world.clone();
        // Three fresh Armour units, raised with no casualties and no supply
        // movement to speak of.
        for i in 0..3 {
            let id = archipelago_sim::ids::UnitId(end.units.len() as u32);
            end.units.push(archipelago_sim::military::Unit {
                id,
                owner: faction,
                name: format!("Armour Buildup {i}"),
                station: archipelago_sim::world::Station::Region(capital),
                movement: None,
                manpower: archipelago_sim::balance::UNIT_MANPOWER,
                equipment: archipelago_sim::balance::UNIT_EQUIPMENT,
                organization: archipelago_sim::balance::UNIT_ORG,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: archipelago_sim::world::Station::Region(capital),
                branch: Some(Branch::Armour),
                experience: 0.0,
                alive: true,
            });
        }
        end.day += NEWSPAPER_INTERVAL_DAYS;

        let article = generate_article(&backend, &world, &end, faction, &[]);
        assert!(
            article.text.contains("機甲") && article.text.contains("増強"),
            "a force build-up with nothing else moving must be reported: {}",
            article.text
        );
        assert!(
            !article.text.contains("静穏"),
            "a faction whose only notable change is a force build-up must never read as a quiet period: {}",
            article.text
        );
    }

    /// Defect fix: the government's closing framing (docs/newspaper-spec.md
    /// §2) must survive intact even in the most eventful issue this module
    /// can produce - many political groups in crisis, a stability/war-support
    /// collapse, a shortage/ration squeeze, heavy casualties, a stockpile
    /// crash, a force build-up, territory trading hands in both directions,
    /// repeated battles, and a heavy air campaign both ways - all competing
    /// for room under `MAX_ARTICLE_CHARS`.
    ///
    /// Proven able to fail: reverting `template_summary` to truncate the
    /// whole `body + tone` string in one pass (`truncate_chars(&s,
    /// MAX_ARTICLE_CHARS)` after appending the tone, the pre-fix shape) was
    /// run by hand against this exact scenario - the resulting
    /// `article.text` was exactly `MAX_ARTICLE_CHARS` (900) characters long
    /// and ended mid-clause on a mover sentence, containing none of the four
    /// `tone_options` strings anywhere, let alone at the end. Reverted after
    /// confirming.
    #[test]
    fn government_tone_survives_a_maximally_eventful_issue() {
        let world = scenario::build_world();
        let faction = FactionId(0); // touhou_rengou: hokkaido/kita_tohoku/minami_tohoku/kanto
        let rival_a = FactionId(1); // chuo_domei: shinetsu_hokuriku/tokai/kinki
        let rival_b = FactionId(2); // seihou_domei: chugoku/shikoku/kyushu
        let capital = world.faction(faction).capital;
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let mut end = world.clone();
        let fi = faction.index();

        for g in ALL_GROUPS {
            end.factions[fi].group_support[g.index()] = 5.0;
        }
        end.factions[fi].stability = 10.0;
        end.factions[fi].war_support = 5.0;
        end.factions[fi].shortage = 0.9;
        end.factions[fi].civilian_ration = 0.2;
        end.factions[fi].casualties = world.factions[fi].casualties + 50.0;
        end.factions[fi].supply_ratio = (world.factions[fi].supply_ratio - 0.6).max(0.0);
        let munitions = Good::Munitions.index();
        end.factions[fi].stock[munitions] *= 0.1;

        // A force build-up across every branch.
        for i in 0..12 {
            let id = archipelago_sim::ids::UnitId(end.units.len() as u32);
            end.units.push(archipelago_sim::military::Unit {
                id,
                owner: faction,
                name: format!("Surge {i}"),
                station: archipelago_sim::world::Station::Region(capital),
                movement: None,
                manpower: archipelago_sim::balance::UNIT_MANPOWER,
                equipment: archipelago_sim::balance::UNIT_EQUIPMENT,
                organization: archipelago_sim::balance::UNIT_ORG,
                morale: 1.0,
                supply: 1.0,
                arms_delivery: 1.0,
                arms_budget: 0.0,
                arms_delivery_station: archipelago_sim::world::Station::Region(capital),
                branch: Some(ALL_BRANCHES[i % 3]),
                experience: 0.0,
                alive: true,
            });
        }

        // Territory trades hands in both directions.
        let captured_1 = RegionId(4); // 信越・北陸
        let captured_2 = RegionId(5); // 東海
        let captured_3 = RegionId(6); // 近畿
        let lost_1 = RegionId(1); // 北東北
        let lost_2 = RegionId(2); // 南東北
        end.regions[captured_1.index()].owner = faction;
        end.regions[captured_2.index()].owner = faction;
        end.regions[captured_3.index()].owner = faction;
        end.regions[lost_1.index()].owner = rival_b;
        end.regions[lost_2.index()].owner = rival_b;
        for r in [captured_1, captured_2, captured_3] {
            end.regions[r.index()].devastation = 0.6;
        }
        end.day += NEWSPAPER_INTERVAL_DAYS;

        let mut events = vec![
            Event::RegionCaptured { region: captured_1, from: rival_a, to: faction },
            Event::RegionCaptured { region: captured_2, from: rival_a, to: faction },
            Event::RegionCaptured { region: captured_3, from: rival_a, to: faction },
            Event::RegionCaptured { region: lost_1, from: faction, to: rival_b },
            Event::RegionCaptured { region: lost_2, from: faction, to: rival_b },
            Event::TreatySigned { a: faction, b: rival_a, treaty: Treaty::Ceasefire },
            Event::TreatySigned { a: faction, b: rival_b, treaty: Treaty::NonAggression },
            Event::TreatySigned { a: rival_a, b: faction, treaty: Treaty::MilitaryAccess },
            Event::WarDeclared { a: faction, b: rival_b },
            Event::WarDeclared { a: rival_a, b: faction },
            Event::Strike { faction: rival_a },
            Event::Protest { faction: rival_b },
            Event::Mutiny { faction: rival_a },
            Event::CapitalFlight { faction: rival_b },
            Event::RegimeChange { faction: rival_a },
            Event::RegionCaptured { region: RegionId(7), from: rival_b, to: rival_a },
            Event::RegionCaptured { region: RegionId(8), from: rival_b, to: rival_a },
        ];
        for region in [RegionId(0), RegionId(1), RegionId(2), RegionId(3)] {
            events.push(Event::Battle { region, factions: vec![faction, rival_a], casualties: 3.0, sides: vec![] });
        }
        for _ in 0..15 {
            events.push(Event::NodeStruck {
                attacker: rival_a,
                defender: faction,
                node: TransportNodeId(0),
                region: RegionId(0),
                node_kind: TransportNodeKind::Port,
                outcome: StrikeOutcome::KnockedOut,
            });
        }
        for _ in 0..15 {
            events.push(Event::LineInterdicted { attacker: rival_b, defender: faction, line: TransportLineId(0), capacity_cut: true });
        }
        for _ in 0..3 {
            events.push(Event::NodeStruck {
                attacker: rival_a,
                defender: rival_b,
                node: TransportNodeId(1),
                region: RegionId(7),
                node_kind: TransportNodeKind::Port,
                outcome: StrikeOutcome::KnockedOut,
            });
        }
        for _ in 0..5 {
            events.push(Event::NodeStruck {
                attacker: faction,
                defender: rival_a,
                node: TransportNodeId(2),
                region: RegionId(4),
                node_kind: TransportNodeKind::Port,
                outcome: StrikeOutcome::KnockedOut,
            });
        }

        let article = generate_article(&backend, &world, &end, faction, &events);

        assert!(
            article.text.chars().count() <= MAX_ARTICLE_CHARS,
            "article must respect the character cap: {} chars: {}",
            article.text.chars().count(),
            article.text
        );

        let tone_options = [
            "政府は「事態は完全に掌握している」と発表しているが、国内の実情はそれとかけ離れている。",
            "政府は被害を限定的なものと発表し、動揺の沈静化を強調している。",
            "政府はこの期間の成果を国民に誇らしげに公表している。",
            "政府はおおむね平静な調子でこの期間の状況を発表している。",
        ];
        assert!(
            tone_options.iter().any(|t| article.text.ends_with(t)),
            "a maximally eventful issue must still end with the government's complete framing, not a truncated fragment: {}",
            article.text
        );
    }
}
