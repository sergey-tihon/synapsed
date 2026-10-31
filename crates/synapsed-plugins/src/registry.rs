use crate::{loader::Package, manifest::Manifest, runtime::InvocationContext};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Instant,
};
use synapsed_core::{DriverError, ErrorCode};

/// A factory supplies an actual bounded native event channel, never a synthetic hardware event.
pub struct CapabilityEventStream {
    pub receiver: tokio::sync::mpsc::Receiver<Value>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for CapabilityEventStream {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

pub trait LuaCapabilityFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    fn validate_permissions(&self, permissions: &Value) -> Result<(), DriverError>;
    fn install(&self, lua: &mlua::Lua, invocation: InvocationContext) -> mlua::Result<mlua::Table>;
    /// Factories, not the device host, interpret capability-private write bindings.
    fn validate_bindings(
        &self,
        _permissions: &Value,
        bindings: &Value,
    ) -> Result<BTreeMap<u8, u16>, DriverError> {
        if bindings.as_array().is_some_and(Vec::is_empty)
            || bindings.as_object().is_some_and(serde_json::Map::is_empty)
        {
            Ok(BTreeMap::new())
        } else {
            Err(unsupported())
        }
    }
    fn read_permissions(&self, _permissions: &Value) -> Result<BTreeSet<u8>, DriverError> {
        Ok(BTreeSet::new())
    }
    fn subscribe_events(
        &self,
        _invocation: InvocationContext,
        _endpoint: synapsed_core::EndpointToken,
        _event: &str,
    ) -> Result<CapabilityEventStream, DriverError> {
        Err(DriverError::new(
            ErrorCode::UnsupportedOperation,
            "This capability does not implement the requested event stream.",
        ))
    }
    fn shutdown(
        &self,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), DriverError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}
#[derive(Clone, Default)]
pub struct CapabilityRegistry {
    factories: BTreeMap<String, Arc<dyn LuaCapabilityFactory>>,
}
impl CapabilityRegistry {
    pub fn register(&mut self, factory: Arc<dyn LuaCapabilityFactory>) -> Result<(), DriverError> {
        synapsed_api::validate_driver_id(factory.id())?;
        if factory.version() == 0 || self.factories.contains_key(factory.id()) {
            return Err(DriverError::new(
                ErrorCode::UnsupportedCapability,
                "Capability registration is duplicate or invalid.",
            ));
        }
        self.factories.insert(factory.id().into(), factory);
        Ok(())
    }
    pub(crate) fn validate_manifest(&self, manifest: &Manifest) -> Result<(), DriverError> {
        for (id, version) in &manifest.capabilities {
            let factory = self.factory(id)?;
            if factory.version() != *version {
                return Err(unsupported());
            }
            factory.validate_permissions(&manifest.permissions[id])?;
        }
        Ok(())
    }
    pub(crate) fn factory(&self, id: &str) -> Result<&Arc<dyn LuaCapabilityFactory>, DriverError> {
        self.factories.get(id).ok_or_else(unsupported)
    }
    pub(crate) fn install(
        &self,
        lua: &mlua::Lua,
        manifest: &Manifest,
        invocation: InvocationContext,
    ) -> mlua::Result<mlua::Table> {
        let capabilities = lua.create_table()?;
        for id in manifest.capabilities.keys() {
            let mut context = invocation.clone();
            context.permissions = manifest.permissions[id].clone();
            capabilities.raw_set(
                id.as_str(),
                self.factory(id)
                    .map_err(mlua::Error::external)?
                    .install(lua, context)?,
            )?;
        }
        Ok(capabilities)
    }
    pub(crate) fn reads(&self, manifest: &Manifest) -> Result<BTreeSet<u8>, DriverError> {
        let mut reads = BTreeSet::new();
        for (id, permissions) in &manifest.permissions {
            reads.extend(self.factory(id)?.read_permissions(permissions)?);
        }
        Ok(reads)
    }
    pub async fn shutdown(&self, deadline: Instant) -> Result<(), DriverError> {
        for factory in self.factories.values() {
            tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                factory.shutdown(deadline),
            )
            .await
            .map_err(|_| {
                DriverError::new(ErrorCode::Timeout, "Native capability shutdown timed out.")
            })??;
        }
        Ok(())
    }
}
fn unsupported() -> DriverError {
    DriverError::new(
        ErrorCode::UnsupportedCapability,
        "Requested capability or version is unavailable.",
    )
}

pub(crate) fn supersedes(owner: &str, other: &str, packages: &BTreeMap<String, Package>) -> bool {
    fn visit(
        owner: &str,
        other: &str,
        packages: &BTreeMap<String, Package>,
        seen: &mut BTreeSet<String>,
    ) -> bool {
        if !seen.insert(owner.into()) {
            return false;
        }
        packages.get(owner).is_some_and(|package| {
            package
                .manifest
                .supersedes
                .iter()
                .any(|id| id == other || visit(id, other, packages, seen))
        })
    }
    owner != other && visit(owner, other, packages, &mut BTreeSet::new())
}
