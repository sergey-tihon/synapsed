//! Deterministic topology construction and public-record sanitization.

use crate::model::{
    Bus, DeviceBinding, DriverCatalogEntry, InventoryMatch, InventoryRole, RawRecord,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use synapsed_api::MAX_INVENTORY_NODES;
use synapsed_api::{
    API_VERSION, InventoryAttachment, InventoryDriver, InventoryDriverClass, InventoryNode,
    InventoryNodeKind, InventoryRelation, InventoryResponse, InventoryRule, InventorySource,
};

const MAX_LABEL_CHARS: usize = 128;
const MAX_DETAIL_CHARS: usize = 256;

/// Builds a sanitized public inventory from native records and published driver bindings.
///
/// `salt` is a stable per-install secret (or the daemon instance UUID); it is used only as
/// input to opaque node IDs and is never returned. Source states are supplied separately by
/// collectors through [`build_with_sources`].
pub fn build(
    records: &[RawRecord],
    bindings: &BTreeMap<String, DeviceBinding>,
    driver_matches: &[DriverCatalogEntry],
    salt: &[u8],
) -> InventoryResponse {
    build_with_sources(records, bindings, driver_matches, salt, Vec::new())
}

/// Builds an inventory response while retaining independently collected source states.
pub fn build_with_sources(
    records: &[RawRecord],
    bindings: &BTreeMap<String, DeviceBinding>,
    driver_matches: &[DriverCatalogEntry],
    salt: &[u8],
    sources: Vec<InventorySource>,
) -> InventoryResponse {
    let mut sorted: Vec<&RawRecord> = records.iter().collect();
    sorted.sort_by(|left, right| record_key(left).cmp(record_key(right)));

    sorted.truncate(MAX_INVENTORY_NODES.saturating_sub(1));
    let mut nodes = vec![NodeBuild::host(salt)];
    let mut by_key = HashMap::<String, usize>::new();
    let mut endpoint_to_node = HashMap::<String, usize>::new();
    for record in sorted {
        let key = record_key(record).to_owned();
        if by_key.contains_key(&key) {
            continue;
        }
        let node = NodeBuild::from_record(record, salt);
        let index = nodes.len();
        if let RawRecord::Display { endpoint_token, .. } = record {
            endpoint_to_node.insert(endpoint_token.clone(), index);
        }
        by_key.insert(key, index);
        nodes.push(node);
    }

    connect_exact_edges(records, &mut nodes, &by_key);
    attach_drivers(
        records,
        bindings,
        driver_matches,
        &mut nodes,
        &by_key,
        &endpoint_to_node,
    );
    infer_docks(records, &mut nodes, &by_key);
    infer_monitor_companions(
        records,
        bindings,
        driver_matches,
        &mut nodes,
        &by_key,
        &endpoint_to_node,
    );
    propagate_attachment(&mut nodes);

    let sources = sources
        .into_iter()
        .map(|mut source| {
            source.message = source.message.as_deref().map(safe_detail);
            source
        })
        .collect();
    let public_nodes = nodes.into_iter().map(NodeBuild::into_public).collect();
    InventoryResponse {
        api_version: API_VERSION,
        instance_id: instance_uuid(salt),
        generated_at: timestamp(SystemTime::now()),
        sources,
        nodes: public_nodes,
    }
}

struct NodeBuild {
    key: String,
    bus: Option<Bus>,
    node: InventoryNode,
}

impl NodeBuild {
    fn host(salt: &[u8]) -> Self {
        Self {
            key: "host".to_owned(),
            bus: None,
            node: InventoryNode {
                id: opaque_id(salt, "host", "host"),
                parent_id: None,
                kind: InventoryNodeKind::Host,
                label: "This machine".to_owned(),
                detail: None,
                vendor_id: None,
                product_id: None,
                speed_mbps: None,
                connected: true,
                attachment: InventoryAttachment::Host,
                relation: InventoryRelation::Exact,
                rule: None,
                driver: None,
            },
        }
    }

    fn from_record(record: &RawRecord, salt: &[u8]) -> Self {
        let key = record_key(record).to_owned();
        let bus = match record {
            RawRecord::Controller { bus, .. } => Some(*bus),
            _ => None,
        };
        let (kind, label, detail, vendor_id, product_id, speed_mbps, connected) = match record {
            RawRecord::Controller { label, .. } => (
                InventoryNodeKind::Controller,
                safe_label(label, "Controller"),
                None,
                None,
                None,
                None,
                true,
            ),
            RawRecord::UsbDevice {
                vendor_id,
                product_id,
                manufacturer,
                product,
                speed_mbps,
                ..
            } => {
                let hub = record_is_hub(record);
                let label = joined_label(
                    manufacturer.as_deref(),
                    product.as_deref(),
                    if hub { "USB hub" } else { "USB device" },
                );
                let detail = usb_detail(vendor_id.as_deref(), product_id.as_deref(), *speed_mbps);
                (
                    if hub {
                        InventoryNodeKind::Hub
                    } else {
                        InventoryNodeKind::UsbDevice
                    },
                    label,
                    detail,
                    normalized_usb_id(vendor_id.as_deref()),
                    normalized_usb_id(product_id.as_deref()),
                    speed_mbps.filter(|value| value.is_finite() && *value > 0.0),
                    true,
                )
            }
            RawRecord::ThunderboltDevice {
                vendor_name,
                device_name,
                ..
            } => (
                if matches!(
                    record,
                    RawRecord::ThunderboltDevice {
                        link_usb4: true,
                        ..
                    }
                ) {
                    InventoryNodeKind::Dock
                } else {
                    InventoryNodeKind::Controller
                },
                joined_label(
                    vendor_name.as_deref(),
                    device_name.as_deref(),
                    "Thunderbolt device",
                ),
                None,
                None,
                None,
                None,
                true,
            ),
            RawRecord::Display {
                connector,
                manufacturer,
                product_name,
                ..
            } => (
                InventoryNodeKind::Display,
                joined_label(manufacturer.as_deref(), product_name.as_deref(), "Display"),
                Some(safe_detail(connector)),
                None,
                None,
                None,
                true,
            ),
            RawRecord::BluetoothAdapter { name, powered, .. } => (
                InventoryNodeKind::BluetoothAdapter,
                safe_label(
                    name.as_deref().unwrap_or("Bluetooth adapter"),
                    "Bluetooth adapter",
                ),
                None,
                None,
                None,
                None,
                *powered,
            ),
            RawRecord::BluetoothDevice {
                name,
                icon,
                connected,
                ..
            } => (
                InventoryNodeKind::BluetoothDevice,
                safe_label(
                    name.as_deref().unwrap_or("Bluetooth device"),
                    "Bluetooth device",
                ),
                icon.as_deref().map(safe_detail),
                None,
                None,
                None,
                *connected,
            ),
        };
        Self {
            node: InventoryNode {
                id: opaque_id(salt, kind_name(kind), &key),
                parent_id: None,
                kind,
                label,
                detail,
                vendor_id,
                product_id,
                speed_mbps,
                connected,
                attachment: InventoryAttachment::Host,
                relation: InventoryRelation::Exact,
                rule: None,
                driver: None,
            },
            key,
            bus,
        }
    }

    fn into_public(self) -> InventoryNode {
        self.node
    }
}

fn connect_exact_edges(
    records: &[RawRecord],
    nodes: &mut [NodeBuild],
    by_key: &HashMap<String, usize>,
) {
    let host_id = nodes[0].node.id.clone();
    for record in records {
        let Some(index) = by_key.get(record_key(record)).copied() else {
            continue;
        };
        let parent_key = match record {
            RawRecord::Controller { .. } => None,
            RawRecord::UsbDevice { parent_key, .. } => parent_key.clone(),
            RawRecord::ThunderboltDevice {
                parent_key,
                link_usb4: true,
                ..
            } => parent_key
                .as_deref()
                .and_then(|parent| thunderbolt_domain_key(parent, records))
                .or_else(|| parent_key.clone()),
            RawRecord::ThunderboltDevice { parent_key, .. } => parent_key.clone(),
            RawRecord::Display { controller_key, .. } => Some(controller_key.clone()),
            RawRecord::BluetoothAdapter { controller_key, .. } => controller_key.clone(),
            RawRecord::BluetoothDevice { adapter_key, .. } => Some(adapter_key.clone()),
        };
        let parent_index = parent_key
            .as_deref()
            .and_then(|key| by_key.get(key).copied())
            .filter(|parent| valid_parent(record, nodes[*parent].node.kind))
            .unwrap_or(0);

        nodes[index].node.parent_id = if parent_index == 0 {
            Some(host_id.clone())
        } else {
            Some(nodes[parent_index].node.id.clone())
        };
    }
    for node in nodes.iter_mut().skip(1) {
        if node.node.parent_id.is_none() {
            node.node.parent_id = Some(host_id.clone());
        }
    }
}

fn thunderbolt_domain_key(start: &str, records: &[RawRecord]) -> Option<String> {
    let mut current = start.to_owned();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        let record = records
            .iter()
            .find(|record| record_key(record) == current.as_str())?;
        match record {
            RawRecord::Controller {
                key,
                bus: Bus::Thunderbolt,
                ..
            } => return Some(key.clone()),
            RawRecord::ThunderboltDevice {
                parent_key: Some(parent),
                ..
            } => current.clone_from(parent),
            _ => return None,
        }
    }
}

