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

    // -------------------------------------------------------------------
    // Scenario-acceptance property: "the bundled font actually covers the
    // characters the game renders" (see `apps/headless/tests/
    // scenario_acceptance.rs`'s module doc for the rest of this suite, and
    // for what it explicitly cannot cover - legibility/aesthetics need a
    // human, not a test). The original defect this guards against: every
    // Japanese string in this entirely-Japanese game rendering as a tofu
    // box, discovered by looking at the running client, not by any of the
    // 236 pre-existing tests (none of which ever asked whether the font
    // this binary ships actually *has a glyph* for the text it draws).
    //
    // This crate stays dependency-free beyond `bevy` itself
    // (docs/conventions.md §4/"no new dependencies") - `read-fonts`/
    // `skrifa` already sit in `Cargo.lock` transitively through
    // `bevy_text`, but adding either as a *direct* dependency here would
    // still be a new line in this crate's own `Cargo.toml`. So this parses
    // just enough of the OpenType `cmap` table by hand (format 12, the
    // full-Unicode "segmented coverage" subtable every Windows-targeting
    // font - `NotoSansJP-VariableFont_wght.ttf` included, confirmed via a
    // one-off dump during development) publishes at (platform 3, encoding
    // 10) - std-only, no font-parsing dependency anywhere in this crate.
    // -------------------------------------------------------------------

    /// Reads a big-endian `u16` at `data[at..]`, or `None` if that's out of
    /// bounds - every parser below fails closed (returns `None`/`false`)
    /// rather than panicking on a malformed/truncated font, since a test
    /// helper crashing the whole suite on bad input would be a worse
    /// failure mode than just reporting "not covered".
    fn u16_at(data: &[u8], at: usize) -> Option<u16> {
        data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32_at(data: &[u8], at: usize) -> Option<u32> {
        data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Finds the byte offset (from the start of `data`) of this font's
    /// `cmap` format-12 subtable for (platform 3 "Windows", encoding 10
    /// "UCS-4") - the subtable that actually covers the full Unicode range
    /// this game's Japanese text needs (kanji well outside the BMP-only
    /// format-4 subtable every OpenType font also carries for compatibility).
    /// `None` means either no `cmap` table, or no such subtable - both
    /// treated as "nothing is covered" by `covers`, not a panic.
    fn find_cmap_format12_subtable(data: &[u8]) -> Option<usize> {
        let num_tables = u16_at(data, 4)? as usize;
        let mut dir_off = 12;
        let mut cmap_off = None;
        for _ in 0..num_tables {
            let tag = data.get(dir_off..dir_off + 4)?;
            if tag == b"cmap" {
                cmap_off = Some(u32_at(data, dir_off + 8)? as usize);
                break;
            }
            dir_off += 16;
        }
        let cmap_off = cmap_off?;

        let num_subtables = u16_at(data, cmap_off + 2)? as usize;
        let mut sub_off = cmap_off + 4;
        for _ in 0..num_subtables {
            let platform_id = u16_at(data, sub_off)?;
            let encoding_id = u16_at(data, sub_off + 2)?;
            let offset = u32_at(data, sub_off + 4)? as usize;
            if platform_id == 3 && encoding_id == 10 {
                let subtable_off = cmap_off + offset;
                if u16_at(data, subtable_off)? == 12 {
                    return Some(subtable_off);
                }
            }
            sub_off += 8;
        }
        None
    }

    /// Whether `data`'s (3,10) format-12 `cmap` subtable at `subtable_off`
    /// has a mapped glyph for codepoint `cp` - linear scan over the
    /// subtable's `(startCharCode, endCharCode, startGlyphID)` groups
    /// (OpenType spec, `cmap` format 12). A font this size has at most a
    /// few hundred groups, so this is plenty fast for a handful of test
    /// strings; nothing here runs outside `#[cfg(test)]`.
    fn format12_covers(data: &[u8], subtable_off: usize, cp: u32) -> bool {
        let Some(num_groups) = u32_at(data, subtable_off + 12) else { return false };
        let mut group_off = subtable_off + 16;
        for _ in 0..num_groups {
            let (Some(start), Some(end)) = (u32_at(data, group_off), u32_at(data, group_off + 4)) else {
                return false;
            };
            if (start..=end).contains(&cp) {
                return true;
            }
            group_off += 12;
        }
        false
    }

    /// Self-test for the hand-rolled parser above, independent of anything
    /// this game renders: a real font must cover plain ASCII and common
    /// kanji, and must *not* claim to cover an unassigned Private Use Area
    /// codepoint - the negative case is what proves this is actually
    /// reading the font's real coverage data rather than trivially
    /// returning `true` for everything.
    #[test]
    fn cmap_format12_parser_reads_real_coverage_from_the_bundled_font() {
        let subtable = find_cmap_format12_subtable(BUNDLED_FONT_BYTES).expect("the bundled font must have a (3,10) format-12 cmap subtable");
        assert!(format12_covers(BUNDLED_FONT_BYTES, subtable, 'A' as u32), "a real CJK font must still cover plain ASCII");
        assert!(format12_covers(BUNDLED_FONT_BYTES, subtable, '近' as u32), "must cover a common kanji actually used by this game's faction/region names");
        assert!(
            !format12_covers(BUNDLED_FONT_BYTES, subtable, '\u{E000}' as u32),
            "must NOT claim coverage for an unassigned Private Use Area codepoint - a parser that always returns true would pass every real assertion vacuously"
        );
    }

    /// Every character actually drawn by a representative slice of this
    /// game's real UI systems, run against a real `bevy::ecs::World` the
    /// same way `ui::tests`/`panels::tests` already drive their own
    /// systems - not a scan of raw scenario JSON. `japan_hex` (289 regions,
    /// 8 factions, the richest name/vocabulary set of the three shipped
    /// scenarios) is driven for a few in-game days so the event log picks
    /// up real battle/political Japanese text
    /// (`sim_control::advance_simulation` -> `event_text::format_event`),
    /// not just static names.
    ///
    /// Every region/faction *name* in the scenario is unioned in directly
    /// from `World` data (not just whichever one happened to be selected
    /// during this run) - `Tab`/region-clicks make every single one of them
    /// reachable in the real client, so font coverage has to hold for all
    /// 289 of them, not only the lucky handful this test's own systems
    /// happened to render.
    ///
    /// Checked this fails when broken: temporarily pointed the coverage
    /// check at `plausible_bundled_bytes()` (a synthetic stand-in with no
    /// real `cmap` table at all) instead of `BUNDLED_FONT_BYTES` - the
    /// assertion then failed listing 225 distinct characters as uncovered,
    /// starting with `'中'`/`'部'`/`'同'`/`'盟'` (faction name 中部同盟) -
    /// i.e. every non-control character this fixture actually rendered.
    /// Reverted before committing - see this test's own body for why the
    /// real bundled font is what must be checked, not a stand-in.
    #[test]
    fn bundled_font_covers_every_character_the_running_game_actually_renders() {
        use archipelago_sim::ids::FactionId;
        use archipelago_sim::scenario;

        use crate::app::{
            EventLog, EventLogText, FactionPanelText, InspectText, LastRejection, MenuRegion, NewspaperState, PlayerFaction, PlayerPanelText,
            ScenarioMeta, SelectedFaction, SelectedRegion, SelectedUnits, SimRes, SpeedRes, TopBarPlayerStatsText, TopBarText,
        };
        use crate::sim_driver::{SimDriver, Speed};

        // Same pattern `ui::tests`/`panels::tests` each already duplicate
        // locally for driving one plain-function system against a `World`
        // with no full `App`/schedule involved.
        fn run<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
            let mut system = IntoSystem::into_system(system);
            system.initialize(world);
            system.run((), world).unwrap();
        }

        let world_data = scenario::load_file("../../scenarios/japan_hex.json").expect("scenarios/japan_hex.json must load");
        let player_faction = FactionId(0);
        let region_names: Vec<String> = world_data.regions.iter().map(|r| r.name.clone()).collect();
        let faction_names: Vec<String> = world_data.factions.iter().map(|f| f.name.clone()).collect();
        let some_region = world_data.regions.first().map(|r| r.id).expect("japan_hex must have at least one region");

        let mut world = World::new();
        world.insert_resource(SimRes(SimDriver::new_with_player(world_data, 2, Some(player_faction), None)));
        world.insert_resource(SpeedRes { last_active: Speed::X1, paused: false });
        world.insert_resource(ScenarioMeta { name: "日本ヘクスマップ（テスト用）".to_string(), max_days: 720 });
        world.insert_resource(EventLog::default());
        world.insert_resource(LastRejection::default());
        world.insert_resource(NewspaperState::default());
        world.insert_resource(PlayerFaction(Some(player_faction)));
        world.insert_resource(SelectedFaction(player_faction));
        world.insert_resource(SelectedRegion(Some(some_region)));
        world.insert_resource(SelectedUnits::default());
        world.insert_resource(MenuRegion::default());

        world.spawn((Text::new(String::new()), TopBarText));
        world.spawn((Text::new(String::new()), TopBarPlayerStatsText));
        world.spawn((Text::new(String::new()), FactionPanelText));
        world.spawn((Text::new(String::new()), PlayerPanelText));
        world.spawn((Text::new(String::new()), InspectText));
        world.spawn((Text::new(String::new()), EventLogText));

        // A few real in-game days, through the actual per-frame system
        // (`sim_control::advance_simulation`) - populates `EventLog` with
        // real Japanese event text, not just static names.
        for _ in 0..20 {
            run(&mut world, super::super::sim_control::advance_simulation);
        }

        run(&mut world, super::super::ui::update_top_bar);
        run(&mut world, super::super::ui::update_top_bar_player_stats);
        run(&mut world, super::super::ui::update_faction_panel);
        run(&mut world, super::super::ui::update_player_panel);
        run(&mut world, super::super::ui::update_inspect_panel);
        run(&mut world, super::super::ui::update_event_log);

        let mut rendered_text = String::new();
        let mut texts = world.query::<&Text>();
        for text in texts.iter(&world) {
            rendered_text.push_str(&text.0);
        }
        for name in region_names.iter().chain(faction_names.iter()) {
            rendered_text.push_str(name);
        }

        let subtable = find_cmap_format12_subtable(BUNDLED_FONT_BYTES).expect("the bundled font must have a (3,10) format-12 cmap subtable");
        let mut missing: Vec<char> = Vec::new();
        for ch in rendered_text.chars() {
            // Whitespace/control characters carry no visible glyph
            // requirement of their own (a font not covering U+000A isn't a
            // "tofu box" bug) - every other character, ASCII included, is
            // checked exactly like every Japanese one.
            if ch.is_control() {
                continue;
            }
            if !format12_covers(BUNDLED_FONT_BYTES, subtable, ch as u32) && !missing.contains(&ch) {
                missing.push(ch);
            }
        }
        assert!(
            missing.is_empty(),
            "the bundled font is missing a glyph for {} character(s) actually rendered by the game: {:?} - these would draw as tofu boxes",
            missing.len(),
            missing
        );
    }
}
