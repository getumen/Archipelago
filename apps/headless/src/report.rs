//! Human-readable console output: day-by-day events, periodic faction
//! summaries, and the final board (mvp-spec.md §8). Region and faction
//! names come straight from the scenario data and are Japanese; the
//! surrounding labels are Japanese too so the two read as one table.

use archipelago_agents::newspaper::NewspaperArticle;
use archipelago_sim::diplomacy::Treaty;
use archipelago_sim::event::Event;
use archipelago_sim::focus;
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::group::ALL_GROUPS;
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::{Domain, Station, World};

/// Japanese label for a `Treaty`, used by `print_event`'s Stage 3B lines.
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

/// Terminal columns count double-width for non-ASCII (CJK) characters, so a
/// plain `.len()`-based pad leaves Japanese names looking ragged; this
/// approximates the visual width instead.
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

fn pad_right(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    for _ in display_width(s)..width {
        out.push(' ');
    }
    out
}

pub fn print_header(args_seed: u64, args_days: u32, args_report: u32) {
    println!(
        "=== archipelago-headless: seed={args_seed} days={args_days} report={args_report} ==="
    );
}

pub fn print_event(world: &World, day: u32, event: &Event) {
    let line = match event {
        Event::Battle { region, factions, casualties } => {
            let region_name = &world.region(*region).name;
            let names: Vec<&str> = factions.iter().map(|f| world.faction(*f).name.as_str()).collect();
            format!(
                "戦闘: {} で {} が交戦 (損耗 {:.2}万人)",
                region_name,
                names.join(" vs "),
                casualties
            )
        }
        Event::NavalBattle { zone, factions, casualties } => {
            let zone_name = &world.sea_zone(*zone).name;
            let names: Vec<&str> = factions.iter().map(|f| world.faction(*f).name.as_str()).collect();
            format!(
                "海戦: {} で {} が交戦 (損耗 {:.2}万人)",
                zone_name,
                names.join(" vs "),
                casualties
            )
        }
        Event::UnitDestroyed { unit, station, owner } => match station {
            Station::Region(region) => format!(
                "部隊壊滅: {} 軍 部隊#{} が {} で失われた",
                world.faction(*owner).name,
                unit.0,
                world.region(*region).name
            ),
            Station::Sea(zone) => format!(
                "艦隊撃沈: {} 軍 部隊#{} が {} で撃沈された",
                world.faction(*owner).name,
                unit.0,
                world.sea_zone(*zone).name
            ),
        },
        Event::RegionCaptured { region, from, to } => {
            format!(
                "占領: {} を {} が {} から奪取",
                world.region(*region).name,
                world.faction(*to).name,
                world.faction(*from).name
            )
        }
        Event::FactionEliminated { faction } => {
            format!("敗北: {} が全領土を失い脱落", world.faction(*faction).name)
        }
        Event::Strike { faction } => {
            format!("ストライキ: {} で労働者がストライキ開始 (工業生産低下)", world.faction(*faction).name)
        }
        Event::Protest { faction } => {
            format!("デモ: {} で市民デモが拡大 (治安悪化)", world.faction(*faction).name)
        }
        Event::Mutiny { faction } => {
            format!("軍部不服従: {} で軍の統制が乱れる (組織率回復低下)", world.faction(*faction).name)
        }
        Event::CapitalFlight { faction } => format!(
            "資本逃避: {} で資本が流出 (建設・機械生産低下)",
            world.faction(*faction).name
        ),
        Event::RegimeChange { faction } => format!(
            "政権交代: {} で政権が崩壊、政策が既定値に戻る",
            world.faction(*faction).name
        ),
        Event::Separatism { region, from, to } => format!(
            "地方独立運動: {} が {} から {} へ復帰",
            world.region(*region).name,
            world.faction(*from).name,
            world.faction(*to).name
        ),
        Event::TreatyProposed { from, to, treaty } => format!(
            "外交提案: {} が {} に {} を提案",
            world.faction(*from).name,
            world.faction(*to).name,
            treaty_label(*treaty),
        ),
        Event::TreatySigned { a, b, treaty } => format!(
            "条約締結: {} と {} が {} を締結",
            world.faction(*a).name,
            world.faction(*b).name,
            treaty_label(*treaty),
        ),
        Event::TreatyRejected { from, to, treaty } => format!(
            "外交拒否: {} が {} の {} 提案を拒否",
            world.faction(*to).name,
            world.faction(*from).name,
            treaty_label(*treaty),
        ),
        Event::TreatyBroken { a, b, treaty } => format!(
            "条約破棄: {} が {} との {} を破棄",
            world.faction(*a).name,
            world.faction(*b).name,
            treaty_label(*treaty),
        ),
        Event::WarDeclared { a, b } => format!(
            "宣戦布告: {} が {} に宣戦",
            world.faction(*a).name,
            world.faction(*b).name,
        ),
        Event::AllianceDragIn { faction, into_war_with } => format!(
            "同盟参戦: {} が同盟により {} との戦争に参戦",
            world.faction(*faction).name,
            world.faction(*into_war_with).name,
        ),
        Event::NaturalLanguageProposed { from, to, text } => format!(
            "自然言語外交: {} が {} に提案 「{}」",
            world.faction(*from).name,
            world.faction(*to).name,
            text,
        ),
        Event::NaturalLanguageAccepted { from, to, terms } => format!(
            "自然言語外交・成立: {} が {} の提案を受諾 ({}件)",
            world.faction(*to).name,
            world.faction(*from).name,
            terms.len(),
        ),
        Event::NaturalLanguageRejected { from, to, terms } => format!(
            "自然言語外交・拒否: {} が {} の提案を拒否 ({}件)",
            world.faction(*to).name,
            world.faction(*from).name,
            terms.len(),
        ),
        Event::NaturalLanguageTermsInvalid { from, to, terms } => format!(
            "自然言語外交・不成立: {} が {} の提案を受諾しようとしたが条件が満たせず不成立 ({}件)",
            world.faction(*to).name,
            world.faction(*from).name,
            terms.len(),
        ),
    };
    println!("[day {day:4}] {line}");
}