fn valid_parent(record: &RawRecord, parent_kind: InventoryNodeKind) -> bool {
    match record {
        RawRecord::UsbDevice { .. } => matches!(
            parent_kind,
            InventoryNodeKind::Controller | InventoryNodeKind::Hub | InventoryNodeKind::UsbDevice
        ),
        RawRecord::ThunderboltDevice { .. } => matches!(
            parent_kind,
            InventoryNodeKind::Controller | InventoryNodeKind::Dock
        ),
        RawRecord::Display { .. } => parent_kind == InventoryNodeKind::Controller,
        RawRecord::BluetoothAdapter { .. } => matches!(
            parent_kind,
            InventoryNodeKind::Hub | InventoryNodeKind::UsbDevice
        ),
        RawRecord::BluetoothDevice { .. } => parent_kind == InventoryNodeKind::BluetoothAdapter,
        RawRecord::Controller { .. } => false,
    }
}

fn attach_drivers(
    records: &[RawRecord],
    bindings: &BTreeMap<String, DeviceBinding>,
    catalog: &[DriverCatalogEntry],
    nodes: &mut [NodeBuild],
    by_key: &HashMap<String, usize>,
    endpoint_to_node: &HashMap<String, usize>,
) {
    for (endpoint, node_index) in endpoint_to_node {
        let matches: Vec<(&str, &DeviceBinding)> = bindings
            .iter()
            .filter(|(_, binding)| binding.endpoints.iter().any(|token| token == endpoint))
            .map(|(device_id, binding)| (device_id.as_str(), binding))
            .collect();
        if matches.len() != 1 {
            continue;
        }
        let (device_id, binding) = matches[0];
        let Some(driver) = catalog.iter().find(|driver| driver.id == binding.driver) else {
            continue;
        };
        nodes[*node_index].node.driver = Some(driver_badge(driver, Some(device_id), false));
    }

    for record in records {
        let matches: Vec<(&DriverCatalogEntry, bool)> = catalog
            .iter()
            .filter_map(|driver| match_record(record, driver).then_some((driver, true)))
            .collect();
        if matches.len() != 1 {
            continue;
        }
        let Some(index) = by_key.get(record_key(record)).copied() else {
            continue;
        };
        let (driver, declared_match) = matches[0];
        let specialized = declared_match || !driver.supersedes.is_empty();
        nodes[index].node.driver = Some(driver_badge(driver, None, specialized));
    }
}

