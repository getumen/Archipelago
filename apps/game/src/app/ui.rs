//! Updates the text content of every UI panel from the current `SimRes`
//! state - date/scenario/speed (top bar), the selected faction's summary
//! (left), the event log (bottom), and the selected region's detail
//! (right, only while `SelectedRegion` is `Some`). Read-only against
//! `SimRes`, same as `visuals` - see that module's doc.

use bevy::prelude::*;

use archipelago_sim::diplomacy::{Treaty, ALL_TREATIES};
use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::naval::is_port_blockaded;
use archipelago_sim::world::Station;

use super::input::MENU_ITEMS;
use super::{
    ActiveGood, DiplomacyPanel, EventLog, EventLogText, FactionPanelText, InspectText, LastRejection, MenuRegion, NewspaperPanelText,
    NewspaperState, NlCompose, PlayerFaction, PlayerPanelText, ScenarioMeta, SelectedFaction, SelectedRegion, SelectedUnits, SimRes, SpeedRes,
    TopBarText,
};

pub(super) fn update_top_bar(sim: Res<SimRes>, speed: Res<SpeedRes>, meta: Res<ScenarioMeta>, mut query: Query<&mut Text, With<TopBarText>>) {
    let Ok(mut text) = query.single_mut() else { return };
    let world = sim.0.world();
    let status = if speed.paused { "paused".to_string() } else { format!("running ({})", speed.last_active.label()) };
    text.0 = format!("{}  —  day {} / {}  —  {status}", meta.name, world.day, meta.max_days);
}

pub(super) fn update_faction_panel(sim: Res<SimRes>, selected: Res<SelectedFaction>, mut query: Query<&mut Text, With<FactionPanelText>>) {
    let Ok(mut text) = query.single_mut() else { return };
    let world = sim.0.world();
    let Some(faction) = world.factions.get(selected.0.index()) else {
        text.0 = String::new();
        return;
    };

    let regions = world.region_count(faction.id);
    let units = world.units.iter().filter(|u| u.alive && u.owner == faction.id).count();
    let stock_summary: Vec<String> = ALL_GOODS
        .iter()
        .map(|g| format!("{}={:.0}", g.key(), faction.stock[g.index()]))
        .collect();

    let mut diplo_lines = String::new();
    for other in &world.factions {
        if other.id == faction.id || !other.alive {
            continue;
        }
        let stance = world.diplomacy.stance(faction.id, other.id);
        diplo_lines.push_str(&format!("\n    vs {}: {:?} (opinion {:.0})", other.name, stance, world.diplomacy.opinion(faction.id, other.id)));
    }

    text.0 = format!(
        "[Tab to switch]  {} {}\n\
         territory: {regions} regions\n\
         units: {units}\n\
         manpower: {:.1}\n\
         stock: {}\n\
         stability: {:.0}   war support: {:.0}\n\
         civilian ration: {:.2}   shortage: {:.2}\n\
         national focus: {:?}\n\
         diplomacy:{diplo_lines}",
        if faction.alive { "" } else { "(eliminated)" },
        faction.name,
        faction.manpower,
        stock_summary.join(", "),
        faction.stability,
        faction.war_support,
        faction.civilian_ration,
        faction.shortage,
        faction.national_focus,
    );
}

pub(super) fn update_event_log(log: Res<EventLog>, mut query: Query<&mut Text, With<EventLogText>>) {
    if !log.is_changed() {
        return;
    }
    let Ok(mut text) = query.single_mut() else { return };
    text.0 = log.0.iter().cloned().collect::<Vec<_>>().join("\n");
}

pub(super) fn update_inspect_panel(sim: Res<SimRes>, selected: Res<SelectedRegion>, mut query: Query<&mut Text, With<InspectText>>) {
    let Ok(mut text) = query.single_mut() else { return };
    let world = sim.0.world();
    let Some(region_id) = selected.0 else {
        text.0 = String::new();
        return;
    };
    let Some(region) = world.regions.get(region_id.index()) else {
        text.0 = String::new();
        return;
    };

    let owner_name = world.faction(region.owner).name.clone();
    let occupier_line = match region.occupier {
        Some(occ) => format!("\noccupied by {} ({:.0}%)", world.faction(occ).name, region.occupation * 100.0),
        None => String::new(),
    };
    let blockaded = if region.port > 0.0 && is_port_blockaded(world, region.id) { "  (blockaded)" } else { "" };
    let capacity: Vec<String> = ALL_GOODS.iter().map(|g| format!("{}={:.1}", g.key(), region.effective_capacity(*g))).collect();

    text.0 = format!(
        "{}  ({:?})\n\
         owner: {owner_name}{occupier_line}\n\
         population: {:.0}\n\
         capacity: {}\n\
         infrastructure: {:.2}   port: {:.2}{blockaded}\n\
         unrest: {:.2}   devastation: {:.2}\n\
         supply: {:.2}   import flow: {:.2}\n\
         construction: {}",
        region.name,
        region.terrain,
        region.population,
        capacity.join(", "),
        region.infrastructure,
        region.port,
        region.unrest,
        region.devastation,
        world.supply.get(region.id.index()).copied().unwrap_or(0.0),
        region.import_flow,
        region
            .construction
            .as_ref()
            .map(|c| format!("{:?} ({:.0}/{:.0})", c.project, c.invested, c.required))
            .unwrap_or_else(|| "none".to_string()),
    );
}

