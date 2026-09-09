//! `--screenshot <path>` support: captures the primary window to a PNG and
//! exits with status 0. Exists so the client can be verified without a
//! human at the screen or a screenshot tool on the host (see this crate's
//! own doc comment / docs/phase7-spec.md "Stage 7A の受け入れ基準" - this
//! machine has none of `import`/`scrot`/`grim`/`maim`/`xwd`/
//! `gnome-screenshot`, so Bevy's own screenshot capability is the only way
//! to confirm the window actually draws pixels).
//!
//! This module only *reads* `SimRes` indirectly (by existing on the same
//! frame timeline as `sim_control::advance_simulation`, which already runs
//! every unpaused frame) - it never calls `SimDriver::tick` itself, so it
//! cannot touch the determinism invariant.

use bevy::app::AppExit;
use bevy::ecs::message::MessageWriter;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};

use archipelago_sim::good::Good;
use archipelago_sim::ids::RegionId;

use super::{MainCamera, MapMode, PlayerFaction, RegionLayout, SelectedUnits, SimRes};

/// `--screenshot-after <frames>` / `--screenshot-at-day <day>` (`main.rs`'s
/// own doc): when the automated capture fires.
///
/// `AfterFrames` (Stage 7A's original, unchanged behavior) counts client
/// frames since startup - which tracks game *days* only when the frame rate
/// and `Speed` happen to hold steady, so it cannot reliably aim a shot at a
/// specific day (confirmed against a real play session: the same
/// `--screenshot-after` value landed on different days across runs).
/// `AtDay` instead waits for `SimRes::world().day` to actually reach the
/// target - the same day number a player would name when describing what
/// they saw, and the target `app::run` reads to decide whether this run
/// needs to advance unattended in the first place (see `app::run`'s own
/// doc for that half).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotTrigger {
    AfterFrames(u32),
    AtDay(u32),
}

