//! `--screenshot`/`--screenshot-at-day` regression: the defect this guards
//! against produced a fully black PNG and still exited 0, for any small
//! target day - a verification tool silently reporting success while
//! capturing nothing (see this crate's own `app::screenshot` module doc,
//! `MIN_RENDER_WARMUP_FRAMES`, for the confirmed root cause: the primary
//! window's render pipeline is not warmed up on the very first frames after
//! startup, so a screenshot taken too early is a real, valid, entirely
//! black frame rather than a failure).
//!
//! `screenshot_at_small_day_is_not_black` below is a narrow, purely
//! mechanical check - "did the screenshot mechanism actually capture a
//! rendered frame at all", never "does the map look like Japan" or "is the
//! text legible". CLAUDE.md's own "検証についての教訓" records that pixel
//! statistics previously failed *as a proxy for readability* (every
//! statistic passed while the map was cut off-screen and the Japanese text
//! was tofu); asserting "not suspiciously all-black" makes no claim about
//! either of those and does not repeat that mistake - legibility and map
//! framing still need a human to look, same as always.
//!
//! `screenshot_at_day_captures_that_day_not_a_later_one` below is the fix
//! for a second, worse defect that fixing the black-frame one on its own
//! exposed: `--screenshot-at-day 5` stopped being black, but started
//! silently capturing day 200 instead - a fully-rendered, entirely
//! plausible-looking PNG of the *wrong* day, which the black-frame check
//! above cannot see (it only ever asks "is this non-black", and day 200's
//! render obviously is). See that test's own doc for how it tells the two
//! apart without any OCR/glyph-reading machinery.
//!
//! ## Runtime / how to run
//!
//! The two tests above spawn the real `archipelago-game` binary with a
//! real Bevy/wgpu window (needs a working X11 `DISPLAY`, same requirement
//! `--screenshot` itself always had), so - like every other test in this
//! suite - they are `#[ignore]`d:
//!
//! ```sh
//! cargo test -p archipelago-game --test screenshot_acceptance -- --ignored
//! ```
//!
//! The remaining two tests (`screenshot_at_day_past_days_is_rejected_at_startup`/
//! `screenshot_after_below_the_warmup_floor_is_rejected_at_startup`) check a
//! `main.rs` argument-parsing rejection that fires before any window is
//! ever opened, so they need no `DISPLAY` and run as ordinary tests.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

/// The three `#[ignore]`d tests below each spawn a real Bevy/wgpu window in
/// its own subprocess (`run_and_decode`'s own doc). Confirmed by hand: this
/// machine's GPU/window stack flakes when three of them run concurrently
/// (`cargo test -- --ignored`'s default parallelism runs every ignored test
/// as a thread in this same binary) - one run failed to write its PNG at
/// all, a second crashed with SIGSEGV - while the exact same three, one at a
/// time (`--test-threads=1`), pass cleanly every time. `Command::new` still
/// spawns a genuinely separate OS process for each window, so a plain
/// in-process `Mutex` shared across all three test *functions* is enough to
/// serialize them without requiring `--test-threads=1` to be remembered by
/// hand at every call site (including `CLAUDE.md`'s own documented `cargo
/// test -p archipelago-game --test scenario_acceptance -- --ignored`-style
/// invocations, none of which pass it).
static GUI_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Runs `archipelago-game --scenario scenarios/japan_hex.json --seed 2
/// --screenshot <tmp> <extra_args>` (via `CARGO_BIN_EXE_archipelago-game`,
/// guaranteed built by Cargo before this test runs because it references
/// that env var) and returns the decoded PNG it wrote, or panics naming the
/// process's own exit status/stderr if it did not exit 0 with a decodable
/// PNG at the path.
fn run_and_decode(out_name: &str, extra_args: &[String]) -> image::RgbImage {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_archipelago-game"));
    let out_path = std::env::temp_dir().join(out_name);
    let _ = std::fs::remove_file(&out_path);

    let mut args: Vec<String> = vec![
        "--scenario".to_string(),
        "../../scenarios/japan_hex.json".to_string(),
        "--seed".to_string(),
        "2".to_string(),
        "--screenshot".to_string(),
        out_path.to_str().expect("temp path must be valid UTF-8").to_string(),
    ];
    args.extend(extra_args.iter().cloned());

    let output = Command::new(&bin).args(&args).output().unwrap_or_else(|e| panic!("failed to launch {}: {e}", bin.display()));
    assert!(
        output.status.success(),
        "archipelago-game {args:?} exited with {} (stderr: {})",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    image::open(&out_path).unwrap_or_else(|e| panic!("{args:?} did not write a decodable PNG at {}: {e}", out_path.display())).into_rgb8()
}

/// Fraction of pixel positions that differ between two same-size images -
/// sampled every other pixel on each axis (a quarter of the full count,
/// still tens of thousands of samples at this resolution) since this only
/// ever needs to tell a ~0.3%-scale legitimate difference apart from a
/// ~5%-scale wrong-day one (see the test below), not locate individual
/// pixels.
fn diff_fraction(a: &image::RgbImage, b: &image::RgbImage) -> f32 {
    assert_eq!(a.dimensions(), b.dimensions(), "images captured from the same scenario/seed/window must be the same size");
    let (w, h) = a.dimensions();
    let mut total = 0u32;
    let mut diff = 0u32;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            total += 1;
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                diff += 1;
            }
            x += 2;
        }
        y += 2;
    }
    diff as f32 / total as f32
}

