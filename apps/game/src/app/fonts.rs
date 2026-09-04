//! Loads a CJK-capable font so Japanese text (every region name, faction
//! name, event-log line, and anything Stage 4B's natural-language proposal
//! box lets a player type - this game's entire content is Japanese) draws
//! actual glyphs instead of tofu boxes. Bevy's `default_font`
//! (`FiraMono-subset.ttf`) covers Latin only.
//!
//! ## Font source: bundled, not a system path
//!
//! `BUNDLED_FONT_BYTES` embeds `assets/fonts/NotoSansJP-VariableFont_wght.ttf`
//! into this binary at compile time via `include_bytes!`. This is a
//! deliberate reversal of two earlier designs, both of which turned out to
//! be wrong in ways this project actually hit:
//!
//! 1. The very first version left every `TextFont` on Bevy's Latin-only
//!    default font when no CJK font could be found, logging a warning
//!    instead. That renders this entirely-Japanese game as tofu boxes end
//!    to end - not a minor cosmetic degradation - and docs/conventions.md
//!    §3 (フォールバック原則禁止) rules it out. External code review fix
//!    B3 replaced it with a hard panic naming the exact path that was
//!    missing.
//! 2. The panic-with-one-hardcoded-path version only ever looked at
//!    `/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc` - the
//!    Ubuntu/Debian `fonts-noto-cjk` install location. Still fail-fast,
//!    but it panicked unconditionally on macOS and Windows even though
//!    docs/design.md §16 lists both as target platforms, and it panicked
//!    on any Linux box that simply hadn't installed that one package. It
//!    "worked" only because the dev machine happened to have Noto CJK
//!    installed - which is exactly the class of bug a distributed game
//!    cannot ship with: **a released game must not require its players to
//!    independently install a system font before it will start.**
//!
//! Bundling the font is what fixes that: it is unconditionally present
//! wherever this binary is, on every platform, with no install step. Noto
//! Sans JP is licensed under the SIL Open Font License 1.1
//! (`assets/fonts/OFL.txt`, shipped alongside it - the license requires the
//! text travel with the font), which explicitly permits redistribution.
//! The JP-only family was chosen over the system Noto Sans **CJK** font
//! this module used to read (which bundles SC/TC/JP/KR/HK in one ~19 MB
//! `.ttc`) because this game's text is exclusively Japanese - Noto Sans JP
//! alone is ~9.1 MB and covers it completely, including every character
//! Stage 4B's free-text natural-language proposal box could produce. It is
//! **not** subset: because that box accepts arbitrary player-typed
//! Japanese, this binary cannot know ahead of time which glyphs will
//! actually be needed, so the full font ships unmodified.
//!
//! `include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), ...))` resolves
//! against this crate's own `Cargo.toml` location, not the process's
//! current working directory - unlike Bevy's own `AssetServer` (which
//! resolves relative paths under `assets/` against the *working directory*
//! by default, i.e. wherever `cargo run`/the built binary was launched
//! from), this never needs `apps/game/assets/` to sit next to the
//! executable or next to the shell's cwd. `cargo run -p archipelago-game`
//! works identically from the repository root or from inside `apps/game`.
//! A missing/renamed font file is therefore a *build*-time error (the
//! `include_bytes!` fails to compile), never something `load` can observe
//! at runtime - which is also why there is no "file not found" case for
//! the bundled font below, only the `MIN_PLAUSIBLE_FONT_BYTES` sanity
//! check for a corrupt-but-present blob (see its own doc).
//!
//! ## Explicit override still wins
//!
//! `--cjk-font <path>` / `ARCHIPELAGO_CJK_FONT` still take precedence over
//! the bundled font (CLI beats env var when both are set) and, if the
//! named file can't be read, `load` still panics immediately naming that
//! exact path rather than quietly falling back to the bundled font: the
//! user asked for a specific font, so silently substituting another one on
//! their behalf would be exactly the fallback-behind-their-back
//! docs/conventions.md §3 forbids. This is genuinely useful even with a
//! bundled default - e.g. swapping in a font with better glyph coverage
//! for a name Noto Sans JP happens to render poorly, without a rebuild.
//!
//! ## No more per-platform system search
//!
//! An earlier version of this fix (before the decision to bundle) searched
//! a per-platform list of well-known system font locations (Noto CJK paths
//! on Linux, Hiragino/PingFang on macOS, MS/Yu Gothic on Windows) as the
//! non-override fallback. That list is gone now: with the bundled font
//! always present and always loadable barring outright corruption, a
//! system search would never run - dead code with no reachable test,
//! which docs/conventions.md §1's ban on speculative abstraction argues
//! against keeping around "just in case". If a future need reappears (e.g.
//! an install that must not embed the font for size reasons), searching
//! system paths as one more source ahead of the bundled fallback would be
//! a small, easy addition then - not a reason to carry the complexity now.
//!
//! ## `.ttc`/variable-font loading mechanics
//!
//! `bevy_text` 0.19 shapes text through Parley/`fontique`, whose
//! `Collection::register_fonts` reads a font blob via `read_fonts::
//! FileRef`, which understands both TrueType collections (`.ttc`, what the
//! old system-search version loaded) and single-face variable fonts like
//! this one (`fvar`/`gvar`/`avar`/`HVAR` tables - confirmed present in
//! `assets/fonts/NotoSansJP-VariableFont_wght.ttf`). Every `TextFont` this
//! crate spawns references the one registered family (this asset's own
//! alias id), at whatever the font's default weight instance is - Stage 7
//! never needs multiple weights.