/// Present as a resource only when `--screenshot <path>` was given -
/// `app::run` inserts this conditionally, so `maybe_capture_screenshot`
/// simply no-ops on every frame when it isn't there.
///
/// `open_*`/`map_mode`/`camera_focus_region`/`camera_zoom` are
/// verification-only conveniences (`--debug-*` flags, `main.rs`'s own doc) -
/// there is no interactive way to press `M`/`D`/`N` or scroll-zoom before a
/// screenshot fires when nothing is at the keyboard, so `app::run` reads
/// these once at startup to pre-set the corresponding resource/camera
/// framing instead. They never affect anything but initial UI-panel
/// visibility/toggle state or the camera's own `Transform` - not scenario,
/// seed, or any simulated day's outcome.
#[derive(Resource, Clone)]
pub struct ScreenshotConfig {
    pub path: String,
    pub trigger: ScreenshotTrigger,
    pub open_diplomacy: bool,
    pub open_newspaper: bool,
    /// `--debug-open-policy`: Stage 8B's policy panel, same rationale as
    /// `open_diplomacy`/`open_newspaper` - no keyboard at a screenshot run
    /// to press `P` first.
    pub open_policy: bool,
    /// `--debug-map-mode <key>`: which `MapMode` to start in (`political`/
    /// `terrain`/`population`/`industry`/`unrest`/`supply`, `MapMode::key()`),
    /// so any mode - the old standalone supply overlay included, now
    /// `MapMode::Supply` - can be captured unattended. `None` keeps the
    /// ordinary `MapMode::default()` (`Political`) a live run always starts
    /// in.
    pub map_mode: Option<MapMode>,
    /// `--debug-map-mode industry:<good>`: which `Good` an `Industry`-mode
    /// screenshot run starts showing (`main.rs`'s own `good_from_key`) -
    /// `MapMode` alone has no room to carry this (it names a *mode*, not a
    /// mode-plus-commodity), so it rides alongside `map_mode` as its own
    /// field instead. `None` (a bare `industry`, or any other mode) keeps
    /// `ActiveGood::default()` (`Steel`) - the same defined default a live
    /// run always starts with, `--debug-map-mode` given or not.
    pub industry_good: Option<Good>,
    /// `--debug-select-region <region>`: pre-selects a region so Stage 8B's
    /// region panel (action buttons included) renders in a `--screenshot`
    /// run with nothing to click it open.
    pub select_region: Option<RegionId>,
    /// `--debug-select-units`: keeps `SelectedUnits` matching every
    /// *currently* living unit the `--play`ed faction owns, so Stage 8B's
    /// unit panel renders in a `--screenshot` run with nothing to click a
    /// unit marker. No effect without `--play`.
    ///
    /// `sync_debug_selected_units` recomputes this every frame from
    /// `SimRes`'s live world rather than snapshotting it once at startup -
    /// this task's own finding: a `--screenshot-at-day <D>` run for `D > 0`
    /// (the only way to screenshot a squadron at all, since none of this
    /// crate's own scenarios start with one - `docs/phase10-spec.md`'s own
    /// "シナリオに航空部隊を置くのは 10B 以降でよい") ticks the world well past
    /// day 0 before the shot fires, and a one-time day-0 snapshot goes stale
    /// long before then: the day-0 roster may since have died (`--delegate-
    /// military` fighting a real war) and can, by construction, never have
    /// contained a unit recruited afterward. Confirmed by screenshot before
    /// this fix: `--play 関東府 --delegate-military --screenshot-at-day 200
    /// --debug-select-units` rendered the unit panel's own title with zero
    /// rows beneath it - every day-0 unit id in the stale selection had
    /// since died, and the 21 units 関東府 actually owned by day 200
    /// (squadrons included) were never in it to begin with.
    pub select_units: bool,
    /// `--debug-camera-region <index>`: center the camera on this region
    /// instead of the whole-map fit `camera_fit::fit_camera_to_map` computes
    /// by default - a dense map (japan47's 47 prefectures) can render a
    /// short link's overlay color at only a few pixels wide at whole-map
    /// zoom, exactly what a real player would Ctrl+scroll in on
    /// (`input::mouse_pan_zoom`) but a screenshot run has no mouse or
    /// keyboard at all.
    pub camera_focus_region: Option<RegionId>,
    /// `--debug-camera-zoom <scale>`: the orthographic projection's `scale`
    /// to use with `camera_focus_region` - smaller is closer in. Ignored
    /// without `camera_focus_region`.
    pub camera_zoom: f32,
    /// `--debug-force-blank-screenshot` (hidden verification-only flag, not
    /// something a real capture should ever pass): shrinks both the
    /// render-warmup floor and the blank-retry budget down to 1 for this run
    /// only, so `maybe_capture_screenshot` fires on literally the first
    /// frame with zero retries allowed - landing inside the real, measured
    /// black-frame window (`MIN_RENDER_WARMUP_FRAMES`'s own doc: frame ≤2
    /// came out fully black, five runs each, on this machine) without
    /// needing an actually slow/broken GPU to prove it. Exists solely so
    /// `screenshot_acceptance.rs` can demonstrate, against the real binary,
    /// that a blank capture is refused (non-zero exit, no file written)
    /// rather than silently written out - `std::process::exit` can't be
    /// exercised from an in-process test, so this is the honest way to
    /// reproduce the historical defect's precondition on demand. `false`
    /// (every real run) keeps both constants at their production value.
    pub debug_force_blank: bool,
}

/// Overrides the camera's framing every frame once `ScreenshotConfig::
/// camera_focus_region` names a region, running after `camera_fit::
/// fit_camera_to_map`'s own one-time whole-map fit so it always wins - see
/// `ScreenshotConfig`'s own doc for why this exists at all. A no-op
/// whenever there's no `ScreenshotConfig`, or it named no region (the
/// overwhelmingly common case - every `--screenshot` run before Stage 7C
/// used the default whole-map fit unchanged).
pub(super) fn apply_debug_camera(
    config: Option<Res<ScreenshotConfig>>,
    layout: Res<RegionLayout>,
    mut camera: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
) {
    let Some(config) = config else { return };
    let Some(region) = config.camera_focus_region else { return };
    let Some(&[x, y]) = layout.0.get(region.index()) else { return };
    let Ok((mut transform, mut projection)) = camera.single_mut() else { return };
    let Projection::Orthographic(ortho) = &mut *projection else { return };
    transform.translation.x = x;
    transform.translation.y = y;
    ortho.scale = config.camera_zoom;
}

