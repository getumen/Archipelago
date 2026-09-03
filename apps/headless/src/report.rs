//! Human-readable console output: day-by-day events, periodic faction
//! summaries, and the final board (mvp-spec.md §8). Region and faction
//! names come straight from the scenario data and are Japanese; the
//! surrounding labels are Japanese too so the two read as one table.

use archipelago_sim::event::Event;
use archipelago_sim::sim::Outcome;
use archipelago_sim::world::World;

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
        Event::UnitDestroyed { unit, region, owner } => {
            format!(
                "部隊壊滅: {} 軍 部隊#{} が {} で失われた",
                world.faction(*owner).name,
                unit.0,
                world.region(*region).name
            )
        }
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
        "{}  領土  部隊   人的資源    備蓄    装備   補給率  安定度  戦意",
        pad_right("勢力", 10)
    );
    for faction in &world.factions {
        let status = if faction.alive { "" } else { "(脱落)" };
        let regions = world.region_count(faction.id);
        let units = world.units.iter().filter(|u| u.alive && u.owner == faction.id).count();
        println!(
            "{}  {:4}  {:4}  {:8.2}  {:6.1}  {:6.1}  {:6.1}%  {:6.1}  {:5.1}{status}",
            pad_right(&faction.name, 10),
            regions,
            units,
            faction.manpower,
            faction.supplies,
            faction.equipment,
            faction.supply_ratio * 100.0,
            faction.stability,
            faction.war_support,
        );
    }
}

pub fn print_final_board(world: &World) {
    println!();
    println!("=== 最終盤面 (day {}) ===", world.day);
    println!(
        "{}  {}  治安    補給",
        pad_right("地域", 12),
        pad_right("所有勢力", 10),
    );
    for region in &world.regions {
        println!(
            "{}  {}  {:5.1}  {:6.1}",
            pad_right(&region.name, 12),
            pad_right(&world.faction(region.owner).name, 10),
            region.unrest,
            world.supply[region.id.index()],
        );
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
