use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use synapsed_api::*;
use synapsed_client::{Client, ClientError, ClientEvent, config::parse_endpoint};
use tokio::{
    sync::{Notify, broadcast},
    task::JoinHandle,
    time::{Duration, timeout},
};

const NODE: &str = "00000000-0000-4000-8000-000000000001";
const FIRST: &str = "00000000-0000-4000-8000-000000000002";
const SECOND: &str = "00000000-0000-4000-8000-000000000003";
fn snapshot(instance: &str, revision: u64) -> Snapshot {
    Snapshot {
        api_version: 1,
        node: NodeInfo {
            node_id: NODE.into(),
            instance_id: instance.into(),
            name: "Test node".into(),
        },
        revision: revision.to_string(),
        drivers: vec![],
        devices: vec![],
    }
}
fn log(id: u64) -> LogEntry {
    LogEntry {
        id: id.to_string(),
        timestamp: "2026-10-31T00:00:00Z".into(),
        level: LogLevel::Info,
        target: "test".into(),
        message: format!("Event {id}"),
    }
}
#[derive(Clone, Debug)]
enum Frame {
    Event(ServerEvent),
    Ping,
    Close,
}
struct ServerState {
    snapshot: Mutex<Snapshot>,
    recent: Mutex<Vec<LogEntry>>,
    frames: broadcast::Sender<Frame>,
    delay_commands: AtomicBool,
    delay_reads: AtomicBool,
    started: Notify,
    release: Notify,
    commands: AtomicUsize,
    closed: AtomicUsize,
    pong: Notify,
    override_body: Mutex<Option<(StatusCode, serde_json::Value)>>,
    requested_uri: Mutex<Option<String>>,
}
struct Server {
    state: Arc<ServerState>,
    client: Client,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.client.close();
        self.task.abort();
    }
}
impl Server {
    async fn start() -> Self {
        Self::start_with_timeouts(Duration::from_secs(65), Duration::from_secs(5)).await
    }
    async fn start_with_idle_timeout(socket_idle_timeout: Duration) -> Self {
        Self::start_with_timeouts(socket_idle_timeout, Duration::from_secs(5)).await
    }
    async fn start_with_request_timeout(request_timeout: Duration) -> Self {
        Self::start_with_timeouts(Duration::from_secs(65), request_timeout).await
    }
    async fn start_with_timeouts(socket_idle_timeout: Duration, request_timeout: Duration) -> Self {
        let (frames, _) = broadcast::channel(32);
        let state = Arc::new(ServerState {
            snapshot: Mutex::new(snapshot(FIRST, 9)),
            recent: Mutex::new(vec![log(1), log(2)]),
            frames,
            delay_commands: AtomicBool::new(false),
            delay_reads: AtomicBool::new(false),
            started: Notify::new(),
            release: Notify::new(),
            commands: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            pong: Notify::new(),
            override_body: Mutex::new(None),
            requested_uri: Mutex::new(None),
        });
        let app = Router::new()
            .route("/api/v1/devices", get(read_snapshot))
            .route(
                "/api/v1/devices/{device}/characteristics/{characteristic}",
                post(command),
            )
            .route("/api/v1/devices/{device}", get(unknown_device))
            .route("/api/v1/events", get(events))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint =
            parse_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            state,
            client: Client::with_timeouts(endpoint, request_timeout, socket_idle_timeout).unwrap(),
            task,
        }
    }
    fn publish(&self, instance: &str, revision: u64) {
        let next = snapshot(instance, revision);
        *self.state.snapshot.lock().unwrap() = next.clone();
        self.state
            .frames
            .send(Frame::Event(ServerEvent::Snapshot { snapshot: next }))
            .unwrap();
    }
}
async fn read_snapshot(State(state): State<Arc<ServerState>>) -> Response {
    let snapshot = state.snapshot.lock().unwrap().clone();
    if state.delay_reads.load(Ordering::SeqCst) {
        state.started.notify_one();
        state.release.notified().await;
    }
    if let Some((status, body)) = state.override_body.lock().unwrap().clone() {
        return (status, Json(body)).into_response();
    }
    Json(snapshot).into_response()
}
async fn command(
    State(state): State<Arc<ServerState>>,
    Json(_): Json<CharacteristicCommand>,
) -> Json<Snapshot> {
    state.commands.fetch_add(1, Ordering::SeqCst);
    // Deliberately capture the *previous authoritative state*, not a requested
    // value. This models a delayed wire response, not a fake write confirmation.
    let snapshot = state.snapshot.lock().unwrap().clone();
    state.started.notify_one();
    if state.delay_commands.load(Ordering::SeqCst) {
        state.release.notified().await;
    }
    Json(snapshot)
}
async fn unknown_device(State(state): State<Arc<ServerState>>, uri: Uri) -> Response {
    *state.requested_uri.lock().unwrap() = Some(uri.path().into());
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: ApiError::new(ErrorCode::UnknownDevice, "Unknown device."),
        }),
    )
        .into_response()
}
async fn events(State(state): State<Arc<ServerState>>, upgrade: WebSocketUpgrade) -> Response {
    let frames = state.frames.subscribe();
    upgrade
        .on_upgrade(move |socket| socket_loop(socket, state, frames))
        .into_response()
}
async fn socket_loop(
    mut socket: WebSocket,
    state: Arc<ServerState>,
    mut frames: broadcast::Receiver<Frame>,
) {
    let initial = state.snapshot.lock().unwrap().clone();
    let recent = state.recent.lock().unwrap().clone();
    for event in [
        ServerEvent::Snapshot { snapshot: initial },
        ServerEvent::Logs { entries: recent },
    ] {
        if socket
            .send(Message::Text(serde_json::to_string(&event).unwrap().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Ok(Frame::Event(event)) => if socket.send(Message::Text(serde_json::to_string(&event).unwrap().into())).await.is_err() { break; },
                Ok(Frame::Ping) => if socket.send(Message::Ping(vec![1, 2, 3].into())).await.is_err() { break; },
                Ok(Frame::Close) => { let _ = socket.send(Message::Close(None)).await; break; },
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Pong(_))) => state.pong.notify_one(),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {},
            }
        }
    }
    state.closed.fetch_add(1, Ordering::SeqCst);
}
async fn next_snapshot(events: &mut synapsed_client::EventStream) -> Arc<Snapshot> {
    loop {
        if let ClientEvent::Snapshot(snapshot) = timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            return snapshot;
        }
    }
}

