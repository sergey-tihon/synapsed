//! Passive BlueZ ObjectManager snapshot collection; no discovery is started.

use crate::model::{RawRecord, SourceCollection};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};
use synapsed_api::{InventorySource, InventorySourceKind, InventorySourceState};
use zbus::{
    blocking::{connection::Builder, fdo::ObjectManagerProxy},
    fdo::ManagedObjects,
    zvariant::{OwnedValue, Value},
};

const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
const DEVICE_INTERFACE: &str = "org.bluez.Device1";

/// Reads the existing BlueZ object tree with a two-second D-Bus method timeout.
pub fn collect(sysfs_root: &Path) -> SourceCollection {
    let connection = match Builder::system()
        .and_then(|builder| builder.method_timeout(Duration::from_secs(2)).build())
    {
        Ok(connection) => connection,
        Err(error) => return source_error(&error.to_string()),
    };
    let proxy = match ObjectManagerProxy::new(&connection, "org.bluez", "/") {
        Ok(proxy) => proxy,
        Err(error) => return source_error(&error.to_string()),
    };
    match proxy.get_managed_objects() {
        Ok(objects) => SourceCollection {
            records: parse_managed_objects(&objects, sysfs_root),
            status: InventorySource {
                kind: InventorySourceKind::Bluetooth,
                state: InventorySourceState::Ok,
                message: None,
            },
        },
        Err(error) => source_error(&error.to_string()),
    }
}

fn source_error(error: &str) -> SourceCollection {
    let lower = error.to_ascii_lowercase();
    let (state, message) = if lower.contains("accessdenied") || lower.contains("permission denied")
    {
        (
            InventorySourceState::PermissionDenied,
            "Access to BlueZ was denied.",
        )
    } else if lower.contains("timeout") || lower.contains("timed out") {
        (
            InventorySourceState::Timeout,
            "The BlueZ object tree did not respond before the deadline.",
        )
    } else if lower.contains("serviceunknown")
        || lower.contains("namehasnoowner")
        || lower.contains("no such service")
    {
        (
            InventorySourceState::Unavailable,
            "The BlueZ service is unavailable.",
        )
    } else {
        (
            InventorySourceState::Error,
            "The BlueZ object tree could not be read.",
        )
    };
    failed(state, message)
}

/// Parses a captured ObjectManager snapshot into adapter/device records.
pub fn parse_managed_objects(objects: &ManagedObjects, sysfs_root: &Path) -> Vec<RawRecord> {
    let mut records = Vec::new();
    let mut adapter_keys = HashSet::<String>::new();
    let mut adapters: Vec<_> = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let properties = interface(interfaces, ADAPTER_INTERFACE)?;
            let key = path.as_str().to_owned();
            let name = string_property(properties, "Name");
            let powered = bool_property(properties, "Powered").unwrap_or(false);
            let parent_key = usb_parent_for_adapter(sysfs_root, &key);
            Some((key, name, powered, parent_key))
        })
        .collect();
    adapters.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, name, powered, parent_key) in adapters {
        adapter_keys.insert(key.clone());
        records.push(RawRecord::BluetoothAdapter {
            key,
            controller_key: parent_key,
            name,
            powered,
        });
    }

    let mut devices: Vec<_> = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let properties = interface(interfaces, DEVICE_INTERFACE)?;
            let key = path.as_str().to_owned();
            let adapter_key = object_path_property(properties, "Adapter")?;
            Some((key, adapter_key, properties))
        })
        .collect();
    devices.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, adapter_key, properties) in devices {
        if !adapter_keys.contains(&adapter_key) {
            continue;
        }
        records.push(RawRecord::BluetoothDevice {
            key,
            adapter_key,
            name: string_property(properties, "Alias"),
            icon: string_property(properties, "Icon"),
            paired: bool_property(properties, "Paired").unwrap_or(false),
            connected: bool_property(properties, "Connected").unwrap_or(false),
        });
    }
    records
}

fn interface<'a>(
    interfaces: &'a HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedValue>>,
    name: &str,
) -> Option<&'a HashMap<String, OwnedValue>> {
    interfaces
        .iter()
        .find_map(|(interface, properties)| (interface.as_str() == name).then_some(properties))
}

fn string_property(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    let value = properties.get(name)?;
    match &**value {
        Value::Str(value) => Some(value.to_string()),
        _ => None,
    }
}

