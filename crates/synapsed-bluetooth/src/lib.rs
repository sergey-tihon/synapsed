#![warn(missing_docs)]

//! Native Bluetooth Low Energy ownership and its sendable queue proxy.

use futures_util::future::LocalBoxFuture;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use synapsed_core::{
    DriverError, EndpointDescriptor, EndpointToken, ErrorCode, GrantLiveness, OperationContext,
    allocate_endpoint_id,
};
use tokio::{
    runtime::Builder,
    sync::{Notify, mpsc, oneshot},
    task::LocalSet,
    time::{MissedTickBehavior, interval},
};
/// UUID type used by Bluetooth service and characteristic declarations.
pub use uuid::Uuid;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native;
/// Configurable in-memory Bluetooth backend for tests.
#[cfg(feature = "test-support")]
pub mod test_support;

/// Capability identifier registered with the Lua plugin host.
pub const CAPABILITY: &str = "bluetooth";
const MAILBOX_SIZE: usize = 64;
const EVENT_QUEUE_SIZE: usize = 256;
const CONNECT_QUEUE_SIZE: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TRACKED: usize = 64;
const MAX_FILTERS: usize = 16;
const MAX_LISTENERS: usize = 32;
const MAX_AGE: Duration = Duration::from_secs(60);

/// Advertisement selectors registered by a capability instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFilter {
    /// Service UUIDs accepted by this filter.
    pub services: Vec<Uuid>,
    /// Case-insensitive prefixes accepted for an advertised name.
    pub name_prefixes: Vec<String>,
}

/// Discovered endpoint descriptors and non-fatal scan errors.
#[derive(Clone, Debug)]
pub struct EndpointScan {
    /// Endpoints matching the requested filter.
    pub endpoints: Vec<EndpointDescriptor>,
    /// Errors that did not prevent the scan from completing.
    pub errors: Vec<DriverError>,
}

/// A characteristic write repeated until stopped, revoked, or expired.
#[derive(Clone, Debug)]
pub struct RepeatSpec {
    /// Permission code authorizing the repeated write.
    pub code: u8,
    /// UUID receiving the repeated payload.
    pub characteristic: Uuid,
    /// Bytes written on each repeat.
    pub bytes: Vec<u8>,
    /// Delay between repeated writes.
    pub interval: Duration,
    /// Maximum lifetime of this repeat session.
    pub max_duration: Duration,
    /// Writes attempted in order when the session ends normally.
    pub finally: Vec<FinalWrite>,
}

/// One final write performed when a repeat session ends.
#[derive(Clone, Debug)]
pub struct FinalWrite {
    /// Permission code authorizing this write.
    pub code: u8,
    /// UUID receiving the final payload.
    pub characteristic: Uuid,
    /// Bytes written at session end.
    pub bytes: Vec<u8>,
}

/// Event delivered by a Bluetooth backend to the owner.
#[derive(Clone, Debug)]
pub enum BackendEvent {
    /// A peripheral advertisement observed during scanning.
    Advertisement {
        /// Owner-local platform identifier.
        key: String,
        /// Advertised local name, when available.
        name: Option<String>,
        /// Advertised service UUIDs.
        services: Vec<Uuid>,
        /// Backend-assigned order of this advertisement, passed back to [`BluetoothBackend::forget`].
        sequence: u64,
    },
    /// A connected peripheral disconnected.
    Disconnected {
        /// Owner-local platform identifier.
        key: String,
    },
    /// A value notification received from a peripheral.
    Notification {
        /// Owner-local platform identifier.
        key: String,
        /// UUID of the notifying characteristic.
        characteristic: Uuid,
        /// Notification payload.
        bytes: Vec<u8>,
    },
}

/// Local future returned by an owner-thread Bluetooth backend operation.
pub type BackendFuture<T> = LocalBoxFuture<'static, Result<T, DriverError>>;