fn match_record(record: &RawRecord, driver: &DriverCatalogEntry) -> bool {
    match record {
        RawRecord::UsbDevice { vendor_id: Some(vendor), product_id: Some(product), .. } => driver.inventory_matches.iter().any(|rule| matches!(
            rule,
            InventoryMatch::Usb { vendor_id, product_id, .. } if vendor.eq_ignore_ascii_case(vendor_id) && product.eq_ignore_ascii_case(product_id)
        )),
        RawRecord::BluetoothDevice { name: Some(name), .. } => driver.inventory_matches.iter().any(|rule| matches!(
            rule,
            InventoryMatch::Bluetooth { name_prefix, .. } if name.get(..name_prefix.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(name_prefix))
        )),
        _ => false,
    }
}

fn driver_badge(
    driver: &DriverCatalogEntry,
    device_id: Option<&str>,
    declared_match: bool,
) -> InventoryDriver {
    InventoryDriver {
        id: driver.id.clone(),
        version: driver.version.clone(),
        class: if declared_match || !driver.supersedes.is_empty() {
            InventoryDriverClass::Specialized
        } else {
            InventoryDriverClass::Generic
        },
        device_id: device_id.map(str::to_owned),
    }
}

fn infer_docks(records: &[RawRecord], nodes: &mut [NodeBuild], by_key: &HashMap<String, usize>) {
    let mut candidates: Vec<usize> = records
        .iter()
        .filter_map(|record| match record {
            RawRecord::ThunderboltDevice {
                key,
                link_usb4: true,
                ..
            } => by_key.get(key).copied(),
            _ => None,
        })
        .filter(|index| domain_controller(*index, nodes).is_some())
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    let mut by_domain = BTreeMap::<usize, Vec<usize>>::new();
    for dock in &candidates {
        if let Some(domain) = domain_controller(*dock, nodes) {
            by_domain.entry(domain).or_default().push(*dock);
        }
    }

    let mut assignment_claims = HashMap::<usize, Vec<(usize, InventoryRule)>>::new();
    for (domain, docks) in by_domain {
        let domain_key = nodes[domain].key.clone();
        let pci = pci_bus_device(&domain_key);
        let usb_controllers: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                if index != 0
                    && node.bus == Some(Bus::Usb)
                    && pci_bus_device(&node.key) == pci
                    && pci.is_some()
                {
                    Some(index)
                } else {
                    None
                }
            })
            .collect();
        if docks.len() == 1 && usb_controllers.len() == 1 {
            let dock = docks[0];
            let controller = usb_controllers[0];
            let root_children: Vec<usize> = nodes
                .iter()
                .enumerate()
                .filter_map(|(index, node)| {
                    (index != 0
                        && node.node.parent_id.as_deref()
                            == Some(nodes[controller].node.id.as_str()))
                    .then_some(index)
                })
                .collect();
            for root in root_children {
                assignment_claims
                    .entry(root)
                    .or_default()
                    .push((dock, InventoryRule::Usb4Tunnel));
            }
        } else if docks.len() > 1 || usb_controllers.len() > 1 {
            let roots: Vec<usize> = nodes
                .iter()
                .enumerate()
                .filter_map(|(index, node)| {
                    usb_controllers
                        .iter()
                        .any(|controller| {
                            node.node.parent_id.as_deref()
                                == Some(nodes[*controller].node.id.as_str())
                        })
                        .then_some(index)
                })
                .collect();
            let mut claims = HashMap::<usize, Vec<usize>>::new();
            for root in roots {
                let manufacturer = records
                    .iter()
                    .find_map(|record| match record {
                        RawRecord::UsbDevice {
                            key, manufacturer, ..
                        } if key == &nodes[root].key => manufacturer.as_deref(),
                        _ => None,
                    })
                    .unwrap_or_default();
                let normalized_manufacturer = normalize_name(manufacturer);
                if normalized_manufacturer.is_empty() {
                    continue;
                }
                for dock in &docks {
                    let vendor = records
                        .iter()
                        .find_map(|record| match record {
                            RawRecord::ThunderboltDevice {
                                key, vendor_name, ..
                            } if key == &nodes[*dock].key => vendor_name.as_deref(),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let normalized_vendor = normalize_name(vendor);
                    if !normalized_vendor.is_empty()
                        && normalized_manufacturer.starts_with(&normalized_vendor)
                    {
                        claims.entry(root).or_default().push(*dock);
                    }
                }
            }
            for (root, claimed_docks) in claims {
                let mut unique: Vec<usize> = claimed_docks.into_iter().collect();
                unique.sort_unstable();
                unique.dedup();
                if unique.len() == 1 {
                    assignment_claims
                        .entry(root)
                        .or_default()
                        .push((unique[0], InventoryRule::VendorMatch));
                }
            }
        }
    }

    for (root, claims) in assignment_claims {
        let mut docks: Vec<usize> = claims.iter().map(|(dock, _)| *dock).collect();
        docks.sort_unstable();
        docks.dedup();
        if docks.len() != 1 {
            continue;
        }
        let dock = docks[0];
        let rule = if claims
            .iter()
            .any(|(claimed_dock, rule)| *claimed_dock == dock && *rule == InventoryRule::Usb4Tunnel)
        {
            InventoryRule::Usb4Tunnel
        } else {
            InventoryRule::VendorMatch
        };
        nodes[root].node.parent_id = Some(nodes[dock].node.id.clone());
        nodes[root].node.relation = InventoryRelation::Inferred;
        nodes[root].node.rule = Some(rule);
    }
}

