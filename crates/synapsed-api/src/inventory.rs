//! Public, privacy-bounded hardware inventory wire types.

use crate::{
    ValidationResult, validate_driver_id, validate_driver_version, validate_id, validate_label,
    validate_timestamp, validate_uuid_v4,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Maximum number of public inventory nodes in one response.
pub const MAX_INVENTORY_NODES: usize = 1024;

/// The independently collected hardware source represented by a source status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventorySourceKind {
    /// USB devices and host controllers.
    Usb,
    /// Thunderbolt and USB4 topology.
    Thunderbolt,
    /// Display endpoints discovered by the DDC owner.
    Ddc,
    /// Bluetooth adapters and paired devices.
    Bluetooth,
}

/// Availability of one inventory source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventorySourceState {
    /// The source was read successfully.
    Ok,
    /// The DDC owner has not performed its first scan.
    NotScanned,
    /// The source is not available on this machine.
    Unavailable,
    /// Access was denied by the operating system.
    PermissionDenied,
    /// The source failed to read.
    Error,
    /// The source did not complete before its deadline.
    Timeout,
}

/// State and safe diagnostic for one independently collected source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InventorySource {
    /// Source category.
    pub kind: InventorySourceKind,
    /// Collection outcome.
    pub state: InventorySourceState,
    /// Optional bounded, non-sensitive diagnostic.
    #[cfg_attr(feature = "schema", schemars(length(max = 256)))]
    pub message: Option<String>,
}

/// Public kind of a node in the hardware tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventoryNodeKind {
    /// The current computer.
    Host,
    /// A USB, display, Thunderbolt or Bluetooth controller.
    Controller,
    /// A Thunderbolt/USB4 dock.
    Dock,
    /// A USB hub.
    Hub,
    /// A non-hub USB device.
    UsbDevice,
    /// A display endpoint.
    Display,
    /// A Bluetooth adapter.
    BluetoothAdapter,
    /// A paired Bluetooth device.
    BluetoothDevice,
}

/// Physical attachment category shown in the device tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventoryAttachment {
    /// Connected directly to the host or to an unclassified host controller.
    Host,
    /// Connected through an inferred dock.
    Dock,
    /// Connected through a display's inferred USB companion hub.
    Monitor,
}

/// Whether a topology edge is directly observed or inferred by an explicit rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventoryRelation {
    /// The operating system directly reports this parent-child relationship.
    Exact,
    /// The relationship was derived from a documented matching rule.
    Inferred,
}

/// Named rule used for an inferred topology edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventoryRule {
    /// USB root ports were matched to a USB4 tunnel on the same PCI device.
    Usb4Tunnel,
    /// A dock vendor name was matched to USB root-port manufacturer data.
    VendorMatch,
    /// A driver inventory declaration linked a USB companion to a display.
    DriverLink,
}

/// Driver category shown alongside a discovered node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventoryDriverClass {
    /// A driver claims specialized support for this hardware.
    Specialized,
    /// A generic driver publishes the device.
    Generic,
}

/// Driver publishing a discovered node, when one can be linked safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InventoryDriver {
    /// Installed driver identifier.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::DRIVER_ID_PATTERN)))]
    pub id: String,
    /// Installed driver version.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::DRIVER_VERSION_PATTERN)))]
    pub version: String,
    /// Whether the driver is generic or specialized.
    pub class: InventoryDriverClass,
    /// Published device identifier, if the inventory match resolved to one.
    pub device_id: Option<String>,
}

