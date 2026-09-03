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