/// The defect this test reproduces: `--screenshot-at-day 5` used to write a
/// PNG that was 0.0000% non-black (confirmed by hand, `docs/` task writeup)
/// while still exiting 0. Any small target day races the same render-
/// pipeline warm-up, so day 5 alone is enough to pin the regression - it is
/// not a threshold this test needs to sweep.
#[test]
#[ignore = "spawns a real Bevy/wgpu window; needs a working X11 DISPLAY"]
fn screenshot_at_small_day_is_not_black() {
    let _guard = GUI_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let img = run_and_decode("archipelago_screenshot_acceptance_day5.png", &["--screenshot-at-day".to_string(), "5".to_string()]);
    let total = img.pixels().count();
    let non_black = img.pixels().filter(|p| p.0 != [0, 0, 0]).count();
    let fraction = non_black as f32 / total as f32;
    assert!(
        fraction > 0.5,
        "--screenshot-at-day 5 must capture a real rendered frame, not the window's own clear color: \
         got {:.4}% non-black pixels",
        fraction * 100.0
    );
}

/// The defect fixing the black-frame check above exposed: the render-warmup
/// floor (`app::screenshot::MIN_RENDER_WARMUP_FRAMES`) stops a capture
/// firing before frame 10, but on its own did nothing to stop the
/// *simulation* advancing while it waited - `AtDay` runs start at
/// `Speed::X20` (`app::run`'s own doc: 20 simulated days per client frame),
/// so by the time frame 10 arrived the world had ticked 200 days, not 5.
/// Confirmed by hand against this exact fixture: `--screenshot-at-day 5`
/// produced a fully-rendered (not black - it clears the check above) PNG
/// whose own top-bar text read "200日目 / 720 — 実行中（20x）". A
/// correct-looking screenshot of the wrong day is worse than the black PNG
/// it replaced (CLAUDE.md's own "検証についての教訓": a plausible-looking
/// measurement standing in for the property that actually matters), and the
/// old `screenshot_at_small_day_is_not_black` check above cannot see it -
/// day 200's render is exactly as non-black as day 5's.
///
/// This test asserts the *requested* day was captured, without any
/// OCR/glyph-reading of the rendered text (this repo has no such
/// machinery - `app::fonts`'s own tests check codepoint *coverage*, not
/// rendered pixel shapes). Instead it leans on the repo's own strongest
/// tool for this: determinism. `--screenshot-after <D>` (the older,
/// frame-counting trigger) starts an observing-only run unpaused at
/// `Speed::X1` (`app::run`'s own doc - the `X20`/`start_paused` overrides
/// only ever apply to `AtDay`), which ticks exactly one simulated day per
/// client frame - so for `D` at or past the render-warmup floor,
/// `--screenshot-after D` fires on frame `D`, which is world day `D`
/// exactly, on a trigger path this fix's `advance_simulation` change never
/// touches (its `day_target` is `None` whenever `ScreenshotConfig::trigger`
/// is `AfterFrames`). `crates/sim`'s own `determinism` test already
/// establishes that a fixed (scenario, seed) pair produces a bit-identical
/// `World` after `D` ticks no matter how those ticks are batched across
/// frames, so this reference and `--screenshot-at-day D`'s own capture must
/// show the *same simulated day* if the fix is doing what it claims.
///
/// They are not expected to be pixel-*identical* - `AtDay`'s own fix pauses
/// the run once day `D` is reached (this same task's change to
/// `sim_control::advance_simulation`), so its top status bar legitimately
/// reads "一時停止中" where the `AfterFrames` reference (which never
/// pauses) still reads "実行中（1x）"; that one differing string, confirmed
/// by hand, accounts for ~0.3% of pixels. A screenshot of the *wrong* day
/// is not a small, localized difference like that one: confirmed by hand
/// (temporarily forcing `advance_simulation`'s `day_target` to `None`,
/// reproducing the pre-fix behavior, then reverting before committing)
/// that `--screenshot-at-day 15` against this exact fixture then landed on
/// day 200 instead of day 15 - a ~4.9% difference against this same
/// reference, roughly sixteen times the legitimate status-bar-only gap.
/// `DIFF_THRESHOLD` sits well inside that gap, close to the legitimate side
/// of it, so it cannot pass on the wrong day.
#[test]
#[ignore = "spawns a real Bevy/wgpu window; needs a working X11 DISPLAY"]
fn screenshot_at_day_captures_that_day_not_a_later_one() {
    let _guard = GUI_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Must be at or past the render-warmup floor - see this test's own doc
    // for why `--screenshot-after DAY` only equals world day `DAY` exactly
    // from that point on. The `+ 5` margin keeps this comfortably past the
    // floor rather than sitting exactly on its boundary.
    let day = archipelago_game::app::MIN_RENDER_WARMUP_FRAMES + 5;
    const DIFF_THRESHOLD: f32 = 0.02; // 2%: between the ~0.3% legitimate gap and the ~4.9% wrong-day gap measured by hand (see this test's own doc).

    let reference = run_and_decode("archipelago_screenshot_acceptance_reference.png", &["--screenshot-after".to_string(), day.to_string()]);
    let at_day = run_and_decode("archipelago_screenshot_acceptance_atday.png", &["--screenshot-at-day".to_string(), day.to_string()]);

    let diff = diff_fraction(&reference, &at_day);
    assert!(
        diff < DIFF_THRESHOLD,
        "--screenshot-at-day {day} must capture day {day}'s own render (matching a --screenshot-after {day} \
         reference - the same simulated day reached via a code path this fix never touches - within {:.2}%, the \
         known status-bar-only difference), not some later day the simulation had already ticked past by the time \
         the render-warmup floor was satisfied: got {:.4}% of sampled pixels differing",
        DIFF_THRESHOLD * 100.0,
        diff * 100.0,
    );
}