use bevy::prelude::*;

/// Overrides the bundled font - see this module's own doc.
const ENV_VAR: &str = "ARCHIPELAGO_CJK_FONT";

/// Where the bundled font lives, relative to this crate's `Cargo.toml` -
/// used only for panic messages/`eprintln!` provenance below; the actual
/// read happens at compile time via `include_bytes!` (`BUNDLED_FONT_BYTES`).
const BUNDLED_FONT_ASSET_PATH: &str = "apps/game/assets/fonts/NotoSansJP-VariableFont_wght.ttf";

/// The font this binary ships with - see this module's own doc for why
/// bundling replaced both the earlier Latin-fallback and single-system-path
/// designs. `CARGO_MANIFEST_DIR` is this crate's own directory
/// (`apps/game`), so this resolves the same way regardless of the
/// process's current working directory.
static BUNDLED_FONT_BYTES: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/NotoSansJP-VariableFont_wght.ttf"));

/// Sanity floor on `BUNDLED_FONT_BYTES`: real CJK-capable font files run
/// multiple megabytes, so anything drastically smaller means the file at
/// `BUNDLED_FONT_ASSET_PATH` got replaced with something else (an empty
/// placeholder, a Git LFS pointer stub instead of the real blob, a
/// half-written file) rather than this binary actually having a usable CJK
/// font compiled in. `include_bytes!` guarantees *a* file was present at
/// build time (a genuinely missing file fails the build) - this is the one
/// remaining "unreadable"/"corrupt" case `resolve` can still hit at
/// runtime, and it must still fail fast and name what happened rather than
/// silently drawing tofu (docs/conventions.md §3). The real file is ~9.1
/// MB; 1 MB is comfortably below any real Noto Sans JP build while being
/// far above anything a placeholder file would be.
const MIN_PLAUSIBLE_FONT_BYTES: usize = 1_000_000;

/// The font handle every `TextFont` this crate spawns should reference - the
/// loaded CJK font. `load` never inserts this resource with anything else:
/// a client that couldn't load a CJK font never reaches the point of having
/// an `AppFont` at all (it panics first), so nothing downstream needs to
/// consider "what if this is Bevy's Latin-only default".
#[derive(Resource, Clone)]
pub(crate) struct AppFont(pub Handle<Font>);

