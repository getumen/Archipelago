//! Stage 4C (docs/phase4-spec.md "Stage 4C — 新聞・報道生成", design.md
//! §13): periodic newspaper generation from the accumulated `Event` stream
//! and board deltas, with a per-faction slant - a faction's own defeat reads
//! as a strategic redeployment, a rival's crisis reads as systemic collapse.
//!
//! ## The boundary this module exists to enforce
//!
//! Everything here only ever *reads* `&World`/`&[Event]` and produces
//! `String`s. No function in this module takes a `&mut World`, a `&mut
//! Simulation`, or anything else that could feed generated prose back into
//! the simulation - see `newspaper_does_not_affect_simulation` for the
//! guarantee that toggling newspaper generation on or off cannot change a
//! single value in the game. Unlike Stage 4B's `Doctrine`/`TreatyTerm`
//! parsing, there is no structured schema to parse a response into at all:
//! a backend's raw text (trimmed and length-capped) *is* the article, so
//! there is nothing here for a hostile or malformed response to smuggle
//! into the simulation through.
//!
//! Generation failure (`LlmBackend::complete` returning `Err`, or an `Ok`
//! response that's empty/whitespace-only) falls back to a mechanical,
//! backend-free template built straight from `events`/`world`
//! (`template_summary`) - see `newspaper_falls_back_to_template`.

use archipelago_sim::diplomacy::Treaty;
use archipelago_sim::event::{Event, StrikeOutcome};
use archipelago_sim::ids::FactionId;
use archipelago_sim::naval;
use archipelago_sim::transport::TransportNodeKind;
use archipelago_sim::world::World;

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
/// than backend-generated prose (`newspaper_falls_back_to_template`).
#[derive(Clone, Debug, PartialEq)]
pub struct NewspaperArticle {
    pub faction: FactionId,
    pub period_start: u32,
    pub period_end: u32,
    pub text: String,
    pub from_backend: bool,
}

/// System prompt for generating one faction's article - the whole point of
/// Stage 4C's "勢力ごとに異なる論調になる" (design.md §13): the *same*
/// events, written from a different faction's newsroom, read differently.
const SYSTEM_PROMPT: &str = "You are writing today's newspaper for one faction's home audience in a grand-strategy \
war simulation. You are that faction's own domestic press, not a neutral chronicler: frame this faction's own \
setbacks as strategic redeployments or deliberate tactical adjustments, and frame a rival faction's turmoil as \
systemic collapse or the fruit of poor leadership. Write 2-4 short sentences of plain prose covering the period's \
events for this faction's readers. Output nothing but the article text itself - no JSON, no headline markup, no \
preamble.";

/// ", armour/infantry" style English suffix for `event_line`'s `Battle`
/// case - which branches this fight actually involved (`event::
/// battle_branch_totals`'s own doc), in `military::ALL_BRANCHES` order.
/// Empty string when `sides` is empty (a battle whose sides never entered
/// the damage loop - `event::BattleSide`'s own doc), so a caller can splice
/// this straight into a sentence without a dangling separator.
fn branch_summary_en(sides: &[archipelago_sim::event::BattleSide]) -> String {
    let totals = archipelago_sim::event::battle_branch_totals(sides);
    if totals.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = totals.iter().map(|t| t.branch.key()).collect();
    format!(", {}", parts.join("/"))
}

/// A short, human-readable line for one `Event`, used by both the LLM
/// prompt and the mechanical template - falls back to `Event`'s own
/// `Display` (id-based) for any variant not specifically named here.
fn event_line(world: &World, event: &Event) -> String {
    match event {
        Event::RegionCaptured { region, from, to } => format!(
            "{} was captured by {} from {}",
            world.region(*region).name,
            world.faction(*to).name,
            world.faction(*from).name,
        ),
        Event::Battle { region, factions, casualties, sides } => {
            let names: Vec<&str> = factions.iter().map(|f| world.faction(*f).name.as_str()).collect();
            format!(
                "battle at {}: {} ({:.1} manpower lost{})",
                world.region(*region).name,
                names.join(" vs "),
                casualties,
                branch_summary_en(sides),
            )
        }
        Event::TreatySigned { a, b, treaty } => {
            format!("{} and {} signed a {}", world.faction(*a).name, world.faction(*b).name, treaty.key())
        }
        Event::WarDeclared { a, b } => {
            format!("{} declared war on {}", world.faction(*a).name, world.faction(*b).name)
        }
        Event::FactionEliminated { faction } => format!("{} was eliminated", world.faction(*faction).name),
        other => format!("{other}"),
    }
}

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

