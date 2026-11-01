//! Private IOAV transport. Service-bound EDID is optional; no registry-wide identity joins.
use crate::{
    DdcBackend, DdcTransport, DriverError, ErrorCode, InventoryWithOwnedHandles, OwnedEndpoint,
    VcpValue, protocol,
};
use std::{
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
    ptr,
    rc::Rc,
    thread,
};

type CFRef = *const c_void;
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOAVServiceCreateWithService(allocator: CFRef, service: u32) -> CFRef;
    fn IOAVServiceReadI2C(
        service: CFRef,
        chip_address: u32,
        offset: u32,
        output: *mut u8,
        size: u32,
    ) -> i32;
    fn IOAVServiceWriteI2C(
        service: CFRef,
        chip_address: u32,
        data_address: u32,
        input: *mut u8,
        size: u32,
    ) -> i32;
    fn IORegistryGetRootEntry(main_port: u32) -> u32;
    fn IORegistryEntryCreateIterator(
        entry: u32,
        plane: *const c_char,
        options: u32,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFRef,
        allocator: CFRef,
        options: u32,
    ) -> CFRef;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
    fn IORegistryEntryGetName(entry: u32, name: *mut c_char) -> i32;
    fn IOObjectConformsTo(object: u32, class_name: *const c_char) -> u32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CFRef);
    fn CFGetTypeID(value: CFRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithCString(allocator: CFRef, string: *const c_char, encoding: u32) -> CFRef;
    fn CFStringCompare(first: CFRef, second: CFRef, flags: usize) -> isize;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetLength(data: CFRef) -> isize;
    fn CFDataGetBytePtr(data: CFRef) -> *const u8;
}
// Explicit owner-local RAII: CF/IOKit refs are never declared Send or Sync.
struct CFHandle {
    value: CFRef,
    _owner_only: PhantomData<Rc<()>>,
}
impl CFHandle {
    fn owned(value: CFRef) -> Option<Self> {
        if value.is_null() {
            None
        } else {
            Some(Self {
                value,
                _owner_only: PhantomData,
            })
        }
    }
}
impl Drop for CFHandle {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.value);
        }
    }
}
struct IOHandle(u32, PhantomData<Rc<()>>);
impl IOHandle {
    fn owned(value: u32) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value, PhantomData))
        }
    }
}
impl Drop for IOHandle {
    fn drop(&mut self) {
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

type CopyEdid = unsafe extern "C" fn(CFRef, *mut CFRef) -> i32;
/// macOS IOAV backend for scanning and operating external display endpoints.
pub struct MacosBackend {
    copy_edid: Option<CopyEdid>,
    location_key: CFHandle,
    external_value: CFHandle,
    _owner_only: PhantomData<Rc<()>>,
}
impl MacosBackend {
    /// Create a macOS backend and prepare its owner-local registry keys.
    pub fn new() -> Result<Self, DriverError> {
        // Symbol absence is expected on some OS versions and is not a capability failure.
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"IOAVServiceCopyEDID".as_ptr()) };
        let copy_edid = if symbol.is_null() {
            None
        } else {
            // dlsym resolves this exact optional C function signature; no symbol/pointer escapes.
            Some(unsafe { std::mem::transmute::<*mut c_void, CopyEdid>(symbol) })
        };
        let location_key = CFHandle::owned(unsafe {
            CFStringCreateWithCString(ptr::null(), c"Location".as_ptr(), 0x08000100)
        })
        .ok_or_else(|| protocol::ddc_error("Could not allocate display registry key."))?;
        let external_value = CFHandle::owned(unsafe {
            CFStringCreateWithCString(ptr::null(), c"External".as_ptr(), 0x08000100)
        })
        .ok_or_else(|| protocol::ddc_error("Could not allocate display registry value."))?;
        Ok(Self {
            copy_edid,
            location_key,
            external_value,
            _owner_only: PhantomData,
        })
    }
    fn identity(&self, service: CFRef) -> Option<protocol::EdidIdentity> {
        let copy = self.copy_edid?;
        let mut data = ptr::null();
        let result = unsafe { copy(service, &mut data) };
        // Copy is owned even when a failing implementation returns a non-null output.
        let data = CFHandle::owned(data)?;
        if result != 0
            || unsafe {
                CFGetTypeID(data.value) != CFDataGetTypeID() || CFDataGetLength(data.value) < 128
            }
        {
            return None;
        }
        let bytes = unsafe { CFDataGetBytePtr(data.value) };
        if bytes.is_null() {
            return None;
        }
        // Hash only the base block; never decode/export serial fields, extensions or raw dumps.
        protocol::parse_base_edid(unsafe { std::slice::from_raw_parts(bytes, 128) }).ok()
    }
}
fn native_error(code: i32) -> DriverError {
    if matches!(code as u32, 0xe00002c1 | 0xe00002e2) {
        DriverError::new(
            ErrorCode::PermissionDenied,
            "Access to an external display service was denied.",
        )
    } else {
        protocol::ddc_error("Native display DDC transaction failed.")
    }
}
fn placement(service: u32) -> Option<crate::Placement> {
    let mut parent = 0;
    let result =
        unsafe { IORegistryEntryGetParentEntry(service, c"IOService".as_ptr(), &mut parent) };
    if result != 0 {
        return None;
    }
    let parent = IOHandle::owned(parent)?;
    if unsafe { IOObjectConformsTo(parent.0, c"IOFramebuffer".as_ptr()) } == 0 {
        return None;
    }
    let mut name = [0 as c_char; 128];
    if unsafe { IORegistryEntryGetName(parent.0, name.as_mut_ptr()) } != 0 {
        return None;
    }
    let connector = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_owned();
    if connector.is_empty() {
        return None;
    }
    let mut registry_id = 0;
    if unsafe { IORegistryEntryGetRegistryEntryID(parent.0, &mut registry_id) } != 0 {
        return None;
    }
    Some(crate::Placement {
        connector,
        controller_key: format!("macos-framebuffer-{registry_id}"),
    })
}
impl DdcBackend for MacosBackend {
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, DriverError> {
        let root = IOHandle::owned(unsafe { IORegistryGetRootEntry(0) }).ok_or_else(|| {
            DriverError::new(
                ErrorCode::DeviceUnavailable,
                "Display registry is unavailable.",
            )
        })?;
        let mut iterator = 0;
        let result = unsafe {
            IORegistryEntryCreateIterator(root.0, c"IOService".as_ptr(), 1, &mut iterator)
        };
        let iterator = IOHandle::owned(iterator);
        if result != 0 {
            return Err(native_error(result));
        }
        let iterator = iterator.ok_or_else(|| {
            DriverError::new(
                ErrorCode::DeviceUnavailable,
                "Display registry iterator is unavailable.",
            )
        })?;
        let mut endpoints = Vec::new();
        let mut errors = Vec::new();
        while let Some(entry) = IOHandle::owned(unsafe { IOIteratorNext(iterator.0) }) {
            let Some(location) = CFHandle::owned(unsafe {
                IORegistryEntryCreateCFProperty(entry.0, self.location_key.value, ptr::null(), 1)
            }) else {
                continue;
            };
            if unsafe {
                CFGetTypeID(location.value) != CFStringGetTypeID()
                    || CFStringCompare(location.value, self.external_value.value, 0) != 0
            } {
                continue;
            }
            let mut registry_id = 0u64;
            let result = unsafe { IORegistryEntryGetRegistryEntryID(entry.0, &mut registry_id) };
            if result != 0 {
                errors.push(native_error(result));
                continue;
            }
            // External registry nodes are not necessarily IOAV/DDC endpoints.
            // This constructor has no error result; null supplies no evidence of
            // an eligible denied monitor. Preserve explicit registry/I2C errors.
            let Some(service) =
                CFHandle::owned(unsafe { IOAVServiceCreateWithService(ptr::null(), entry.0) })
            else {
                continue;
            };
            let identity = self.identity(service.value);
            endpoints.push(OwnedEndpoint {
                // Registry ID is only a boot-local internal key, not physical monitor identity.
                key: registry_id.to_string(),
                manufacturer: identity
                    .as_ref()
                    .and_then(|identity| identity.manufacturer.clone()),
                product: identity.as_ref().map(|identity| identity.product),
                product_name: identity
                    .as_ref()
                    .and_then(|identity| identity.product_name.clone()),
                identity: identity.map(|identity| identity.identity),
                placement: placement(entry.0),
                transport: Box::new(MacosTransport { service }),
            });
        }
        Ok(InventoryWithOwnedHandles { endpoints, errors })
    }
}
struct MacosTransport {
    service: CFHandle,
}
impl MacosTransport {
    fn write(&mut self, frame: &mut [u8]) -> Result<(), DriverError> {
        let result = unsafe {
            IOAVServiceWriteI2C(
                self.service.value,
                0x37,
                0x51,
                frame.as_mut_ptr(),
                u32::try_from(frame.len()).unwrap_or(u32::MAX),
            )
        };
        if result != 0 {
            return Err(native_error(result));
        }
        Ok(())
    }
}
impl DdcTransport for MacosTransport {
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, DriverError> {
        self.write(&mut protocol::macos_get_frame(code))?;
        thread::sleep(protocol::IO_DELAY);
        let mut reply = [0u8; protocol::REPLY_BYTES];
        let result = unsafe {
            IOAVServiceReadI2C(
                self.service.value,
                0x37,
                0,
                reply.as_mut_ptr(),
                u32::try_from(reply.len()).unwrap_or(u32::MAX),
            )
        };
        if result != 0 {
            return Err(native_error(result));
        }
        protocol::parse_reply(code, &reply)
    }
    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DriverError> {
        self.write(&mut protocol::macos_set_frame(code, value))?;
        thread::sleep(protocol::IO_DELAY);
        Ok(())
    }
}
