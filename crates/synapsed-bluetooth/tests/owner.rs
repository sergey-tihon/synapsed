use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use synapsed_bluetooth::{
    BluetoothClient, FinalWrite, RepeatSpec, ScanFilter,
    test_support::{FakeBackend, FakePeripheral, FakeState, FakeStateHandle, drop_link},
};
use synapsed_core::{EndpointGates, EndpointToken, ErrorCode, OperationContext};
use tokio::sync::mpsc;
use uuid::Uuid;

const SERVICE: Uuid = Uuid::from_u128(1);
const VALUE: Uuid = Uuid::from_u128(2);
const OTHER: Uuid = Uuid::from_u128(3);

fn state_with(peripherals: Vec<FakePeripheral>) -> FakeStateHandle {
    Arc::new(Mutex::new(FakeState {
        peripherals,
        ..FakeState::default()
    }))
}

fn peripheral(key: &str, name: &str) -> FakePeripheral {
    FakePeripheral {
        key: key.to_owned(),
        name: Some(name.to_owned()),
        services: Vec::new(),
        values: HashMap::from([(VALUE, vec![1, 2, 3, 4])]),
    }
}

fn start_client(state: &FakeStateHandle) -> BluetoothClient {
    let state = state.clone();
    BluetoothClient::start_with_backend(move || Ok(FakeBackend::new(state))).unwrap()
}

async fn enumerate(client: &BluetoothClient) -> EndpointToken {
    let result = client
        .enumerate(ScanFilter {
            services: vec![SERVICE],
            name_prefixes: vec!["Desk".to_owned()],
        })
        .await
        .unwrap();
    result.endpoints[0].token
}

