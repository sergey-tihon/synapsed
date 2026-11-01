//! Trusted installed Lua device packages. Rust owns transport grants, never device policy.
mod loader;
mod manifest;
mod registry;
mod runtime;
pub mod capabilities {
    pub mod ddc;
}
pub use registry::{CapabilityEventStream, CapabilityRegistry, LuaCapabilityFactory};
pub use runtime::{InvocationContext, InvocationGuard};
pub use synapsed_ddc::EndpointDescriptor;
pub use synapsed_inventory::{DriverCatalogEntry, InventoryMatch, InventoryRole};

use loader::{Catalog, Package};
use runtime::{
    CALLBACK_LIMIT, Candidate, Failure, InvocationGuard as Guard, RuntimeCommand, RuntimeEvent,
    RuntimeHandle, exposed_error, unavailable,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use synapsed_api::{
    API_VERSION, CharacteristicCommand, Device, DeviceStatus, DriverState, DriverStatus, Snapshot,
};
use synapsed_core::{
    DeviceGrant, DriverError, EndpointGates, EndpointToken, ErrorCode, OperationContext, StateStore,
};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, mpsc, oneshot, watch};

pub const MAX_SETTINGS_DEPTH: usize = 8;
pub const MAX_SETTINGS_ENTRIES: usize = 4096;
pub const MAX_DISABLED: usize = 16;
pub const MAX_DRIVER_SETTINGS: usize = 16;
pub const MIN_DISCOVERY_INTERVAL_MS: u64 = 1000;
type CandidateChoice = (String, Candidate);
type CandidateSelection = (BTreeMap<String, CandidateChoice>, Vec<CandidateChoice>);

