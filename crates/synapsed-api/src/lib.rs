#![warn(missing_docs)]

//! Device-neutral local API wire data and shared semantic validation.
//! Deserialization is deliberately separate from semantic validation: callers must
//! validate received declarations, commands and observations before using them.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
mod inventory;
pub use inventory::*;
/// Current wire API version.
pub const API_VERSION: u8 = 1;
/// Maximum encoded characteristic command size in bytes.
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;
/// Maximum encoded snapshot size in bytes.
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
/// Maximum devices in a snapshot.
pub const MAX_DEVICES: usize = 64;
/// Maximum characteristics in a snapshot.
pub const MAX_CHARACTERISTICS: usize = 512;
/// Maximum characteristics on one device.
pub const MAX_CHARACTERISTICS_PER_DEVICE: usize = 64;
/// Maximum groups on one device.
pub const MAX_GROUPS_PER_DEVICE: usize = 16;
/// Maximum drivers in a snapshot.
pub const MAX_DRIVERS: usize = 16;
/// Maximum recent log entries.
pub const MAX_LOG_ENTRIES: usize = 200;
/// Regular expression for canonical non-negative decimal sequence strings.
pub const DECIMAL_SEQUENCE_PATTERN: &str = "^(0|[1-9][0-9]*)$";
/// Regular expression for declaration identifiers.
pub const DECLARATION_ID_PATTERN: &str = "^[a-z][a-z0-9_.-]{0,63}$";
/// Regular expression for driver identifiers.
pub const DRIVER_ID_PATTERN: &str = "^[a-z][a-z0-9-]{0,63}$";
/// Regular expression for opaque public inventory node identifiers.
pub const INVENTORY_NODE_ID_PATTERN: &str = "^n[0-9a-f]{16}$";
/// Regular expression for four-digit lowercase USB identifiers.
pub const USB_ID_PATTERN: &str = "^[0-9a-f]{4}$";
/// Regular expression for numeric semantic driver versions.
pub const DRIVER_VERSION_PATTERN: &str = "^[0-9]+\\.[0-9]+\\.[0-9]+$";
/// Regular expression for canonical lowercase UUID version 4 strings.
pub const UUID_V4_PATTERN: &str =
    "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$";
/// Regular expression for UTC RFC 3339 timestamps with an optional fractional second.
pub const UTC_TIMESTAMP_PATTERN: &str =
    "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?(Z|\\+00:00)$";