pub fn print_faction_table(world: &World) {
    println!();
    println!("--- 勢力サマリ (day {}) ---", world.day);
    println!(
        "{}  領土  部隊  艦隊   人的資源  補給率  安定度  戦意  不足率  配給率",
        pad_right("勢力", 10)
    );
    for faction in &world.factions {
        let status = if faction.alive { "" } else { "(脱落)" };
        let regions = world.region_count(faction.id);
        let units = world
            .units
            .iter()
            .filter(|u| u.alive && u.owner == faction.id && u.station.domain() == Domain::Land)
            .count();
        let fleets = world
            .units
            .iter()
            .filter(|u| u.alive && u.owner == faction.id && u.station.domain() == Domain::Sea)
            .count();
        println!(
            "{}  {:4}  {:4}  {:4}  {:8.2}  {:6.1}%  {:6.1}  {:5.1}  {:5.1}%  {:5.1}%{status}",
            pad_right(&faction.name, 10),
            regions,
            units,
            fleets,
            faction.manpower,
            faction.supply_ratio * 100.0,
            faction.stability,
            faction.war_support,
            faction.shortage * 100.0,
            faction.civilian_ration * 100.0,
        );
        let stock_line: Vec<String> = ALL_GOODS
            .iter()
            .map(|g| format!("{}={:.1}", g.label(), faction.stock[g.index()]))
            .collect();
        println!("  {}  在庫: {}", pad_right("", 8), stock_line.join(" "));

        // Stage 3A (docs/phase3-spec.md "Stage 3A"): support for each of the
        // seven domestic political groups.
        let group_line: Vec<String> = ALL_GROUPS
            .iter()
            .map(|g| format!("{}={:.1}", g.label(), faction.group_support[g.index()]))
            .collect();
        println!("  {}  支持: {}", pad_right("", 8), group_line.join(" "));

        // Stage 3C (docs/phase3-spec.md "Stage 3C — 国家方針"): the current
        // national focus and whether its transition has settled.
        let focus_status = if focus::active(faction).is_some() {
            "有効"
        } else {
            "移行中"
        };
        println!(
            "  {}  方針: {} ({})",
            pad_right("", 8),
            faction.national_focus.label(),
            focus_status,
        );

        // Stage 2C (docs/phase2-spec.md "Stage 2C"): import plan (what the
        // faction is asking to bring in) and the total actually landed
        // today, summed across every owned port's `import_flow`.
        let plan_line: Vec<String> = [Good::Food, Good::Energy]
            .iter()
            .map(|g| format!("{}={:.1}", g.label(), faction.import_plan[g.index()]))
            .collect();
        let landed: f32 = world
            .regions
            .iter()
            .filter(|r| r.owner == faction.id)
            .map(|r| r.import_flow)
            .sum();
        println!(
            "  {}  輸入計画: {}  実績: {:.1}",
            pad_right("", 8),
            plan_line.join(" "),
            landed,
        );

        // Stage 3B (docs/phase3-spec.md "Stage 3B"): stance and opinion
        // toward every other faction.
        let dip_line: Vec<String> = world
            .factions
            .iter()
            .filter(|other| other.id != faction.id)
            .map(|other| {
                format!(
                    "{}:{}(opinion {:.0})",
                    other.name,
                    stance_label(world.diplomacy.stance(faction.id, other.id)),
                    world.diplomacy.opinion(faction.id, other.id),
                )
            })
            .collect();
        println!("  {}  外交: {}", pad_right("", 8), dip_line.join(" "));
    }
}

