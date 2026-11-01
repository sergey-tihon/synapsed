mod common;
use axum::http::{HeaderMap, HeaderValue, header};
use futures_util::{SinkExt, StreamExt};
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    time::Duration,
};
use synapsed::{
    access::{AccessPolicy, validate_origin},
    config::{Config, ConfigPaths, PrivateState},
    logs::LogRing,
};
use synapsed_api::{ErrorCode, ErrorResponse, LogLevel, ServerEvent};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

fn paths(directory: &std::path::Path) -> ConfigPaths {
    ConfigPaths {
        home: directory.join("home"),
        config_home: directory.join("config"),
        data_home: directory.join("data"),
        state_home: directory.join("state"),
    }
}
#[test]
fn strict_configuration_defaults_paths_and_missing_files() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let default = Config::load_with_paths(None, &paths).unwrap();
    assert_eq!(default.listen.to_string(), "127.0.0.1:24816");
    assert_eq!(
        default.plugin_directory,
        paths.data_home.join("synapsed/drivers")
    );
    assert!(!default.plugin_directory_explicit);
    assert_eq!(default.state_dir, paths.state_home.join("synapsed"));
    assert!(Config::load_with_paths(Some(&directory.path().join("absent.toml")), &paths).is_err());
    let file = directory.path().join("selected/config.toml");
    let config = Config::from_text("[server]\nlisten='[::1]:24816'\nstate_dir='state'\nallowed_origins=['http://localhost:3000']\n[plugins]\ndirectory='~/drivers'\n[plugins.settings.example]\nfraction=1.25\n", &file, &paths).unwrap();
    assert_eq!(config.state_dir, directory.path().join("selected/state"));
    assert_eq!(config.plugin_directory, paths.home.join("drivers"));
    assert!(config.plugin_directory_explicit);
    assert_eq!(config.settings["example"]["fraction"], 1.25);
    for source in [
        "token='secret'",
        "[server]\ntoken='secret'",
        "[server]\nlisten='0.0.0.0:24816'",
        "[server]\nlisten='192.0.2.1:24816'",
        "[server]\nlisten='localhost:24816'",
        "[plugins]\nprofile='benq'",
        "[monitor]\nprofile='benq'",
        "[plugins]\ndiscovery_interval_ms=999",
        "[plugins]\ndiscovery_interval_ms=4294967296",
        "[plugins]\ndisabled=['generic-ddc','generic-ddc']",
        "[plugins]\ndirectory=''",
        "[plugins]\ndirectory='~user/drivers'",
        "[server]\nallowed_origins=['*']",
        "[server]\nallowed_origins=['null']",
        "[server]\nallowed_origins=['http://untrusted.example']",
        "[server]\nallowed_origins=['http://127.1:3000']",
        "[plugins.settings.example]\nnumber=nan",
        "[plugins.settings.example]\ndate=1979-05-27T07:32:00Z",
        &format!("[plugins.settings.example]\n'{}'=1", "k".repeat(129)),
        &format!(
            "[plugins]\ndisabled=[{}]",
            (0..17)
                .map(|i| format!("'driver-{i}'"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        &format!(
            "[plugins.settings.{}]\nvalue=1",
            (0..9)
                .map(|i| format!("d{i}."))
                .collect::<String>()
                .trim_end_matches('.')
        ),
        &format!(
            "[plugins.settings.example]\n{}",
            (0..2049)
                .map(|i| format!("k{i}=1"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        &format!(
            "[server]\nallowed_origins=[{}]",
            (0..33)
                .map(|i| format!("'http://localhost:{i}'"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        &format!(
            "[plugins.settings]\n{}",
            (0..17)
                .map(|i| format!("driver-{i}={{value=1}}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    ] {
        assert!(
            Config::from_text(source, &file, &paths).is_err(),
            "configuration unexpectedly accepted: {source}"
        );
    }
    let oversized = format!("[plugins.settings.example]\nvalue='{}'", "a".repeat(1025));
    assert!(Config::from_text(&oversized, &file, &paths).is_err());
    assert!(Config::from_text(&" ".repeat(64 * 1024 + 1), &file, &paths).is_err());
}
#[test]
fn private_state_lock_modes_stable_node_and_new_instance() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("private");
    let first = PrivateState::acquire(&state, Some("Isolated node")).unwrap();
    first.node.validate().unwrap();
    assert_eq!(
        std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in ["daemon.lock", "node-id"] {
        assert_eq!(
            std::fs::metadata(state.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(PrivateState::acquire(&state, Some("Isolated node")).is_err());
    let old = first.node.clone();
    drop(first);
    let second = PrivateState::acquire(&state, Some("Isolated node")).unwrap();
    assert_eq!(old.node_id, second.node.node_id);
    assert_ne!(old.instance_id, second.node.instance_id);
    drop(second);
    std::fs::write(state.join("node-id"), "corrupt").unwrap();
    assert!(PrivateState::acquire(&state, Some("Isolated node")).is_err());
    assert_eq!(
        std::fs::read_to_string(state.join("node-id")).unwrap(),
        "corrupt"
    );
    let alias = directory.path().join("alias");
    symlink(&state, &alias).unwrap();
    assert!(PrivateState::acquire(&alias, Some("Isolated node")).is_err());
    std::fs::remove_file(state.join("node-id")).unwrap();
    symlink(directory.path().join("absent"), state.join("node-id")).unwrap();
    assert!(PrivateState::acquire(&state, Some("Isolated node")).is_err());
}
#[test]
fn exact_host_origin_and_literal_loopback_policy() {
    let policy = AccessPolicy::new(
        "127.0.0.1:24816".parse().unwrap(),
        vec!["http://localhost:3000".into()],
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    assert_eq!(
        policy.check(&headers).unwrap_err().code,
        ErrorCode::ForbiddenHost
    );
    headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:24816"));
    assert!(policy.check(&headers).unwrap().is_none());
    for origin in ["http://127.0.0.1:24816", "http://localhost:3000"] {
        headers.insert(header::ORIGIN, origin.parse().unwrap());
        assert!(policy.check(&headers).is_ok());
    }
    for origin in [
        "null",
        "https://untrusted.example",
        "http://127.0.0.1:3000",
        "http://localhost:24816",
        "http://127.0.0.1:24816/",
        "http://user@127.0.0.1:24816",
        "http://127.0.0.1:24816#fragment",
    ] {
        headers.insert(header::ORIGIN, origin.parse().unwrap());
        assert_eq!(
            policy.check(&headers).unwrap_err().code,
            ErrorCode::ForbiddenOrigin
        );
    }
    headers.remove(header::ORIGIN);
    for host in [
        "evil.example:24816",
        "127.0.0.1:24817",
        "127.0.0.2:24816",
        "localhost",
        "user@127.0.0.1:24816",
    ] {
        headers.insert(header::HOST, host.parse().unwrap());
        assert_eq!(
            policy.check(&headers).unwrap_err().code,
            ErrorCode::ForbiddenHost
        );
    }
    headers.insert(header::HOST, "LOCALHOST:24816".parse().unwrap());
    headers.insert(header::ORIGIN, "http://LOCALHOST:24816".parse().unwrap());
    assert!(policy.check(&headers).is_ok());
    assert!(AccessPolicy::new("0.0.0.0:24816".parse().unwrap(), vec![]).is_err());
    for origin in [
        "*",
        "http://localhost:3000/path",
        "http://127.1:3000",
        "http://127.0.0.1:99999",
        "http://example.com",
        "http://localhost:3000?x=1",
    ] {
        assert!(validate_origin(origin).is_err());
    }
    assert!(validate_origin("http://LOCALHOST:3000").is_ok());
}
#[tokio::test]
async fn http_and_websocket_policy_precedes_upgrade() {
    let mut fixture = common::start(&[]).await;
    for origin in ["null", "https://untrusted.example"] {
        let response = fixture
            .http
            .get(format!("{}/api/v1/devices", fixture.base()))
            .header("Origin", origin)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
        let error: ErrorResponse = response.json().await.unwrap();
        assert_eq!(error.error.code, ErrorCode::ForbiddenOrigin);
        let mut request = fixture.websocket().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
        let error = connect_async(request).await.unwrap_err();
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status(), 403)
            }
            other => panic!("unexpected policy result: {other:?}"),
        }
    }
    assert_eq!(
        fixture
            .http
            .get(format!("{}/api/v1/status", fixture.base()))
            .header("Host", "untrusted.example:24816")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = fixture
        .http
        .get(format!("{}/api/v1/status", fixture.base()))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://localhost:3000"
    );
    let response = fixture
        .http
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/api/v1/devices", fixture.base()),
        )
        .header("Origin", "http://localhost:3000")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "Content-Type")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn commands_are_bounded_and_unknown_fields_are_sanitized() {
    let mut fixture = common::start(&["generic-ddc"]).await;
    fixture
        .wait_for(|snapshot| common::value(snapshot, "brightness").is_some())
        .await;
    for body in [
        format!(
            "{{\"op\":\"set\",\"value\":50,\"secret\":\"{}\"}}",
            "PRIVATE-INPUT"
        ),
        "{malformed PRIVATE-INPUT".to_owned(),
        format!("{}{{\"op\":\"set\",\"value\":50}}", " ".repeat(16 * 1024)),
    ] {
        let response = fixture
            .http
            .post(format!(
                "{}/api/v1/devices/monitor/characteristics/brightness",
                fixture.base()
            ))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let text = response.text().await.unwrap();
        assert!(!text.contains("PRIVATE-INPUT"));
        let error: ErrorResponse = serde_json::from_str(&text).unwrap();
        error.validate().unwrap();
    }
    assert_eq!(
        fixture
            .execute("brightness", serde_json::json!({"op":"step","steps":101}))
            .await
            .status(),
        422
    );
    assert_eq!(
        fixture
            .execute("brightness", serde_json::json!({"op":"toggle"}))
            .await
            .status(),
        422
    );
    assert_eq!(
        fixture
            .execute("brightness", serde_json::json!({"op":"set","value":101}))
            .await
            .status(),
        422
    );
    assert!(fixture.hardware.lock().unwrap().set_calls.is_empty());
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn recent_live_logs_deduplicate_and_lag_closes_1013() {
    let mut fixture = common::start(&[]).await;
    for _ in 0..220 {
        fixture.server.application.logs().record(
            LogLevel::Info,
            "synapsed",
            "Isolated recent log.",
        );
    }
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    let first = socket.next().await.unwrap().unwrap();
    assert!(matches!(first, Message::Text(_)));
    let Message::Text(recent) = socket.next().await.unwrap().unwrap() else {
        panic!("recent logs expected")
    };
    let ServerEvent::Logs { entries } = serde_json::from_str(&recent).unwrap() else {
        panic!("recent logs event expected")
    };
    assert_eq!(entries.len(), 200);
    assert!(entries.windows(2).all(
        |entries| entries[0].id.parse::<u64>().unwrap() < entries[1].id.parse::<u64>().unwrap()
    ));
    let last = entries.last().unwrap().id.parse::<u64>().unwrap();
    fixture
        .server
        .application
        .logs()
        .record(LogLevel::Info, "synapsed", "Isolated live log.");
    let Message::Text(live) = socket.next().await.unwrap().unwrap() else {
        panic!("live log expected")
    };
    let ServerEvent::Log { entry } = serde_json::from_str(&live).unwrap() else {
        panic!("live log event expected")
    };
    assert!(entry.id.parse::<u64>().unwrap() > last);
    // No await: overflow the subscribed receiver deterministically before it can run.
    for _ in 0..600 {
        fixture.server.application.logs().record(
            LogLevel::Info,
            "synapsed",
            "Isolated lagging log.",
        );
    }
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Close(Some(frame)) = message else {
        panic!("lag must close rather than drop ordered logs")
    };
    assert_eq!(u16::from(frame.code), 1013);
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn event_stream_has_no_authenticate_frame_and_shutdown_is_bounded() {
    let mut fixture = common::start(&[]).await;
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    socket.next().await.unwrap().unwrap();
    socket.next().await.unwrap().unwrap();
    socket
        .send(Message::Text(
            "{\"type\":\"authenticate\",\"token\":\"PRIVATE-INPUT\"}".into(),
        ))
        .await
        .unwrap();
    let Message::Close(Some(frame)) = socket.next().await.unwrap().unwrap() else {
        panic!("client commands must close the events-only stream")
    };
    assert_eq!(u16::from(frame.code), 1008);
    let started = std::time::Instant::now();
    fixture.server.shutdown().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
}
#[test]
fn logging_discards_raw_fields_and_bounds_public_ring() {
    let logs = LogRing::new(false);
    tracing::dispatcher::with_default(&logs.tracing_dispatch(), || {
        tracing::error!(target: "dependency", request = "PRIVATE-INPUT", settings = ?"PRIVATE-SETTINGS", "PRIVATE-SOURCE");
    });
    let serialized = serde_json::to_string(&logs.recent()).unwrap();
    assert!(!serialized.contains("PRIVATE-"));
    for _ in 0..250 {
        logs.record(
            LogLevel::Info,
            "synapsed\nunsafe-target",
            "Bounded public log.",
        );
    }
    let entries = logs.recent();
    assert_eq!(entries.len(), 200);
    for entry in entries {
        entry.validate().unwrap();
        assert!(!entry.target.contains('\n'));
        assert!(entry.message.len() <= 256);
    }
}

#[tokio::test]
async fn oversized_websocket_frames_do_not_enter_the_router() {
    let mut fixture = common::start(&[]).await;
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    socket.next().await.unwrap().unwrap();
    socket.next().await.unwrap().unwrap();
    let _ = socket
        .send(Message::Binary(vec![0; 1024 * 1024 + 1].into()))
        .await;
    let response = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap();
    assert!(!matches!(response, Some(Ok(Message::Text(_)))));
    assert_eq!(
        fixture
            .http
            .get(format!("{}/api/v1/status", fixture.base()))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn heartbeat_closes_after_the_next_missed_pong() {
    let mut fixture = common::start_with_heartbeat(&[], Duration::from_millis(100)).await;
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    socket.next().await.unwrap().unwrap();
    socket.next().await.unwrap().unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(first, Message::Ping(_)));
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Close(Some(frame)) = second else {
        panic!("missed next pong must close the connection")
    };
    assert_eq!(u16::from(frame.code), 1001);
    fixture.server.shutdown().await.unwrap();
}

#[test]
fn config_paths_use_injected_environment() {
    let values = std::collections::BTreeMap::from([
        ("HOME".to_owned(), std::ffi::OsString::from("/home/test")),
        (
            "XDG_CONFIG_HOME".to_owned(),
            std::ffi::OsString::from("/config"),
        ),
        (
            "XDG_DATA_HOME".to_owned(),
            std::ffi::OsString::from("relative"),
        ),
        (
            "XDG_STATE_HOME".to_owned(),
            std::ffi::OsString::from("/state"),
        ),
    ]);
    let paths = ConfigPaths::from_environment(|key| values.get(key).cloned()).unwrap();
    assert_eq!(paths.home, std::path::PathBuf::from("/home/test"));
    assert_eq!(paths.config_home, std::path::PathBuf::from("/config"));
    assert_eq!(
        paths.data_home,
        std::path::PathBuf::from("/home/test/.local/share")
    );
    assert_eq!(paths.state_home, std::path::PathBuf::from("/state"));
}

#[tokio::test]
async fn plugin_runtime_warning_text_reaches_http_logs() {
    let mut fixture = common::start(&[]).await;
    let dispatch = fixture.logs.tracing_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::warn!(
            target: "synapsed::plugins",
            public_message = %"Lua runtime warning from ctx.log."
        );
    });
    let logs: synapsed_api::LogsResponse = fixture
        .http
        .get(format!("{}/api/v1/logs", fixture.base()))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(logs.entries.iter().any(|entry| {
        entry.target == "synapsed::plugins" && entry.message == "Lua runtime warning from ctx.log."
    }));
    fixture.server.shutdown().await.unwrap();
}