/// One sanitized public node and its relationship to its parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InventoryNode {
    /// Per-install opaque identifier; never contains a native key.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::INVENTORY_NODE_ID_PATTERN)))]
    pub id: String,
    /// Opaque identifier of the parent node, or `None` for the host root.
    pub parent_id: Option<String>,
    /// Public node category.
    pub kind: InventoryNodeKind,
    /// Human-readable, bounded and sanitized label.
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    pub label: String,
    /// Optional safe supplemental text.
    #[cfg_attr(feature = "schema", schemars(length(max = 256)))]
    pub detail: Option<String>,
    /// Four-digit lowercase USB vendor identifier, when available.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::USB_ID_PATTERN)))]
    pub vendor_id: Option<String>,
    /// Four-digit lowercase USB product identifier, when available.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::USB_ID_PATTERN)))]
    pub product_id: Option<String>,
    /// USB link rate in megabits per second, when available.
    pub speed_mbps: Option<f64>,
    /// Whether the device is currently connected or powered.
    pub connected: bool,
    /// Inferred attachment group inherited from the tree.
    pub attachment: InventoryAttachment,
    /// Evidence quality for this node's parent edge.
    pub relation: InventoryRelation,
    /// Rule explaining an inferred parent edge.
    pub rule: Option<InventoryRule>,
    /// Driver linked to this node, if any.
    pub driver: Option<InventoryDriver>,
}

/// Read-only inventory snapshot returned by the daemon API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InventoryResponse {
    /// Version of the API contract.
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    pub api_version: u8,
    /// Stable identifier for this daemon process.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::UUID_V4_PATTERN)))]
    pub instance_id: String,
    /// UTC RFC3339 generation timestamp.
    #[cfg_attr(feature = "schema", schemars(regex(pattern = crate::UTC_TIMESTAMP_PATTERN)))]
    pub generated_at: String,
    /// Independent status for each inventory source.
    #[cfg_attr(feature = "schema", schemars(length(max = 4)))]
    pub sources: Vec<InventorySource>,
    /// Bounded host-rooted topology.
    #[cfg_attr(feature = "schema", schemars(length(max = MAX_INVENTORY_NODES)))]
    pub nodes: Vec<InventoryNode>,
}

