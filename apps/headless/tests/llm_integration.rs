//! Stage 4A CLI-level acceptance tests (docs/phase4-spec.md "Stage 4A の
//! 受け入れ基準"). Runs the compiled `archipelago-headless` binary itself
//! (via `CARGO_BIN_EXE_archipelago-headless`, which Cargo sets for
//! integration tests in this package) rather than driving the simulation
//! in-process, so these exercise exactly the `--agent llm --backend ...`
//! surface a real user would type. Neither test opens a network connection
//! - `--backend fail`/`--backend mock` both resolve to `MockBackend`
//! (`main.rs::build_agents`), which is pure in-memory canned data.

use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_archipelago-headless"))
        .args(args)
        .output()
        .expect("failed to launch archipelago-headless");
    assert!(
        output.status.success(),
        "archipelago-headless {:?} exited with {}\nstderr:\n{}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("archipelago-headless stdout was not valid UTF-8")
}

/// `llm_failure_falls_back_to_heuristic` (docs/phase4-spec.md: "バックエン
/// ドが常に Err を返してもゲームが 720 日完走し、HeuristicAgent 単独と同じ
/// 結果になる"): `--backend fail` wires every faction's `LlmAgent` to a
/// `MockBackend` that errors on every single consult
/// (`main.rs::build_agents`'s `BackendKind::Fail` arm), so no faction ever
/// installs a `Doctrine` and `HeuristicAgent::decide_for_llm` always takes
/// its `doctrine == None` branch - byte-for-byte the same branches plain
/// `--agent heuristic` takes.
#[test]
fn llm_failure_falls_back_to_heuristic() {
    let heuristic =
        run(&["--agent", "heuristic", "--seed", "1", "--days", "720", "--json", "--quiet"]);
    let llm_always_fails =
        run(&["--agent", "llm", "--backend", "fail", "--seed", "1", "--days", "720", "--json", "--quiet"]);

    assert_eq!(
        heuristic, llm_always_fails,
        "a backend that fails on every consult must produce a run byte-identical to no LLM at all"
    );
}

/// `mock_backend_run_is_deterministic` (docs/phase4-spec.md: "同じ
/// MockBackend と seed で --json がバイト一致する"): `--backend mock`'s
/// rotation of canned `Doctrine` responses is a pure function of call count
/// (`llm::MockBackend`'s doc), so two independent runs with the same seed
/// must produce identical `--json` output, even though this run actually
/// exercises `Doctrine`-driven behaviour changes (unlike the always-failing
/// backend above).
#[test]
fn mock_backend_run_is_deterministic() {
    let run_a = run(&["--agent", "llm", "--backend", "mock", "--seed", "1", "--days", "720", "--json", "--quiet"]);
    let run_b = run(&["--agent", "llm", "--backend", "mock", "--seed", "1", "--days", "720", "--json", "--quiet"]);

    assert_eq!(
        run_a, run_b,
        "the same MockBackend and seed must produce byte-identical --json output across runs"
    );
}

/// Not one of Stage 4A's five named tests, but the plainest possible check
/// that `--agent llm --backend mock` actually completes a full run
/// (docs/phase4-spec.md "Stage 4A の受け入れ基準": "`--agent llm --backend
/// mock` で headless が完走する").
#[test]
fn llm_mock_backend_completes_a_full_run() {
    let output =
        run(&["--agent", "llm", "--backend", "mock", "--seed", "2", "--days", "720", "--json", "--quiet"]);
    assert!(output.trim_start().starts_with('{'), "expected --json output to be a JSON object");
}
