//! GPU sampling through IOKit, without spawning `ioreg`.
//!
//! `IOAccelerator.PerformanceStatistics` is an undocumented but long-stable
//! CoreFoundation dictionary (SPECS.md sections 2.4 and 5.5). The values are
//! whole-device figures sampled by the AGX driver; they are never attributed to
//! oMLX.
//!
//! # Object ownership
//!
//! CoreFoundation/IOKit use manual reference counting:
//!
//! * `IOServiceMatching` returns a +1 dictionary that
//!   `IOServiceGetMatchingServices` consumes, so it is never released here.
//! * the iterator and every object returned by `IOIteratorNext` are +1 and are
//!   released by the [`IoObject`] guard;
//! * `IORegistryEntryCreateCFProperty` returns a +1 value, released by the
//!   [`OwnedCf`] guard;
//! * `CFStringCreateWithCString` returns a +1 string, also released by
//!   [`OwnedCf`];
//! * values returned by `CFDictionaryGetValue` are borrowed and must not be
//!   released, so they are read and copied out immediately.
//!
//! Every failure path returns `None`: a missing property, a changed type or a
//! permission error degrades the GPU panel, never the process.

use std::ffi::{c_char, c_void, CStr};
use std::ptr;

use crate::model::GpuBlock;

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT64_TYPE: i32 = 4;
const KERN_SUCCESS: libc::c_int = 0;
/// `kIOMainPortDefault` is 0 in the modern SDK (formerly `kIOMasterPortDefault`).
const IO_MAIN_PORT_DEFAULT: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        existing: *mut u32,
    ) -> libc::c_int;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> libc::c_int;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: *const c_void,
        allocator: *const c_void,
        options: u32,
    ) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        string: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, number_type: i32, value: *mut c_void) -> u8;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFRelease(value: *const c_void);
}

/// A +1 CoreFoundation object that is released on drop.
struct OwnedCf(*const c_void);

impl OwnedCf {
    fn new(value: *const c_void) -> Option<Self> {
        (!value.is_null()).then_some(OwnedCf(value))
    }

    fn as_ptr(&self) -> *const c_void {
        self.0
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: the pointer was returned with a +1 reference and is released
        // exactly once here.
        unsafe { CFRelease(self.0) };
    }
}

/// A +1 IOKit object (iterator or registry entry), released on drop.
struct IoObject(u32);

impl Drop for IoObject {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the object was returned with a +1 reference and is
            // released exactly once here.
            unsafe {
                IOObjectRelease(self.0);
            }
        }
    }
}

/// Read the first `IOAccelerator` service that exposes a usable
/// `PerformanceStatistics` dictionary.
pub fn sample() -> Option<GpuBlock> {
    // SAFETY: `IOServiceMatching` copies the C string; the returned dictionary
    // is consumed by `IOServiceGetMatchingServices` as documented above.
    unsafe {
        let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
        if matching.is_null() {
            return None;
        }

        let mut iterator: u32 = 0;
        let result = IOServiceGetMatchingServices(IO_MAIN_PORT_DEFAULT, matching, &mut iterator);
        // `matching` is consumed even when the call fails.
        if result != KERN_SUCCESS || iterator == 0 {
            return None;
        }
        let iterator_guard = IoObject(iterator);

        loop {
            let entry = IOIteratorNext(iterator_guard.0);
            if entry == 0 {
                // The iterator is exhausted; no accelerator had a property.
                return None;
            }
            let entry_guard = IoObject(entry);
            if let Some(block) = read_entry(entry_guard.0) {
                return Some(block);
            }
        }
    }
}

fn read_entry(entry: u32) -> Option<GpuBlock> {
    // SAFETY: `entry` is a live registry object; `CFStringCreateWithCString`
    // copies the key and returns a +1 object.
    let key = OwnedCf::new(unsafe {
        CFStringCreateWithCString(
            ptr::null(),
            c"PerformanceStatistics".as_ptr(),
            CF_STRING_ENCODING_UTF8,
        )
    })?;

    // SAFETY: `entry` and `key` are valid for the duration of the call. The
    // property is returned with a +1 reference.
    let property = OwnedCf::new(unsafe {
        IORegistryEntryCreateCFProperty(entry, key.as_ptr(), ptr::null(), 0)
    })?;

    // SAFETY: `property` is a live CoreFoundation object. Comparing type ids
    // does not take ownership.
    let is_dictionary = unsafe { CFGetTypeID(property.as_ptr()) == CFDictionaryGetTypeID() };
    if !is_dictionary {
        return None;
    }

    let dictionary = property.as_ptr();
    let block = GpuBlock {
        device_util_percent: read_percent(dictionary, c"Device Utilization %"),
        renderer_util_percent: read_percent(dictionary, c"Renderer Utilization %"),
        tiler_util_percent: read_percent(dictionary, c"Tiler Utilization %"),
        in_use_bytes: read_bytes(dictionary, c"In use system memory"),
        alloc_bytes: read_bytes(dictionary, c"Alloc system memory"),
    };

    // A dictionary with none of the expected keys is not a usable sample.
    if block == GpuBlock::default() {
        None
    } else {
        Some(block)
    }
}

fn read_percent(dictionary: *const c_void, key: &CStr) -> Option<u32> {
    percent_from_raw(read_i64(dictionary, key)?)
}

fn read_bytes(dictionary: *const c_void, key: &CStr) -> Option<u64> {
    bytes_from_raw(read_i64(dictionary, key)?)
}

/// Percent values are unsigned; a negative number cannot be displayed and is
/// treated as unavailable rather than wrapped.
fn percent_from_raw(value: i64) -> Option<u32> {
    u32::try_from(value).ok()
}

/// Byte values are unsigned for the same reason.
fn bytes_from_raw(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

/// Copy one 64-bit integer out of the dictionary. The borrowed value is not
/// retained or released.
fn read_i64(dictionary: *const c_void, name: &CStr) -> Option<i64> {
    // SAFETY: `CFStringCreateWithCString` copies the C string and returns a +1
    // object owned by the guard.
    let key = OwnedCf::new(unsafe {
        CFStringCreateWithCString(ptr::null(), name.as_ptr(), CF_STRING_ENCODING_UTF8)
    })?;

    // SAFETY: both objects are valid; the returned value is borrowed from the
    // dictionary and remains valid while `dictionary` lives.
    let value = unsafe { CFDictionaryGetValue(dictionary, key.as_ptr()) };
    if value.is_null() {
        return None;
    }

    // SAFETY: `value` is a live CoreFoundation object.
    if unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }

    let mut number: i64 = 0;
    // SAFETY: the value is a CFNumber and the output buffer is a valid i64.
    let converted = unsafe {
        CFNumberGetValue(
            value,
            CF_NUMBER_SINT64_TYPE,
            &mut number as *mut i64 as *mut c_void,
        )
    };
    (converted != 0).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_numbers_are_unavailable_not_wrapped() {
        assert_eq!(percent_from_raw(-1), None);
        assert_eq!(bytes_from_raw(-1), None);
        assert_eq!(percent_from_raw(17), Some(17));
        assert_eq!(bytes_from_raw(1_176_158_208), Some(1_176_158_208));
    }
}
