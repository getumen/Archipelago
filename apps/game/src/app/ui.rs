//! Updates the text content of every UI panel from the current `SimRes`
//! state - date/scenario/speed (top bar), the selected faction's summary
//! (left), the event log (bottom), and the selected region's detail
//! (right, only while `SelectedRegion` is `Some`). Read-only against
//! `SimRes`, same as `visuals` - see that module's doc.

use bevy::prelude::*;

use archipelago_sim::balance::{
    CAPITAL_FLIGHT_THRESHOLD, GROUP_SHORTAGE_CITIZENS_PENALTY, GROUP_SHORTAGE_GOVERNMENT_PENALTY, GROUP_SHORTAGE_LABOR_PENALTY, MUTINY_THRESHOLD,
    PROTEST_THRESHOLD, REGIME_CHANGE_THRESHOLD, SEPARATISM_THRESHOLD, STRIKE_THRESHOLD, UNIT_MANPOWER,
};
use archipelago_sim::good::{Good, ALL_GOODS};
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
/// support drops below the constant returned. `Government`/`Bureaucracy`
/// gate no event of their own in `balance.rs` and get `None` rather than an
/// invented parallel number.
fn group_threshold(group: Group) -> Option<f32> {
    match group {
        Group::Military => Some(MUTINY_THRESHOLD),
        Group::Labor => Some(STRIKE_THRESHOLD),
        Group::Citizens => Some(PROTEST_THRESHOLD),
        Group::Business => Some(CAPITAL_FLIGHT_THRESHOLD),
        // `politics::tick_separatism`'s own condition: this faction's own
        // LocalGovernment support, not the occupied region's - a low value
        // here risks losing whichever foreign territory this faction
        // currently holds back to its original owner.
        Group::LocalGovernment => Some(SEPARATISM_THRESHOLD),
        Group::Government | Group::Bureaucracy => None,
    }
}

fn group_event_name(group: Group) -> &'static str {
    match group {
        Group::Military => "反乱",
        Group::Labor => "ストライキ",
        Group::Citizens => "暴動",
        Group::Business => "資本逃避",
        Group::LocalGovernment => "分離独立",
        Group::Government | Group::Bureaucracy => "",
    }
}

/// `balance.rs`'s `GROUP_SHORTAGE_*_PENALTY` constants
/// (`politics::tick_politics`: `target[g] -= GROUP_SHORTAGE_*_PENALTY *
/// shortage_f`) - `None` for the four groups `Faction::shortage` does not
/// touch at all (Military/Business/LocalGovernment/Bureaucracy).
fn group_shortage_penalty(group: Group) -> Option<f32> {
    match group {
        Group::Citizens => Some(GROUP_SHORTAGE_CITIZENS_PENALTY),
        Group::Labor => Some(GROUP_SHORTAGE_LABOR_PENALTY),
        Group::Government => Some(GROUP_SHORTAGE_GOVERNMENT_PENALTY),
        Group::LocalGovernment | Group::Bureaucracy | Group::Military | Group::Business => None,
    }
}

