#![warn(missing_docs)]

//! Native-only DDC owner and Send queue proxy. Handles and blocking I/O never leave the owner.
/// Linux-native DDC transport backend.
#[cfg(target_os = "linux")]
pub mod linux;
/// macOS-native DDC transport backend.
#[cfg(target_os = "macos")]
pub mod macos;
/// DDC/CI protocol framing and EDID parsing.
pub mod protocol;
/// Configurable in-memory DDC backend for tests.
#[cfg(feature = "test-support")]
pub mod test_support;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};
/// Shared error and endpoint identity types from the core crate.
pub use synapsed_core::{DriverError, EndpointToken, ErrorCode};
use synapsed_core::{OperationContext, OperationState, TransportOutcome};
use tokio::sync::{Notify, oneshot};

const MAILBOX_SIZE: usize = 64;
const OWNER_TICK: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ENDPOINTS: usize = 256;
const MAX_ENDPOINT_HISTORY: usize = 4096;
const MAX_FRESH_READS: usize = 4096;
static ENDPOINT_IDS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Current and maximum values observed for a VCP feature.
pub struct VcpValue {
    /// Currently observed value.
    pub current: u16,
    /// Maximum supported value reported by the display.
    pub maximum: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Public metadata for an endpoint discovered by a DDC scan.
pub struct EndpointDescriptor {
    /// Token used to authorize operations on this endpoint generation.
    pub token: EndpointToken,
    /// Capability identifier advertised for this endpoint.
    pub capability: String,
    /// Three-letter manufacturer code when valid EDID data is available.
    pub manufacturer: Option<String>,
    /// Manufacturer-specific product code when available.
    pub product: Option<u16>,
    /// Product name decoded from valid base EDID data, when available.
    pub product_name: Option<String>,
    /// Opaque stable identity derived from validated base EDID data.
    pub identity: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Public endpoint scan and any non-fatal scan errors.
pub struct EndpointScan {
    /// Discovered endpoint descriptors.
    pub endpoints: Vec<EndpointDescriptor>,
    /// Errors that did not prevent the rest of the scan from completing.
    pub errors: Vec<DriverError>,
}
/// Owner's most recent successful scan, including any non-fatal error state.
#[derive(Debug, Clone)]
pub struct CachedEndpointScan {
    /// Endpoints and owner-local placements from the completed scan.
    pub endpoints: Vec<(EndpointDescriptor, Option<Placement>)>,
    /// First non-fatal error, if the scan was only partially successful.
    pub error: Option<DriverError>,
}

/// Owner-local display placement; controller keys are never serialized by the daemon API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// DRM connector or framebuffer label.
    pub connector: String,
    /// Graphics-controller key used only by the inventory builder.
    pub controller_key: String,
}

/// Shared lazy handle for one DDC owner used by plugins and passive inventory.
#[derive(Clone, Default)]
pub struct LazyDdc(Arc<std::sync::Mutex<Option<DdcClient>>>);
impl LazyDdc {
    /// Creates an empty lazy handle without starting a native DDC owner.
    pub fn new() -> Self {
        Self::default()
    }
    /// Shares an already-started owner.
    pub fn with_client(client: DdcClient) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(client))))
    }
    /// Returns a clone of the existing client without starting an owner.
    pub fn existing(&self) -> Option<DdcClient> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Returns the shared client, starting the native owner only when first requested.
    pub fn get_or_start_native(&self) -> Result<DdcClient, DriverError> {
        let mut client = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if client.is_none() {
            *client = Some(DdcClient::start_native()?);
        }
        client.clone().ok_or_else(unavailable)
    }
}