async fn connect(client: &BluetoothClient, endpoint: EndpointToken) {
    assert!(!client.connect(endpoint).await.unwrap());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if client.connect(endpoint).await.unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn operation(grant: &synapsed_core::DeviceGrant) -> OperationContext {
    grant
        .begin(
            BTreeSet::from([0]),
            BTreeMap::from([(0, 0xffff)]),
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap()
}

fn repeat(interval: Duration, max_duration: Duration, finally: Vec<FinalWrite>) -> RepeatSpec {
    RepeatSpec {
        code: 0,
        characteristic: VALUE,
        bytes: vec![9],
        interval,
        max_duration,
        finally,
    }
}

fn write_bytes(state: &FakeStateHandle) -> Vec<Vec<u8>> {
    state
        .lock()
        .unwrap()
        .writes
        .iter()
        .map(|write| write.bytes.clone())
        .collect()
}

fn final_writes() -> Vec<FinalWrite> {
    vec![
        FinalWrite {
            code: 0,
            characteristic: OTHER,
            bytes: vec![90],
        },
        FinalWrite {
            code: 0,
            characteristic: OTHER,
            bytes: vec![91],
        },
    ]
}

#[tokio::test]
async fn matching_advertisements_are_returned_and_nonmatches_are_ignored() {
    let state = state_with(vec![
        peripheral("desk-key", " desk 1 "),
        peripheral("lamp", "Lamp"),
    ]);
    let client = start_client(&state);
    let scan = client
        .enumerate(ScanFilter {
            services: vec![],
            name_prefixes: vec!["Desk".to_owned()],
        })
        .await
        .unwrap();
    assert_eq!(scan.endpoints.len(), 1);
    assert_eq!(scan.endpoints[0].product_name.as_deref(), Some("desk 1"));
    assert_eq!(
        scan.endpoints[0].identity.as_deref(),
        Some("2889fbf6251e04b227e90f4df8a8fc3c7290586122a5175232e700536786cfab")
    );
    assert_eq!(scan.endpoints[0].capability, "bluetooth");
    assert!(scan.errors.is_empty());
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn rejected_advertisements_release_backend_state() {
    let state = state_with(vec![
        peripheral("desk-key", "Desk 1"),
        peripheral("lamp", "Lamp"),
    ]);
    let client = start_client(&state);
    let scan = client
        .enumerate(ScanFilter {
            services: vec![],
            name_prefixes: vec!["Desk".to_owned()],
        })
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().forgotten, vec!["lamp".to_owned()]);
    assert_eq!(scan.endpoints.len(), 1);
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn connect_reports_pending_then_ready_and_surfaces_the_previous_failure() {
    let state = state_with(vec![peripheral("desk-key", "Desk 1")]);
    let client = start_client(&state);
    let endpoint = enumerate(&client).await;
    connect(&client, endpoint).await;
    let connected = endpoint;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    {
        let mut state = state.lock().unwrap();
        state.fail_connect = true;
        state.connect_gate = Some(gate.clone());
    }
    // Link loss creates a fresh endpoint generation for the reconnect attempt.
    drop_link(&state, "desk-key");
    let endpoint = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let scan = client
                .enumerate(ScanFilter {
                    services: vec![],
                    name_prefixes: vec!["Desk".to_owned()],
                })
                .await
                .unwrap();
            let endpoint = scan.endpoints[0].token;
            if endpoint != connected {
                break endpoint;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!client.connect(endpoint).await.unwrap());
    gate.add_permits(1);

    async fn connect_failure(
        client: &BluetoothClient,
        endpoint: EndpointToken,
    ) -> synapsed_core::DriverError {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match client.connect(endpoint).await {
                    Ok(false) => tokio::time::sleep(Duration::from_millis(5)).await,
                    Ok(true) => panic!("unexpected connection"),
                    Err(error) => break error,
                }
            }
        })
        .await
        .unwrap()
    }

    let error = connect_failure(&client, endpoint).await;
    assert_eq!(error.code, ErrorCode::BluetoothError);
    assert!(!client.connect(endpoint).await.unwrap());
    gate.add_permits(1);
    let error = connect_failure(&client, endpoint).await;
    assert_eq!(error.code, ErrorCode::BluetoothError);
    assert_eq!(state.lock().unwrap().connects, 3);
    gate.close();
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn repeat_writes_until_stop_then_runs_final_writes_in_order() {
    let state = state_with(vec![peripheral("desk-key", "Desk 1")]);
    let client = start_client(&state);
    let endpoint = enumerate(&client).await;
    connect(&client, endpoint).await;
    let grant = EndpointGates::default()
        .activate("pkg", "desk", 1, vec![endpoint])
        .unwrap();
    let context = operation(&grant).await;
    client
        .start_repeat(
            context.clone(),
            endpoint,
            repeat(
                Duration::from_millis(100),
                Duration::from_secs(2),
                final_writes(),
            ),
        )
        .await
        .unwrap();
    context.finish();
    tokio::time::sleep(Duration::from_millis(450)).await;
    let before_stop = write_bytes(&state);
    assert!(
        before_stop
            .iter()
            .filter(|bytes| **bytes == vec![9])
            .count()
            >= 4
    );
    let stop_context = operation(&grant).await;
    assert!(
        client
            .stop_repeat(stop_context.clone(), endpoint)
            .await
            .unwrap()
    );
    stop_context.finish();
    let after_stop = write_bytes(&state);
    assert_eq!(after_stop[after_stop.len() - 2..], [vec![90], vec![91]]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(write_bytes(&state), after_stop);
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn grant_revocation_and_duration_expiry_run_final_writes() {
    let state = state_with(vec![peripheral("desk-key", "Desk 1")]);
    let client = start_client(&state);
    let endpoint = enumerate(&client).await;
    connect(&client, endpoint).await;
    let grant = EndpointGates::default()
        .activate("pkg", "desk", 1, vec![endpoint])
        .unwrap();
    let context = operation(&grant).await;
    client
        .start_repeat(
            context.clone(),
            endpoint,
            repeat(
                Duration::from_millis(100),
                Duration::from_secs(3),
                final_writes(),
            ),
        )
        .await
        .unwrap();
    context.finish();
    grant.revoke();
    tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            if write_bytes(&state).ends_with(&[vec![90], vec![91]]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();

    let state = state_with(vec![peripheral("desk-key", "Desk 1")]);
    let client = start_client(&state);
    let endpoint = enumerate(&client).await;
    connect(&client, endpoint).await;
    let grant = EndpointGates::default()
        .activate("pkg", "desk", 1, vec![endpoint])
        .unwrap();
    let context = operation(&grant).await;
    client
        .start_repeat(
            context.clone(),
            endpoint,
            repeat(
                Duration::from_millis(100),
                Duration::from_millis(1000),
                final_writes(),
            ),
        )
        .await
        .unwrap();
    context.finish();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if write_bytes(&state).ends_with(&[vec![90], vec![91]]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn disconnect_drops_sessions_and_subscriptions_without_final_writes() {
    let state = state_with(vec![peripheral("desk-key", "Desk 1")]);
    let client = start_client(&state);
    let endpoint = enumerate(&client).await;
    connect(&client, endpoint).await;
    let grant = EndpointGates::default()
        .activate("pkg", "desk", 1, vec![endpoint])
        .unwrap();
    let context = operation(&grant).await;
    let (sender, mut receiver) = mpsc::channel(8);
    client
        .subscribe(
            &context,
            endpoint,
            0,
            VALUE,
            sender,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    client
        .start_repeat(
            context.clone(),
            endpoint,
            repeat(
                Duration::from_millis(100),
                Duration::from_secs(2),
                final_writes(),
            ),
        )
        .await
        .unwrap();
    context.finish();
    drop_link(&state, "desk-key");
    let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event["error"]["code"], "device_unavailable");
    assert!(
        tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .is_none()
    );
    let context = operation(&grant).await;
    let error = client
        .read(context.clone(), endpoint, 0, VALUE)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DeviceUnavailable);
    context.finish();
    let writes = write_bytes(&state);
    assert!(!writes.contains(&vec![90]));
    assert!(!writes.contains(&vec![91]));
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn read_only_context_cannot_write() {
    let state = state_with(vec![peripheral("desk-key", "Desk 1")]);
    let client = start_client(&state);
    let endpoint = enumerate(&client).await;
    connect(&client, endpoint).await;
    let grant = EndpointGates::default()
        .activate("pkg", "desk", 1, vec![endpoint])
        .unwrap();
    let context = operation(&grant).await.read_only();
    let error = client
        .write(context.clone(), endpoint, 0, VALUE, vec![7])
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(state.lock().unwrap().writes.is_empty());
    context.finish();
    client
        .shutdown(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap();
}
