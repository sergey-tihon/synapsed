//! Read-only Thunderbolt/USB4 sysfs collection.

use crate::model::{Bus, RawRecord, SourceCollection};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path},
};
use synapsed_api::{InventorySource, InventorySourceKind, InventorySourceState};

/// Collects Thunderbolt domain controllers and switch/peripheral records below a sysfs root.
pub fn collect(root: &Path) -> SourceCollection {
    let devices_path = root.join("bus/thunderbolt/devices");
    let entries = match fs::read_dir(&devices_path) {
        Ok(entries) => entries,
        Err(error) => return unavailable(error.kind() == std::io::ErrorKind::NotFound),
    };
    let entries: Vec<_> = entries.filter_map(Result::ok).collect();
    let mut domains = BTreeMap::<String, String>::new();
    for entry in &entries {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(domain_number) = name.strip_prefix("domain") else {
            continue;
        };
        if !domain_number.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let key = entry
            .path()
            .canonicalize()
            .ok()
            .and_then(|path| pci_ancestor(&path))
            .unwrap_or_else(|| format!("thunderbolt-domain-{domain_number}"));
        domains.insert(domain_number.to_owned(), key);
    }

    let mut records = Vec::new();
    let mut controller_keys: HashSet<String> = domains.values().cloned().collect();
    let mut controller_keys: Vec<String> = controller_keys.drain().collect();
    controller_keys.sort();
    for key in controller_keys {
        records.push(RawRecord::Controller {
            key,
            label: "Thunderbolt controller".to_owned(),
            bus: Bus::Thunderbolt,
        });
    }
    let mut devices: Vec<_> = entries
        .into_iter()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (name.contains('-') && !name.starts_with("domain")).then_some((name, entry.path()))
        })
        .collect();
    devices.sort_by(|left, right| left.0.cmp(&right.0));
    let names: HashSet<String> = devices.iter().map(|(name, _)| name.clone()).collect();
    for (name, path) in &devices {
        let Some((domain_number, _)) = name.split_once('-') else {
            continue;
        };
        let Some(domain_key) = domains.get(domain_number) else {
            continue;
        };
        let parent_key = thunderbolt_parent(name, domain_key, &names);
        let vendor_name = read_attr(path, "vendor_name").filter(|value| !value.is_empty());
        let device_name = read_attr(path, "device_name").filter(|value| !value.is_empty());
        let depth = read_attr(path, "depth")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_else(|| derived_depth(name));
        let upstream_usb4 = devices
            .iter()
            .find(|(candidate, _)| candidate == &parent_key)
            .is_some_and(|(_, parent_path)| parent_port_is_usb4(name, parent_path));
        let link_usb4 = depth >= 1 && (upstream_usb4 || usb4_link(path));
        records.push(RawRecord::ThunderboltDevice {
            key: name.clone(),
            parent_key: Some(parent_key),
            vendor_name,
            device_name,
            link_usb4,
        });
    }
    SourceCollection {
        records,
        status: InventorySource {
            kind: InventorySourceKind::Thunderbolt,
            state: InventorySourceState::Ok,
            message: None,
        },
    }
}

fn derived_depth(name: &str) -> u32 {
    let Some((_, ports)) = name.split_once('-') else {
        return 0;
    };
    if ports == "0" {
        0
    } else {
        1 + ports.bytes().filter(|byte| *byte == b'.').count() as u32
    }
}

fn unavailable(missing: bool) -> SourceCollection {
    SourceCollection {
        records: Vec::new(),
        status: InventorySource {
            kind: InventorySourceKind::Thunderbolt,
            state: if missing {
                InventorySourceState::Unavailable
            } else {
                InventorySourceState::Error
            },
            message: Some(
                if missing {
                    "Thunderbolt sysfs is unavailable."
                } else {
                    "Thunderbolt sysfs could not be read."
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

fn usb4_link(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry.file_name().to_string_lossy().starts_with("usb4_port")
            && read_attr(&entry.path(), "link")
                .is_some_and(|link| link.eq_ignore_ascii_case("usb4"))
    })
}

fn parent_port_is_usb4(device_name: &str, parent_path: &Path) -> bool {
    let Some((_, ports)) = device_name.split_once('-') else {
        return false;
    };
    let Some(port) = ports.rsplit('.').next() else {
        return false;
    };
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    read_attr(&parent_path.join(format!("usb4_port{port}")), "link")
        .is_some_and(|link| link.eq_ignore_ascii_case("usb4"))
}

fn thunderbolt_parent(name: &str, domain_key: &str, devices: &HashSet<String>) -> String {
    let Some((domain, ports)) = name.split_once('-') else {
        return domain_key.to_owned();
    };
    if ports == "0" {
        return domain_key.to_owned();
    }
    let parent = ports.rsplit_once('.').map_or_else(
        || format!("{domain}-0"),
        |(prefix, _)| format!("{domain}-{prefix}"),
    );
    if devices.contains(&parent) {
        parent
    } else {
        domain_key.to_owned()
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
    let (Some(domain), Some(bus), Some(device_function)) =
        (parts.next(), parts.next(), parts.next())
    else {
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
    fn collects_controller_switch_and_usb4_link() {
        let temp = tempdir().unwrap();
        let sys = temp.path();
        let bus = sys.join("bus/thunderbolt/devices");
        fs::create_dir_all(&bus).unwrap();
        let domain = sys.join("devices/pci0000:68/0000:68:00.5/thunderbolt/domain0");
        fs::create_dir_all(&domain).unwrap();
        symlink(&domain, bus.join("domain0")).unwrap();
        let dock = sys.join("devices/pci0000:68/0000:68:00.5/thunderbolt/0-2");
        fs::create_dir_all(dock.join("usb4_port0")).unwrap();
        fs::write(dock.join("vendor_name"), "CalDigit, Inc.").unwrap();
        fs::write(dock.join("device_name"), "TS4").unwrap();
        fs::write(dock.join("usb4_port0/link"), "usb4\n").unwrap();
        symlink(&dock, bus.join("0-2")).unwrap();
        let collected = collect(sys);
        assert_eq!(collected.status.state, InventorySourceState::Ok);
        assert!(collected.records.iter().any(|record| matches!(record,
            RawRecord::Controller { key, bus: Bus::Thunderbolt, .. } if key == "0000:68:00.5"
        )));
        assert!(collected.records.iter().any(|record| matches!(record,
            RawRecord::ThunderboltDevice { key, parent_key: Some(parent), vendor_name: Some(vendor), link_usb4: true, .. }
                if key == "0-2" && parent == "0000:68:00.5" && vendor == "CalDigit, Inc."
        )));
    }
}
