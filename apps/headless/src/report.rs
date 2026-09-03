//! Human-readable console output: day-by-day events, periodic faction
//! summaries, and the final board (mvp-spec.md §8). Region and faction
//! names come straight from the scenario data and are Japanese; the
//! surrounding labels are Japanese too so the two read as one table.

use archipelago_sim::event::Event;
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::{Domain, Station, World};

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
