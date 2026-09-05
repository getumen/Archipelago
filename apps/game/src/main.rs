//! Entry point: parses `--scenario <path>`/`--seed <n>`/`--days <n>`,
//! Stage 7B's `--play <faction>`/`--record <path>`/`--replay <path>`
//! (docs/phase7-spec.md "Stage 7B — 遊ぶ"), `--delegate-military`
//! (docs/design.md §14), and `--screenshot`, loads the map, and hands off
//! to `archipelago_game::app::run` for everything Bevy-side.
//!
//! **`--delegate-military`'s history**: this used to be `--debug-delegate-units`,
//! a screenshot-only verification convenience wired through
//! `ScreenshotConfig` - and, because it was wired *only* that way, it had
//! no effect at all unless `--screenshot <path>` was also given, despite
//! looking like an ordinary standalone flag. "The AI runs the war while I
//! run the economy" is a legitimate, entirely ordinary way to play - not
//! achievable by hand (`apps/game`'s unit panel delegates one unit per
//! click) and not a debug/verification concern - so it is now parsed like
//! `--play`/`--record`/`--replay`, requires `--play` the same way `--record`/
//! `--replay` do, applies to a live game with no `--screenshot` in sight,
//! and is listed in the ordinary usage line below rather than the
//! debug/screenshot block.
//!
//! **`--replay` is layer-scoped.** A `--replay <path>` file can declare, via
//! its own `layers` field (`archipelago_game::action_codec::read_replay`'s
//! own doc), which `archipelago_sim::action::Layer`s it drives for the
//! played faction; every layer it leaves out goes to a fresh AI instead
//! (`archipelago_game::sim_driver::Replay`/`build_replay_controller`). A
//! plain recording (whatever `--record` has always produced, no `layers`
//! field at all) still replays the whole faction exactly as before - that
//! is this mechanism's degenerate, unscoped case, not a separate one. This
//! is also why `--delegate-military` and `--replay` cannot be combined: the
//! file's own declared scope is now how a replayed faction hands
//! `Layer::Military` to the AI, so the flag would either restate what the
//! file already says or, if the two disagreed, be silently unable to do
//! anything at all (see the rejection below).
use archipelago_game::app::{MapMode, PlayConfig, ScreenshotConfig, ScreenshotTrigger};
use archipelago_sim::ids::FactionId;
use archipelago_sim::world::World;

fn print_usage_and_exit(msg: &str) -> ! {
    eprintln!("error: {msg}");
    eprintln!(
        "usage: archipelago-game [--scenario <path>] [--seed <n>] [--days <n>] \
         [--play <faction index or name>] [--record <path>] [--replay <path>] \
         [--delegate-military] [--cjk-font <path>]\n\
         \n\
         --delegate-military: hand the entire military (orders and recruitment) to the AI \
         for the whole game, so you can just run the economy/diplomacy - requires --play, \
         cannot be combined with --replay.\n\
         \n\
         --replay <path>: a plain recording (from --record) replays the whole faction, exactly \
         as always. A file with its own top-level {{\"layers\": [...], \"days\": [...]}} instead \
         drives only the named layers (\"military\"/\"economy\"/\"grand_strategy\"/\"diplomacy\") \
         and lets the AI decide the rest.\n\
         \n\
         debug/screenshot flags (automated capture only - not needed to play):\n\
         [--screenshot <path>] [--screenshot-after <frames> | --screenshot-at-day <day>] \
         [--debug-map-mode <political|terrain|population|industry|unrest|supply>] \
         [--debug-open-diplomacy] [--debug-open-newspaper] \
         [--debug-open-policy] [--debug-select-region <region index or name>] [--debug-select-units] \
         [--debug-camera-region <region index or name>] [--debug-camera-zoom <scale>]"
    );
    std::process::exit(1);
}