/// Why `resolve` couldn't produce a usable font - kept structured (not a
/// pre-formatted string) per docs/conventions.md 1 so `load`'s panic
/// message is built the same way regardless of which case fired.
#[derive(Debug)]
enum CjkFontNotFound {
    /// `--cjk-font`/`{ENV_VAR}` named a file that could not be read.
    /// `resolve` never falls through to the bundled font in this case -
    /// the user asked for a specific font (see module doc).
    Override { path: String, error: std::io::Error },
    /// The bytes compiled in at `BUNDLED_FONT_ASSET_PATH` are smaller than
    /// any real CJK font could plausibly be - see `MIN_PLAUSIBLE_FONT_BYTES`.
    BundledTooSmall { len: usize },
}

impl CjkFontNotFound {
    fn message(&self) -> String {
        match self {
            CjkFontNotFound::Override { path, error } => format!(
                "archipelago-game: could not load the CJK font named via --cjk-font/{ENV_VAR} at {path} \
                 ({error}). Every name and label in this game is Japanese and needs a real CJK font to \
                 render as anything but empty boxes - point --cjk-font (or {ENV_VAR}) at a CJK-capable \
                 font that actually exists on this machine, or drop the override entirely to use the \
                 font this binary already ships with."
            ),
            CjkFontNotFound::BundledTooSmall { len } => format!(
                "archipelago-game: the CJK font bundled at {BUNDLED_FONT_ASSET_PATH} is only {len} bytes - \
                 too small to be a real font (expected several megabytes), so it's almost certainly \
                 corrupt, truncated, or a placeholder rather than the actual Noto Sans JP build. This \
                 binary cannot render its (entirely Japanese) text without it. Rebuild with the real font \
                 file in place, or work around it for now with --cjk-font <path> or the {ENV_VAR} \
                 environment variable pointing at a CJK-capable font on this machine."
            ),
        }
    }
}

/// Picks which CJK font bytes to load: `override_path` (already resolved
/// from `--cjk-font` then `{ENV_VAR}` by `load`, CLI winning when both are
/// set) wins outright and is the *only* thing tried when present - a
/// deliberately-named font that fails to read must fail immediately, not
/// quietly fall back to the bundled font (docs/conventions.md §3).
/// Otherwise uses `bundled`, after the `MIN_PLAUSIBLE_FONT_BYTES` sanity
/// check.
///
/// Split out from `load` so this - the actual precedence policy - is
/// testable without spinning up a Bevy `App` or `Assets<Font>`. `bundled`
/// is a parameter (rather than reading `BUNDLED_FONT_BYTES` directly) so
/// tests can exercise `BundledTooSmall` without needing to actually ship a
/// broken font file.
fn resolve(override_path: Option<String>, bundled: &'static [u8]) -> Result<(String, std::borrow::Cow<'static, [u8]>), CjkFontNotFound> {
    if let Some(path) = override_path {
        return std::fs::read(&path)
            .map(|bytes| (path.clone(), std::borrow::Cow::Owned(bytes)))
            .map_err(|error| CjkFontNotFound::Override { path, error });
    }
    if bundled.len() < MIN_PLAUSIBLE_FONT_BYTES {
        return Err(CjkFontNotFound::BundledTooSmall { len: bundled.len() });
    }
    Ok((BUNDLED_FONT_ASSET_PATH.to_string(), std::borrow::Cow::Borrowed(bundled)))
}

