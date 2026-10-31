mod common;

use common::start;
use std::time::Duration;
use synapsed_api::{DeviceResponse, ErrorCode, ErrorResponse};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

#[tokio::test]
async fn driver_status_transition_uses_warning_log_level() {
    let mut fixture = start(&[]).await;
    let mut log_events = fixture.server.application.logs().subscribe();
    let driver_id = "status-test";
    let publish = |state| {
        fixture
            .server
            .application
            .store()
            .publish_driver_state(
                driver_id,
                synapsed_api::DriverStatus {
                    id: driver_id.into(),
                    version: "1.0.0".into(),
                    state,
                    error: None,
                },
                vec![],
            )
            .unwrap();
    };

    publish(synapsed_api::DriverState::Online);
    let online_log = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_, entry) = log_events.recv().await.unwrap();
            if entry.target == driver_id {
                break entry;
            }
        }
    })
    .await
    .expect("online driver status was not logged");
    assert_eq!(online_log.level, synapsed_api::LogLevel::Info);

    publish(synapsed_api::DriverState::Disconnected);
    let disconnected_log = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_, entry) = log_events.recv().await.unwrap();
            if entry.target == driver_id {
                break entry;
            }
        }
    })
    .await
    .expect("disconnected driver status was not logged");
    assert_eq!(disconnected_log.level, synapsed_api::LogLevel::Warn);

    let logs: synapsed_api::LogsResponse = fixture
        .http
        .get(format!("{}/api/v1/logs", fixture.base()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    logs.validate().unwrap();
    assert!(logs.entries.iter().any(|entry| {
        entry.target == driver_id
            && entry.message == "Installed driver is disconnected."
            && entry.level == synapsed_api::LogLevel::Warn
    }));

    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn dispatch_busy_and_timeout_keep_their_http_statuses() {
    for (code, status) in [(ErrorCode::Busy, 503), (ErrorCode::Timeout, 504)] {
        let mut fixture = start(&["generic-ddc"]).await;
        fixture
            .wait_for(|snapshot| common::value(snapshot, "brightness").is_some())
            .await;
        {
            let mut state = fixture.hardware.lock().unwrap();
            state.get_failures.clear();
            state.fail_get_code(
                0x10,
                synapsed_ddc::DriverError::new(code, "Injected dispatch failure."),
            );
        }
        let response = fixture
            .execute("brightness", serde_json::json!({"op":"set","value":50}))
            .await;
        assert_eq!(response.status(), status);
        let error: ErrorResponse = response.json().await.unwrap();
        assert_eq!(error.error.code, code);

        fixture.server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn execute_rejects_wrong_content_type() {
    let mut fixture = start(&["generic-ddc"]).await;
    fixture
        .wait_for(|snapshot| common::value(snapshot, "brightness").is_some())
        .await;

    let response = fixture
        .http
        .post(format!(
            "{}/api/v1/devices/monitor/characteristics/brightness",
            fixture.base()
        ))
        .header("Content-Type", "text/plain")
        .body(r#"{"op":"set","value":50}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let error: ErrorResponse = response.json().await.unwrap();
    assert_eq!(error.error.code, ErrorCode::InvalidRequest);

    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn execute_body_timeout_returns_gateway_timeout() {
    let mut fixture = start(&["generic-ddc"]).await;
    fixture
        .wait_for(|snapshot| common::value(snapshot, "brightness").is_some())
        .await;
    let mut stream = TcpStream::connect(fixture.server.address).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /api/v1/devices/monitor/characteristics/brightness HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{{\"op\":",
                fixture.server.address
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    tokio::time::timeout(Duration::from_secs(8), reader.read_line(&mut status_line))
        .await
        .expect("server did not enforce the command body deadline")
        .unwrap();
    assert!(status_line.starts_with("HTTP/1.1 504"), "{status_line}");

    drop(reader);
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_device_returns_device_response_body() {
    let mut fixture = start(&["generic-ddc"]).await;
    let snapshot = fixture
        .wait_for(|snapshot| snapshot.devices.iter().any(|device| device.id == "monitor"))
        .await;

    let response = fixture
        .http
        .get(format!("{}/api/v1/devices/monitor", fixture.base()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let device: DeviceResponse = response.json().await.unwrap();
    device.validate().unwrap();
    assert_eq!(device.node, snapshot.node);
    assert_eq!(device.revision, snapshot.revision);
    assert_eq!(
        device.device,
        snapshot
            .devices
            .iter()
            .find(|candidate| candidate.id == "monitor")
            .unwrap()
            .clone()
    );

    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_device_rejects_commands_as_service_unavailable() {
    let mut fixture = start(&[]).await;
    let device = synapsed_api::Device {
        id: "monitor".into(),
        driver: "offline-test".into(),
        kind: "monitor".into(),
        label: "Offline monitor".into(),
        status: synapsed_api::DeviceStatus::Unavailable,
        error: None,
        characteristics: vec![synapsed_api::Characteristic {
            id: "brightness".into(),
            label: "Brightness".into(),
            spec: synapsed_api::CharacteristicSpec::Number {
                minimum: Some(0.0),
                maximum: Some(100.0),
                step: Some(1.0),
                integer: true,
                unit: None,
            },
            access: synapsed_api::CharacteristicAccess {
                read: true,
                write: true,
                notify: false,
            },
            value: Some(synapsed_api::CharacteristicValue::Number(40.0)),
            status: synapsed_api::CharacteristicStatus::Ok,
            error: None,
        }],
    };
    fixture
        .server
        .application
        .store()
        .publish_driver_state(
            "offline-test",
            synapsed_api::DriverStatus {
                id: "offline-test".into(),
                version: "1.0.0".into(),
                state: synapsed_api::DriverState::Online,
                error: None,
            },
            vec![device],
        )
        .unwrap();

    let response = fixture
        .http
        .post(format!(
            "{}/api/v1/devices/monitor/characteristics/brightness",
            fixture.base()
        ))
        .json(&serde_json::json!({"op":"set","value":50}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let error: ErrorResponse = response.json().await.unwrap();
    assert_eq!(error.error.code, ErrorCode::DeviceUnavailable);

    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn preflight_rejects_put_and_unapproved_headers() {
    let mut fixture = start(&[]).await;
    let url = format!("{}/api/v1/devices", fixture.base());

    for (method, headers) in [("PUT", "content-type"), ("POST", "content-type, x-token")] {
        let response = fixture
            .http
            .request(reqwest::Method::OPTIONS, &url)
            .header("Origin", "http://localhost:3000")
            .header("Access-Control-Request-Method", method)
            .header("Access-Control-Request-Headers", headers)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let error: ErrorResponse = response.json().await.unwrap();
        assert_eq!(error.error.code, ErrorCode::InvalidRequest);
    }

    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn requests_during_shutdown_return_service_unavailable() {
    let mut fixture = start(&[]).await;
    fixture.server.application.begin_shutdown();

    let response = fixture
        .http
        .get(format!("{}/api/v1/status", fixture.base()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let error: ErrorResponse = response.json().await.unwrap();
    assert_eq!(error.error.code, ErrorCode::DeviceUnavailable);

    fixture.server.shutdown().await.unwrap();
}
