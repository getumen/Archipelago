//! Notable happenings emitted by tick systems, for logging and UI.

use std::fmt;

use crate::ids::{FactionId, RegionId, SeaZoneId, UnitId};
use crate::world::Station;

#[derive(Clone, Debug)]
pub enum Event {
    Battle {
        region: RegionId,
        factions: Vec<FactionId>,
        casualties: f32,
    },
    /// Stage 2D (docs/phase2-spec.md "3. 海戦"): the sea-domain counterpart
    /// of `Battle`, kept as its own variant rather than reusing `Battle`'s
    /// `region: RegionId` field, which a sea zone can't fill.
    NavalBattle {
        zone: SeaZoneId,
        factions: Vec<FactionId>,
        casualties: f32,
    },
    UnitDestroyed {
        unit: UnitId,
        station: Station,
        owner: FactionId,
    },
    RegionCaptured {
        region: RegionId,
        from: FactionId,
        to: FactionId,
    },
    FactionEliminated {
        faction: FactionId,
    },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Battle {
                region,
                factions,
                casualties,
            } => {
                let ids: Vec<String> = factions.iter().map(|f| f.0.to_string()).collect();
                write!(
                    f,
                    "battle in region {} between factions [{}]: {:.2} manpower lost",
                    region.0,
                    ids.join(", "),
                    casualties
                )
            }
            Event::NavalBattle {
                zone,
                factions,
                casualties,
            } => {
                let ids: Vec<String> = factions.iter().map(|f| f.0.to_string()).collect();
                write!(
                    f,
                    "naval battle in sea zone {} between factions [{}]: {:.2} manpower lost",
                    zone.0,
                    ids.join(", "),
                    casualties
                )
            }
            Event::UnitDestroyed { unit, station, owner } => match station {
                Station::Region(region) => write!(
                    f,
                    "unit {} (faction {}) destroyed in region {}",
                    unit.0, owner.0, region.0
                ),
                Station::Sea(zone) => write!(
                    f,
                    "unit {} (faction {}) sunk in sea zone {}",
                    unit.0, owner.0, zone.0
                ),
            },
            Event::RegionCaptured { region, from, to } => write!(
                f,
                "region {} captured by faction {} from faction {}",
                region.0, to.0, from.0
            ),
            Event::FactionEliminated { faction } => {
                write!(f, "faction {} eliminated", faction.0)
            }
        }
    }
}
