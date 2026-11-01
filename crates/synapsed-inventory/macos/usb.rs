//! Typed IOKit USB host-device collection.

use super::registry;
use crate::model::{Bus, RawRecord, SourceCollection};
use objc2_io_kit::io_object_t;
use std::collections::{BTreeSet, HashSet};
use synapsed_api::{InventorySource, InventorySourceKind, InventorySourceState};

struct DeviceRecord {
    location_id: u32,
    vendor_id: Option<String>,
    product_id: Option<String>,
    manufacturer: Option<String>,
    product: Option<String>,
    class: Option<String>,
    removable: Option<bool>,
}

/// Collects registered `IOUSBHostDevice` services using objc2 IOKit/CoreFoundation bindings.
pub fn collect() -> SourceCollection {
    let mut devices = Vec::new();
    let result = registry::for_each_service(c"IOUSBHostDevice", |service: io_object_t| {
        let Some(location_id) =
            registry::number(service, "locationID").and_then(|value| u32::try_from(value).ok())
        else {
            return;
        };
        let vendor_id = registry::number(service, "idVendor").and_then(hex_u16);
        let product_id = registry::number(service, "idProduct").and_then(hex_u16);
        let manufacturer =
            registry::string(service, "USB Vendor Name").filter(|value| !value.trim().is_empty());
        let product =
            registry::string(service, "USB Product Name").filter(|value| !value.trim().is_empty());
        let class = registry::number(service, "bDeviceClass")
            .and_then(|value| u8::try_from(value).ok())
            .map(|value| format!("{value:02x}"));
        let removable = registry::boolean(service, "Removable");
        devices.push(DeviceRecord {
            location_id,
            vendor_id,
            product_id,
            manufacturer,
            product,
            class,
            removable,
        });
    });
    if result.is_err() {
        return unavailable();
    }

    devices.sort_by_key(|device| device.location_id);
    let locations: HashSet<u32> = devices.iter().map(|device| device.location_id).collect();
    let controller_keys: BTreeSet<String> = devices
        .iter()
        .map(|device| controller_key(device.location_id))
        .collect();
    let mut records: Vec<RawRecord> = controller_keys
        .into_iter()
        .map(|key| RawRecord::Controller {
            key,
            label: "USB host controller".to_owned(),
            bus: Bus::Usb,
        })
        .collect();
    for device in devices {
        let parent_key = parent_location(device.location_id)
            .filter(|parent| locations.contains(parent))
            .map(location_key)
            .unwrap_or_else(|| controller_key(device.location_id));
        records.push(RawRecord::UsbDevice {
            key: location_key(device.location_id),
            parent_key: Some(parent_key),
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            manufacturer: device.manufacturer,
            product: device.product,
            speed_mbps: None,
            class: device.class,
            removable: device.removable,
        });
    }
    SourceCollection {
        records,
        status: InventorySource {
            kind: InventorySourceKind::Usb,
            state: InventorySourceState::Ok,
            message: None,
        },
    }
}

fn unavailable() -> SourceCollection {
    SourceCollection {
        records: Vec::new(),
        status: InventorySource {
            kind: InventorySourceKind::Usb,
            state: InventorySourceState::Unavailable,
            message: Some("IOKit USB services are unavailable.".to_owned()),
        },
    }
}

fn hex_u16(value: i64) -> Option<String> {
    u16::try_from(value)
        .ok()
        .map(|value| format!("{value:04x}"))
}

fn controller_key(location_id: u32) -> String {
    format!("usb-controller-{:02x}", location_id >> 24)
}

fn location_key(location_id: u32) -> String {
    format!("usb-location-{location_id:08x}")
}

fn parent_location(location_id: u32) -> Option<u32> {
    (0..24).step_by(4).find_map(|shift| {
        (((location_id >> shift) & 0x0f) != 0).then_some(location_id & !(0x0f << shift))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_id_nibbles_build_a_hub_chain() {
        let child = 0x1423_0000;
        assert_eq!(parent_location(child), Some(0x1420_0000));
        assert_eq!(parent_location(0x1400_0000), None);
        assert_eq!(controller_key(child), controller_key(0x1400_0000));
    }
}
