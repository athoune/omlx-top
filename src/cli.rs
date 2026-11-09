//! Command-line surface.
//!
//! Every option from SPECS.md section 8 is declared so the parser never rejects
//! a documented flag. Options that belong to later batches are accepted and
//! turned into an explicit "not implemented in this build" error by
//! [`Cli::unimplemented_flag`].

use std::time::Duration;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "omlx-top",
    version,
    about = "Read-only macOS monitor for oMLX resource usage",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Base URL; by default discovered from the running process
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Bearer token; env OMLX_API_KEY
    #[arg(long, value_name = "KEY")]
    pub api_key: Option<String>,

    /// Sampling period, e.g. 2s, 500ms (default 2s)
    #[arg(long, value_name = "DUR")]
    pub interval: Option<String>,

    /// Measurement window used by --once/--json to obtain a CPU delta
    /// (default 250ms; 0 reports null CPU)
    #[arg(long, value_name = "DUR", default_value = "250ms")]
    pub warmup: String,

    /// Print one snapshot as text, exit 0/1
    #[arg(long)]
    pub once: bool,

    /// Print one snapshot as JSON, exit 0/1 (implies --once)
    #[arg(long)]
    pub json: bool,

    /// Print discovered instances (mode, pid, port) and exit
    #[arg(long)]
    pub list: bool,

    /// Probe /admin/api/stats even without prior success
    #[arg(long)]
    pub admin: bool,

    /// Disable styling
    #[arg(long = "no-color")]
    pub no_color: bool,
}

impl Cli {
    /// Return the first option that is declared but not implemented yet.
    ///
    /// `--url`, `--api-key`, `--once`, `--json` and `--warmup` are implemented
    /// in this batch. `--interval` is parsed and accepted but only used by the
    /// TUI, which is still rejected separately.
    pub fn unimplemented_flag(&self) -> Option<&'static str> {
        if self.admin {
            Some("--admin")
        } else if self.no_color {
            Some("--no-color")
        } else {
            None
        }
    }
}

/// Parse the duration syntax documented in SPECS.md section 8: `250ms`, `2s`
/// and `0`. A bare `0` is the only accepted unit-less value; it means "no
/// window", which reports a null CPU instead of inventing one.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty duration".to_string());
    }
    if text == "0" {
        return Ok(Duration::ZERO);
    }

    let (number, unit) = if let Some(number) = text.strip_suffix("ms") {
        (number, DurationUnit::Millis)
    } else if let Some(number) = text.strip_suffix('s') {
        (number, DurationUnit::Seconds)
    } else {
        return Err(format!(
            "invalid duration {text:?}: expected e.g. 250ms, 2s, or 0"
        ));
    };

    let value: u64 = number
        .parse()
        .map_err(|_| format!("invalid duration {text:?}: expected a whole number"))?;

    match unit {
        DurationUnit::Millis => Ok(Duration::from_millis(value)),
        DurationUnit::Seconds => Ok(Duration::from_secs(value)),
    }
}

enum DurationUnit {
    Millis,
    Seconds,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_accepts_documented_forms() {
        assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(parse_duration("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_duration("0"), Ok(Duration::ZERO));
        assert_eq!(parse_duration(" 500ms "), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("0ms"), Ok(Duration::ZERO));
    }

    #[test]
    fn duration_rejects_other_forms() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("2").is_err());
        assert!(parse_duration("2m").is_err());
        assert!(parse_duration("-1s").is_err());
        assert!(parse_duration("1.5s").is_err());
        assert!(parse_duration("ms").is_err());
    }
}
