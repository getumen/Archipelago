//! `archipelago-api` binary: runs the Stage 5A server in the foreground.
//!
//! ```text
//! archipelago-api [--bind HOST:PORT] [--idle-timeout SECS]
//! ```
//!
//! Defaults to `127.0.0.1:8080`, a 30-minute idle-session timeout, and a
//! 30-second reaper sweep interval.

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
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    let handle = archipelago_api::serve_background(
        &bind_addr,
        Duration::from_secs(idle_timeout_secs),
        Duration::from_secs(30),
    )
    .unwrap_or_else(|e| {
        eprintln!("error: could not bind {bind_addr}: {e}");
        std::process::exit(1);
    });

    println!("archipelago-api listening on http://{}", handle.addr);
    println!("(idle sessions are reclaimed after {idle_timeout_secs}s)");

    // The server itself runs entirely on background threads
    // (`serve_background`'s doc); this thread just keeps the process alive.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