fn treaty_label_ja(t: Treaty) -> &'static str {
    match t {
        Treaty::Ceasefire => "停戦",
        Treaty::NonAggression => "不可侵条約",
        Treaty::Alliance => "同盟",
        Treaty::MilitaryAccess => "通行権",
        Treaty::PortAccess => "港湾利用",
        Treaty::TradeAgreement => "貿易協定",
    }
}

/// Stage 7B's player-facing panel (docs/phase7-spec.md "Stage 7B — 遊ぶ"):
/// selection state and the units at the inspected region, the recruit/
/// build menu, the diplomacy panel, current policy values, and the most
/// recent rejection reasons - see `super::input` for what drives each
/// resource this reads. Empty text whenever no faction was `--play`ed
/// (Stage 7A observing-only mode).
#[allow(clippy::too_many_arguments)]
pub(super) fn update_player_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    selected_region: Res<SelectedRegion>,
    selected_units: Res<SelectedUnits>,
    menu: Res<MenuRegion>,
    diplomacy: Res<DiplomacyPanel>,
    active_good: Res<ActiveGood>,
    rejection: Res<LastRejection>,
    nl_compose: Res<NlCompose>,
    event_log: Res<EventLog>,
    mut query: Query<&mut Text, With<PlayerPanelText>>,
) {
    let Ok(mut text) = query.single_mut() else { return };
    let Some(player_faction) = player.0 else {
        text.0 = String::new();
        return;
    };
    let world = sim.0.world();
    let faction = world.faction(player_faction);

    let mut out = format!("== プレイヤー: {} ==\n", faction.name);

    // Units at the currently-inspected region, when it's the player's own -
    // docs/phase7-spec.md "自国地域をクリック: 選択。その地域の部隊一覧を表示".
    if let Some(region_id) = selected_region.0
        && world.region(region_id).owner == player_faction
    {
        let mut units: Vec<_> = world.units.iter().filter(|u| u.alive && u.owner == player_faction && u.station == Station::Region(region_id)).collect();
        units.sort_by_key(|u| u.id.0);
        if units.is_empty() {
            out.push_str("この地域に部隊はいない\n");
        } else {
            out.push_str("この地域の部隊（クリックで選択/解除）:\n");
            for u in units {
                let mark = if selected_units.0.contains(&u.id.0) { "*" } else { " " };
                out.push_str(&format!("  {mark}#{} {} 兵力{:.0} 装備{:.0} 組織{:.0}\n", u.id.0, u.name, u.manpower, u.equipment, u.organization));
            }
        }
    }
    out.push_str(&format!(
        "選択部隊: {} 隊 (地域/海域をクリックで移動)\n",
        selected_units.0.len()
    ));

    if let Some(region_id) = menu.0 {
        out.push_str(&format!("-- 命令メニュー: {} (Esc で閉じる) --\n", world.region(region_id).name));
        for (i, item) in MENU_ITEMS.iter().enumerate() {
            out.push_str(&format!("{}: {item}\n", i + 1));
        }
    }

    if diplomacy.open {
        out.push_str("-- 外交パネル (D/Esc で閉じる, V で対象切替) --\n");
        if let Some(target) = diplomacy.target {
            let tf = world.faction(target);
            out.push_str(&format!(
                "対象: {}   関係: {:?}   感情: {:.0}\n",
                tf.name,
                world.diplomacy.stance(player_faction, target),
                world.diplomacy.opinion(player_faction, target)
            ));
            for (i, &treaty) in ALL_TREATIES.iter().enumerate() {
                out.push_str(&format!("{}:{} ", i + 1, treaty_label_ja(treaty)));
            }
            out.push_str("\nA:受諾 R:拒否 W:宣戦 B:破棄 T:自然言語で提案\n");
            let incoming: Vec<_> = world.diplomacy.pending.iter().filter(|p| p.from == target && p.to == player_faction).collect();
            if incoming.is_empty() {
                out.push_str("相手からの提案: なし\n");
            } else {
                for p in incoming {
                    out.push_str(&format!("相手からの提案: {}\n", treaty_label_ja(p.treaty)));
                }
            }

            // Stage 7C (docs/phase7-spec.md "4."): this faction's own
            // outstanding natural-language proposal to `target`, if any -
            // the compose box below is disabled while one is still pending
            // (`action::apply_propose_nl` would reject a second one anyway;
            // showing it here is the honest reason why, not a silent no-op).
            if let Some(pending) = world.diplomacy.pending_nl.iter().find(|p| p.from == player_faction && p.to == target) {
                out.push_str(&format!("自国からの自然言語提案（返答待ち）: 「{}」\n", pending.text));
            }

            if nl_compose.active {
                out.push_str(&format!("自然言語提案を入力中> {}_\n(Enter で送信, Esc でキャンセル)\n", nl_compose.buffer));
            }

            // The interpreted `TreatyTerm`s and accept/reject verdict for
            // any natural-language proposal that has resolved recently -
            // `event_text::format_event` already spells both out
            // (`Event::NaturalLanguage{Accepted,Rejected,TermsInvalid}::terms`),
            // so this reads straight off the same event log the bottom
            // panel shows rather than keeping a second copy of the same
            // data.
            let nl_history: Vec<&String> = event_log.0.iter().filter(|line| line.contains("自然言語外交")).take(3).collect();
            if !nl_history.is_empty() {
                out.push_str("-- 自然言語外交の履歴 --\n");
                for line in nl_history {
                    out.push_str(&format!("{line}\n"));
                }
            }
        } else {
            out.push_str("対象となる勢力がいない\n");
        }
    }

    out.push_str(&format!(
        "-- 政策 (対象品目 [G で切替]: {}) --\n\
         徴兵率[-/=]: {:.2}   配給率[\u{5b}/\u{5d}]: {:.2}\n\
         生産優先度[;/']: {:.2}   物流優先度[,/.]: {:.2}   輸入計画[8/9]: {:.1}\n\
         国家方針[F]: {}\n",
        active_good.0.label(),
        faction.conscription,
        faction.civilian_ration,
        faction.industry_priority[active_good.0.index()],
        faction.logistics_priority[active_good.0.index()],
        faction.import_plan[active_good.0.index()],
        faction.national_focus.label(),
    ));

    if !rejection.0.is_empty() {
        out.push_str("!! 却下された命令 !!\n");
        for reason in &rejection.0 {
            out.push_str(&format!("  - {reason}\n"));
        }
    }

    text.0 = out;
}

