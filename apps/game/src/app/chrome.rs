//! Shared visual chrome for every player-facing panel (top bar, faction
//! summary, region inspect, player/policy/diplomacy panels, newspaper, event
//! log, legend). Defined once here and applied by `setup`/`panels`, rather
//! than each panel repeating its own background/border/padding literals, so
//! a new panel gets this same look "by construction" instead of by copying
//! numbers around (docs/conventions.md §1: don't repeat what can be shared).
//!
//! Before this, every panel was a bare `Text` node floating directly on the
//! map: no background, no border, no distinction between a panel's title and
//! its body. Against `map_mode::MapMode::Terrain`'s muted grays/greens or
//! `MapMode::Industry`'s saturated fills underneath, that reads as a debug
//! overlay, not a game UI, and legibility itself suffers wherever a bright
//! hex happens to sit under a line of text.
//!
//! `PANEL_BG`'s alpha is high enough (0.92) that even `map_mode`'s single
//! brightest fill (`Color::srgb(1.00, 0.78, 0.08)`, `MapMode::Industry`'s own
//! top capacity band) contributes only a faint tint behind a panel, never
//! enough to fight with `PANEL_BODY_COLOR`/`PANEL_TITLE_COLOR` text sitting
//! on top of it - confirmed by screenshot over both `Terrain` and `Industry`
//! (this module's own doc references the same two modes docs/conventions.md
//! and this task both name as "the hard cases").

use bevy::prelude::*;

use super::setup::text_font;

/// Panel background - see this module's own doc for why this alpha was
/// chosen. Replaces `panels::PANEL_BG`'s own former private copy (one
/// definition now, not two that could quietly drift apart).
pub(super) const PANEL_BG: Color = Color::srgba(0.05, 0.06, 0.09, 0.92);

/// A subdued border - just visible enough to separate a panel's own edge
/// from the map/other panels behind it without itself competing for
/// attention (task ask: "restrained and consistent... legibility beats
/// decoration"). Colored off `palette::NEUTRAL` rather than a fresh gray, so
/// every border in this crate that means "no particular owner/emphasis"
/// shares one source.
pub(super) const PANEL_BORDER: Color = Color::srgba(0.55, 0.58, 0.62, 0.55);

/// Title text color - the same warm accent `panels::spawn_policy_panel`/
/// `spawn_diplomacy_panel` already used for their own `"-- 政策 ... --"`/
/// `"-- 外交 ... --"` headers before this task, now the one shared constant
/// every panel's title uses, so "this text is a title" reads as one visual
/// language across every panel rather than each picking its own accent.
pub(super) const PANEL_TITLE_COLOR: Color = Color::srgb(1.0, 0.82, 0.45);

/// Body text color - a slightly dimmer, neutral white so a title's warm
/// accent (`PANEL_TITLE_COLOR`) is the thing the eye lands on first.
pub(super) const PANEL_BODY_COLOR: Color = Color::srgb(0.90, 0.92, 0.95);

const PANEL_PADDING: f32 = 8.0;
const PANEL_BORDER_WIDTH: f32 = 1.0;
const PANEL_TITLE_FONT_SIZE: f32 = 13.0;

/// Total chrome overhead (padding + border) a panel's frame adds on one
/// edge - the number every position/width derivation that must stay clear
/// of a chrome-framed panel's *outer* box reads, rather than restating
/// `8.0 + 1.0` (or worse, a slightly different guess) at each call site.
/// `setup::spawn_ui`'s top-bar-to-faction-panel gap is exactly this; so is
/// every derived width in this module's own callers that already accounted
/// for a panel's own historical `padding: 8.0` before this task (the four
/// `panels::spawn_*_panel` functions) now also needs `PANEL_BORDER_WIDTH` on
/// top.
pub(super) const PANEL_FRAME_INSET: f32 = PANEL_PADDING + PANEL_BORDER_WIDTH;

/// Sets the padding/border every chrome-framed panel shares onto a `Node` the
/// caller has already built with its own `position_type`/`top`/`left`/
/// `width`/`flex_direction`/... - one place that owns "how thick is a
/// panel's frame" so every panel's content inset stays numerically identical.
pub(super) fn framed(mut node: Node) -> Node {
    node.padding = UiRect::all(Val::Px(PANEL_PADDING));
    node.border = UiRect::all(Val::Px(PANEL_BORDER_WIDTH));
    node
}

pub(super) fn panel_background() -> BackgroundColor {
    BackgroundColor(PANEL_BG)
}

pub(super) fn panel_border() -> BorderColor {
    BorderColor::all(PANEL_BORDER)
}

/// A panel's title row: `text`, styled consistently across every panel that
/// has one. Some panels (the four `panels::spawn_*_panel` button panels)
/// already had their own static title text before this task - callers there
/// just restyle their existing title with this instead of spawning a new
/// node; panels that had no title at all (the region-inspect panel) gain one
/// as a genuinely new, static child (never touching the *dynamic* text a
/// sync system writes elsewhere in the same panel - task ask: "do not change
/// what any panel says").
pub(super) fn panel_title(text: impl Into<String>, font: &Handle<Font>) -> (Text, TextFont, TextColor) {
    (Text::new(text.into()), text_font(PANEL_TITLE_FONT_SIZE, font), TextColor(PANEL_TITLE_COLOR))
}

/// Body text color for a chrome-framed panel's own dynamic content -
/// `TextColor(PANEL_BODY_COLOR)` spelled out once so every panel's body
/// reads as the same shade of "not the title".
pub(super) fn panel_body_color() -> TextColor {
    TextColor(PANEL_BODY_COLOR)
}

/// Shared by every collapsible chrome-framed panel's own root-visibility
/// toggle (`panels::sync_region_action_buttons`/`sync_policy_panel`/
/// `sync_diplomacy_panel`, and - since this fix - `ui::
/// update_top_bar_player_stats`/`update_inspect_panel`/`update_player_panel`/
/// `update_newspaper_panel`): a closed/empty panel has to mean *both*
/// "don't render" (`Visibility::Hidden` - still what keeps a closed panel's
/// buttons reporting `Interaction::None`, per `bevy_ui`'s own documented
/// guarantee `panels`' own module doc already cites) and "don't reserve flex
/// space" (`Node::display = Display::None`) - without the second half, a
/// closed panel still occupies its full content box as permanent dead space
/// in whatever flex column it sits in, even while invisible.
///
/// Originally lived only in `panels.rs`, used solely by the three
/// button-panel `sync_*` systems above; moved here and made a single call
/// site for the four `ui.rs` panels this task's own fix adds, rather than
/// each of those seven call sites (three original, four new) restating the
/// same two-field toggle on its own - see this fix's own note for the defect
/// that repeating it caused (`ui::update_top_bar_player_stats`/`ui::
/// update_inspect_panel`/`ui::update_player_panel` toggled only `Visibility`,
/// leaving `Node::display` at its default `Flex` forever, so a hidden panel
/// kept reserving its full width/padding/border/gap in its flex column).
pub(super) fn set_panel_shown(visibility: &mut Visibility, node: &mut Node, showing: bool) {
    *visibility = if showing { Visibility::Visible } else { Visibility::Hidden };
    node.display = if showing { Display::Flex } else { Display::None };
}
