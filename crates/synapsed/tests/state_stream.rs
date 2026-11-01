mod common;
use common::{start, value};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use synapsed_api::{
    CharacteristicStatus, CharacteristicValue, ErrorResponse, ServerEvent, Snapshot,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn next_event(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ServerEvent {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    let event: ServerEvent = serde_json::from_str(&text).unwrap();
                    event.validate().unwrap();
                    return event;
                }
                Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn immediate_empty_snapshot_and_real_routes() {
    let mut fixture = start(&[]).await;
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    let ServerEvent::Snapshot { snapshot } = next_event(&mut socket).await else {
        panic!("snapshot must precede logs without any client handshake")
    };
    assert!(snapshot.devices.is_empty());
    assert!(snapshot.drivers.is_empty());
    assert!(matches!(
        next_event(&mut socket).await,
        ServerEvent::Logs { .. }
    ));
    let status: synapsed_api::StatusResponse = fixture
        .http
        .get(format!("{}/api/v1/status", fixture.base()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    status.validate().unwrap();
    assert_eq!(status.node, snapshot.node);
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
    for path in [
        "/api/v1/missing",
        "/api/v1/devices/missing",
        "/api/v1/monitor",
    ] {
        assert_eq!(
            fixture
                .http
                .get(format!("{}{path}", fixture.base()))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    assert_eq!(
        fixture
            .execute("missing", serde_json::json!({"op":"toggle"}))
            .await
            .status(),
        404
    );
    socket.close(None).await.unwrap();
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn two_clients_relative_writes_serialize_and_websocket_observes() {
    let mut fixture = start(&["generic-ddc"]).await;
    let initial = fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(40.0))
        })
        .await;
    assert_eq!(initial.devices[0].driver, "generic-ddc");
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    next_event(&mut socket).await;
    next_event(&mut socket).await;
    let (first, second) = tokio::join!(
        fixture.execute("brightness", serde_json::json!({"op":"step","steps":1})),
        fixture.execute("brightness", serde_json::json!({"op":"step","steps":1}))
    );
    assert_eq!(first.status(), 200);
    assert_eq!(second.status(), 200);
    let first: Snapshot = first.json().await.unwrap();
    let second: Snapshot = second.json().await.unwrap();
    first.validate().unwrap();
    second.validate().unwrap();
    let observed = fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(50.0))
        })
        .await;
    assert!(observed.revision.parse::<u64>().unwrap() > initial.revision.parse::<u64>().unwrap());
    let writes = fixture
        .hardware
        .lock()
        .unwrap()
        .set_calls
        .iter()
        .map(|(_, code, value)| (*code, *value))
        .collect::<Vec<_>>();
    assert_eq!(writes, [(0x10, 45), (0x10, 50)]);
    loop {
        if let ServerEvent::Snapshot { snapshot } = next_event(&mut socket).await
            && value(&snapshot, "brightness") == Some(CharacteristicValue::Number(50.0))
        {
            break;
        }
    }
    socket.close(None).await.unwrap();
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn polling_observes_external_changes_without_subscribers() {
    let mut fixture = start(&["generic-ddc"]).await;
    fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(40.0))
        })
        .await;
    fixture.hardware.lock().unwrap().endpoints[0]
        .replies
        .get_mut(&0x10)
        .unwrap()
        .current = 55;
    // No websocket or API mutation causes this transition; only the actual Lua observation loop.
    let observed = fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(55.0))
        })
        .await;
    assert_eq!(
        observed.devices[0].characteristics[0].status,
        CharacteristicStatus::Ok
    );
    assert!(fixture.hardware.lock().unwrap().set_calls.is_empty());
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn polling_change_emits_characteristic_changed_after_snapshot() {
    let mut fixture = start(&["generic-ddc"]).await;
    fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(40.0))
        })
        .await;
    let (mut socket, _) = connect_async(fixture.websocket()).await.unwrap();
    assert!(matches!(
        next_event(&mut socket).await,
        ServerEvent::Snapshot { .. }
    ));
    assert!(matches!(
        next_event(&mut socket).await,
        ServerEvent::Logs { .. }
    ));

    fixture.hardware.lock().unwrap().endpoints[0]
        .replies
        .get_mut(&0x10)
        .unwrap()
        .current = 55;

    let mut snapshot_revisions = Vec::new();
    let change = loop {
        match next_event(&mut socket).await {
            ServerEvent::Snapshot { snapshot } => snapshot_revisions.push(snapshot.revision),
            ServerEvent::CharacteristicChanged { change } => break change,
            _ => {}
        }
    };
    assert_eq!(change.device_id, "monitor");
    assert_eq!(change.characteristic_id, "brightness");
    assert_eq!(change.value, Some(CharacteristicValue::Number(55.0)));
    assert_eq!(change.status, CharacteristicStatus::Ok);
    assert_eq!(change.error, None);
    assert!(
        snapshot_revisions
            .iter()
            .any(|revision| revision == &change.revision)
    );
    socket.close(None).await.unwrap();
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn packed_observations_and_coerced_readback_are_authoritative() {
    let mut fixture = start(&["generic-ddc", "benq-rd280u"]).await;
    fixture
        .wait_for(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
                && value(snapshot, "moonhalo.power") == Some(CharacteristicValue::Boolean(true))
        })
        .await;
    let response = fixture
        .execute("moonhalo.power", serde_json::json!({"op":"toggle"}))
        .await;
    assert_eq!(response.status(), 200);
    let snapshot: Snapshot = response.json().await.unwrap();
    snapshot.validate().unwrap();
    assert_eq!(
        value(&snapshot, "moonhalo.power"),
        Some(CharacteristicValue::Boolean(false))
    );
    assert_eq!(
        fixture.hardware.lock().unwrap().endpoints[0].replies[&0xd7].current,
        0x0110
    );
    let response = fixture
        .execute(
            "moonhalo.brightness",
            serde_json::json!({"op":"step","steps":1}),
        )
        .await;
    assert_eq!(response.status(), 200);
    let snapshot: Snapshot = response.json().await.unwrap();
    assert_eq!(
        value(&snapshot, "moonhalo.brightness"),
        Some(CharacteristicValue::Number(6.0))
    );
    assert_eq!(
        value(&snapshot, "moonhalo.color_temperature"),
        Some(CharacteristicValue::Number(3.0))
    );
    assert_eq!(
        fixture.hardware.lock().unwrap().endpoints[0].replies[&0xd9].current,
        0x0306
    );
    fixture.hardware.lock().unwrap().coercion.insert(0x10, 57);
    let response = fixture
        .execute("brightness", serde_json::json!({"op":"set","value":60}))
        .await;
    assert_eq!(response.status(), 200);
    let snapshot: Snapshot = response.json().await.unwrap();
    assert_eq!(
        value(&snapshot, "brightness"),
        Some(CharacteristicValue::Number(57.0))
    );
    assert_ne!(
        value(&snapshot, "brightness"),
        Some(CharacteristicValue::Number(60.0))
    );
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn failed_fresh_read_never_writes_and_later_observation_recovers() {
    let mut fixture = start(&["generic-ddc"]).await;
    fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(40.0))
        })
        .await;
    fixture.hardware.lock().unwrap().fail_reads.insert(0x10);
    let response = fixture
        .execute("brightness", serde_json::json!({"op":"step","steps":1}))
        .await;
    assert_eq!(response.status(), 503);
    let error: ErrorResponse = response.json().await.unwrap();
    error.validate().unwrap();
    assert!(fixture.hardware.lock().unwrap().set_calls.is_empty());
    fixture.hardware.lock().unwrap().fail_reads.clear();
    fixture.hardware.lock().unwrap().endpoints[0]
        .replies
        .get_mut(&0x10)
        .unwrap()
        .current = 51;
    fixture
        .wait_for(|snapshot| {
            value(snapshot, "brightness") == Some(CharacteristicValue::Number(51.0))
                && snapshot.devices[0].characteristics[0].status == CharacteristicStatus::Ok
        })
        .await;
    fixture.server.shutdown().await.unwrap();
}
#[tokio::test]
async fn vendor_failure_retains_healthy_brightness_through_lua_refresh() {
    let mut fixture = start(&["generic-ddc", "benq-rd280u"]).await;
    fixture
        .wait_for(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
                && value(snapshot, "brightness").is_some()
        })
        .await;
    fixture.hardware.lock().unwrap().fail_reads.insert(0xd9);
    let response = fixture
        .execute("brightness", serde_json::json!({"op":"set","value":52}))
        .await;
    assert_eq!(response.status(), 200);
    let snapshot: Snapshot = response.json().await.unwrap();
    assert_eq!(
        value(&snapshot, "brightness"),
        Some(CharacteristicValue::Number(52.0))
    );
    let monitor = snapshot
        .devices
        .iter()
        .find(|device| device.id == "monitor")
        .unwrap();
    assert_eq!(monitor.driver, "benq-rd280u");
    assert_eq!(
        monitor
            .characteristics
            .iter()
            .find(|characteristic| characteristic.id == "brightness")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    assert_ne!(
        monitor
            .characteristics
            .iter()
            .find(|characteristic| characteristic.id == "moonhalo.brightness")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    fixture.server.shutdown().await.unwrap();
}