/// Backend operations that run only on the dedicated owner thread.
pub trait BluetoothBackend {
    /// Start scanning and forward advertisements, link state, and notifications.
    fn start_scan(&mut self, events: mpsc::Sender<BackendEvent>) -> BackendFuture<()>;
    /// Connect, discover services, and start notification forwarding.
    fn connect(&mut self, key: &str) -> BackendFuture<()>;
    /// Release backend state for an untracked peripheral unless a newer advertisement for it is queued.
    fn forget(&mut self, key: &str, sequence: u64);
    /// Disconnect a peripheral.
    fn disconnect(&mut self, key: &str) -> BackendFuture<()>;
    /// Read a characteristic value.
    fn read(&mut self, key: &str, characteristic: Uuid) -> BackendFuture<Vec<u8>>;
    /// Write a characteristic value.
    fn write(&mut self, key: &str, characteristic: Uuid, bytes: Vec<u8>) -> BackendFuture<()>;
    /// Enable notifications for a characteristic.
    fn subscribe(&mut self, key: &str, characteristic: Uuid) -> BackendFuture<()>;
}

/// Shared lazy handle that starts a native Bluetooth owner only when requested.
#[derive(Clone, Default)]
pub struct LazyBluetooth(Arc<std::sync::Mutex<Option<BluetoothClient>>>);
impl LazyBluetooth {
    /// Create an empty lazy Bluetooth handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Share a client that has already started.
    pub fn with_client(client: BluetoothClient) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(client))))
    }

    /// Return the existing client without starting a native owner.
    pub fn existing(&self) -> Option<BluetoothClient> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Return the shared client, starting its native owner on first use.
    pub fn get_or_start_native(&self) -> Result<BluetoothClient, DriverError> {
        let mut client = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if client.is_none() {
            *client = Some(BluetoothClient::start_native()?);
        }
        client.clone().ok_or_else(unavailable)
    }
}

/// Sendable proxy to the thread that owns the Bluetooth backend.
#[derive(Clone)]
pub struct BluetoothClient {
    inner: Arc<ClientInner>,
}
struct ClientInner {
    sender: mpsc::Sender<Request>,
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
        filter: ScanFilter,
        reply: Reply<EndpointScan>,
    },
    Connect {
        endpoint: EndpointToken,
        reply: Reply<bool>,
    },
    Read {
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
        reply: Reply<Vec<u8>>,
    },
    Write {
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
        bytes: Vec<u8>,
        reply: Reply<()>,
    },
    StartRepeat {
        context: OperationContext,
        endpoint: EndpointToken,
        spec: RepeatSpec,
        reply: Reply<()>,
    },
    StopRepeat {
        context: OperationContext,
        endpoint: EndpointToken,
        reply: Reply<bool>,
    },
    Subscribe {
        context: OperationContext,
        endpoint: EndpointToken,
        characteristic: Uuid,
        events: mpsc::Sender<Value>,
        cancelled: Arc<AtomicBool>,
    },
}

