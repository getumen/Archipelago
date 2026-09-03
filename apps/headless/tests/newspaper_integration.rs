//! Stage 4C CLI-level acceptance tests (docs/phase4-spec.md "Stage 4C の受
//! け入れ基準"). Runs the compiled `archipelago-headless` binary itself, the
//! same way `llm_integration.rs` does. `--newspaper` never opens a network
//! connection - `newspaper_mock_backend` (`main.rs`) is pure in-memory
//! canned data, exactly like `--backend mock`/`fail`.

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

/// `newspaper_does_not_affect_simulation` (docs/phase4-spec.md "Stage 4C の
/// 受け入れ基準": "新聞生成の有無で --json が一致する"): `--newspaper` is
/// purely additional console output, gated behind `!args.json`
/// (`main.rs`) - a run with it enabled must produce byte-identical `--json`
/// output to one without it, for every agent kind this binary supports.
#[test]
fn newspaper_does_not_affect_simulation() {
    for agent_args in [vec!["--agent", "heuristic"], vec!["--agent", "llm", "--backend", "mock"]] {
        let mut without_newspaper: Vec<&str> =
            vec!["--seed", "1", "--days", "720", "--json", "--quiet"];
        without_newspaper.extend(agent_args.iter());
        let mut with_newspaper = without_newspaper.clone();
        with_newspaper.push("--newspaper");

        let baseline = run(&without_newspaper);
        let with_flag = run(&with_newspaper);

        assert_eq!(
            baseline, with_flag,
            "--newspaper must not change --json output at all (agent args: {agent_args:?})"
        );
    }
}

/// Not one of Stage 4C's two named tests, but the plainest possible check
/// that `--newspaper` actually does something observable
/// (docs/phase4-spec.md "Stage 4C の受け入れ基準": "--newspaper で headless
/// が記事つきのログを出す").
#[test]
fn newspaper_flag_produces_articles() {
    let output = run(&["--agent", "heuristic", "--seed", "1", "--days", "60", "--newspaper", "--quiet"]);
    assert!(output.contains("新聞"), "expected at least one newspaper issue in the output:\n{output}");
}

/// A run with `--newspaper` but no agent/backend flags (defaults to
/// `--agent heuristic`, which has no LLM at all) must still complete and
/// print articles - the newspaper backend is independent of `--agent`/
/// `--backend`.
#[test]
fn newspaper_works_without_llm_agent() {
    let output = run(&["--seed", "2", "--days", "35", "--newspaper", "--quiet"]);
    assert!(output.contains("新聞"));
}
