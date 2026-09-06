//! Newtype identifiers for regions, factions, and units.
//!
//! Every id is a plain index into the corresponding `Vec` on `World`
//! (`World::regions`, `World::factions`, `World::units`), so `id.index()`
//! can always be used directly for indexing.

macro_rules! def_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(pub u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

def_id!(RegionId);
def_id!(FactionId);
def_id!(UnitId);
def_id!(SeaZoneId);
def_id!(TransportNodeId);
// TransportLineId: index into `World::transport_lines`, in the same order
// the scenario file declared `transport.lines` (`Scenario::build_world`'s
// straight `.map()`, no reordering) - the same "declaration order fixes the
// id" convention `TransportNodeId` already uses, since a `TransportLineDef`
// carries no author-chosen string id of its own to resolve instead
// (`transport::TransportLineDef`'s own doc). Stage 9D (docs/phase9-spec.md
// "4. 行動"): what `Action::InterdictLine`/`construction::Project::
// TransportLine` address a specific route by.
def_id!(TransportLineId);
