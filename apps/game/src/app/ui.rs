//! Updates the text content of every UI panel from the current `SimRes`
//! state - date/scenario/speed (top bar), the selected faction's summary
//! (left), the event log (bottom), and the selected region's detail
//! (right, only while `SelectedRegion` is `Some`). Read-only against
//! `SimRes`, same as `visuals` - see that module's doc.

use bevy::prelude::*;

use archipelago_sim::balance::{
    CAPITAL_FLIGHT_THRESHOLD, MUTINY_THRESHOLD, PROTEST_THRESHOLD, REGIME_CHANGE_THRESHOLD, SEPARATISM_THRESHOLD, STRIKE_THRESHOLD, UNIT_MANPOWER,
};
use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::group::{Group, ALL_GROUPS};
use archipelago_sim::naval::is_port_blockaded;
use archipelago_sim::world::Station;

use super::input::MENU_ITEMS;
use super::{
    EventLog, EventLogText, FactionPanelText, InspectText, LastRejection, MenuRegion, NewspaperPanelText, NewspaperState, PlayerFaction,
    PlayerPanelText, ScenarioMeta, SelectedFaction, SelectedRegion, SelectedUnits, SimRes, SpeedRes, TopBarPlayerStatsText, TopBarText,
};

/// Usability fix (play-test finding #2 - "nothing tells the player when a
/// number is in trouble"): a suffix appended right after a value that has
/// crossed a threshold `crates/sim/src/balance.rs` already branches on for
/// a real in-game consequence, named here. Never invents a parallel number -
/// every constant these call sites use is imported straight from `balance`.
///
/// `stability`/`manpower`/`group_support` all use this; `war_support` and
/// `shortage` do not, because `balance.rs` has no discrete threshold for
/// either (`war_support` only ever drifts toward its 50 baseline -
/// `WAR_SUPPORT_DRIFT`'s own doc; `shortage` feeds unrest continuously via
/// `UNREST_SHORTAGE_PRESSURE`, with no on/off point) - marking either would
/// mean inventing a number the simulation itself does not act on, which
/// this fix deliberately does not do.
fn stability_marker(stability: f32) -> &'static str {
    if stability < REGIME_CHANGE_THRESHOLD {
        "  ※政権崩壊の危険"
    } else {
        ""
    }
}

/// `UNIT_MANPOWER` is exactly the pool `action::apply_recruit` requires
/// before it will accept `RecruitUnit` at all (`panels::recruit_reason`
/// mirrors the same check for the click-to-recruit button) - below it, the
/// faction cannot rebuild its military no matter how the player orders it.
fn manpower_marker(manpower: f32) -> &'static str {
    if manpower < UNIT_MANPOWER {
        "  ※徴兵不能"
    } else {
        ""
    }
}

/// Per-`Group` political-event threshold (`balance.rs`'s "political event
/// thresholds" section, `politics::tick_politics`/`politics::tick_separatism`):
/// each of these five groups gates one specific named event once its
/// support drops below the constant shown. `Government`/`Bureaucracy` gate
/// no event of their own in `balance.rs` and are left unmarked rather than
/// inventing a parallel number for them.
fn group_marker(group: Group, support: f32) -> &'static str {
    match group {
        Group::Military if support < MUTINY_THRESHOLD => "※反乱の危険",
        Group::Labor if support < STRIKE_THRESHOLD => "※ストライキの危険",
        Group::Citizens if support < PROTEST_THRESHOLD => "※暴動の危険",
        Group::Business if support < CAPITAL_FLIGHT_THRESHOLD => "※資本逃避の危険",
        // `politics::tick_separatism`'s own condition: this faction's own
        // LocalGovernment support, not the occupied region's - a low value
        // here risks losing whichever foreign territory this faction
        // currently holds back to its original owner.
        Group::LocalGovernment if support < SEPARATISM_THRESHOLD => "※分離独立の危険",
        _ => "",
    }
}