/// The defect `MIN_RENDER_WARMUP_FRAMES` alone used to reintroduce on a
/// slower GPU/driver/cold shader cache: a frame count is not a
/// render-readiness signal, so a capture that fires before the pipeline is
/// actually ready is still a real, valid, entirely black PNG - and the old
/// single-shot `maybe_capture_screenshot` would write it out and exit 0
/// regardless. `app::screenshot::handle_screenshot_captured` is supposed to
/// catch this by inspecting the pixels themselves (`is_blank`) before ever
/// calling a capture a success, retrying up to `MAX_BLANK_CAPTURE_ATTEMPTS`
/// times and then failing loudly.
///
/// `std::process::exit` can't be exercised by calling into the tool
/// in-process (it would kill the test runner), and this real machine's
/// pipeline reliably finishes warming up within a handful of frames
/// (`MIN_RENDER_WARMUP_FRAMES`'s own doc), so there is no way to make a real
/// GPU stay blank for the whole `MAX_BLANK_CAPTURE_ATTEMPTS` budget by
/// waiting - the tool would simply recover on retry, which is the correct
/// behavior, not the defect. `--debug-force-blank-screenshot`
/// (`ScreenshotConfig::debug_force_blank`'s own doc) reproduces the defect's
/// actual precondition honestly instead: it fires the very first capture
/// attempt on frame 1 (well inside the real, measured black-frame window)
/// with the retry budget shrunk to 1, i.e. zero retries - the same shape of
/// race the original bug report hit, just with the tolerance for it removed
/// rather than the GPU slowed down. This exercises the exact same
/// `is_blank`/`handle_screenshot_captured` code path a real slow pipeline
/// would.
///
/// Confirmed this can fail: before this task's fix, this flag did not exist
/// and `maybe_capture_screenshot` had no blank check at all - it would have
/// written the black PNG from frame 1 and exited 0, failing the assertions
/// below (a `0` exit status, and a written file).
#[test]
#[ignore = "spawns a real Bevy/wgpu window; needs a working X11 DISPLAY"]
fn a_capture_that_never_becomes_non_blank_is_refused_not_written() {
    let _guard = GUI_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_archipelago-game"));
    let out_path = std::env::temp_dir().join("archipelago_screenshot_acceptance_forced_blank.png");
    let _ = std::fs::remove_file(&out_path);

    let output = Command::new(&bin)
        .args([
            "--scenario",
            "../../scenarios/japan_hex.json",
            "--seed",
            "2",
            "--screenshot",
            out_path.to_str().expect("temp path must be valid UTF-8"),
            "--screenshot-at-day",
            "0",
            "--debug-force-blank-screenshot",
        ])
        .output()
        .unwrap_or_else(|e| panic!("failed to launch {}: {e}", bin.display()));

    assert!(
        !output.status.success(),
        "a capture that never becomes non-blank must exit non-zero, not silently succeed - got status {} \
         (stderr: {})",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("uniform colour") || stderr.contains("blank"),
        "the failure must say what went wrong (a blank/uniform-colour capture), got: {stderr}"
    );
    assert!(
        !out_path.exists(),
        "a refused capture must never write a file at all - found one at {}",
        out_path.display()
    );
}

