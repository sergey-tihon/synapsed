//! Internal hardware facts collected from native, read-only sources.

use sha2::{Digest, Sha256};
use synapsed_api::InventorySource;

/// Stable identity shared by Bluetooth endpoint and inventory records.
pub fn bluetooth_endpoint_identity(native_key: &str) -> String {
    let digest = Sha256::digest(format!("bluetooth:{native_key}").as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Sanitized advertised Bluetooth name used for inventory matching.
pub fn bluetooth_display_name(raw: &str) -> Option<String> {
    let name = raw
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let name = name.trim().chars().take(64).collect::<String>();
    (!name.is_empty()).then_some(name)
}

/// The broad controller bus represented by a raw controller record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// USB host controller.
    Usb,
    /// Thunderbolt/USB4 domain controller.
    Thunderbolt,
    /// Display/GPU controller.
    Display,
    /// Bluetooth controller when no USB parent can be resolved.
    Bluetooth,
}

/// Unsanitized, owner-local device data. Keys are never copied into public fields.
#[derive(Debug, Clone, PartialEq)]
pub enum RawRecord {
    /// A native host controller or display controller.
    Controller {
        /// Internal stable key used only to join collected records.
        key: String,
        /// Human-readable source label.
        label: String,
        /// Controller bus class.
        bus: Bus,
    },
    /// A USB device (not an interface).
    UsbDevice {
        /// USB port-path key, e.g. `5-1.5.6`.
        key: String,
        /// Parent USB device or host-controller key.
        parent_key: Option<String>,
        /// Four-digit USB vendor identifier.
        vendor_id: Option<String>,
        /// Four-digit USB product identifier.
        product_id: Option<String>,
        /// USB manufacturer string, if provided by the source.
        manufacturer: Option<String>,
        /// USB product string, if provided by the source.
        product: Option<String>,
        /// Negotiated link speed in megabits per second.
        speed_mbps: Option<f64>,
        /// USB device class as a lowercase hexadecimal string.
        class: Option<String>,
        /// Whether the USB device is removable.
        removable: Option<bool>,
    },
    /// A Thunderbolt/USB4 switch or peripheral.
    ThunderboltDevice {
        /// Internal registry/sysfs key.
        key: String,
        /// Parent switch or domain-controller key.
        parent_key: Option<String>,
        /// Vendor label.
        vendor_name: Option<String>,
        /// Model label.
        device_name: Option<String>,
        /// Whether a USB4 tunnel is reported for one of its ports.
        link_usb4: bool,
    },
    /// A DDC display endpoint with optional graphics-controller placement.
    Display {
        /// Owner-local endpoint token, never serialized.
        endpoint_token: String,
        /// DRM/IORegistry connector label, when placement is known.
        connector: Option<String>,
        /// Graphics-controller key, when placement is known.
        controller_key: Option<String>,
        /// Display manufacturer label.
        manufacturer: Option<String>,
        /// Display product name.
        product_name: Option<String>,
    },
    /// A Bluetooth adapter.
    BluetoothAdapter {
        /// Internal adapter key.
        key: String,
        /// USB device key, when the native topology identifies one.
        controller_key: Option<String>,
        /// Adapter name.
        name: Option<String>,
        /// Whether the adapter is powered.
        powered: bool,
    },
    /// A Bluetooth device observed through the OS's passive device list.
    BluetoothDevice {
        /// Internal adapter-qualified key.
        key: String,
        /// Identity of the matching native Bluetooth endpoint, when the platform key permits an exact join.
        endpoint_identity: Option<String>,
        /// Parent Bluetooth adapter key.
        adapter_key: String,
        /// User-visible alias, if available.
        name: Option<String>,
        /// BlueZ or IOBluetooth icon/category string.
        icon: Option<String>,
        /// Whether the device is paired.
        paired: bool,
        /// Whether the device is connected.
        connected: bool,
    },
}

/// A native endpoint associated with a published device binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundEndpoint {
    /// Stable inventory key of a DDC display record.
    Display(String),
    /// Native Bluetooth endpoint identity and sanitized advertised name.
    Bluetooth {
        identity: String,
        name: Option<String>,
    },
}

/// Driver binding for a device published by an installed plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBinding {
    /// Publishing driver ID.
    pub driver: String,
    /// Native endpoints associated with this device.
    pub endpoints: Vec<BoundEndpoint>,
}
/// Inventory match declared by an installed driver manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InventoryMatch {
    /// USB VID/PID selector.
    Usb {
        /// Four lowercase hexadecimal vendor digits.
        vendor_id: String,
        /// Four lowercase hexadecimal product digits.
        product_id: String,
        /// Declared inventory role.
        role: InventoryRole,
    },
    /// Bluetooth name-prefix selector.
    Bluetooth {
        /// Prefix compared against the device name.
        name_prefix: String,
        /// Declared inventory role.
        role: InventoryRole,
    },
}

/// Manifest role for an inventory selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryRole {
    /// A USB companion belonging to a display handled by this driver.
    MonitorCompanion,
    /// A device supported by the driver.
    Device,
}

/// Driver information needed to derive public driver badges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverCatalogEntry {
    /// Driver identifier.
    pub id: String,
    /// Driver semantic version.
    pub version: String,
    /// Whether the manifest declares this driver generic.
    pub generic: bool,
    /// Explicit inventory selectors from the manifest.
    pub inventory_matches: Vec<InventoryMatch>,
}

/// One source's independent collection result.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceCollection {
    /// Raw records from the source.
    pub records: Vec<RawRecord>,
    /// Sanitized source status for the public response.
    pub status: InventorySource,
}
