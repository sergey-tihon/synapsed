//! Trusted installed Lua device packages. Rust owns transport grants, never device policy.
pub mod loader;
pub mod manifest;
pub mod registry;
pub mod runtime;
pub mod capabilities {
    pub mod ddc;
}
pub use registry::{CapabilityEventStream, CapabilityRegistry, LuaCapabilityFactory};
pub use runtime::{InvocationContext, InvocationGuard};
pub use synapsed_ddc::EndpointDescriptor;
#[cfg(test)]
extern crate self as synapsed_plugins;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

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
    CharacteristicCommand, Device, DeviceStatus, DriverState, DriverStatus, Snapshot,
};
use synapsed_core::{
    DeviceGrant, DriverError, EndpointGates, EndpointToken, ErrorCode, OperationContext, StateStore,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct PluginConfig {
    pub directory: PathBuf,
    pub directory_explicit: bool,
    pub disabled: Vec<String>,
    pub discovery_interval_ms: u64,
    pub settings: BTreeMap<String, Value>,
}
impl PluginConfig {
    fn validate(&self) -> Result<(), DriverError> {
        if self.discovery_interval_ms < 1000 || self.disabled.len() > 16 || self.settings.len() > 16
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
            if !settings.is_object() || !bounded_settings(settings, 0, &mut 0) {
                return Err(invalid_config());
            }
            synapsed_api::validate_encoded_size(settings, 64 * 1024)?;
        }
        Ok(())
    }
}
fn bounded_settings(value: &Value, depth: usize, count: &mut usize) -> bool {
    *count += 1;
    if depth > 8 || *count > 4096 {
        return false;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .all(|value| bounded_settings(value, depth + 1, count)),
        Value::Object(values) => values.iter().all(|(key, value)| {
            key.chars().count() <= 128 && bounded_settings(value, depth + 1, count)
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
struct HostInner {
    commands: mpsc::Sender<HostCommand>,
    accepting: Arc<AtomicBool>,
    current: Arc<Mutex<Option<OperationContext>>>,
    store: StateStore,
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
    Shutdown {
        deadline: Instant,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
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
                api_version: 1,
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
        let (commands, receiver) = mpsc::channel(64);
        let worker_store = store.clone();
        let accepting = Arc::new(AtomicBool::new(true));
        let current = Arc::new(Mutex::new(None));
        let worker_accepting = accepting.clone();
        let worker_current = current.clone();
        std::thread::Builder::new()
            .name("synapsed-plugins".into())
            .spawn(move || {
                let runner = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build();
                let Ok(runner) = runner else {
                    return;
                };
                runner.block_on(Coordinator::run(
                    worker_store,
                    config,
                    registry,
                    catalog,
                    receiver,
                    worker_accepting,
                    worker_current,
                ));
            })
            .map_err(|_| {
                DriverError::new(
                    ErrorCode::PluginError,
                    "Plugin coordinator thread could not start.",
                )
            })?;
        Ok(Self {
            inner: Arc::new(HostInner {
                commands,
                accepting,
                current,
                store,
            }),
        })
    }
    pub async fn dispatch(
        &self,
        device: &str,
        characteristic: &str,
        command: CharacteristicCommand,
    ) -> Result<Arc<Snapshot>, DriverError> {
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
}
impl Coordinator {
    async fn run(
        store: StateStore,
        config: PluginConfig,
        registry: CapabilityRegistry,
        catalog: Catalog,
        mut commands: mpsc::Receiver<HostCommand>,
        accepting: Arc<AtomicBool>,
        current: Arc<Mutex<Option<OperationContext>>>,
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
                    let status = host.statuses.get_mut(id).expect("package status");
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
        let mut next = Instant::now() + Duration::from_millis(config.discovery_interval_ms);
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(HostCommand::Dispatch { device, characteristic, command, deadline, cancellation, reply }) => {
                        let result = host.dispatch(&device, &characteristic, command, deadline, cancellation).await;
                        let _ = reply.send(result);
                        // State events are drained before the next command, preventing command floods from starving observations.
                        while let Ok(event) = observed.try_recv() { host.observation(event).await; }
                    }
                    Some(HostCommand::Shutdown { deadline, reply }) => { let result = host.stop(deadline).await; let _ = reply.send(result); break; }
                    None => { let _ = host.stop(Instant::now() + CALLBACK_LIMIT).await; break; }
                },
                event = observed.recv(), if !observed.is_closed() => if let Some(event) = event { host.observation(event).await; },
                _ = tokio::time::sleep_until(next.into()) => { host.discover().await; next = Instant::now() + Duration::from_millis(config.discovery_interval_ms); },
            }
            if Instant::now() >= next {
                host.discover().await;
                next = Instant::now() + Duration::from_millis(config.discovery_interval_ms);
            }
        }
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
                    let status = self.statuses.get_mut(&id).expect("status");
                    status.state = DriverState::Online;
                    status.error = None;
                }
                Err(failure) => {
                    self.failed.insert(id.clone(), failure.error.clone());
                    if failure.fatal {
                        self.quarantine(&id, &failure.error).await;
                    } else {
                        let status = self.statuses.get_mut(&id).expect("status");
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
            tracing::error!(target: "plugins", code = ?error.code, "Plugin ownership publication failed.");
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
                self.statuses.get_mut(&id).expect("status").state = DriverState::Disabled;
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
            api_version: 1,
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
            // Preserve every retained failed report and every pending winner while
            // observations are acquired. Validation must see the complete cutover.
            let mut staged: BTreeMap<String, Vec<Device>> = self
                .statuses
                .keys()
                .map(|id| (id.clone(), vec![]))
                .collect();
            let mut represented = BTreeSet::new();
            let pending = DriverError::new(ErrorCode::DeviceUnavailable, StateStore::FAULT_MESSAGE);
            for (id, (owner, candidate)) in &winners {
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
            for (owner, candidate) in &conflicts {
                if represented.insert(candidate.device.id.clone()) {
                    staged
                        .get_mut(owner)
                        .expect("staged owner")
                        .push(self.stale(candidate, &conflict));
                }
            }
            let published = self.store.snapshot();
            for previous in &published.devices {
                if let Some(error) = self.failed.get(&previous.driver) {
                    if represented.insert(previous.id.clone()) {
                        let mut device = previous.clone();
                        unavailable(&mut device, error);
                        staged
                            .get_mut(&previous.driver)
                            .expect("retained owner")
                            .push(device);
                    }
                }
            }
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
                if self.quarantined.contains(&owner) {
                    continue;
                }
                if !self.routes.contains_key(&id) {
                    let key = (owner.clone(), id.clone());
                    if !self.history.contains_key(&key) && self.history.len() >= 4096 {
                        let error = DriverError::new(
                            ErrorCode::PluginError,
                            "Plugin device identity history is exhausted.",
                        );
                        self.quarantine(&owner, &error).await;
                        let device = self.stale(&candidate, &error);
                        self.set_report(&owner, device);
                        continue;
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
                        self.set_report(&owner, device);
                        let status = self.statuses.get_mut(&owner).expect("status");
                        status.state = DriverState::Disconnected;
                        status.error = Some(exposed_error(&error));
                        continue;
                    }
                    self.epoch += 1;
                    let grant = self.gates.activate(
                        owner.clone(),
                        id.clone(),
                        self.epoch,
                        tokens.clone(),
                    )?;
                    let runtime = &self.runtimes[&owner];
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
                        self.check_report(&owner, &device).map_err(Failure::fatal)?;
                        Ok(device)
                    });
                    let device = match result {
                        Ok(device) => device,
                        Err(failure) => {
                            grant.revoke();
                            grant.drain(Instant::now() + CALLBACK_LIMIT).await?;
                            if failure.fatal {
                                self.quarantine(&owner, &failure.error).await;
                            }
                            let device = self.stale(&candidate, &failure.error);
                            self.set_report(&owner, device);
                            continue;
                        }
                    };
                    self.history.insert(key, History { tokens, identities });
                    self.routes.insert(
                        id.clone(),
                        Route {
                            owner: owner.clone(),
                            candidate,
                            grant,
                            device,
                            sequence: 0,
                        },
                    );
                }
                if let Some(device) = self.routes.get(&id).map(|route| route.device.clone()) {
                    self.set_report(&owner, device);
                }
            }
            for (owner, candidate) in conflicts {
                let error = DriverError::new(
                    ErrorCode::DriverConflict,
                    "Installed drivers have conflicting device or endpoint claims.",
                );
                let status = self.statuses.get_mut(&owner).expect("status");
                status.state = DriverState::Ambiguous;
                status.error = Some(exposed_error(&error));
                // This is a reporting representative only. No directory-order write grant is awarded.
                if !self
                    .reports
                    .values()
                    .flatten()
                    .any(|device| device.id == candidate.device.id)
                {
                    let device = self.stale(&candidate, &error);
                    self.reports.get_mut(&owner).expect("report").push(device);
                }
            }
            let published = self.store.snapshot();
            for previous in &published.devices {
                if let Some(error) = self.failed.get(&previous.driver) {
                    if !self
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
    fn set_report(&mut self, owner: &str, device: Device) {
        let report = self.reports.get_mut(owner).expect("driver report");
        if let Some(previous) = report.iter_mut().find(|previous| previous.id == device.id) {
            *previous = device;
        } else {
            report.push(device);
        }
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
        Ok(())
    }
    async fn retire(&mut self, id: &str, deadline: Instant) -> Result<(), DriverError> {
        let Some(route) = self.routes.remove(id) else {
            return Ok(());
        };
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
            tracing::warn!(target: "plugins", code = ?failure.error.code, "Plugin retirement callback failed.");
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
            let status = self.statuses.get_mut(id).expect("status");
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
                tracing::error!(target: "plugins", code = ?error.code, "Quarantined native work did not drain before deadline.");
            }
        }
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
                if let Some(current) = self.routes.get_mut(id) {
                    if current.grant.epoch == route.grant.epoch {
                        current.device = device.clone();
                        current.sequence = sequence;
                    }
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
                if let Some(operation) = cancellation
                    .operation
                    .lock()
                    .expect("operation cancellation")
                    .as_ref()
                {
                    for outcome in operation.outcomes() {
                        if outcome.observed.is_some() && outcome.error.is_none() {
                            tracing::info!(target: "plugins", "Native write completion retained after callback failure.");
                        } else {
                            tracing::warn!(target: "plugins", "Native write confirmation remained unavailable after callback failure.");
                        }
                    }
                }
                let _ = self.publish();
                Err(failure.error)
            }
        }
    }
    async fn observation(&mut self, event: RuntimeEvent) {
        let fatal = event.result.as_ref().is_err_and(|failure| failure.fatal);
        if self.routes.get(&event.id).is_none_or(|route| {
            route.owner != event.owner
                || route.grant.epoch != event.epoch
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
            tracing::error!(target: "plugins", code = ?error.code, "Shutdown left an unknown callback or native outcome.");
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