fn infer_monitor_companions(
    records: &[RawRecord],
    bindings: &BTreeMap<String, DeviceBinding>,
    catalog: &[DriverCatalogEntry],
    nodes: &mut [NodeBuild],
    by_key: &HashMap<String, usize>,
    endpoint_to_node: &HashMap<String, usize>,
) {
    let mut candidates = Vec::<(&DriverCatalogEntry, &str, usize, usize)>::new();
    for driver in catalog {
        let monitor_rules: Vec<&InventoryMatch> = driver
            .inventory_matches
            .iter()
            .filter(|rule| {
                matches!(
                    rule,
                    InventoryMatch::Usb {
                        role: InventoryRole::MonitorCompanion,
                        ..
                    }
                )
            })
            .collect();
        if monitor_rules.is_empty() {
            continue;
        }
        for (endpoint, display) in endpoint_to_node {
            let mut matching_bindings = bindings
                .iter()
                .filter(|(_, binding)| binding.endpoints.iter().any(|token| token == endpoint));
            let Some((device_id, binding)) = matching_bindings.next() else {
                continue;
            };
            if matching_bindings.next().is_some() || binding.driver != driver.id {
                continue;
            }
            for record in records {
                let RawRecord::UsbDevice {
                    vendor_id: Some(vendor),
                    product_id: Some(product),
                    ..
                } = record
                else {
                    continue;
                };
                let matches_monitor_rule = monitor_rules.iter().any(|rule| matches!(
                    (vendor.as_str(), product.as_str(), rule),
                    (vendor, product, InventoryMatch::Usb { vendor_id, product_id, .. })
                        if vendor.eq_ignore_ascii_case(vendor_id) && product.eq_ignore_ascii_case(product_id)
                ));
                if matches_monitor_rule && let Some(index) = by_key.get(record_key(record)) {
                    candidates.push((driver, device_id.as_str(), *display, *index));
                }
            }
        }
    }
    if candidates.len() != 1 {
        return;
    }
    let (driver, device_id, display, matched) = candidates[0];
    let parent = nodes.iter().position(|node| {
        node.node.id == nodes[matched].node.parent_id.as_deref().unwrap_or_default()
    });
    let anchor = parent
        .filter(|parent| nodes[*parent].node.kind == InventoryNodeKind::Hub)
        .unwrap_or(matched);
    nodes[anchor].node.parent_id = Some(nodes[display].node.id.clone());
    nodes[anchor].node.relation = InventoryRelation::Inferred;
    nodes[anchor].node.rule = Some(InventoryRule::DriverLink);
    nodes[matched].node.driver = Some(driver_badge(driver, Some(device_id), true));
}

