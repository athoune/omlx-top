//! Process metadata, per-process resource usage and CPU delta computation.
//!
//! Sources: `proc_listallpids`, `proc_pidpath`, `proc_name`,
//! `proc_pidinfo(PROC_PIDTBSDINFO)`, `proc_pid_rusage(RUSAGE_INFO_V0)` and
//! `sysctl(KERN_PROCARGS2)`.

use std::ptr;

use super::info::MachTimebaseInfo;
use super::SysError;

const CTL_KERN: libc::c_int = 1;
const KERN_PROCARGS2: libc::c_int = 49;
const PROC_PIDTBSDINFO: libc::c_int = 3;
const RUSAGE_INFO_V0: libc::c_int = 0;
const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;
const MAXCOMLEN: usize = 16;

extern "C" {
    fn proc_listallpids(buffer: *mut libc::c_void, buffersize: libc::c_int) -> libc::c_int;
    fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, buffersize: u32) -> libc::c_int;
    fn proc_name(pid: libc::c_int, buffer: *mut libc::c_void, buffersize: u32) -> libc::c_int;
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        arg: u64,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
    fn proc_pid_rusage(
        pid: libc::c_int,
        flavor: libc::c_int,
        buffer: *mut libc::c_void,
    ) -> libc::c_int;
    fn mach_absolute_time() -> u64;
}

/// `struct rusage_info_v0` from `sys/resource.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RusageInfoV0 {
    pub ri_uuid: [u8; 16],
    pub ri_user_time: u64,
    pub ri_system_time: u64,
    pub ri_pkg_idle_wkups: u64,
    pub ri_interrupt_wkups: u64,
    pub ri_pageins: u64,
    pub ri_wired_size: u64,
    pub ri_resident_size: u64,
    pub ri_phys_footprint: u64,
    pub ri_proc_start_abstime: u64,
    pub ri_proc_exit_abstime: u64,
}

/// `struct proc_bsdinfo` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [u8; MAXCOMLEN],
    pbi_name: [u8; 2 * MAXCOMLEN],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

/// One reading of a process's cumulative CPU time and footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawUsage {
    /// `ri_user_time + ri_system_time`, in mach absolute ticks.
    pub ticks: u64,
    /// `ri_phys_footprint`, in bytes.
    pub footprint_bytes: u64,
    /// Start time used to detect pid reuse.
    pub start_abstime: u64,
}

/// A single input to the CPU delta calculator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    pub ticks: u64,
    pub start_abstime: u64,
    pub wall_ns: u64,
}

/// Converts consecutive CPU samples into a percentage of one core.
///
/// The first call always returns `None`, and a counter reset or a pid reuse
/// resynchronises the sampler without reporting a bogus value.
#[derive(Debug, Default)]
pub struct CpuSampler {
    previous: Option<CpuSample>,
}

impl CpuSampler {
    pub fn push(&mut self, sample: CpuSample, timebase: MachTimebaseInfo) -> Option<f64> {
        let previous = self.previous.replace(sample)?;

        // A different start time means the pid now belongs to another process.
        if sample.start_abstime != previous.start_abstime {
            return None;
        }
        // A non-monotonic wall clock or tick counter is a reset, not a sample.
        if sample.wall_ns <= previous.wall_ns || sample.ticks < previous.ticks {
            return None;
        }

        let delta_ns = mach_ticks_to_ns(sample.ticks - previous.ticks, timebase)?;
        let delta_wall_ns = sample.wall_ns - previous.wall_ns;
        Some(delta_ns as f64 / delta_wall_ns as f64 * 100.0)
    }
}

/// Convert mach absolute ticks to nanoseconds.
pub fn mach_ticks_to_ns(ticks: u64, timebase: MachTimebaseInfo) -> Option<u64> {
    if timebase.denom == 0 {
        return None;
    }
    let nanos = (ticks as u128 * timebase.numer as u128) / timebase.denom as u128;
    Some(nanos as u64)
}

/// Current value of the mach absolute clock.
pub fn mach_now() -> u64 {
    // SAFETY: `mach_absolute_time` takes no arguments and cannot fail.
    unsafe { mach_absolute_time() }
}