struct Args {
    scenario: Option<String>,
    seed: u64,
    days: u32,
    play: Option<String>,
    record: Option<String>,
    replay: Option<String>,
    /// `--delegate-military` (this module's own doc): requires `--play`,
    /// same as `--record`/`--replay` do.
    delegate_military: bool,
    screenshot: Option<String>,
    screenshot_after: u32,
    /// `--screenshot-at-day <day>`: overrides `screenshot_after` entirely
    /// when given - see `ScreenshotTrigger::AtDay`'s own doc for why a
    /// frame count cannot reliably aim a capture at a specific game day.
    /// `None` (the overwhelmingly common case) keeps the frame-based
    /// trigger exactly as it always was.
    screenshot_at_day: Option<u32>,
    // Verification-only conveniences (`ScreenshotConfig`'s own doc): start
    // in a particular map mode / with the diplomacy panel / newspaper panel
    // already open, for `--screenshot` runs where nothing is at the
    // keyboard to press `M`/`D`/`N` first. No effect without `--screenshot`.
    debug_map_mode: Option<MapMode>,
    debug_open_diplomacy: bool,
    debug_open_newspaper: bool,
    debug_open_policy: bool,
    debug_select_region: Option<String>,
    debug_select_units: bool,
    debug_camera_region: Option<String>,
    debug_camera_zoom: f32,
    /// `--cjk-font <path>`: overrides `apps/game/src/app/fonts.rs`'s
    /// platform-specific search entirely - see that module's own doc.
    cjk_font: Option<String>,
}

/// Default `--debug-camera-zoom` (orthographic `scale`) when `--debug-camera-region`
/// is given without one - close enough to read a single region's own
/// overlay ring/links clearly, wide enough to still show its immediate
/// neighbors for context.
const DEFAULT_DEBUG_CAMERA_ZOOM: f32 = 0.35;

/// Default frame at which `--screenshot` fires when `--screenshot-after` is
/// not given: small enough to exit quickly, but large enough that
/// `sim_control::advance_simulation` (one tick/frame at the default `X1`
/// speed - see `crate::app::SpeedRes`'s own doc) has already advanced the
/// simulation a bit, so the captured map isn't the empty day-0 board.
const DEFAULT_SCREENSHOT_AFTER_FRAMES: u32 = 120;

fn parse_args() -> Args {
    let mut scenario = None;
    let mut seed = 1u64;
    let mut days = 720u32;
    let mut play = None;
    let mut record = None;
    let mut replay = None;
    let mut delegate_military = false;
    let mut screenshot = None;
    let mut screenshot_after = DEFAULT_SCREENSHOT_AFTER_FRAMES;
    let mut screenshot_at_day = None;
    let mut debug_map_mode = None;
    let mut debug_open_diplomacy = false;
    let mut debug_open_newspaper = false;
    let mut debug_open_policy = false;
    let mut debug_select_region = None;
    let mut debug_select_units = false;
    let mut debug_camera_region = None;
    let mut debug_camera_zoom = DEFAULT_DEBUG_CAMERA_ZOOM;
    let mut cjk_font = None;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--scenario" => {
                scenario = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--scenario expects a value")));
            }
            "--seed" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--seed expects a value"));
                seed = v.parse().unwrap_or_else(|_| print_usage_and_exit("--seed expects an integer"));
            }
            "--days" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--days expects a value"));
                days = v.parse().unwrap_or_else(|_| print_usage_and_exit("--days expects an integer"));
            }
            "--play" => {
                play = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--play expects a faction index or name")));
            }
            "--record" => {
                record = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--record expects a value")));
            }
            "--replay" => {
                replay = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--replay expects a value")));
            }
            "--delegate-military" => delegate_military = true,
            "--screenshot" => {
                screenshot = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--screenshot expects a value")));
            }
            "--screenshot-after" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--screenshot-after expects a value"));
                screenshot_after = v.parse().unwrap_or_else(|_| print_usage_and_exit("--screenshot-after expects an integer"));
            }
            "--screenshot-at-day" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--screenshot-at-day expects a value"));
                screenshot_at_day = Some(v.parse().unwrap_or_else(|_| print_usage_and_exit("--screenshot-at-day expects an integer")));
            }
            "--debug-map-mode" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--debug-map-mode expects a value"));
                debug_map_mode = Some(parse_map_mode(&v));
            }
            "--debug-open-diplomacy" => debug_open_diplomacy = true,
            "--debug-open-newspaper" => debug_open_newspaper = true,
            "--debug-open-policy" => debug_open_policy = true,
            "--debug-select-region" => {
                debug_select_region = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--debug-select-region expects a region index or name")));
            }
            "--debug-select-units" => debug_select_units = true,
            "--debug-camera-region" => {
                debug_camera_region = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--debug-camera-region expects a region index or name")));
            }
            "--debug-camera-zoom" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--debug-camera-zoom expects a value"));
                debug_camera_zoom = v.parse().unwrap_or_else(|_| print_usage_and_exit("--debug-camera-zoom expects a number"));
            }
            "--cjk-font" => {
                cjk_font = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--cjk-font expects a path")));
            }
            other => {
                if let Some(v) = other.strip_prefix("--scenario=") {
                    scenario = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--seed=") {
                    seed = v.parse().unwrap_or_else(|_| print_usage_and_exit("--seed expects an integer"));
                } else if let Some(v) = other.strip_prefix("--days=") {
                    days = v.parse().unwrap_or_else(|_| print_usage_and_exit("--days expects an integer"));
                } else if let Some(v) = other.strip_prefix("--play=") {
                    play = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--record=") {
                    record = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--replay=") {
                    replay = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--screenshot=") {
                    screenshot = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--screenshot-after=") {
                    screenshot_after = v.parse().unwrap_or_else(|_| print_usage_and_exit("--screenshot-after expects an integer"));
                } else if let Some(v) = other.strip_prefix("--screenshot-at-day=") {
                    screenshot_at_day = Some(v.parse().unwrap_or_else(|_| print_usage_and_exit("--screenshot-at-day expects an integer")));
                } else if let Some(v) = other.strip_prefix("--cjk-font=") {
                    cjk_font = Some(v.to_string());
                } else if let Some(v) = other.strip_prefix("--debug-map-mode=") {
                    debug_map_mode = Some(parse_map_mode(v));
                } else {
                    print_usage_and_exit(&format!("unknown argument: {other}"));
                }
            }
        }
    }
    Args {
        scenario,
        seed,
        days,
        play,
        record,
        replay,
        delegate_military,
        screenshot,
        screenshot_after,
        screenshot_at_day,
        debug_map_mode,
        debug_open_diplomacy,
        debug_open_newspaper,
        debug_open_policy,
        debug_select_region,
        debug_select_units,
        debug_camera_region,
        debug_camera_zoom,
        cjk_font,
    }
}