/// Defect fix (`--screenshot-at-day <D>` for small `D` used to write a
/// fully black PNG and exit 0): the primary window's swap-chain surface and
/// sprite/mesh render pipelines are not ready on the first frames after
/// startup - `bevy_render`'s own `submit_screenshot_commands` silently
/// skips a screenshot request whenever `ExtractedWindow::
/// swap_chain_texture_view` is still `None`, and even once the surface
/// exists, `PipelineCache`'s async shader specialization means the very
/// first frames actually presented can render nothing but the window's
/// clear color - so a screenshot taken too early captures a real, valid,
/// entirely black frame rather than failing.
///
/// Confirmed, not assumed: `--screenshot-after <n>` against this same
/// build/machine/scenario was run five times each at `n = 2` and `n = 3` -
/// `n = 2` produced a 0.0000%-non-black PNG in all five runs, `n = 3`
/// produced a 100%-non-black PNG in all five runs. The boundary is exactly
/// this sharp and perfectly repeatable (not a flaky race that "usually"
/// clears by some frame), which is what makes a frame-count floor the right
/// fix here rather than a guessed sleep: it is compensating for a fixed
/// number of engine warm-up frames, not for indeterminate timing. This also
/// explains the original bug report precisely - `ScreenshotTrigger::AtDay`
/// runs at `Speed::X20` (`mod::run`'s own doc), so `--screenshot-at-day 5`
/// always fires on frame 1 (day 5 is already behind day 20 by the end of
/// that first frame's tick loop) and `--screenshot-at-day 100` fires on
/// frame 5 - comfortably past the floor, which is why only small `D` ever
/// showed the defect.
///
/// Set well above the observed 2-frame failure/3-frame success boundary as
/// a margin for a slower GPU/driver than this one, while staying cheap in
/// wall-clock time regardless (a handful of frames, not a fixed sleep).
///
/// **Necessary, not sufficient** (`codex review`, P2): this floor was
/// originally the *only* guard, chosen from a boundary measured on one
/// machine. A frame count is not a render-readiness signal - on a slower
/// GPU, a software renderer, or a cold shader cache, async pipeline
/// compilation can take longer than this many frames, and the tool would go
/// right back to writing a black PNG and exiting 0. It is kept as a cheap
/// first filter (no point even trying before the fastest machine this was
/// ever measured on would be ready), but `MAX_BLANK_CAPTURE_ATTEMPTS` below
/// is what actually guarantees a blank frame is never mistaken for success.
///
/// `pub`, not private: `main.rs` validates `--screenshot-after <frames>`
/// against this same floor at parse time (see its own call site) - a
/// requested frame count this module could never actually honor (the
/// render pipeline simply isn't up yet) must be rejected loudly, not
/// silently rounded up to this value the way `maybe_capture_screenshot`'s
/// own `ready` check below does internally for `AtDay`. Kept as the same
/// single constant rather than a second copy so the two can never drift
/// apart.
pub const MIN_RENDER_WARMUP_FRAMES: u32 = 10;

/// How many times a capture that comes back as a single uniform colour
/// (`is_blank`'s own doc - nothing was drawn on top of the window's
/// `ClearColor`) may be retried before `handle_screenshot_captured` gives up
/// and fails loudly instead of writing it out. `MIN_RENDER_WARMUP_FRAMES`
/// alone cannot bound how long real pipeline warm-up takes (its own doc), so
/// this is the actual guarantee: every retry costs only a few frames plus
/// one GPU round-trip, so a budget this size is still cheap in wall-clock
/// time even on a slow machine, while remaining a hard, finite bound rather
/// than an unbounded wait - conventions.md's fail-fast rule means a pipeline
/// that is *still* not ready after 30 fresh attempts gets reported as broken
/// rather than waited on forever.
pub const MAX_BLANK_CAPTURE_ATTEMPTS: u32 = 30;

/// Cross-attempt state shared between `maybe_capture_screenshot` (which
/// decides *when* to spawn a capture) and `handle_screenshot_captured`
/// (which decides, once one lands, whether to accept it, retry, or fail) -
/// a plain `Local` cannot do this job because each retry spawns a new
/// `Screenshot` entity with its own observer instance, so the two systems
/// have no other state in common to coordinate through. Always present
/// (`app::run` calls `init_resource` for it unconditionally, like most
/// resources here) - inert whenever there is no `ScreenshotConfig` to make
/// `maybe_capture_screenshot` do anything with it.
#[derive(Resource, Default)]
pub(super) struct ScreenshotAttempts {
    /// Set the instant a `Screenshot` request is spawned; cleared once its
    /// `ScreenshotCaptured` observer resolves it. Stops
    /// `maybe_capture_screenshot` from spawning a second, concurrent request
    /// while the first one's GPU readback is still in flight.
    in_flight: bool,
    /// How many captures so far have come back blank. Compared against
    /// `MAX_BLANK_CAPTURE_ATTEMPTS` by `handle_screenshot_captured`.
    blank_count: u32,
}

