use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use synapsed_api::{
    CharacteristicCommand, CharacteristicSpec, CharacteristicValue, ErrorCode, NodeInfo, Snapshot,
};
use synapsed_bluetooth::{
    BluetoothClient, Uuid,
    test_support::{FakeBackend, FakePeripheral, FakeState, FakeStateHandle, notify},
};
use synapsed_core::StateStore;
use synapsed_ddc::{
    DdcClient,
    test_support::{FakeBackend as DdcFakeBackend, FakeState as DdcFakeState},
};
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost,
    capabilities::{bluetooth::BluetoothCapabilityFactory, ddc::DdcCapabilityFactory},
};
use tempfile::TempDir;

const SERVICE: Uuid = Uuid::from_u128(0x99fa0001338a10248a49009c0215f78a);
const COMMAND: Uuid = Uuid::from_u128(0x99fa0002338a10248a49009c0215f78a);
const HEIGHT: Uuid = Uuid::from_u128(0x99fa0021338a10248a49009c0215f78a);
const REFERENCE: Uuid = Uuid::from_u128(0x99fa0031338a10248a49009c0215f78a);
const TARGET_110: [u8; 2] = [0xC0, 0x12];

fn package_dir() -> TempDir {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drivers/ikea-idasen");
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&source, &directory.path().join("ikea-idasen"));
    directory
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fake_state(desks: &[(&str, &str)]) -> FakeStateHandle {
    Arc::new(Mutex::new(FakeState {
        peripherals: desks
            .iter()
            .map(|(key, name)| FakePeripheral {
                key: (*key).to_owned(),
                name: Some((*name).to_owned()),
                services: vec![SERVICE],
                values: BTreeMap::from([(HEIGHT, vec![0xE8, 0x03, 0, 0])])
                    .into_iter()
                    .collect(),
            })
            .collect(),
        ..FakeState::default()
    }))
}

fn registry(state: &FakeStateHandle, with_ddc: bool) -> CapabilityRegistry {
    let state = state.clone();
    let client = BluetoothClient::start_with_backend(move || Ok(FakeBackend::new(state))).unwrap();
    let mut registry = CapabilityRegistry::default();
    registry
        .register(Arc::new(BluetoothCapabilityFactory::new(client)))
        .unwrap();
    if with_ddc {
        let state = Arc::new(Mutex::new(DdcFakeState::default()));
        let client =
            DdcClient::start_with_backend(move || Ok(DdcFakeBackend::new(state.clone()))).unwrap();
        registry
            .register(Arc::new(DdcCapabilityFactory::new(client)))
            .unwrap();
    }
    registry
}

fn start_host(
    directory: &Path,
    state: &FakeStateHandle,
    settings: BTreeMap<String, serde_json::Value>,
    with_ddc: bool,
) -> (PluginHost, StateStore) {
    let store = StateStore::new(NodeInfo {
        node_id: "00000000-0000-4000-8000-000000000001".to_owned(),
        instance_id: "00000000-0000-4000-8000-000000000002".to_owned(),
        name: "IDASEN test host".to_owned(),
    })
    .unwrap();
    let host = PluginHost::start(
        store.clone(),
        PluginConfig {
            directory: directory.to_owned(),
            directory_explicit: true,
            disabled: Vec::new(),
            discovery_interval_ms: 1000,
            settings,
        },
        registry(state, with_ddc),
    )
    .unwrap();
    (host, store)
}

async fn until(store: &StateStore, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut changed = store.subscribe();
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let snapshot = changed.borrow_and_update().snapshot().clone();
            if predicate(&snapshot) {
                return snapshot;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected plugin host state transition; snapshot: {:#?}",
            store.snapshot()
        )
    })
}

fn height(snapshot: &Snapshot) -> f64 {
    let device = snapshot
        .devices
        .iter()
        .find(|device| device.id == "desk")
        .unwrap();
    match device
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == "height")
        .unwrap()
        .value
        .as_ref()
        .unwrap()
    {
        CharacteristicValue::Number(value) => *value,
        other => panic!("expected numeric height, got {other:?}"),
    }
}