/// Raw DDC transport operations, invoked only on the native owner thread.
pub trait DdcTransport {
    /// Read a VCP feature's current and maximum values.
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, DriverError>;
    /// Raw transport Set, including its post-Set delay. Confirmation belongs to the owner.
    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DriverError>;
}
/// Owner-local endpoint handle and metadata; native keys and transports are never serialized.
pub struct OwnedEndpoint {
    /// Opaque native key used only by the owner to track this endpoint.
    pub key: String,
    /// Validated manufacturer code, if available.
    pub manufacturer: Option<String>,
    /// Manufacturer-specific product code, if available.
    pub product: Option<u16>,
    /// Product name, if available.
    pub product_name: Option<String>,
    /// Stable identity derived from validated EDID data, if available.
    pub identity: Option<String>,
    /// Display placement discovered by the platform backend, if available.
    pub placement: Option<Placement>,
    /// Native transport handle that must remain on the owner thread.
    pub transport: Box<dyn DdcTransport>,
}
/// Owner-local results of a backend scan, including handles not exposed to clients.
pub struct InventoryWithOwnedHandles {
    /// Endpoint handles discovered by the scan.
    pub endpoints: Vec<OwnedEndpoint>,
    /// Non-fatal scan errors.
    pub errors: Vec<DriverError>,
}
/// Factory interface for scanning native DDC endpoints on the owner thread.
pub trait DdcBackend {
    /// Scan available endpoints and preserve ownership of their native transports.
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, DriverError>;
}