pub(super) fn update_top_bar(sim: Res<SimRes>, speed: Res<SpeedRes>, meta: Res<ScenarioMeta>, mut query: Query<&mut Text, With<TopBarText>>) {
    let Ok(mut text) = query.single_mut() else { return };
    let world = sim.0.world();
    let status = if speed.paused { "paused".to_string() } else { format!("running ({})", speed.last_active.label()) };
    text.0 = format!("{}  —  day {} / {}  —  {status}", meta.name, world.day, meta.max_days);
}

/// Stage 8B: the player faction's own key figures, always visible in the top
/// bar (docs/design.md §16 owner ask - stockpiles/manpower/stability/war
/// support/shortage at a glance) rather than only reachable by Tab-cycling
/// `FactionPanelText` to land on the player's own faction. Empty text
/// whenever no faction was `--play`ed.
pub(super) fn update_top_bar_player_stats(sim: Res<SimRes>, player: Res<PlayerFaction>, mut query: Query<&mut Text, With<TopBarPlayerStatsText>>) {
    let Ok(mut text) = query.single_mut() else { return };
    let Some(player_faction) = player.0 else {
        text.0 = String::new();
        return;
    };
    let world = sim.0.world();
    let faction = world.faction(player_faction);
    let stock: Vec<String> = ALL_GOODS.iter().map(|g| format!("{}={:.0}", g.key(), faction.stock[g.index()])).collect();
    text.0 = format!(
        "{}  |  在庫: {}  |  人的資源{:.1}{}  安定度{:.0}{}  戦争支持{:.0}  不足{:.2}",
        faction.name,
        stock.join(" "),
        faction.manpower,
        manpower_marker(faction.manpower),
        faction.stability,
        stability_marker(faction.stability),
        faction.war_support,
        faction.shortage,
    );
}