#[tokio::test]
async fn delayed_mutation_and_http_reads_never_regress_numeric_revision() {
    let server = Server::start().await;
    let mut events = server.client.events().await.unwrap();
    assert_eq!(events.initial_snapshot().revision, "9");
    server.state.delay_commands.store(true, Ordering::SeqCst);
    let client = server.client.clone();
    let command = tokio::spawn(async move {
        client
            .execute("desk", "height", CharacteristicCommand::Step { steps: 1 })
            .await
    });
    server.state.started.notified().await;
    server.publish(FIRST, 10);
    assert_eq!(next_snapshot(&mut events).await.revision, "10");
    server.state.release.notify_one();
    assert_eq!(command.await.unwrap().unwrap().revision, "10");
    assert_eq!(server.state.commands.load(Ordering::SeqCst), 1);
    *server.state.snapshot.lock().unwrap() = snapshot(FIRST, 9);
    assert_eq!(server.client.snapshot().await.unwrap().revision, "10");
    events.close().await;
    assert!(server.client.is_stale());
}

#[tokio::test]
async fn restart_invalidates_old_requests_and_deduplicates_logs_only_within_instance() {
    let server = Server::start().await;
    let mut events = server.client.events().await.unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(ClientEvent::Logs(entries)) if entries.len() == 2)
    );
    server.state.delay_commands.store(true, Ordering::SeqCst);
    let client = server.client.clone();
    let old = tokio::spawn(async move {
        client
            .execute("desk", "height", CharacteristicCommand::Step { steps: -1 })
            .await
    });
    server.state.started.notified().await;
    events.close().await;
    let mut same = server.client.events().await.unwrap();
    assert!(
        matches!(same.next().await.unwrap(), Some(ClientEvent::Logs(entries)) if entries.is_empty())
    );
    same.close().await;
    *server.state.snapshot.lock().unwrap() = snapshot(SECOND, 0);
    let mut restarted = server.client.events().await.unwrap();
    assert_eq!(restarted.initial_snapshot().node.instance_id, SECOND);
    assert!(
        matches!(restarted.next().await.unwrap(), Some(ClientEvent::Logs(entries)) if entries.len() == 2)
    );
    server.state.release.notify_one();
    assert!(matches!(old.await.unwrap(), Err(ClientError::Stale)));
    assert_eq!(
        server.client.cached_snapshot().unwrap().node.instance_id,
        SECOND
    );
}

