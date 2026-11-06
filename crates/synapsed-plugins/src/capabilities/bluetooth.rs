use crate::{
    capabilities::endpoint::Endpoint,
    registry::{CapabilityEventStream, LuaCapabilityFactory},
    runtime::{InvocationContext, lua_error},
};
use mlua::{AnyUserData, Lua, LuaSerdeExt, Table, Value};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use synapsed_bluetooth::{
    BluetoothClient, EndpointScan, FinalWrite, LazyBluetooth, RepeatSpec, ScanFilter, Uuid,
};
use synapsed_core::{DriverError, EndpointToken, ErrorCode};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Permissions {
    scan: ScanPermissions,
    characteristics: Vec<CharacteristicPermission>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScanPermissions {
    services: Vec<String>,
    name_prefixes: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CharacteristicPermission {
    id: String,
    uuid: String,
    access: Vec<Access>,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Access {
    Read,
    Write,
    Notify,
}
#[derive(Clone)]
struct ParsedPermissions {
    scan: ScanFilter,
    characteristics: Vec<ParsedCharacteristic>,
    all_codes: BTreeSet<u8>,
    reads: BTreeSet<u8>,
    writes: BTreeSet<u8>,
    notifies: BTreeSet<u8>,
}
#[derive(Clone)]
struct ParsedCharacteristic {
    id: String,
    uuid: Uuid,
    access: BTreeSet<Access>,
}
impl Permissions {
    fn parse(value: &serde_json::Value) -> Result<ParsedPermissions, DriverError> {
        let permissions: Self = serde_json::from_value(value.clone()).map_err(|_| denied())?;
        let service_count = permissions.scan.services.len();
        let prefix_count = permissions.scan.name_prefixes.len();
        if service_count > 8
            || prefix_count > 8
            || (service_count == 0 && prefix_count == 0)
            || permissions
                .scan
                .name_prefixes
                .iter()
                .any(|prefix| !(1..=64).contains(&prefix.chars().count()))
            || !(1..=32).contains(&permissions.characteristics.len())
        {
            return Err(denied());
        }

        let mut services = Vec::with_capacity(service_count);
        let mut service_ids = BTreeSet::new();
        for service in permissions.scan.services {
            let uuid = Uuid::parse_str(&service).map_err(|_| denied())?;
            if !service_ids.insert(uuid) {
                return Err(denied());
            }
            services.push(uuid);
        }

        let mut characteristics = Vec::with_capacity(permissions.characteristics.len());
        let mut ids = BTreeSet::new();
        let mut uuids = BTreeSet::new();
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        let mut notifies = BTreeSet::new();
        for (index, characteristic) in permissions.characteristics.into_iter().enumerate() {
            if !valid_id(&characteristic.id) || !ids.insert(characteristic.id.clone()) {
                return Err(denied());
            }
            let uuid = Uuid::parse_str(&characteristic.uuid).map_err(|_| denied())?;
            if !uuids.insert(uuid) || characteristic.access.is_empty() {
                return Err(denied());
            }
            let access_count = characteristic.access.len();
            let access: BTreeSet<_> = characteristic.access.into_iter().collect();
            if access.len() != access_count {
                return Err(denied());
            }
            let code = index as u8;
            if access.contains(&Access::Read) {
                reads.insert(code);
            }
            if access.contains(&Access::Write) {
                writes.insert(code);
            }
            if access.contains(&Access::Notify) {
                notifies.insert(code);
            }
            characteristics.push(ParsedCharacteristic {
                id: characteristic.id,
                uuid,
                access,
            });
        }
        let all_codes = (0..characteristics.len())
            .map(|index| index as u8)
            .collect();
        Ok(ParsedPermissions {
            scan: ScanFilter {
                services,
                name_prefixes: permissions.scan.name_prefixes,
            },
            characteristics,
            all_codes,
            reads,
            writes,
            notifies,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    characteristic: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepeatOptions {
    interval_ms: u32,
    max_duration_ms: u32,
    #[serde(default)]
    finally: Vec<FinalOption>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalOption {
    characteristic: String,
    bytes: Vec<u8>,
}

/// Bluetooth Low Energy capability factory.
pub struct BluetoothCapabilityFactory {
    client: LazyBluetooth,
}
impl BluetoothCapabilityFactory {
    /// Create a factory around an already-started Bluetooth client.
    pub fn new(client: BluetoothClient) -> Self {
        Self::with_shared(LazyBluetooth::with_client(client))
    }

    /// Create a factory that starts a native Bluetooth owner only when invoked.
    pub fn lazy_native() -> Self {
        Self::with_shared(LazyBluetooth::new())
    }

    /// Create a factory that shares the supplied lazy client.
    pub fn with_shared(client: LazyBluetooth) -> Self {
        Self { client }
    }

    fn resolve_client(source: &LazyBluetooth) -> Result<BluetoothClient, DriverError> {
        source.get_or_start_native()
    }
}

impl LuaCapabilityFactory for BluetoothCapabilityFactory {
    fn id(&self) -> &'static str {
        "bluetooth"
    }

    fn version(&self) -> u32 {
        1
    }

    fn validate_permissions(&self, permissions: &serde_json::Value) -> Result<(), DriverError> {
        Permissions::parse(permissions).map(|_| ())
    }

    fn read_permissions(
        &self,
        permissions: &serde_json::Value,
    ) -> Result<BTreeSet<u8>, DriverError> {
        Ok(Permissions::parse(permissions)?.all_codes)
    }

    fn validate_bindings(
        &self,
        permissions: &serde_json::Value,
        bindings: &serde_json::Value,
    ) -> Result<BTreeMap<u8, u16>, DriverError> {
        let permissions = Permissions::parse(permissions)?;
        let bindings: Vec<Binding> =
            serde_json::from_value(bindings.clone()).map_err(|_| denied())?;
        let mut result = BTreeMap::new();
        for binding in bindings {
            let Some((index, characteristic)) =
                characteristic(&permissions, &binding.characteristic)
            else {
                return Err(denied());
            };
            if !characteristic.access.contains(&Access::Write)
                || result.insert(index, 0xffff).is_some()
            {
                return Err(denied());
            }
        }
        Ok(result)
    }

    fn install(&self, lua: &Lua, invocation: InvocationContext) -> mlua::Result<Table> {
        let permissions =
            Arc::new(Permissions::parse(&invocation.permissions).map_err(mlua::Error::external)?);
        let table = lua.create_table()?;
        let enumerate_client = self.client.clone();
        let enumerate_permissions = permissions.clone();
        let enumerate_context = invocation.clone();
        table.raw_set(
            "enumerate",
            lua.create_async_function(move |lua, _: Table| {
                let client = enumerate_client.clone();
                let permissions = enumerate_permissions.clone();
                let context = enumerate_context.clone();
                async move {
                    if let Err(error) = context.check() {
                        return pair_error(&lua, error);
                    }
                    let client = match Self::resolve_client(&client) {
                        Ok(client) => client,
                        Err(error) => return pair_error(&lua, error),
                    };
                    match client.enumerate(permissions.scan.clone()).await {
                        Ok(inventory) => {
                            context.check().map_err(mlua::Error::external)?;
                            let endpoints = endpoint_table(&lua, &context, inventory)?;
                            Ok((Value::Table(endpoints), Value::Nil))
                        }
                        Err(error) => pair_error(&lua, error),
                    }
                }
            })?,
        )?;

        let connect_client = self.client.clone();
        let connect_context = invocation.clone();
        table.raw_set(
            "connect",
            lua.create_async_function(move |lua, (_, endpoint): (Table, AnyUserData)| {
                let client = connect_client.clone();
                let context = connect_context.clone();
                let endpoint = context.resolve_endpoint(&endpoint);
                async move {
                    let endpoint = match endpoint {
                        Ok(endpoint) => endpoint,
                        Err(error) => return pair_error(&lua, error),
                    };
                    let client = match Self::resolve_client(&client) {
                        Ok(client) => client,
                        Err(error) => return pair_error(&lua, error),
                    };
                    let result = client.connect(endpoint).await;
                    context.check().map_err(mlua::Error::external)?;
                    match result {
                        Ok(ready) => Ok((Value::Boolean(ready), Value::Nil)),
                        Err(error) => pair_error(&lua, error),
                    }
                }
            })?,
        )?;

        let read_client = self.client.clone();
        let read_permissions = permissions.clone();
        let read_context = invocation.clone();
        table.raw_set(
            "read",
            lua.create_async_function(
                move |lua, (_, endpoint, id): (Table, AnyUserData, String)| {
                    let client = read_client.clone();
                    let permissions = read_permissions.clone();
                    let context = read_context.clone();
                    let endpoint_data = endpoint
                        .borrow::<Endpoint>()
                        .map(|endpoint| (endpoint.descriptor.token, endpoint.owner.clone()));
                    async move {
                        let (token, owner) = endpoint_data?;
                        if let Err(error) = context.resolve_endpoint(&endpoint) {
                            return pair_error(&lua, error);
                        }
                        let (code, uuid) = match validate_characteristic(
                            &context,
                            &owner,
                            token,
                            characteristic(&permissions, &id),
                            false,
                            &permissions.reads,
                        ) {
                            Ok(value) => value,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let client = match Self::resolve_client(&client) {
                            Ok(client) => client,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let operation = context.native_context(&permissions.all_codes);
                        let result = client.read(operation.clone(), token, code, uuid).await;
                        if context.operation.is_none() {
                            operation.finish();
                        }
                        context.check().map_err(mlua::Error::external)?;
                        match result {
                            Ok(bytes) => Ok((lua.to_value(&bytes)?, Value::Nil)),
                            Err(error) => pair_error(&lua, error),
                        }
                    }
                },
            )?,
        )?;

        let write_client = self.client.clone();
        let write_permissions = permissions.clone();
        let write_context = invocation.clone();
        table.raw_set(
            "write",
            lua.create_async_function(
                move |lua, (_, endpoint, id, bytes): (Table, AnyUserData, String, Vec<u8>)| {
                    let client = write_client.clone();
                    let permissions = write_permissions.clone();
                    let context = write_context.clone();
                    let endpoint_data = endpoint
                        .borrow::<Endpoint>()
                        .map(|endpoint| (endpoint.descriptor.token, endpoint.owner.clone()));
                    async move {
                        let (token, owner) = endpoint_data?;
                        if let Err(error) = context.resolve_endpoint(&endpoint) {
                            return pair_error(&lua, error);
                        }
                        if !(1..=512).contains(&bytes.len()) {
                            return pair_error(&lua, payload_error());
                        }
                        let (code, uuid) = match validate_characteristic(
                            &context,
                            &owner,
                            token,
                            characteristic(&permissions, &id),
                            true,
                            &permissions.writes,
                        ) {
                            Ok(value) => value,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let Some(operation) = context.operation.clone() else {
                            return pair_error(&lua, denied());
                        };
                        let client = match Self::resolve_client(&client) {
                            Ok(client) => client,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let result = client.write(operation, token, code, uuid, bytes).await;
                        context.check().map_err(mlua::Error::external)?;
                        match result {
                            Ok(()) => Ok((Value::Boolean(true), Value::Nil)),
                            Err(error) => {
                                if error.code == ErrorCode::PermissionDenied {
                                    context.guard.trip();
                                }
                                pair_error(&lua, error)
                            }
                        }
                    }
                },
            )?,
        )?;

        let repeat_client = self.client.clone();
        let repeat_permissions = permissions.clone();
        let repeat_context = invocation.clone();
        table.raw_set(
            "start_repeat",
            lua.create_async_function(
                move |lua,
                      (_, endpoint, id, bytes, options): (
                    Table,
                    AnyUserData,
                    String,
                    Vec<u8>,
                    Value,
                )| {
                    let client = repeat_client.clone();
                    let permissions = repeat_permissions.clone();
                    let context = repeat_context.clone();
                    let endpoint_data = endpoint
                        .borrow::<Endpoint>()
                        .map(|endpoint| (endpoint.descriptor.token, endpoint.owner.clone()));
                    let options = lua.from_value::<RepeatOptions>(options);
                    async move {
                        let (token, owner) = endpoint_data?;
                        if let Err(error) = context.resolve_endpoint(&endpoint) {
                            return pair_error(&lua, error);
                        }
                        if !(1..=512).contains(&bytes.len()) {
                            return pair_error(&lua, payload_error());
                        }
                        let options = match options {
                            Ok(options)
                                if (100..=1000).contains(&options.interval_ms)
                                    && (1000..=60_000).contains(&options.max_duration_ms)
                                    && options.finally.len() <= 4
                                    && options.finally.iter().all(|final_write| {
                                        (1..=512).contains(&final_write.bytes.len())
                                    }) =>
                            {
                                options
                            }
                            _ => return pair_error(&lua, repeat_options_error()),
                        };
                        let (code, uuid) = match validate_characteristic(
                            &context,
                            &owner,
                            token,
                            characteristic(&permissions, &id),
                            true,
                            &permissions.writes,
                        ) {
                            Ok(value) => value,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let mut finally = Vec::with_capacity(options.finally.len());
                        for final_option in options.finally {
                            let (final_code, final_uuid) = match validate_characteristic(
                                &context,
                                &owner,
                                token,
                                characteristic(&permissions, &final_option.characteristic),
                                true,
                                &permissions.writes,
                            ) {
                                Ok(value) => value,
                                Err(error) => return pair_error(&lua, error),
                            };
                            finally.push(FinalWrite {
                                code: final_code,
                                characteristic: final_uuid,
                                bytes: final_option.bytes,
                            });
                        }
                        let Some(operation) = context.operation.clone() else {
                            return pair_error(&lua, denied());
                        };
                        let client = match Self::resolve_client(&client) {
                            Ok(client) => client,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let result = client
                            .start_repeat(
                                operation,
                                token,
                                RepeatSpec {
                                    code,
                                    characteristic: uuid,
                                    bytes,
                                    interval: Duration::from_millis(u64::from(options.interval_ms)),
                                    max_duration: Duration::from_millis(u64::from(
                                        options.max_duration_ms,
                                    )),
                                    finally,
                                },
                            )
                            .await;
                        context.check().map_err(mlua::Error::external)?;
                        match result {
                            Ok(()) => Ok((Value::Boolean(true), Value::Nil)),
                            Err(error) => {
                                if error.code == ErrorCode::PermissionDenied {
                                    context.guard.trip();
                                }
                                pair_error(&lua, error)
                            }
                        }
                    }
                },
            )?,
        )?;

        let stop_client = self.client.clone();
        let stop_context = invocation;
        table.raw_set(
            "stop_repeat",
            lua.create_async_function(move |lua, (_, endpoint): (Table, AnyUserData)| {
                let client = stop_client.clone();
                let context = stop_context.clone();
                async move {
                    let token = match context.resolve_endpoint(&endpoint) {
                        Ok(token) => token,
                        Err(error) => return pair_error(&lua, error),
                    };
                    let Some(operation) = context.operation.clone() else {
                        context.guard.trip();
                        return pair_error(&lua, denied());
                    };
                    let client = match Self::resolve_client(&client) {
                        Ok(client) => client,
                        Err(error) => return pair_error(&lua, error),
                    };
                    let result = client.stop_repeat(operation, token).await;
                    context.check().map_err(mlua::Error::external)?;
                    match result {
                        Ok(stopped) => Ok((Value::Boolean(stopped), Value::Nil)),
                        Err(error) => pair_error(&lua, error),
                    }
                }
            })?,
        )?;
        Ok(table)
    }

    fn subscribe_events(
        &self,
        invocation: InvocationContext,
        endpoint: EndpointToken,
        event: &str,
    ) -> Result<CapabilityEventStream, DriverError> {
        let permissions = Permissions::parse(&invocation.permissions)?;
        let Some((code, characteristic)) = characteristic(&permissions, event) else {
            return Err(unsupported_event());
        };
        if !characteristic.access.contains(&Access::Notify) {
            return Err(unsupported_event());
        }
        invocation.validate_native(
            &invocation.owner,
            endpoint,
            code,
            false,
            &permissions.notifies,
        )?;
        let client = self.client.existing().ok_or_else(unavailable)?;
        let operation = invocation.operation.clone().ok_or_else(denied)?;
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let cancelled = Arc::new(AtomicBool::new(false));
        client.subscribe(
            &operation,
            endpoint,
            code,
            characteristic.uuid,
            sender,
            cancelled.clone(),
        )?;
        Ok(CapabilityEventStream {
            receiver,
            cancelled,
        })
    }

    fn shutdown(
        &self,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + '_>> {
        let client = self.client.existing();
        Box::pin(async move {
            match client {
                Some(client) => client.shutdown(deadline).await,
                None => Ok(()),
            }
        })
    }
}

fn endpoint_table(
    lua: &Lua,
    context: &InvocationContext,
    inventory: EndpointScan,
) -> mlua::Result<Table> {
    let endpoints = lua.create_table()?;
    let mut index = 0;
    for descriptor in inventory.endpoints {
        if context.operation.is_some() && !context.allowed_endpoints.contains(&descriptor.token) {
            continue;
        }
        index += 1;
        context
            .discovered
            .borrow_mut()
            .insert(descriptor.token, descriptor.clone());
        endpoints.raw_set(
            index,
            lua.create_userdata(Endpoint {
                descriptor,
                owner: context.owner.clone(),
            })?,
        )?;
    }
    let result = lua.create_table()?;
    result.raw_set("endpoints", endpoints)?;
    result.raw_set("errors", lua.to_value(&inventory.errors)?)?;
    Ok(result)
}

fn characteristic<'a>(
    permissions: &'a ParsedPermissions,
    id: &str,
) -> Option<(u8, &'a ParsedCharacteristic)> {
    permissions
        .characteristics
        .iter()
        .enumerate()
        .find(|(_, characteristic)| characteristic.id == id)
        .map(|(index, characteristic)| (index as u8, characteristic))
}

fn validate_characteristic(
    context: &InvocationContext,
    owner: &str,
    endpoint: EndpointToken,
    characteristic: Option<(u8, &ParsedCharacteristic)>,
    write: bool,
    codes: &BTreeSet<u8>,
) -> Result<(u8, Uuid), DriverError> {
    let code = characteristic.map_or(u8::MAX, |(code, _)| code);
    context.validate_native(owner, endpoint, code, write, codes)?;
    let (code, characteristic) = characteristic.ok_or_else(denied)?;
    Ok((code, characteristic.uuid))
}

fn valid_id(id: &str) -> bool {
    let mut characters = id.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && id.len() <= 32
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

fn denied() -> DriverError {
    DriverError::new(
        ErrorCode::PermissionDenied,
        "Bluetooth permissions or bindings are invalid.",
    )
}
fn unavailable() -> DriverError {
    DriverError::new(
        ErrorCode::DeviceUnavailable,
        "Native Bluetooth endpoint or owner is unavailable.",
    )
}
fn unsupported_event() -> DriverError {
    DriverError::new(
        ErrorCode::UnsupportedOperation,
        "Bluetooth notification is not permitted for this characteristic.",
    )
}
fn payload_error() -> DriverError {
    DriverError::new(
        ErrorCode::InvalidValue,
        "Bluetooth payload must contain 1 to 512 bytes.",
    )
}
fn repeat_options_error() -> DriverError {
    DriverError::new(
        ErrorCode::InvalidValue,
        "Bluetooth repeat options are invalid.",
    )
}
fn pair_error(lua: &Lua, error: DriverError) -> mlua::Result<(Value, Value)> {
    Ok((Value::Nil, lua_error(lua, &error)?))
}
