//! Stage 6A CLI-level acceptance test (docs/phase6-spec.md "Stage 6A の受
//! け入れ基準"): `--scenario scenarios/mvp.json` must produce `--json`
//! output byte-identical to the embedded built-in scenario, for the same
//! seed - the regression guard for the whole stage ("externalising the
//! data must not change behaviour at all"). Runs the compiled
//! `archipelago-headless` binary itself, the same way `newspaper_
//! integration.rs`/`llm_integration.rs` do.

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

#[test]
fn scenario_flag_matches_builtin_scenario() {
    let baseline = run(&["--seed", "1", "--days", "720", "--json", "--quiet"]);
    let via_scenario_flag = run(&["--seed", "1", "--days", "720", "--json", "--quiet", "--scenario", "../../scenarios/mvp.json"]);
    assert_eq!(
        baseline, via_scenario_flag,
        "--scenario scenarios/mvp.json must produce byte-identical --json output to the embedded built-in scenario"
    );
}

/// A scenario file that fails to parse or validate must be a hard error
/// (nonzero exit, a reason on stderr), never a silent fall-back to the
/// built-in scenario (docs/phase6-spec.md "壊れたデータで暗黙に既定値へ落ち
/// ないこと").
#[test]
fn missing_scenario_file_is_a_hard_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_archipelago-headless"))
        .args(["--seed", "1", "--days", "5", "--json", "--quiet", "--scenario", "does/not/exist.json"])
        .output()
        .expect("failed to launch archipelago-headless");
    assert!(!output.status.success(), "a missing --scenario file must not exit successfully");
    assert!(output.stdout.is_empty(), "a rejected --scenario must never fall back to printing built-in --json output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not load"), "expected a clear load-failure reason on stderr, got: {stderr}");
}
