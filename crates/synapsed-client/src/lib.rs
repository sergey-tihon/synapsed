//! Local-only transport and shared, immutable snapshot reconciliation.
//! There are no command retries, hardware dependencies, or automatic daemon startup.
pub mod config;
mod ordering;
mod transport;
mod wire;

use ordering::Ordering;
use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use synapsed_api::{
    ApiError, Characteristic, CharacteristicCommand, CharacteristicSpec, CharacteristicValue,
    DeviceResponse, ErrorCode, ErrorResponse, InventoryResponse, LogEntry, LogsResponse,
    MAX_COMMAND_BYTES, ServerEvent, Snapshot, StatusResponse, parse_revision,
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{self, protocol::WebSocketConfig},
};
use url::{Host, Url};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const SOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(65);
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub enum ClientError {
    Config(&'static str),
    InvalidCommand(&'static str),
    Protocol(&'static str),
    Api(ApiError),
    Network,
    Timeout,
    Stale,
    Closed,
}
impl ClientError {
    /// Protocol incompatibility and local access-policy refusal require replacement
    /// of the client/configuration, not an endlessly reconnecting socket.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Config(_) | Self::Protocol(_))
            || matches!(self, Self::Api(error) if matches!(error.code, ErrorCode::ForbiddenHost | ErrorCode::ForbiddenOrigin))
    }
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) | Self::InvalidCommand(message) | Self::Protocol(message) => {
                formatter.write_str(message)
            }
            Self::Api(error) => write!(formatter, "{:?}: {}", error.code, error.message),
            Self::Network => {
                formatter.write_str("Cannot communicate with the local Synapsed service.")
            }
            Self::Timeout => formatter.write_str("Synapsed operation timed out."),
            Self::Stale => formatter.write_str(
                "Service instance or connection changed; cached observations are stale.",
            ),
            Self::Closed => formatter.write_str("Synapsed client is closed."),
        }
    }
}
impl std::error::Error for ClientError {}

pub fn validate_endpoint(endpoint: &Url) -> Result<(), ClientError> {
    let loopback = match endpoint.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    };
    if endpoint.scheme() != "http"
        || !loopback
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.path() != "/"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(ClientError::Config(
            "Endpoint must be an HTTP loopback origin without credentials, path, query, or fragment.",
        ));
    }
    Ok(())
}

/// Reconnect scheduling belongs to the long-running CLI/tray, not mutation transport.
pub fn reconnect_delay(attempt: usize) -> Duration {
    Duration::from_secs([1, 2, 4, 8, 10][attempt.min(4)])
}

/// Parse the complete argument using declared metadata. Strings are literal, including
/// an explicitly supplied empty argument; there is no JSON-object or partial parse path.
pub fn parse_value(
    characteristic: &Characteristic,
    argument: &str,
) -> Result<CharacteristicValue, ClientError> {
    let value = match &characteristic.spec {
        CharacteristicSpec::Number { .. } => {
            if argument.is_empty() || argument.trim() != argument {
                return Err(ClientError::InvalidCommand(
                    "Expected a complete finite number.",
                ));
            }
            let value: f64 = argument
                .parse()
                .map_err(|_| ClientError::InvalidCommand("Expected a complete finite number."))?;
            if !value.is_finite() {
                return Err(ClientError::InvalidCommand(
                    "Expected a complete finite number.",
                ));
            }
            CharacteristicValue::Number(value)
        }
        CharacteristicSpec::Boolean => match argument {
            "true" => CharacteristicValue::Boolean(true),
            "false" => CharacteristicValue::Boolean(false),
            _ => {
                return Err(ClientError::InvalidCommand(
                    "Boolean value must be exactly true or false.",
                ));
            }
        },
        CharacteristicSpec::Enum { .. } | CharacteristicSpec::String { .. } => {
            CharacteristicValue::String(argument.to_owned())
        }
        CharacteristicSpec::Action => {
            return Err(ClientError::InvalidCommand("Actions use invoke, not set."));
        }
    };
    characteristic
        .validate()
        .map_err(|_| ClientError::InvalidCommand("Invalid characteristic metadata."))?;
    if !characteristic.access.write {
        return Err(ClientError::InvalidCommand("Characteristic is read-only."));
    }
    characteristic.spec.validate_value(&value).map_err(|_| {
        ClientError::InvalidCommand("Set value is unsupported or outside the declared metadata.")
    })?;
    Ok(value)
}

