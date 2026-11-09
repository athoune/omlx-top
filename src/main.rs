//! omlx-top entry point.
//!
//! First batch: `--list` (plus `--url` for selection), `--help` and
//! `--version`. Every other documented option is parsed but rejected with a
//! clear "not implemented in this build" message and exit code 2.

#[cfg(not(target_os = "macos"))]
compile_error!("omlx-top currently supports macOS only");

pub mod cli;
pub mod config;
pub mod format;
pub mod model;
pub mod sys;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    if let Some(flag) = cli.unimplemented_flag() {
        eprintln!("omlx-top: {flag} is not implemented in this build");
        return ExitCode::from(2);
    }

    if cli.list {
        list(&cli)
    } else {
        eprintln!("omlx-top: the TUI is not implemented in this build; use --list");
        ExitCode::from(2)
    }
}

fn list(cli: &cli::Cli) -> ExitCode {
    // Machine facts are read once at startup, like the sampler will need in
    // later batches. `--list` itself does not depend on them.
    if let Err(error) = sys::info::init() {
        eprintln!("omlx-top: warning: machine info unavailable: {error}");
    }

    let settings_port = config::load_settings_port();
    // The HTTP client belongs to a later batch; until then no probe can
    // succeed, so the resolver falls through to the documented defaults.
    let mut probe = |_port: u16| false;

    let candidates = match sys::discover::discover() {
        Ok(candidates) => candidates,
        Err(error) => {
            eprintln!("omlx-top: discovery failed: {error}");
            return ExitCode::from(1);
        }
    };

    let instances = match sys::discover::build_instances(
        &candidates,
        cli.url.as_deref(),
        settings_port,
        &mut probe,
    ) {
        Ok(instances) => instances,
        Err(error) => {
            eprintln!("omlx-top: {error}");
            return ExitCode::from(2);
        }
    };

    if instances.is_empty() {
        println!("no oMLX instance found");
        return ExitCode::from(1);
    }

    print_table(&instances);

    // SPECS.md section 8: with an explicit --url, the exit code follows
    // whether any discovered instance listens on that port.
    if let Some(url) = cli.url.as_deref() {
        let port = match config::parse_url_port(url) {
            Ok(port) => port,
            Err(error) => {
                eprintln!("omlx-top: {error}");
                return ExitCode::from(2);
            }
        };
        if !sys::discover::has_instance_on_port(&instances, port) {
            eprintln!("warning: no instance on port {port}");
            return ExitCode::from(1);
        }
    }

    ExitCode::SUCCESS
}

fn print_table(instances: &[model::Instance]) {
    println!(
        "{:<8} {:<6} {:<6} {:<6} {:<22} SELECTED",
        "MODE", "PID", "GUI", "PORT", "URL"
    );

    for instance in instances {
        let gui = instance
            .gui_pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string());
        let port = instance
            .port
            .map(|port| port.to_string())
            .unwrap_or_else(|| "-".to_string());
        let url = instance.url.as_deref().unwrap_or("-");

        println!(
            "{:<8} {:<6} {:<6} {:<6} {:<22} {}",
            instance.mode.as_str(),
            instance.pid,
            gui,
            port,
            url,
            if instance.selected { "*" } else { "" }
        );
    }
}
