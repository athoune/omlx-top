//! Data types shared by discovery, sampling and (in later batches) the UI.
//!
//! Only the types needed by the first batch are populated; the shape follows
//! the JSON schema in SPECS.md section 8.1 so later batches can extend it
//! without renaming fields.

use serde::{Deserialize, Serialize};

/// How an oMLX server process was started.
///
/// SPECS.md section 6.3.2 calls the launchd case `launchd` in its table, while
/// section 7.2 and the JSON schema in section 8.1 spell the displayed value
/// `service`. The display contract wins here: `Service` serialises as
/// `"service"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    App,
    Service,
    Manual,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub mode: Mode,
    pub pid: i32,
    pub gui_pid: Option<i32>,
    pub port: Option<u16>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub wired_bytes: u64,
    pub compressed_bytes: u64,
    pub cached_bytes: u64,
    pub free_bytes: u64,
    /// Application memory: `(internal - purgeable) * page_size`.
    pub app_memory_bytes: u64,
    /// Logical size of the data held by the compressor; never added to `used`.
    pub uncompressed_bytes: u64,
}

/// Swap totals from `vm.swapusage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// Kernel memory-pressure signal plus the derived used/total ratio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pressure {
    /// Raw `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning, 4 critical.
    pub kernel_level: u32,
    pub kernel_label: String,
    /// `used_bytes / total_bytes`, kept separate from the kernel signal.
    pub used_ratio: Option<f64>,
}

/// Host-wide counters. Per-process figures live in [`ProcessMetrics`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostMetrics {
    pub cpu_percent: Option<f64>,
    pub memory: Option<VmMetrics>,
    pub swap: Option<SwapMetrics>,
    pub pressure: Option<Pressure>,
}

/// A full sample. Intentionally minimal in this batch; later batches add the
/// HTTP-derived serving and model blocks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub timestamp_unix_ms: u64,
    pub instances: Vec<Instance>,
    pub host: HostMetrics,
}
