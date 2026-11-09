//! macOS system layer.
//!
//! Everything here goes through libproc, sysctl or Mach FFI. The tool never
//! spawns a child process (no `ioreg`, `ps`, `lsof`, `launchctl` or `top`).

pub mod discover;
pub mod gpu;
pub mod info;
pub mod proc;
pub mod socket;
pub mod vm;

use std::ffi::CStr;
use std::fmt;
use std::ptr;

/// A typed system-layer failure. Callers either degrade to `None` or surface
/// the error in a warning; nothing in this module panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SysError {
    /// A libc/sysctl call failed with `errno`.
    Errno(&'static str, i32),
    /// A Mach routine returned a non-zero `kern_return_t`.
    Mach(&'static str, i32),
    /// A call succeeded but returned an unexpected shape.
    Invalid(&'static str, &'static str),
}

impl SysError {
    pub fn last_os_error(call: &'static str) -> Self {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        SysError::Errno(call, code)
    }

    pub fn is_permission_denied(&self) -> bool {
        matches!(
            self,
            SysError::Errno(_, libc::EPERM) | SysError::Errno(_, libc::EACCES)
        )
    }
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SysError::Errno(call, code) => {
                write!(
                    f,
                    "{call} failed: {}",
                    std::io::Error::from_raw_os_error(*code)
                )
            }
            SysError::Mach(call, code) => write!(f, "{call} failed with kern_return_t {code}"),
            SysError::Invalid(call, what) => write!(f, "{call} returned {what}"),
        }
    }
}

impl std::error::Error for SysError {}

extern "C" {
    fn sysctlbyname(
        name: *const libc::c_char,
        oldp: *mut libc::c_void,
        oldlenp: *mut usize,
        newp: *mut libc::c_void,
        newlen: usize,
    ) -> libc::c_int;
}

/// Read a `sysctlbyname` value into `buffer`, returning the number of bytes
/// written.
pub fn sysctl_bytes(name: &CStr, buffer: &mut [u8]) -> Result<usize, SysError> {
    let mut len = buffer.len();
    let result = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(SysError::last_os_error("sysctlbyname"));
    }
    Ok(len)
}

/// Read an integer sysctl that may be reported as 32 or 64 bits.
pub fn sysctl_u64(name: &CStr) -> Result<u64, SysError> {
    let mut buffer = [0u8; 8];
    let len = sysctl_bytes(name, &mut buffer)?;
    match len {
        4 => Ok(u32::from_ne_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as u64),
        8 => Ok(u64::from_ne_bytes(buffer)),
        _ => Err(SysError::Invalid(
            "sysctlbyname",
            "an unexpected integer width",
        )),
    }
}

pub fn sysctl_i32(name: &CStr) -> Result<i32, SysError> {
    let mut buffer = [0u8; 4];
    let len = sysctl_bytes(name, &mut buffer)?;
    if len != 4 {
        return Err(SysError::Invalid(
            "sysctlbyname",
            "an unexpected integer width",
        ));
    }
    Ok(i32::from_ne_bytes(buffer))
}

pub fn sysctl_string(name: &CStr) -> Result<String, SysError> {
    let mut buffer = [0u8; 256];
    let len = sysctl_bytes(name, &mut buffer)?;
    let end = buffer[..len]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(len);
    Ok(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// View a plain-old-data struct as its raw bytes. Only used for fixed-layout C
/// structs returned by sysctl.
pub fn struct_as_bytes_mut<T>(value: &mut T) -> &mut [u8] {
    // SAFETY: `T` is a `#[repr(C)]` POD struct and the slice covers exactly the
    // object's memory; the borrow lives only for the duration of the call.
    unsafe { std::slice::from_raw_parts_mut(value as *mut T as *mut u8, std::mem::size_of::<T>()) }
}