/// Japanese label for a `TransportNodeKind`, for the fallback template -
/// same small, deliberate duplicate `treaty_label` above already is, this
/// time of `apps/game`'s `event_text::node_kind_ja`/`apps/headless`'s
/// `report::node_kind_label`.
fn node_kind_label(kind: TransportNodeKind) -> &'static str {
    match kind {
        TransportNodeKind::Airfield => "飛行場",
        TransportNodeKind::Port => "港",
        TransportNodeKind::Depot | TransportNodeKind::Junction => "拠点",
    }
}

/// The mechanical, backend-free fallback (docs/phase4-spec.md "生成に失敗し
/// たら、テンプレートによる機械的な要約に落ちる"), and - since Stage 4C's
/// default headless run has no LLM backend configured at all - the
/// newspaper's ordinary output, not just its failure path (see `main.rs`'s
/// `newspaper_backend`). Built entirely from the period's actual `events`
/// plus a `naval::is_port_blockaded` read of the board as it stands at
/// `world.day` (docs/phase4-spec.md "Stage 4C": "その期間の Event 列と盤面
/// の変化から記事を生成する" - events for the period, board state for
/// "blockade" specifically, since blockade is a standing condition rather
/// than a discrete `Event`), in Japanese to match every other piece of this
/// game's output (`report.rs`'s labels, and the region/faction names
/// themselves, are already Japanese).
///
/// Carries design.md §13's per-faction slant without any LLM: this
/// faction's own losses are worded as a "strategic redeployment"
/// (「戦略的転進」, design.md's own example phrase) rather than a defeat, and
/// any *other* faction's strikes/protests/mutinies/capital flight/regime
/// change/elimination are worded as that faction's "体制の動揺" (systemic
/// turmoil) - the same event data, read once per faction and framed by
/// whether it concerns this faction or a rival, exactly what Stage 4C asks
/// the *template* (not just the LLM path) to be capable of.
fn template_summary(world: &World, faction: FactionId, events: &[Event], period_start: u32) -> String {
    let f = world.faction(faction);

    let mut own_gained: Vec<String> = Vec::new();
    let mut own_lost: Vec<String> = Vec::new();
    // (location clause, engagement count, total casualties) - tallied rather
    // than one clause per `Event::Battle`/`NavalBattle`, since a single
    // contested front can generate several such events a day; over a whole
    // `NEWSPAPER_INTERVAL_DAYS` period that would otherwise read as dozens
    // of near-identical repeated clauses instead of one legible summary.
    // `Vec`-based linear lookup (not a `HashMap`) so the order clauses
    // appear in is the deterministic order their location was first seen in
    // `events`, matching this project's "no iteration-order dependence"
    // discipline (docs/phase4-spec.md "共通の制約").
    // Fourth element: per-`Branch` manpower lost, tallied the same way as
    // the total (`tally_battle`'s own doc) - always empty for a naval
    // engagement (`event_line`'s `NavalBattle` arm has no branch data to
    // give; naval isn't split into branches, `military::Branch`'s own
    // module doc).
    let mut own_battles: Vec<(String, u32, f32, Vec<(archipelago_sim::military::Branch, f32)>)> = Vec::new();
    let mut own_treaties: Vec<String> = Vec::new();
    let mut own_wars: Vec<String> = Vec::new();
    let mut own_unrest: Vec<&str> = Vec::new();
    // Defect fix: `Action::StrikeNode`/`Action::InterdictLine` used to emit
    // no `Event` at all, so a whole bombing campaign vanished from this
    // template with nothing to show for it. `own_air_ops` is this faction's
    // own strikes/interdictions against a rival; `struck_at_home` is a
    // rival's strikes/interdictions against this faction - the same "own
    // vs happened-to-us" split `own_gained`/`own_lost` already draw for
    // `RegionCaptured`.
    let mut own_air_ops: Vec<String> = Vec::new();
    let mut struck_at_home: Vec<String> = Vec::new();
    let mut rival_moves: Vec<String> = Vec::new();
    let mut rival_turmoil: Vec<String> = Vec::new();

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
                let branch_casualties: Vec<(archipelago_sim::military::Branch, f32)> =
                    archipelago_sim::event::battle_branch_totals(sides)
                        .into_iter()
                        .map(|e| (e.branch, e.casualties))
                        .collect();
                tally_battle(
                    &mut own_battles,
                    format!("{}での戦闘", world.region(*region).name),
                    *casualties,
                    &branch_casualties,
                );
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
            Event::Strike { faction: f2 } if *f2 == faction => own_unrest.push("労働者のストライキ"),
            Event::Protest { faction: f2 } if *f2 == faction => own_unrest.push("市民デモの拡大"),
            Event::Mutiny { faction: f2 } if *f2 == faction => own_unrest.push("軍内統制の乱れ"),
            Event::CapitalFlight { faction: f2 } if *f2 == faction => own_unrest.push("資本の流出"),
            Event::RegimeChange { faction: f2 } if *f2 == faction => own_unrest.push("政権交代"),
            Event::Strike { faction: f2 } => {
                rival_turmoil.push(format!("{}でストライキが発生", world.faction(*f2).name))
            }
            Event::Protest { faction: f2 } => {
                rival_turmoil.push(format!("{}でデモが拡大", world.faction(*f2).name))
            }
            Event::Mutiny { faction: f2 } => {
                rival_turmoil.push(format!("{}で軍規律が乱れている", world.faction(*f2).name))
            }
            Event::CapitalFlight { faction: f2 } => {
                rival_turmoil.push(format!("{}で資本逃避が進んでいる", world.faction(*f2).name))
            }
            Event::RegimeChange { faction: f2 } => {
                rival_turmoil.push(format!("{}で政権が崩壊した", world.faction(*f2).name))
            }
            Event::FactionEliminated { faction: f2 } if *f2 != faction => {
                rival_turmoil.push(format!("{}が全領土を失い脱落した", world.faction(*f2).name))
            }
            Event::NodeStruck { attacker, defender, node, node_kind, outcome, .. } => {
                let node_name = &world.transport_node(*node).name;
                let kind_label = node_kind_label(*node_kind);
                if *attacker == faction {
                    own_air_ops.push(format!(
                        "{}の{}「{}」を空爆{}",
                        world.faction(*defender).name,
                        kind_label,
                        node_name,
                        match outcome {
                            StrikeOutcome::KnockedOut => "し機能を停止させた",
                            StrikeOutcome::StillOperational => "したが機能は継続している",
                            StrikeOutcome::AlreadyDown => "したが、既に機能を停止していた",
                        },
                    ));
                } else if *defender == faction {
                    struck_at_home.push(format!(
                        "{}軍の空爆を受けた{}「{}」{}",
                        world.faction(*attacker).name,
                        kind_label,
                        node_name,
                        match outcome {
                            StrikeOutcome::KnockedOut => "は機能を停止した",
                            StrikeOutcome::StillOperational => "は稼働を継続している",
                            StrikeOutcome::AlreadyDown => "は既に停止していた",
                        },
                    ));
                } else {
                    rival_moves.push(format!(
                        "{}が{}の{}を空爆",
                        world.faction(*attacker).name,
                        world.faction(*defender).name,
                        kind_label,
                    ));
                }
            }
            Event::LineInterdicted { attacker, defender, line, capacity_cut } => {
                let l = world.transport_line(*line);
                let route =
                    format!("{}⇔{}", world.transport_node(l.from).name, world.transport_node(l.to).name);
                if *attacker == faction {
                    own_air_ops.push(format!(
                        "{}の輸送路線「{}」を攻撃{}",
                        world.faction(*defender).name,
                        route,
                        if *capacity_cut { "し輸送力を低下させた" } else { "したが既に途絶していた" },
                    ));
                } else if *defender == faction {
                    struck_at_home.push(format!(
                        "輸送路線「{}」が{}軍の攻撃を受け{}",
                        route,
                        world.faction(*attacker).name,
                        if *capacity_cut { "輸送力が低下した" } else { "既に途絶していた" },
                    ));
                } else {
                    rival_moves.push(format!(
                        "{}が{}の輸送路線を攻撃",
                        world.faction(*attacker).name,
                        world.faction(*defender).name,
                    ));
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

    let mut s = format!("{}日〜{}日 {}紙: ", period_start, world.day, f.name);

    let quiet = own_gained.is_empty()
        && own_lost.is_empty()
        && own_battles.is_empty()
        && own_treaties.is_empty()
        && own_wars.is_empty()
        && own_unrest.is_empty()
        && own_air_ops.is_empty()
        && struck_at_home.is_empty()
        && blockaded_ports.is_empty();
    if quiet {
        s.push_str("前線に大きな動きはなく、静穏な期間が続いた。");
    } else {
        if !own_battles.is_empty() {
            let lines: Vec<String> = own_battles
                .iter()
                .map(|(label, count, casualties, branch_casualties)| {
                    let branch_note = if branch_casualties.is_empty() {
                        String::new()
                    } else {
                        let parts: Vec<String> = branch_casualties
                            .iter()
                            .map(|(branch, lost)| format!("{}{:.1}万人", branch.label(), lost))
                            .collect();
                        format!("、内訳 {}", parts.join("/"))
                    };
                    if *count > 1 {
                        format!("{label}が{count}回 (損耗合計 {casualties:.1}万人{branch_note})")
                    } else {
                        format!("{label} (損耗 {casualties:.1}万人{branch_note})")
                    }
                })
                .collect();
            s.push_str(&format!("前線各地で交戦が続いた。{}。", lines.join("、")));
        }
        if !own_wars.is_empty() {
            s.push_str(&format!("{}。", own_wars.join("、")));
        }
        if !own_gained.is_empty() {
            s.push_str(&format!("{}を制圧し、戦線を前進させた。", own_gained.join("、")));
        }
        if !own_lost.is_empty() {
            s.push_str(&format!(
                "{}からは戦略的転進を行った。政府は被害を限定的と発表している。",
                own_lost.join("、"),
            ));
        }
        if !blockaded_ports.is_empty() {
            s.push_str(&format!("{}は依然として海上封鎖下にある。", blockaded_ports.join("、")));
        }
        if !own_air_ops.is_empty() {
            s.push_str(&format!("航空作戦では{}。", own_air_ops.join("、")));
        }
        if !struck_at_home.is_empty() {
            s.push_str(&format!("{}。政府は被害を限定的と発表している。", struck_at_home.join("、")));
        }
        if !own_treaties.is_empty() {
            s.push_str(&format!("外交面では{}。", own_treaties.join("、")));
        }
        if !own_unrest.is_empty() {
            s.push_str(&format!(
                "国内では{}が伝えられているが、政府は事態を掌握していると説明している。",
                own_unrest.join("、"),
            ));
        }
    }

    if !rival_moves.is_empty() {
        s.push_str(&format!(" 他方、{}という動きもあった。", rival_moves.join("、")));
    }
    if !rival_turmoil.is_empty() {
        s.push_str(&format!(" 敵陣営では{}など、体制の動揺が伝えられている。", rival_turmoil.join("、")));
    }

    s.push_str(&format!(" (安定度 {:.0}/100、戦意 {:.0}/100)", f.stability, f.war_support));
    // Same length cap the backend path applies to its own response
    // (`MAX_ARTICLE_CHARS`) - a long, eventful period (many battles across
    // many fronts) can otherwise produce a template article far longer than
    // anything an LLM-generated one would ever be.
    truncate_chars(&s, MAX_ARTICLE_CHARS)
}

/// Adds one `Battle`/`NavalBattle` occurrence to `tally`'s running per-
/// location count, casualty total, and per-branch casualty breakdown
/// (`branch_casualties` - empty for a naval engagement), appending a fresh
/// entry if `label` hasn't been seen yet in this period - see
/// `own_battles`' doc for why this is a linear `Vec` scan rather than a
/// `HashMap`. Same linear-scan merge for the per-branch breakdown - never
/// more than 3 entries (`military::ALL_BRANCHES`), so a nested `HashMap`
/// would be pure overhead for no benefit.
fn tally_battle(
    tally: &mut Vec<(String, u32, f32, Vec<(archipelago_sim::military::Branch, f32)>)>,
    label: String,
    casualties: f32,
    branch_casualties: &[(archipelago_sim::military::Branch, f32)],
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

/// Generates one faction's article for the period `period_start..world.day`.
/// Always succeeds - a backend `Err`, or an empty/whitespace-only `Ok`
/// response, falls back to `template_summary` rather than propagating a
/// failure (docs/phase4-spec.md "Stage 4C": generation failure must still
/// produce a text, never stop the game or leave a gap in the log).
pub fn generate_article<B: LlmBackend>(
    backend: &B,
    world: &World,
    faction: FactionId,
    events: &[Event],
    period_start: u32,
) -> NewspaperArticle {
    let template = template_summary(world, faction, events, period_start);

    let mut user = format!(
        "You write for {}. Period: day {} to day {}. Stability {:.0}/100, war support {:.0}/100, regions held {}.\n",
        world.faction(faction).name,
        period_start,
        world.day,
        world.faction(faction).stability,
        world.faction(faction).war_support,
        world.region_count(faction),
    );
    user.push_str("Events this period:\n");
    if events.is_empty() {
        user.push_str("(no notable events)\n");
    } else {
        for e in events {
            user.push_str(&format!("- {}\n", event_line(world, e)));
        }
    }

    let request = LlmRequest { system: SYSTEM_PROMPT.to_string(), user, max_output_tokens: 220 };

    match backend.complete(&request) {
        Ok(text) if !text.trim().is_empty() => NewspaperArticle {
            faction,
            period_start,
            period_end: world.day,
            text: truncate_chars(text.trim(), MAX_ARTICLE_CHARS),
            from_backend: true,
        },
        _ => NewspaperArticle { faction, period_start, period_end: world.day, text: template, from_backend: false },
    }
}

/// One article per currently-living faction, for the same reporting period -
/// this is what a headless `--newspaper` "issue" actually consists of.
pub fn generate_issue<B: LlmBackend>(
    backend: &B,
    world: &World,
    events: &[Event],
    period_start: u32,
) -> Vec<NewspaperArticle> {
    world
        .factions
        .iter()
        .filter(|f| f.alive)
        .map(|f| generate_article(backend, world, f.id, events, period_start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipelago_sim::ids::RegionId;
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
        let article = generate_article(&backend, &world, faction, &events, 0);

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

        let article = generate_article(&backend, &world, faction, &[], 0);
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

        let article = generate_article(&backend, &world, faction, &[], 0);
        assert!(article.from_backend);
        assert_eq!(article.text, "Our forces made a bold strategic redeployment today.");
    }

    /// `template_article_reflects_actual_events`: the fallback template is
    /// event-driven, not canned - an issue generated over a period
    /// containing a specific capture and a specific battle names both, and
    /// an issue over a quiet period claims neither happened. Guards against
    /// exactly the regression this fix addresses: headless previously wired
    /// `--newspaper` to a fixed rotation of English flavor text
    /// (`newspaper_mock_backend` in `apps/headless/src/main.rs`) that never
    /// read `events`/`world` at all.
    #[test]
    fn template_article_reflects_actual_events() {
        let world = scenario::build_world();
        let faction = FactionId(0);
        let backend = MockBackend::always_err(LlmError::Unavailable);

        let captured_region = RegionId(4); // 信越・北陸, owned by faction 1 at scenario start
        let battle_region = RegionId(1); // 北東北, owned by faction 0
        let events = vec![
            Event::RegionCaptured { region: captured_region, from: FactionId(1), to: faction },
            Event::Battle { region: battle_region, factions: vec![faction, FactionId(1)], casualties: 3.5, sides: vec![] },
        ];

        let busy = generate_article(&backend, &world, faction, &events, 0);
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

        let quiet = generate_article(&backend, &world, faction, &[], 30);
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

        let issue = generate_issue(&backend, &world, &[], 0);
        assert_eq!(issue.len(), 2);
        assert!(issue.iter().all(|a| a.faction != FactionId(2)));
    }
}