/// List all process ids. The kernel may return fewer pids than it reported a
/// moment earlier; the caller tolerates that normal churn.
pub fn list_pids() -> Result<Vec<i32>, SysError> {
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { proc_listallpids(ptr::null_mut(), 0) };
    if count < 0 {
        return Err(SysError::last_os_error("proc_listallpids"));
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    // Leave headroom for processes started between the count and the read.
    let mut pids = vec![0i32; count as usize + 64];
    let buffer_size = pids.len() * std::mem::size_of::<i32>();
    // SAFETY: the buffer is valid for `buffer_size` bytes.
    let found = unsafe {
        proc_listallpids(
            pids.as_mut_ptr() as *mut libc::c_void,
            buffer_size as libc::c_int,
        )
    };
    if found < 0 {
        return Err(SysError::last_os_error("proc_listallpids"));
    }

    pids.truncate(found as usize);
    pids.retain(|pid| *pid > 0);
    Ok(pids)
}

/// Executable path of a process, as resolved by `proc_pidpath`.
pub fn pid_path(pid: i32) -> Option<String> {
    let mut buffer = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    // SAFETY: the buffer is valid for the stated length.
    let length = unsafe {
        proc_pidpath(
            pid,
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    let length = length as usize;
    Some(String::from_utf8_lossy(&buffer[..length]).into_owned())
}

/// Short process name, as resolved by `proc_name`.
pub fn process_name(pid: i32) -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for the stated length.
    let length = unsafe {
        proc_name(
            pid,
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    let end = buffer[..length as usize]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length as usize);
    Some(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

fn bsd_info(pid: i32) -> Result<ProcBsdInfo, SysError> {
    let mut info = ProcBsdInfo::default();
    let size = std::mem::size_of::<ProcBsdInfo>();
    // SAFETY: `info` is a correctly sized, writable `struct proc_bsdinfo`.
    let result = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size as libc::c_int,
        )
    };
    if result <= 0 {
        return Err(SysError::last_os_error("proc_pidinfo(PROC_PIDTBSDINFO)"));
    }
    Ok(info)
}

/// Parent pid, used for mode classification. EPERM is surfaced as an error so
/// discovery can report an unreadable ancestry rather than guessing.
pub fn parent_pid(pid: i32) -> Result<i32, SysError> {
    let info = bsd_info(pid)?;
    Ok(info.pbi_ppid as i32)
}

/// Owning uid, used to report candidates we cannot fully inspect.
pub fn owner_uid(pid: i32) -> Result<u32, SysError> {
    let info = bsd_info(pid)?;
    Ok(info.pbi_uid)
}

/// Read `rusage_info_v0` for a process.
pub fn read_usage(pid: i32) -> Result<RawUsage, SysError> {
    let mut info = RusageInfoV0::default();
    // SAFETY: `info` is a correctly sized, writable `struct rusage_info_v0`.
    let result = unsafe {
        proc_pid_rusage(
            pid,
            RUSAGE_INFO_V0,
            &mut info as *mut _ as *mut libc::c_void,
        )
    };
    if result != 0 {
        return Err(SysError::last_os_error("proc_pid_rusage"));
    }

    Ok(RawUsage {
        ticks: info.ri_user_time.saturating_add(info.ri_system_time),
        footprint_bytes: info.ri_phys_footprint,
        start_abstime: info.ri_proc_start_abstime,
    })
}

/// The argument and environment block of a process, parsed from
/// `KERN_PROCARGS2`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcArgs {
    pub exec_path: String,
    pub argv: Vec<String>,
    pub env: Vec<String>,
}

impl ProcArgs {
    pub fn env_value(&self, key: &str) -> Option<&str> {
        let prefix = format!("{key}=");
        self.env
            .iter()
            .find_map(|entry| entry.strip_prefix(&prefix))
    }

    /// `OMLX_PORT`, when present and valid.
    pub fn env_port(&self) -> Option<u16> {
        self.env_value("OMLX_PORT")
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|port| *port != 0)
    }

    /// `OMLX_SUPERVISED`, when present. Used only to confirm mode
    /// classification; it never decides acceptance.
    pub fn env_supervised(&self) -> Option<&str> {
        self.env_value("OMLX_SUPERVISED")
    }

    /// The rewritten process title. `setproctitle` puts `omlx-server` here.
    pub fn argv0(&self) -> Option<&str> {
        self.argv.first().map(String::as_str)
    }
}

/// Read and parse `KERN_PROCARGS2` for a process.
pub fn read_procargs(pid: i32) -> Result<ProcArgs, SysError> {
    let mut mib = [CTL_KERN, KERN_PROCARGS2, pid];
    let mut size = 0usize;

    // SAFETY: a null buffer asks only for the required size.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(SysError::last_os_error("sysctl(KERN_PROCARGS2)"));
    }
    if size < std::mem::size_of::<i32>() {
        return Err(SysError::Invalid(
            "sysctl(KERN_PROCARGS2)",
            "a truncated argument block",
        ));
    }

    let mut buffer = vec![0u8; size];
    // SAFETY: the buffer is valid for `size` bytes.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buffer.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(SysError::last_os_error("sysctl(KERN_PROCARGS2)"));
    }

    parse_procargs(&buffer[..size.min(buffer.len())]).ok_or(SysError::Invalid(
        "sysctl(KERN_PROCARGS2)",
        "a malformed argument block",
    ))
}

