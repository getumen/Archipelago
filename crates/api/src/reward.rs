//! The default `reward` scalar `POST /step` reports (docs/phase5-spec.md
//! "POST /step ... -> { observation, reward, terminated, info }"). Stage 5B
//! (out of scope here) makes the *Python*-side reward function swappable;
//! this is only the API's own built-in default, so `/step` always has
//! something meaningful to report even to a caller that never plugs in a
//! custom reward.
//!
//! These weights are an API/RL-facing knob, not a simulation balance
//! constant (`crates/sim/src/balance.rs`) and not an AI-agent tuning knob
//! (`crates/agents`) - they don't affect the simulation at all, only how
//! this crate scores a faction's day-over-day change for a caller that
//! hasn't supplied its own reward function - so they live here instead of
//! either of those.
//!
//! Built entirely from `Simulation::apply`-validated state deltas (region
//! count, industrial capacity, manpower), so it inherits every defence
//! `crates/sim` already has against a farmable resource: nothing here lets
//! repeating a no-op action, or any action `Simulation::apply` would
//! reject, manufacture positive reward out of nothing (docs/phase5-spec.md
//! "報酬関数は差し替え可能にする" and "希少な資源に固定の優先順位を置か
//! ない" both anticipate exactly this kind of optimiser pressure).

use archipelago_sim::ids::FactionId;
use archipelago_sim::world::World;

const WEIGHT_REGIONS: f32 = 10.0;
const WEIGHT_INDUSTRY: f32 = 0.1;
const WEIGHT_MANPOWER: f32 = 0.001;

/// A snapshot of the scalars `reward_delta` diffs, taken once per
/// simulated day so a multi-day `/step steps=N` call reports the sum of
/// each day's own reward, not just a start/end diff (which would let a
/// transient loss "cancel out" on the way to a transient gain within the
/// same request).
#[derive(Clone, Copy)]
pub struct RewardBasis {
    regions: f32,
    industry: f32,
    manpower: f32,
}

impl RewardBasis {
    pub fn snapshot(world: &World, faction: FactionId) -> RewardBasis {
        RewardBasis {
            regions: world.region_count(faction) as f32,
            industry: world.industry_total(faction),
            manpower: world.faction(faction).manpower,
        }
    }
}

/// The weighted change from `before` to `world`'s current state for
/// `faction` - territory gained/lost dominates, industrial capacity and
/// manpower contribute smaller terms so a faction that's merely rebuilding
/// (no territory change) still sees nonzero signal.
pub fn reward_delta(before: RewardBasis, world: &World, faction: FactionId) -> f32 {
    let after = RewardBasis::snapshot(world, faction);
    WEIGHT_REGIONS * (after.regions - before.regions)
        + WEIGHT_INDUSTRY * (after.industry - before.industry)
        + WEIGHT_MANPOWER * (after.manpower - before.manpower)
}