impl BluetoothClient {
    /// Start a native Bluetooth owner on supported macOS and Linux targets.
    pub fn start_native() -> Result<Self, DriverError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            Self::start_with_backend(native::NativeBackend::new)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(DriverError::new(
                ErrorCode::UnsupportedCapability,
                "Bluetooth is not supported on this platform.",
            ))
        }
    }

    /// Start an owner whose backend factory executes on its dedicated thread.
    pub fn start_with_backend<B, F>(factory: F) -> Result<Self, DriverError>
    where
        B: BluetoothBackend + 'static,
        F: FnOnce() -> Result<B, DriverError> + Send + 'static,
    {
        let (sender, mut receiver) = mpsc::channel(MAILBOX_SIZE);
        let signals = Arc::new(Signals {
            stopping: AtomicBool::new(false),
            done: AtomicBool::new(false),
            completed: Notify::new(),
        });
        let worker_signals = signals.clone();
        let run_signals = worker_signals.clone();
        thread::Builder::new()
            .name("synapsed-bluetooth".into())
            .spawn(move || {
                let backend = factory();
                match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => {
                        let local = LocalSet::new();
                        local.block_on(&runtime, async move {
                            let mut owner = Owner::default();
                            owner.run(backend, receiver, run_signals).await;
                        });
                    }
                    Err(_) => {
                        while let Ok(request) = receiver.try_recv() {
                            reject(request, unavailable());
                        }
                    }
                }
                worker_signals.done.store(true, Ordering::SeqCst);
                worker_signals.completed.notify_waiters();
            })
            .map_err(|_| {
                DriverError::new(
                    ErrorCode::DeviceUnavailable,
                    "Could not start native Bluetooth owner.",
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
                mpsc::error::TrySendError::Full(_) => busy("Native Bluetooth queue is full."),
                mpsc::error::TrySendError::Closed(_) => unavailable(),
            })
    }

    /// Enumerate endpoints matching an advertisement filter.
    pub async fn enumerate(&self, filter: ScanFilter) -> Result<EndpointScan, DriverError> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        self.transact(None, deadline, |reply| Request::Enumerate { filter, reply })
            .await
    }

    /// Connect to a previously enumerated endpoint; false means the attempt is still pending.
    pub async fn connect(&self, endpoint: EndpointToken) -> Result<bool, DriverError> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        self.transact(None, deadline, |reply| Request::Connect { endpoint, reply })
            .await
    }

    /// Read a characteristic after validating the operation grant.
    pub async fn read(
        &self,
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
    ) -> Result<Vec<u8>, DriverError> {
        context.validate(endpoint, code, false)?;
        self.transact(Some(&context), context.deadline(), |reply| Request::Read {
            context: context.clone(),
            endpoint,
            code,
            characteristic,
            reply,
        })
        .await
    }

    /// Write a characteristic after validating the operation grant.
    pub async fn write(
        &self,
        context: OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
        bytes: Vec<u8>,
    ) -> Result<(), DriverError> {
        context.validate(endpoint, code, true)?;
        self.transact(Some(&context), context.deadline(), |reply| Request::Write {
            context: context.clone(),
            endpoint,
            code,
            characteristic,
            bytes,
            reply,
        })
        .await
    }

    /// Start a repeated write after validating its primary and final-write permissions.
    pub async fn start_repeat(
        &self,
        context: OperationContext,
        endpoint: EndpointToken,
        spec: RepeatSpec,
    ) -> Result<(), DriverError> {
        context.validate(endpoint, spec.code, true)?;
        for final_write in &spec.finally {
            context.validate(endpoint, final_write.code, true)?;
        }
        self.transact(Some(&context), context.deadline(), |reply| {
            Request::StartRepeat {
                context: context.clone(),
                endpoint,
                spec,
                reply,
            }
        })
        .await
    }

    /// Stop the active repeated write for an endpoint.
    pub async fn stop_repeat(
        &self,
        context: OperationContext,
        endpoint: EndpointToken,
    ) -> Result<bool, DriverError> {
        context.validate_endpoint(endpoint)?;
        self.transact(Some(&context), context.deadline(), |reply| {
            Request::StopRepeat {
                context: context.clone(),
                endpoint,
                reply,
            }
        })
        .await
    }

    /// Subscribe to characteristic notifications using a bounded event channel.
    pub fn subscribe(
        &self,
        context: &OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
        events: mpsc::Sender<Value>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), DriverError> {
        context.validate(endpoint, code, false)?;
        self.enqueue(Request::Subscribe {
            context: context.clone(),
            endpoint,
            characteristic,
            events,
            cancelled,
        })
    }

    /// Stop admission and wait for owner shutdown until the supplied deadline.
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

    async fn transact<T>(
        &self,
        context: Option<&OperationContext>,
        deadline: Instant,
        request: impl FnOnce(Reply<T>) -> Request,
    ) -> Result<T, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.enqueue(request(reply))?;
        let mut abandonment = context.map(|context| Abandonment {
            context,
            armed: true,
        });
        let result = receive(receiver, deadline).await;
        if let Some(abandonment) = &mut abandonment {
            abandonment.armed = false;
        }
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::Timeout)
            && let Some(context) = context
        {
            context.cancel();
        }
        result
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
        Request::Connect { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        Request::Read { context, reply, .. } => {
            context.cancel();
            let _ = reply.send(Err(error));
        }
        Request::Write { context, reply, .. } => {
            context.cancel();
            let _ = reply.send(Err(error));
        }
        Request::StartRepeat { context, reply, .. } => {
            context.cancel();
            let _ = reply.send(Err(error));
        }
        Request::StopRepeat { context, reply, .. } => {
            context.cancel();
            let _ = reply.send(Err(error));
        }
        Request::Subscribe { events, .. } => send_error(&events, error),
    }
}