fn object_path_property(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    let value = properties.get(name)?;
    match &**value {
        Value::ObjectPath(value) => Some(value.to_string()),
        _ => None,
    }
}

fn bool_property(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    let value = properties.get(name)?;
    match &**value {
        Value::Bool(value) => Some(*value),
        _ => None,
    }
}

fn usb_parent_for_adapter(sysfs_root: &Path, object_path: &str) -> Option<String> {
    let adapter = object_path.rsplit('/').next()?;
    if !adapter.starts_with("hci") {
        return None;
    }
    let device = sysfs_root
        .join("class/bluetooth")
        .join(adapter)
        .join("device")
        .canonicalize()
        .ok()?;
    device
        .ancestors()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .find(|component| is_usb_port_key(component))
        .map(str::to_owned)
}

fn is_usb_port_key(value: &str) -> bool {
    let Some((bus, ports)) = value.split_once('-') else {
        return false;
    };
    !bus.is_empty()
        && bus.bytes().all(|byte| byte.is_ascii_digit())
        && !ports.is_empty()
        && ports
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

fn failed(state: InventorySourceState, message: &'static str) -> SourceCollection {
    SourceCollection {
        records: Vec::new(),
        status: InventorySource {
            kind: InventorySourceKind::Bluetooth,
            state,
            message: Some(message.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    use tempfile::tempdir;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};

    fn object_path(value: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(value.to_owned()).unwrap()
    }

    fn object_path_value(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(object_path(value))).unwrap()
    }

    fn properties(entries: Vec<(&str, OwnedValue)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    #[test]
    fn parses_captured_managed_objects_without_exposing_bluez_paths_or_addresses() {
        let temp = tempdir().unwrap();
        let sys = temp.path();
        let usb = sys.join("devices/pci0000:66/0000:66:00.3/usb1/1-5/1-5:1.0/bluetooth/hci0");
        fs::create_dir_all(&usb).unwrap();
        let sys_adapter = sys.join("class/bluetooth/hci0");
        fs::create_dir_all(&sys_adapter).unwrap();
        symlink(&usb, sys_adapter.join("device")).unwrap();

        let mut objects = ManagedObjects::new();
        let mut adapter = HashMap::new();
        adapter.insert(
            zbus::names::OwnedInterfaceName::try_from(ADAPTER_INTERFACE).unwrap(),
            properties(vec![
                (
                    "Name",
                    OwnedValue::from(Str::from_static("Intel Bluetooth")),
                ),
                ("Powered", OwnedValue::from(true)),
            ]),
        );
        objects.insert(object_path("/org/bluez/hci0"), adapter);
        let mut device = HashMap::new();
        device.insert(
            zbus::names::OwnedInterfaceName::try_from(DEVICE_INTERFACE).unwrap(),
            properties(vec![
                ("Adapter", object_path_value("/org/bluez/hci0")),
                (
                    "Alias",
                    OwnedValue::from(Str::from_static("Keyboard 01:23:45:67:89:AB")),
                ),
                ("Icon", OwnedValue::from(Str::from_static("input-keyboard"))),
                ("Paired", OwnedValue::from(true)),
                ("Connected", OwnedValue::from(false)),
            ]),
        );
        objects.insert(object_path("/org/bluez/hci0/dev_01_23_45_67_89_AB"), device);

        let records = parse_managed_objects(&objects, sys);
        let adapter = records
            .iter()
            .find_map(|record| match record {
                RawRecord::BluetoothAdapter {
                    controller_key,
                    name,
                    powered,
                    ..
                } => Some((controller_key.as_deref(), name.as_deref(), *powered)),
                _ => None,
            })
            .unwrap();
        assert_eq!(adapter, (Some("1-5"), Some("Intel Bluetooth"), true));
        let bluetooth_device = records
            .iter()
            .find_map(|record| match record {
                RawRecord::BluetoothDevice {
                    name,
                    icon,
                    paired,
                    connected,
                    ..
                } => Some((name.as_deref(), icon.as_deref(), *paired, *connected)),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            bluetooth_device,
            (
                Some("Keyboard 01:23:45:67:89:AB"),
                Some("input-keyboard"),
                true,
                false
            )
        );
        let response = crate::topology::build(
            &records,
            &std::collections::BTreeMap::new(),
            &[],
            b"inventory-test-salt",
        );
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("01:23:45:67:89:AB"));
        assert!(!json.contains("/org/bluez"));
        assert!(!json.contains("/sys/devices"));
        assert!(!json.contains("1-5"));
    }
}
