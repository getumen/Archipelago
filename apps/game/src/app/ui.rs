//! Updates the text content of every UI panel from the current `SimRes`
//! state - date/scenario/speed (top bar), the selected faction's summary
//! (left), the event log (bottom), and the selected region's detail
//! (right, only while `SelectedRegion` is `Some`). Read-only against
//! `SimRes`, same as `visuals` - see that module's doc.

use bevy::prelude::*;

use archipelago_sim::good::ALL_GOODS;
use archipelago_sim::naval::is_port_blockaded;

use super::{EventLog, EventLogText, FactionPanelText, InspectText, ScenarioMeta, SelectedFaction, SelectedRegion, SimRes, SpeedRes, TopBarText};

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
