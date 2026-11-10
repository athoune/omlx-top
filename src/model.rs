//! Data types shared by discovery, sampling and the UI.
//!
//! The JSON shape follows SPECS.md section 8.1 field by field. Every value that
//! can be unavailable is an `Option`, so a missing key serialises as `null`
//! rather than as a misleading zero (SPECS.md section 8.1: "Absent or
//! unavailable values are `null`, never `0`").

use serde::{Deserialize, Serialize};

/// How an oMLX server process was started.
///
/// SPECS.md section 6.3.2 calls the launchd case `launchd` in its table, while
/// section 7.2 and the JSON schema in section 8.1 spell the displayed value
/// `service`. The display contract wins here: `Service` serialises as
/// `"service"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    App,
    Service,
    Manual,
    #[default]
    Unknown,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::App => "app",
            Mode::Service => "service",
            Mode::Manual => "manual",
            Mode::Unknown => "unknown",
        }
    }
}

/// One discovered, addressable oMLX deployment.
///
/// `url` is an internal convenience for `--list` and the sampler; section 8.1
/// does not list it in `instances[]`, so it is not serialised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub mode: Mode,
    pub pid: i32,
    pub gui_pid: Option<i32>,
    pub port: Option<u16>,
    #[serde(default, skip_serializing)]
    pub url: Option<String>,
    pub selected: bool,
}

/// CPU and memory figures for a single process, as reported by the OS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessMetrics {
    pub pid: i32,
    /// `None` until two samples exist (SPECS.md section 5.1).
    pub cpu_percent: Option<f64>,
    /// `ri_phys_footprint`, the figure Activity Monitor calls "Memory".
    pub footprint_bytes: Option<u64>,
}

/// System memory accounting; see SPECS.md section 5.4 for the formulas.
///
/// Serialises as `system.memory` in the section 8.1 schema. The two derived
/// values that the schema does not expose (`app_memory_bytes` and
/// `uncompressed_bytes`) stay available for the TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub cached_bytes: u64,
    pub free_bytes: u64,
    /// Application memory: `(internal - purgeable) * page_size`.
    #[serde(skip_serializing)]
    pub app_memory_bytes: u64,
    /// Logical size of the data held by the compressor; never added to `used`.
    #[serde(skip_serializing)]
    pub uncompressed_bytes: u64,
}

/// Swap totals from `vm.swapusage`; serialises as `system.swap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// Kernel memory-pressure signal plus the derived used/total ratio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pressure {
    /// Raw `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning, 4 critical.
    pub kernel_level: Option<u32>,
    /// Label derived from [`Pressure::kernel_level`]; `None` when unavailable.
    pub kernel_label: Option<String>,
    /// `used_bytes / total_bytes`, kept separate from the kernel signal.
    pub used_ratio: Option<f64>,
}

/// The `server` block of the JSON schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerBlock {
    pub mode: Mode,
    pub pid: Option<i32>,
    pub gui_pid: Option<i32>,
    pub url: String,
    /// True when the transport answered, regardless of HTTP status.
    pub reachable: bool,
    pub http_status: Option<u16>,
    /// The body's `status` when present; `"unauthorized"` for 401/403.
    pub status: Option<String>,
    pub version: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub last_latency_ms: Option<u64>,
    /// Whether the discovered server pid still exists, checked with
    /// `kill(pid, 0)` when a poll fails at the transport level.
    ///
    /// TUI-only: SPECS.md section 7.2 uses it to distinguish `unresponsive`
    /// (alive but blocked) from `unreachable` (gone). It is skipped by serde,
    /// so the section 8.1 JSON contract is unchanged.
    #[serde(skip)]
    pub pid_alive: Option<bool>,
}

impl Default for ServerBlock {
    fn default() -> Self {
        ServerBlock {
            mode: Mode::Unknown,
            pid: None,
            gui_pid: None,
            url: String::new(),
            reachable: false,
            http_status: None,
            status: None,
            version: None,
            uptime_seconds: None,
            last_latency_ms: None,
            pid_alive: None,
        }
    }
}