/// Result type returned by API validation routines.
pub type ValidationResult = Result<(), ApiError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
/// Machine-readable category describing an API error.
pub enum ErrorCode {
    /// The request could not be parsed or otherwise violates the API contract.
    InvalidRequest,
    /// The request origin is not allowed.
    ForbiddenOrigin,
    /// The request host is not allowed.
    ForbiddenHost,
    /// The requested device does not exist.
    UnknownDevice,
    /// The requested characteristic does not exist.
    UnknownCharacteristic,
    /// The request identifies more than one device.
    AmbiguousDevice,
    /// The requested driver operation conflicts with current ownership.
    DriverConflict,
    /// The requested operation is not supported.
    UnsupportedOperation,
    /// A supplied value is invalid.
    InvalidValue,
    /// The operation is not permitted.
    PermissionDenied,
    /// The requested device is unavailable.
    DeviceUnavailable,
    /// A DDC transport operation failed.
    DdcError,
    /// A driver plugin operation failed.
    PluginError,
    /// The requested capability is not supported.
    UnsupportedCapability,
    /// The operation could not proceed because the service is busy.
    Busy,
    /// The operation exceeded its deadline.
    Timeout,
}
impl ErrorCode {
    /// Map this error category to its HTTP response status.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::ForbiddenOrigin | Self::ForbiddenHost => 403,
            Self::UnknownDevice | Self::UnknownCharacteristic => 404,
            Self::AmbiguousDevice | Self::DriverConflict => 409,
            Self::UnsupportedOperation | Self::InvalidValue => 422,
            Self::PermissionDenied
            | Self::DeviceUnavailable
            | Self::DdcError
            | Self::PluginError
            | Self::UnsupportedCapability
            | Self::Busy => 503,
            Self::Timeout => 504,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// An API error with a bounded, user-safe message.
pub struct ApiError {
    /// Category identifying the kind of failure.
    pub code: ErrorCode,
    #[cfg_attr(feature = "schema", schemars(length(max = 256)))]
    /// Bounded human-readable description of the failure.
    pub message: String,
}
impl ApiError {
    /// Messages supplied here must be static, bounded English descriptions, never input bodies.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    /// Validate that the message satisfies the API's size and non-empty requirements.
    pub fn validate(&self) -> ValidationResult {
        bounded(&self.message, 256, false)
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ApiError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// Standard response body containing an API error.
pub struct ErrorResponse {
    /// The error details returned to the caller.
    pub error: ApiError,
}
impl ErrorResponse {
    /// Validate this error response.
    pub fn validate(&self) -> ValidationResult {
        self.error.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// Identifiers and display name for the daemon node and its running instance.
pub struct NodeInfo {
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(
                pattern = UUID_V4_PATTERN
            )
        )
    )]
    /// Stable identifier for this node, encoded as canonical lowercase UUID-v4.
    pub node_id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(
                pattern = UUID_V4_PATTERN
            )
        )
    )]
    /// Identifier for this daemon instance, encoded as canonical lowercase UUID-v4.
    pub instance_id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    /// User-facing node name.
    pub name: String,
}
impl NodeInfo {
    /// Validate both identities and the node name.
    pub fn validate(&self) -> ValidationResult {
        validate_uuid_v4(&self.node_id)?;
        validate_uuid_v4(&self.instance_id)?;
        validate_label(&self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
/// Current lifecycle or failure state of an installed driver.
pub enum DriverState {
    /// The driver is installed but disabled.
    Disabled,
    /// The driver is operating normally.
    Online,
    /// The driver is disconnected from its resource.
    Disconnected,
    /// The driver identity or resource is ambiguous.
    Ambiguous,
    /// Required access was denied.
    PermissionDenied,
    /// The driver encountered an error.
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// Status and optional error details for an installed driver.
pub struct DriverStatus {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 256)))]
    /// Driver identifier.
    pub id: String,
    /// Three-component numeric driver version, or `unknown`.
    pub version: String,
    /// Current driver lifecycle state.
    pub state: DriverState,
    /// Optional safe description of the driver's current error.
    pub error: Option<ApiError>,
}
impl DriverStatus {
    /// Validate the driver's identifier, version, and optional error.
    pub fn validate(&self) -> ValidationResult {
        // Failed catalogs identify their directory, including malformed package IDs.
        if self.state == DriverState::Error {
            bounded(&self.id, 256, false)?;
        } else {
            validate_driver_id(&self.id)?;
        }
        if self.version != "unknown" {
            validate_driver_version(&self.version)?;
        }
        validate_error(&self.error)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
/// JSON-compatible value of a characteristic observation or command.
pub enum CharacteristicValue {
    /// Boolean characteristic value.
    Boolean(
        /// Boolean value contents.
        bool,
    ),
    /// Finite numeric characteristic value.
    Number(
        /// Finite numeric value contents.
        f64,
    ),
    /// Bounded text characteristic value.
    String(
        /// Text value contents.
        #[cfg_attr(feature = "schema", schemars(length(max = 1024)))]
        String,
    ),
}
impl CharacteristicValue {
    /// Validate that numeric values are finite and text values are bounded.
    pub fn validate(&self) -> ValidationResult {
        match self {
            Self::Number(value) if !value.is_finite() => invalid("Number must be finite."),
            Self::String(value) => bounded(value, 1024, true),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// A permitted value and its user-facing label for an enum characteristic.
pub struct EnumOption {
    #[cfg_attr(feature = "schema", schemars(length(max = 1024)))]
    /// Serialized enum value.
    pub value: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    /// User-facing label for the value.
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
/// Metadata describing the shape and constraints of a characteristic value.
pub enum CharacteristicSpec {
    /// Numeric value with optional bounds, step, integer restriction, and unit.
    Number {
        /// Inclusive minimum value, when specified.
        minimum: Option<f64>,
        /// Inclusive maximum value, when specified.
        maximum: Option<f64>,
        /// Positive increment supported by step commands, when specified.
        step: Option<f64>,
        /// Whether values must be integral.
        integer: bool,
        #[cfg_attr(feature = "schema", schemars(length(max = 32)))]
        /// Optional unit label.
        unit: Option<String>,
    },
    /// Boolean value.
    Boolean,
    /// One value selected from a bounded set of options.
    Enum {
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 32)))]
        /// Available enum values and their labels.
        options: Vec<EnumOption>,
    },
    /// Text value with a maximum character count.
    String {
        #[cfg_attr(feature = "schema", schemars(range(max = 1024)))]
        /// Maximum number of Unicode scalar values accepted.
        max_length: u32,
    },
    /// Write-only action without a stored value.
    Action,
}
impl CharacteristicSpec {
    /// Validate the internal consistency and bounds of this metadata.
    pub fn validate(&self) -> ValidationResult {
        match self {
            Self::Number {
                minimum,
                maximum,
                step,
                integer,
                unit,
            } => {
                for value in [minimum, maximum, step].into_iter().flatten() {
                    if !value.is_finite() || (*integer && value.fract() != 0.0) {
                        return invalid(
                            "Numeric metadata must be finite and respect integer metadata.",
                        );
                    }
                }
                if matches!((minimum, maximum), (Some(min), Some(max)) if min > max) {
                    return invalid("Numeric minimum must not exceed maximum.");
                }
                if step.is_some_and(|step| step <= 0.0) {
                    return invalid("Numeric step must be positive.");
                }
                if let Some(unit) = unit {
                    bounded(unit, 32, true)?;
                }
                Ok(())
            }
            Self::Enum { options } => {
                if options.is_empty() || options.len() > 32 {
                    return invalid("Enum must contain one to 32 options.");
                }
                let mut seen = HashSet::with_capacity(options.len());
                for option in options {
                    bounded(&option.value, 1024, true)?;
                    validate_label(&option.label)?;
                    if !seen.insert(&option.value) {
                        return invalid("Enum option values must be unique.");
                    }
                }
                Ok(())
            }
            Self::String { max_length } if *max_length > 1024 => {
                invalid("String maximum length exceeds 1024 characters.")
            }
            _ => Ok(()),
        }
    }
    /// Validate a value against this metadata's type and constraints.
    pub fn validate_value(&self, value: &CharacteristicValue) -> ValidationResult {
        self.validate()?;
        value.validate()?;
        let conforms = match (self, value) {
            (
                Self::Number {
                    minimum,
                    maximum,
                    integer,
                    ..
                },
                CharacteristicValue::Number(value),
            ) => {
                (!integer || value.fract() == 0.0)
                    && minimum.is_none_or(|minimum| *value >= minimum)
                    && maximum.is_none_or(|maximum| *value <= maximum)
            }
            (Self::Boolean, CharacteristicValue::Boolean(_)) => true,
            (Self::Enum { options }, CharacteristicValue::String(value)) => {
                options.iter().any(|option| option.value == *value)
            }
            (Self::String { max_length }, CharacteristicValue::String(value)) => {
                value.chars().count() <= *max_length as usize
            }
            _ => false,
        };
        if conforms {
            Ok(())
        } else {
            invalid("Value does not conform to characteristic metadata.")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// Read, write, and notification permissions for a characteristic.
pub struct CharacteristicAccess {
    /// The current value may be observed.
    pub read: bool,
    /// A command may change the characteristic.
    pub write: bool,
    /// Updates may be pushed without polling.
    pub notify: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
/// Availability state of a characteristic observation.
pub enum CharacteristicStatus {
    /// The characteristic has a valid current observation.
    Ok,
    /// The characteristic is temporarily unavailable.
    Unavailable,
    /// The characteristic operation or observation failed.
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// Named presentation section for related characteristics of one device.
pub struct CharacteristicGroup {
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = DECLARATION_ID_PATTERN))
    )]
    /// Stable group identifier within the device.
    pub id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    /// User-facing group label.
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// A characteristic declaration, access policy, and optional current observation.
pub struct Characteristic {
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = DECLARATION_ID_PATTERN))
    )]
    /// Stable identifier within the device.
    pub id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    /// User-facing characteristic label.
    pub label: String,
    /// Identifier of the device group this characteristic belongs to, if the device declares groups.
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = DECLARATION_ID_PATTERN))
    )]
    pub group: Option<String>,
    /// Value type and metadata constraints.
    pub spec: CharacteristicSpec,
    /// Permissions granted for this characteristic.
    pub access: CharacteristicAccess,
    /// Most recently observed value, when one is available.
    pub value: Option<CharacteristicValue>,
    /// Current availability of the characteristic.
    pub status: CharacteristicStatus,
    /// Optional safe description of an error.
    pub error: Option<ApiError>,
}
impl Characteristic {
    /// Validate the declaration, permissions, observation, and optional error.
    pub fn validate(&self) -> ValidationResult {
        validate_id(&self.id)?;
        if let Some(group) = &self.group {
            validate_id(group)?;
        }
        validate_label(&self.label)?;
        self.spec.validate()?;
        validate_error(&self.error)?;
        if self.access.notify && !self.access.read {
            return invalid("Notify requires readable access.");
        }
        if matches!(self.spec, CharacteristicSpec::Action) {
            if !self.access.write || self.access.read || self.access.notify || self.value.is_some()
            {
                return invalid("Action requires write-only access and no stored value.");
            }
        } else if self.access.read && self.status == CharacteristicStatus::Ok {
            self.spec
                .validate_value(self.value.as_ref().ok_or_else(|| {
                    validation_error("Readable current characteristic requires an observed value.")
                })?)?;
        }
        // Stale observations can predate a changed hardware range. They remain bounded
        // and typed data but are not claimed to conform to the current numeric range.
        if let Some(value) = &self.value {
            value.validate()?;
        }
        Ok(())
    }
    /// Validate a command against this characteristic's permissions and metadata.
    pub fn validate_command(&self, command: &CharacteristicCommand) -> ValidationResult {
        self.validate()?;
        command.validate()?;
        if !self.access.write {
            return unsupported("Characteristic is read-only.");
        }
        match command {
            CharacteristicCommand::Set { value }
                if !matches!(self.spec, CharacteristicSpec::Action) =>
            {
                self.spec.validate_value(value)
            }
            CharacteristicCommand::Step { .. }
                if matches!(
                    self.spec,
                    CharacteristicSpec::Number { step: Some(_), .. }
                        | CharacteristicSpec::Enum { .. }
                ) =>
            {
                Ok(())
            }
            CharacteristicCommand::Toggle {}
                if matches!(self.spec, CharacteristicSpec::Boolean) =>
            {
                Ok(())
            }
            CharacteristicCommand::Invoke {} if matches!(self.spec, CharacteristicSpec::Action) => {
                Ok(())
            }
            _ => unsupported("Command is not supported by this characteristic."),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
/// Availability state of a device.
pub enum DeviceStatus {
    /// The device is available for operations.
    Online,
    /// The device cannot currently be used.
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// A device declaration and its characteristics.
pub struct Device {
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = DECLARATION_ID_PATTERN))
    )]
    /// Stable device identifier.
    pub id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = DRIVER_ID_PATTERN))
    )]
    /// Identifier of the driver that owns this device.
    pub driver: String,
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = DECLARATION_ID_PATTERN))
    )]
    /// Device-kind identifier.
    pub kind: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    /// User-facing device label.
    pub label: String,
    /// Current availability of the device.
    pub status: DeviceStatus,
    /// Optional safe description of a device-level error.
    pub error: Option<ApiError>,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    /// Ordered presentation groups; empty when the device is ungrouped.
    pub groups: Vec<CharacteristicGroup>,
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    /// Characteristics declared by the device.
    pub characteristics: Vec<Characteristic>,
}
impl Device {
    /// Validate identifiers, bounds, ownership data, and all characteristics.
    pub fn validate(&self) -> ValidationResult {
        validate_id(&self.id)?;
        validate_id(&self.kind)?;
        validate_driver_id(&self.driver)?;
        validate_label(&self.label)?;
        validate_error(&self.error)?;
        if self.characteristics.len() > MAX_CHARACTERISTICS_PER_DEVICE {
            return invalid("Device has too many characteristics.");
        }
        if self.groups.len() > MAX_GROUPS_PER_DEVICE {
            return invalid("Device has too many characteristic groups.");
        }
        let mut group_ids = HashSet::with_capacity(self.groups.len());
        for group in &self.groups {
            validate_id(&group.id)?;
            validate_label(&group.label)?;
            if !group_ids.insert(&group.id) {
                return invalid("Characteristic group IDs must be unique within a device.");
            }
        }
        let mut seen = HashSet::with_capacity(self.characteristics.len());
        let mut used_groups = HashSet::with_capacity(self.groups.len());
        for characteristic in &self.characteristics {
            characteristic.validate()?;
            if !seen.insert(&characteristic.id) {
                return invalid("Characteristic IDs must be unique within a device.");
            }
            match (&self.groups.is_empty(), &characteristic.group) {
                (true, Some(_)) => {
                    return invalid("Characteristic group requires declared device groups.");
                }
                (false, None) => {
                    return invalid("Every characteristic must reference a declared device group.");
                }
                (false, Some(group)) if !group_ids.contains(group) => {
                    return invalid("Every characteristic must reference a declared device group.");
                }
                (false, Some(group)) => {
                    used_groups.insert(group.as_str());
                }
                (true, None) => {}
            }
        }
        if !self.groups.is_empty() && used_groups.len() != self.groups.len() {
            return invalid("Characteristic groups must not be empty.");
        }
        Ok(())
    }

