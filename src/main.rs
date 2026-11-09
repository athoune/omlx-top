//! omlx-top entry point.
//!
//! Batch 2 implements the HTTP client, one-shot sampling and the `--once` /
//! `--json` outputs. `--list` keeps its batch 1 behaviour. The TUI and
//! `--admin` are still rejected with exit code 2.

#[cfg(not(target_os = "macos"))]
compile_error!("omlx-top currently supports macOS only");

pub mod cli;
pub mod config;
pub mod format;
pub mod http;
pub mod model;
pub mod sampler;
pub mod sys;

use std::process::ExitCode;

use clap::Parser;
use model::Snapshot;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    if let Some(flag) = cli.unimplemented_flag() {
        eprintln!("omlx-top: {flag} is not implemented in this build");
        return ExitCode::from(2);
    }

    if cli.list {
        list(&cli)
    } else if cli.json || cli.once {
        one_shot(&cli, cli.json)
    } else {
        eprintln!(
            "omlx-top: the TUI is not implemented in this build; use --list, --once or --json"
        );
        ExitCode::from(2)
    }
}

fn list(cli: &cli::Cli) -> ExitCode {
    // Machine facts are read once at startup, like the sampler needs in later
    // batches. `--list` itself does not depend on them.
    if let Err(error) = sys::info::init() {
        eprintln!("omlx-top: warning: machine info unavailable: {error}");
    }

    let settings_port = config::load_settings_port();
    let mut probe = |port: u16| http::Client::new(config::canonical_url(port), None).probe();

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

fn one_shot(cli: &cli::Cli, json: bool) -> ExitCode {
    let warmup = match cli::parse_duration(&cli.warmup) {
        Ok(warmup) => warmup,
        Err(error) => {
            eprintln!("omlx-top: {error}");
            return ExitCode::from(2);
        }
    };

    if let Err(error) = sys::info::init() {
        eprintln!("omlx-top: warning: machine info unavailable: {error}");
    }

    let settings_port = config::load_settings_port();
    let mut probe = |port: u16| http::Client::new(config::canonical_url(port), None).probe();

    // Discovery failure is not fatal for one-shot mode: an explicit --url (or
    // the documented default) can still be probed without a process table.
    let candidates = match sys::discover::discover() {
        Ok(candidates) => candidates,
        Err(error) => {
            eprintln!("omlx-top: warning: discovery failed: {error}");
            Vec::new()
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

    let target_url = cli
        .url
        .clone()
        .or_else(|| {
            instances
                .iter()
                .find(|instance| instance.selected)
                .and_then(|instance| instance.url.clone())
        })
        .or_else(|| settings_port.map(config::canonical_url))
        .unwrap_or_else(|| config::canonical_url(8000));

    let api_key = cli
        .api_key
        .clone()
        .or_else(|| std::env::var("OMLX_API_KEY").ok());

    let mut sampler = sampler::Sampler::new(instances, &target_url, warmup, api_key);
    let snapshot = sampler.sample_once();

    if json {
        match serde_json::to_string(&snapshot) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("omlx-top: could not serialise the snapshot: {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        print_once(&snapshot);
    }

    if is_healthy(&snapshot) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The one health rule from SPECS.md section 8: exit 0 only for a 200 whose
/// body says `status == "ok"`. A 200 from the `/health` fallback says
/// `"healthy"` and is therefore still degraded for this contract.
fn is_healthy(snapshot: &Snapshot) -> bool {
    snapshot.server.http_status == Some(200) && snapshot.server.status.as_deref() == Some("ok")
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

fn print_once(snapshot: &Snapshot) {
    let server = &snapshot.server;
    let port = snapshot
        .instances
        .iter()
        .find(|instance| instance.selected)
        .and_then(|instance| instance.port)
        .or_else(|| config::parse_url_port(&server.url).ok());
    let endpoint = match port {
        Some(port) => format!("{}:{}", server.mode.as_str(), port),
        None => server.mode.as_str().to_string(),
    };

    let status = server.status.as_deref().unwrap_or("unreachable");
    let http = option(server.http_status, |value| value.to_string());
    let version = server
        .version
        .as_deref()
        .map(|version| format!("v{version}"))
        .unwrap_or_else(|| "v?".to_string());
    let uptime = option(server.uptime_seconds, |value| {
        format::duration(value.max(0.0) as u64)
    });
    let latency = option(server.last_latency_ms, |value| format!("{value}ms"));

    println!("server   {endpoint}  {status} (http {http})  {version}  up {uptime}  {latency}");
    println!(
        "process  server {}",
        process_line(snapshot.processes.server.as_ref())
    );
    println!(
        "process  app    {}",
        process_line(snapshot.processes.app.as_ref())
    );

    match snapshot.system.memory.as_ref() {
        Some(memory) => {
            println!(
                "memory   used {} / {}  wired {}  compressed {}",
                format::bytes(memory.used_bytes),
                format::bytes(memory.total_bytes),
                format::bytes(memory.wired_bytes),
                format::bytes(memory.compressed_bytes)
            );
            println!(
                "         cached {}  free {}",
                format::bytes(memory.cached_bytes),
                format::bytes(memory.free_bytes)
            );
        }
        None => println!("memory   -"),
    }

    match snapshot.system.swap.as_ref() {
        Some(swap) => println!(
            "swap     {} / {}",
            format::bytes(swap.used_bytes),
            format::bytes(swap.total_bytes)
        ),
        None => println!("swap     -"),
    }

    match snapshot.system.pressure.as_ref() {
        Some(pressure) => {
            let label = pressure.kernel_label.as_deref().unwrap_or("unknown");
            let level = option(pressure.kernel_level, |value| value.to_string());
            let ratio = option(pressure.used_ratio, format::ratio_percent);
            println!("pressure {label} (level {level})  used {ratio}");
        }
        None => println!("pressure -"),
    }

    match snapshot.gpu.as_ref() {
        Some(gpu) => {
            println!(
                "gpu      device {}  renderer {}  tiler {}",
                percent_option(gpu.device_util_percent),
                percent_option(gpu.renderer_util_percent),
                percent_option(gpu.tiler_util_percent)
            );
            println!(
                "vram     in use {}  alloc {}",
                bytes_option(gpu.in_use_bytes),
                bytes_option(gpu.alloc_bytes)
            );
        }
        None => println!("gpu      -"),
    }

    let serving = &snapshot.serving;
    println!(
        "serving  {} requests  {} active  {} waiting",
        count(serving.total_requests),
        count(serving.active_requests),
        count(serving.waiting_requests)
    );
    println!(
        "tokens   prompt {}  completion {}  cached {}  cache eff {}",
        count(serving.total_prompt_tokens),
        count(serving.total_completion_tokens),
        count(serving.total_cached_tokens),
        option(serving.cache_efficiency, format::ratio_percent)
    );

    let models = &snapshot.models;
    let loaded_label = match (&models.loaded, models.loaded_count) {
        // The ids are unknown, so a count alone would look like "0 loaded".
        (None, _) => "loaded unknown".to_string(),
        (Some(_), Some(count)) => format!("{count} loaded"),
        (Some(loaded), None) => format!("{} loaded", loaded.len()),
    };
    println!(
        "models   {loaded_label} / {} discovered  memory {} / {}",
        count(models.discovered),
        bytes_option(models.memory_used_bytes),
        bytes_option(models.memory_max_bytes)
    );
}

fn process_line(process: Option<&model::ProcessMetrics>) -> String {
    match process {
        Some(process) => {
            let cpu = option(process.cpu_percent, format::percent);
            let footprint = bytes_option(process.footprint_bytes);
            format!("pid {}  cpu {cpu}  footprint {footprint}", process.pid)
        }
        None => "-".to_string(),
    }
}

fn count(value: Option<u64>) -> String {
    option(value, |value| value.to_string())
}

fn percent_option(value: Option<u32>) -> String {
    option(value, |value| format::percent(f64::from(value)))
}

fn bytes_option(value: Option<u64>) -> String {
    option(value, format::bytes)
}

fn option<T>(value: Option<T>, format_value: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "-".to_string(), format_value)
}
