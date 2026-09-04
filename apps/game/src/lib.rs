//! Stage 7A Bevy client (docs/phase7-spec.md "Stage 7A — 観る").
//!
//! Split so the acceptance test `client_run_matches_headless` can drive the
//! simulation with no window and no `bevy` dependency actually exercised:
//! `sim_driver` and `layout` are plain Rust, no `bevy` import anywhere in
//! either file - only `app` (and its submodules) touches Bevy at all.

pub mod layout;
pub mod sim_driver;

pub mod app;
