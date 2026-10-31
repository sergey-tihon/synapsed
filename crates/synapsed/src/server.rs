use crate::{
    access::{AccessPolicy, error_response, preflight},
    logs::LogRing,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{
        Path, Request, State,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    },
    http::header,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::{
    future::Future,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use synapsed_api::{
    ApiError, CharacteristicCommand, DeviceResponse, DeviceStatus, ErrorCode, LogLevel,
    LogsResponse, ServerEvent,
};
use synapsed_core::StateStore;
use synapsed_plugins::PluginHost;
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
    task::JoinHandle,
};

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const WS_LIMIT: usize = 1024 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Application {
    inner: Arc<Inner>,
}
struct Inner {
    store: StateStore,
    host: PluginHost,
    logs: LogRing,
    accepting: AtomicBool,
    stopping: watch::Sender<bool>,
    frame: Mutex<Option<SnapshotFrame>>,
}
struct SnapshotFrame {
    encoded: Arc<[u8]>,
    text: Utf8Bytes,
}
impl Application {
    pub fn new(store: StateStore, host: PluginHost, logs: LogRing) -> Self {
        let (stopping, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                store,
                host,
                logs,
                accepting: AtomicBool::new(true),
                stopping,
                frame: Mutex::new(None),
            }),
        }
    }
    pub fn store(&self) -> &StateStore {
        &self.inner.store
    }
    pub fn logs(&self) -> &LogRing {
        &self.inner.logs
    }
    pub fn router(&self, policy: AccessPolicy) -> Router {
        Router::new()
            .route("/api/v1/status", get(status).options(preflight))
            .route("/api/v1/devices", get(snapshot).options(preflight))
            .route(
                "/api/v1/devices/{device_id}",
                get(device).options(preflight),
            )
            .route(
                "/api/v1/devices/{device_id}/characteristics/{characteristic_id}",
                post(execute).options(preflight),
            )
            .route("/api/v1/logs", get(logs).options(preflight))
            .route("/api/v1/events", get(events).options(preflight))
            .fallback(fallback)
            .with_state(self.clone())
            .layer(middleware::from_fn_with_state(
                self.clone(),
                reject_shutdown,
            ))
            .layer(middleware::from_fn_with_state(
                policy,
                crate::access::enforce,
            ))
    }
    pub fn begin_shutdown(&self) {
        if self.inner.accepting.swap(false, Ordering::AcqRel) {
            self.inner
                .logs
                .record(LogLevel::Info, "synapsed", "Daemon shutdown started.");
            self.inner.stopping.send_replace(true);
        }
    }
    async fn stopped(&self) {
        let mut stopping = self.inner.stopping.subscribe();
        if *stopping.borrow_and_update() {
            return;
        }
        let _ = stopping.changed().await;
    }
    async fn shutdown_host(&self, deadline: Instant) -> Result<(), ApiError> {
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            self.inner.host.shutdown(deadline),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ApiError::new(
                ErrorCode::Timeout,
                "Shutdown deadline expired; native outcomes may be unknown.",
            )),
        }
    }
    fn snapshot_frame(&self) -> Utf8Bytes {
        let encoded = self.inner.store.encoded_snapshot();
        let mut cached = self
            .inner
            .frame
            .lock()
            .expect("snapshot frame mutex poisoned");
        if let Some(frame) = &*cached {
            if Arc::ptr_eq(&frame.encoded, &encoded) {
                return frame.text.clone();
            }
        }
        // StateStore serialized the validated immutable snapshot once. Only the event
        // envelope is added here, once per publication, and shared by WS subscribers.
        const PREFIX: &[u8] = b"{\"type\":\"snapshot\",\"snapshot\":";
        let mut bytes = Vec::with_capacity(encoded.len() + PREFIX.len() + 1);
        bytes.extend_from_slice(PREFIX);
        bytes.extend_from_slice(&encoded);
        bytes.push(b'}');
        let text = Utf8Bytes::from(String::from_utf8(bytes).expect("snapshot is UTF-8 JSON"));
        *cached = Some(SnapshotFrame {
            encoded,
            text: text.clone(),
        });
        text
    }
}

