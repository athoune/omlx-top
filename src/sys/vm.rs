//! Host VM statistics, swap, memory pressure and system CPU.
//!
//! Sources: `host_statistics64(HOST_VM_INFO64)`,
//! `host_statistics(HOST_CPU_LOAD_INFO)`, `sysctl vm.swapusage` and
//! `sysctl kern.memorystatus_vm_pressure_level`.

use std::mem::{offset_of, size_of};
use std::sync::OnceLock;

use super::{struct_as_bytes_mut, sysctl_bytes, sysctl_i32, SysError};
use crate::model::{Pressure, SwapMetrics, VmMetrics};

const HOST_VM_INFO64: libc::c_int = 4;
const HOST_CPU_LOAD_INFO: libc::c_int = 3;

const CPU_STATE_USER: usize = 0;
const CPU_STATE_SYSTEM: usize = 1;
const CPU_STATE_IDLE: usize = 2;
const CPU_STATE_NICE: usize = 3;

extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics64(
        host: u32,
        flavor: libc::c_int,
        info: *mut libc::c_int,
        count: *mut u32,
    ) -> libc::c_int;
    fn host_statistics(
        host: u32,
        flavor: libc::c_int,
        info: *mut libc::c_int,
        count: *mut u32,
    ) -> libc::c_int;
}

/// Cached host port.
///
/// `mach_host_self()` returns a send right and increments its user reference
/// count on every call, so calling it once per tick would leak a port right
/// every tick. Resolve it exactly once and reuse the same right for both
/// `host_statistics64` and `host_statistics`.
static HOST_PORT: OnceLock<u32> = OnceLock::new();

fn host_port() -> u32 {
    *HOST_PORT.get_or_init(|| {
        // SAFETY: `mach_host_self` takes no arguments; the returned send right
        // is deliberately kept for the lifetime of the process.
        unsafe { mach_host_self() }
    })
}

/// The revision-1 portion of `struct vm_statistics64` from
/// `mach/vm_statistics.h`.
///
/// It stops at `swapped_count`; every field used by SPECS.md section 5.4 is
/// present. Passing the matching count keeps this code valid on kernels that
/// do not know the newer revision fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VmStatistics64 {
    pub free_count: u32,
    pub active_count: u32,
    pub inactive_count: u32,
    pub wire_count: u32,
    pub zero_fill_count: u64,
    pub reactivations: u64,
    pub pageins: u64,
    pub pageouts: u64,
    pub faults: u64,
    pub cow_faults: u64,
    pub lookups: u64,
    pub hits: u64,
    pub purges: u64,
    pub purgeable_count: u32,
    pub speculative_count: u32,
    pub decompressions: u64,
    pub compressions: u64,
    pub swapins: u64,
    pub swapouts: u64,
    pub compressor_page_count: u32,
    pub throttled_count: u32,
    pub external_page_count: u32,
    pub internal_page_count: u32,
    pub total_uncompressed_pages_in_compressor: u64,
    pub swapped_count: u64,
}

/// `struct host_cpu_load_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HostCpuLoadInfo {
    pub cpu_ticks: [u32; 4],
}

/// `struct xsw_usage` from `sys/sysctl.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XswUsage {
    pub xsu_total: u64,
    pub xsu_avail: u64,
    pub xsu_used: u64,
    pub xsu_pagesize: u32,
    pub xsu_encrypted: i32,
}

/// Read one `vm_statistics64` snapshot.
pub fn read_vm_stats() -> Result<VmStatistics64, SysError> {
    let mut stats = VmStatistics64::default();
    // HOST_VM_INFO64_REV1_COUNT is the offset of `swapped_count`, in integers.
    let mut count = (offset_of!(VmStatistics64, swapped_count) / size_of::<libc::c_int>()) as u32;

    // SAFETY: `stats` is a correctly sized POD struct and `count` describes
    // the number of 32-bit words the kernel may write.
    let result = unsafe {
        host_statistics64(
            host_port(),
            HOST_VM_INFO64,
            &mut stats as *mut _ as *mut libc::c_int,
            &mut count,
        )
    };
    if result != 0 {
        return Err(SysError::Mach("host_statistics64", result));
    }
    Ok(stats)
}

