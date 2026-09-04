//! Formats an `Event` as a human-readable Japanese line for the bottom
//! event-log panel, naming regions/factions/sea zones instead of their raw
//! numeric ids ("faction 3 rejects faction 5's ceasefire proposal" read
//! unreadable even before the tofu-box problem, and stayed that way only
//! because nothing else printed a name either).
//!
//! Deliberately duplicates `apps/headless/src/report.rs::print_event`'s own
//! line text verbatim (same Japanese phrasing, same field order) rather than
//! importing it - `apps/headless` is a binary-only crate with no library
//! target (see `crate::sim_driver`'s own test doc, which duplicates that
//! crate's day-loop for the identical reason). Keeping the two in sync by
//! hand is cheap: this `match` only grows when `archipelago_sim::event::
//! Event` itself grows a variant, and `sim_control::advance_simulation`'s
//! own `EventLog` doesn't need the console-only helpers (`print_faction_
//! table`, `print_final_board`, ...) that make up the rest of that file.

use archipelago_sim::diplomacy::Treaty;
use archipelago_sim::event::Event;
use archipelago_sim::world::{Station, World};

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

pub(super) fn format_event(world: &World, event: &Event) -> String {
    match event {
        Event::Battle { region, factions, casualties } => {
            let region_name = &world.region(*region).name;
            let names: Vec<&str> = factions.iter().map(|f| world.faction(*f).name.as_str()).collect();
            format!("戦闘: {} で {} が交戦 (損耗 {:.2}万人)", region_name, names.join(" vs "), casualties)
        }
        Event::NavalBattle { zone, factions, casualties } => {
            let zone_name = &world.sea_zone(*zone).name;
            let names: Vec<&str> = factions.iter().map(|f| world.faction(*f).name.as_str()).collect();
            format!("海戦: {} で {} が交戦 (損耗 {:.2}万人)", zone_name, names.join(" vs "), casualties)
        }
        Event::UnitDestroyed { unit, station, owner } => match station {
            Station::Region(region) => {
                format!("部隊壊滅: {} 軍 部隊#{} が {} で失われた", world.faction(*owner).name, unit.0, world.region(*region).name)
            }
            Station::Sea(zone) => {
                format!("艦隊撃沈: {} 軍 部隊#{} が {} で撃沈された", world.faction(*owner).name, unit.0, world.sea_zone(*zone).name)
            }
        },
        Event::RegionCaptured { region, from, to } => {
            format!("占領: {} を {} が {} から奪取", world.region(*region).name, world.faction(*to).name, world.faction(*from).name)
        }
        Event::FactionEliminated { faction } => format!("敗北: {} が全領土を失い脱落", world.faction(*faction).name),
        Event::Strike { faction } => format!("ストライキ: {} で労働者がストライキ開始 (工業生産低下)", world.faction(*faction).name),
        Event::Protest { faction } => format!("デモ: {} で市民デモが拡大 (治安悪化)", world.faction(*faction).name),
        Event::Mutiny { faction } => format!("軍部不服従: {} で軍の統制が乱れる (組織率回復低下)", world.faction(*faction).name),
        Event::CapitalFlight { faction } => format!("資本逃避: {} で資本が流出 (建設・機械生産低下)", world.faction(*faction).name),
        Event::RegimeChange { faction } => format!("政権交代: {} で政権が崩壊、政策が既定値に戻る", world.faction(*faction).name),
        Event::Separatism { region, from, to } => {
            format!("地方独立運動: {} が {} から {} へ復帰", world.region(*region).name, world.faction(*from).name, world.faction(*to).name)
        }
        Event::TreatyProposed { from, to, treaty } => {
            format!("外交提案: {} が {} に {} を提案", world.faction(*from).name, world.faction(*to).name, treaty_label(*treaty))
        }
        Event::TreatySigned { a, b, treaty } => {
            format!("条約締結: {} と {} が {} を締結", world.faction(*a).name, world.faction(*b).name, treaty_label(*treaty))
        }
        Event::TreatyRejected { from, to, treaty } => {
            format!("外交拒否: {} が {} の {} 提案を拒否", world.faction(*to).name, world.faction(*from).name, treaty_label(*treaty))
        }
        Event::TreatyBroken { a, b, treaty } => {
            format!("条約破棄: {} が {} との {} を破棄", world.faction(*a).name, world.faction(*b).name, treaty_label(*treaty))
        }
        Event::WarDeclared { a, b } => format!("宣戦布告: {} が {} に宣戦", world.faction(*a).name, world.faction(*b).name),
        Event::AllianceDragIn { faction, into_war_with } => {
            format!("同盟参戦: {} が同盟により {} との戦争に参戦", world.faction(*faction).name, world.faction(*into_war_with).name)
        }
        Event::NaturalLanguageProposed { from, to, text } => {
            format!("自然言語外交: {} が {} に提案 「{}」", world.faction(*from).name, world.faction(*to).name, text)
        }
        Event::NaturalLanguageAccepted { from, to } => {
            format!("自然言語外交・成立: {} が {} の提案を受諾", world.faction(*to).name, world.faction(*from).name)
        }
        Event::NaturalLanguageRejected { from, to } => {
            format!("自然言語外交・拒否: {} が {} の提案を拒否", world.faction(*to).name, world.faction(*from).name)
        }
        Event::NaturalLanguageTermsInvalid { from, to } => format!(
            "自然言語外交・不成立: {} が {} の提案を受諾しようとしたが条件が満たせず不成立",
            world.faction(*to).name,
            world.faction(*from).name
        ),
    }
}