/// Serving counters from `GET /api/status`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Serving {
    pub total_requests: Option<u64>,
    pub active_requests: Option<u64>,
    pub waiting_requests: Option<u64>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub total_cached_tokens: Option<u64>,
    pub cache_efficiency: Option<f64>,
    pub avg_prefill_tps: Option<f64>,
    pub avg_generation_tps: Option<f64>,
}

/// The `models` block of the JSON schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelsBlock {
    pub discovered: Option<u64>,
    pub loaded_count: Option<u64>,
    pub loading_count: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_max_bytes: Option<u64>,
    /// Ids of the models currently marked `loaded: true`.
    ///
    /// `Some([])` means a per-model source was fetched and nothing is loaded;
    /// `None` means the source carried no ids (the `/health` last resort or no
    /// source at all). SPECS.md section 8.1 requires that distinction.
    pub loaded: Option<Vec<String>>,
}

/// One row of the models table (SPECS.md section 7.5).
///
/// `models.loaded` stays the frozen JSON contract (section 8.1); these rows are
/// delivered alongside the snapshot for the TUI only, so they are skipped by
/// serde and never appear in `--json` output.
///
/// Built-in engines are included here with `builtin: true` so the table can
/// honour section 2.3's exclusion rule at render time without losing the
/// information that the server reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRow {
    pub id: String,
    pub loaded: bool,
    pub is_loading: bool,
    pub engine_type: Option<String>,
    pub model_type: Option<String>,
    pub context_window: Option<u64>,
    /// Server estimate; the table uses it when no actual size is reported.
    pub estimated_size: Option<u64>,
    /// Actual size when the server reports one.
    pub actual_size: Option<u64>,
    pub builtin: bool,
}

/// The `processes` block. `app` is `None` in service/manual mode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessesBlock {
    pub app: Option<ProcessMetrics>,
    pub server: Option<ProcessMetrics>,
}

/// The `system` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub cpu_percent: Option<f64>,
    pub memory: Option<VmMetrics>,
    pub swap: Option<SwapMetrics>,
    pub pressure: Option<Pressure>,
}

/// Whole-device GPU figures from IOKit `IOAccelerator.PerformanceStatistics`.
///
/// These are device-wide and are never attributed to oMLX (SPECS.md section
/// 5.5). A missing key is `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuBlock {
    pub device_util_percent: Option<u32>,
    pub renderer_util_percent: Option<u32>,
    pub tiler_util_percent: Option<u32>,
    pub in_use_bytes: Option<u64>,
    pub alloc_bytes: Option<u64>,
}

