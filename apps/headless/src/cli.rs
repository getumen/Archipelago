//! Minimal `std`-only argument parsing for the headless runner (mvp-spec.md §8).

/// Which `Agent` impl drives every living faction (docs/phase4-spec.md
/// "Stage 4A の受け入れ基準": "headless defaults to HeuristicAgent; LLM is
/// opt-in via `--agent llm --backend mock`").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Heuristic,
    Llm,
}

/// Which `LlmBackend` an `--agent llm` run uses. Only meaningful alongside
/// `AgentKind::Llm` - ignored otherwise.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BackendKind {
    /// A small, fixed, deterministic rotation of valid canned `Doctrine`
    /// responses - see `main.rs::mock_doctrine_backend`.
    Mock,
    /// Fails every single consult (`MockBackend::always_err`) - the backend
    /// used to demonstrate `llm_failure_falls_back_to_heuristic` at the CLI
    /// level (`apps/headless/tests/llm_integration.rs`).
    Fail,
    /// Replays canned responses from a local file (docs/phase4-spec.md
    /// "ScriptedBackend": "ファイルから応答を読む").
    Scripted(String),
}

pub struct Args {
    pub seed: u64,
    pub days: u32,
    pub report: u32,
    pub quiet: bool,
    pub json: bool,
    pub agent: AgentKind,
    pub backend: BackendKind,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            seed: 1,
            days: 720,
            report: 30,
            quiet: false,
            json: false,
            agent: AgentKind::Heuristic,
            backend: BackendKind::Mock,
        }
    }
}

fn parse_agent(v: &str) -> Result<AgentKind, String> {
    match v {
        "heuristic" => Ok(AgentKind::Heuristic),
        "llm" => Ok(AgentKind::Llm),
        other => Err(format!("unknown --agent value: {other} (expected \"heuristic\" or \"llm\")")),
    }
}

fn parse_backend(v: &str) -> Result<BackendKind, String> {
    if v == "mock" {
        return Ok(BackendKind::Mock);
    }
    if v == "fail" {
        return Ok(BackendKind::Fail);
    }
    if let Some(path) = v.strip_prefix("scripted:") {
        return Ok(BackendKind::Scripted(path.to_string()));
    }
    Err(format!("unknown --backend value: {v} (expected \"mock\", \"fail\", or \"scripted:<path>\")"))
}

impl Args {
    pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Self, String> {
        let mut args = Args::default();
        let mut iter = argv.into_iter().peekable();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--seed" => args.seed = take_value(&mut iter, "--seed")?.parse().map_err(|_| "--seed expects an integer".to_string())?,
                "--days" => args.days = take_value(&mut iter, "--days")?.parse().map_err(|_| "--days expects an integer".to_string())?,
                "--report" => args.report = take_value(&mut iter, "--report")?.parse().map_err(|_| "--report expects an integer".to_string())?,
                "--quiet" => args.quiet = true,
                "--json" => args.json = true,
                "--agent" => args.agent = parse_agent(&take_value(&mut iter, "--agent")?)?,
                "--backend" => args.backend = parse_backend(&take_value(&mut iter, "--backend")?)?,
                other => {
                    if let Some(v) = other.strip_prefix("--seed=") {
                        args.seed = v.parse().map_err(|_| "--seed expects an integer".to_string())?;
                    } else if let Some(v) = other.strip_prefix("--days=") {
                        args.days = v.parse().map_err(|_| "--days expects an integer".to_string())?;
                    } else if let Some(v) = other.strip_prefix("--report=") {
                        args.report = v.parse().map_err(|_| "--report expects an integer".to_string())?;
                    } else if let Some(v) = other.strip_prefix("--agent=") {
                        args.agent = parse_agent(v)?;
                    } else if let Some(v) = other.strip_prefix("--backend=") {
                        args.backend = parse_backend(v)?;
                    } else {
                        return Err(format!("unknown argument: {other}"));
                    }
                }
            }
        }

        if args.report == 0 {
            return Err("--report must be greater than zero".to_string());
        }
        Ok(args)
    }
}

fn take_value<I: Iterator<Item = String>>(iter: &mut I, flag: &str) -> Result<String, String> {
    iter.next().ok_or_else(|| format!("{flag} expects a value"))
}