/// Resolves `--play <value>` against the loaded `world`'s factions: either a
/// plain integer index (`--play 0`, docs/phase7-spec.md's own example) or an
/// exact faction name (`--play 東方連合`).
fn resolve_faction(world: &World, value: &str) -> FactionId {
    if let Ok(i) = value.parse::<u32>() {
        if (i as usize) < world.factions.len() {
            return FactionId(i);
        }
        eprintln!("error: --play {i}: only {} factions exist (0..{})", world.factions.len(), world.factions.len());
        std::process::exit(1);
    }
    if let Some(f) = world.factions.iter().find(|f| f.name == value) {
        return f.id;
    }
    eprintln!(
        "error: --play {value}: no faction with that index or name. Available: {}",
        world.factions.iter().map(|f| format!("{} (\"{}\")", f.id.0, f.name)).collect::<Vec<_>>().join(", ")
    );
    std::process::exit(1);
}

/// Resolves `--debug-camera-region <value>` the same way `resolve_faction`
/// resolves `--play`: a plain integer index, or an exact `Region::name`
/// match (the Japanese display name shown on the map, since that's what's
/// actually visible to whoever is choosing where to point the camera).
fn resolve_region(world: &World, value: &str) -> archipelago_sim::ids::RegionId {
    if let Ok(i) = value.parse::<u32>() {
        if (i as usize) < world.regions.len() {
            return archipelago_sim::ids::RegionId(i);
        }
        eprintln!("error: --debug-camera-region {i}: only {} regions exist (0..{})", world.regions.len(), world.regions.len());
        std::process::exit(1);
    }
    if let Some(r) = world.regions.iter().find(|r| r.name == value) {
        return r.id;
    }
    eprintln!("error: --debug-camera-region {value}: no region with that index or name");
    std::process::exit(1);
}

/// Resolves `--debug-map-mode <value>` against `MapMode::from_key` - a hard
/// error naming every valid key on a miss, following docs/conventions.md's
/// fail-fast rule the same way every other malformed flag in this file
/// does, rather than silently falling back to `MapMode::default()`.
fn parse_map_mode(value: &str) -> MapMode {
    MapMode::from_key(value)
        .unwrap_or_else(|| print_usage_and_exit(&format!("--debug-map-mode {value}: unknown mode, expected one of: {}", MapMode::ALL_KEYS.join(", "))))
}