fn send_error(events: &mpsc::Sender<Value>, error: DriverError) {
    let _ = events.try_send(json!({
        "error": {
            "code": error.code,
            "message": error.message,
        }
    }));
}

#[derive(Default)]
struct Owner {
    peripherals: HashMap<String, PeripheralState>,
    filters: Vec<ScanFilter>,
    scan_started: bool,
    overflow: bool,
}
struct PeripheralState {
    token: EndpointToken,
    name: Option<String>,
    services: Vec<Uuid>,
    last_seen: Instant,
    connected: bool,
    connecting: bool,
    last_failure: Option<DriverError>,
    subscribed: BTreeSet<Uuid>,
    listeners: Vec<Listener>,
    session: Option<Session>,
}
struct Listener {
    characteristic: Uuid,
    sender: mpsc::Sender<Value>,
    cancelled: Arc<AtomicBool>,
}
struct Session {
    characteristic: Uuid,
    bytes: Vec<u8>,
    interval: Duration,
    next_at: Instant,
    until: Instant,
    finally: Vec<FinalWrite>,
    liveness: GrantLiveness,
}

impl Owner {
    async fn run<B: BluetoothBackend + 'static>(
        &mut self,
        mut backend: Result<B, DriverError>,
        mut receiver: mpsc::Receiver<Request>,
        signals: Arc<Signals>,
    ) {
        let (event_tx, mut event_rx) = mpsc::channel(EVENT_QUEUE_SIZE);
        let (connect_tx, mut connect_rx) = mpsc::channel(CONNECT_QUEUE_SIZE);
        let mut ticker = interval(Duration::from_millis(20));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            if signals.stopping.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                biased;
                request = receiver.recv() => {
                    match request {
                        Some(request) if signals.stopping.load(Ordering::SeqCst) => reject(request, unavailable()),
                        Some(request) => match backend.as_mut() {
                            Ok(backend) => self.handle_request(backend, request, &event_tx, &connect_tx, &mut event_rx).await,
                            Err(error) => reject(request, error.clone()),
                        },
                        None => break,
                    }
                }
                event = event_rx.recv() => {
                    if let Some(event) = event
                        && let Ok(backend) = backend.as_mut()
                    {
                        self.handle_event(backend, event);
                    }
                }
                result = connect_rx.recv() => {
                    if let Some((key, generation, result)) = result {
                        self.handle_connect_result(key, generation, result);
                    }
                }
                _ = ticker.tick() => {
                    if let Ok(backend) = backend.as_mut() {
                        self.tick(backend).await;
                    }
                }
            }
        }

        while let Ok(request) = receiver.try_recv() {
            reject(request, unavailable());
        }
        if let Ok(backend) = backend.as_mut() {
            self.shutdown_backend(backend).await;
        }
        drop(backend);
    }

    async fn handle_request<B: BluetoothBackend + 'static>(
        &mut self,
        backend: &mut B,
        request: Request,
        event_tx: &mpsc::Sender<BackendEvent>,
        connect_tx: &mpsc::Sender<(String, u64, Result<(), DriverError>)>,
        event_rx: &mut mpsc::Receiver<BackendEvent>,
    ) {
        match request {
            Request::Enumerate { filter, reply } => {
                let result = self.enumerate(backend, filter, event_tx, event_rx).await;
                let _ = reply.send(result);
            }
            Request::Connect { endpoint, reply } => {
                self.connect(backend, endpoint, reply, connect_tx);
            }
            Request::Read {
                context,
                endpoint,
                code,
                characteristic,
                reply,
            } => {
                let result = self
                    .read(backend, &context, endpoint, code, characteristic)
                    .await;
                let _ = reply.send(result);
            }
            Request::Write {
                context,
                endpoint,
                code,
                characteristic,
                bytes,
                reply,
            } => {
                let result = self
                    .write(backend, &context, endpoint, code, characteristic, bytes)
                    .await;
                let _ = reply.send(result);
            }
            Request::StartRepeat {
                context,
                endpoint,
                spec,
                reply,
            } => {
                let result = self.start_repeat(backend, &context, endpoint, spec).await;
                let _ = reply.send(result);
            }
            Request::StopRepeat {
                context,
                endpoint,
                reply,
            } => {
                let result = self.stop_repeat(backend, &context, endpoint).await;
                let _ = reply.send(result);
            }
            Request::Subscribe {
                context,
                endpoint,
                characteristic,
                events,
                cancelled,
            } => {
                self.subscribe(
                    backend,
                    &context,
                    endpoint,
                    characteristic,
                    events,
                    cancelled,
                )
                .await;
            }
        }
    }

    async fn enumerate<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        filter: ScanFilter,
        event_tx: &mpsc::Sender<BackendEvent>,
        event_rx: &mut mpsc::Receiver<BackendEvent>,
    ) -> Result<EndpointScan, DriverError> {
        if !self.filters.contains(&filter) {
            if self.filters.len() >= MAX_FILTERS {
                return Err(busy("Too many Bluetooth scan filters are registered."));
            }
            self.filters.push(filter.clone());
        }
        if !self.scan_started {
            backend.start_scan(event_tx.clone()).await?;
            self.scan_started = true;
            while let Ok(event) = event_rx.try_recv() {
                self.handle_event(backend, event);
            }
        }
        let mut endpoints: Vec<_> = self
            .peripherals
            .iter()
            .filter(|(_, peripheral)| {
                filter_matches(&filter, &peripheral.name, &peripheral.services)
                    && (peripheral.connected
                        || Instant::now().saturating_duration_since(peripheral.last_seen)
                            <= MAX_AGE)
            })
            .map(|(key, peripheral)| descriptor(key, peripheral))
            .collect();
        endpoints.sort_by_key(|endpoint| endpoint.token);
        let errors = if self.overflow {
            vec![bluetooth_error("Too many matching Bluetooth peripherals.")]
        } else {
            Vec::new()
        };
        Ok(EndpointScan { endpoints, errors })
    }

    fn connect(
        &mut self,
        backend: &mut impl BluetoothBackend,
        endpoint: EndpointToken,
        reply: Reply<bool>,
        connect_tx: &mpsc::Sender<(String, u64, Result<(), DriverError>)>,
    ) {
        let Some((key, result)) = self.connect_state(endpoint) else {
            let _ = reply.send(Err(stale_endpoint()));
            return;
        };
        match result {
            ConnectState::Connected => {
                let _ = reply.send(Ok(true));
                return;
            }
            ConnectState::Connecting => {
                let _ = reply.send(Ok(false));
                return;
            }
            ConnectState::Failed(error) => {
                let _ = reply.send(Err(error));
            }
            ConnectState::Start => {
                let _ = reply.send(Ok(false));
            }
        }
        let future = backend.connect(&key);
        let key_for_result = key.clone();
        let sender = connect_tx.clone();
        tokio::task::spawn_local(async move {
            let result = match tokio::time::timeout(CONNECT_TIMEOUT, future).await {
                Ok(result) => result,
                Err(_) => Err(bluetooth_error("Bluetooth connection timed out.")),
            };
            let _ = sender
                .send((key_for_result, endpoint.generation, result))
                .await;
        });
    }

    fn connect_state(&mut self, endpoint: EndpointToken) -> Option<(String, ConnectState)> {
        let (key, state) = self
            .peripherals
            .iter_mut()
            .find(|(_, peripheral)| peripheral.token.id == endpoint.id)?;
        if key.is_empty() || state.token.generation != endpoint.generation {
            return None;
        }
        let result = if state.connected {
            ConnectState::Connected
        } else if state.connecting {
            ConnectState::Connecting
        } else if let Some(error) = state.last_failure.take() {
            state.connecting = true;
            ConnectState::Failed(error)
        } else {
            state.connecting = true;
            ConnectState::Start
        };
        Some((key.clone(), result))
    }

    async fn read<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        context: &OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
    ) -> Result<Vec<u8>, DriverError> {
        let key = self.connected_key(endpoint)?;
        let _work = context.begin_native(endpoint, code, false)?;
        let future = backend.read(&key, characteristic);
        let result = bounded_io(future, context.deadline(), Duration::from_secs(2)).await;
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::Timeout)
        {
            context.cancel();
        }
        result
    }

    async fn write<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        context: &OperationContext,
        endpoint: EndpointToken,
        code: u8,
        characteristic: Uuid,
        bytes: Vec<u8>,
    ) -> Result<(), DriverError> {
        let key = self.connected_key(endpoint)?;
        let _work = context.begin_native(endpoint, code, true)?;
        context.admit_write(endpoint, code)?;
        let future = backend.write(&key, characteristic, bytes);
        let result = bounded_io(future, context.deadline(), Duration::from_secs(2)).await;
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::Timeout)
        {
            context.cancel();
        }
        result
    }

    async fn start_repeat<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        context: &OperationContext,
        endpoint: EndpointToken,
        spec: RepeatSpec,
    ) -> Result<(), DriverError> {
        let key = self.connected_key(endpoint)?;
        let _work = context.begin_native(endpoint, spec.code, true)?;
        context.admit_write(endpoint, spec.code)?;
        for final_write in &spec.finally {
            context.admit_write(endpoint, final_write.code)?;
        }
        self.end_session(backend, &key).await;
        let future = backend.write(&key, spec.characteristic, spec.bytes.clone());
        bounded_io(future, context.deadline(), Duration::from_secs(2)).await?;
        let now = Instant::now();
        let state = self.peripherals.get_mut(&key).ok_or_else(stale_endpoint)?;
        state.session = Some(Session {
            characteristic: spec.characteristic,
            bytes: spec.bytes,
            interval: spec.interval,
            next_at: now + spec.interval,
            until: now + spec.max_duration,
            finally: spec.finally,
            liveness: context.grant_liveness(),
        });
        Ok(())
    }

    async fn stop_repeat<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        context: &OperationContext,
        endpoint: EndpointToken,
    ) -> Result<bool, DriverError> {
        let key = self.connected_key(endpoint)?;
        context.validate_endpoint(endpoint)?;
        let had_session = self
            .peripherals
            .get(&key)
            .is_some_and(|peripheral| peripheral.session.is_some());
        self.end_session(backend, &key).await;
        Ok(had_session)
    }

    async fn subscribe<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        context: &OperationContext,
        endpoint: EndpointToken,
        characteristic: Uuid,
        events: mpsc::Sender<Value>,
        cancelled: Arc<AtomicBool>,
    ) {
        let key = match self.connected_key(endpoint) {
            Ok(key) => key,
            Err(error) => {
                send_error(&events, error);
                return;
            }
        };
        let Some(state) = self.peripherals.get(&key) else {
            send_error(&events, stale_endpoint());
            return;
        };
        if state.listeners.len() >= MAX_LISTENERS {
            send_error(&events, busy("Too many Bluetooth notification listeners."));
            return;
        }
        if !state.subscribed.contains(&characteristic) {
            let future = backend.subscribe(&key, characteristic);
            if let Err(error) = bounded_io(future, context.deadline(), Duration::from_secs(2)).await
            {
                send_error(&events, error);
                return;
            }
            if let Some(state) = self.peripherals.get_mut(&key) {
                state.subscribed.insert(characteristic);
            }
        }
        if let Some(state) = self.peripherals.get_mut(&key) {
            state.listeners.push(Listener {
                characteristic,
                sender: events,
                cancelled,
            });
        }
    }

    fn connected_key(&self, endpoint: EndpointToken) -> Result<String, DriverError> {
        self.peripherals
            .iter()
            .find(|(_, peripheral)| peripheral.token.id == endpoint.id)
            .and_then(|(key, peripheral)| {
                (peripheral.token.generation == endpoint.generation && peripheral.connected)
                    .then(|| key.clone())
            })
            .ok_or_else(|| {
                DriverError::new(
                    ErrorCode::DeviceUnavailable,
                    "Bluetooth peripheral is not connected.",
                )
            })
    }

    fn handle_event(&mut self, backend: &mut impl BluetoothBackend, event: BackendEvent) {
        match event {
            BackendEvent::Advertisement {
                key,
                name,
                services,
                sequence,
            } => {
                if !self
                    .filters
                    .iter()
                    .any(|filter| filter_matches(filter, &name, &services))
                {
                    if !self.peripherals.contains_key(&key) {
                        backend.forget(&key, sequence);
                    }
                    return;
                }
                let now = Instant::now();
                if let Some(peripheral) = self.peripherals.get_mut(&key) {
                    peripheral.name = name;
                    peripheral.services = services;
                    peripheral.last_seen = now;
                } else if self.peripherals.len() < MAX_TRACKED {
                    self.peripherals.insert(
                        key,
                        PeripheralState {
                            token: EndpointToken {
                                id: allocate_endpoint_id(),
                                generation: 1,
                            },
                            name,
                            services,
                            last_seen: now,
                            connected: false,
                            connecting: false,
                            last_failure: None,
                            subscribed: BTreeSet::new(),
                            listeners: Vec::new(),
                            session: None,
                        },
                    );
                } else {
                    self.overflow = true;
                    backend.forget(&key, sequence);
                }
            }
            BackendEvent::Disconnected { key } => {
                if let Some(peripheral) = self.peripherals.get_mut(&key) {
                    peripheral.connected = false;
                    peripheral.connecting = false;
                    peripheral.token.generation = peripheral.token.generation.wrapping_add(1);
                    peripheral.subscribed.clear();
                    peripheral.session = None;
                    for listener in peripheral.listeners.drain(..) {
                        send_error(
                            &listener.sender,
                            DriverError::new(
                                ErrorCode::DeviceUnavailable,
                                "Bluetooth link was lost.",
                            ),
                        );
                    }
                }
            }
            BackendEvent::Notification {
                key,
                characteristic,
                bytes,
            } => {
                if let Some(peripheral) = self.peripherals.get_mut(&key) {
                    peripheral.listeners.retain(|listener| {
                        if listener.cancelled.load(Ordering::SeqCst) || listener.sender.is_closed()
                        {
                            return false;
                        }
                        if listener.characteristic == characteristic {
                            let _ = listener.sender.try_send(json!({"bytes": bytes}));
                        }
                        true
                    });
                }
            }
        }
    }

    fn handle_connect_result(
        &mut self,
        key: String,
        generation: u64,
        result: Result<(), DriverError>,
    ) {
        let Some(peripheral) = self.peripherals.get_mut(&key) else {
            return;
        };
        if peripheral.token.generation != generation {
            return;
        }
        peripheral.connecting = false;
        match result {
            Ok(()) => {
                peripheral.connected = true;
                peripheral.last_failure = None;
            }
            Err(error) => {
                peripheral.connected = false;
                peripheral.last_failure = Some(error);
            }
        }
    }

    async fn tick<B: BluetoothBackend>(&mut self, backend: &mut B) {
        let now = Instant::now();
        let keys: Vec<String> = self.peripherals.keys().cloned().collect();
        for key in keys {
            let due = self.peripherals.get(&key).and_then(|peripheral| {
                peripheral.session.as_ref().map(|session| {
                    if !session.liveness.is_active() || now >= session.until {
                        TickAction::End
                    } else if now >= session.next_at {
                        TickAction::Write(session.characteristic, session.bytes.clone())
                    } else {
                        TickAction::None
                    }
                })
            });
            match due {
                Some(TickAction::End) => self.end_session(backend, &key).await,
                Some(TickAction::Write(characteristic, bytes)) => {
                    let future = backend.write(&key, characteristic, bytes);
                    match tokio::time::timeout(Duration::from_secs(1), future).await {
                        Ok(Ok(())) => {
                            if let Some(Some(session)) = self
                                .peripherals
                                .get_mut(&key)
                                .map(|peripheral| peripheral.session.as_mut())
                            {
                                session.next_at =
                                    (session.next_at + session.interval).max(Instant::now());
                            }
                        }
                        _ => self.end_session(backend, &key).await,
                    }
                }
                Some(TickAction::None) | None => {}
            }
            if let Some(peripheral) = self.peripherals.get_mut(&key) {
                peripheral.listeners.retain(|listener| {
                    !listener.cancelled.load(Ordering::SeqCst) && !listener.sender.is_closed()
                });
            }
        }
    }

    async fn end_session<B: BluetoothBackend>(&mut self, backend: &mut B, key: &str) {
        let session = self
            .peripherals
            .get_mut(key)
            .and_then(|peripheral| peripheral.session.take());
        if let Some(session) = session {
            self.final_writes(backend, key, session.finally).await;
        }
    }

    async fn final_writes<B: BluetoothBackend>(
        &mut self,
        backend: &mut B,
        key: &str,
        writes: Vec<FinalWrite>,
    ) {
        for write in writes {
            let future = backend.write(key, write.characteristic, write.bytes);
            if !matches!(
                tokio::time::timeout(Duration::from_secs(1), future).await,
                Ok(Ok(()))
            ) {
                tracing::warn!(
                    target: "synapsed::bluetooth",
                    public_message = %"Bluetooth session final write failed."
                );
            }
        }
    }

    async fn shutdown_backend<B: BluetoothBackend>(&mut self, backend: &mut B) {
        let keys: Vec<String> = self.peripherals.keys().cloned().collect();
        for key in &keys {
            self.end_session(backend, key).await;
        }
        for key in keys {
            let connected = self
                .peripherals
                .get(&key)
                .is_some_and(|peripheral| peripheral.connected);
            if connected {
                let future = backend.disconnect(&key);
                let _ = tokio::time::timeout(Duration::from_secs(1), future).await;
            }
        }
    }
}