/// Stage 7C's newspaper panel (`N` to toggle, docs/phase7-spec.md "5. 新聞"):
/// shows the currently-viewed issue's article for whichever faction the
/// left-hand faction panel is currently showing (`SelectedFaction`, `Tab`
/// to cycle) - one panel, one faction, exactly like the faction summary it
/// sits next to. Empty text whenever the panel is closed; an explicit "no
/// issue yet" line (never a placeholder article) before
/// `NEWSPAPER_INTERVAL_DAYS` has elapsed once.
pub(super) fn update_newspaper_panel(
    news: Res<NewspaperState>,
    selected: Res<SelectedFaction>,
    mut query: Query<&mut Text, With<NewspaperPanelText>>,
) {
    let Ok(mut text) = query.single_mut() else { return };
    if !news.open {
        text.0 = String::new();
        return;
    }
    let Some(issue_index) = news.viewing.or_else(|| news.history.len().checked_sub(1)) else {
        text.0 = "-- 新聞 (N で閉じる) --\nまだ号外は発行されていない\n".to_string();
        return;
    };
    let issue = &news.history[issue_index];

    let mut out = format!(
        "-- 新聞 (N で閉じる, \u{2190}/\u{2192} で号を送る) -- 第{}号 (day {}\u{301c}{}) --\n",
        issue_index + 1,
        issue.period_start,
        issue.period_end,
    );
    match issue.articles.iter().find(|a| a.faction == selected.0) {
        Some(article) => out.push_str(&format!("{}\n", article.text)),
        // The faction the left panel is currently showing was eliminated
        // and dropped out of this issue's per-faction articles
        // (`newspaper::generate_issue` only covers `f.alive` factions) -
        // say so, never show a stale or blank article.
        None => out.push_str("この勢力の記事はない（脱落済み）\n"),
    }
    text.0 = out;
}
