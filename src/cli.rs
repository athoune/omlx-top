//! Command-line surface.
//!
//! Every option from SPECS.md section 8 is implemented: the TUI is the bare
//! invocation, `--once`/`--json`/`--list` are the scriptable modes, and
//! `--no-color` disables styling.

use std::time::Duration;

use clap::Parser;

/// Default sampling period (SPECS.md sections 6.1 and 8).
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);

/// Accepted `--interval` range. Values outside are clamped with a warning.
pub const MIN_INTERVAL: Duration = Duration::from_millis(500);
pub const MAX_INTERVAL: Duration = Duration::from_secs(60);

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

    /// Disable styling
    #[arg(long = "no-color")]
    pub no_color: bool,
}

impl Cli {
    /// Bearer token from the flag or the documented environment variable.
    pub fn api_key(&self) -> Option<String> {
        self.api_key
            .clone()
            .or_else(|| std::env::var("OMLX_API_KEY").ok())
    }

    /// Whether colour must be disabled by the flag, `NO_COLOR` or a mono
    /// terminal (SPECS.md section 7.3).
    pub fn color_disabled(&self) -> bool {
        self.no_color
            || std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").is_ok_and(|term| term == "xterm-mono")
    }
}

/// Clamp a sampling interval to the documented 500 ms – 60 s range.
pub fn clamp_interval(duration: Duration) -> Duration {
    duration.clamp(MIN_INTERVAL, MAX_INTERVAL)
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

/// Parse `--interval`, applying the documented clamp.
pub fn parse_interval(text: Option<&str>) -> Result<Duration, String> {
    let raw = match text {
        Some(text) => parse_duration(text)?,
        None => DEFAULT_INTERVAL,
    };
    Ok(clamp_interval(raw))
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

    #[test]
    fn interval_is_clamped_to_the_documented_range() {
        assert_eq!(
            parse_interval(None).expect("default"),
            Duration::from_secs(2)
        );
        assert_eq!(
            parse_interval(Some("100ms")).expect("clamped"),
            Duration::from_millis(500)
        );
        assert_eq!(
            parse_interval(Some("120s")).expect("clamped"),
            Duration::from_secs(60)
        );
        assert_eq!(
            parse_interval(Some("1500ms")).expect("inside"),
            Duration::from_millis(1500)
        );
        assert!(parse_interval(Some("fast")).is_err());
    }
}