/// Once `ScreenshotConfig::trigger` fires - `AfterFrames(n)`: this many
/// client frames have elapsed; `AtDay(d)`: the simulated day has reached
/// `d` (read from `SimRes`, already advanced this frame by `sim_control::
/// advance_simulation`, which this system runs `.after(..)` - `app::run`'s
/// own doc) - *and* at least the render-warmup floor has elapsed either way
/// (`MIN_RENDER_WARMUP_FRAMES`'s own doc; `ScreenshotConfig::debug_force_blank`
/// shrinks this to 1 for testing) - spawns a `Screenshot` of the primary
/// window, guarded by `ScreenshotAttempts::in_flight` so it never spawns a
/// second one while an earlier attempt's GPU readback is still pending.
///
/// This can run again after a "failed" attempt: `handle_screenshot_captured`
/// clears `in_flight` (without setting a permanent latch) whenever it
/// rejects a capture as blank and retries remain, so this system simply
/// spawns another one the very next frame - `target_reached`/the warm-up
/// floor still hold from before, they never un-become true.
pub(super) fn maybe_capture_screenshot(
    mut commands: Commands,
    config: Option<Res<ScreenshotConfig>>,
    sim: Res<SimRes>,
    mut frame_count: Local<u32>,
    mut attempts: ResMut<ScreenshotAttempts>,
) {
    let Some(config) = config else {
        return;
    };
    if attempts.in_flight {
        return;
    }
    *frame_count += 1;
    let target_reached = match config.trigger {
        ScreenshotTrigger::AfterFrames(frames) => *frame_count >= frames,
        // `>=`, not `==`: at `Speed::X5`/`X20` several days tick within one
        // frame (`sim_control`'s own module doc - ticks per frame, not per
        // second), so the exact target day can be stepped over inside a
        // single frame rather than ever landing on it precisely.
        ScreenshotTrigger::AtDay(day) => sim.0.world().day >= day,
    };
    let warmup_floor = MIN_RENDER_WARMUP_FRAMES;
    let ready = target_reached && *frame_count >= warmup_floor;
    if !ready {
        return;
    }
    attempts.in_flight = true;

    commands.spawn(Screenshot::primary_window()).observe(handle_screenshot_captured);
}

/// The single place that decides whether a captured frame is real. Fires
/// once per spawned `Screenshot` request, asynchronously, once its GPU
/// readback lands (`ScreenshotCaptured`'s own doc - a few frames after
/// `maybe_capture_screenshot` spawned it).
///
/// Defect fix (`codex review`, P2): `MIN_RENDER_WARMUP_FRAMES` alone is a
/// frame count, not a render-readiness signal, so it cannot bound how long
/// async pipeline compilation actually takes on a slower GPU/driver/cold
/// shader cache - and a screenshot taken too early is a real, valid, entirely
/// black PNG (`MIN_RENDER_WARMUP_FRAMES`'s own doc), not a decode failure, so
/// nothing about the file itself would ever say a capture went wrong. This
/// closes that gap by inspecting the actual pixels before ever calling this
/// a success: `is_blank` checks whether the whole frame is a single uniform
/// colour - the shape a "nothing was drawn yet" frame necessarily has,
/// regardless of what colour `ClearColor` happens to be - and only a
/// genuinely blank capture is retried/rejected; anything with real variation
/// in it (map terrain, borders, the top status bar) is accepted immediately.
///
/// A blank result is retried, not failed immediately, up to
/// `MAX_BLANK_CAPTURE_ATTEMPTS` times (clearing `ScreenshotAttempts::
/// in_flight` so `maybe_capture_screenshot` spawns the next one) - a
/// slow-but-eventually-ready pipeline should not be treated as broken just
/// because it missed the fixed warm-up floor. Only once that budget is
/// exhausted does this exit non-zero with a message naming what happened,
/// per docs/conventions.md's fail-fast/no-fallback rule: **never** silently
/// write the blank frame out and exit 0, which is exactly the defect this
/// whole mechanism exists to close.
fn handle_screenshot_captured(
    captured: On<ScreenshotCaptured>,
    config: Res<ScreenshotConfig>,
    mut attempts: ResMut<ScreenshotAttempts>,
    mut exit: MessageWriter<AppExit>,
) {
    let dyn_img = match captured.image.clone().try_into_dynamic() {
        Ok(img) => img,
        Err(e) => {
            eprintln!(
                "archipelago-game: error: --screenshot capture could not be decoded ({e}) - refusing to \
                 write anything rather than guess what it was."
            );
            std::process::exit(1);
        }
    };
    // The exact conversion `save_rgb8` below will save - discards the alpha
    // channel HDR stores brightness in, same as `bevy_render`'s own
    // `save_to_disk` used to - so blankness is judged on the very bytes that
    // would be written, never on some other representation of the frame.
    let rgb = dyn_img.to_rgb8();

    // `codex review` (P2, twice): the forced-blank hook has to hand
    // `is_blank` a genuinely blank frame, not short-circuit around it.
    //
    // Capturing early and hoping the frame happens to be empty made the test
    // pass or fail on how warm the machine's shader cache was; classifying
    // every frame as blank when the flag is set made it pass even with the
    // pixel detector broken. Neither tests what it claims. Substituting a
    // uniform buffer of the real frame's own dimensions reproduces exactly
    // what a "nothing drawn yet" readback looks like, deterministically, and
    // leaves the production `is_blank` as the thing that decides.
    let rgb = if config.debug_force_blank {
        image::RgbImage::from_pixel(rgb.width(), rgb.height(), image::Rgb([0, 0, 0]))
    } else {
        rgb
    };

    if is_blank(&rgb) {
        attempts.blank_count += 1;
        let max_attempts = if config.debug_force_blank { 1 } else { MAX_BLANK_CAPTURE_ATTEMPTS };
        if attempts.blank_count >= max_attempts {
            eprintln!(
                "archipelago-game: error: --screenshot captured a single uniform colour {} time(s) in a row \
                 (limit {max_attempts}) - the render pipeline never actually drew anything (see \
                 app::screenshot::MAX_BLANK_CAPTURE_ATTEMPTS's own doc). Refusing to write a blank PNG; no \
                 screenshot was saved to {}.",
                attempts.blank_count, config.path,
            );
            std::process::exit(1);
        }
        // Let `maybe_capture_screenshot` spawn another attempt next frame -
        // the target/warm-up floor it checks both still hold from before.
        attempts.in_flight = false;
        return;
    }

    if let Err(e) = save_rgb8(&rgb, &config.path) {
        eprintln!("archipelago-game: error: could not save screenshot to {}: {e}", config.path);
        std::process::exit(1);
    }
    exit.write(AppExit::Success);
}