pub struct RunningServer {
    pub address: SocketAddr,
    pub application: Application,
    task: JoinHandle<std::io::Result<()>>,
    observer: JoinHandle<()>,
}
impl RunningServer {
    pub fn start(
        listener: TcpListener,
        application: Application,
        allowed_origins: Vec<String>,
    ) -> Result<Self, ApiError> {
        let address = listener.local_addr().map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidRequest,
                "Cannot determine bound server address.",
            )
        })?;
        let policy = AccessPolicy::new(address, allowed_origins)?;
        let router = application.router(policy);
        let shutdown = application.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.stopped().await })
                .await
        });
        let observer_app = application.clone();
        let observer = tokio::spawn(async move { observe_driver_status(observer_app).await });
        application.logs().listening(address);
        Ok(Self {
            address,
            application,
            task,
            observer,
        })
    }
    pub async fn run_until<F: Future<Output = ()>>(&mut self, signal: F) -> Result<(), ApiError> {
        tokio::select! {
            _ = signal => Ok(()),
            result = &mut self.task => {
                self.application.begin_shutdown();
                match result { Ok(Ok(())) => Ok(()), _ => Err(ApiError::new(ErrorCode::DeviceUnavailable, "Local API server stopped unexpectedly.")) }
            }
        }
    }
    pub async fn shutdown(&mut self) -> Result<(), ApiError> {
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        self.application.begin_shutdown();
        let host = self.application.clone();
        let task = &mut self.task;
        let server_finished = task.is_finished();
        let (host_result, server_result) = tokio::join!(host.shutdown_host(deadline), async {
            // A run_until-completed JoinHandle must not be polled a second time.
            if server_finished {
                return Ok(());
            }
            match tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), task)
                .await
            {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(_) => Err(ApiError::new(
                    ErrorCode::DeviceUnavailable,
                    "Local API shutdown failed.",
                )),
                Err(_) => Err(ApiError::new(
                    ErrorCode::Timeout,
                    "Local API shutdown deadline expired.",
                )),
            }
        });
        self.observer.abort();
        if host_result.is_err() || server_result.is_err() {
            self.task.abort();
            self.application.logs().record(LogLevel::Error, "synapsed", "Shutdown incomplete; started native writes or plugin finalizers may have unknown outcomes.");
        } else {
            self.application.logs().record(
                LogLevel::Info,
                "synapsed",
                "Daemon shutdown completed.",
            );
        }
        host_result.and(server_result)
    }
}
impl Drop for RunningServer {
    fn drop(&mut self) {
        self.application.begin_shutdown();
        self.task.abort();
        self.observer.abort();
    }
}
async fn observe_driver_status(application: Application) {
    let mut snapshots = application.store().subscribe();
    let mut stopping = application.inner.stopping.subscribe();
    let mut previous = Vec::new();
    loop {
        let current = snapshots.borrow_and_update().drivers.clone();
        for driver in &current {
            if !previous.iter().any(|old| old == driver) {
                let (level, message) = match driver.state {
                    synapsed_api::DriverState::Disabled => {
                        (LogLevel::Info, "Installed driver is disabled.")
                    }
                    synapsed_api::DriverState::Online => {
                        (LogLevel::Info, "Installed driver is online.")
                    }
                    synapsed_api::DriverState::Disconnected => {
                        (LogLevel::Warn, "Installed driver is disconnected.")
                    }
                    synapsed_api::DriverState::Ambiguous => (
                        LogLevel::Warn,
                        "Installed driver has ambiguous device ownership.",
                    ),
                    synapsed_api::DriverState::PermissionDenied => (
                        LogLevel::Error,
                        "Installed driver lacks transport permission.",
                    ),
                    synapsed_api::DriverState::Error => {
                        (LogLevel::Error, "Installed driver reported an error.")
                    }
                };
                application.logs().record(level, &driver.id, message);
            }
        }
        previous = current;
        tokio::select! { _ = stopping.changed() => break, result = snapshots.changed() => if result.is_err() { break; } }
    }
}
async fn reject_shutdown(
    State(application): State<Application>,
    request: Request,
    next: middleware::Next,
) -> Response {
    if !application.inner.accepting.load(Ordering::Acquire) {
        return error_response(ApiError::new(
            ErrorCode::DeviceUnavailable,
            "Daemon is shutting down.",
        ));
    }
    next.run(request).await
}
async fn status(State(application): State<Application>) -> Response {
    axum::Json(application.store().status()).into_response()
}
async fn snapshot(State(application): State<Application>) -> Response {
    let bytes = Bytes::from_owner(application.store().encoded_snapshot());
    (
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(bytes),
    )
        .into_response()
}
async fn device(State(application): State<Application>, Path(device_id): Path<String>) -> Response {
    let snapshot = application.store().snapshot();
    match snapshot
        .devices
        .iter()
        .find(|device| device.id == device_id)
    {
        Some(device) => axum::Json(DeviceResponse {
            node: snapshot.node.clone(),
            revision: snapshot.revision.clone(),
            device: device.clone(),
        })
        .into_response(),
        None => error_response(ApiError::new(
            ErrorCode::UnknownDevice,
            "Device does not exist.",
        )),
    }
}
async fn execute(
    State(application): State<Application>,
    Path((device_id, characteristic_id)): Path<(String, String)>,
    request: Request,
) -> Response {
    if !application.inner.accepting.load(Ordering::Acquire) {
        return error_response(ApiError::new(
            ErrorCode::DeviceUnavailable,
            "Daemon is shutting down.",
        ));
    }
    let current = application.store().snapshot();
    let Some(device) = current.devices.iter().find(|device| device.id == device_id) else {
        return error_response(ApiError::new(
            ErrorCode::UnknownDevice,
            "Device does not exist.",
        ));
    };
    let Some(characteristic) = device
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == characteristic_id)
    else {
        return error_response(ApiError::new(
            ErrorCode::UnknownCharacteristic,
            "Characteristic does not exist.",
        ));
    };
    if device.status != DeviceStatus::Online {
        return error_response(ApiError::new(
            ErrorCode::DeviceUnavailable,
            "Device is unavailable.",
        ));
    }
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
    {
        return error_response(ApiError::new(
            ErrorCode::InvalidRequest,
            "Command requires application/json.",
        ));
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), synapsed_api::MAX_COMMAND_BYTES),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return error_response(ApiError::new(
                ErrorCode::InvalidRequest,
                "Command is unreadable or exceeds 16 KiB.",
            ));
        }
        Err(_) => {
            return error_response(ApiError::new(
                ErrorCode::Timeout,
                "Command body deadline expired.",
            ));
        }
    };
    let command = match CharacteristicCommand::from_json(&bytes).and_then(|command| {
        characteristic.validate_command(&command)?;
        Ok(command)
    }) {
        Ok(command) => command,
        Err(error) => return error_response(error),
    };
    match application
        .inner
        .host
        .dispatch(&device_id, &characteristic_id, command)
        .await
    {
        // Dispatch confirmed the command. Return the newest authoritative publication
        // using its cached encoding; concurrent observations must not regress clients.
        Ok(_) => snapshot(State(application.clone())).await,
        Err(error) => {
            application.logs().record(
                LogLevel::Warn,
                "synapsed",
                command_error_message(error.code),
            );
            error_response(error)
        }
    }
}
fn command_error_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Timeout => "Command failed: timeout.",
        ErrorCode::DdcError => "Command failed: DDC transport error.",
        ErrorCode::PermissionDenied => "Command failed: transport permission denied.",
        ErrorCode::PluginError => "Command failed: plugin error.",
        ErrorCode::Busy => "Command failed: driver is busy.",
        ErrorCode::DriverConflict | ErrorCode::AmbiguousDevice => {
            "Command failed: conflicting device ownership."
        }
        _ => "Command was rejected by its current driver.",
    }
}
async fn logs(State(application): State<Application>) -> Response {
    axum::Json(LogsResponse {
        node: application.store().snapshot().node.clone(),
        entries: application.logs().recent(),
    })
    .into_response()
}
async fn fallback(request: Request) -> Response {
    #[cfg(feature = "web-ui")]
    {
        crate::web::serve(request)
    }
    #[cfg(not(feature = "web-ui"))]
    {
        let _ = request;
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}
async fn events(State(application): State<Application>, upgrade: WebSocketUpgrade) -> Response {
    if !application.inner.accepting.load(Ordering::Acquire) {
        return error_response(ApiError::new(
            ErrorCode::DeviceUnavailable,
            "Daemon is shutting down.",
        ));
    }
    upgrade
        .max_message_size(WS_LIMIT)
        .max_frame_size(WS_LIMIT)
        .on_upgrade(move |socket| stream_events(socket, application))
        .into_response()
}
async fn send(
    socket: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: Message,
) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, socket.send(message)).await,
        Ok(Ok(()))
    )
}
fn log_frame(event: ServerEvent) -> Message {
    Message::Text(
        serde_json::to_string(&event)
            .expect("bounded log event serializes")
            .into(),
    )
}
async fn stream_events(socket: WebSocket, application: Application) {
    // Subscribe first; entries appended while taking the ring are deduplicated below.
    let mut live_logs = application.logs().subscribe();
    let recent = application.logs().recent();
    let mut last_log = recent.last().and_then(|entry| entry.id.parse::<u64>().ok());
    let mut changes = application.store().subscribe();
    let mut stopping = application.inner.stopping.subscribe();
    let (mut outgoing, mut incoming) = socket.split();
    let shutting_down = *stopping.borrow_and_update();
    if shutting_down {
        let _ = send(
            &mut outgoing,
            Message::Close(Some(CloseFrame {
                code: 1001,
                reason: "Daemon shutdown".into(),
            })),
        )
        .await;
        return;
    }
    changes.borrow_and_update();
    if !send(&mut outgoing, Message::Text(application.snapshot_frame())).await {
        return;
    }
    if !send(
        &mut outgoing,
        log_frame(ServerEvent::Logs { entries: recent }),
    )
    .await
    {
        return;
    }
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_sequence = 0u64;
    let mut expected_pong: Option<Bytes> = None;
    loop {
        tokio::select! {
            _ = stopping.changed() => {
                let _ = send(&mut outgoing, Message::Close(Some(CloseFrame { code: 1001, reason: "Daemon shutdown".into() }))).await;
                break;
            }
            message = incoming.next() => match message {
                Some(Ok(Message::Pong(payload))) => { if expected_pong.as_ref() == Some(&payload) { expected_pong = None; } }
                Some(Ok(Message::Ping(payload))) => { if !send(&mut outgoing, Message::Pong(payload)).await { break; } }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) => {
                    let _ = send(&mut outgoing, Message::Close(Some(CloseFrame { code: 1008, reason: "Events stream accepts no commands".into() }))).await;
                    break;
                }
            },
            _ = ping.tick() => {
                if expected_pong.is_some() {
                    let _ = send(&mut outgoing, Message::Close(Some(CloseFrame { code: 1001, reason: "Pong deadline expired".into() }))).await;
                    break;
                }
                ping_sequence = ping_sequence.wrapping_add(1);
                let payload = Bytes::copy_from_slice(&ping_sequence.to_be_bytes());
                expected_pong = Some(payload.clone());
                if !send(&mut outgoing, Message::Ping(payload)).await { break; }
            },
            changed = changes.changed() => {
                if changed.is_err() { break; }
                changes.borrow_and_update();
                if !send(&mut outgoing, Message::Text(application.snapshot_frame())).await { break; }
            },
            event = live_logs.recv() => match event {
                Ok(entry) => {
                    let sequence = entry.id.parse::<u64>().expect("host log ID is decimal");
                    if last_log.is_some_and(|last| sequence <= last) { continue; }
                    last_log = Some(sequence);
                    if !send(&mut outgoing, log_frame(ServerEvent::Log { entry })).await { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = send(&mut outgoing, Message::Close(Some(CloseFrame { code: 1013, reason: "Logs lagged; reconnect for recent state".into() }))).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}
