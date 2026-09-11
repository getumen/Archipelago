//! The tradeable commodities in Stage 2A's production chain, and the fixed
//! index each one occupies in every `[f32; GOOD_COUNT]` array
//! (`Region::capacity`, `Faction::stock`, `Faction::industry_priority`).
//!
//! Always index those arrays through `Good::index()` — never derive an
//! index from hashing or from iterating a `HashMap`/`HashSet` — so the
//! encoding and every accumulation order stay fully deterministic.
//!
//! Stage 11A (docs/phase11-spec.md §2) splits the single `Arms` commodity
//! Phase 1 introduced into one good per land branch: `Infantry`, `Armour`
//! and `Artillery`. `Arms` is **re-read as `Infantry`**, not eliminated
//! outright: every existing consumer (land unit equipment, and — until
//! Stage 11B gives Naval/Air their own dedicated commodity — Sea/Air
//! equipment too) keeps drawing exactly the same stock/capacity numbers it
//! always has, under its new, narrower name. The alternative the spec
//! allowed (dropping `Arms` for three brand-new goods with no `Infantry`
//! survivor) would have meant re-deriving Sea/Air's supply from scratch
//! with no historical baseline to check it against - re-reading preserves
//! one, unambiguous meaning for the renamed slot instead
//! (docs/conventions.md's "同じ名前が2つの意味を持つと必ず食い違う": `Arms`
//! never meant "every branch's equipment" *and* "just infantry's" at once
//! in this codebase, and it still doesn't after the rename).
//!
//! `Armour` and `Artillery` were genuinely new in Stage 11A: every scenario
//! got region-varying production capacity for them (see
//! `tools/hexmap/build_scenario.py` and `scenarios/mvp.json`/`japan47.json`)
//! so the data existed and differed region to region, but no unit type drew
//! on either stock yet ("部隊種別は入れない" - Stage 11B). Stage 11B
//! (docs/phase11-spec.md §1) is what wires them up: `military::Branch`
//! (Infantry/Armour/Artillery) maps 1:1 onto `Good::Infantry`/`Good::Armour`/
//! `Good::Artillery` via `Branch::equipment_good`, and `Region::industry_total`
//! now includes both (see that function's own doc for why it didn't before).
//!
//! `Naval` and `Aircraft` are Stage 11B's other half - finishing what 11A
//! deliberately deferred. Until this stage, `Domain::Sea`/`Domain::Air` units
//! drew `Good::Infantry` for their own equipment (11A's own doc used to
//! record this here), sharing a stock with land's infantry-branch equipment
//! even though a warship and a rifle have nothing to do with each other
//! industrially. That is exactly the single-scalar-pool shape
//! `docs/future-work.md`'s "単一プールの実測" measured and this whole phase
//! exists to break: every domain now draws its own commodity, with no
//! fallback to a shared one. `naval::sea_demand`/`naval::apply_fleet_supply`/
//! `action::apply_recruit`'s `Domain::Sea` arm read `Good::Naval`;
//! `air::air_demand`/`air::apply_air_supply`/`apply_recruit`'s `Domain::Air`
//! arm read `Good::Aircraft`. Neither gets a recipe of its own any more than
//! `Armour`/`Artillery` do (`economy::tick_economy`'s own doc) - produced
//! straight from capacity, so introducing them doesn't perturb the existing
//! Steel/Machinery/Infantry chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Good {
    Food,
    Energy,
    Steel,
    Machinery,
    Munitions,
    /// The historical `Arms` slot, re-read as land's infantry-branch
    /// equipment (this module's own doc has the full account of why this
    /// was a rename, not a replacement).
    Infantry,
    Armour,
    Artillery,
    /// Stage 11B: a fleet's own equipment commodity (`military::Domain::Sea`).
    Naval,
    /// Stage 11B: an air squadron's own equipment commodity, on top of the
    /// airframe `Good::Machinery` cost `action::apply_recruit` already
    /// charges a `Domain::Air` recruit (`balance::AIR_UNIT_MACHINERY_COST`).
    Aircraft,
}

pub const GOOD_COUNT: usize = 10;

/// Every `Good`, in the fixed order that matches `Good::index()` and the
/// `[f32; GOOD_COUNT]` layout. Iterate this instead of hand-rolling a
/// `0..GOOD_COUNT` loop whenever the code needs the `Good` value itself.
pub const ALL_GOODS: [Good; GOOD_COUNT] = [
    Good::Food,
    Good::Energy,
    Good::Steel,
    Good::Machinery,
    Good::Munitions,
    Good::Infantry,
    Good::Armour,
    Good::Artillery,
    Good::Naval,
    Good::Aircraft,
];

impl Good {
    pub const fn index(self) -> usize {
        match self {
            Good::Food => 0,
            Good::Energy => 1,
            Good::Steel => 2,
            Good::Machinery => 3,
            Good::Munitions => 4,
            Good::Infantry => 5,
            Good::Armour => 6,
            Good::Artillery => 7,
            Good::Naval => 8,
            Good::Aircraft => 9,
        }
    }

    /// Short Japanese label, used by the headless console report.
    pub const fn label(self) -> &'static str {
        match self {
            Good::Food => "食料",
            Good::Energy => "エネルギー",
            Good::Steel => "鉄鋼",
            Good::Machinery => "機械",
            Good::Munitions => "軍需品",
            Good::Infantry => "歩兵装備",
            Good::Armour => "機甲装備",
            Good::Artillery => "砲兵装備",
            Good::Naval => "艦艇装備",
            Good::Aircraft => "航空装備",
        }
    }

    /// Lowercase English key, used by the headless `--json` output.
    pub const fn key(self) -> &'static str {
        match self {
            Good::Food => "food",
            Good::Energy => "energy",
            Good::Steel => "steel",
            Good::Machinery => "machinery",
            Good::Munitions => "munitions",
            Good::Infantry => "infantry",
            Good::Armour => "armour",
            Good::Artillery => "artillery",
            Good::Naval => "naval",
            Good::Aircraft => "aircraft",
        }
    }
}
