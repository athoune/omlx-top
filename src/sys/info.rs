//! One-shot machine facts, read once at startup.

use std::sync::OnceLock;

use super::{sysctl_string, sysctl_u64, SysError};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

#[derive(Debug, Clone)]
pub struct SystemInfo {
    /// `hw.memsize`
    pub memsize: u64,
    /// `hw.pagesize`; 16384 on Apple Silicon.
    pub pagesize: u64,
    /// `machdep.cpu.brand_string`.
    pub chip: String,
    /// Mach absolute-time to nanosecond conversion.
    pub timebase: MachTimebaseInfo,
}

static INFO: OnceLock<SystemInfo> = OnceLock::new();

extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
}

/// Load the machine facts exactly once. Later calls return the cached values.
pub fn init() -> Result<&'static SystemInfo, SysError> {
    if let Some(info) = INFO.get() {
        return Ok(info);
    }
    let loaded = load()?;
    Ok(INFO.get_or_init(|| loaded))
}

pub fn get() -> Option<&'static SystemInfo> {
    INFO.get()
}

fn load() -> Result<SystemInfo, SysError> {
    let memsize = sysctl_u64(c"hw.memsize")?;
    let pagesize = sysctl_u64(c"hw.pagesize")?;
    let chip = sysctl_string(c"machdep.cpu.brand_string")?;

    let mut timebase = MachTimebaseInfo { numer: 1, denom: 1 };
    // SAFETY: the pointer is valid for the duration of the call.
    let result = unsafe { mach_timebase_info(&mut timebase) };
    if result != 0 {
        return Err(SysError::Mach("mach_timebase_info", result));
    }
    if timebase.numer == 0 || timebase.denom == 0 {
        return Err(SysError::Invalid("mach_timebase_info", "a zero timebase"));
    }

    Ok(SystemInfo {
        memsize,
        pagesize,
        chip,
        timebase,
    })
}