    /// Presentation sections in declared group order, each listing indices into `characteristics`
    /// in declaration order. An ungrouped device yields one `None` section, even when it has no characteristics.
    pub fn characteristic_sections(&self) -> Vec<(Option<&CharacteristicGroup>, Vec<usize>)> {
        if self.groups.is_empty() {
            return vec![(None, (0..self.characteristics.len()).collect())];
        }
        let mut sections = Vec::with_capacity(self.groups.len() + 1);
        for group in &self.groups {
            let indices = self
                .characteristics
                .iter()
                .enumerate()
                .filter_map(|(index, characteristic)| {
                    (characteristic.group.as_deref() == Some(group.id.as_str())).then_some(index)
                })
                .collect::<Vec<_>>();
            if !indices.is_empty() {
                sections.push((Some(group), indices));
            }
        }
        let rest = self
            .characteristics
            .iter()
            .enumerate()
            .filter_map(|(index, characteristic)| {
                let matched = self
                    .groups
                    .iter()
                    .any(|group| characteristic.group.as_deref() == Some(group.id.as_str()));
                (!matched).then_some(index)
            })
            .collect::<Vec<_>>();
        if !rest.is_empty() {
            sections.push((None, rest));
        }
        sections
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// Complete device and driver state published at a single revision.
pub struct Snapshot {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    /// Wire API version used by this snapshot.
    pub api_version: u8,
    /// Identity and display name of the node publishing the snapshot.
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECIMAL_SEQUENCE_PATTERN)))]
    /// Canonical unsigned decimal revision identifier.
    pub revision: String,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    /// Installed driver statuses represented by this snapshot.
    pub drivers: Vec<DriverStatus>,
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    /// Devices represented by this snapshot.
    pub devices: Vec<Device>,
}
impl Snapshot {
    /// Validate structure and enforce the maximum encoded snapshot size.
    pub fn validate(&self) -> ValidationResult {
        self.validate_structure()?;
        validate_encoded_size(self, MAX_SNAPSHOT_BYTES)
    }
    /// Validate fields, identifiers, ownership relationships, and collection limits.
    pub fn validate_structure(&self) -> ValidationResult {
        validate_api_version(self.api_version)?;
        self.node.validate()?;
        validate_revision(&self.revision)?;
        validate_drivers(&self.drivers)?;
        if self.devices.len() > MAX_DEVICES {
            return invalid("Snapshot has too many devices.");
        }
        let mut ids = HashSet::with_capacity(self.devices.len());
        let mut total = 0;
        for device in &self.devices {
            device.validate()?;
            if !ids.insert(&device.id) {
                return invalid("Device IDs must be unique.");
            }
            if !self.drivers.iter().any(|driver| driver.id == device.driver) {
                return invalid("Device owner must be an installed driver.");
            }
            total += device.characteristics.len();
        }
        if total > MAX_CHARACTERISTICS {
            return invalid("Snapshot has too many characteristics.");
        }
        Ok(())
    }
    /// Validate and serialize once for immutable state publication and WS fanout.
    pub fn validated_json(&self) -> Result<Vec<u8>, ApiError> {
        self.validate_structure()?;
        struct Buffer(Vec<u8>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > MAX_SNAPSHOT_BYTES - self.0.len() {
                    return Err(std::io::Error::other("snapshot size exceeded"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut buffer = Buffer(Vec::new());
        serde_json::to_writer(&mut buffer, self)
            .map_err(|_| validation_error("Snapshot cannot be encoded within 512 KiB."))?;
        Ok(buffer.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// Node and driver status returned by the status endpoint.
pub struct StatusResponse {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    /// Wire API version used by the response.
    pub api_version: u8,
    /// Identity and display name of the node.
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECIMAL_SEQUENCE_PATTERN)))]
    /// Canonical unsigned decimal revision identifier.
    pub revision: String,
    #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 4294967295_u64)))]
    /// Daemon uptime in whole seconds, saturated at the wire type's maximum.
    pub uptime_seconds: u32,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    /// Installed driver statuses.
    pub drivers: Vec<DriverStatus>,
}
impl StatusResponse {
    /// Validate the API version, node, revision, and driver statuses.
    pub fn validate(&self) -> ValidationResult {
        validate_api_version(self.api_version)?;
        self.node.validate()?;
        validate_revision(&self.revision)?;
        validate_drivers(&self.drivers)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// A device together with the node and revision that describe its state.
pub struct DeviceResponse {
    /// Identity and display name of the node.
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECIMAL_SEQUENCE_PATTERN)))]
    /// Canonical unsigned decimal revision identifier.
    pub revision: String,
    /// Device data at the indicated revision.
    pub device: Device,
}
impl DeviceResponse {
    /// Validate the node, revision, and device declaration.
    pub fn validate(&self) -> ValidationResult {
        self.node.validate()?;
        validate_revision(&self.revision)?;
        self.device.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
/// Write operation requested for a characteristic.
pub enum CharacteristicCommand {
    /// Set a characteristic to the supplied value.
    Set {
        /// Target value for the set operation.
        value: CharacteristicValue,
    },
    /// Move a numeric or enum characteristic by a relative number of steps.
    Step {
        #[cfg_attr(feature = "schema", schemars(range(min = -100, max = 100)))]
        /// Signed number of steps, constrained to the inclusive range -100 through 100.
        steps: i32,
    },
    /// Invert the current value of a boolean characteristic.
    Toggle {},
    /// Invoke a write-only action characteristic.
    Invoke {},
}
impl CharacteristicCommand {
    /// Validate operation-specific command fields.
    pub fn validate(&self) -> ValidationResult {
        match self {
            Self::Set { value } => value.validate(),
            Self::Step { steps } if !(-100..=100).contains(steps) => {
                invalid("Steps must be between -100 and 100.")
            }
            _ => Ok(()),
        }
    }
    /// Reject oversized/unknown-field commands before dispatch. No input is included in errors.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ApiError> {
        if bytes.len() > MAX_COMMAND_BYTES {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Command exceeds 16 KiB.",
            ));
        }
        let command: Self = serde_json::from_slice(bytes).map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidRequest,
                "Malformed characteristic command.",
            )
        })?;
        command.validate()?;
        Ok(command)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