#[tokio::test]
async fn different_instance_http_with_active_socket_disconnects_without_adoption() {
    let server = Server::start().await;
    let mut events = server.client.events().await.unwrap();
    *server.state.snapshot.lock().unwrap() = snapshot(SECOND, 0);
    assert!(matches!(
        server.client.snapshot().await,
        Err(ClientError::Stale)
    ));
    assert!(server.client.is_stale());
    assert_eq!(
        server.client.cached_snapshot().unwrap().node.instance_id,
        FIRST
    );
    assert!(matches!(events.next().await, Err(ClientError::Stale)));
    let restarted = server.client.events().await.unwrap();
    assert_eq!(restarted.initial_snapshot().node.instance_id, SECOND);
}

#[tokio::test]
async fn ping_and_client_close_work_without_consumer_reads() {
    let server = Server::start().await;
    let mut events = server.client.events().await.unwrap();
    server.state.frames.send(Frame::Ping).unwrap();
    timeout(Duration::from_secs(2), server.state.pong.notified())
        .await
        .unwrap();
    server.state.delay_reads.store(true, Ordering::SeqCst);
    let client = server.client.clone();
    let read = tokio::spawn(async move { client.snapshot().await });
    server.state.started.notified().await;
    server.client.close();
    assert!(matches!(read.await.unwrap(), Err(ClientError::Closed)));
    assert!(matches!(events.next().await, Err(ClientError::Closed)));
    timeout(Duration::from_secs(2), async {
        while server.state.closed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn quiet_event_stream_survives_heartbeats_past_application_idle_deadline() {
    let server = Server::start_with_idle_timeout(Duration::from_millis(500)).await;
    let mut events = server.client.events().await.unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Some(ClientEvent::Logs(_))
    ));
    let mut pending = Box::pin(events.next());
    assert!(futures_util::poll!(&mut pending).is_pending());
    let mut heartbeat = tokio::time::interval(Duration::from_millis(300));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    for _ in 0..3 {
        heartbeat.tick().await;
        server.state.frames.send(Frame::Ping).unwrap();
        timeout(Duration::from_secs(2), server.state.pong.notified())
            .await
            .unwrap();
        assert!(futures_util::poll!(&mut pending).is_pending());
        assert!(!server.client.is_stale());
        assert_eq!(server.state.closed.load(Ordering::SeqCst), 0);
    }
    drop(pending);
    server.publish(FIRST, 10);
    assert_eq!(next_snapshot(&mut events).await.revision, "10");
    assert!(!server.client.is_stale());
    events.close().await;
}

#[tokio::test]
async fn quiet_event_stream_without_transport_heartbeats_becomes_stale() {
    let server = Server::start_with_idle_timeout(Duration::from_millis(100)).await;
    let mut events = server.client.events().await.unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Some(ClientEvent::Logs(_))
    ));
    server.state.frames.send(Frame::Ping).unwrap();
    timeout(Duration::from_secs(2), server.state.pong.notified())
        .await
        .unwrap();
    let mut pending = Box::pin(events.next());
    assert!(futures_util::poll!(&mut pending).is_pending());
    let result = timeout(Duration::from_secs(2), pending).await.unwrap();
    assert!(matches!(result, Err(ClientError::Stale) | Ok(None)));
    assert!(server.client.is_stale());
}

#[tokio::test]
async fn protocol_and_policy_failures_are_terminal_but_invalid_value_is_not() {
    for (status, body) in [
        (
            StatusCode::OK,
            serde_json::json!({"apiVersion": 2, "node": {"nodeId": NODE, "instanceId": FIRST, "name": "Node"}, "revision": "0", "drivers": [], "devices": []}),
        ),
        (
            StatusCode::OK,
            serde_json::json!({"apiVersion": 1, "node": {"nodeId": NODE, "instanceId": FIRST, "name": "Node"}, "revision": "01", "drivers": [], "devices": []}),
        ),
        (
            StatusCode::FORBIDDEN,
            serde_json::json!({"error": {"code": "forbidden_host", "message": "Host denied."}}),
        ),
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": {"code": "invalid_value", "message": "Wrong status."}}),
        ),
    ] {
        let server = Server::start().await;
        *server.state.override_body.lock().unwrap() = Some((status, body));
        assert!(server.client.snapshot().await.err().unwrap().is_terminal());
        *server.state.override_body.lock().unwrap() = None;
        assert!(server.client.snapshot().await.err().unwrap().is_terminal());
    }
    let server = Server::start().await;
    *server.state.override_body.lock().unwrap() = Some((
        StatusCode::UNPROCESSABLE_ENTITY,
        serde_json::json!({"error": {"code": "invalid_value", "message": "Outside range."}}),
    ));
    assert!(!server.client.snapshot().await.err().unwrap().is_terminal());
    *server.state.override_body.lock().unwrap() = None;
    assert!(server.client.snapshot().await.is_ok());
}