/// Reads a CJK font (`cli_override`, else `{ENV_VAR}`, else the font
/// bundled into this binary - see `resolve`), registers it as a `Font`
/// asset, and inserts `AppFont`. Called directly from `super::run` (not
/// wired up as a `Startup` system) so the returned handle exists before
/// `setup::setup` runs, with no need to reason about command-flush
/// ordering between two `Startup` systems.
///
/// `cli_override` is `--cjk-font <path>`, already parsed by `main.rs`.
///
/// Panics if no CJK font could be resolved - see this module's own doc for
/// why silently falling back to Bevy's Latin-only default font is not an
/// option here.
pub(super) fn load(app: &mut App, cli_override: Option<String>) {
    let override_path = cli_override.or_else(|| std::env::var(ENV_VAR).ok());
    let (source, bytes) = resolve(override_path, BUNDLED_FONT_BYTES).unwrap_or_else(|not_found| panic!("{}", not_found.message()));
    let handle = app.world_mut().resource_mut::<Assets<Font>>().add(Font::from_bytes(bytes.into_owned()));
    eprintln!("archipelago-game: loaded CJK font from {source}");
    app.insert_resource(AppFont(handle));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in "bundled" bytes for tests that don't care about the
    /// override path: large enough to clear `MIN_PLAUSIBLE_FONT_BYTES`,
    /// distinguishable from real font content, and `'static` so it fits
    /// `resolve`'s signature the same way the real embedded font does.
    fn plausible_bundled_bytes() -> &'static [u8] {
        static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        BYTES.get_or_init(|| vec![0xAB; MIN_PLAUSIBLE_FONT_BYTES + 1])
    }

    fn write_temp_file(dir: &std::path::Path, name: &str, contents: &[u8]) -> String {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("failed to write temp file");
        path.to_str().expect("temp path must be valid UTF-8").to_string()
    }

    #[test]
    fn override_is_honoured_over_the_bundled_font() {
        let dir = tempdir();
        let override_path = write_temp_file(dir.path(), "override.ttf", b"override bytes");

        let (source, bytes) = resolve(Some(override_path.clone()), plausible_bundled_bytes()).expect("override file exists and must resolve");

        assert_eq!(source, override_path, "an override must win over the bundled font");
        assert_eq!(bytes.as_ref(), b"override bytes");
    }

    #[test]
    fn override_naming_a_missing_file_fails_without_falling_back_to_the_bundled_font() {
        let dir = tempdir();
        let missing_override = dir.path().join("does-not-exist.ttf").to_str().unwrap().to_string();

        let err = resolve(Some(missing_override.clone()), plausible_bundled_bytes())
            .expect_err("a nonexistent override must fail, not silently fall back to the bundled font");

        let CjkFontNotFound::Override { path, .. } = &err else {
            panic!("a missing override must fail as Override, not BundledTooSmall: {err:?}");
        };
        assert_eq!(path, &missing_override, "the failure must name the exact override path");
        let message = err.message();
        assert!(message.contains(&missing_override), "the message must name the exact override path: {message}");
    }

    #[test]
    fn no_override_uses_the_bundled_font() {
        let bundled = plausible_bundled_bytes();

        let (source, bytes) = resolve(None, bundled).expect("a plausible bundled font must resolve");

        assert_eq!(source, BUNDLED_FONT_ASSET_PATH, "with no override, the source must be the bundled font's own path");
        assert_eq!(bytes.as_ref(), bundled, "with no override, the bundled bytes must be used unchanged (never subset/altered)");
    }

    #[test]
    fn a_too_small_bundled_font_fails_and_names_what_was_expected() {
        let tiny = &[0u8; 16][..];
        assert!(tiny.len() < MIN_PLAUSIBLE_FONT_BYTES, "test setup: this case only makes sense below the sanity floor");

        let err = resolve(None, tiny).expect_err("a font far below any real font's size must be rejected, not loaded as-is");

        assert!(matches!(err, CjkFontNotFound::BundledTooSmall { len: 16 }), "must report the actual (too-small) byte count: {err:?}");
        let message = err.message();
        assert!(message.contains(BUNDLED_FONT_ASSET_PATH), "failure message must name where the bundled font was expected: {message}");
        assert!(message.contains(ENV_VAR), "failure message must say how to self-serve via the env var: {message}");
        assert!(message.contains("--cjk-font"), "failure message must say how to self-serve via the CLI flag: {message}");
    }

    /// Minimal `std::env::temp_dir`-based unique-directory helper - no
    /// extra dependency (`tempfile`) for two tests that only need a
    /// throwaway directory with one file in it.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("archipelago-fonts-test-{}-{nanos}-{count}", std::process::id()));
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        TempDir(path)
    }
}
