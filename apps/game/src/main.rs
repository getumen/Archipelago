//! Stage 7A entry point (docs/phase7-spec.md "Stage 7A — 観る"): parses
//! `--scenario <path>`/`--seed <n>`, loads the map, and hands off to
//! `archipelago_game::app::run` for everything Bevy-side.

use archipelago_game::app::ScreenshotConfig;

fn print_usage_and_exit(msg: &str) -> ! {
    eprintln!("error: {msg}");
    eprintln!(
        "usage: archipelago-game [--scenario <path>] [--seed <n>] [--days <n>] \
         [--screenshot <path>] [--screenshot-after <frames>]"
    );
    std::process::exit(1);
}

struct Args {
    scenario: Option<String>,
    seed: u64,
    days: u32,
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
    Args { scenario, seed, days, screenshot, screenshot_after }
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

    let screenshot = args.screenshot.map(|path| ScreenshotConfig { path, after_frames: args.screenshot_after });

    archipelago_game::app::run(world, args.seed, scenario_name, args.days, screenshot);
}