fn main() {
    let args = parse_args();

    // Per docs/phase6-spec.md's "壊れたデータで暗黙に既定値へ落ちないこと",
    // followed here exactly as `apps/headless/src/main.rs::load_world`
    // does: a `--scenario` that fails to load is a hard error, never a
    // silent fall-back to the embedded default.
    let (world, scenario_name) = match &args.scenario {
        None => (archipelago_sim::scenario::build_world(), "mvp".to_string()),
        Some(path) => {
            let world = archipelago_sim::scenario::load_file(path).unwrap_or_else(|e| {
                eprintln!("error: could not load --scenario {path}: {e}");
                std::process::exit(1);
            });
            (world, path.clone())
        }
    };

    if args.screenshot_at_day.is_some() && args.screenshot.is_none() {
        print_usage_and_exit("--screenshot-at-day requires --screenshot <path>");
    }

    if args.replay.is_some() && args.play.is_none() {
        print_usage_and_exit("--replay requires --play <faction> to say which faction it replays");
    }
    if args.record.is_some() && args.play.is_none() {
        print_usage_and_exit("--record requires --play <faction> to say which faction to record");
    }
    if args.delegate_military && args.play.is_none() {
        print_usage_and_exit("--delegate-military requires --play <faction> to say which faction's military to delegate");
    }
    // A replay declares which `Layer`s it drives in the file itself
    // (`archipelago_game::action_codec::read_replay`'s own doc) - Military
    // included, if the replay wants it. `--delegate-military` only ever
    // reaches a live `HumanAgent` controller (`PlayConfig::delegate_military`'s
    // own doc): combined with `--replay` it would either duplicate what the
    // file already declares or, if the two disagreed, silently do nothing at
    // all (`SimDriver::delegate_military` is a no-op against a
    // `Controller::Replay`) - both outcomes are exactly the silent-fallback
    // shape docs/conventions.md forbids, so this is rejected outright rather
    // than left to quietly do the wrong thing.
    if args.delegate_military && args.replay.is_some() {
        print_usage_and_exit(
            "--delegate-military cannot be combined with --replay - a replay's own file declares which layers \
             (Military included) it drives; leave Military out of that declaration instead",
        );
    }

    let play = args.play.as_deref().map(|v| resolve_faction(&world, v));
    let replay = args.replay.as_deref().map(|path| {
        let (layers, days) = archipelago_game::action_codec::read_replay(std::path::Path::new(path)).unwrap_or_else(|e| {
            eprintln!("error: could not load --replay {path}: {e}");
            std::process::exit(1);
        });
        archipelago_game::sim_driver::Replay { layers, days }
    });

    let play_config =
        play.map(|player| PlayConfig { player, record: args.record.clone(), replay, delegate_military: args.delegate_military });

    let debug_camera_region = args.debug_camera_region.as_deref().map(|v| resolve_region(&world, v));
    let debug_select_region = args.debug_select_region.as_deref().map(|v| resolve_region(&world, v));
    // `--screenshot-at-day` wins outright when given - the mutual-exclusion
    // check above already rejected any run that tries to combine it with a
    // meaningful `--screenshot-after` misunderstanding; a bare
    // `--screenshot-after` (or neither) falls back to the frame-counting
    // trigger exactly as it always has.
    let screenshot_trigger = match args.screenshot_at_day {
        Some(day) => ScreenshotTrigger::AtDay(day),
        None => ScreenshotTrigger::AfterFrames(args.screenshot_after),
    };
    let screenshot = args.screenshot.map(|path| ScreenshotConfig {
        path,
        trigger: screenshot_trigger,
        open_diplomacy: args.debug_open_diplomacy,
        open_newspaper: args.debug_open_newspaper,
        open_policy: args.debug_open_policy,
        map_mode: args.debug_map_mode,
        select_region: debug_select_region,
        select_units: args.debug_select_units,
        camera_focus_region: debug_camera_region,
        camera_zoom: args.debug_camera_zoom,
    });

    archipelago_game::app::run(world, args.seed, scenario_name, args.days, screenshot, play_config, args.cjk_font);
}