#[derive(Clone, Debug)]
pub struct PluginConfig {
    pub directory: PathBuf,
    pub directory_explicit: bool,
    pub disabled: Vec<String>,
    pub discovery_interval_ms: u64,
    pub settings: BTreeMap<String, Value>,
}
impl PluginConfig {
    pub fn validate(&self) -> Result<(), DriverError> {
        self.validate_with_max_settings_entries(MAX_SETTINGS_ENTRIES)
    }
    pub fn validate_with_max_settings_entries(
        &self,
        max_settings_entries: usize,
    ) -> Result<(), DriverError> {
        if self.discovery_interval_ms < MIN_DISCOVERY_INTERVAL_MS
            || self.disabled.len() > MAX_DISABLED
            || self.settings.len() > MAX_DRIVER_SETTINGS
        {
            return Err(invalid_config());
        }
        let mut disabled = BTreeSet::new();
        for id in &self.disabled {
            synapsed_api::validate_driver_id(id)?;
            if !disabled.insert(id) {
                return Err(invalid_config());
            }
        }
        for (id, settings) in &self.settings {
            synapsed_api::validate_driver_id(id)?;
            if !settings.is_object() || !bounded_settings(settings, 0, &mut 0, max_settings_entries)
            {
                return Err(invalid_config());
            }
            synapsed_api::validate_encoded_size(settings, 64 * 1024)?;
        }
        Ok(())
    }
}
fn bounded_settings(value: &Value, depth: usize, count: &mut usize, max_entries: usize) -> bool {
    *count += 1;
    if depth > MAX_SETTINGS_DEPTH || *count > max_entries {
        return false;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .all(|value| bounded_settings(value, depth + 1, count, max_entries)),
        Value::Object(values) => values.iter().all(|(key, value)| {
            key.chars().count() <= 128 && bounded_settings(value, depth + 1, count, max_entries)
        }),
        Value::String(value) => value.chars().count() <= 1024,
        Value::Number(value) => value.as_f64().is_some_and(f64::is_finite),
        _ => true,
    }
}
fn invalid_config() -> DriverError {
    DriverError::new(
        ErrorCode::InvalidRequest,
        "Plugin configuration is invalid or exceeds data limits.",
    )
}
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<HostInner>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub driver: String,
    pub endpoints: Vec<EndpointToken>,
}
pub type Bindings = BTreeMap<String, Binding>;
fn coordinator_initialization_error() -> DriverError {
    DriverError::new(
        ErrorCode::PluginError,
        "Plugin coordinator initialization failed.",
    )
}
struct HostInner {
    commands: mpsc::Sender<HostCommand>,
    accepting: Arc<AtomicBool>,
    current: Arc<Mutex<Option<OperationContext>>>,
    store: StateStore,
    catalog: Arc<[DriverCatalogEntry]>,
    bindings: watch::Sender<Arc<Bindings>>,
    readiness: AsyncMutex<Option<oneshot::Receiver<Result<(), DriverError>>>>,
    startup: OnceCell<Result<(), DriverError>>,
}
impl HostInner {
    async fn await_ready(&self) -> Result<(), DriverError> {
        self.startup
            .get_or_init(|| async {
                let mut readiness = self.readiness.lock().await;
                let Some(receiver) = readiness.as_mut() else {
                    return Err(coordinator_initialization_error());
                };
                let result = receiver
                    .await
                    .unwrap_or_else(|_| Err(coordinator_initialization_error()));
                *readiness = None;
                result
            })
            .await
            .clone()
    }
}
impl Drop for HostInner {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::SeqCst);
        if let Some(operation) = self.current.lock().expect("current operation").as_ref() {
            operation.cancel();
        }
    }
}
struct Cancellation {
    abandoned: AtomicBool,
    operation: Mutex<Option<OperationContext>>,
}
struct Abandonment {
    cancellation: Arc<Cancellation>,
    armed: bool,
}
impl Drop for Abandonment {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.abandoned.store(true, Ordering::SeqCst);
            if let Some(operation) = self
                .cancellation
                .operation
                .lock()
                .expect("operation cancellation")
                .as_ref()
            {
                operation.abandon();
            }
        }
    }
}
struct CurrentOperation {
    current: Arc<Mutex<Option<OperationContext>>>,
    sequence: u64,
}
impl Drop for CurrentOperation {
    fn drop(&mut self) {
        let mut current = self.current.lock().expect("current operation");
        if current
            .as_ref()
            .is_some_and(|operation| operation.sequence() == self.sequence)
        {
            current.take();
        }
    }
}
enum HostCommand {
    Dispatch {
        device: String,
        characteristic: String,
        command: CharacteristicCommand,
        deadline: Instant,
        cancellation: Arc<Cancellation>,
        reply: oneshot::Sender<Result<Arc<Snapshot>, DriverError>>,
    },
    #[cfg(debug_assertions)]
    DiscoveryRound { reply: oneshot::Sender<u64> },
    Shutdown {
        deadline: Instant,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
}
fn catalog_entries(catalog: &Catalog) -> Arc<[DriverCatalogEntry]> {
    catalog
        .packages
        .values()
        .map(|package| {
            let inventory_matches =
                package
                    .manifest
                    .inventory
                    .as_ref()
                    .map_or_else(Vec::new, |inventory| {
                        let mut matches = inventory
                            .usb
                            .iter()
                            .map(|entry| InventoryMatch::Usb {
                                vendor_id: entry.vendor_id.clone(),
                                product_id: entry.product_id.clone(),
                                role: match entry.role {
                                    manifest::ManifestInventoryRole::MonitorCompanion => {
                                        InventoryRole::MonitorCompanion
                                    }
                                    manifest::ManifestInventoryRole::Device => {
                                        InventoryRole::Device
                                    }
                                },
                            })
                            .collect::<Vec<_>>();
                        matches.extend(inventory.bluetooth.iter().map(|entry| {
                            InventoryMatch::Bluetooth {
                                name_prefix: entry.name_prefix.clone(),
                                role: match entry.role {
                                    manifest::ManifestInventoryRole::MonitorCompanion => {
                                        InventoryRole::MonitorCompanion
                                    }
                                    manifest::ManifestInventoryRole::Device => {
                                        InventoryRole::Device
                                    }
                                },
                            }
                        }));
                        matches
                    });
            DriverCatalogEntry {
                id: package.manifest.id.clone(),
                version: package.manifest.version.clone(),
                supersedes: package.manifest.supersedes.clone(),
                inventory_matches,
            }
        })
        .collect::<Vec<_>>()
        .into()
}
impl PluginHost {
    pub fn start(
        store: StateStore,
        config: PluginConfig,
        registry: CapabilityRegistry,
    ) -> Result<Self, DriverError> {
        config.validate()?;
        let mut catalog = loader::load(&config, &registry)?;
        let mut rejected = BTreeSet::new();
        loop {
            let status_snapshot = Snapshot {
                api_version: API_VERSION,
                node: store.snapshot().node.clone(),
                revision: u64::MAX.to_string(),
                drivers: catalog.statuses.values().cloned().collect(),
                devices: vec![],
            };
            match StateStore::validate_snapshot(&status_snapshot).and_then(|_| {
                store.publish_driver_states(
                    catalog
                        .statuses
                        .values()
                        .cloned()
                        .map(|status| (status, vec![]))
                        .collect(),
                )
            }) {
                Ok(_) => break,
                Err(error) => {
                    let invalid = catalog
                        .statuses
                        .iter()
                        .find(|(id, status)| !rejected.contains(*id) && status.validate().is_err());
                    let offender = invalid.or_else(|| {
                        catalog
                            .statuses
                            .iter()
                            .filter(|(id, _)| !rejected.contains(*id))
                            .max_by_key(|(_, status)| status.version.len())
                    });
                    let Some((id, _)) = offender else {
                        return Err(error);
                    };
                    let id = id.clone();
                    rejected.insert(id.clone());
                    catalog.packages.remove(&id);
                    let status = catalog.statuses.get_mut(&id).expect("catalog status");
                    status.version = "unknown".into();
                    status.state = DriverState::Error;
                    status.error = Some(DriverError::new(
                        ErrorCode::PluginError,
                        "Package status cannot be represented within snapshot limits.",
                    ));
                    loader::disable_dependants(&mut catalog);
                }
            }
        }
        let public_catalog = catalog_entries(&catalog);
        let (commands, receiver) = mpsc::channel(64);
        let (binding_updates, _) = watch::channel(Arc::new(BTreeMap::new()));
        let (ready, readiness) = oneshot::channel();
        let worker_store = store.clone();
        let accepting = Arc::new(AtomicBool::new(true));
        let current = Arc::new(Mutex::new(None));
        let worker_accepting = accepting.clone();
        let worker_current = current.clone();
        let worker_bindings = binding_updates.clone();
        std::thread::Builder::new()
            .name("synapsed-plugins".into())
            .spawn(move || {
                let runner = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build();
                let runner = match runner {
                    Ok(runner) => runner,
                    Err(_) => {
                        let _ = ready.send(Err(coordinator_initialization_error()));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                runner.block_on(Coordinator::run(
                    worker_store,
                    config,
                    registry,
                    catalog,
                    receiver,
                    worker_accepting,
                    worker_current,
                    worker_bindings,
                ));
            })
            .map_err(|_| {
                DriverError::new(
                    ErrorCode::PluginError,
                    "Plugin coordinator thread could not start.",
                )
            })?;
        let startup = OnceCell::<Result<(), DriverError>>::new();
        let readiness = if tokio::runtime::Handle::try_current().is_err() {
            let result = readiness
                .blocking_recv()
                .unwrap_or_else(|_| Err(coordinator_initialization_error()));
            result?;
            let _ = startup.set(Ok(()));
            None
        } else {
            Some(readiness)
        };
        Ok(Self {
            inner: Arc::new(HostInner {
                commands,
                accepting,
                current,
                store,
                catalog: public_catalog,
                bindings: binding_updates,
                readiness: AsyncMutex::new(readiness),
                startup,
            }),
        })
    }
    /// Subscribe to current published driver/device/endpoint bindings.
    pub fn bindings(&self) -> watch::Receiver<Arc<Bindings>> {
        self.inner.bindings.subscribe()
    }
    /// Return installed catalog metadata used only for passive hardware matching.
    pub fn catalog(&self) -> Arc<[DriverCatalogEntry]> {
        self.inner.catalog.clone()
    }
    pub async fn dispatch(
        &self,
        device: &str,
        characteristic: &str,
        command: CharacteristicCommand,
    ) -> Result<Arc<Snapshot>, DriverError> {
        self.inner.await_ready().await?;
        if !self.inner.accepting.load(Ordering::SeqCst) {
            return Err(unavailable_error());
        }
        command.validate()?;
        let deadline = Instant::now() + CALLBACK_LIMIT;
        let cancellation = Arc::new(Cancellation {
            abandoned: AtomicBool::new(false),
            operation: Mutex::new(None),
        });
        let mut abandonment = Abandonment {
            cancellation: cancellation.clone(),
            armed: true,
        };
        let (reply, result) = oneshot::channel();
        self.inner
            .commands
            .try_send(HostCommand::Dispatch {
                device: device.into(),
                characteristic: characteristic.into(),
                command,
                deadline,
                cancellation,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => busy(),
                mpsc::error::TrySendError::Closed(_) => unavailable_error(),
            })?;
        let response = tokio::time::timeout(CALLBACK_LIMIT, result)
            .await
            .map_err(|_| timeout_error())?
            .map_err(|_| unavailable_error())?;
        abandonment.armed = false;
        response
    }
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub async fn complete_discovery_round_for_test(&self) -> Result<u64, DriverError> {
        self.inner.await_ready().await?;
        if !self.inner.accepting.load(Ordering::SeqCst) {
            return Err(unavailable_error());
        }
        let (reply, receiver) = oneshot::channel();
        self.inner
            .commands
            .try_send(HostCommand::DiscoveryRound { reply })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => busy(),
                _ => unavailable_error(),
            })?;
        tokio::time::timeout(CALLBACK_LIMIT, receiver)
            .await
            .map_err(|_| timeout_error())?
            .map_err(|_| unavailable_error())
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.inner.store.snapshot()
    }
    pub async fn shutdown(&self, deadline: Instant) -> Result<(), DriverError> {
        self.inner.accepting.store(false, Ordering::SeqCst);
        if let Some(operation) = self
            .inner
            .current
            .lock()
            .expect("current operation")
            .as_ref()
        {
            operation.cancel();
        }
        self.inner.await_ready().await?;
        let (reply, receiver) = oneshot::channel();
        tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            self.inner
                .commands
                .send(HostCommand::Shutdown { deadline, reply }),
        )
        .await
        .map_err(|_| timeout_error())?
        .map_err(|_| unavailable_error())?;
        tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), receiver)
            .await
            .map_err(|_| timeout_error())?
            .map_err(|_| unavailable_error())?
    }
}
#[derive(Clone)]
struct Route {
    owner: String,
    candidate: Candidate,
    grant: DeviceGrant,
    device: Device,
    sequence: u64,
}
struct History {
    tokens: Vec<EndpointToken>,
    identities: Vec<Option<String>>,
}
struct Coordinator {
    store: StateStore,
    registry: CapabilityRegistry,
    packages: BTreeMap<String, Package>,
    statuses: BTreeMap<String, DriverStatus>,
    runtimes: BTreeMap<String, RuntimeHandle>,
    candidates: BTreeMap<String, Vec<Candidate>>,
    failed: BTreeMap<String, DriverError>,
    quarantined: BTreeSet<String>,
    routes: BTreeMap<String, Route>,
    history: BTreeMap<(String, String), History>,
    gates: EndpointGates,
    epoch: u64,
    reports: BTreeMap<String, Vec<Device>>,
    last: BTreeMap<(String, String), Device>,
    dominance: BTreeMap<String, BTreeSet<String>>,
    current: Arc<Mutex<Option<OperationContext>>>,
    binding_updates: watch::Sender<Arc<Bindings>>,
    #[cfg(debug_assertions)]
    discovery_round: u64,
}
impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        store: StateStore,
        config: PluginConfig,
        registry: CapabilityRegistry,
        catalog: Catalog,
        mut commands: mpsc::Receiver<HostCommand>,
        accepting: Arc<AtomicBool>,
        current: Arc<Mutex<Option<OperationContext>>>,
        binding_updates: watch::Sender<Arc<Bindings>>,
    ) {
        let (events, mut observed) = mpsc::channel(64);
        let dominance = catalog
            .packages
            .keys()
            .map(|owner| {
                (
                    owner.clone(),
                    catalog
                        .packages
                        .keys()
                        .filter(|other| registry::supersedes(owner, other, &catalog.packages))
                        .cloned()
                        .collect(),
                )
            })
            .collect();
        let mut host = Self {
            store,
            registry: registry.clone(),
            packages: catalog.packages,
            statuses: catalog.statuses,
            runtimes: BTreeMap::new(),
            candidates: BTreeMap::new(),
            failed: BTreeMap::new(),
            quarantined: BTreeSet::new(),
            routes: BTreeMap::new(),
            history: BTreeMap::new(),
            gates: EndpointGates::default(),
            epoch: 0,
            reports: BTreeMap::new(),
            last: BTreeMap::new(),
            dominance,
            current,
            binding_updates,
            #[cfg(debug_assertions)]
            discovery_round: 0,
        };
        for (id, package) in &host.packages {
            let settings = config
                .settings
                .get(id)
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            match runtime::start(
                package.clone(),
                host.packages.clone(),
                registry.clone(),
                settings,
                events.clone(),
                accepting.clone(),
            ) {
                Ok(runtime) => {
                    host.runtimes.insert(id.clone(), runtime);
                }
                Err(error) => {
                    let status = host.statuses.get_mut(id).expect("package status exists");
                    status.state = DriverState::Error;
                    status.error = Some(exposed_error(&error));
                    host.quarantined.insert(id.clone());
                }
            }
        }
        drop(events);
        let initializations = host
            .runtimes
            .iter_mut()
            .filter_map(|(id, runtime)| {
                let receiver = runtime.ready.take()?;
                let id = id.clone();
                Some(async move {
                    (
                        id,
                        match tokio::time::timeout(CALLBACK_LIMIT, receiver).await {
                            Ok(Ok(result)) => result,
                            _ => Err(Failure::fatal(timeout_error())),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for (id, result) in futures_util::future::join_all(initializations).await {
            if let Err(failure) = result {
                host.quarantine(&id, &failure.error).await;
            }
        }
        host.disable_dependants().await;
        host.discover().await;
        #[cfg(debug_assertions)]
        host.record_discovery_round();
        let period = Duration::from_millis(config.discovery_interval_ms);
        let mut discovery = tokio::time::interval(period);
        discovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        discovery.tick().await;
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(HostCommand::Dispatch { device, characteristic, command, deadline, cancellation, reply }) => {
                        let result = host.dispatch(&device, &characteristic, command, deadline, cancellation).await;
                        let _ = reply.send(result);
                        // State events are drained before the next command, preventing command floods from starving observations.
                        while let Ok(event) = observed.try_recv() {
                            host.observation(event).await;
                        }
                    }
                    #[cfg(debug_assertions)]
                    Some(HostCommand::DiscoveryRound { reply }) => {
                        host.discover().await;
                        while let Ok(event) = observed.try_recv() {
                            host.observation(event).await;
                        }
                        let _ = reply.send(host.record_discovery_round());
                    }
                    Some(HostCommand::Shutdown { deadline, reply }) => {
                        let result = host.stop(deadline).await;
                        let _ = reply.send(result);
                        break;
                    }
                    None => {
                        let _ = host.stop(Instant::now() + CALLBACK_LIMIT).await;
                        break;
                    }
                },
                event = observed.recv(), if !observed.is_closed() => {
                    if let Some(event) = event {
                        host.observation(event).await;
                    }
                }
                _ = discovery.tick() => {
                    host.discover().await;
                    #[cfg(debug_assertions)]
                    host.record_discovery_round();
                }
            }
        }
    }
    #[cfg(debug_assertions)]
    fn record_discovery_round(&mut self) -> u64 {
        self.discovery_round = self.discovery_round.saturating_add(1);
        self.discovery_round
    }
    async fn discover(&mut self) {
        let requests = self
            .runtimes
            .iter()
            .filter(|(id, _)| !self.quarantined.contains(*id))
            .map(|(id, runtime)| {
                let id = id.clone();
                let sender = runtime.sender.clone();
                async move {
                    let guard = Guard::new(Instant::now() + CALLBACK_LIMIT);
                    let (reply, receiver) = oneshot::channel();
                    let result = match sender.try_send(RuntimeCommand::Discover {
                        guard: guard.clone(),
                        reply,
                    }) {
                        Ok(()) => match tokio::time::timeout(CALLBACK_LIMIT, receiver).await {
                            Ok(Ok(result)) => result,
                            _ => {
                                guard.trip();
                                Err(Failure::fatal(timeout_error()))
                            }
                        },
                        Err(mpsc::error::TrySendError::Full(_)) => Err(Failure {
                            error: busy(),
                            fatal: false,
                        }),
                        Err(_) => Err(Failure::fatal(unavailable_error())),
                    };
                    (id, result)
                }
            })
            .collect::<Vec<_>>();
        for (id, result) in futures_util::future::join_all(requests).await {
            let result = result.and_then(|candidates| {
                self.check_candidates(&id, &candidates)
                    .map_err(Failure::fatal)?;
                Ok(candidates)
            });
            match result {
                Ok(candidates) => {
                    self.candidates.insert(id.clone(), candidates);
                    self.failed.remove(&id);
                    let status = self.status_mut(&id);
                    status.state = DriverState::Online;
                    status.error = None;
                }
                Err(failure) => {
                    self.failed.insert(id.clone(), failure.error.clone());
                    if failure.fatal {
                        self.quarantine(&id, &failure.error).await;
                    } else {
                        let status = self.status_mut(&id);
                        status.state = match failure.error.code {
                            ErrorCode::PermissionDenied => DriverState::PermissionDenied,
                            ErrorCode::AmbiguousDevice | ErrorCode::DriverConflict => {
                                DriverState::Ambiguous
                            }
                            _ => DriverState::Disconnected,
                        };
                        status.error = Some(exposed_error(&failure.error));
                    }
                }
            }
        }
        self.disable_dependants().await;
        if let Err(error) = self.arbitrate().await {
            tracing::error!(target: "synapsed::plugins", public_message = %"Plugin ownership publication failed.", code = ?error.code);
            let _ = self.publish();
        }
    }
    async fn disable_dependants(&mut self) {
        loop {
            let disabled: Vec<_> = self
                .packages
                .iter()
                .filter(|(id, package)| {
                    !self.quarantined.contains(*id)
                        && package
                            .manifest
                            .dependencies
                            .iter()
                            .any(|dependency| self.quarantined.contains(dependency))
                })
                .map(|(id, _)| id.clone())
                .collect();
            if disabled.is_empty() {
                break;
            }
            for id in disabled {
                let error = DriverError::new(
                    ErrorCode::PluginError,
                    "Plugin dependency failed during initialization or execution.",
                );
                self.quarantine(&id, &error).await;
                self.status_mut(&id).state = DriverState::Disabled;
            }
        }
    }
    fn check_candidates(&self, owner: &str, candidates: &[Candidate]) -> Result<(), DriverError> {
        let published = self.store.snapshot();
        let mut devices: BTreeMap<_, _> = published
            .devices
            .iter()
            .filter(|device| device.driver != owner)
            .map(|device| (device.id.clone(), device.clone()))
            .collect();
        for (id, report) in &self.candidates {
            if id != owner && !self.failed.contains_key(id) && !self.quarantined.contains(id) {
                for candidate in report {
                    devices.insert(
                        candidate.device.id.clone(),
                        self.stale(
                            candidate,
                            &DriverError::new(ErrorCode::PluginError, StateStore::FAULT_MESSAGE),
                        ),
                    );
                }
            }
        }
        for candidate in candidates {
            devices.insert(
                candidate.device.id.clone(),
                self.stale(
                    candidate,
                    &DriverError::new(ErrorCode::PluginError, StateStore::FAULT_MESSAGE),
                ),
            );
        }
        self.check_snapshot(devices.into_values().collect())
    }
    fn check_report(&self, owner: &str, replacement: &Device) -> Result<(), DriverError> {
        let mut devices = self
            .reports
            .values()
            .flatten()
            .filter(|device| device.id != replacement.id)
            .cloned()
            .collect::<Vec<_>>();
        if replacement.driver != owner {
            return Err(invalid_config());
        }
        devices.push(replacement.clone());
        self.check_snapshot(devices)
    }
    fn check_snapshot(&self, devices: Vec<Device>) -> Result<(), DriverError> {
        let snapshot = Snapshot {
            api_version: API_VERSION,
            node: self.store.snapshot().node.clone(),
            revision: u64::MAX.to_string(),
            drivers: self.statuses.values().cloned().collect(),
            devices,
        };
        StateStore::validate_snapshot(&snapshot)
            .map(|_| ())
            .map_err(|_| {
                DriverError::new(
                    ErrorCode::PluginError,
                    "Plugin result exceeds declaration or recoverable snapshot limits.",
                )
            })
    }
    async fn arbitrate(&mut self) -> Result<(), DriverError> {
        loop {
            let (winners, conflicts) = self.select_winners();
            let staged = self.stage_reports(&winners, &conflicts);
            if let Err(error) = self.check_snapshot(staged.values().flatten().cloned().collect()) {
                let owner = winners
                    .values()
                    .rev()
                    .find(|(owner, candidate)| {
                        self.routes.get(&candidate.device.id).is_none_or(|route| {
                            route.owner != *owner || !route.candidate.equivalent(candidate)
                        })
                    })
                    .map(|(owner, _)| owner.clone())
                    .or_else(|| conflicts.last().map(|(owner, _)| owner.clone()));
                let Some(owner) = owner else {
                    return Err(error);
                };
                self.quarantine(&owner, &error).await;
                continue;
            }
            let retire: Vec<_> = self
                .routes
                .iter()
                .filter(|(id, route)| {
                    winners.get(*id).is_none_or(|(owner, candidate)| {
                        owner != &route.owner || !candidate.equivalent(&route.candidate)
                    })
                })
                .map(|(id, _)| id.clone())
                .collect();
            let retirement_deadline = Instant::now() + CALLBACK_LIMIT;
            for id in retire {
                self.retire(&id, retirement_deadline).await?;
            }
            self.reports = staged;
            for (id, (owner, candidate)) in winners {
                if !self.quarantined.contains(&owner) {
                    self.activate_route(&id, &owner, candidate).await?;
                }
            }
            self.report_conflicts(&conflicts);
            let published = self.store.snapshot();
            for previous in &published.devices {
                if let Some(error) = self.failed.get(&previous.driver)
                    && !self
                        .reports
                        .values()
                        .flatten()
                        .any(|device| device.id == previous.id)
                {
                    let mut device = previous.clone();
                    unavailable(&mut device, error);
                    self.set_report(&previous.driver, device);
                }
            }
            self.last.retain(|(owner, id), _| {
                self.candidates.get(owner).is_some_and(|candidates| {
                    candidates
                        .iter()
                        .any(|candidate| &candidate.device.id == id)
                })
            });
            return self.publish();
        }
    }
    fn select_winners(&self) -> CandidateSelection {
        let candidates: Vec<_> = self
            .candidates
            .iter()
            .filter(|(id, _)| !self.quarantined.contains(*id) && !self.failed.contains_key(*id))
            .flat_map(|(owner, candidates)| {
                candidates
                    .iter()
                    .map(move |candidate| (owner.clone(), candidate.clone()))
            })
            .collect();
        let mut endpoints = BTreeMap::<u64, BTreeSet<usize>>::new();
        let mut ids = BTreeMap::<&str, BTreeSet<usize>>::new();
        for (index, (_, candidate)) in candidates.iter().enumerate() {
            ids.entry(&candidate.device.id).or_default().insert(index);
            for endpoint in &candidate.endpoints {
                endpoints
                    .entry(endpoint.token.id)
                    .or_default()
                    .insert(index);
            }
        }
        let mut selected = BTreeSet::new();
        for (index, (owner, candidate)) in candidates.iter().enumerate() {
            let mut peers = ids[candidate.device.id.as_str()].clone();
            for endpoint in &candidate.endpoints {
                peers.extend(&endpoints[&endpoint.token.id]);
            }
            peers.remove(&index);
            let wins = peers.iter().all(|other_index| {
                let (other_owner, other) = &candidates[*other_index];
                let shared = candidate
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoints[&endpoint.token.id].contains(other_index));
                (candidate.device.id != other.device.id || shared)
                    && self.dominance[owner].contains(other_owner)
            });
            if wins {
                selected.insert(index);
            }
        }
        let mut endpoint_winners = BTreeMap::new();
        let mut winners = BTreeMap::new();
        for index in &selected {
            let (owner, candidate) = &candidates[*index];
            for endpoint in &candidate.endpoints {
                endpoint_winners.insert(endpoint.token.id, owner);
            }
            winners.insert(
                candidate.device.id.clone(),
                (owner.clone(), candidate.clone()),
            );
        }
        let mut conflicts = Vec::new();
        for (index, (owner, candidate)) in candidates.iter().enumerate() {
            if selected.contains(&index) {
                continue;
            }
            let fully_superseded = !candidate.endpoints.is_empty()
                && candidate.endpoints.iter().all(|endpoint| {
                    endpoint_winners
                        .get(&endpoint.token.id)
                        .is_some_and(|winner| self.dominance[*winner].contains(owner))
                });
            if !fully_superseded {
                conflicts.push((owner.clone(), candidate.clone()));
            }
        }
        (winners, conflicts)
    }
    fn stage_reports(
        &self,
        winners: &BTreeMap<String, (String, Candidate)>,
        conflicts: &[(String, Candidate)],
    ) -> BTreeMap<String, Vec<Device>> {
        // Preserve every retained failed report and every pending winner while
        // observations are acquired. Validation must see the complete cutover.
        let mut staged: BTreeMap<String, Vec<Device>> = self
            .statuses
            .keys()
            .map(|id| (id.clone(), vec![]))
            .collect();
        let mut represented = BTreeSet::new();
        let pending = DriverError::new(ErrorCode::DeviceUnavailable, StateStore::FAULT_MESSAGE);
        for (id, (owner, candidate)) in winners {
            let device = self
                .routes
                .get(id)
                .filter(|route| route.owner == *owner && route.candidate.equivalent(candidate))
                .map(|route| route.device.clone())
                .unwrap_or_else(|| self.stale(candidate, &pending));
            represented.insert(id.clone());
            staged.get_mut(owner).expect("staged owner").push(device);
        }
        let conflict = DriverError::new(ErrorCode::DriverConflict, StateStore::FAULT_MESSAGE);
        for (owner, candidate) in conflicts {
            if represented.insert(candidate.device.id.clone()) {
                staged
                    .get_mut(owner)
                    .expect("staged owner")
                    .push(self.stale(candidate, &conflict));
            }
        }
        let published = self.store.snapshot();
        for previous in &published.devices {
            if let Some(error) = self.failed.get(&previous.driver)
                && represented.insert(previous.id.clone())
            {
                let mut device = previous.clone();
                unavailable(&mut device, error);
                staged
                    .get_mut(&previous.driver)
                    .expect("retained owner")
                    .push(device);
            }
        }
        staged
    }
    async fn activate_route(
        &mut self,
        id: &str,
        owner: &str,
        candidate: Candidate,
    ) -> Result<(), DriverError> {
        if !self.routes.contains_key(id) {
            let key = (owner.to_owned(), id.to_owned());
            if !self.history.contains_key(&key) && self.history.len() >= 4096 {
                let error = DriverError::new(
                    ErrorCode::PluginError,
                    "Plugin device identity history is exhausted.",
                );
                self.quarantine(owner, &error).await;
                let device = self.stale(&candidate, &error);
                self.set_report(owner, device);
                return Ok(());
            }
            let tokens: Vec<_> = candidate
                .endpoints
                .iter()
                .map(|endpoint| endpoint.token)
                .collect();
            let mut identities: Vec<_> = candidate
                .endpoints
                .iter()
                .map(|endpoint| endpoint.identity.clone())
                .collect();
            identities.sort();
            // An unchanged token generation is the same live connection; reconnection needs nonempty identity equality.
            let restore = !candidate.same_identity
                || self.history.get(&key).is_none_or(|history| {
                    history.tokens == tokens
                        || (history.identities.len() == identities.len()
                            && history
                                .identities
                                .iter()
                                .zip(&identities)
                                .all(|(old, new)| old.is_some() && old == new))
                });
            if !restore {
                let error = DriverError::new(
                    ErrorCode::DeviceUnavailable,
                    "Device identity cannot safely restore this driver's retired claim.",
                );
                let device = self.stale(&candidate, &error);
                self.set_report(owner, device);
                let status = self.status_mut(owner);
                status.state = DriverState::Disconnected;
                status.error = Some(exposed_error(&error));
                return Ok(());
            }
            self.epoch += 1;
            let grant =
                self.gates
                    .activate(owner.to_owned(), id.to_owned(), self.epoch, tokens.clone())?;
            let runtime = &self.runtimes[owner];
            let (reply, receiver) = oneshot::channel();
            let activation = runtime.sender.try_send(RuntimeCommand::Activate {
                serial: candidate.serial,
                grant: grant.clone(),
                reply,
            });
            let result = if activation.is_err() {
                Err(Failure {
                    error: busy(),
                    fatal: false,
                })
            } else {
                match tokio::time::timeout(CALLBACK_LIMIT, receiver).await {
                    Ok(Ok(result)) => result,
                    _ => Err(Failure::fatal(timeout_error())),
                }
            };
            let result = result.and_then(|device| {
                self.check_report(owner, &device).map_err(Failure::fatal)?;
                Ok(device)
            });
            let device = match result {
                Ok(device) => device,
                Err(failure) => {
                    grant.revoke();
                    grant.drain(Instant::now() + CALLBACK_LIMIT).await?;
                    if failure.fatal {
                        self.quarantine(owner, &failure.error).await;
                    }
                    let device = self.stale(&candidate, &failure.error);
                    self.set_report(owner, device);
                    return Ok(());
                }
            };
            self.history.insert(key, History { tokens, identities });
            self.routes.insert(
                id.to_owned(),
                Route {
                    owner: owner.to_owned(),
                    candidate,
                    grant,
                    device,
                    sequence: 0,
                },
            );
        }
        if let Some(device) = self.routes.get(id).map(|route| route.device.clone()) {
            self.set_report(owner, device);
        }
        Ok(())
    }
    fn report_conflicts(&mut self, conflicts: &[(String, Candidate)]) {
        for (owner, candidate) in conflicts {
            let error = DriverError::new(
                ErrorCode::DriverConflict,
                "Installed drivers have conflicting device or endpoint claims.",
            );
            let status = self.status_mut(owner);
            status.state = DriverState::Ambiguous;
            status.error = Some(exposed_error(&error));
            // This is a reporting representative only. No directory-order write grant is awarded.
            if !self
                .reports
                .values()
                .flatten()
                .any(|device| device.id == candidate.device.id)
            {
                let device = self.stale(candidate, &error);
                self.reports.get_mut(owner).expect("report").push(device);
            }
        }
    }
    fn stale(&self, candidate: &Candidate, error: &DriverError) -> Device {
        let mut device = candidate.device.clone();
        let previous = self
            .routes
            .get(&device.id)
            .filter(|route| route.owner == device.driver)
            .map(|route| &route.device)
            .or_else(|| self.last.get(&(device.driver.clone(), device.id.clone())));
        if let Some(previous) = previous {
            for characteristic in &mut device.characteristics {
                if let Some(old) = previous
                    .characteristics
                    .iter()
                    .find(|old| old.id == characteristic.id)
                {
                    characteristic.value = old.value.clone();
                }
            }
        }
        unavailable(&mut device, error);
        device
    }
    /// Every package has a status row for the coordinator's entire lifetime.
    fn status_mut(&mut self, id: &str) -> &mut DriverStatus {
        self.statuses.get_mut(id).expect("status")
    }
    fn set_report(&mut self, owner: &str, device: Device) {
        let report = self.reports.get_mut(owner).expect("driver report");
        if let Some(previous) = report.iter_mut().find(|previous| previous.id == device.id) {
            *previous = device;
        } else {
            report.push(device);
        }
    }
    fn refresh_bindings(&self) {
        let bindings = self
            .routes
            .iter()
            .map(|(device_id, route)| {
                let mut endpoints: Vec<_> = route
                    .candidate
                    .endpoints
                    .iter()
                    .map(|endpoint| endpoint.token)
                    .collect();
                endpoints.sort_unstable();
                endpoints.dedup();
                (
                    device_id.clone(),
                    Binding {
                        driver: route.owner.clone(),
                        endpoints,
                    },
                )
            })
            .collect();
        self.binding_updates.send_replace(Arc::new(bindings));
    }
    fn publish(&self) -> Result<(), DriverError> {
        self.store.publish_driver_states(
            self.statuses
                .iter()
                .map(|(id, status)| {
                    (
                        status.clone(),
                        self.reports.get(id).cloned().unwrap_or_default(),
                    )
                })
                .collect(),
        )?;
        self.refresh_bindings();
        Ok(())
    }
    async fn retire(&mut self, id: &str, deadline: Instant) -> Result<(), DriverError> {
        let Some(route) = self.routes.remove(id) else {
            return Ok(());
        };
        self.refresh_bindings();
        self.last
            .insert((route.owner.clone(), id.into()), route.device.clone());
        route.grant.revoke();
        let mut failure = None;
        if let Some(runtime) = self.runtimes.get(&route.owner) {
            let (reply, receiver) = oneshot::channel();
            if runtime
                .sender
                .try_send(RuntimeCommand::Retire {
                    id: id.into(),
                    deadline,
                    reply,
                })
                .is_ok()
            {
                match tokio::time::timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    receiver,
                )
                .await
                {
                    Ok(Ok(Ok(()))) => {}
                    Ok(Ok(Err(error))) => failure = Some(error),
                    _ => {
                        failure = Some(Failure {
                            error: timeout_error(),
                            fatal: false,
                        })
                    }
                }
            } else {
                failure = Some(Failure {
                    error: busy(),
                    fatal: false,
                });
            }
        }
        if let Some(failure) = &failure {
            tracing::warn!(target: "synapsed::plugins", public_message = %"Plugin retirement callback failed.", code = ?failure.error.code);
            if failure.fatal {
                self.revoke_failure(&route.owner, &failure.error);
            }
        }
        route.grant.drain(deadline).await?;
        match failure {
            Some(failure) if failure.fatal => Err(failure.error),
            _ => Ok(()),
        }
    }
    fn revoke_failure(&mut self, owner: &str, error: &DriverError) -> Vec<String> {
        let mut affected = BTreeSet::from([owner.to_owned()]);
        loop {
            let next: Vec<_> = self
                .packages
                .iter()
                .filter(|(id, package)| {
                    !affected.contains(*id)
                        && package
                            .manifest
                            .dependencies
                            .iter()
                            .any(|dependency| affected.contains(dependency))
                })
                .map(|(id, _)| id.clone())
                .collect();
            if next.is_empty() {
                break;
            }
            affected.extend(next);
        }
        for id in &affected {
            let failure = if id == owner {
                error.clone()
            } else {
                DriverError::new(
                    ErrorCode::PluginError,
                    "Plugin dependency failed during initialization or execution.",
                )
            };
            self.quarantined.insert(id.clone());
            self.failed.insert(id.clone(), failure.clone());
            let status = self.status_mut(id);
            status.state = if id == owner {
                DriverState::Error
            } else {
                DriverState::Disabled
            };
            status.error = Some(exposed_error(&failure));
        }
        let ids: Vec<_> = self
            .routes
            .iter()
            .filter(|(_, route)| affected.contains(&route.owner))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            let route = self.routes.get_mut(id).expect("affected route");
            route.grant.revoke();
            unavailable(&mut route.device, &self.failed[&route.owner]);
        }
        for id in affected {
            if let Some(devices) = self.reports.get_mut(&id) {
                for device in devices {
                    unavailable(device, &self.failed[&id]);
                }
            }
        }
        let _ = self.publish();
        ids
    }
    async fn quarantine(&mut self, owner: &str, error: &DriverError) {
        let ids = self.revoke_failure(owner, error);
        let deadline = Instant::now() + CALLBACK_LIMIT;
        for id in ids {
            if let Err(error) = self.retire(&id, deadline).await {
                tracing::error!(target: "synapsed::plugins", public_message = %"Quarantined native work did not drain before deadline.", code = ?error.code);
            }
        }
        self.refresh_bindings();
    }
    async fn dispatch(
        &mut self,
        id: &str,
        characteristic: &str,
        command: CharacteristicCommand,
        deadline: Instant,
        cancellation: Arc<Cancellation>,
    ) -> Result<Arc<Snapshot>, DriverError> {
        let snapshot = self.store.snapshot();
        let device = snapshot
            .devices
            .iter()
            .find(|device| device.id == id)
            .ok_or_else(|| DriverError::new(ErrorCode::UnknownDevice, "Device is not declared."))?;
        let selected = device
            .characteristics
            .iter()
            .find(|item| item.id == characteristic)
            .ok_or_else(|| {
                DriverError::new(
                    ErrorCode::UnknownCharacteristic,
                    "Characteristic is not declared.",
                )
            })?;
        if device.status != DeviceStatus::Online {
            return Err(unavailable_error());
        }
        selected.validate_command(&command)?;
        if cancellation.abandoned.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err(timeout_error());
        }
        let route = self.routes.get(id).ok_or_else(unavailable_error)?.clone();
        let reads = self.registry.reads(&self.packages[&route.owner].manifest)?;
        let writes = route
            .candidate
            .writes
            .get(characteristic)
            .cloned()
            .unwrap_or_default();
        let guard = Guard::new(deadline);
        let (reply, receiver) = oneshot::channel();
        if let Err(error) = self.runtimes[&route.owner]
            .sender
            .try_send(RuntimeCommand::Write {
                id: id.into(),
                characteristic: characteristic.into(),
                command,
                grant: route.grant.clone(),
                reads,
                writes,
                guard: guard.clone(),
                cancellation: cancellation.clone(),
                current: self.current.clone(),
                reply,
            })
        {
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => busy(),
                _ => unavailable_error(),
            });
        }
        let result = match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            receiver,
        )
        .await
        {
            Ok(Ok(result)) => result,
            _ => {
                guard.trip();
                if let Some(operation) = cancellation
                    .operation
                    .lock()
                    .expect("operation cancellation")
                    .as_ref()
                {
                    operation.cancel();
                    operation.finish();
                }
                Err(Failure::fatal(timeout_error()))
            }
        };
        let sequence = cancellation
            .operation
            .lock()
            .expect("operation cancellation")
            .as_ref()
            .map_or(0, OperationContext::sequence);
        let result = result.and_then(|device| {
            self.check_report(&route.owner, &device)
                .map_err(Failure::fatal)?;
            Ok(device)
        });
        match result {
            Ok(device) => {
                if let Some(current) = self.routes.get_mut(id)
                    && current.grant.epoch() == route.grant.epoch()
                {
                    current.device = device.clone();
                    current.sequence = sequence;
                }
                if let Some(report) = self
                    .reports
                    .get_mut(&route.owner)
                    .and_then(|devices| devices.iter_mut().find(|device| device.id == id))
                {
                    *report = device;
                }
                self.publish()?;
                Ok(self.store.snapshot())
            }
            Err(failure) => {
                if let Some(current) = self.routes.get_mut(id) {
                    unavailable(&mut current.device, &failure.error);
                    current.sequence = current.sequence.max(sequence);
                }
                if let Some(device) = self
                    .reports
                    .get_mut(&route.owner)
                    .and_then(|devices| devices.iter_mut().find(|device| device.id == id))
                {
                    unavailable(device, &failure.error);
                }
                if failure.fatal {
                    self.quarantine(&route.owner, &failure.error).await;
                }
                self.log_dispatch_outcomes(&cancellation);
                let _ = self.publish();
                Err(failure.error)
            }
        }
    }
    fn log_dispatch_outcomes(&self, cancellation: &Cancellation) {
        let operation = cancellation
            .operation
            .lock()
            .expect("operation cancellation");
        if let Some(operation) = operation.as_ref() {
            for outcome in operation.outcomes() {
                if outcome.observed.is_some() && outcome.error.is_none() {
                    tracing::info!(target: "synapsed::plugins", public_message = %"Native write completion retained after callback failure.");
                } else {
                    tracing::warn!(target: "synapsed::plugins", public_message = %"Native write confirmation remained unavailable after callback failure.");
                }
            }
        }
    }
    async fn observation(&mut self, event: RuntimeEvent) {
        let fatal = event.result.as_ref().is_err_and(|failure| failure.fatal);
        if self.routes.get(&event.id).is_none_or(|route| {
            route.owner != event.owner
                || route.grant.epoch() != event.epoch
                || (!fatal && event.sequence != 0 && event.sequence <= route.sequence)
        }) {
            return;
        }
        let result = event.result.and_then(|device| {
            self.check_report(&event.owner, &device)
                .map_err(Failure::fatal)?;
            Ok(device)
        });
        let route = self
            .routes
            .get_mut(&event.id)
            .expect("current observed route");
        route.sequence = route.sequence.max(event.sequence);
        match result {
            Ok(device) => {
                route.device = device.clone();
                if let Some(report) = self
                    .reports
                    .get_mut(&event.owner)
                    .and_then(|devices| devices.iter_mut().find(|device| device.id == event.id))
                {
                    *report = device;
                }
            }
            Err(failure) => {
                unavailable(&mut route.device, &failure.error);
                if let Some(device) = self
                    .reports
                    .get_mut(&event.owner)
                    .and_then(|devices| devices.iter_mut().find(|device| device.id == event.id))
                {
                    unavailable(device, &failure.error);
                }
                if failure.fatal {
                    self.quarantine(&event.owner, &failure.error).await;
                }
            }
        }
        if let Err(error) = self.publish() {
            self.quarantine(&event.owner, &error).await;
            let _ = self.publish();
        }
    }
    async fn stop(&mut self, deadline: Instant) -> Result<(), DriverError> {
        // Cancel scheduled device work before invoking bounded lifecycle shutdown.
        let ids: Vec<_> = self.routes.keys().cloned().collect();
        let mut error = None;
        for id in ids {
            if let Err(failure) = self.retire(&id, deadline).await {
                error = Some(failure);
            }
        }
        for runtime in self.runtimes.values() {
            let (reply, receiver) = oneshot::channel();
            if runtime
                .sender
                .try_send(RuntimeCommand::Shutdown { deadline, reply })
                .is_ok()
            {
                match tokio::time::timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    receiver,
                )
                .await
                {
                    Ok(Ok(Ok(()))) => {}
                    _ => error = Some(timeout_error()),
                }
            }
        }
        if let Err(failure) = self.registry.shutdown(deadline).await {
            error = Some(failure);
        }
        if let Some(error) = error {
            tracing::error!(target: "synapsed::plugins", public_message = %"Shutdown left an unknown callback or native outcome.", code = ?error.code);
            Err(error)
        } else {
            Ok(())
        }
    }
}
fn busy() -> DriverError {
    DriverError::new(ErrorCode::Busy, "Plugin command mailbox is full.")
}
fn unavailable_error() -> DriverError {
    DriverError::new(ErrorCode::DeviceUnavailable, "Device owner is unavailable.")
}
fn timeout_error() -> DriverError {
    DriverError::new(
        ErrorCode::Timeout,
        "Plugin operation exceeded its deadline.",
    )
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn plugin_catalog_projects_benq_inventory_match() {
        let manifest: manifest::Manifest =
            serde_json::from_str(include_str!("../../../drivers/benq-rd280u/plugin.json")).unwrap();
        let catalog = Catalog {
            packages: BTreeMap::from([(
                manifest.id.clone(),
                Package {
                    manifest,
                    sources: Arc::new(BTreeMap::new()),
                },
            )]),
            statuses: BTreeMap::new(),
        };
        let entries = catalog_entries(&catalog);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "benq-rd280u");
        assert_eq!(entries[0].version, "0.1.0");
        assert_eq!(entries[0].supersedes, vec!["generic-ddc".to_owned()]);
        assert_eq!(
            entries[0].inventory_matches,
            vec![InventoryMatch::Usb {
                vendor_id: "0bda".to_owned(),
                product_id: "1100".to_owned(),
                role: InventoryRole::MonitorCompanion,
            }]
        );
    }

    #[tokio::test]
    async fn published_bindings_follow_active_routes_and_retirement() {
        let store = StateStore::new(synapsed_api::NodeInfo {
            node_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".to_owned(),
            instance_id: "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff".to_owned(),
            name: "Bindings test host".to_owned(),
        })
        .unwrap();
        let token = EndpointToken {
            id: 7,
            generation: 2,
        };
        let endpoint = EndpointDescriptor {
            token,
            capability: "ddc".to_owned(),
            manufacturer: None,
            product: None,
            product_name: None,
            identity: None,
        };
        let device = Device {
            id: "monitor".to_owned(),
            driver: "fixture-driver".to_owned(),
            kind: "display".to_owned(),
            label: "Fixture display".to_owned(),
            status: DeviceStatus::Online,
            error: None,
            groups: vec![],
            characteristics: Vec::new(),
        };
        let candidate = Candidate {
            serial: 1,
            device: device.clone(),
            endpoints: vec![endpoint],
            same_identity: false,
            poll_ms: None,
            writes: BTreeMap::new(),
        };
        let gates = EndpointGates::default();
        let grant = gates
            .activate("fixture-driver", "monitor", 1, vec![token])
            .unwrap();
        let (binding_updates, bindings) = watch::channel(Arc::new(BTreeMap::new()));
        let mut coordinator = Coordinator {
            store,
            registry: CapabilityRegistry::default(),
            packages: BTreeMap::new(),
            statuses: BTreeMap::new(),
            runtimes: BTreeMap::new(),
            candidates: BTreeMap::new(),
            failed: BTreeMap::new(),
            quarantined: BTreeSet::new(),
            routes: BTreeMap::from([(
                "monitor".to_owned(),
                Route {
                    owner: "fixture-driver".to_owned(),
                    candidate,
                    grant,
                    device,
                    sequence: 0,
                },
            )]),
            history: BTreeMap::new(),
            gates,
            epoch: 1,
            reports: BTreeMap::new(),
            last: BTreeMap::new(),
            dominance: BTreeMap::new(),
            current: Arc::new(Mutex::new(None)),
            binding_updates,
            #[cfg(debug_assertions)]
            discovery_round: 0,
        };
        coordinator.publish().unwrap();
        let published = bindings.borrow().clone();
        assert_eq!(published.len(), 1);
        assert_eq!(published["monitor"].driver, "fixture-driver");
        assert_eq!(published["monitor"].endpoints, vec![token]);

        coordinator
            .retire("monitor", Instant::now() + CALLBACK_LIMIT)
            .await
            .unwrap();
        assert!(bindings.borrow().is_empty());
    }
    #[tokio::test]
    async fn plugin_host_publishes_manifest_catalog_without_starting_ddc() {
        let directory = tempfile::tempdir().unwrap();
        let plugins = directory.path().join("drivers");
        std::fs::create_dir(&plugins).unwrap();
        let driver_source = "return { configure = function() return { discover = function() return { devices = {} } end } end }";
        for (id, manifest) in [
            (
                "generic-ddc",
                r#"{"apiVersion":1,"id":"generic-ddc","version":"0.1.0","entry":"driver.lua","dependencies":[],"supersedes":[],"capabilities":{"ddc":1},"permissions":{"ddc":{"read":[],"write":[]}}}"#,
            ),
            (
                "benq-rd280u",
                include_str!("../../../drivers/benq-rd280u/plugin.json"),
            ),
        ] {
            let package = plugins.join(id);
            std::fs::create_dir(&package).unwrap();
            std::fs::write(package.join("plugin.json"), manifest).unwrap();
            std::fs::write(package.join("driver.lua"), driver_source).unwrap();
        }
        let store = StateStore::new(synapsed_api::NodeInfo {
            node_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".to_owned(),
            instance_id: "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff".to_owned(),
            name: "Catalog test host".to_owned(),
        })
        .unwrap();
        let ddc = synapsed_ddc::LazyDdc::new();
        let mut registry = CapabilityRegistry::default();
        registry
            .register(Arc::new(
                capabilities::ddc::DdcCapabilityFactory::with_shared(ddc.clone()),
            ))
            .unwrap();
        let host = PluginHost::start(
            store,
            PluginConfig {
                directory: plugins,
                directory_explicit: true,
                disabled: Vec::new(),
                discovery_interval_ms: 5000,
                settings: BTreeMap::new(),
            },
            registry,
        )
        .unwrap();
        let entries = host.catalog();
        assert!(entries.iter().any(|entry| {
            entry.id == "benq-rd280u"
                && entry.inventory_matches.contains(&InventoryMatch::Usb {
                    vendor_id: "0bda".to_owned(),
                    product_id: "1100".to_owned(),
                    role: InventoryRole::MonitorCompanion,
                })
        }));
        assert!(ddc.existing().is_none());
        host.shutdown(Instant::now() + CALLBACK_LIMIT)
            .await
            .unwrap();
    }
}
