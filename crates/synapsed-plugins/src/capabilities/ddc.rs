use crate::{
    registry::LuaCapabilityFactory,
    runtime::{InvocationContext, lua_error},
};
use mlua::{
    AnyUserData, Lua, LuaSerdeExt, MetaMethod, Table, UserData, UserDataFields, UserDataMethods,
    Value,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Mutex,
    time::Instant,
};
use synapsed_core::{DriverError, ErrorCode};
use synapsed_ddc::{DdcClient, EndpointDescriptor};

#[derive(Clone)]
pub(crate) struct Endpoint {
    pub descriptor: EndpointDescriptor,
    pub owner: std::sync::Arc<str>,
}
impl UserData for Endpoint {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("capability", |_, this| {
            Ok(this.descriptor.capability.clone())
        });
        fields.add_field_method_get("manufacturer", |_, this| {
            Ok(this.descriptor.manufacturer.clone())
        });
        fields.add_field_method_get("product", |_, this| Ok(this.descriptor.product));
        fields.add_field_method_get("productName", |_, this| {
            Ok(this.descriptor.product_name.clone())
        });
        fields.add_field_method_get("identity", |_, this| Ok(this.descriptor.identity.clone()));
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("endpoint"));
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Permissions {
    read: Vec<u8>,
    write: Vec<u8>,
}
impl Permissions {
    fn parse(value: &serde_json::Value) -> Result<Self, DriverError> {
        let result: Self = serde_json::from_value(value.clone()).map_err(|_| denied())?;
        let read: BTreeSet<_> = result.read.iter().copied().collect();
        let write: BTreeSet<_> = result.write.iter().copied().collect();
        if read.len() != result.read.len()
            || write.len() != result.write.len()
            || !write.is_subset(&read)
        {
            return Err(denied());
        }
        Ok(result)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    code: u8,
    change_mask: u16,
}

pub struct DdcCapabilityFactory {
    client: std::sync::Arc<Mutex<Option<DdcClient>>>,
}
impl DdcCapabilityFactory {
    pub fn new(client: DdcClient) -> Self {
        Self {
            client: std::sync::Arc::new(Mutex::new(Some(client))),
        }
    }
    pub fn lazy_native() -> Self {
        Self {
            client: std::sync::Arc::new(Mutex::new(None)),
        }
    }
    fn resolve_client(source: &Mutex<Option<DdcClient>>) -> Result<DdcClient, DriverError> {
        let mut client = source.lock().expect("native client lock");
        if client.is_none() {
            *client = Some(DdcClient::start_native()?);
        }
        Ok(client.as_ref().expect("started client").clone())
    }
}
impl LuaCapabilityFactory for DdcCapabilityFactory {
    fn id(&self) -> &'static str {
        "ddc"
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
        Ok(Permissions::parse(permissions)?.read.into_iter().collect())
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
            if binding.change_mask == 0
                || !permissions.read.contains(&binding.code)
                || !permissions.write.contains(&binding.code)
                || result.insert(binding.code, binding.change_mask).is_some()
            {
                return Err(denied());
            }
        }
        Ok(result)
    }
    fn install(&self, lua: &Lua, invocation: InvocationContext) -> mlua::Result<Table> {
        let client = self.client.clone();
        let permissions =
            Permissions::parse(&invocation.permissions).map_err(mlua::Error::external)?;
        let table = lua.create_table()?;
        let context = invocation.clone();
        let enumerate_client = client.clone();
        table.raw_set(
            "enumerate",
            lua.create_async_function(move |lua, _: Table| {
                let context = context.clone();
                let client = enumerate_client.clone();
                async move {
                    if let Err(error) = context.check() {
                        return pair_error(&lua, error);
                    }
                    let client = match Self::resolve_client(&client) {
                        Ok(client) => client,
                        Err(error) => return pair_error(&lua, error),
                    };
                    match client.enumerate().await {
                        Ok(inventory) => {
                            context.check().map_err(mlua::Error::external)?;
                            let endpoints = lua.create_table()?;
                            let mut index = 0;
                            for descriptor in inventory.endpoints {
                                // Activated scopes never acquire another device's native endpoint.
                                if context.operation.is_some()
                                    && !context.allowed_endpoints.contains(&descriptor.token)
                                {
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
                            Ok((Value::Table(result), Value::Nil))
                        }
                        Err(error) => pair_error(&lua, error),
                    }
                }
            })?,
        )?;
        let context = invocation.clone();
        let get_client = client.clone();
        let reads: std::sync::Arc<BTreeSet<u8>> =
            std::sync::Arc::new(permissions.read.into_iter().collect());
        table.raw_set(
            "get_vcp",
            lua.create_async_function(
                move |lua, (_, endpoint, code): (Table, AnyUserData, u8)| {
                    let context = context.clone();
                    let client = get_client.clone();
                    let reads = reads.clone();
                    let endpoint = endpoint
                        .borrow::<Endpoint>()
                        .map(|endpoint| (endpoint.descriptor.token, endpoint.owner.clone()));
                    async move {
                        let (endpoint, owner) = endpoint?;
                        if let Err(error) =
                            context.validate_native(&owner, endpoint, code, false, &reads)
                        {
                            return pair_error(&lua, error);
                        }
                        let client = match Self::resolve_client(&client) {
                            Ok(client) => client,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let operation = context.native_context(&reads);
                        let result = client.get_vcp(operation.clone(), endpoint, code).await;
                        if context.operation.is_none() {
                            operation.finish();
                        }
                        context.check().map_err(mlua::Error::external)?;
                        match result {
                            Ok(value) => Ok((lua.to_value(&value)?, Value::Nil)),
                            Err(error) => pair_error(&lua, error),
                        }
                    }
                },
            )?,
        )?;
        let context = invocation;
        let writes: std::sync::Arc<BTreeSet<u8>> =
            std::sync::Arc::new(permissions.write.into_iter().collect());
        table.raw_set(
            "set_vcp",
            lua.create_async_function(
                move |lua, (_, endpoint, code, value): (Table, AnyUserData, u8, u16)| {
                    let context = context.clone();
                    let client = client.clone();
                    let writes = writes.clone();
                    let endpoint = endpoint
                        .borrow::<Endpoint>()
                        .map(|endpoint| (endpoint.descriptor.token, endpoint.owner.clone()));
                    async move {
                        let (endpoint, owner) = endpoint?;
                        if let Err(error) =
                            context.validate_native(&owner, endpoint, code, true, &writes)
                        {
                            return pair_error(&lua, error);
                        }
                        let client = match Self::resolve_client(&client) {
                            Ok(client) => client,
                            Err(error) => return pair_error(&lua, error),
                        };
                        let operation = context
                            .operation
                            .clone()
                            .ok_or_else(|| mlua::Error::external(denied()))?;
                        let result = client.set_vcp(operation, endpoint, code, value).await;
                        context.check().map_err(mlua::Error::external)?;
                        match result {
                            Ok(value) => Ok((lua.to_value(&value)?, Value::Nil)),
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
        Ok(table)
    }
    fn shutdown(
        &self,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + '_>> {
        let client = self.client.lock().expect("native client lock").clone();
        Box::pin(async move {
            match client {
                Some(client) => client.shutdown(deadline).await,
                None => Ok(()),
            }
        })
    }
}
fn denied() -> DriverError {
    DriverError::new(
        ErrorCode::PermissionDenied,
        "DDC register permissions or bindings are invalid.",
    )
}
fn pair_error(lua: &Lua, error: DriverError) -> mlua::Result<(Value, Value)> {
    Ok((Value::Nil, lua_error(lua, &error)?))
}