/// Apply the memory formulas from SPECS.md section 5.4.
pub fn account_pages(stats: &VmStatistics64, page_size: u64, total_bytes: u64) -> VmMetrics {
    let scaled = |pages: u32| u64::from(pages).saturating_mul(page_size);

    let app_memory_bytes = scaled(
        stats
            .internal_page_count
            .saturating_sub(stats.purgeable_count),
    );
    let wired_bytes = scaled(stats.wire_count);
    let compressed_bytes = scaled(stats.compressor_page_count);
    let used_bytes = app_memory_bytes
        .saturating_add(wired_bytes)
        .saturating_add(compressed_bytes);
    let cached_bytes = scaled(stats.external_page_count);
    let free_bytes = u64::from(stats.free_count)
        .saturating_add(u64::from(stats.speculative_count))
        .saturating_mul(page_size);
    let uncompressed_bytes = stats
        .total_uncompressed_pages_in_compressor
        .saturating_mul(page_size);

    VmMetrics {
        total_bytes,
        used_bytes,
        wired_bytes,
        compressed_bytes,
        cached_bytes,
        free_bytes,
        app_memory_bytes,
        uncompressed_bytes,
    }
}

/// Read and account system memory using the machine page size.
pub fn sample_memory() -> Result<VmMetrics, SysError> {
    let info = super::info::init()?;
    let stats = read_vm_stats()?;
    Ok(account_pages(&stats, info.pagesize, info.memsize))
}

/// Read swap totals.
pub fn swap() -> Result<SwapMetrics, SysError> {
    let mut usage = XswUsage::default();
    let length = sysctl_bytes(c"vm.swapusage", struct_as_bytes_mut(&mut usage))?;
    if length < size_of::<XswUsage>() {
        return Err(SysError::Invalid("sysctl(vm.swapusage)", "a short struct"));
    }
    Ok(SwapMetrics {
        total_bytes: usage.xsu_total,
        used_bytes: usage.xsu_used,
    })
}

/// Raw `kern.memorystatus_vm_pressure_level` value.
pub fn pressure_level() -> Result<u32, SysError> {
    let level = sysctl_i32(c"kern.memorystatus_vm_pressure_level")?;
    Ok(level.max(0) as u32)
}

/// Human label for the kernel pressure level. Unknown values are shown as
/// `unknown`; they are never silently coerced.
pub fn pressure_label(level: u32) -> &'static str {
    match level {
        1 => "normal",
        2 => "warning",
        4 => "critical",
        _ => "unknown",
    }
}

/// Combine the kernel level with the derived used/total ratio.
///
/// `level` is `None` when the sysctl failed: the ratio is still useful, and the
/// missing kernel signal must stay `null` rather than becoming a fake `normal`.
pub fn pressure(level: Option<u32>, total_bytes: u64, used_bytes: u64) -> Pressure {
    let used_ratio = if total_bytes == 0 {
        None
    } else {
        Some(used_bytes as f64 / total_bytes as f64)
    };

    Pressure {
        kernel_level: level,
        kernel_label: level.map(|level| pressure_label(level).to_string()),
        used_ratio,
    }
}

/// Read the host CPU tick counters.
pub fn read_cpu_ticks() -> Result<[u32; 4], SysError> {
    let mut load = HostCpuLoadInfo::default();
    let mut count = (size_of::<HostCpuLoadInfo>() / size_of::<libc::c_int>()) as u32;

    // SAFETY: `load` is a correctly sized POD struct and `count` is its size
    // in 32-bit words.
    let result = unsafe {
        host_statistics(
            host_port(),
            HOST_CPU_LOAD_INFO,
            &mut load as *mut _ as *mut libc::c_int,
            &mut count,
        )
    };
    if result != 0 {
        return Err(SysError::Mach("host_statistics", result));
    }
    Ok(load.cpu_ticks)
}

/// Converts consecutive host CPU tick snapshots into a busy percentage.
#[derive(Debug, Default)]
pub struct SystemCpuSampler {
    previous: Option<[u32; 4]>,
}