impl InventoryResponse {
    /// Validate public bounds, references, identifiers and topology invariants.
    pub fn validate(&self) -> ValidationResult {
        if self.api_version != crate::API_VERSION {
            return Err(crate::ApiError::new(
                crate::ErrorCode::InvalidValue,
                "Inventory API version is unsupported.",
            ));
        }
        validate_uuid_v4(&self.instance_id)?;
        validate_timestamp(&self.generated_at)?;
        if self.nodes.len() > MAX_INVENTORY_NODES {
            return Err(crate::ApiError::new(
                crate::ErrorCode::InvalidValue,
                "Inventory contains too many nodes.",
            ));
        }
        if self.sources.len() > 4 {
            return Err(crate::ApiError::new(
                crate::ErrorCode::InvalidValue,
                "Inventory contains too many sources.",
            ));
        }
        let mut source_kinds = HashSet::with_capacity(self.sources.len());
        for source in &self.sources {
            if !source_kinds.insert(source.kind) {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "Inventory source kinds must be unique.",
                ));
            }
            if source
                .message
                .as_ref()
                .is_some_and(|message| message.chars().count() > 256)
            {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "Inventory source message is too long.",
                ));
            }
        }
        let mut by_id = HashMap::with_capacity(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            if !valid_node_id(&node.id) || by_id.insert(node.id.as_str(), index).is_some() {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "Inventory node IDs must be unique opaque identifiers.",
                ));
            }
            validate_label(&node.label)?;
            if node
                .detail
                .as_ref()
                .is_some_and(|detail| detail.chars().count() > 256)
            {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "Inventory node detail is too long.",
                ));
            }
            if !valid_optional_hex_id(&node.vendor_id) || !valid_optional_hex_id(&node.product_id) {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "USB identifiers must be four lowercase hexadecimal characters.",
                ));
            }
            if node
                .speed_mbps
                .is_some_and(|speed| !speed.is_finite() || speed <= 0.0)
            {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "USB link speed must be positive and finite.",
                ));
            }
            if (node.relation == InventoryRelation::Exact) != node.rule.is_none() {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "Only inferred inventory edges may declare a rule.",
                ));
            }
            if let Some(driver) = &node.driver {
                validate_driver_id(&driver.id)?;
                validate_driver_version(&driver.version)?;
                if let Some(device_id) = &driver.device_id {
                    validate_id(device_id)?;
                }
            }
            if node.kind == InventoryNodeKind::Host {
                if node.parent_id.is_some()
                    || node.attachment != InventoryAttachment::Host
                    || node.relation != InventoryRelation::Exact
                {
                    return Err(crate::ApiError::new(
                        crate::ErrorCode::InvalidValue,
                        "Inventory host must be the exact root node.",
                    ));
                }
            } else if node.parent_id.is_none() {
                return Err(crate::ApiError::new(
                    crate::ErrorCode::InvalidValue,
                    "Only the inventory host may be a root node.",
                ));
            }
        }
        let hosts = self
            .nodes
            .iter()
            .filter(|node| node.kind == InventoryNodeKind::Host)
            .count();
        if hosts != 1 {
            return Err(crate::ApiError::new(
                crate::ErrorCode::InvalidValue,
                "Inventory must contain exactly one host node.",
            ));
        }
        for node in &self.nodes {
            if let Some(parent_id) = &node.parent_id {
                let Some(parent_index) = by_id.get(parent_id.as_str()) else {
                    return Err(crate::ApiError::new(
                        crate::ErrorCode::InvalidValue,
                        "Inventory parent reference is invalid.",
                    ));
                };
                if parent_id == &node.id {
                    return Err(crate::ApiError::new(
                        crate::ErrorCode::InvalidValue,
                        "Inventory parent reference is invalid.",
                    ));
                }
                let parent = &self.nodes[*parent_index];
                let expected_attachment = if node.kind == InventoryNodeKind::Dock {
                    InventoryAttachment::Dock
                } else if node.rule == Some(InventoryRule::DriverLink) {
                    InventoryAttachment::Monitor
                } else {
                    parent.attachment
                };
                if node.attachment != expected_attachment {
                    return Err(crate::ApiError::new(
                        crate::ErrorCode::InvalidValue,
                        "Inventory attachment contradicts its topology.",
                    ));
                }
                let mut seen = HashSet::new();
                let mut current = node;
                while let Some(parent) = current.parent_id.as_deref() {
                    if !seen.insert(current.id.as_str()) {
                        return Err(crate::ApiError::new(
                            crate::ErrorCode::InvalidValue,
                            "Inventory topology must not contain cycles.",
                        ));
                    }
                    let Some(parent_index) = by_id.get(parent) else {
                        return Err(crate::ApiError::new(
                            crate::ErrorCode::InvalidValue,
                            "Inventory parent reference is invalid.",
                        ));
                    };
                    current = &self.nodes[*parent_index];
                }
            }
        }
        Ok(())
    }
}

