//! Stage 6A (docs/phase6-spec.md "Stage 6A"): the JSON `Value` type, parser
//! and serializer moved to `archipelago_sim::json` so `crates/sim::scenario`
//! can parse `--scenario` files with it too, without `crates/sim` gaining a
//! dependency on this crate. Every existing `crate::json::Value`/
//! `crate::json::parse`/`crate::json::JsonError` call site in this crate
//! keeps working unchanged against this re-export.

pub use archipelago_sim::json::*;
