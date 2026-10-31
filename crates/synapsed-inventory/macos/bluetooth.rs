//! Passive IOBluetooth paired-device collection with read-only CoreBluetooth authorization status.

use crate::model::{RawRecord, SourceCollection};
use objc2::runtime::AnyObject;
use objc2_core_bluetooth::{CBManager, CBManagerAuthorization};
use objc2_io_bluetooth::{BluetoothHCIPowerState, IOBluetoothDevice, IOBluetoothHostController};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use synapsed_api::{InventorySource, InventorySourceKind, InventorySourceState};

const ADAPTER_KEY: &str = "macos-default-bluetooth";

#[derive(Clone)]
struct PairedDevice {
    name: String,
    class: String,
    paired: bool,
    connected: bool,
}

/// Reads the Bluetooth authorization state and paired-device list without starting discovery.
pub fn collect() -> SourceCollection {
    let authorization = unsafe { CBManager::authorization_class() };
    if bluetooth_access_denied(authorization) {
        return SourceCollection {
            records: Vec::new(),
            status: InventorySource {
                kind: InventorySourceKind::Bluetooth,
                state: InventorySourceState::PermissionDenied,
                message: Some(
                    "Bluetooth access is denied or restricted by macOS privacy settings."
                        .to_owned(),
                ),
            },
        };
    }

    let controller = unsafe { IOBluetoothHostController::defaultController() };
    let name = controller
        .as_ref()
        .and_then(|controller| unsafe { controller.nameAsString() })
        .map(|name| name.to_string());
    let powered = controller
        .as_ref()
        .is_some_and(|controller| unsafe { controller.powerState() } == BluetoothHCIPowerState::ON);
    let mut records = vec![RawRecord::BluetoothAdapter {
        key: ADAPTER_KEY.to_owned(),
        controller_key: None,
        name,
        powered,
    }];

    // IOBluetooth documents a nil paired-device list as empty, not as denied access.
    let mut paired_devices = Vec::new();
    if let Some(devices) = unsafe { IOBluetoothDevice::pairedDevices() } {
        for index in 0..devices.count() {
            let object: objc2::rc::Retained<AnyObject> = devices.objectAtIndex(index);
            let Ok(device) = object.downcast::<IOBluetoothDevice>() else {
                continue;
            };
            paired_devices.push(PairedDevice {
                name: unsafe { device.name() }.to_string(),
                class: format!("class-{:?}", unsafe { device.deviceClassMajor() }),
                paired: unsafe { device.isPaired() },
                connected: unsafe { device.isConnected() },
            });
        }
    }
    records.extend(paired_device_records(paired_devices));
    SourceCollection {
        records,
        status: InventorySource {
            kind: InventorySourceKind::Bluetooth,
            state: InventorySourceState::Ok,
            message: None,
        },
    }
}

fn bluetooth_access_denied(authorization: CBManagerAuthorization) -> bool {
    authorization == CBManagerAuthorization::Denied
        || authorization == CBManagerAuthorization::Restricted
}

fn paired_device_records(mut devices: Vec<PairedDevice>) -> Vec<RawRecord> {
    // Stable sorting preserves OS order within a name/class group. Without reading the
    // forbidden address, indistinguishable devices cannot be tracked across list reorders.
    devices.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.class.cmp(&right.class))
    });
    let mut ordinals = HashMap::<String, usize>::new();
    devices
        .into_iter()
        .map(|device| {
            let fingerprint = stable_device_fingerprint(&device.name, &device.class);
            let ordinal = ordinals.entry(fingerprint.clone()).or_default();
            let key = stable_device_key(&fingerprint, *ordinal);
            *ordinal += 1;
            RawRecord::BluetoothDevice {
                key,
                adapter_key: ADAPTER_KEY.to_owned(),
                name: Some(device.name),
                icon: Some(device.class),
                paired: device.paired,
                connected: device.connected,
            }
        })
        .collect()
}

fn stable_device_fingerprint(name: &str, class: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(name.as_bytes());
    hash.update([0]);
    hash.update(class.as_bytes());
    let digest = hash.finalize();
    let mut fingerprint = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(fingerprint, "{byte:02x}");
    }
    fingerprint
}

fn stable_device_key(fingerprint: &str, duplicate_ordinal: usize) -> String {
    format!("paired-device-{fingerprint}-{duplicate_ordinal}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paired(name: &str, class: &str, connected: bool) -> PairedDevice {
        PairedDevice {
            name: name.to_owned(),
            class: class.to_owned(),
            paired: true,
            connected,
        }
    }

    fn keys(devices: Vec<PairedDevice>) -> Vec<String> {
        let mut keys: Vec<String> = paired_device_records(devices)
            .into_iter()
            .filter_map(|record| match record {
                RawRecord::BluetoothDevice { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn duplicate_name_and_class_get_distinct_stable_keys() {
        let keys = keys(vec![
            paired("Keyboard", "class-HID", true),
            paired("Keyboard", "class-HID", true),
        ]);
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn duplicate_key_set_survives_reordering_and_unrelated_removal() {
        let devices = vec![
            paired("Keyboard", "class-HID", true),
            paired("Headphones", "class-Audio", false),
            paired("Keyboard", "class-HID", true),
            paired("Gamepad", "class-Peripheral", true),
        ];
        let forward = keys(devices.clone());
        let mut reversed = devices;
        reversed.reverse();
        assert_eq!(forward, keys(reversed));

        let keyboard_prefix = format!(
            "paired-device-{}-",
            stable_device_fingerprint("Keyboard", "class-HID")
        );
        let keyboard_keys: Vec<String> = forward
            .into_iter()
            .filter(|key| key.starts_with(&keyboard_prefix))
            .collect();
        assert_eq!(
            keyboard_keys,
            keys(vec![
                paired("Keyboard", "class-HID", true),
                paired("Keyboard", "class-HID", true),
            ])
        );
    }

    #[test]
    fn only_explicit_denied_or_restricted_authorization_is_permission_denied() {
        assert!(bluetooth_access_denied(CBManagerAuthorization::Denied));
        assert!(bluetooth_access_denied(CBManagerAuthorization::Restricted));
        assert!(!bluetooth_access_denied(
            CBManagerAuthorization::NotDetermined
        ));
        assert!(!bluetooth_access_denied(
            CBManagerAuthorization::AllowedAlways
        ));
    }
}
