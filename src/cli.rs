//! Command-line surface.
//!
//! Every option from SPECS.md section 8 is declared so the parser never rejects
//! a documented flag. Options that belong to later batches are accepted and
//! turned into an explicit "not implemented in this build" error by
//! [`Cli::unimplemented_flag`].

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
    /// `--url` is implemented for `--list` in this batch, so it is not listed
    /// here; the TUI itself is rejected separately.
    pub fn unimplemented_flag(&self) -> Option<&'static str> {
        if self.api_key.is_some() {
            Some("--api-key")
        } else if self.interval.is_some() {
            Some("--interval")
        } else if self.once {
            Some("--once")
        } else if self.json {
            Some("--json")
        } else if self.admin {
            Some("--admin")
        } else if self.no_color {
            Some("--no-color")
        } else {
            None
        }
    }
}