/// `--screenshot-at-day <day>` past `--days <n>` can never be reached: past
/// day `n` the simulation's own `Outcome` goes `Stalemate` and `world.day`
/// never advances again (`archipelago_sim::sim::Simulation::outcome`), so
/// nothing would ever satisfy the trigger - `maybe_capture_screenshot`
/// would wait forever and the process would hang rather than exit.
/// `main.rs` rejects this at argument-parsing time instead
/// (docs/conventions.md's fail-fast/no-fallback rule), before
/// `archipelago_game::app::run` ever opens a window - so, unlike the two
/// tests above, this needs no `DISPLAY` and is not `#[ignore]`d.
#[test]
fn screenshot_at_day_past_days_is_rejected_at_startup() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_archipelago-game"));
    let out_path = std::env::temp_dir().join("archipelago_screenshot_acceptance_unreachable_day.png");
    let _ = std::fs::remove_file(&out_path);

    let output = Command::new(&bin)
        .args(["--days", "10", "--screenshot", out_path.to_str().expect("temp path must be valid UTF-8"), "--screenshot-at-day", "20"])
        .output()
        .unwrap_or_else(|e| panic!("failed to launch {}: {e}", bin.display()));

    assert!(
        !output.status.success(),
        "--screenshot-at-day 20 combined with --days 10 must be rejected at startup, not left to run/hang forever \
         waiting for a day the simulation can never reach"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("20") && stderr.contains("10"),
        "the rejection must name both the requested day and the --days cap so the mistake is obvious, got: {stderr}"
    );
    assert!(!out_path.exists(), "a rejected run must not have written a screenshot at all");
}

/// `--screenshot-after <frames>` below the render pipeline's own warm-up
/// floor (`app::screenshot::MIN_RENDER_WARMUP_FRAMES`) can never actually
/// be honored: `maybe_capture_screenshot` will not fire before that floor
/// regardless of what was asked for, so silently capturing at the floor
/// instead would be the exact same "quietly captured a different frame
/// than the one requested" shape as the `AtDay` defect this task fixes -
/// just for a frame count instead of a day. Rejected the same way and at
/// the same place as the `AtDay` case above, so this needs no `DISPLAY`
/// either.
#[test]
fn screenshot_after_below_the_warmup_floor_is_rejected_at_startup() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_archipelago-game"));
    let out_path = std::env::temp_dir().join("archipelago_screenshot_acceptance_too_early.png");
    let _ = std::fs::remove_file(&out_path);

    let floor = archipelago_game::app::MIN_RENDER_WARMUP_FRAMES;
    let too_few = (floor - 1).to_string();
    let output = Command::new(&bin)
        .args(["--screenshot", out_path.to_str().expect("temp path must be valid UTF-8"), "--screenshot-after", &too_few])
        .output()
        .unwrap_or_else(|e| panic!("failed to launch {}: {e}", bin.display()));

    assert!(
        !output.status.success(),
        "--screenshot-after {too_few} (below the warm-up floor of {floor}) must be rejected at startup, not \
         silently rounded up to a frame count nobody asked for"
    );
    assert!(!out_path.exists(), "a rejected run must not have written a screenshot at all");
}