/// Usability fix (play-test finding #1 - "the faction detail panel does not
/// show the player's own nation"): `SelectedFaction` now starts on the
/// `--play`ed faction (`app::run`'s own `insert_resource` call), so this
/// system shows it from the very first frame without needing a `Tab` press;
/// `Tab` (`input::keyboard_input`, unchanged) still cycles to any other
/// faction from there. `player` here only decides which faction currently
/// gets the "this is you" marker below - it never overrides `selected.0`
/// itself, so cycling away from the player's own faction still works
/// exactly as before.
pub(super) fn update_faction_panel(
    sim: Res<SimRes>,
    selected: Res<SelectedFaction>,
    player: Res<PlayerFaction>,
    mut query: Query<&mut Text, With<FactionPanelText>>,
) {
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

    // Play-test finding #1's "group support" - previously not shown by any
    // panel at all, sim-side or client-side, even though five of its seven
    // `Group`s are exactly what `balance.rs`'s political-event thresholds
    // (`group_marker`'s own doc) key off. `Government`/`Bureaucracy` carry
    // no threshold of their own but are listed anyway, same as every other
    // group, rather than silently dropped from the list.
    let group_summary: Vec<String> = ALL_GROUPS
        .iter()
        .map(|&g| {
            let support = faction.group_support[g.index()];
            format!("{}{:.0}{}", g.label(), support, group_marker(g, support))
        })
        .collect();

    // Which faction this is - always shown, so it's never ambiguous which
    // one is on screen (task ask: "make it obvious ... whether it is the
    // player's"). `【あなたの国】` only when `selected` is exactly the
    // `--play`ed faction; every other case (observing-only, or `Tab`ed away
    // to a different faction) shows nothing extra here.
    let you_marker = if player.0 == Some(faction.id) { "【あなたの国】 " } else { "" };

    text.0 = format!(
        "{you_marker}{}{}  [Tab で他勢力に切替]\n\
         territory: {regions} regions\n\
         units: {units}\n\
         manpower: {:.1}{}\n\
         stock: {}\n\
         stability: {:.0}{}   war support: {:.0}\n\
         civilian ration: {:.2}   shortage: {:.2}\n\
         group support: {}\n\
         national focus: {:?}\n\
         diplomacy:{diplo_lines}",
        if faction.alive { "" } else { "(eliminated) " },
        faction.name,
        faction.manpower,
        manpower_marker(faction.manpower),
        stock_summary.join(", "),
        faction.stability,
        stability_marker(faction.stability),
        faction.war_support,
        faction.civilian_ration,
        faction.shortage,
        group_summary.join("  "),
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

/// Stage 7B's player-facing panel (docs/phase7-spec.md "Stage 7B — 遊ぶ"),
/// now (Stage 8B) a lighter-weight corner summary now that the diplomacy/
/// policy content it used to hold text-only lives in `panels::
/// DiplomacyPanelRoot`/`PolicyPanelRoot` as real buttons: selection state,
/// the units at the inspected region, the right-click recruit/build menu's
/// own key legend (`MenuRegion` - unchanged, still keyboard-only; the region
/// panel's own buttons are `panels::RegionActionPanelRoot`), and every
/// rejected order's reason regardless of which panel issued it (docs/
/// phase7-spec.md "命令の可否を隠さない" - Stage 8B additionally attaches
/// each one to its own issuing panel, but this corner still shows the full
/// list too, so a rejection is never missed just because its panel happens
/// to be closed). Empty text whenever no faction was `--play`ed (Stage 7A
/// observing-only mode).
pub(super) fn update_player_panel(
    sim: Res<SimRes>,
    player: Res<PlayerFaction>,
    selected_region: Res<SelectedRegion>,
    selected_units: Res<SelectedUnits>,
    menu: Res<MenuRegion>,
    rejection: Res<LastRejection>,
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

    // Diplomacy/policy content moved to `panels::DiplomacyPanelRoot`/
    // `PolicyPanelRoot` (Stage 8B, real buttons) - `D`/`P`, or their top-bar
    // buttons, open those instead of anything printed here.

    if !rejection.0.is_empty() {
        out.push_str("!! 却下された命令（全パネル）!!\n");
        for r in &rejection.0 {
            out.push_str(&format!("  - {}\n", r.reason));
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

#[cfg(test)]
mod tests {
    use super::*;

    use archipelago_sim::ids::FactionId;
    use archipelago_sim::scenario;

    use crate::sim_driver::SimDriver;

    /// A `SimRes` with `player` as the live human faction, on the unmodified
    /// MVP scenario - same shape as `panels::tests::player_sim`, duplicated
    /// here (not shared) because that helper is private to `panels`' own
    /// test module, matching how every other test module in this crate
    /// builds its own fixture rather than reaching into another file's
    /// `#[cfg(test)]` items.
    fn player_sim(player: FactionId) -> SimRes {
        SimRes(SimDriver::new_with_player(scenario::build_world(), 1, Some(player), None))
    }

    fn run<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
        let mut system = IntoSystem::into_system(system);
        system.initialize(world);
        system.run((), world).unwrap();
    }

    /// Runs `update_faction_panel` against `world` and returns what it wrote -
    /// `world` must already have its own `FactionPanelText` entity (spawned
    /// once, via `spawn_faction_panel_text` below) plus `SimRes`/
    /// `SelectedFaction`/`PlayerFaction`, exactly like `setup::setup`/
    /// `app::run` leave it. Spawning a *fresh* entity on every call here
    /// instead would leave two `FactionPanelText` entities behind after a
    /// second call within the same test - `update_faction_panel`'s own
    /// `query.single_mut()` then finds more than one and silently no-ops
    /// (returns early), which would make every assertion below pass
    /// vacuously against stale text rather than actually re-running the
    /// system.
    fn faction_panel_text(world: &mut World) -> String {
        run(world, update_faction_panel);
        let mut q = world.query_filtered::<&Text, With<FactionPanelText>>();
        q.iter(world).next().expect("call spawn_faction_panel_text(world) once before this").0.clone()
    }

    fn spawn_faction_panel_text(world: &mut World) {
        world.spawn((Text::new(String::new()), FactionPanelText));
    }

    /// Play-test finding #1's regression guard: `SelectedFaction` set to the
    /// `--play`ed faction (what `app::run` now does at startup, instead of
    /// always `FactionId(0)`) must render *that* faction, marked as the
    /// player's own - not merely "some faction that happens to also be
    /// selected". Checked this fails when broken: temporarily reverted
    /// `you_marker` to always `""` - the first assertion below then fails;
    /// temporarily hardcoded it to always show the marker - the second
    /// assertion (a different, non-`Tab`bed-to faction) then fails instead.
    #[test]
    fn faction_panel_shows_the_players_own_faction_with_a_you_marker() {
        let mut world = World::new();
        let sim = player_sim(FactionId(1));
        let player_name = sim.0.world().faction(FactionId(1)).name.clone();
        let other_name = sim.0.world().faction(FactionId(0)).name.clone();
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(1))));
        world.insert_resource(SelectedFaction(FactionId(1)));
        spawn_faction_panel_text(&mut world);

        let text = faction_panel_text(&mut world);
        assert!(text.contains("【あなたの国】"), "selecting the player's own faction must show the 'this is you' marker, got: {text}");
        assert!(text.contains(&player_name), "must show the player's own faction's name, got: {text}");

        // `Tab` (input::keyboard_input, unchanged by this fix) can still
        // move `SelectedFaction` away from the player - the marker must
        // disappear then, so it's never shown for the wrong faction.
        world.resource_mut::<SelectedFaction>().0 = FactionId(0);
        let text = faction_panel_text(&mut world);
        assert!(!text.contains("【あなたの国】"), "a Tab-cycled-away faction must not carry the player marker, got: {text}");
        assert!(text.contains(&other_name), "must still show whichever faction is actually selected, got: {text}");
    }

    /// Play-test finding #2's regression guard for the panel's numeric
    /// markers: `stability`/`manpower`/each thresholded `Group`'s support
    /// must be marked once they cross the exact `balance.rs` constant named
    /// in each marker function's own doc - not sometime before or after it,
    /// and not for a value that's still safe. Checked this fails when
    /// broken: temporarily changed `stability_marker`'s comparison to
    /// `stability < 0.0` (never true) - the "danger" assertions below then
    /// fail; changed it to always `true` - the "safe" assertions fail
    /// instead.
    #[test]
    fn faction_panel_marks_values_that_cross_their_balance_rs_threshold() {
        // Scenario defaults start every faction comfortably above every
        // threshold below (stability/group support both start well above
        // 40..=50 - `stability_is_weighted_group_support`'s own sim test
        // relies on the same fact) - confirmed unmarked first, on a
        // separate, unmutated `World`, so the later "now marked" assertions
        // are known to be caused by the mutation below, not a
        // coincidentally-already-triggered default.
        let baseline_text = {
            let mut w = World::new();
            w.insert_resource(player_sim(FactionId(0)));
            w.insert_resource(PlayerFaction(Some(FactionId(0))));
            w.insert_resource(SelectedFaction(FactionId(0)));
            spawn_faction_panel_text(&mut w);
            faction_panel_text(&mut w)
        };
        assert!(!baseline_text.contains("危険"), "a freshly-built scenario must start with no marker at all, got: {baseline_text}");
        assert!(!baseline_text.contains("徴兵不能"), "a freshly-built scenario must start able to recruit, got: {baseline_text}");

        let mut world = World::new();
        let mut sim = player_sim(FactionId(0));
        {
            let f = sim.0.sim.world.faction_mut(FactionId(0));
            f.stability = REGIME_CHANGE_THRESHOLD - 1.0;
            f.manpower = UNIT_MANPOWER - 0.1;
            f.group_support[Group::Military.index()] = MUTINY_THRESHOLD - 1.0;
        }
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedFaction(FactionId(0)));
        spawn_faction_panel_text(&mut world);

        let text = faction_panel_text(&mut world);
        assert!(text.contains("※政権崩壊の危険"), "stability below REGIME_CHANGE_THRESHOLD must be marked, got: {text}");
        assert!(text.contains("※徴兵不能"), "manpower below UNIT_MANPOWER must be marked, got: {text}");
        assert!(text.contains("軍部") && text.contains("※反乱の危険"), "Military group support below MUTINY_THRESHOLD must be marked, got: {text}");
    }
}