/// Japanese label for a `Stance`, used by `print_faction_table`'s Stage 3B line.
fn stance_label(stance: archipelago_sim::diplomacy::Stance) -> &'static str {
    use archipelago_sim::diplomacy::Stance;
    match stance {
        Stance::War => "交戦",
        Stance::Ceasefire => "停戦",
        Stance::NonAggression => "不可侵",
        Stance::Alliance => "同盟",
    }
}

/// Stage 2D (docs/phase2-spec.md "海域の表(制海権と艦隊数)"): sea control per
/// faction and fleet counts per zone.
pub fn print_sea_zone_table(world: &World) {
    println!();
    println!("--- 海域 (day {}) ---", world.day);
    let header: Vec<String> = world.factions.iter().map(|f| format!("{}制海権", f.name)).collect();
    println!(
        "{}  {}  艦隊数",
        pad_right("海域", 10),
        header.join("  "),
    );
    for zone in &world.sea_zones {
        let control_line: Vec<String> = zone.control.iter().map(|c| format!("{:5.1}%", c * 100.0)).collect();
        let fleets = world
            .units
            .iter()
            .filter(|u| u.alive && u.station == Station::Sea(zone.id))
            .count();
        println!(
            "{}  {}  {:4}",
            pad_right(&zone.name, 10),
            control_line.join("  "),
            fleets,
        );
    }
}

pub fn print_final_board(world: &World) {
    println!();
    println!("=== 最終盤面 (day {}) ===", world.day);
    println!(
        "{}  {}  治安    補給    戦災     輸入   ノード上限",
        pad_right("地域", 12),
        pad_right("所有勢力", 10),
    );
    for region in &world.regions {
        println!(
            "{}  {}  {:5.1}  {:6.1}  {:5.1}%  {:5.1}  {:8.1}",
            pad_right(&region.name, 12),
            pad_right(&world.faction(region.owner).name, 10),
            region.unrest,
            world.supply[region.id.index()],
            region.devastation * 100.0,
            region.import_flow,
            region.node_throughput(),
        );
    }
    print_sea_zone_table(world);
}

/// Stage 4C `--newspaper` (docs/phase4-spec.md "Stage 4C — 新聞・報道生成"):
/// prints one issue's worth of per-faction articles. Purely console output -
/// nothing here reads or writes anything that could feed back into `World`.
pub fn print_newspaper_issue(world: &World, issue: &[NewspaperArticle]) {
    println!();
    println!("=== 新聞 (day {}) ===", world.day);
    for article in issue {
        let source = if article.from_backend { "" } else { " [機械要約]" };
        println!(
            "-- {} 紙 (day {}-{}){} --",
            world.faction(article.faction).name,
            article.period_start,
            article.period_end,
            source,
        );
        println!("{}", article.text);
    }
}

pub fn print_outcome(world: &World, outcome: Outcome) {
    println!();
    match outcome {
        Outcome::Victory(faction) => {
            println!("結果: {} の勝利 (day {})", world.faction(faction).name, world.day);
        }
        Outcome::Stalemate => {
            println!("結果: 膠着 (day {} で終了)", world.day);
        }
        Outcome::Ongoing => unreachable!("print_outcome called before the simulation ended"),
    }
}