fn parse_procargs(bytes: &[u8]) -> Option<ProcArgs> {
    if bytes.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if argc < 0 {
        return None;
    }
    let argc = argc as usize;
    let mut index = 4usize;

    let exec_end = bytes[index..].iter().position(|byte| *byte == 0)? + index;
    let exec_path = String::from_utf8_lossy(&bytes[index..exec_end]).into_owned();
    index = exec_end + 1;

    // The kernel pads the executable path area with NULs before argv.
    while index < bytes.len() && bytes[index] == 0 {
        index += 1;
    }

    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        if index >= bytes.len() {
            break;
        }
        let end = bytes[index..].iter().position(|byte| *byte == 0)? + index;
        argv.push(String::from_utf8_lossy(&bytes[index..end]).into_owned());
        index = end + 1;
    }

    // Everything left is the environment, one NUL-terminated string per entry.
    let mut env = Vec::new();
    while index < bytes.len() {
        let end = bytes[index..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(bytes.len(), |offset| index + offset);
        if end > index {
            let entry = String::from_utf8_lossy(&bytes[index..end]).into_owned();
            if entry.contains('=') {
                env.push(entry);
            }
        }
        if end >= bytes.len() {
            break;
        }
        index = end + 1;
    }

    Some(ProcArgs {
        exec_path,
        argv,
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TB: MachTimebaseInfo = MachTimebaseInfo { numer: 1, denom: 1 };

    fn sample(ticks: u64, start: u64, wall_ns: u64) -> CpuSample {
        CpuSample {
            ticks,
            start_abstime: start,
            wall_ns,
        }
    }

    #[test]
    fn first_sample_is_none() {
        let mut sampler = CpuSampler::default();
        assert_eq!(sampler.push(sample(100, 7, 1_000), TB), None);
    }

    #[test]
    fn cpu_delta_uses_ticks_and_wall_clock() {
        let mut sampler = CpuSampler::default();
        assert_eq!(sampler.push(sample(100, 7, 1_000), TB), None);

        // 100 ticks of CPU in 100 ns of wall time is one full core.
        assert_eq!(sampler.push(sample(200, 7, 1_100), TB), Some(100.0));

        // 50 ticks of CPU in 200 ns of wall time is a quarter core.
        assert_eq!(sampler.push(sample(250, 7, 1_300), TB), Some(25.0));
    }

    #[test]
    fn timebase_is_applied() {
        let mut sampler = CpuSampler::default();
        let timebase = MachTimebaseInfo {
            numer: 125,
            denom: 3,
        };
        assert_eq!(sampler.push(sample(0, 7, 0), timebase), None);

        // 24 ticks * 125/3 = 1000 ns over 2000 ns of wall time = 50 %.
        assert_eq!(sampler.push(sample(24, 7, 2_000), timebase), Some(50.0));
    }

    #[test]
    fn counter_reset_is_detected_and_skipped() {
        let mut sampler = CpuSampler::default();
        assert_eq!(sampler.push(sample(500, 7, 1_000), TB), None);
        assert_eq!(sampler.push(sample(100, 7, 2_000), TB), None);
        // The next pair is measured from the reset sample.
        assert_eq!(sampler.push(sample(150, 7, 3_000), TB), Some(5.0));
    }

    #[test]
    fn pid_reuse_is_detected_and_skipped() {
        let mut sampler = CpuSampler::default();
        assert_eq!(sampler.push(sample(100, 11, 1_000), TB), None);
        // Same pid, different start time: the counter belongs to a new process.
        assert_eq!(sampler.push(sample(900, 12, 2_000), TB), None);
        assert_eq!(sampler.push(sample(1_000, 12, 3_000), TB), Some(10.0));
    }

    #[test]
    fn non_monotonic_wall_clock_is_skipped() {
        let mut sampler = CpuSampler::default();
        assert_eq!(sampler.push(sample(100, 7, 1_000), TB), None);
        assert_eq!(sampler.push(sample(200, 7, 1_000), TB), None);
    }

    #[test]
    fn procargs_parse_synthetic_block() {
        let mut block = Vec::new();
        block.extend_from_slice(&2i32.to_ne_bytes());
        block.extend_from_slice(b"/usr/bin/python3\0");
        block.extend_from_slice(&[0, 0, 0]); // kernel padding
        block.extend_from_slice(b"omlx-server\0serve\0");
        block.extend_from_slice(b"OMLX_PORT=8010\0OMLX_SUPERVISED=launchd\0PATH=/usr/bin\0");

        let parsed = parse_procargs(&block).expect("synthetic block parses");
        assert_eq!(parsed.exec_path, "/usr/bin/python3");
        assert_eq!(parsed.argv, vec!["omlx-server", "serve"]);
        assert_eq!(parsed.argv0(), Some("omlx-server"));
        assert_eq!(parsed.env_port(), Some(8010));
        assert_eq!(parsed.env_supervised(), Some("launchd"));
    }

    #[test]
    fn procargs_tolerates_padding_and_garbage() {
        assert!(parse_procargs(&[]).is_none());
        assert!(parse_procargs(&[1, 0, 0]).is_none());
    }
}