/// Play-test finding (this task - "the *cause* is invisible, so a player
/// learns only when the strike happens, and cannot connect it to the
/// decision that caused it 200 days earlier"): the previous pass's
/// `group_marker` only announced a threshold already crossed. This now
/// additionally names, for every group `group_threshold` covers, the exact
/// point margin remaining before that happens (`残N`) - the "five points
/// from its strike threshold and falling" moment the task asks to make
/// legible - and, for every group `group_shortage_penalty` covers, exactly
/// how many of those remaining points `Faction::shortage` is spending right
/// now (`不足-N.N`), computed straight from the same `balance.rs` constant
/// `politics::tick_politics` itself multiplies by `shortage_f` - never a
/// parallel/invented number.
///
/// Below threshold this still emits the *exact* marker text play-test
/// finding #2 added (`※反乱の危険` etc.) as a substring, so
/// `faction_panel_marks_values_that_cross_their_balance_rs_threshold` (which
/// greps for it verbatim) keeps passing unchanged.
fn group_annotation(group: Group, support: f32, shortage: f32) -> String {
    let mut parts = Vec::new();
    if let Some(threshold) = group_threshold(group) {
        let margin = support - threshold;
        if margin < 0.0 {
            parts.push(format!("※{}の危険", group_event_name(group)));
        } else {
            parts.push(format!("残{margin:.0}"));
        }
    }
    if let Some(penalty) = group_shortage_penalty(group) {
        let cost = penalty * shortage.clamp(0.0, 1.0);
        if cost > 0.0 {
            parts.push(format!("不足-{cost:.1}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("({})", parts.join(" "))
    }
}

/// Play-test finding (this task): a commodity stock `economy::tick_economy`'s
/// `consume()` has floored at exactly `0.0` this tick - production plus
/// carryover could not cover ration-adjusted demand - is a qualitatively
/// different state from a merely low but still-positive stock (design.md
/// §2/§9's 物不足 chain starts here). Only `Food`/`Energy`/`Machinery` are
/// marked: the three goods `Faction::shortage_by_good` actually tracks -
/// Steel/Munitions/Arms hitting zero has no equivalent civilian-facing
/// political consequence to flag.
fn stock_marker(good: Good, stock: f32) -> &'static str {
    let civilian_good = matches!(good, Good::Food | Good::Energy | Good::Machinery);
    if civilian_good && stock <= 0.0 {
        "※枯渇"
    } else {
        ""
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
    let stock: Vec<String> = ALL_GOODS
        .iter()
        .map(|g| format!("{}={:.0}{}", g.key(), faction.stock[g.index()], stock_marker(*g, faction.stock[g.index()])))
        .collect();
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
        .map(|g| format!("{}={:.0}{}", g.key(), faction.stock[g.index()], stock_marker(*g, faction.stock[g.index()])))
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
    // (`group_threshold`'s own doc) key off. `Government`/`Bureaucracy` carry
    // no threshold of their own but are listed anyway, same as every other
    // group, rather than silently dropped from the list. This task's
    // `group_annotation` additionally names each group's margin to its own
    // threshold and, where `balance::GROUP_SHORTAGE_*_PENALTY` applies to it,
    // exactly how much of that margin today's `shortage` is spending.
    let group_summary: Vec<String> = ALL_GROUPS
        .iter()
        .map(|&g| {
            let support = faction.group_support[g.index()];
            format!("{}{:.0}{}", g.label(), support, group_annotation(g, support, faction.shortage))
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

    /// This task's regression guard: a civilian good whose stock
    /// `economy::tick_economy` has floored at exactly `0.0` must be flagged
    /// distinctly from a merely low stock, in both the always-visible top
    /// bar and the faction detail panel - and a non-civilian good hitting
    /// zero (Steel here) must *not* get the same flag, since
    /// `Faction::shortage_by_good` never tracks it. Checked this fails when
    /// broken: temporarily made `stock_marker` always return `""` - the
    /// `枯渇` assertions below then fail; temporarily made it ignore the
    /// `civilian_good` check - the "must not" assertion fails instead. Both
    /// reverted before committing.
    #[test]
    fn depleted_civilian_stock_is_flagged_distinctly_from_a_depleted_industrial_one() {
        fn deplete(sim: &mut SimRes) {
            let f = sim.0.sim.world.faction_mut(FactionId(0));
            f.stock[archipelago_sim::good::Good::Food.index()] = 0.0;
            f.stock[archipelago_sim::good::Good::Steel.index()] = 0.0;
        }

        let mut top_bar_sim = player_sim(FactionId(0));
        deplete(&mut top_bar_sim);
        let mut top_bar_world = World::new();
        top_bar_world.insert_resource(top_bar_sim);
        top_bar_world.insert_resource(PlayerFaction(Some(FactionId(0))));
        top_bar_world.spawn((Text::new(String::new()), TopBarPlayerStatsText));
        run(&mut top_bar_world, update_top_bar_player_stats);
        let top_bar_text = {
            let mut q = top_bar_world.query_filtered::<&Text, With<TopBarPlayerStatsText>>();
            q.iter(&top_bar_world).next().unwrap().0.clone()
        };
        assert!(top_bar_text.contains("food=0※枯渇"), "a depleted civilian good must be flagged in the top bar, got: {top_bar_text}");
        assert!(!top_bar_text.contains("steel=0※枯渇"), "a depleted non-civilian good must not be flagged the same way, got: {top_bar_text}");

        let mut panel_sim = player_sim(FactionId(0));
        deplete(&mut panel_sim);
        let mut panel_world = World::new();
        panel_world.insert_resource(panel_sim);
        panel_world.insert_resource(PlayerFaction(Some(FactionId(0))));
        panel_world.insert_resource(SelectedFaction(FactionId(0)));
        spawn_faction_panel_text(&mut panel_world);
        let panel_text = faction_panel_text(&mut panel_world);
        assert!(panel_text.contains("food=0※枯渇"), "the faction detail panel must also flag the depleted good, got: {panel_text}");
        assert!(!panel_text.contains("steel=0※枯渇"), "the faction detail panel must not flag a depleted non-civilian good, got: {panel_text}");
    }

    /// This task's regression guard for "what the shortage is currently
    /// costing": the group support line must name exactly the
    /// `balance::GROUP_SHORTAGE_*_PENALTY * shortage` product
    /// `politics::tick_politics` itself subtracts from each affected group's
    /// target, right next to that group's own support number - not a
    /// parallel/invented figure. Checked this fails when broken: temporarily
    /// made `group_shortage_penalty` always return `None` - the `不足-N.N`
    /// assertions below then fail. Reverted before committing.
    #[test]
    fn group_support_names_its_own_shortage_cost() {
        let mut world = World::new();
        let mut sim = player_sim(FactionId(0));
        {
            let f = sim.0.sim.world.faction_mut(FactionId(0));
            f.shortage = 0.10;
        }
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedFaction(FactionId(0)));
        spawn_faction_panel_text(&mut world);

        let text = faction_panel_text(&mut world);
        assert!(text.contains("市民") && text.contains("不足-2.0"), "Citizens' shortage cost (GROUP_SHORTAGE_CITIZENS_PENALTY * shortage) must be shown, got: {text}");
        assert!(text.contains("労働者") && text.contains("不足-1.0"), "Labor's shortage cost must be shown, got: {text}");
        assert!(text.contains("中央政府") && text.contains("不足-0.8"), "Government's shortage cost must be shown, got: {text}");
        assert!(!text.contains("軍部(不足"), "Military support carries no shortage penalty in balance.rs and must not show one, got: {text}");
    }

    /// This task's regression guard for the "proximity to threshold" part of
    /// the fix: a group still *above* its `balance.rs` threshold must show
    /// how many points of margin remain, not just silence until the moment
    /// it crosses - the "five points from its strike threshold and falling"
    /// moment the task names. Checked this fails when broken: temporarily
    /// made `group_annotation` skip the `margin >= 0.0` branch entirely
    /// (returning `String::new()` there) - the `残5` assertions below then
    /// fail. Reverted before committing.
    #[test]
    fn faction_panel_shows_margin_to_threshold_before_it_is_crossed() {
        let mut world = World::new();
        let mut sim = player_sim(FactionId(0));
        {
            let f = sim.0.sim.world.faction_mut(FactionId(0));
            f.group_support[Group::Labor.index()] = STRIKE_THRESHOLD + 5.0;
            f.group_support[Group::Citizens.index()] = PROTEST_THRESHOLD + 5.0;
            f.shortage = 0.10;
        }
        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedFaction(FactionId(0)));
        spawn_faction_panel_text(&mut world);

        let text = faction_panel_text(&mut world);
        assert!(!text.contains("危険"), "a group still above its threshold must not show the crossed-threshold marker, got: {text}");
        assert!(text.contains("残5"), "a group 5 points above its threshold must show the remaining margin, got: {text}");
        assert!(text.contains("不足-1.0"), "Labor's shortage cost must be shown alongside its margin, got: {text}");
        assert!(text.contains("不足-2.0"), "Citizens' shortage cost must be shown alongside its margin, got: {text}");
    }

    /// Scenario-acceptance regression guard (see
    /// `apps/headless/tests/scenario_acceptance.rs`'s own module doc for the
    /// rest of this suite): today's `Tab` handler
    /// (`input::keyboard_input`'s `KeyCode::Tab` arm) cycles the faction
    /// panel through *every* faction, alive or not - `(selected_faction.0.0
    /// + 1) % n` with no `alive` filter - so an accidental Tab press late in
    /// a long game can easily land the panel on a faction that was
    /// eliminated hundreds of days ago. It must say so plainly, not quietly
    /// show that faction's frozen pre-elimination numbers as though it were
    /// still playing - a real play-test finding (a faction panel that, by
    /// day 719, was silently still showing an already-eliminated faction).
    /// `update_faction_panel`'s own `if faction.alive {...}` guard already
    /// covers this; this test is the regression guard that keeps it that
    /// way.
    ///
    /// Runs a full `mvp` seed-1 game to completion (it ends well inside a
    /// second - `sim_driver::tests::client_run_matches_headless`'s own doc
    /// notes it ends in a day-366 `Victory`, eliminating two of the three
    /// factions along the way) rather than hand-constructing an eliminated
    /// `Faction`, so this exercises the real elimination path
    /// (`military`/`politics` systems setting `alive = false`), not a
    /// fixture standing in for it.
    ///
    /// Checked this fails when broken: temporarily changed
    /// `update_faction_panel`'s `if faction.alive {"" } else {"(eliminated)
    /// "}` to always the empty-string arm - the first assertion below then
    /// fails, showing a blank-frozen panel with no elimination marker at
    /// all. Reverted before committing.
    #[test]
    fn faction_panel_marks_an_eliminated_faction_as_eliminated_not_silently_frozen() {
        let mut world = World::new();
        // Plain `SimDriver::new` (every faction `HeuristicAgent`, nobody
        // passive) rather than `player_sim` - a `--play`ed faction is driven
        // by `HumanAgent`, which never acts on its own with nothing pushed
        // to it, and that alone is enough to change mvp's whole war outcome
        // (a passive faction 0 does not fight back the way the AI baseline
        // does). This test only needs *some* faction to actually reach
        // elimination through the real elimination path, which needs every
        // faction actually playing.
        let mut sim = SimRes(SimDriver::new(scenario::build_world(), 1));
        while sim.0.outcome(720) == archipelago_sim::sim::Outcome::Ongoing {
            sim.0.tick();
        }
        let eliminated = sim
            .0
            .world()
            .factions
            .iter()
            .find(|f| !f.alive)
            .map(|f| f.id)
            .expect("mvp seed 1 must eliminate at least one faction by the time the game ends");
        let eliminated_name = sim.0.world().faction(eliminated).name.clone();

        world.insert_resource(sim);
        world.insert_resource(PlayerFaction(Some(FactionId(0))));
        world.insert_resource(SelectedFaction(eliminated));
        spawn_faction_panel_text(&mut world);

        let text = faction_panel_text(&mut world);
        assert!(text.contains("(eliminated)"), "an eliminated faction's panel must say so plainly, got: {text}");
        assert!(text.contains(&eliminated_name), "must still name which faction this is, not go blank, got: {text}");
    }
}