#[derive(Debug, Clone)]
pub enum ClientEvent {
    Snapshot(Arc<Snapshot>),
    Logs(Vec<LogEntry>),
    Log(LogEntry),
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}
struct Inner {
    endpoint: Url,
    http: reqwest::Client,
    ordering: Mutex<Ordering>,
    changed: watch::Sender<u64>,
    socket_idle_timeout: Duration,
    request_timeout: Duration,
}
impl Inner {
    fn state(&self) -> MutexGuard<'_, Ordering> {
        self.ordering
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn notify(&self, generation: u64) {
        self.changed.send_replace(generation);
    }
    fn fail(&self, error: &ClientError) {
        let mut state = self.state();
        state.fail(error);
        self.notify(state.generation);
    }
    fn fail_socket(&self, token: u64, error: &ClientError) {
        let mut state = self.state();
        if state.socket == Some(token) {
            state.fail(error);
            self.notify(state.generation);
        }
    }
    fn disconnect(&self, token: u64) {
        let mut state = self.state();
        if state.socket == Some(token) {
            state.disconnect(token);
            self.notify(state.generation);
        }
    }
}
impl Client {
    pub fn new(endpoint: Url) -> Result<Self, ClientError> {
        Self::with_socket_idle_timeout(endpoint, SOCKET_IDLE_TIMEOUT)
    }
    /// Create a client with an explicit WebSocket inactivity deadline.
    #[doc(hidden)]
    pub fn with_socket_idle_timeout(
        endpoint: Url,
        socket_idle_timeout: Duration,
    ) -> Result<Self, ClientError> {
        Self::with_timeouts(endpoint, REQUEST_TIMEOUT, socket_idle_timeout)
    }
    /// Create a client with injected operation and WebSocket timeouts for tests.
    #[doc(hidden)]
    pub fn with_timeouts(
        endpoint: Url,
        request_timeout: Duration,
        socket_idle_timeout: Duration,
    ) -> Result<Self, ClientError> {
        validate_endpoint(&endpoint)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(request_timeout)
            .connect_timeout(request_timeout)
            .build()
            .map_err(|_| ClientError::Config("Cannot initialize HTTP client."))?;
        let (changed, _) = watch::channel(0);
        let mut ordering = Ordering::default();
        ordering.stale = true;
        Ok(Self {
            inner: Arc::new(Inner {
                endpoint,
                http,
                ordering: Mutex::new(ordering),
                changed,
                socket_idle_timeout,
                request_timeout,
            }),
        })
    }
    pub fn endpoint(&self) -> &Url {
        &self.inner.endpoint
    }
    pub fn cached_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.inner.state().snapshot.clone()
    }
    pub fn is_stale(&self) -> bool {
        self.inner.state().stale
    }
    /// Closes all clones together and cancels their owned in-flight HTTP/WS futures.
    pub fn close(&self) {
        let mut state = self.inner.state();
        state.closed = true;
        state.stale = true;
        state.socket = None;
        state.generation += 1;
        self.inner.notify(state.generation);
    }
    fn route(&self, parts: &[&str]) -> Url {
        let mut endpoint = self.inner.endpoint.clone();
        endpoint
            .path_segments_mut()
            .expect("validated HTTP origin")
            .clear()
            .extend(parts);
        endpoint
    }
    fn generation(&self) -> Result<u64, ClientError> {
        let state = self.inner.state();
        state.check()?;
        Ok(state.generation)
    }
    async fn cancellable<T>(
        &self,
        future: impl Future<Output = Result<T, ClientError>>,
    ) -> Result<T, ClientError> {
        let mut changed = self.inner.changed.subscribe();
        tokio::pin!(future);
        loop {
            self.inner.state().check()?;
            tokio::select! {
                result = &mut future => {
                    return result;
                }
                _ = changed.changed() => {
                    self.inner.state().check()?;
                }
            }
        }
    }
    async fn request<T: wire::Wire>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(u64, T), ClientError> {
        let generation = self.generation()?;
        let result = self
            .cancellable(async {
                let mut response = request.send().await.map_err(http_error)?;
                let status = response.status();
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
                {
                    return Err(ClientError::Protocol("Response exceeds one MiB."));
                }
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(http_error)? {
                    if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                        return Err(ClientError::Protocol("Response exceeds one MiB."));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                if status.as_u16() != 200 {
                    let response: ErrorResponse = wire::decode(&bytes)?;
                    if response.error.code.http_status() != status.as_u16() {
                        return Err(ClientError::Protocol(
                            "Error code does not match HTTP status.",
                        ));
                    }
                    return Err(ClientError::Api(response.error));
                }
                Ok((generation, wire::decode::<T>(&bytes)?))
            })
            .await;
        if let Err(error) = &result {
            let mut state = self.inner.state();
            state.check()?;
            if state.generation != generation {
                return Err(ClientError::Stale);
            }
            if error.is_terminal() {
                state.fail(error);
                self.inner.notify(state.generation);
            }
            if state.socket.is_none()
                && matches!(error, ClientError::Network | ClientError::Timeout)
            {
                state.stale = true;
            }
        }
        result
    }
    fn identity(&self, generation: u64, instance: &str) -> Result<(), ClientError> {
        let mut state = self.inner.state();
        let result = state.http_identity(generation, instance);
        self.inner.notify(state.generation);
        result
    }
    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        let (generation, response) = self
            .request::<StatusResponse>(self.inner.http.get(self.route(&["api", "v1", "status"])))
            .await?;
        let mut state = self.inner.state();
        let identity = state.http_identity(generation, &response.node.instance_id);
        self.inner.notify(state.generation);
        identity?;
        if let Some(snapshot) = &state.snapshot
            && parse_revision(&response.revision).unwrap()
                < parse_revision(&snapshot.revision).unwrap()
        {
            return Err(ClientError::Stale);
        }
        Ok(response)
    }
    pub async fn inventory(&self) -> Result<InventoryResponse, ClientError> {
        let (generation, response) = self
            .request::<InventoryResponse>(self.inner.http.get(self.route(&[
                "api",
                "v1",
                "inventory",
            ])))
            .await?;
        self.identity(generation, &response.instance_id)?;
        Ok(response)
    }
    pub async fn snapshot(&self) -> Result<Arc<Snapshot>, ClientError> {
        let (generation, snapshot) = self
            .request::<Snapshot>(self.inner.http.get(self.route(&["api", "v1", "devices"])))
            .await?;
        self.reconcile(generation, snapshot)
    }
    fn reconcile(&self, generation: u64, snapshot: Snapshot) -> Result<Arc<Snapshot>, ClientError> {
        let mut state = self.inner.state();
        let result = state.http_snapshot(generation, snapshot);
        self.inner.notify(state.generation);
        result
    }
    pub async fn device(&self, device_id: &str) -> Result<DeviceResponse, ClientError> {
        let (generation, mut response) = self
            .request::<DeviceResponse>(
                self.inner
                    .http
                    .get(self.route(&["api", "v1", "devices", device_id])),
            )
            .await?;
        let mut state = self.inner.state();
        let identity = state.http_identity(generation, &response.node.instance_id);
        self.inner.notify(state.generation);
        identity?;
        if response.device.id != device_id {
            let error = ClientError::Protocol("Device response does not match the requested ID.");
            state.fail(&error);
            self.inner.notify(state.generation);
            return Err(error);
        }
        if let Some(snapshot) = &state.snapshot
            && parse_revision(&response.revision).unwrap()
                < parse_revision(&snapshot.revision).unwrap()
        {
            response.device = snapshot
                .devices
                .iter()
                .find(|device| device.id == device_id)
                .cloned()
                .ok_or(ClientError::Stale)?;
            response.node = snapshot.node.clone();
            response.revision = snapshot.revision.clone();
        }
        Ok(response)
    }
    pub async fn execute(
        &self,
        device_id: &str,
        characteristic_id: &str,
        command: CharacteristicCommand,
    ) -> Result<Arc<Snapshot>, ClientError> {
        command
            .validate()
            .map_err(|_| ClientError::InvalidCommand("Invalid characteristic command."))?;
        let bytes = serde_json::to_vec(&command)
            .map_err(|_| ClientError::InvalidCommand("Command cannot be encoded."))?;
        if bytes.len() > MAX_COMMAND_BYTES {
            return Err(ClientError::InvalidCommand("Command exceeds 16 KiB."));
        }
        let request = self
            .inner
            .http
            .post(self.route(&[
                "api",
                "v1",
                "devices",
                device_id,
                "characteristics",
                characteristic_id,
            ]))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes);
        let (generation, snapshot) = self.request::<Snapshot>(request).await?;
        self.reconcile(generation, snapshot)
    }
    pub async fn logs(&self) -> Result<LogsResponse, ClientError> {
        let (generation, response) = self
            .request::<LogsResponse>(self.inner.http.get(self.route(&["api", "v1", "logs"])))
            .await?;
        self.identity(generation, &response.node.instance_id)?;
        Ok(response)
    }
    /// Establish a real WS stream and consume its immediate authoritative snapshot.
    /// Calling again supersedes the previous stream; reconnect scheduling is external.
    pub async fn events(&self) -> Result<EventStream, ClientError> {
        let token = {
            let mut state = self.inner.state();
            let token = state.begin_socket()?;
            self.inner.notify(state.generation);
            token
        };
        let guard = SocketGuard {
            client: self.clone(),
            token,
        };
        let mut endpoint = self.route(&["api", "v1", "events"]);
        endpoint.set_scheme("ws").expect("HTTP converts to WS");
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_RESPONSE_BYTES))
            .max_frame_size(Some(MAX_RESPONSE_BYTES));
        self.cancellable(async {
            let connection = timeout(
                self.inner.request_timeout,
                connect_async_with_config(endpoint.as_str(), Some(config), false),
            )
            .await
            .map_err(|_| ClientError::Timeout)
            .and_then(|result| result.map_err(socket_error));
            let (socket, _) = connection.inspect_err(|error| {
                if error.is_terminal() {
                    self.inner.fail_socket(token, error);
                }
            })?;
            let (receiver, task) = transport::start(socket, self.clone(), token);
            let mut stream = EventStream {
                guard,
                receiver,
                task,
                initial: None,
                changed: self.inner.changed.subscribe(),
                finished: false,
            };
            let event = timeout(self.inner.request_timeout, stream.read_event())
                .await
                .map_err(|_| ClientError::Timeout)??
                .ok_or(ClientError::Network)?;
            let ServerEvent::Snapshot { snapshot } = event else {
                let error = ClientError::Protocol("First WebSocket event must be a snapshot.");
                self.inner.fail_socket(token, &error);
                return Err(error);
            };
            let snapshot = {
                let mut state = self.inner.state();
                let snapshot = state.initial(token, snapshot)?;
                self.inner.notify(state.generation);
                snapshot
            };
            stream.initial = Some(snapshot);
            Ok(stream)
        })
        .await
    }
}
fn http_error(error: reqwest::Error) -> ClientError {
    if error.is_timeout() {
        ClientError::Timeout
    } else {
        ClientError::Network
    }
}
fn socket_error(error: tungstenite::Error) -> ClientError {
    match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            if let Some(bytes) = response.body()
                && let Ok(envelope) = wire::decode::<ErrorResponse>(bytes)
                && envelope.error.code.http_status() == status
            {
                return ClientError::Api(envelope.error);
            }
            ClientError::Protocol(
                "WebSocket upgrade was refused with an incompatible error response.",
            )
        }
        tungstenite::Error::Protocol(_)
        | tungstenite::Error::Capacity(_)
        | tungstenite::Error::Utf8(_) => {
            ClientError::Protocol("Invalid WebSocket protocol or frame.")
        }
        _ => ClientError::Network,
    }
}
struct SocketGuard {
    client: Client,
    token: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.client.inner.disconnect(self.token);
    }
}
pub struct EventStream {
    guard: SocketGuard,
    receiver: mpsc::Receiver<ServerEvent>,
    task: JoinHandle<()>,
    initial: Option<Arc<Snapshot>>,
    changed: watch::Receiver<u64>,
    finished: bool,
}
impl EventStream {
    pub fn initial_snapshot(&self) -> &Arc<Snapshot> {
        self.initial
            .as_ref()
            .expect("events consumes initial snapshot")
    }
    async fn read_event(&mut self) -> Result<Option<ServerEvent>, ClientError> {
        loop {
            self.guard
                .client
                .inner
                .state()
                .check_socket(self.guard.token)?;
            let event = tokio::select! {
                result = self.receiver.recv() => {
                    result
                }
                _ = self.changed.changed() => {
                    continue;
                }
            };
            self.guard
                .client
                .inner
                .state()
                .check_socket(self.guard.token)?;
            return Ok(event);
        }
    }
    pub async fn next(&mut self) -> Result<Option<ClientEvent>, ClientError> {
        if self.finished {
            return Ok(None);
        }
        let result = self.next_event().await;
        if matches!(result, Ok(None) | Err(_)) {
            self.finished = true;
            self.task.abort();
            match &result {
                Err(error) if error.is_terminal() => self.guard.client.inner.fail(error),
                _ => self.guard.client.inner.disconnect(self.guard.token),
            }
        }
        result
    }
    async fn next_event(&mut self) -> Result<Option<ClientEvent>, ClientError> {
        loop {
            // Quiet streams remain connected while the transport owner receives heartbeats.
            let Some(event) = self.read_event().await? else {
                return Ok(None);
            };
            let mut state = self.guard.client.inner.state();
            let event = match event {
                ServerEvent::Snapshot { snapshot } => {
                    state.live_snapshot(self.guard.token, snapshot)?
                }
                ServerEvent::Logs { entries } => {
                    Some(state.recent_logs(self.guard.token, entries)?)
                }
                ServerEvent::Log { entry } => state.live_log(self.guard.token, entry)?,
            };
            if let Some(event) = event {
                return Ok(Some(event));
            }
        }
    }
    pub async fn close(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.guard.client.inner.disconnect(self.guard.token);
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}
impl Drop for EventStream {
    fn drop(&mut self) {
        self.guard.client.inner.disconnect(self.guard.token);
        self.task.abort();
    }
}
