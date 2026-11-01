//! Read-only USB sysfs collection.

use crate::model::{Bus, RawRecord, SourceCollection};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};
use synapsed_api::{InventorySource, InventorySourceKind, InventorySourceState};

/// Collects USB host controllers and non-interface device entries below a sysfs root.
pub fn collect(root: &Path) -> SourceCollection {
    let devices_path = root.join("bus/usb/devices");
    let entries = match fs::read_dir(&devices_path) {
        Ok(entries) => entries,
        Err(error) => return unavailable(error.kind() == std::io::ErrorKind::NotFound),
    };
    let mut paths: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.contains(':') || name.starts_with("usb") {
                return None;
            }
            Some((name, entry.path()))
        })
        .collect();
    paths.sort_by(|left, right| left.0.cmp(&right.0));

    let mut controllers = BTreeMap::<String, String>::new();
    if let Ok(entries) = fs::read_dir(&devices_path) {
        for entry in entries.filter_map(Result::ok) {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Some(bus_number) = name.strip_prefix("usb") else {
                continue;
            };
            if bus_number.is_empty() || !bus_number.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let controller = entry
                .path()
                .canonicalize()
                .ok()
                .and_then(|path| pci_ancestor(&path))
                .unwrap_or_else(|| format!("usb-controller-{bus_number}"));
            controllers.insert(bus_number.to_owned(), controller);
        }
    }

    let mut records = Vec::new();
    let mut controller_keys: HashSet<String> = controllers.values().cloned().collect();
    let mut keys: Vec<String> = controller_keys.drain().collect();
    keys.sort();
    for key in keys {
        records.push(RawRecord::Controller {
            key,
            label: "USB host controller".to_owned(),
            bus: Bus::Usb,
        });
    }

    let device_names: HashSet<String> = paths.iter().map(|(name, _)| name.clone()).collect();
    for (name, path) in paths {
        let bus_number = name.split_once('-').map(|(bus, _)| bus).unwrap_or_default();
        let Some(controller_key) = controllers.get(bus_number) else {
            continue;
        };
        let parent_key = usb_parent_key(&name, controller_key, &device_names);
        let vendor_id = read_attr(&path, "idVendor").and_then(parse_hex_id);
        let product_id = read_attr(&path, "idProduct").and_then(parse_hex_id);
        let manufacturer = read_attr(&path, "manufacturer").filter(|value| !value.is_empty());
        let product = read_attr(&path, "product").filter(|value| !value.is_empty());
        let speed_mbps = read_attr(&path, "speed")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0);
        let class = read_attr(&path, "bDeviceClass").and_then(|value| parse_class(&value));
        let removable = read_attr(&path, "removable").and_then(|value| match value.as_str() {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        });
        records.push(RawRecord::UsbDevice {
            key: name,
            parent_key: Some(parent_key),
            vendor_id,
            product_id,
            manufacturer,
            product,
            speed_mbps,
            class,
            removable,
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

fn unavailable(missing: bool) -> SourceCollection {
    SourceCollection {
        records: Vec::new(),
        status: InventorySource {
            kind: InventorySourceKind::Usb,
            state: if missing {
                InventorySourceState::Unavailable
            } else {
                InventorySourceState::Error
            },
            message: Some(
                if missing {
                    "USB sysfs is unavailable."
                } else {
                    "USB sysfs could not be read."
                }
                .to_owned(),
            ),
        },
    }
}

fn read_attr(path: &Path, name: &str) -> Option<String> {
    fs::read_to_string(path.join(name))
        .ok()
        .map(|value| value.trim().to_owned())
}

fn parse_hex_id(value: String) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

fn parse_class(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches("0x").to_ascii_lowercase();
    (value.len() <= 2 && !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("{value:0>2}"))
}

fn usb_parent_key(name: &str, controller_key: &str, devices: &HashSet<String>) -> String {
    let Some((bus, ports)) = name.split_once('-') else {
        return controller_key.to_owned();
    };
    let Some((parent_ports, _)) = ports.rsplit_once('.') else {
        return controller_key.to_owned();
    };
    let parent = format!("{bus}-{parent_ports}");
    if devices.contains(&parent) {
        parent
    } else {
        controller_key.to_owned()
    }
}

fn pci_ancestor(path: &Path) -> Option<String> {
    path.ancestors()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .find(|component| is_pci_address(component))
        .map(str::to_owned)
}

fn is_pci_address(value: &str) -> bool {
    let mut parts = value.split(':');
    let Some(domain) = parts.next() else {
        return false;
    };
    let Some(bus) = parts.next() else {
        return false;
    };
    let Some(device_function) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || domain.len() != 4 || bus.len() != 2 {
        return false;
    }
    let Some((device, function)) = device_function.split_once('.') else {
        return false;
    };
    device.len() == 2
        && function.len() == 1
        && domain
            .bytes()
            .chain(bus.bytes())
            .chain(device.bytes())
            .chain(function.bytes())
            .all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    use tempfile::tempdir;

    #[test]
    fn collects_controller_device_parents_and_skips_interfaces() {
        let temp = tempdir().unwrap();
        let sys = temp.path();
        let devices = sys.join("bus/usb/devices");
        fs::create_dir_all(&devices).unwrap();
        let host = sys.join("devices/pci0000:68/0000:68:00.3/usb5");
        fs::create_dir_all(&host).unwrap();
        symlink(&host, devices.join("usb5")).unwrap();
        for (name, attrs) in [
            (
                "5-1",
                vec![
                    ("idVendor", "2188"),
                    ("idProduct", "0450"),
                    ("manufacturer", "CalDigit, Inc."),
                    ("product", "USB2 Hub"),
                    ("speed", "480"),
                    ("bDeviceClass", "09"),
                    ("removable", "0"),
                ],
            ),
            (
                "5-1.5",
                vec![
                    ("idVendor", "0bda"),
                    ("idProduct", "5411"),
                    ("manufacturer", "Realtek"),
                    ("product", "Hub"),
                    ("speed", "480"),
                    ("bDeviceClass", "09"),
                    ("removable", "0"),
                ],
            ),
            (
                "5-1.5:1.0",
                vec![("idVendor", "0bda"), ("idProduct", "5411")],
            ),
        ] {
            let device = sys.join(format!("devices/{name}"));
            fs::create_dir_all(&device).unwrap();
            for (key, value) in attrs {
                fs::write(device.join(key), value).unwrap();
            }
            symlink(&device, devices.join(name)).unwrap();
        }
        let collected = collect(sys);
        assert_eq!(collected.status.state, InventorySourceState::Ok);
        let controller = collected
            .records
            .iter()
            .find_map(|record| match record {
                RawRecord::Controller { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(controller, "0000:68:00.3");
        let root = collected
            .records
            .iter()
            .find_map(|record| match record {
                RawRecord::UsbDevice {
                    key,
                    parent_key,
                    class,
                    ..
                } if key == "5-1" => Some((parent_key.as_deref(), class.as_deref())),
                _ => None,
            })
            .unwrap();
        assert_eq!(root, (Some(controller), Some("09")));
        let hub = collected
            .records
            .iter()
            .find_map(|record| match record {
                RawRecord::UsbDevice {
                    key, parent_key, ..
                } if key == "5-1.5" => Some(parent_key.as_deref()),
                _ => None,
            })
            .unwrap();
        assert_eq!(hub, Some("5-1"));
        assert!(
            !collected.records.iter().any(
                |record| matches!(record, RawRecord::UsbDevice { key, .. } if key.contains(':'))
            )
        );
    }

    #[test]
    fn missing_sysfs_is_reported_without_leaking_a_path() {
        let temp = tempdir().unwrap();
        let result = collect(temp.path());
        assert_eq!(result.status.state, InventorySourceState::Unavailable);
        assert!(
            !result
                .status
                .message
                .unwrap()
                .contains(temp.path().to_str().unwrap())
        );
    }
}