/// A full sample; the single object printed by `--json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub timestamp_unix_ms: u64,
    pub server: ServerBlock,
    pub instances: Vec<Instance>,
    pub serving: Serving,
    pub models: ModelsBlock,
    pub processes: ProcessesBlock,
    pub system: SystemBlock,
    pub gpu: Option<GpuBlock>,
    /// Per-model rows for the TUI table. Not part of the JSON contract.
    #[serde(skip)]
    pub model_rows: Vec<ModelRow>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;

    fn keys(value: &Value) -> BTreeSet<String> {
        value
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect()
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            timestamp_unix_ms: 1_794_226_879_000,
            server: ServerBlock {
                mode: Mode::App,
                pid: Some(93_916),
                gui_pid: Some(38_225),
                url: "http://127.0.0.1:8008".to_string(),
                reachable: true,
                http_status: Some(200),
                status: Some("ok".to_string()),
                version: Some("0.7.3".to_string()),
                uptime_seconds: Some(72_645.9),
                last_latency_ms: Some(3),
                pid_alive: None,
            },
            instances: vec![Instance {
                mode: Mode::App,
                pid: 93_916,
                gui_pid: Some(38_225),
                port: Some(8008),
                url: Some("http://127.0.0.1:8008".to_string()),
                selected: true,
            }],
            serving: Serving {
                total_requests: Some(3),
                active_requests: Some(0),
                waiting_requests: Some(0),
                total_prompt_tokens: Some(506_739),
                total_completion_tokens: Some(0),
                total_cached_tokens: Some(0),
                cache_efficiency: Some(0.0),
                avg_prefill_tps: Some(15_605.1),
                avg_generation_tps: Some(0.0),
            },
            models: ModelsBlock {
                discovered: Some(19),
                loaded_count: Some(1),
                loading_count: Some(0),
                memory_used_bytes: Some(288_687_808),
                memory_max_bytes: Some(11_374_576_480),
                loaded: Some(vec!["gemma-4-12B-it-4bit".to_string()]),
            },
            processes: ProcessesBlock {
                app: Some(ProcessMetrics {
                    pid: 38_225,
                    cpu_percent: Some(5.3),
                    footprint_bytes: Some(33_520),
                }),
                server: Some(ProcessMetrics {
                    pid: 93_916,
                    cpu_percent: Some(0.9),
                    footprint_bytes: Some(120_000_000),
                }),
            },
            system: SystemBlock {
                cpu_percent: Some(12.4),
                memory: Some(VmMetrics {
                    total_bytes: 34_359_738_368,
                    used_bytes: 13_200_000_000,
                    wired_bytes: 4_400_000_000,
                    compressed_bytes: 12_000_000_000,
                    cached_bytes: 5_800_000_000,
                    free_bytes: 1_180_000_000,
                    app_memory_bytes: 7_400_000_000,
                    uncompressed_bytes: 2_900_000_000,
                }),
                swap: Some(SwapMetrics {
                    total_bytes: 4_294_967_296,
                    used_bytes: 3_078_000_000,
                }),
                pressure: Some(Pressure {
                    kernel_level: Some(2),
                    kernel_label: Some("warning".to_string()),
                    used_ratio: Some(0.78),
                }),
            },
            gpu: Some(GpuBlock {
                device_util_percent: Some(17),
                renderer_util_percent: Some(17),
                tiler_util_percent: Some(18),
                in_use_bytes: Some(1_176_158_208),
                alloc_bytes: Some(7_001_751_552),
            }),
            model_rows: Vec::new(),
        }
    }

    #[test]
    fn serialises_one_line_with_the_section_8_1_shape() {
        let snapshot = sample_snapshot();
        let text = serde_json::to_string(&snapshot).expect("serialisable");
        assert!(!text.contains('\n'), "must stay on one line");
        assert!(
            !text.contains("model_rows"),
            "the TUI-only rows must stay out of --json"
        );

        let value: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(
            keys(&value),
            BTreeSet::from(
                [
                    "timestamp_unix_ms",
                    "server",
                    "instances",
                    "serving",
                    "models",
                    "processes",
                    "system",
                    "gpu",
                ]
                .map(str::to_string)
            )
        );

        let server = &value["server"];
        assert_eq!(
            keys(server),
            BTreeSet::from(
                [
                    "mode",
                    "pid",
                    "gui_pid",
                    "url",
                    "reachable",
                    "http_status",
                    "status",
                    "version",
                    "uptime_seconds",
                    "last_latency_ms",
                ]
                .map(str::to_string)
            )
        );
        assert_eq!(server["mode"], "app");
        assert_eq!(server["pid"], 93_916);

        let instance = &value["instances"][0];
        assert_eq!(
            keys(instance),
            BTreeSet::from(["mode", "pid", "gui_pid", "port", "selected"].map(str::to_string)),
            "instances[] must not leak the internal url field"
        );

        let serving = &value["serving"];
        assert_eq!(
            keys(serving),
            BTreeSet::from(
                [
                    "total_requests",
                    "active_requests",
                    "waiting_requests",
                    "total_prompt_tokens",
                    "total_completion_tokens",
                    "total_cached_tokens",
                    "cache_efficiency",
                    "avg_prefill_tps",
                    "avg_generation_tps",
                ]
                .map(str::to_string)
            )
        );

        let models = &value["models"];
        assert_eq!(
            keys(models),
            BTreeSet::from(
                [
                    "discovered",
                    "loaded_count",
                    "loading_count",
                    "memory_used_bytes",
                    "memory_max_bytes",
                    "loaded",
                ]
                .map(str::to_string)
            )
        );
        assert_eq!(models["loaded"][0], "gemma-4-12B-it-4bit");

        let processes = &value["processes"];
        assert_eq!(
            keys(processes),
            BTreeSet::from(["app", "server"].map(str::to_string))
        );
        assert_eq!(
            keys(&processes["server"]),
            BTreeSet::from(["pid", "cpu_percent", "footprint_bytes"].map(str::to_string))
        );

        let system = &value["system"];
        assert_eq!(
            keys(system),
            BTreeSet::from(["cpu_percent", "memory", "swap", "pressure"].map(str::to_string))
        );
        assert_eq!(
            keys(&system["memory"]),
            BTreeSet::from(
                [
                    "total_bytes",
                    "used_bytes",
                    "wired_bytes",
                    "compressed_bytes",
                    "cached_bytes",
                    "free_bytes",
                ]
                .map(str::to_string)
            ),
            "the derived-only memory fields stay out of the JSON contract"
        );
        assert_eq!(
            keys(&system["swap"]),
            BTreeSet::from(["total_bytes", "used_bytes"].map(str::to_string))
        );
        assert_eq!(
            keys(&system["pressure"]),
            BTreeSet::from(["kernel_level", "kernel_label", "used_ratio"].map(str::to_string))
        );

        assert_eq!(
            keys(&value["gpu"]),
            BTreeSet::from(
                [
                    "device_util_percent",
                    "renderer_util_percent",
                    "tiler_util_percent",
                    "in_use_bytes",
                    "alloc_bytes",
                ]
                .map(str::to_string)
            )
        );
    }

    #[test]
    fn unavailable_values_serialise_as_null_not_zero() {
        let snapshot = Snapshot {
            timestamp_unix_ms: 1,
            server: ServerBlock {
                url: "http://127.0.0.1:8000".to_string(),
                ..ServerBlock::default()
            },
            ..Snapshot::default()
        };
        let value: Value = serde_json::to_value(&snapshot).expect("serialisable");

        assert!(value["server"]["pid"].is_null());
        assert!(value["server"]["http_status"].is_null());
        assert!(value["server"]["status"].is_null());
        assert!(value["processes"]["app"].is_null());
        assert!(value["processes"]["server"].is_null());
        assert!(value["system"]["cpu_percent"].is_null());
        assert!(value["system"]["memory"].is_null());
        assert!(value["gpu"].is_null());
        assert!(value["models"]["discovered"].is_null());
        assert!(value["models"]["loaded_count"].is_null());
        assert!(value["models"]["loaded"].is_null());

        // An empty array means "fetched, nothing loaded"; it is not the same
        // as an unknown list (`null`).
        let empty_models = ModelsBlock {
            loaded: Some(Vec::new()),
            ..ModelsBlock::default()
        };
        let value = serde_json::to_value(empty_models).expect("serialisable");
        assert_eq!(value["loaded"], serde_json::json!([]));

        // A partial memory block keeps the known fields and leaves the rest
        // null rather than inventing zeros.
        let partial = SystemBlock {
            memory: Some(VmMetrics {
                total_bytes: 100,
                used_bytes: 40,
                wired_bytes: 10,
                compressed_bytes: 5,
                cached_bytes: 20,
                free_bytes: 25,
                app_memory_bytes: 25,
                uncompressed_bytes: 4,
            }),
            ..SystemBlock::default()
        };
        let value = serde_json::to_value(partial).expect("serialisable");
        assert_eq!(value["memory"]["used_bytes"], 40);
        assert!(value["cpu_percent"].is_null());
    }
}
