//! `archipelago-api` (docs/phase5-spec.md "Stage 5A — API"): a std-only
//! HTTP + WebSocket server that lets an external program - an RL agent, a
//! bot, a test harness - drive `archipelago-sim` through the same
//! `Simulation`/`Agent`/`Observation` surface `apps/headless` already
//! drives directly. See `docs/phase5-spec.md`'s "0. 方針": "既にあるものを
//! 外に出すだけである" - no new game logic lives in this crate, only the
//! wire protocol around what `crates/sim`/`crates/agents` already do.
//!
//! ## Determinism
//!
//! Each `Session` owns one `Simulation`, which owns its own `Rng`
//! (`archipelago_sim::rng::Rng`), seeded once at `POST /reset` and never
//! shared with any other session. `Session::advance_one_day` applies
//! uncontrolled factions' actions in ascending `FactionId` order (the same
//! order `apps/headless`'s own per-day loop uses) before calling
//! `Simulation::step`, and `POST /action` applies a request's `actions[]`
//! in array order via `Simulation::apply` - so the same seed plus the same
//! sequence of `/action`/`/step` calls always reproduces the same run, and
//! a controlled-nothing session reproduces a headless run byte-for-byte
//! (see the `api_run_matches_headless` test). `SessionManager` gives each
//! session its own lock (`session::SessionManager`'s doc), so concurrent
//! sessions never share mutable state.
//!
//! ## Hostile input
//!
//! Every JSON action a client sends goes through `action_codec::
//! action_from_value` (which can only fail, never panic) and then
//! `Simulation::apply` (which discards anything invalid and reports why,
//! exactly as it always has) - there is no path from an HTTP request to a
//! world mutation that skips `Simulation::apply`. `http::MAX_BODY_BYTES`
//! bounds request size, `action_codec::MAX_ACTIONS_PER_REQUEST` bounds how
//! many actions one `/action` call may carry, and `server::
//! MAX_STEPS_PER_REQUEST` bounds how many simulated days one `/step` call
//! may advance - all independent of `Simulation::apply`'s own validation,
//! which remains the actual defence against an invalid action doing
//! anything at all.

pub mod action_codec;
pub mod http;
pub mod json;
pub mod reward;
pub mod server;
pub mod session;
pub mod state;
pub mod ws;

pub use server::{serve_background, serve_background_with_scenario, ServerHandle};

#[cfg(test)]
mod tests;