fn propagate_attachment(nodes: &mut [NodeBuild]) {
    let parents: Vec<Option<usize>> = {
        let id_to_index: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.node.id.as_str(), index))
            .collect();
        nodes
            .iter()
            .map(|node| {
                node.node
                    .parent_id
                    .as_deref()
                    .and_then(|parent| id_to_index.get(parent).copied())
            })
            .collect()
    };
    for _ in 0..nodes.len() {
        let mut changed = false;
        for index in 0..nodes.len() {
            let attachment = if nodes[index].node.kind == InventoryNodeKind::Dock {
                InventoryAttachment::Dock
            } else if nodes[index].node.relation == InventoryRelation::Inferred
                && nodes[index].node.rule == Some(InventoryRule::DriverLink)
            {
                InventoryAttachment::Monitor
            } else if let Some(parent) = parents[index] {
                nodes[parent].node.attachment
            } else {
                InventoryAttachment::Host
            };
            if nodes[index].node.attachment != attachment {
                nodes[index].node.attachment = attachment;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn domain_controller(mut index: usize, nodes: &[NodeBuild]) -> Option<usize> {
    let id_to_index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.node.id.as_str(), index))
        .collect();
    loop {
        if nodes[index].bus == Some(Bus::Thunderbolt) {
            return Some(index);
        }
        let parent = nodes[index].node.parent_id.as_deref()?;
        index = *id_to_index.get(parent)?;
        if index == 0 {
            return None;
        }
    }
}

fn pci_bus_device(value: &str) -> Option<String> {
    let mut parts = value.split(':');
    let domain = parts.next()?;
    let bus = parts.next()?;
    let device_function = parts.next()?;
    if parts.next().is_some() || !device_function.contains('.') {
        return None;
    }
    let device = device_function.split('.').next()?;
    Some(format!("{domain}:{bus}:{device}"))
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn record_key(record: &RawRecord) -> &str {
    match record {
        RawRecord::Controller { key, .. }
        | RawRecord::UsbDevice { key, .. }
        | RawRecord::ThunderboltDevice { key, .. }
        | RawRecord::BluetoothAdapter { key, .. }
        | RawRecord::BluetoothDevice { key, .. } => key,
        RawRecord::Display { endpoint_token, .. } => endpoint_token,
    }
}

fn record_is_hub(record: &RawRecord) -> bool {
    match record {
        RawRecord::UsbDevice {
            class: Some(class), ..
        } => u8::from_str_radix(class.trim_start_matches("0x"), 16).is_ok_and(|value| value == 9),
        _ => false,
    }
}

fn normalized_usb_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim().to_ascii_lowercase();
    (value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

fn usb_detail(vendor: Option<&str>, product: Option<&str>, speed: Option<f64>) -> Option<String> {
    let mut parts = Vec::with_capacity(2);
    if let (Some(vendor), Some(product)) = (normalized_usb_id(vendor), normalized_usb_id(product)) {
        parts.push(format!("{vendor}:{product}"));
    }
    if let Some(speed) = speed.filter(|value| value.is_finite() && *value > 0.0) {
        parts.push(format!("{} Mb/s", format_number(speed)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn joined_label(first: Option<&str>, second: Option<&str>, fallback: &str) -> String {
    let first = first.map(|value| safe_label(value, "")).unwrap_or_default();
    let second = second
        .map(|value| safe_label(value, ""))
        .unwrap_or_default();
    match (first.is_empty(), second.is_empty()) {
        (false, false) if first.eq_ignore_ascii_case(&second) => first,
        (false, false) => safe_label(&format!("{first} {second}"), fallback),
        (false, true) => first,
        (true, false) => second,
        (true, true) => fallback.to_owned(),
    }
}

fn safe_label(value: &str, fallback: &str) -> String {
    let mut clean = redact_sensitive(value);
    clean.retain(|character| !character.is_control());
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = clean.chars().take(MAX_LABEL_CHARS).collect();
    if truncated.is_empty() {
        fallback.to_owned()
    } else {
        truncated
    }
}

fn safe_detail(value: &str) -> String {
    redact_sensitive(value)
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_DETAIL_CHARS)
        .collect()
}

fn redact_sensitive(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if is_mac_address(bytes, index) {
            output.push_str("[redacted]");
            index += 17;
        } else if value[index..].starts_with("/dev/") || value[index..].starts_with("/sys/") {
            output.push_str("[redacted]");
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
        } else {
            let character = value[index..].chars().next().unwrap_or_default();
            output.push(character);
            index += character.len_utf8();
        }
    }
    output
}

fn is_mac_address(bytes: &[u8], start: usize) -> bool {
    if start + 17 > bytes.len() {
        return false;
    }
    for offset in 0..17 {
        let byte = bytes[start + offset];
        if offset % 3 == 2 {
            if byte != b':' && byte != b'-' && byte != b'_' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn opaque_id(salt: &[u8], kind: &str, key: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(salt);
    hash.update(kind.as_bytes());
    hash.update(key.as_bytes());
    let digest = hash.finalize();
    let mut id = String::with_capacity(17);
    id.push('n');
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}

fn kind_name(kind: InventoryNodeKind) -> &'static str {
    match kind {
        InventoryNodeKind::Host => "host",
        InventoryNodeKind::Controller => "controller",
        InventoryNodeKind::Dock => "dock",
        InventoryNodeKind::Hub => "hub",
        InventoryNodeKind::UsbDevice => "usb_device",
        InventoryNodeKind::Display => "display",
        InventoryNodeKind::BluetoothAdapter => "bluetooth_adapter",
        InventoryNodeKind::BluetoothDevice => "bluetooth_device",
    }
}

fn instance_uuid(salt: &[u8]) -> String {
    let text = std::str::from_utf8(salt).unwrap_or_default();
    let bytes = text.as_bytes();
    if bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) && *byte == b'-' || byte.is_ascii_hexdigit()
        })
        && bytes[14] == b'4'
        && matches!(bytes[19].to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b')
    {
        return text.to_ascii_lowercase();
    }
    let digest = Sha256::digest(salt);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn timestamp(now: SystemTime) -> String {
    let seconds = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (seconds / 86_400) as i64;
    let within_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        within_day / 3600,
        within_day % 3600 / 60,
        within_day % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &[u8] = b"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    #[test]
    fn utc_timestamp_conversion_uses_rfc3339_calendar_dates() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + std::time::Duration::from_secs(86_400)),
            "1970-01-02T00:00:00Z",
        );
    }

    fn controller(key: &str, bus: Bus) -> RawRecord {
        let label = match bus {
            Bus::Usb => "USB controller",
            Bus::Thunderbolt => "Thunderbolt controller",
            Bus::Display => "Display controller",
            Bus::Bluetooth => "Bluetooth controller",
        };
        RawRecord::Controller {
            key: key.to_owned(),
            label: label.to_owned(),
            bus,
        }
    }

    fn thunderbolt(key: &str, parent: &str, vendor: &str, model: &str) -> RawRecord {
        RawRecord::ThunderboltDevice {
            key: key.to_owned(),
            parent_key: Some(parent.to_owned()),
            vendor_name: Some(vendor.to_owned()),
            device_name: Some(model.to_owned()),
            link_usb4: true,
        }
    }

    fn usb(
        key: &str,
        parent: &str,
        vendor_id: &str,
        product_id: &str,
        manufacturer: &str,
        product: &str,
        class: &str,
    ) -> RawRecord {
        RawRecord::UsbDevice {
            key: key.to_owned(),
            parent_key: Some(parent.to_owned()),
            vendor_id: Some(vendor_id.to_owned()),
            product_id: Some(product_id.to_owned()),
            manufacturer: Some(manufacturer.to_owned()),
            product: Some(product.to_owned()),
            speed_mbps: Some(480.0),
            class: Some(class.to_owned()),
            removable: Some(false),
        }
    }

    fn display(endpoint: &str, controller_key: &str, product: &str) -> RawRecord {
        RawRecord::Display {
            endpoint_token: endpoint.to_owned(),
            connector: "DP-1".to_owned(),
            controller_key: controller_key.to_owned(),
            manufacturer: Some("BenQ".to_owned()),
            product_name: Some(product.to_owned()),
        }
    }

    fn node<'a>(response: &'a InventoryResponse, label: &str) -> &'a InventoryNode {
        response
            .nodes
            .iter()
            .find(|node| node.label == label)
            .unwrap()
    }

    fn parent<'a>(response: &'a InventoryResponse, node: &'a InventoryNode) -> &'a InventoryNode {
        let parent_id = node.parent_id.as_deref().unwrap();
        response
            .nodes
            .iter()
            .find(|candidate| candidate.id == parent_id)
            .unwrap()
    }

    fn monitor_driver() -> DriverCatalogEntry {
        DriverCatalogEntry {
            id: "benq-rd280u".to_owned(),
            version: "1.0.0".to_owned(),
            supersedes: Vec::new(),
            inventory_matches: vec![InventoryMatch::Usb {
                vendor_id: "0bda".to_owned(),
                product_id: "1100".to_owned(),
                role: InventoryRole::MonitorCompanion,
            }],
        }
    }

    #[test]
    fn caldigit_usb4_tunnel_reparents_both_root_port_subtrees() {
        let records = vec![
            controller("0000:68:00.5", Bus::Thunderbolt),
            controller("0000:68:00.3", Bus::Usb),
            thunderbolt("0-2", "0000:68:00.5", "CalDigit, Inc.", "TS4"),
            usb(
                "5-1",
                "0000:68:00.3",
                "2188",
                "0450",
                "CalDigit, Inc.",
                "USB2 Hub",
                "09",
            ),
            usb(
                "5-1.5",
                "5-1",
                "0bda",
                "5411",
                "Realtek",
                "USB2.1 Hub",
                "09",
            ),
            usb(
                "5-1.5.6",
                "5-1.5",
                "0bda",
                "1100",
                "Realtek",
                "HID Device",
                "03",
            ),
            usb(
                "6-1",
                "0000:68:00.3",
                "8087",
                "0b40",
                "Intel",
                "USB hub",
                "09",
            ),
            usb(
                "6-1.4",
                "6-1",
                "2188",
                "0451",
                "CalDigit, Inc.",
                "USB3 Hub",
                "09",
            ),
            usb(
                "6-1.4.2", "6-1.4", "0fd9", "0066", "Elgato", "Facecam", "0e",
            ),
        ];
        let response = build(&records, &BTreeMap::new(), &[], SALT);
        let dock = node(&response, "CalDigit, Inc. TS4");
        assert_eq!(dock.kind, InventoryNodeKind::Dock);
        assert_eq!(parent(&response, dock).label, "Thunderbolt controller");
        let usb2 = node(&response, "CalDigit, Inc. USB2 Hub");
        assert_eq!(parent(&response, usb2).id, dock.id);
        assert_eq!(usb2.relation, InventoryRelation::Inferred);
        assert_eq!(usb2.rule, Some(InventoryRule::Usb4Tunnel));
        let usb3 = node(&response, "Intel USB hub");
        assert_eq!(parent(&response, usb3).id, dock.id);
        assert_eq!(
            node(&response, "Realtek HID Device").attachment,
            InventoryAttachment::Dock
        );
        assert_eq!(
            node(&response, "Elgato Facecam").attachment,
            InventoryAttachment::Dock
        );
        assert!(response.validate().is_ok());
    }

    #[test]
    fn ambiguous_two_dock_tunnel_uses_unique_vendor_matches_only() {
        let records = vec![
            controller("0000:68:00.5", Bus::Thunderbolt),
            controller("0000:68:00.3", Bus::Usb),
            thunderbolt("0-2", "0000:68:00.5", "CalDigit, Inc.", "TS4"),
            thunderbolt("0-3", "0000:68:00.5", "Anker", "Dock"),
            usb(
                "5-1",
                "0000:68:00.3",
                "2188",
                "0450",
                "CalDigit, Inc.",
                "USB Hub",
                "09",
            ),
            usb(
                "6-1",
                "0000:68:00.3",
                "2109",
                "0817",
                "Anker",
                "USB Hub",
                "09",
            ),
            usb(
                "7-1",
                "0000:68:00.3",
                "1234",
                "5678",
                "Unknown",
                "USB Hub",
                "09",
            ),
        ];
        let response = build(&records, &BTreeMap::new(), &[], SALT);
        let caldigit = node(&response, "CalDigit, Inc. USB Hub");
        let anker = node(&response, "Anker USB Hub");
        assert_eq!(parent(&response, caldigit).label, "CalDigit, Inc. TS4");
        assert_eq!(parent(&response, anker).label, "Anker Dock");
        assert_eq!(caldigit.rule, Some(InventoryRule::VendorMatch));
        assert_eq!(anker.rule, Some(InventoryRule::VendorMatch));
        let unknown = node(&response, "Unknown USB Hub");
        assert_eq!(parent(&response, unknown).label, "USB controller");
        assert_eq!(unknown.relation, InventoryRelation::Exact);
    }

    #[test]
    fn unique_monitor_companion_links_hub_and_driver_device() {
        let records = vec![
            controller("0000:04:00.0", Bus::Display),
            controller("0000:00:14.0", Bus::Usb),
            display("endpoint-token", "0000:04:00.0", "RD280U"),
            usb(
                "5-1.5",
                "0000:00:14.0",
                "0bda",
                "5411",
                "Realtek",
                "USB hub",
                "09",
            ),
            usb(
                "5-1.5.6",
                "5-1.5",
                "0bda",
                "1100",
                "Realtek",
                "HID Device",
                "03",
            ),
        ];
        let bindings = BTreeMap::from([(
            "monitor".to_owned(),
            DeviceBinding {
                driver: "benq-rd280u".to_owned(),
                endpoints: vec!["endpoint-token".to_owned()],
            },
        )]);
        let response = build(&records, &bindings, &[monitor_driver()], SALT);
        let display = node(&response, "BenQ RD280U");
        let hub = node(&response, "Realtek USB hub");
        let hid = node(&response, "Realtek HID Device");
        assert_eq!(parent(&response, hub).id, display.id);
        assert_eq!(hub.relation, InventoryRelation::Inferred);
        assert_eq!(hub.rule, Some(InventoryRule::DriverLink));
        assert_eq!(hub.attachment, InventoryAttachment::Monitor);
        assert_eq!(hid.attachment, InventoryAttachment::Monitor);
        let badge = hid.driver.as_ref().unwrap();
        assert_eq!(badge.id, "benq-rd280u");
        assert_eq!(badge.device_id.as_deref(), Some("monitor"));
        assert_eq!(badge.class, InventoryDriverClass::Specialized);
        let display_badge = display.driver.as_ref().unwrap();
        assert_eq!(display_badge.class, InventoryDriverClass::Generic);
    }

    #[test]
    fn monitor_companion_requires_a_globally_unique_driver_display_usb_match() {
        let records = vec![
            controller("gpu0", Bus::Display),
            controller("usb0", Bus::Usb),
            display("endpoint-a", "gpu0", "RD280U A"),
            display("endpoint-b", "gpu0", "RD280U B"),
            usb("5-1.5", "usb0", "0bda", "5411", "Realtek", "USB hub", "09"),
            usb(
                "5-1.5.6",
                "5-1.5",
                "0bda",
                "1100",
                "Realtek",
                "HID Device",
                "03",
            ),
        ];
        let bindings = BTreeMap::from([
            (
                "display-a".to_owned(),
                DeviceBinding {
                    driver: "benq-rd280u".to_owned(),
                    endpoints: vec!["endpoint-a".to_owned()],
                },
            ),
            (
                "display-b".to_owned(),
                DeviceBinding {
                    driver: "other-monitor".to_owned(),
                    endpoints: vec!["endpoint-b".to_owned()],
                },
            ),
        ]);
        let first = monitor_driver();
        let mut second = monitor_driver();
        second.id = "other-monitor".to_owned();
        let catalog = vec![first, second];
        let response = build(&records, &bindings, &catalog, SALT);
        let mut reversed_catalog = catalog;
        reversed_catalog.reverse();
        let reversed = build(&records, &bindings, &reversed_catalog, SALT);

        let companion = node(&response, "Realtek HID Device");
        let hub = node(&response, "Realtek USB hub");
        let controller = node(&response, "USB controller");
        assert_eq!(parent(&response, companion).id, hub.id);
        assert_eq!(parent(&response, hub).id, controller.id);
        assert_eq!(companion.relation, InventoryRelation::Exact);
        assert_eq!(hub.relation, InventoryRelation::Exact);
        assert!(companion.driver.is_none());
        let reversed_companion = node(&reversed, "Realtek HID Device");
        let reversed_hub = node(&reversed, "Realtek USB hub");
        let reversed_controller = node(&reversed, "USB controller");
        assert_eq!(parent(&reversed, reversed_companion).id, reversed_hub.id);
        assert_eq!(parent(&reversed, reversed_hub).id, reversed_controller.id);
        assert_eq!(reversed_companion.relation, InventoryRelation::Exact);
        assert_eq!(reversed_hub.relation, InventoryRelation::Exact);
        assert!(reversed_companion.driver.is_none());
    }

    #[test]
    fn ambiguous_monitor_usb_match_is_not_reparented() {
        let records = vec![
            controller("gpu0", Bus::Display),
            controller("usb0", Bus::Usb),
            display("endpoint-token", "gpu0", "RD280U"),
            usb("5-1.5", "usb0", "0bda", "5411", "Realtek", "USB hub", "09"),
            usb(
                "5-1.5.6",
                "5-1.5",
                "0bda",
                "1100",
                "Realtek",
                "First HID",
                "03",
            ),
            usb(
                "6-1.2",
                "usb0",
                "0bda",
                "1100",
                "Realtek",
                "Second HID",
                "03",
            ),
        ];
        let bindings = BTreeMap::from([(
            "monitor".to_owned(),
            DeviceBinding {
                driver: "benq-rd280u".to_owned(),
                endpoints: vec!["endpoint-token".to_owned()],
            },
        )]);
        let response = build(&records, &bindings, &[monitor_driver()], SALT);
        let hub = node(&response, "Realtek USB hub");
        let controller = node(&response, "USB controller");
        assert_eq!(parent(&response, hub).id, controller.id);
        assert_eq!(hub.relation, InventoryRelation::Exact);
        assert_ne!(hub.rule, Some(InventoryRule::DriverLink));
        assert_eq!(
            node(&response, "Realtek First HID").attachment,
            InventoryAttachment::Host
        );
    }

    #[test]
    fn driver_class_comes_from_supersedes_or_manifest_declarations() {
        let records = vec![
            controller("gpu0", Bus::Display),
            display("endpoint-generic", "gpu0", "Generic panel"),
            display("endpoint-specialized", "gpu0", "Specialized panel"),
        ];
        let bindings = BTreeMap::from([
            (
                "generic-device".to_owned(),
                DeviceBinding {
                    driver: "generic-ddc".to_owned(),
                    endpoints: vec!["endpoint-generic".to_owned()],
                },
            ),
            (
                "specialized-device".to_owned(),
                DeviceBinding {
                    driver: "specialized-ddc".to_owned(),
                    endpoints: vec!["endpoint-specialized".to_owned()],
                },
            ),
        ]);
        let catalog = [
            DriverCatalogEntry {
                id: "generic-ddc".to_owned(),
                version: "1.2.3".to_owned(),
                supersedes: Vec::new(),
                inventory_matches: vec![InventoryMatch::Usb {
                    vendor_id: "ffff".to_owned(),
                    product_id: "ffff".to_owned(),
                    role: InventoryRole::Device,
                }],
            },
            DriverCatalogEntry {
                id: "specialized-ddc".to_owned(),
                version: "2.0.0".to_owned(),
                supersedes: vec!["generic-ddc".to_owned()],
                inventory_matches: Vec::new(),
            },
        ];
        let response = build(&records, &bindings, &catalog, SALT);
        assert_eq!(
            node(&response, "BenQ Generic panel")
                .driver
                .as_ref()
                .unwrap()
                .class,
            InventoryDriverClass::Generic
        );
        assert_eq!(
            node(&response, "BenQ Specialized panel")
                .driver
                .as_ref()
                .unwrap()
                .class,
            InventoryDriverClass::Specialized
        );
    }

    #[test]
    fn public_ids_and_labels_do_not_reveal_native_keys_addresses_or_paths() {
        let native_key = "/sys/devices/pci0000:68/0000:68:00.3/usb5/5-1/J7R0093601Q";
        let records = vec![
            controller("0000:68:00.3", Bus::Usb),
            RawRecord::UsbDevice {
                key: native_key.to_owned(),
                parent_key: Some("0000:68:00.3".to_owned()),
                vendor_id: Some("0BDA".to_owned()),
                product_id: Some("1100".to_owned()),
                manufacturer: Some("Maker 01:23:45:67:89:AB".to_owned()),
                product: Some("device01:23:45:67:89:AB".to_owned()),
                speed_mbps: Some(480.0),
                class: Some("03".to_owned()),
                removable: Some(false),
            },
            RawRecord::Display {
                endpoint_token: "/dev/i2c-7".to_owned(),
                connector: "DP-1".to_owned(),
                controller_key: "gpu0".to_owned(),
                manufacturer: Some("BenQ".to_owned()),
                product_name: Some("RD280U".to_owned()),
            },
            RawRecord::BluetoothAdapter {
                key: "hci0".to_owned(),
                controller_key: None,
                name: Some("Bluetooth".to_owned()),
                powered: true,
            },
            RawRecord::BluetoothDevice {
                key: "/org/bluez/hci0/dev_01_23_45_67_89_AB".to_owned(),
                adapter_key: "hci0".to_owned(),
                name: Some("Keyboard 01:23:45:67:89:AB".to_owned()),
                icon: Some("input-keyboard".to_owned()),
                paired: true,
                connected: true,
            },
        ];
        let response = build(&records, &BTreeMap::new(), &[], SALT);
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains(native_key));
        assert!(!json.contains("/dev/i2c-7"));
        assert!(!json.contains("/org/bluez"));
        assert!(!json.contains("J7R0093601Q"));
        assert!(!json.contains("01:23:45:67:89:AB"));
        assert_eq!(
            redact_sensitive("device01:23:45:67:89:AB"),
            "device[redacted]"
        );
        assert!(json.contains("[redacted]"));
        for node in &response.nodes {
            assert_eq!(node.id.len(), 17);
            assert!(node.id.starts_with('n'));
            assert!(!node.id.contains('/'));
        }
        assert!(response.validate().is_ok());
        let same_install = build(&records, &BTreeMap::new(), &[], SALT);
        let other_install = build(&records, &BTreeMap::new(), &[], b"another-install");
        assert_eq!(response.nodes[1].id, same_install.nodes[1].id);
        assert_ne!(response.nodes[1].id, other_install.nodes[1].id);
    }

    #[test]
    fn topology_stays_within_the_public_node_limit() {
        let records: Vec<RawRecord> = (0..(MAX_INVENTORY_NODES + 20))
            .map(|index| RawRecord::BluetoothDevice {
                key: format!("device-{index}"),
                adapter_key: "missing-adapter".to_owned(),
                name: Some(format!("Device {index}")),
                icon: None,
                paired: true,
                connected: false,
            })
            .collect();
        let response = build(&records, &BTreeMap::new(), &[], SALT);
        assert_eq!(response.nodes.len(), MAX_INVENTORY_NODES);
        assert!(response.validate().is_ok());
    }
}
