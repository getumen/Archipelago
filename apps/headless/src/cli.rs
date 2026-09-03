//! Minimal `std`-only argument parsing for the headless runner (mvp-spec.md §8).

pub struct Args {
    pub seed: u64,
    pub days: u32,
    pub report: u32,
    pub quiet: bool,
    pub json: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            seed: 1,
            days: 720,
            report: 30,
            quiet: false,
            json: false,
        }
    }
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
                other => {
                    if let Some(v) = other.strip_prefix("--seed=") {
                        args.seed = v.parse().map_err(|_| "--seed expects an integer".to_string())?;
                    } else if let Some(v) = other.strip_prefix("--days=") {
                        args.days = v.parse().map_err(|_| "--days expects an integer".to_string())?;
                    } else if let Some(v) = other.strip_prefix("--report=") {
                        args.report = v.parse().map_err(|_| "--report expects an integer".to_string())?;
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
