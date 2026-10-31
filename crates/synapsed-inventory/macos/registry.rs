//! Small RAII wrappers around the typed objc2 IOKit/CoreFoundation bindings.

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_io_kit::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty,
    IORegistryEntryGetRegistryEntryID, IOServiceGetMatchingServices, IOServiceMatching,
    io_iterator_t, io_object_t, kIOMainPortDefault, kIOReturnSuccess,
};
use std::ffi::CStr;

struct OwnedIo(io_object_t);

impl Drop for OwnedIo {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = IOObjectRelease(self.0);
        }
    }
}

/// Visits services matching a typed IOKit class name and releases every handle.
pub(super) fn for_each_service(class: &CStr, mut visit: impl FnMut(io_object_t)) -> Result<(), ()> {
    let matching = unsafe { IOServiceMatching(class.as_ptr()) }.ok_or(())?;
    let matching: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(matching) };
    let mut iterator: io_iterator_t = 0;
    let result =
        unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, Some(matching), &mut iterator) };
    if result != kIOReturnSuccess {
        return Err(());
    }
    let iterator = OwnedIo(iterator);
    loop {
        let service = IOIteratorNext(iterator.0);
        if service == 0 {
            break;
        }
        let service = OwnedIo(service);
        visit(service.0);
    }
    Ok(())
}

/// Reads an IOKit registry string property as a typed CoreFoundation value.
pub(super) fn string(entry: io_object_t, key: &'static str) -> Option<String> {
    let key = CFString::from_static_str(key);
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, Some(&key), None, 0) }?;
    let value = value.downcast::<CFString>().ok()?;
    Some(value.to_string())
}

/// Reads an IOKit numeric property as a signed 64-bit integer.
pub(super) fn number(entry: io_object_t, key: &'static str) -> Option<i64> {
    let key = CFString::from_static_str(key);
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, Some(&key), None, 0) }?;
    let value = value.downcast::<CFNumber>().ok()?;
    value.as_i64()
}

/// Reads an IOKit Boolean property when represented by CFBoolean or CFNumber.
pub(super) fn boolean(entry: io_object_t, key: &'static str) -> Option<bool> {
    let key = CFString::from_static_str(key);
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, Some(&key), None, 0) }?;
    match value.downcast::<objc2_core_foundation::CFBoolean>() {
        Ok(value) => Some(value.as_bool()),
        Err(value) => value
            .downcast::<CFNumber>()
            .ok()?
            .as_i64()
            .map(|value| value != 0),
    }
}

/// Returns a private, process-stable IOKit registry key.
pub(super) fn registry_key(entry: io_object_t) -> Option<String> {
    let mut id = 0_u64;
    let result = unsafe { IORegistryEntryGetRegistryEntryID(entry, &mut id) };
    (result == kIOReturnSuccess).then(|| format!("ioreg-{id:016x}"))
}

/// Gets the immediate parent registry ID without retaining its native handle.
pub(super) fn parent_registry_id(entry: io_object_t) -> Option<u64> {
    let mut plane = [0_i8; 128];
    let plane_bytes = b"IOService\0";
    for (destination, source) in plane.iter_mut().zip(plane_bytes) {
        *destination = *source as i8;
    }
    let mut parent = 0;
    let result =
        unsafe { objc2_io_kit::IORegistryEntryGetParentEntry(entry, &mut plane, &mut parent) };
    if result != kIOReturnSuccess || parent == 0 {
        return None;
    }
    let parent = OwnedIo(parent);
    let mut id = 0_u64;
    let result = unsafe { IORegistryEntryGetRegistryEntryID(parent.0, &mut id) };
    (result == kIOReturnSuccess).then_some(id)
}