enum ConnectState {
    Connected,
    Connecting,
    Failed(DriverError),
    Start,
}
enum TickAction {
    None,
    End,
    Write(Uuid, Vec<u8>),
}

fn descriptor(key: &str, peripheral: &PeripheralState) -> EndpointDescriptor {
    EndpointDescriptor {
        token: peripheral.token,
        capability: CAPABILITY.to_owned(),
        manufacturer: None,
        product: None,
        product_name: synapsed_inventory::bluetooth_display_name(
            peripheral.name.as_deref().unwrap_or_default(),
        ),
        identity: Some(synapsed_inventory::bluetooth_endpoint_identity(key)),
    }
}

fn filter_matches(filter: &ScanFilter, name: &Option<String>, services: &[Uuid]) -> bool {
    filter
        .services
        .iter()
        .any(|service| services.contains(service))
        || name.as_deref().is_some_and(|name| {
            let name = name.trim().to_lowercase();
            filter
                .name_prefixes
                .iter()
                .any(|prefix| name.starts_with(&prefix.to_lowercase()))
        })
}

async fn bounded_io<T>(
    future: BackendFuture<T>,
    deadline: Instant,
    cap: Duration,
) -> Result<T, DriverError> {
    let until = deadline.min(Instant::now() + cap);
    tokio::time::timeout(until.saturating_duration_since(Instant::now()), future)
        .await
        .map_err(|_| timeout())?
}

fn stale_endpoint() -> DriverError {
    DriverError::new(
        ErrorCode::DeviceUnavailable,
        "Bluetooth endpoint generation is stale.",
    )
}
fn unavailable() -> DriverError {
    DriverError::new(
        ErrorCode::DeviceUnavailable,
        "Native Bluetooth endpoint or owner is unavailable.",
    )
}
fn timeout() -> DriverError {
    DriverError::new(ErrorCode::Timeout, "Native Bluetooth operation timed out.")
}
fn busy(message: &'static str) -> DriverError {
    DriverError::new(ErrorCode::Busy, message)
}
fn bluetooth_error(message: &'static str) -> DriverError {
    DriverError::new(ErrorCode::BluetoothError, message)
}
