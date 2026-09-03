//! `archipelago-api` binary: runs the Stage 5A server in the foreground.
//!
//! ```text
//! archipelago-api [--bind HOST:PORT] [--idle-timeout SECS] [--scenario PATH]
//! ```
//!
//! Defaults to `127.0.0.1:8080`, a 30-minute idle-session timeout, and a
//! 30-second reaper sweep interval. `--scenario` (Stage 6A,
//! docs/phase6-spec.md "Stage 6A") loads every session's starting map from
//! the named JSON file instead of the embedded default
//! (`scenarios/mvp.json`) - a file that fails to parse or validate is a
//! startup error (nonzero exit), never a silent fall-back to the default.

use std::time::Duration;

fn take_value<I: Iterator<Item = String>>(iter: &mut I, flag: &str) -> String {
    iter.next().unwrap_or_else(|| {
        eprintln!("error: {flag} expects a value");
        std::process::exit(1);
    })
}

fn main() {
    let mut bind_addr = "127.0.0.1:8080".to_string();
    let mut idle_timeout_secs: u64 = 1800;
    let mut scenario_path: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => bind_addr = take_value(&mut args, "--bind"),
            "--idle-timeout" => {
                idle_timeout_secs = take_value(&mut args, "--idle-timeout").parse().unwrap_or_else(|_| {
                    eprintln!("error: --idle-timeout expects an integer number of seconds");
                    std::process::exit(1);
                });
            }
            "--scenario" => scenario_path = Some(take_value(&mut args, "--scenario")),
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    let idle_timeout = Duration::from_secs(idle_timeout_secs);
    let sweep_interval = Duration::from_secs(30);
    let handle = match &scenario_path {
        None => archipelago_api::serve_background(&bind_addr, idle_timeout, sweep_interval),
        Some(path) => {
            let scenario = archipelago_sim::scenario::load_file(path).unwrap_or_else(|e| {
                eprintln!("error: could not load --scenario {path}: {e}");
                std::process::exit(1);
            });
            archipelago_api::serve_background_with_scenario(&bind_addr, idle_timeout, sweep_interval, scenario)
        }
    }
    .unwrap_or_else(|e| {
        eprintln!("error: could not bind {bind_addr}: {e}");
        std::process::exit(1);
    });

    println!("archipelago-api listening on http://{}", handle.addr);
    println!("(idle sessions are reclaimed after {idle_timeout_secs}s)");
    if let Some(path) = &scenario_path {
        println!("(scenario: {path})");
    }

    // The server itself runs entirely on background threads
    // (`serve_background`'s doc); this thread just keeps the process alive.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