fn writes(state: &FakeStateHandle) -> Vec<(Uuid, Vec<u8>, Instant)> {
    state
        .lock()
        .unwrap()
        .writes
        .iter()
        .map(|write| (write.characteristic, write.bytes.clone(), write.at))
        .collect()
}

async fn shutdown(host: PluginHost) {
    host.shutdown(Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn idasen_height_moves_and_stop_ends_repeated_reference_writes() {
    let packages = package_dir();
    let state = fake_state(&[("desk-key", "Desk 1234")]);
    let (host, store) = start_host(packages.path(), &state, BTreeMap::new(), false);
    let snapshot = until(&store, |snapshot| {
        snapshot.devices.iter().any(|device| device.id == "desk")
    })
    .await;
    assert_eq!(height(&snapshot), 72.0);
    let height_characteristic = snapshot.devices[0]
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == "height")
        .unwrap();
    match &height_characteristic.spec {
        CharacteristicSpec::Number { unit, .. } => assert_eq!(unit.as_deref(), Some("cm")),
        other => panic!("expected numeric height metadata, got {other:?}"),
    }

    tokio::time::timeout(
        Duration::from_secs(1),
        host.dispatch(
            "desk",
            "height",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(110.0),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let movement_writes = writes(&state);
    assert_eq!(movement_writes[0].1, vec![0xFE, 0x00]);
    assert_eq!(movement_writes[1].1, vec![0xFF, 0x00]);
    assert_eq!(movement_writes[2].0, REFERENCE);
    assert_eq!(movement_writes[2].1, TARGET_110);
    assert!(
        movement_writes[2..]
            .iter()
            .filter(|write| write.0 == REFERENCE && write.1 == TARGET_110)
            .count()
            >= 3
    );

    notify(&state, "desk-key", HEIGHT, vec![0xC0, 0x12, 0, 0]);
    let snapshot = until(&store, |snapshot| {
        snapshot.devices.iter().any(|device| device.id == "desk") && height(snapshot) == 110.0
    })
    .await;
    assert_eq!(height(&snapshot), 110.0);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let current = writes(&state);
            if current.ends_with(&[
                (COMMAND, vec![0xFF, 0x00], current[current.len() - 2].2),
                (REFERENCE, vec![0x01, 0x80], current[current.len() - 1].2),
            ]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let ended = writes(&state);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(writes(&state), ended);

    tokio::time::timeout(
        Duration::from_secs(1),
        host.dispatch(
            "desk",
            "height",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(90.0),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    host.dispatch("desk", "stop", CharacteristicCommand::Invoke {})
        .await
        .unwrap();
    let stopped = writes(&state);
    assert!(stopped.ends_with(&[
        (COMMAND, vec![0xFF, 0x00], stopped[stopped.len() - 2].2),
        (REFERENCE, vec![0x01, 0x80], stopped[stopped.len() - 1].2),
    ]));

    let before_stall = stopped.len();
    tokio::time::timeout(
        Duration::from_secs(1),
        host.dispatch(
            "desk",
            "height",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(100.0),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = writes(&state);
            if current.len() >= before_stall + 5
                && current[current.len() - 2].1 == vec![0xFF, 0x00]
                && current[current.len() - 1].1 == vec![0x01, 0x80]
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert!(started.elapsed() <= Duration::from_secs(3));
    shutdown(host).await;
}

#[tokio::test(flavor = "current_thread")]
async fn idasen_restart_waits_for_active_move_final_writes() {
    let packages = package_dir();
    let state = fake_state(&[("desk-key", "Desk 1234")]);
    let (host, store) = start_host(packages.path(), &state, BTreeMap::new(), false);
    until(&store, |snapshot| {
        snapshot.devices.iter().any(|device| device.id == "desk")
    })
    .await;

    tokio::time::timeout(
        Duration::from_secs(1),
        host.dispatch(
            "desk",
            "height",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(110.0),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        writes(&state)
            .iter()
            .filter(|write| write.0 == REFERENCE && write.1 == TARGET_110)
            .count()
            >= 2
    );

    tokio::time::timeout(Duration::from_secs(5), host.restart_driver("ikea-idasen"))
        .await
        .unwrap()
        .unwrap();
    let restarted = writes(&state);
    assert!(restarted.ends_with(&[
        (COMMAND, vec![0xFF, 0x00], restarted[restarted.len() - 2].2),
        (
            REFERENCE,
            vec![0x01, 0x80],
            restarted[restarted.len() - 1].2
        ),
    ]));
    assert_eq!(
        restarted
            .iter()
            .filter(|write| write.0 == REFERENCE && write.1 == vec![0x01, 0x80])
            .count(),
        1
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(writes(&state), restarted);
    shutdown(host).await;
}

#[tokio::test(flavor = "current_thread")]
async fn idasen_set_at_current_height_cancels_the_active_move() {
    let packages = package_dir();
    let state = fake_state(&[("desk-key", "Desk 1234")]);
    let (host, store) = start_host(packages.path(), &state, BTreeMap::new(), false);
    until(&store, |snapshot| {
        snapshot.devices.iter().any(|device| device.id == "desk")
    })
    .await;

    tokio::time::timeout(
        Duration::from_secs(1),
        host.dispatch(
            "desk",
            "height",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(110.0),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        writes(&state)
            .iter()
            .filter(|write| write.0 == REFERENCE && write.1 == TARGET_110)
            .count()
            >= 2
    );

    tokio::time::timeout(
        Duration::from_secs(1),
        host.dispatch(
            "desk",
            "height",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(72.0),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let current = writes(&state);
            if current.ends_with(&[
                (COMMAND, vec![0xFF, 0x00], current[current.len() - 2].2),
                (REFERENCE, vec![0x01, 0x80], current[current.len() - 1].2),
            ]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let ended = writes(&state);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(writes(&state), ended);
    shutdown(host).await;
}

#[tokio::test(flavor = "current_thread")]
async fn idasen_ambiguous_names_require_a_setting() {
    let packages = package_dir();
    let state = fake_state(&[("desk-a", "Desk 1234"), ("desk-b", "Desk 5678")]);
    let (host, store) = start_host(packages.path(), &state, BTreeMap::new(), false);
    let snapshot = until(&store, |snapshot| {
        snapshot.drivers.iter().any(|driver| {
            driver
                .error
                .as_ref()
                .is_some_and(|error| error.code == ErrorCode::AmbiguousDevice)
        })
    })
    .await;
    assert_eq!(
        snapshot.drivers[0].error.as_ref().unwrap().code,
        ErrorCode::AmbiguousDevice
    );
    shutdown(host).await;

    let state = fake_state(&[("desk-a", "Desk 1234"), ("desk-b", "Desk 5678")]);
    let settings = BTreeMap::from([(
        "ikea-idasen".to_owned(),
        serde_json::json!({"name": "Desk 1234"}),
    )]);
    let (host, store) = start_host(packages.path(), &state, settings, false);
    let snapshot = until(&store, |snapshot| {
        snapshot.devices.iter().any(|device| device.id == "desk")
    })
    .await;
    assert_eq!(snapshot.devices.len(), 1);
    assert_eq!(snapshot.devices[0].id, "desk");
    shutdown(host).await;
}

#[tokio::test(flavor = "current_thread")]
async fn capability_permission_code_spaces_must_not_overlap() {
    let packages = package_dir();
    let manifest_path = packages.path().join("ikea-idasen/plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["capabilities"]["ddc"] = serde_json::json!(1);
    manifest["permissions"]["ddc"] = serde_json::json!({"read": [0], "write": []});
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let state = fake_state(&[("desk-key", "Desk 1234")]);
    let (host, store) = start_host(packages.path(), &state, BTreeMap::new(), true);
    let snapshot = until(&store, |snapshot| {
        snapshot.drivers.iter().any(|driver| {
            driver.id == "ikea-idasen"
                && driver
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == ErrorCode::PermissionDenied)
        })
    })
    .await;
    let status = snapshot
        .drivers
        .iter()
        .find(|driver| driver.id == "ikea-idasen")
        .unwrap();
    assert_eq!(
        status.error.as_ref().unwrap().message,
        "Capability permission code spaces overlap."
    );
    shutdown(host).await;
}