#[derive(Clone)]
/// Sendable queue proxy for the thread that owns native DDC handles and I/O.
pub struct DdcClient {
    inner: Arc<ClientInner>,
}
struct ClientInner {
    sender: SyncSender<Request>,
    signals: Arc<Signals>,
}
struct Signals {
    stopping: AtomicBool,
    done: AtomicBool,
    completed: Notify,
}
impl Drop for ClientInner {
    fn drop(&mut self) {
        self.signals.stopping.store(true, Ordering::SeqCst);
    }
}
type Reply<T> = oneshot::Sender<Result<T, DriverError>>;
enum Request {
    Enumerate {
        deadline: Instant,
        reply: Reply<EndpointScan>,
    },
    LastScan {
        reply: Reply<Option<CachedEndpointScan>>,
    },
    Get {
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        reply: Reply<VcpValue>,
    },
    Set {
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        value: u16,
        reply: Reply<VcpValue>,
    },
}
impl DdcClient {
    /// Called lazily by an installed capability factory, never by empty-catalog startup.
    pub fn start_native() -> Result<Self, DriverError> {
        #[cfg(target_os = "macos")]
        {
            Self::start_with_backend(macos::MacosBackend::new)
        }
        #[cfg(target_os = "linux")]
        {
            Self::start_with_backend(linux::LinuxBackend::new)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(DriverError::new(
                ErrorCode::UnsupportedCapability,
                "DDC is not supported on this platform.",
            ))
        }
    }
    /// Start an owner using a backend factory confined to its owner thread.
    pub fn start_with_backend<B, F>(factory: F) -> Result<Self, DriverError>
    where
        B: DdcBackend + 'static,
        F: FnOnce() -> Result<B, DriverError> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(MAILBOX_SIZE);
        let signals = Arc::new(Signals {
            stopping: AtomicBool::new(false),
            done: AtomicBool::new(false),
            completed: Notify::new(),
        });
        let worker_signals = signals.clone();
        thread::Builder::new()
            .name("synapsed-ddc".into())
            .spawn(move || {
                // B and every handle are constructed and destroyed on this thread; B need not be Send.
                let mut backend = factory();
                let mut owner = Owner::default();
                while !worker_signals.stopping.load(Ordering::SeqCst) {
                    owner.prune_reads();
                    match receiver.recv_timeout(OWNER_TICK) {
                        Ok(request) => {
                            if worker_signals.stopping.load(Ordering::SeqCst) {
                                reject(request, unavailable());
                            } else {
                                match backend.as_mut() {
                                    Ok(backend) => owner.handle(backend, request),
                                    Err(error) => reject(request, error.clone()),
                                }
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Started native work completed synchronously before this point; reject only queued work.
                while let Ok(request) = receiver.try_recv() {
                    reject(request, unavailable());
                }
                drop(owner);
                drop(backend);
                worker_signals.done.store(true, Ordering::SeqCst);
                worker_signals.completed.notify_waiters();
            })
            .map_err(|_| {
                DriverError::new(
                    ErrorCode::DeviceUnavailable,
                    "Could not start native DDC owner.",
                )
            })?;
        Ok(Self {
            inner: Arc::new(ClientInner { sender, signals }),
        })
    }
    fn enqueue(&self, request: Request) -> Result<(), DriverError> {
        if self.inner.signals.stopping.load(Ordering::SeqCst) {
            return Err(unavailable());
        }
        self.inner
            .sender
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => busy("Native DDC queue is full."),
                TrySendError::Disconnected(_) => unavailable(),
            })
    }
    /// Enumerate DDC endpoints and return any non-fatal scan errors.
    pub async fn enumerate(&self) -> Result<EndpointScan, DriverError> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let (reply, receiver) = oneshot::channel();
        self.enqueue(Request::Enumerate { deadline, reply })?;
        receive(receiver, deadline).await
    }
    /// Return the owner's last scan without initiating a scan or native I/O.
    pub async fn last_scan(&self) -> Result<Option<CachedEndpointScan>, DriverError> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let (reply, receiver) = oneshot::channel();
        self.enqueue(Request::LastScan { reply })?;
        receive(receiver, deadline).await
    }
    fn resolve_endpoint(
        context: &OperationContext,
        endpoint: EndpointToken,
        code: u8,
        write: bool,
    ) -> Result<(), DriverError> {
        context.validate(endpoint, code, write)
    }
    async fn transact<T>(
        &self,
        context: &OperationContext,
        request: impl FnOnce(Reply<T>) -> Request,
    ) -> Result<T, DriverError> {
        let deadline = context.deadline();
        let (reply, receiver) = oneshot::channel();
        self.enqueue(request(reply))?;
        let mut abandonment = Abandonment {
            context,
            armed: true,
        };
        let result = receive(receiver, deadline).await;
        abandonment.armed = false;
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::Timeout)
        {
            context.cancel();
        }
        result
    }
    /// Read a VCP feature after validating the operation grant.
    pub async fn get_vcp(
        &self,
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
    ) -> Result<VcpValue, DriverError> {
        Self::resolve_endpoint(&context, endpoint, code, false)?;
        self.transact(&context, |reply| Request::Get {
            context: context.clone(),
            endpoint,
            code,
            reply,
        })
        .await
    }
    /// Write a VCP feature and return the confirmed readback value.
    pub async fn set_vcp(
        &self,
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        value: u16,
    ) -> Result<VcpValue, DriverError> {
        Self::resolve_endpoint(&context, endpoint, code, true)?;
        self.transact(&context, |reply| Request::Set {
            context: context.clone(),
            endpoint,
            code,
            value,
            reply,
        })
        .await
    }
    /// Stop admission, drain any already-started I/O, and bound waiting without joining stuck code.
    pub async fn shutdown(&self, deadline: Instant) -> Result<(), DriverError> {
        self.inner.signals.stopping.store(true, Ordering::SeqCst);
        loop {
            let notified = self.inner.signals.completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.signals.done.load(Ordering::SeqCst) {
                return Ok(());
            }
            tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), notified)
                .await
                .map_err(|_| timeout())?;
        }
    }
}
struct Abandonment<'a> {
    context: &'a OperationContext,
    armed: bool,
}
impl Drop for Abandonment<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.context.abandon();
        }
    }
}
async fn receive<T>(
    receiver: oneshot::Receiver<Result<T, DriverError>>,
    deadline: Instant,
) -> Result<T, DriverError> {
    tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), receiver)
        .await
        .map_err(|_| timeout())?
        .map_err(|_| unavailable())?
}
fn reject(request: Request, error: DriverError) {
    match request {
        Request::Enumerate { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        Request::LastScan { reply } => {
            let _ = reply.send(Err(error));
        }
        Request::Get { context, reply, .. } | Request::Set { context, reply, .. } => {
            context.cancel();
            let _ = reply.send(Err(error));
        }
    }
}
struct EndpointRecord {
    token: EndpointToken,
    identity: Option<String>,
    present: bool,
}
struct NativeEndpoint {
    token: EndpointToken,
    transport: Box<dyn DdcTransport>,
}
struct FreshRead {
    value: VcpValue,
    context: OperationContext,
}
#[derive(Default)]
struct Owner {
    last_scan: Option<CachedEndpointScan>,
    last_scan_error: Option<DriverError>,
    records: HashMap<String, EndpointRecord>,
    endpoints: HashMap<u64, NativeEndpoint>,
    fresh: HashMap<(u64, EndpointToken, u8), FreshRead>,
}
impl Owner {
    fn prune_reads(&mut self) {
        self.fresh.retain(|_, read| {
            Instant::now() < read.context.deadline()
                && !matches!(
                    read.context.state(),
                    OperationState::Completed | OperationState::Cancelled
                )
        });
    }
    fn invalidate_scan(&mut self, error: DriverError) {
        self.last_scan = None;
        self.last_scan_error = Some(error);
        self.endpoints.clear();
        self.fresh.clear();
        for record in self.records.values_mut() {
            record.present = false;
        }
    }
    fn scan<B: DdcBackend>(&mut self, backend: &mut B) -> Result<EndpointScan, DriverError> {
        let scanned = match backend.scan() {
            Ok(scanned) => scanned,
            Err(error) => {
                self.invalidate_scan(error.clone());
                return Err(error);
            }
        };
        if scanned.endpoints.len() > MAX_ENDPOINTS
            || self.records.len()
                + scanned
                    .endpoints
                    .iter()
                    .filter(|endpoint| !self.records.contains_key(&endpoint.key))
                    .count()
                > MAX_ENDPOINT_HISTORY
        {
            let error = busy("Native DDC endpoint inventory exceeds its limit.");
            self.invalidate_scan(error.clone());
            return Err(error);
        }
        let mut keys = std::collections::HashSet::with_capacity(scanned.endpoints.len());
        if scanned
            .endpoints
            .iter()
            .any(|endpoint| !keys.insert(&endpoint.key))
        {
            let error = protocol::ddc_error("Native DDC scan returned duplicate endpoints.");
            self.invalidate_scan(error.clone());
            return Err(error);
        }
        drop(keys);
        let mut endpoints = HashMap::with_capacity(scanned.endpoints.len());
        let mut descriptors = Vec::with_capacity(scanned.endpoints.len());
        let mut cached = Vec::with_capacity(scanned.endpoints.len());
        for endpoint in scanned.endpoints {
            let record = self
                .records
                .entry(endpoint.key)
                .or_insert_with(|| EndpointRecord {
                    token: EndpointToken {
                        id: ENDPOINT_IDS.fetch_add(1, Ordering::Relaxed),
                        generation: 0,
                    },
                    identity: None,
                    present: false,
                });
            if !record.present || record.identity != endpoint.identity {
                record.token.generation += 1;
            }
            record.present = true;
            record.identity = endpoint.identity.clone();
            let descriptor = EndpointDescriptor {
                token: record.token,
                capability: "ddc".into(),
                manufacturer: endpoint.manufacturer,
                product: endpoint.product,
                product_name: endpoint.product_name,
                identity: endpoint.identity,
            };
            cached.push((descriptor.clone(), endpoint.placement));
            descriptors.push(descriptor);
            endpoints.insert(
                record.token.id,
                NativeEndpoint {
                    token: record.token,
                    transport: endpoint.transport,
                },
            );
        }
        for record in self.records.values_mut() {
            if !endpoints.contains_key(&record.token.id) {
                record.present = false;
            }
        }
        self.endpoints = endpoints;
        self.fresh.retain(|(_, token, _), _| {
            self.endpoints
                .get(&token.id)
                .is_some_and(|endpoint| endpoint.token == *token)
        });
        descriptors.sort_unstable_by_key(|descriptor| descriptor.token);
        cached.sort_unstable_by_key(|(descriptor, _)| descriptor.token);
        self.last_scan = Some(CachedEndpointScan {
            endpoints: cached,
            error: scanned.errors.first().cloned(),
        });
        self.last_scan_error = None;
        Ok(EndpointScan {
            endpoints: descriptors,
            errors: scanned.errors,
        })
    }
    fn endpoint(&mut self, token: EndpointToken) -> Result<&mut NativeEndpoint, DriverError> {
        self.endpoints
            .get_mut(&token.id)
            .filter(|endpoint| endpoint.token == token)
            .ok_or_else(unavailable)
    }
    fn get(
        &mut self,
        context: &OperationContext,
        endpoint: EndpointToken,
        code: u8,
    ) -> Result<VcpValue, DriverError> {
        let _native = context.begin_native(endpoint, code, false)?;
        let key = (context.sequence(), endpoint, code);
        self.fresh.remove(&key);
        let value = self.endpoint(endpoint)?.transport.get_vcp(code)?;
        context.validate(endpoint, code, false)?;
        if self.fresh.len() >= MAX_FRESH_READS {
            return Err(busy("Native DDC transaction read limit reached."));
        }
        self.fresh.insert(
            key,
            FreshRead {
                value,
                context: context.clone(),
            },
        );
        Ok(value)
    }
    fn set(
        &mut self,
        context: &OperationContext,
        endpoint: EndpointToken,
        code: u8,
        requested: u16,
    ) -> Result<VcpValue, DriverError> {
        let _native = context.begin_native(endpoint, code, true)?;
        let key = (context.sequence(), endpoint, code);
        let previous = self
            .fresh
            .get(&key)
            .ok_or_else(|| {
                DriverError::new(
                    ErrorCode::PermissionDenied,
                    "DDC Set requires a fresh Get in this transaction.",
                )
            })?
            .value;
        let mask = context.change_mask(code).ok_or_else(|| {
            DriverError::new(
                ErrorCode::PermissionDenied,
                "DDC write has no granted change mask.",
            )
        })?;
        if (requested ^ previous.current) & !mask != 0 {
            return Err(DriverError::new(
                ErrorCode::PermissionDenied,
                "DDC write changes bits outside its granted mask.",
            ));
        }
        let native = self.endpoint(endpoint)?;
        if requested == previous.current {
            return Ok(previous);
        }
        context.admit_write(endpoint, code)?;
        // No context/receiver checks after admission: finish Set + delay + actual readback even when abandoned.
        let set_result = native.transport.set_vcp(code, requested);
        // A failed native Set can have an unknown hardware outcome. Still wait and inspect it.
        if set_result.is_err() {
            thread::sleep(protocol::IO_DELAY);
        }
        let readback = native.transport.get_vcp(code);
        let validity = context.validate(endpoint, code, true);
        let error = set_result
            .as_ref()
            .err()
            .cloned()
            .or_else(|| readback.as_ref().err().cloned())
            .or_else(|| validity.as_ref().err().cloned());
        context.record_outcome(TransportOutcome {
            endpoint,
            code,
            requested,
            observed: readback
                .as_ref()
                .ok()
                .map(|value| (value.current, value.maximum)),
            error,
        });
        self.fresh.remove(&key);
        set_result?;
        let value = readback?;
        validity?;
        self.fresh.insert(
            key,
            FreshRead {
                value,
                context: context.clone(),
            },
        );
        Ok(value)
    }
    fn handle<B: DdcBackend>(&mut self, backend: &mut B, request: Request) {
        match request {
            Request::Enumerate { deadline, reply } => {
                let result = if Instant::now() >= deadline {
                    Err(timeout())
                } else {
                    self.scan(backend)
                };
                let result = if Instant::now() >= deadline {
                    Err(timeout())
                } else {
                    result
                };
                let _ = reply.send(result);
            }
            Request::LastScan { reply } => {
                let result = match &self.last_scan_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(self.last_scan.clone()),
                };
                let _ = reply.send(result);
            }
            Request::Get {
                context,
                endpoint,
                code,
                reply,
            } => {
                let result = self.get(&context, endpoint, code);
                let _ = reply.send(result);
            }
            Request::Set {
                context,
                endpoint,
                code,
                value,
                reply,
            } => {
                let result = self.set(&context, endpoint, code, value);
                let _ = reply.send(result);
            }
        }
    }
}
fn unavailable() -> DriverError {
    DriverError::new(
        ErrorCode::DeviceUnavailable,
        "Native DDC endpoint or owner is unavailable.",
    )
}
fn timeout() -> DriverError {
    DriverError::new(
        ErrorCode::Timeout,
        "Native DDC operation timed out or was cancelled.",
    )
}
fn busy(message: &'static str) -> DriverError {
    DriverError::new(ErrorCode::Busy, message)
}