#[tokio::test]
async fn route_segments_are_encoded_and_unknown_ids_stay_api_errors() {
    let server = Server::start().await;
    assert!(matches!(
        server.client.device("nested/part ?#").await,
        Err(ClientError::Api(ApiError {
            code: ErrorCode::UnknownDevice,
            ..
        }))
    ));
    assert_eq!(
        server.state.requested_uri.lock().unwrap().as_deref(),
        Some("/api/v1/devices/nested%2Fpart%20%3F%23")
    );
}

#[tokio::test]
async fn timeout_does_not_replay_a_mutation() {
    let server = Server::start_with_request_timeout(Duration::from_millis(100)).await;
    server.state.delay_commands.store(true, Ordering::SeqCst);
    let client = server.client.clone();
    let write = tokio::spawn(async move {
        client
            .execute("desk", "height", CharacteristicCommand::Toggle {})
            .await
    });
    server.state.started.notified().await;
    let result = timeout(Duration::from_secs(2), write)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(ClientError::Timeout)));
    assert_eq!(server.state.commands.load(Ordering::SeqCst), 1);
    server.state.release.notify_one();
}

#[tokio::test]
async fn same_socket_instance_change_is_incompatible_and_closure_is_stale() {
    let server = Server::start().await;
    let mut events = server.client.events().await.unwrap();
    server.publish(SECOND, 0);
    loop {
        match events.next().await {
            Ok(Some(_)) => {}
            Err(error) => {
                assert!(error.is_terminal());
                break;
            }
            other => panic!("Unexpected stream result: {other:?}"),
        }
    }
    assert!(server.client.events().await.err().unwrap().is_terminal());
    let server = Server::start().await;
    let mut events = server.client.events().await.unwrap();
    server.state.frames.send(Frame::Close).unwrap();
    loop {
        match events.next().await {
            Ok(Some(_)) => {}
            Ok(None) | Err(ClientError::Stale) => break,
            other => panic!("Unexpected close result: {other:?}"),
        }
    }
    assert!(server.client.is_stale());
}

#[tokio::test]
async fn nullable_response_fields_are_required_and_additive_fields_are_allowed() {
    let server = Server::start().await;
    let mut body = serde_json::to_value(snapshot(FIRST, 9)).unwrap();
    body["futureField"] = serde_json::json!({"additive": true});
    *server.state.override_body.lock().unwrap() = Some((StatusCode::OK, body));
    assert!(server.client.snapshot().await.is_ok());
    let server = Server::start().await;
    let mut body = serde_json::to_value(snapshot(FIRST, 9)).unwrap();
    body["drivers"] = serde_json::json!([{"id": "fixture", "version": "0.1.0", "state": "online"}]);
    *server.state.override_body.lock().unwrap() = Some((StatusCode::OK, body));
    assert!(server.client.snapshot().await.err().unwrap().is_terminal());
}

#[tokio::test]
async fn server_logs_before_initial_snapshot_are_terminal_not_a_client_handshake() {
    let app = Router::new().route(
        "/api/v1/events",
        get(|upgrade: WebSocketUpgrade| async move {
            upgrade.on_upgrade(|mut socket| async move {
                let event = ServerEvent::Logs { entries: vec![] };
                socket
                    .send(Message::Text(serde_json::to_string(&event).unwrap().into()))
                    .await
                    .unwrap();
                while socket.recv().await.is_some() {}
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client =
        Client::new(parse_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap())
            .unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let error = client.events().await.err().unwrap();
    assert!(error.is_terminal());
    assert!(client.events().await.err().unwrap().is_terminal());
    client.close();
    task.abort();
}

#[tokio::test]
async fn open_socket_is_not_connected_without_initial_snapshot() {
    let opened = Arc::new(Notify::new());
    let barrier = opened.clone();
    let app = Router::new().route(
        "/api/v1/events",
        get(move |upgrade: WebSocketUpgrade| {
            let barrier = barrier.clone();
            async move {
                upgrade.on_upgrade(move |mut socket| async move {
                    barrier.notify_one();
                    while socket.recv().await.is_some() {}
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = parse_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let client = Client::with_timeouts(
        endpoint,
        Duration::from_millis(100),
        Duration::from_secs(65),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let pending_client = client.clone();
    let pending = tokio::spawn(async move { pending_client.events().await });
    opened.notified().await;
    assert!(matches!(
        timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(ClientError::Timeout)
    ));
    client.close();
    task.abort();
}
