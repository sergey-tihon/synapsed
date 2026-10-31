//! Typed IOKit Thunderbolt switch collection.

use super::registry;
use crate::model::{Bus, RawRecord, SourceCollection};
use objc2_io_kit::io_object_t;
use std::collections::{BTreeSet, HashSet};
use synapsed_api::{InventorySource, InventorySourceKind, InventorySourceState};

struct SwitchRecord {
    registry_id: u64,
    parent_id: Option<u64>,
    depth: i64,
    vendor_name: Option<String>,
    device_name: Option<String>,
    link_usb4: bool,
}

/// Collects typed `IOThunderboltSwitch` service names, depth and registry parent relationships.
pub fn collect() -> SourceCollection {
    let mut switches = Vec::new();
    let result = registry::for_each_service(c"IOThunderboltSwitch", |service: io_object_t| {
        let Some(key) = registry::registry_key(service) else {
            return;
        };
        let Some(registry_id) = parse_registry_key(&key) else {
            return;
        };
        let depth = registry::number(service, "Depth").unwrap_or(0);
        let link_type = registry::string(service, "IOThunderboltLinkType")
            .or_else(|| registry::string(service, "Link Type"))
            .or_else(|| registry::string(service, "Link"));
        let usb4_property = registry::boolean(service, "USB4");
        let link_usb4 = has_usb4_link(depth, link_type.as_deref(), usb4_property);
        switches.push(SwitchRecord {
            registry_id,
            parent_id: registry::parent_registry_id(service),
            depth,
            vendor_name: registry::string(service, "Device Vendor Name"),
            device_name: registry::string(service, "Device Model Name"),
            link_usb4,
        });
    });
    if result.is_err() {
        return unavailable();
    }
    switches.sort_by_key(|switch| (switch.depth, switch.registry_id));

    let switch_ids: HashSet<u64> = switches.iter().map(|switch| switch.registry_id).collect();
    let mut controller_keys: BTreeSet<String> = switches
        .iter()
        .filter_map(|switch| {
            switch
                .parent_id
                .filter(|parent| !switch_ids.contains(parent))
                .map(controller_key)
        })
        .collect();
    if !switches.is_empty() && controller_keys.is_empty() {
        controller_keys.insert("thunderbolt-controller-unresolved".to_owned());
    }
    let mut records: Vec<RawRecord> = controller_keys
        .into_iter()
        .map(|key| RawRecord::Controller {
            key,
            label: "Thunderbolt controller".to_owned(),
            bus: Bus::Thunderbolt,
        })
        .collect();
    for switch in switches {
        let key = registry_key(switch.registry_id);
        let parent_key = switch
            .parent_id
            .map(|parent| {
                if switch_ids.contains(&parent) {
                    registry_key(parent)
                } else {
                    controller_key(parent)
                }
            })
            .or_else(|| {
                records.iter().find_map(|record| match record {
                    RawRecord::Controller {
                        key,
                        bus: Bus::Thunderbolt,
                        ..
                    } => Some(key.clone()),
                    _ => None,
                })
            });
        records.push(RawRecord::ThunderboltDevice {
            key,
            parent_key,
            vendor_name: switch.vendor_name,
            device_name: switch.device_name,
            link_usb4: switch.link_usb4,
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

fn unavailable() -> SourceCollection {
    SourceCollection {
        records: Vec::new(),
        status: InventorySource {
            kind: InventorySourceKind::Thunderbolt,
            state: InventorySourceState::Unavailable,
            message: Some("IOKit Thunderbolt services are unavailable.".to_owned()),
        },
    }
}

fn parse_registry_key(key: &str) -> Option<u64> {
    u64::from_str_radix(key.strip_prefix("ioreg-")?, 16).ok()
}

fn registry_key(id: u64) -> String {
    format!("ioreg-{id:016x}")
}

fn controller_key(id: u64) -> String {
    format!("thunderbolt-controller-{id:016x}")
}

fn has_usb4_link(depth: i64, link_type: Option<&str>, usb4_property: Option<bool>) -> bool {
    depth >= 1
        && (link_type.is_some_and(|value| value.to_ascii_lowercase().contains("usb4"))
            || usb4_property == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb4_classification_requires_affirmative_link_evidence() {
        assert!(!has_usb4_link(1, None, None));
        assert!(!has_usb4_link(1, Some("Thunderbolt"), None));
        assert!(has_usb4_link(1, Some("USB4"), None));
        assert!(has_usb4_link(1, None, Some(true)));
        assert!(!has_usb4_link(0, None, Some(true)));
    }
}