impl SystemCpuSampler {
    /// Forget the baseline, so the next push reports `None`.
    pub fn reset(&mut self) {
        self.previous = None;
    }

    pub fn push(&mut self, ticks: [u32; 4]) -> Option<f64> {
        let previous = self.previous.replace(ticks)?;

        let mut delta = [0u64; 4];
        for (index, (current, old)) in ticks.iter().zip(previous.iter()).enumerate() {
            if current < old {
                return None;
            }
            delta[index] = u64::from(current - old);
        }

        let busy = delta[CPU_STATE_USER] + delta[CPU_STATE_SYSTEM] + delta[CPU_STATE_NICE];
        let all = busy + delta[CPU_STATE_IDLE];
        if all == 0 {
            return None;
        }
        Some(busy as f64 / all as f64 * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_stats() -> VmStatistics64 {
        VmStatistics64 {
            free_count: 40,
            wire_count: 10,
            purgeable_count: 5,
            speculative_count: 2,
            compressor_page_count: 20,
            external_page_count: 30,
            internal_page_count: 100,
            total_uncompressed_pages_in_compressor: 50,
            ..VmStatistics64::default()
        }
    }

    #[test]
    fn page_accounting_follows_the_spec() {
        assert_eq!(size_of::<VmStatistics64>(), 160);
        let stats = synthetic_stats();
        let page_size = 16_384;
        let total = 34_359_738_368;
        let metrics = account_pages(&stats, page_size, total);

        assert_eq!(metrics.total_bytes, total);
        assert_eq!(metrics.app_memory_bytes, 95 * page_size);
        assert_eq!(metrics.wired_bytes, 10 * page_size);
        assert_eq!(metrics.compressed_bytes, 20 * page_size);
        assert_eq!(metrics.used_bytes, (95 + 10 + 20) * page_size);
        assert_eq!(metrics.cached_bytes, 30 * page_size);
        assert_eq!(metrics.free_bytes, (40 + 2) * page_size);
        assert_eq!(metrics.uncompressed_bytes, 50 * page_size);
        // The logical compressor size is never folded into "used".
        assert_ne!(metrics.used_bytes, (95 + 10 + 20 + 50) * page_size);
    }

    #[test]
    fn purgeable_pages_larger_than_internal_do_not_underflow() {
        let stats = VmStatistics64 {
            internal_page_count: 3,
            purgeable_count: 5,
            ..VmStatistics64::default()
        };
        let metrics = account_pages(&stats, 4096, 0);
        assert_eq!(metrics.app_memory_bytes, 0);
    }

    #[test]
    fn pressure_labels() {
        assert_eq!(pressure_label(1), "normal");
        assert_eq!(pressure_label(2), "warning");
        assert_eq!(pressure_label(4), "critical");
        assert_eq!(pressure_label(0), "unknown");
        assert_eq!(pressure_label(3), "unknown");
    }

    #[test]
    fn pressure_ratio_is_derived_separately() {
        let warning = pressure(Some(2), 100, 78);
        assert_eq!(warning.kernel_level, Some(2));
        assert_eq!(warning.kernel_label.as_deref(), Some("warning"));
        assert_eq!(warning.used_ratio, Some(0.78));

        let unavailable = pressure(Some(1), 0, 0);
        assert_eq!(unavailable.used_ratio, None);

        let missing_level = pressure(None, 100, 78);
        assert_eq!(missing_level.kernel_level, None);
        assert_eq!(missing_level.kernel_label, None);
        assert_eq!(missing_level.used_ratio, Some(0.78));
    }

    #[test]
    fn system_cpu_first_sample_and_delta() {
        let mut sampler = SystemCpuSampler::default();
        assert_eq!(sampler.push([10, 10, 10, 0]), None);
        // Busy 30, idle 10 -> 75 %.
        assert_eq!(sampler.push([25, 20, 20, 5]), Some(75.0));
        // A wrap or reset is skipped.
        assert_eq!(sampler.push([1, 1, 1, 1]), None);
    }
}
