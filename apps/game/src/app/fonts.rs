//! Loads a CJK-capable font so Japanese text (every region name, faction
//! name, and event-log line - this game's entire content is Japanese) draws
//! actual glyphs instead of tofu boxes. Bevy's `default_font`
//! (`FiraMono-subset.ttf`) covers Latin only.
//!
//! ## Font source: a system path, not a vendored asset
//!
//! This loads `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` off
//! disk at startup, rather than shipping a font file under
//! `apps/game/assets/`. Weighed deliberately:
//!
//! - Vendoring would mean committing a multi-megabyte binary font into the
//!   repository (the Noto CJK "Regular" `.ttc` alone is ~19 MB; even a
//!   single-family substitute is a few MB) for something this crate's own
//!   Cargo.toml already treats as environment-provided (see its own
//!   comments on the X11-only, audio-less feature set this box has).
//! - The tradeoff is real and worth naming rather than hiding: this exact
//!   path is Ubuntu/Debian-specific (`fonts-noto-cjk`'s install location)
//!   and will not exist on another distro, macOS, or Windows. On any
//!   machine where it's absent, `load` below does not panic - it logs a
//!   clear warning and leaves every `TextFont` on Bevy's own default font,
//!   so the client still starts and runs (Japanese text goes back to tofu
//!   boxes, exactly today's failure mode, rather than a crash).
//! - If this client ever ships to a machine that isn't this one,
//!   vendoring (or bundling a font via a build step) is the fix - this
//!   module's `CJK_FONT_PATH` is the single place that would need to
//!   change.
//!
//! `.ttc` (a *collection* of several font faces - here five region variants,
//! JP/KR/SC/TC/HK, each also covering Hiragana/Katakana/shared Han glyphs)
//! loads fine: `bevy_text` 0.19 shapes text through Parley/`fontique`, whose
//! `Collection::register_fonts` scans every face in a blob via
//! `read_fonts::FileRef`, which natively understands `.ttc`. Every face in
//! the collection gets registered under one alias family (this asset's own
//! id), so which specific regional face is picked per glyph isn't
//! controlled here - a cosmetic detail (stroke-shape variation between,
//! e.g., the JP and SC faces for the same Han character), never a legibility
//! one, since every face carries the shared CJK/Kana glyph repertoire.

use bevy::prelude::*;

/// Ubuntu/Debian's `fonts-noto-cjk` install path - see this module's own doc
/// for the portability tradeoff of hardcoding it.
const CJK_FONT_PATH: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";

/// The font handle every `TextFont` this crate spawns should reference -
/// either the loaded CJK font, or `Handle::default()` (Bevy's own
/// Latin-only default) when `CJK_FONT_PATH` couldn't be read.
#[derive(Resource, Clone)]
pub(crate) struct AppFont(pub Handle<Font>);

/// Reads `CJK_FONT_PATH`, registers it as a `Font` asset, and inserts
/// `AppFont`. Called directly from `super::run` (not wired up as a `Startup`
/// system) so the returned handle exists before `setup::setup` runs, with no
/// need to reason about command-flush ordering between two `Startup`
/// systems.
pub(super) fn load(app: &mut App) {
    let handle = match std::fs::read(CJK_FONT_PATH) {
        Ok(bytes) => {
            let handle = app
                .world_mut()
                .resource_mut::<Assets<Font>>()
                .add(Font::from_bytes(bytes));
            eprintln!("archipelago-game: loaded CJK font from {CJK_FONT_PATH}");
            handle
        }
        Err(err) => {
            eprintln!(
                "archipelago-game: warning: could not load CJK font at {CJK_FONT_PATH} ({err}); \
                 falling back to Bevy's built-in Latin-only font - every Japanese name/label will \
                 render as an empty box until a CJK font is available at that path."
            );
            Handle::default()
        }
    };
    app.insert_resource(AppFont(handle));
}