/// True when every pixel in `img` is exactly the same colour - the shape a
/// frame necessarily has when nothing was ever drawn onto the window's own
/// `ClearColor` (`handle_screenshot_captured`'s own doc). Checks *every*
/// pixel against the first, not a sampled non-black fraction
/// (`screenshot_acceptance.rs`'s own `screenshot_at_small_day_is_not_black`
/// test does that instead, at the PNG level): a real rendered frame always
/// has visible variation somewhere - map terrain, region borders, the top
/// status bar's own text - even in a mostly-empty scene, so "perfectly
/// uniform" cannot occur once anything at all has actually been drawn, while
/// this still catches a nothing-drawn frame regardless of which particular
/// colour the clear pass happens to use (confirmed black on this machine,
/// but nothing here assumes that).
fn is_blank(img: &image::RgbImage) -> bool {
    let mut pixels = img.pixels();
    match pixels.next() {
        Some(&first) => pixels.all(|p| *p == first),
        // A zero-sized capture cannot be a real frame either.
        None => true,
    }
}

/// Saves an already-decoded, already-validated frame to `path`, inferring
/// the on-disk format from its extension the same way `bevy_render`'s own
/// (now-unused here) `save_to_disk` did.
fn save_rgb8(img: &image::RgbImage, path: &str) -> Result<(), String> {
    let format = image::ImageFormat::from_path(path).map_err(|e| e.to_string())?;
    img.save_with_format(path, format).map_err(|e| e.to_string())
}

/// `--debug-select-units` (`ScreenshotConfig::select_units`'s own doc for
/// why this recomputes every frame instead of snapshotting once at
/// startup): sets `SelectedUnits` to every unit the `--play`ed faction
/// currently owns and has alive, read fresh from `SimRes` each call - a
/// no-op without `--screenshot`/`--play`/`--debug-select-units`, so a live
/// (non-`--screenshot`) run is entirely unaffected (`config` is only ever
/// `Some` under `--screenshot`, matching every other `ScreenshotConfig`-gated
/// system in this module).
pub(super) fn sync_debug_selected_units(
    config: Option<Res<ScreenshotConfig>>,
    player: Res<PlayerFaction>,
    sim: Res<SimRes>,
    mut selected: ResMut<SelectedUnits>,
) {
    let Some(config) = config else {
        return;
    };
    if !config.select_units {
        return;
    }
    let Some(player_faction) = player.0 else {
        return;
    };
    selected.0 = sim.0.world().units.iter().filter(|u| u.alive && u.owner == player_faction).map(|u| u.id.0).collect();
}
