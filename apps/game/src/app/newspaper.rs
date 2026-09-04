//! Stage 7C's newspaper panel (docs/phase7-spec.md "5. 新聞"): generates one
//! issue every `NEWSPAPER_INTERVAL_DAYS`, exactly the way
//! `apps/headless --newspaper` does, and keeps every issue generated so far
//! so the player can page back through history.
//!
//! This client wires no LLM backend of its own - every issue always goes
//! through `archipelago_agents::newspaper`'s already-approved mechanical
//! template fallback (docs/conventions.md §3's approved-exceptions table),
//! the same as `apps/headless`'s own default (`--agent heuristic`, no
//! `--backend`). Adding real LLM wiring to `apps/game` is out of this
//! stage's scope - the newspaper is a report on the board, not something
//! the player's own commands depend on, so a template-only client
//! newspaper does not weaken anything docs/phase7-spec.md's Stage 7C asks
//! for ("読める、履歴を遡れる").

use archipelago_agents::llm::MockBackend;
use archipelago_agents::newspaper;
use archipelago_sim::world::World;

use super::{NewspaperIssue, NewspaperState};

/// Generates the issue covering `news.period_start..world.day` and appends
/// it to `news.history`, resetting the period for what comes next -
/// mirrors `apps/headless/src/main.rs`'s own `--newspaper` loop body
/// exactly. `news.viewing` is left untouched: a player already paging back
/// through history keeps looking at the same past issue rather than being
/// yanked back to the new one every time it lands.
pub(super) fn publish_issue(news: &mut NewspaperState, world: &World) {
    // A fresh, stateless backend every issue - `MockBackend::always_err`
    // never touches the network or the filesystem and always fails, so
    // `newspaper::generate_article` always falls through to its own
    // event-driven Japanese template.
    let backend = MockBackend::always_err(archipelago_agents::llm::LlmError::Unavailable);
    let articles = newspaper::generate_issue(&backend, world, &news.period_events, news.period_start);
    news.history.push(NewspaperIssue { period_start: news.period_start, period_end: world.day, articles });
    news.period_events.clear();
    news.period_start = world.day;
}
