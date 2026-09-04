//! Entry point: parses `--scenario <path>`/`--seed <n>`/`--days <n>`,
//! Stage 7B's `--play <faction>`/`--record <path>`/`--replay <path>`
//! (docs/phase7-spec.md "Stage 7B — 遊ぶ"), and `--screenshot`, loads the
//! map, and hands off to `archipelago_game::app::run` for everything
//! Bevy-side.

use archipelago_game::app::{PlayConfig, ScreenshotConfig};
use archipelago_sim::ids::FactionId;
use archipelago_sim::world::World;

fn print_usage_and_exit(msg: &str) -> ! {
    eprintln!("error: {msg}");
    eprintln!(
        "usage: archipelago-game [--scenario <path>] [--seed <n>] [--days <n>] \
         [--play <faction index or name>] [--record <path>] [--replay <path>] \
         [--screenshot <path>] [--screenshot-after <frames>]"
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
    screenshot: Option<String>,
    screenshot_after: u32,
}

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
    let mut screenshot = None;
    let mut screenshot_after = DEFAULT_SCREENSHOT_AFTER_FRAMES;
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
            "--screenshot" => {
                screenshot = Some(iter.next().unwrap_or_else(|| print_usage_and_exit("--screenshot expects a value")));
            }
            "--screenshot-after" => {
                let v = iter.next().unwrap_or_else(|| print_usage_and_exit("--screenshot-after expects a value"));
                screenshot_after = v.parse().unwrap_or_else(|_| print_usage_and_exit("--screenshot-after expects an integer"));
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
                } else {
                    print_usage_and_exit(&format!("unknown argument: {other}"));
                }
            }
        }
    }
    Args { scenario, seed, days, play, record, replay, screenshot, screenshot_after }
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

    if args.replay.is_some() && args.play.is_none() {
        print_usage_and_exit("--replay requires --play <faction> to say which faction it replays");
    }
    if args.record.is_some() && args.play.is_none() {
        print_usage_and_exit("--record requires --play <faction> to say which faction to record");
    }

    let play = args.play.as_deref().map(|v| resolve_faction(&world, v));
    let replay_days = args.replay.as_deref().map(|path| {
        archipelago_game::action_codec::read_record(std::path::Path::new(path)).unwrap_or_else(|e| {
            eprintln!("error: could not load --replay {path}: {e}");
            std::process::exit(1);
        })
    });

    let play_config = play.map(|player| PlayConfig { player, record: args.record.clone(), replay: replay_days });

    let screenshot = args.screenshot.map(|path| ScreenshotConfig { path, after_frames: args.screenshot_after });

    archipelago_game::app::run(world, args.seed, scenario_name, args.days, screenshot, play_config);
}
