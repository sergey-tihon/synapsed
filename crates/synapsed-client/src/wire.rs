//! Required nullable fields need an explicit presence check: Serde's Option alone
//! accepts omitted fields, whereas the public generated response schemas do not.
use crate::ClientError;
use serde::de::DeserializeOwned;
use serde_json::Value;
use synapsed_api::*;

type Result<T, E = ClientError> = std::result::Result<T, E>;
pub(crate) trait Wire: DeserializeOwned {
    fn shape(value: &Value) -> Result<()>;
    fn validate_wire(&self) -> ValidationResult;
}
pub(crate) fn decode<T: Wire>(bytes: &[u8]) -> Result<T> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| ClientError::Protocol("Malformed JSON response."))?;
    T::shape(&value)?;
    let result: T = serde_json::from_value(value)
        .map_err(|_| ClientError::Protocol("Incompatible response fields."))?;
    result
        .validate_wire()
        .map_err(|_| ClientError::Protocol("Invalid response semantics."))?;
    Ok(result)
}
fn fields<'a>(value: &'a Value, names: &[&str]) -> Result<&'a serde_json::Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or(ClientError::Protocol("Expected a response object."))?;
    if names.iter().any(|name| !object.contains_key(*name)) {
        return Err(ClientError::Protocol("Missing required response field."));
    }
    Ok(object)
}
fn array(value: &Value, each: fn(&Value) -> Result<()>) -> Result<()> {
    for item in value
        .as_array()
        .ok_or(ClientError::Protocol("Expected a response array."))?
    {
        each(item)?;
    }
    Ok(())
}
fn error(value: &Value) -> Result<()> {
    fields(value, &["code", "message"]).map(|_| ())
}
fn nullable_error(value: &Value) -> Result<()> {
    if value.is_null() {
        Ok(())
    } else {
        error(value)
    }
}
fn node(value: &Value) -> Result<()> {
    fields(value, &["nodeId", "instanceId", "name"]).map(|_| ())
}
fn driver(value: &Value) -> Result<()> {
    let value = fields(value, &["id", "version", "state", "error"])?;
    nullable_error(&value["error"])
}
fn characteristic(value: &Value) -> Result<()> {
    let value = fields(
        value,
        &["id", "label", "spec", "access", "value", "status", "error"],
    )?;
    nullable_error(&value["error"])?;
    fields(&value["access"], &["read", "write", "notify"])?;
    let spec = fields(&value["spec"], &["kind"])?;
    match spec["kind"].as_str() {
        Some("number") => {
            fields(
                &value["spec"],
                &["minimum", "maximum", "step", "integer", "unit"],
            )?;
        }
        Some("enum") => array(
            spec.get("options")
                .ok_or(ClientError::Protocol("Missing enum options."))?,
            |value| fields(value, &["value", "label"]).map(|_| ()),
        )?,
        Some("string") => {
            fields(&value["spec"], &["maxLength"])?;
        }
        Some("boolean" | "action") => {}
        _ => return Err(ClientError::Protocol("Unsupported characteristic kind.")),
    }
    Ok(())
}
fn device(value: &Value) -> Result<()> {
    let value = fields(
        value,
        &[
            "id",
            "driver",
            "kind",
            "label",
            "status",
            "error",
            "characteristics",
        ],
    )?;
    nullable_error(&value["error"])?;
    array(&value["characteristics"], characteristic)
}
fn snapshot(value: &Value) -> Result<()> {
    let value = fields(
        value,
        &["apiVersion", "node", "revision", "drivers", "devices"],
    )?;
    node(&value["node"])?;
    array(&value["drivers"], driver)?;
    array(&value["devices"], device)
}
fn log(value: &Value) -> Result<()> {
    fields(value, &["id", "timestamp", "level", "target", "message"]).map(|_| ())
}
macro_rules! wire {
    ($type:ty, $shape:expr) => {
        impl Wire for $type {
            fn shape(value: &Value) -> Result<()> {
                ($shape)(value)
            }
            fn validate_wire(&self) -> ValidationResult {
                self.validate()
            }
        }
    };
}
wire!(Snapshot, snapshot);
wire!(ErrorResponse, |value: &Value| {
    let value = fields(value, &["error"])?;
    error(&value["error"])
});
wire!(StatusResponse, |value: &Value| {
    let value = fields(
        value,
        &["apiVersion", "node", "revision", "uptimeSeconds", "drivers"],
    )?;
    node(&value["node"])?;
    array(&value["drivers"], driver)
});
wire!(DeviceResponse, |value: &Value| {
    let value = fields(value, &["node", "revision", "device"])?;
    node(&value["node"])?;
    device(&value["device"])
});
wire!(LogsResponse, |value: &Value| {
    let value = fields(value, &["node", "entries"])?;
    node(&value["node"])?;
    array(&value["entries"], log)
});
wire!(ServerEvent, |value: &Value| {
    let object = fields(value, &["type"])?;
    match object["type"].as_str() {
        Some("snapshot") => snapshot(
            object
                .get("snapshot")
                .ok_or(ClientError::Protocol("Missing event snapshot."))?,
        ),
        Some("logs") => array(
            object
                .get("entries")
                .ok_or(ClientError::Protocol("Missing event logs."))?,
            log,
        ),
        Some("log") => log(object
            .get("entry")
            .ok_or(ClientError::Protocol("Missing event log."))?),
        _ => Err(ClientError::Protocol("Unsupported server event.")),
    }
});
