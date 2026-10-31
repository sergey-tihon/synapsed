//! Device-neutral local API wire data and shared semantic validation.
//! Deserialization is deliberately separate from semantic validation: callers must
//! validate received declarations, commands and observations before using them.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const API_VERSION: u8 = 1;
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
pub const MAX_DEVICES: usize = 64;
pub const MAX_CHARACTERISTICS: usize = 512;
pub const MAX_CHARACTERISTICS_PER_DEVICE: usize = 64;
pub const MAX_DRIVERS: usize = 16;
pub const MAX_LOG_ENTRIES: usize = 200;
pub const DECIMAL_SEQUENCE_PATTERN: &str = "^(0|[1-9][0-9]*)$";
pub const DECLARATION_ID_PATTERN: &str = "^[a-z][a-z0-9_.-]{0,63}$";
pub const DRIVER_ID_PATTERN: &str = "^[a-z][a-z0-9-]{0,63}$";
pub type ValidationResult = Result<(), ApiError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    ForbiddenOrigin,
    ForbiddenHost,
    UnknownDevice,
    UnknownCharacteristic,
    AmbiguousDevice,
    DriverConflict,
    UnsupportedOperation,
    InvalidValue,
    PermissionDenied,
    DeviceUnavailable,
    DdcError,
    PluginError,
    UnsupportedCapability,
    Busy,
    Timeout,
}
impl ErrorCode {
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
pub struct ApiError {
    pub code: ErrorCode,
    #[cfg_attr(feature = "schema", schemars(length(max = 256)))]
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
pub struct ErrorResponse {
    pub error: ApiError,
}
impl ErrorResponse {
    pub fn validate(&self) -> ValidationResult {
        self.error.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(
                pattern = "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
            )
        )
    )]
    pub node_id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(
                pattern = "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
            )
        )
    )]
    pub instance_id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    pub name: String,
}
impl NodeInfo {
    pub fn validate(&self) -> ValidationResult {
        validate_uuid_v4(&self.node_id)?;
        validate_uuid_v4(&self.instance_id)?;
        validate_label(&self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DriverState {
    Disabled,
    Online,
    Disconnected,
    Ambiguous,
    PermissionDenied,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DriverStatus {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 256)))]
    pub id: String,
    pub version: String,
    pub state: DriverState,
    pub error: Option<ApiError>,
}
impl DriverStatus {
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
pub enum CharacteristicValue {
    Boolean(bool),
    Number(f64),
    String(#[cfg_attr(feature = "schema", schemars(length(max = 1024)))] String),
}
impl CharacteristicValue {
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
pub struct EnumOption {
    #[cfg_attr(feature = "schema", schemars(length(max = 1024)))]
    pub value: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CharacteristicSpec {
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
        step: Option<f64>,
        integer: bool,
        #[cfg_attr(feature = "schema", schemars(length(max = 32)))]
        unit: Option<String>,
    },
    Boolean,
    Enum {
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 32)))]
        options: Vec<EnumOption>,
    },
    String {
        #[cfg_attr(feature = "schema", schemars(range(max = 1024)))]
        max_length: u32,
    },
    Action,
}
impl CharacteristicSpec {
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
pub struct CharacteristicAccess {
    pub read: bool,
    pub write: bool,
    pub notify: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CharacteristicStatus {
    Ok,
    Unavailable,
    Error,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Characteristic {
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[a-z][a-z0-9_.-]{0,63}$"))
    )]
    pub id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    pub label: String,
    pub spec: CharacteristicSpec,
    pub access: CharacteristicAccess,
    pub value: Option<CharacteristicValue>,
    pub status: CharacteristicStatus,
    pub error: Option<ApiError>,
}
impl Characteristic {
    pub fn validate(&self) -> ValidationResult {
        validate_id(&self.id)?;
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
pub enum DeviceStatus {
    Online,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Device {
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[a-z][a-z0-9_.-]{0,63}$"))
    )]
    pub id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[a-z][a-z0-9-]{0,63}$"))
    )]
    pub driver: String,
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[a-z][a-z0-9_.-]{0,63}$"))
    )]
    pub kind: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    pub label: String,
    pub status: DeviceStatus,
    pub error: Option<ApiError>,
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    pub characteristics: Vec<Characteristic>,
}
impl Device {
    pub fn validate(&self) -> ValidationResult {
        validate_id(&self.id)?;
        validate_id(&self.kind)?;
        validate_driver_id(&self.driver)?;
        validate_label(&self.label)?;
        validate_error(&self.error)?;
        if self.characteristics.len() > MAX_CHARACTERISTICS_PER_DEVICE {
            return invalid("Device has too many characteristics.");
        }
        let mut seen = HashSet::with_capacity(self.characteristics.len());
        for characteristic in &self.characteristics {
            characteristic.validate()?;
            if !seen.insert(&characteristic.id) {
                return invalid("Characteristic IDs must be unique within a device.");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    pub api_version: u8,
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^(0|[1-9][0-9]*)$")))]
    pub revision: String,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub drivers: Vec<DriverStatus>,
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    pub devices: Vec<Device>,
}
impl Snapshot {
    pub fn validate(&self) -> ValidationResult {
        self.validate_structure()?;
        validate_encoded_size(self, MAX_SNAPSHOT_BYTES)
    }
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
pub struct StatusResponse {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    pub api_version: u8,
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^(0|[1-9][0-9]*)$")))]
    pub revision: String,
    #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 4294967295_u64)))]
    pub uptime_seconds: u32,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub drivers: Vec<DriverStatus>,
}
impl StatusResponse {
    pub fn validate(&self) -> ValidationResult {
        validate_api_version(self.api_version)?;
        self.node.validate()?;
        validate_revision(&self.revision)?;
        validate_drivers(&self.drivers)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeviceResponse {
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^(0|[1-9][0-9]*)$")))]
    pub revision: String,
    pub device: Device,
}
impl DeviceResponse {
    pub fn validate(&self) -> ValidationResult {
        self.node.validate()?;
        validate_revision(&self.revision)?;
        self.device.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum CharacteristicCommand {
    Set {
        value: CharacteristicValue,
    },
    Step {
        #[cfg_attr(feature = "schema", schemars(range(min = -100, max = 100)))]
        steps: i32,
    },
    Toggle {},
    Invoke {},
}
impl CharacteristicCommand {
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
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogEntry {
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^(0|[1-9][0-9]*)$")))]
    pub id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "String",
            regex(
                pattern = "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?(Z|\\+00:00)$"
            )
        )
    )]
    pub timestamp: String,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}
impl LogEntry {
    pub fn validate(&self) -> ValidationResult {
        validate_revision(&self.id)?;
        validate_timestamp(&self.timestamp)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogsResponse {
    pub node: NodeInfo,
    #[cfg_attr(feature = "schema", schemars(length(max = 200)))]
    pub entries: Vec<LogEntry>,
}
impl LogsResponse {
    pub fn validate(&self) -> ValidationResult {
        self.node.validate()?;
        validate_logs(&self.entries)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Snapshot {
        snapshot: Snapshot,
    },
    Logs {
        #[cfg_attr(feature = "schema", schemars(length(max = 200)))]
        entries: Vec<LogEntry>,
    },
    Log {
        entry: LogEntry,
    },
}
impl ServerEvent {
    pub fn validate(&self) -> ValidationResult {
        match self {
            Self::Snapshot { snapshot } => snapshot.validate(),
            Self::Logs { entries } => validate_logs(entries),
            Self::Log { entry } => entry.validate(),
        }
    }
}

pub fn validate_api_version(version: u8) -> ValidationResult {
    if version == API_VERSION {
        Ok(())
    } else {
        invalid("Unsupported API version.")
    }
}
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
pub fn validate_revision(value: &str) -> ValidationResult {
    parse_revision(value).map(|_| ())
}
pub fn validate_id(value: &str) -> ValidationResult {
    validate_identifier(value, true)
}
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
pub fn validate_label(value: &str) -> ValidationResult {
    bounded(value, 128, false)
}
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

/// Deterministic, self-contained draft-07 wire roots. No files are written by derives.
#[cfg(feature = "schema")]
pub mod schema {
    use super::*;
    use schemars::{JsonSchema, generate::SchemaSettings};

    pub const ROOT_NAMES: [&str; 8] = [
        "Snapshot",
        "StatusResponse",
        "DeviceResponse",
        "LogsResponse",
        "Characteristic",
        "CharacteristicCommand",
        "ServerEvent",
        "ErrorResponse",
    ];

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

    pub fn roots() -> [(&'static str, serde_json::Value); 8] {
        [
            ("Snapshot", root::<Snapshot>(false)),
            ("StatusResponse", root::<StatusResponse>(false)),
            ("DeviceResponse", root::<DeviceResponse>(false)),
            ("LogsResponse", root::<LogsResponse>(false)),
            ("Characteristic", root::<Characteristic>(false)),
            ("CharacteristicCommand", root::<CharacteristicCommand>(true)),
            ("ServerEvent", root::<ServerEvent>(false)),
            ("ErrorResponse", root::<ErrorResponse>(false)),
        ]
    }
}