fn valid_node_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 17
        && bytes[0] == b'n'
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn valid_optional_hex_id(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|value| {
        value.len() == 4
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent_id: Option<&str>, kind: InventoryNodeKind) -> InventoryNode {
        InventoryNode {
            id: id.to_owned(),
            parent_id: parent_id.map(str::to_owned),
            kind,
            label: if kind == InventoryNodeKind::Host {
                "This machine"
            } else {
                "Device"
            }
            .to_owned(),
            detail: None,
            vendor_id: None,
            product_id: None,
            speed_mbps: None,
            connected: true,
            attachment: InventoryAttachment::Host,
            relation: InventoryRelation::Exact,
            rule: None,
            driver: None,
        }
    }

    fn response(nodes: Vec<InventoryNode>) -> InventoryResponse {
        InventoryResponse {
            api_version: crate::API_VERSION,
            instance_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".to_owned(),
            generated_at: "2025-01-02T03:04:05Z".to_owned(),
            sources: Vec::new(),
            nodes,
        }
    }

    #[test]
    fn validates_a_bounded_host_rooted_tree() {
        let value = response(vec![
            node("n0000000000000000", None, InventoryNodeKind::Host),
            node(
                "n0000000000000001",
                Some("n0000000000000000"),
                InventoryNodeKind::UsbDevice,
            ),
        ]);
        assert!(value.validate().is_ok());
    }

    #[test]
    fn rejects_inconsistent_attachment_inheritance() {
        let host_id = "n0000000000000000";
        let mut direct_device = response(vec![
            node(host_id, None, InventoryNodeKind::Host),
            node(
                "n0000000000000001",
                Some(host_id),
                InventoryNodeKind::UsbDevice,
            ),
        ]);
        direct_device.nodes[1].attachment = InventoryAttachment::Dock;
        assert!(direct_device.validate().is_err());

        let mut dock = response(vec![
            node(host_id, None, InventoryNodeKind::Host),
            node(
                "n0000000000000001",
                Some(host_id),
                InventoryNodeKind::Controller,
            ),
            node(
                "n0000000000000002",
                Some("n0000000000000001"),
                InventoryNodeKind::Dock,
            ),
        ]);
        dock.nodes[2].relation = InventoryRelation::Inferred;
        dock.nodes[2].rule = Some(InventoryRule::Usb4Tunnel);
        dock.nodes[2].attachment = InventoryAttachment::Dock;
        assert!(dock.validate().is_ok());
        dock.nodes[2].attachment = InventoryAttachment::Host;
        assert!(dock.validate().is_err());

        let mut monitor = response(vec![
            node(host_id, None, InventoryNodeKind::Host),
            node(
                "n0000000000000001",
                Some(host_id),
                InventoryNodeKind::Display,
            ),
            node(
                "n0000000000000002",
                Some("n0000000000000001"),
                InventoryNodeKind::Hub,
            ),
            node(
                "n0000000000000003",
                Some("n0000000000000002"),
                InventoryNodeKind::UsbDevice,
            ),
        ]);
        monitor.nodes[2].relation = InventoryRelation::Inferred;
        monitor.nodes[2].rule = Some(InventoryRule::DriverLink);
        monitor.nodes[2].attachment = InventoryAttachment::Monitor;
        monitor.nodes[3].attachment = InventoryAttachment::Monitor;
        assert!(monitor.validate().is_ok());

        monitor.nodes[2].attachment = InventoryAttachment::Host;
        assert!(monitor.validate().is_err());
        monitor.nodes[2].attachment = InventoryAttachment::Monitor;
        monitor.nodes[3].attachment = InventoryAttachment::Host;
        assert!(monitor.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_sources_bad_ids_and_cycles() {
        let mut duplicate_sources = response(vec![node(
            "n0000000000000000",
            None,
            InventoryNodeKind::Host,
        )]);
        duplicate_sources.sources = vec![
            InventorySource {
                kind: InventorySourceKind::Usb,
                state: InventorySourceState::Ok,
                message: None,
            },
            InventorySource {
                kind: InventorySourceKind::Usb,
                state: InventorySourceState::Error,
                message: None,
            },
        ];
        assert!(duplicate_sources.validate().is_err());

        let bad_id = response(vec![node("native-key", None, InventoryNodeKind::Host)]);
        assert!(bad_id.validate().is_err());

        let cycle = response(vec![
            node("n0000000000000000", None, InventoryNodeKind::Host),
            node(
                "n0000000000000001",
                Some("n0000000000000002"),
                InventoryNodeKind::UsbDevice,
            ),
            node(
                "n0000000000000002",
                Some("n0000000000000001"),
                InventoryNodeKind::UsbDevice,
            ),
        ]);
        assert!(cycle.validate().is_err());
    }

    #[test]
    fn rejects_inventory_over_the_node_limit() {
        let mut nodes = vec![node("n0000000000000000", None, InventoryNodeKind::Host)];
        nodes.extend((1..=MAX_INVENTORY_NODES).map(|index| {
            node(
                &format!("n{index:016x}"),
                Some("n0000000000000000"),
                InventoryNodeKind::UsbDevice,
            )
        }));
        assert!(response(nodes).validate().is_err());
    }
}