/// Severity level for a log entry.
pub enum LogLevel {
    /// Diagnostic details useful during development.
    Debug,
    /// Informational event.
    Info,
    /// Warning that may require attention.
    Warn,
    /// Error event.
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// A recent daemon log entry with a stable sequence identifier.
pub struct LogEntry {
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECIMAL_SEQUENCE_PATTERN)))]
    /// Canonical unsigned decimal sequence identifier.
    pub id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(
                pattern = UTC_TIMESTAMP_PATTERN
            )
        )
    )]
    /// UTC RFC 3339 timestamp.
    pub timestamp: String,
    /// Severity of the log entry.
    pub level: LogLevel,
    /// Component or subsystem that emitted the entry.
    pub target: String,
    /// Bounded log message text.
    pub message: String,
}
impl LogEntry {
    /// Validate the log sequence and UTC timestamp.
    pub fn validate(&self) -> ValidationResult {
        validate_revision(&self.id)?;
        validate_timestamp(&self.timestamp)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
/// A batch of recent log entries associated with a node.
pub struct LogsResponse {
    /// Identity and display name of the node.
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(length(max = 200)))]
    /// Recent log entries, in sequence order.
    pub entries: Vec<LogEntry>,
}
impl LogsResponse {
    /// Validate the node and bounded, uniquely identified log entries.
    pub fn validate(&self) -> ValidationResult {
        self.node.validate()?;
        validate_logs(&self.entries)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
/// One characteristic observation that changed between consecutive published snapshots.
pub struct CharacteristicChange {
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECIMAL_SEQUENCE_PATTERN)))]
    /// Revision of the snapshot that contains the new observation.
    pub revision: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(pattern = UTC_TIMESTAMP_PATTERN)
        )
    )]
    /// UTC RFC 3339 time the daemon emitted the change.
    pub timestamp: String,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECLARATION_ID_PATTERN)))]
    /// Device identifier.
    pub device_id: String,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = DECLARATION_ID_PATTERN)))]
    /// Characteristic identifier.
    pub characteristic_id: String,
    /// New observed value, if any.
    pub value: Option<CharacteristicValue>,
    /// New availability status.
    pub status: CharacteristicStatus,
    /// New safe error, if any.
    pub error: Option<ApiError>,
}
impl CharacteristicChange {
    /// Validate the revision, time, identities, value, and optional error.
    pub fn validate(&self) -> ValidationResult {
        validate_revision(&self.revision)?;
        validate_timestamp(&self.timestamp)?;
        validate_id(&self.device_id)?;
        validate_id(&self.characteristic_id)?;
        if let Some(value) = &self.value {
            value.validate()?;
        }
        validate_error(&self.error)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
/// Event streamed by the server to an API client.
pub enum ServerEvent {
    /// A complete replacement snapshot.
    Snapshot {
        /// Snapshot contents.
        snapshot: Snapshot,
    },
    /// A batch of recent log entries.
    Logs {
        #[cfg_attr(feature = "schema", schemars(length(max = 200)))]
        /// Log entries included in the batch.
        entries: Vec<LogEntry>,
    },
    /// A single new log entry.
    Log {
        /// Log entry contents.
        entry: LogEntry,
    },
    /// A characteristic observation changed.
    CharacteristicChanged {
        /// Change event contents.
        change: CharacteristicChange,
    },
}
impl ServerEvent {
    /// Validate the data carried by this event.
    pub fn validate(&self) -> ValidationResult {
        match self {
            Self::Snapshot { snapshot } => snapshot.validate(),
            Self::Logs { entries } => validate_logs(entries),
            Self::Log { entry } => entry.validate(),
            Self::CharacteristicChanged { change } => change.validate(),
        }
    }
}

/// Validate that a peer's advertised API version is supported.
pub fn validate_api_version(version: u8) -> ValidationResult {
    if version == API_VERSION {
        Ok(())
    } else {
        invalid("Unsupported API version.")
    }
}
/// Validate a canonical lowercase RFC 4122 UUID version 4.
pub fn validate_uuid_v4(value: &str) -> ValidationResult {
    let id = uuid::Uuid::parse_str(value)
        .map_err(|_| validation_error("Node identity must be a canonical UUID-v4."))?;
    let mut canonical = [0; 36];
    if id.get_version_num() != 4
        || id.get_variant() != uuid::Variant::RFC4122
        || id.hyphenated().encode_lower(&mut canonical) != value
    {
        return invalid("Node identity must be a canonical UUID-v4.");
    }
    Ok(())
}
/// Parse a canonical unsigned decimal revision into its numeric value.
pub fn parse_revision(value: &str) -> Result<u64, ApiError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(validation_error(
            "Sequence must be canonical unsigned decimal text.",
        ));
    }
    value
        .parse()
        .map_err(|_| validation_error("Sequence exceeds u64."))
}
/// Validate a canonical unsigned decimal revision.
pub fn validate_revision(value: &str) -> ValidationResult {
    parse_revision(value).map(|_| ())
}
/// Validate a declaration identifier, allowing dots and underscores.
pub fn validate_id(value: &str) -> ValidationResult {
    validate_identifier(value, true)
}
/// Validate a driver identifier, which permits lowercase letters, digits, and hyphens.
pub fn validate_driver_id(value: &str) -> ValidationResult {
    validate_identifier(value, false)
}
fn validate_identifier(value: &str, dotted: bool) -> ValidationResult {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_lowercase()
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || *byte == b'-'
                || (dotted && matches!(*byte, b'_' | b'.'))
        })
    {
        return invalid("Invalid declaration identifier.");
    }
    Ok(())
}
/// Validate a driver version containing exactly three numeric components.
pub fn validate_driver_version(value: &str) -> ValidationResult {
    let mut parts = value.split('.');
    for _ in 0..3 {
        let Some(part) = parts.next() else {
            return invalid("Driver version must have three numeric parts.");
        };
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return invalid("Driver version must have three numeric parts.");
        }
    }
    if parts.next().is_some() {
        return invalid("Driver version must have three numeric parts.");
    }
    Ok(())
}
/// Validate a non-empty user-facing label of at most 128 characters.
pub fn validate_label(value: &str) -> ValidationResult {
    bounded(value, 128, false)
}
/// Validate a UTC RFC 3339 timestamp ending in `Z` or `+00:00`.
pub fn validate_timestamp(value: &str) -> ValidationResult {
    let timestamp =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|_| validation_error("Log timestamp must be UTC RFC3339."))?;
    if timestamp.offset() != time::UtcOffset::UTC
        || value.as_bytes().get(10) != Some(&b'T')
        || !(value.ends_with('Z') || value.ends_with("+00:00"))
    {
        return invalid("Log timestamp must be UTC RFC3339.");
    }
    Ok(())
}
/// Validate text against a caller-supplied maximum length of at most 1024 characters.
pub fn validate_string(value: &str, max_length: u32) -> ValidationResult {
    if max_length > 1024 {
        return invalid("String maximum length exceeds 1024 characters.");
    }
    bounded(value, max_length as usize, true)
}
fn bounded(value: &str, max: usize, empty: bool) -> ValidationResult {
    if (!empty && value.is_empty()) || value.chars().count() > max {
        return invalid("Text length is outside the permitted bounds.");
    }
    Ok(())
}
fn validate_error(error: &Option<ApiError>) -> ValidationResult {
    if let Some(error) = error {
        error.validate()?;
    }
    Ok(())
}
fn validate_drivers(drivers: &[DriverStatus]) -> ValidationResult {
    if drivers.len() > MAX_DRIVERS {
        return invalid("Too many installed drivers.");
    }
    let mut ids = HashSet::with_capacity(drivers.len());
    for driver in drivers {
        driver.validate()?;
        if !ids.insert(&driver.id) {
            return invalid("Driver IDs must be unique.");
        }
    }
    Ok(())
}
fn validate_logs(entries: &[LogEntry]) -> ValidationResult {
    if entries.len() > MAX_LOG_ENTRIES {
        return invalid("Too many recent log entries.");
    }
    let mut ids = HashSet::with_capacity(entries.len());
    for entry in entries {
        entry.validate()?;
        if !ids.insert(&entry.id) {
            return invalid("Recent log IDs must be unique.");
        }
    }
    Ok(())
}
/// Measure encoded bytes without allocating a duplicate JSON buffer.
pub fn validate_encoded_size(value: &impl Serialize, maximum: usize) -> ValidationResult {
    struct Counter {
        bytes: usize,
        maximum: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > self.maximum {
                return Err(std::io::Error::other("encoded size exceeded"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { bytes: 0, maximum }, value)
        .map_err(|_| validation_error("Encoded data exceeds permitted size or cannot be encoded."))
}
fn validation_error(message: &'static str) -> ApiError {
    ApiError::new(ErrorCode::InvalidValue, message)
}
fn invalid<T>(message: &'static str) -> Result<T, ApiError> {
    Err(validation_error(message))
}
fn unsupported(message: &'static str) -> ValidationResult {
    Err(ApiError::new(ErrorCode::UnsupportedOperation, message))
}

/// Deterministic helpers for generating self-contained draft-07 wire schemas.
#[cfg(feature = "schema")]
pub mod schema {
    use super::*;
    use schemars::{JsonSchema, generate::SchemaSettings};

    fn root<T: JsonSchema>(input: bool) -> serde_json::Value {
        // Rust-width numeric formats are annotations, not draft07 validation.
        // Keep their generated minimum/maximum and ordinary numeric types intact.
        let settings = SchemaSettings::draft07().with_transform(
            schemars::transform::RecursiveTransform(|schema: &mut schemars::Schema| {
                if matches!(
                    schema.get("format").and_then(serde_json::Value::as_str),
                    Some("uint8" | "uint32" | "int32" | "double")
                ) {
                    schema.remove("format");
                }
            }),
        );
        let settings = if input {
            settings.for_deserialize()
        } else {
            settings.for_serialize()
        };
        serde_json::to_value(settings.into_generator().into_root_schema_for::<T>())
            .expect("JSON Schema consists only of serializable JSON values")
    }

    /// Generate the named response and command schemas used by the API.
    pub fn roots() -> [(&'static str, serde_json::Value); 9] {
        [
            ("Snapshot", root::<Snapshot>(false)),
            ("StatusResponse", root::<StatusResponse>(false)),
            ("DeviceResponse", root::<DeviceResponse>(false)),
            ("LogsResponse", root::<LogsResponse>(false)),
            ("Characteristic", root::<Characteristic>(false)),
            ("CharacteristicCommand", root::<CharacteristicCommand>(true)),
            ("ServerEvent", root::<ServerEvent>(false)),
            ("ErrorResponse", root::<ErrorResponse>(false)),
            ("InventoryResponse", root::<InventoryResponse>(false)),
        ]
    }
}
