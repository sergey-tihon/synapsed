use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use synapsed::{
    inventory::Sources,
    logs::LogRing,
    server::{Application, RunningServer},
};
use synapsed_api::{
    InventoryNodeKind, InventoryResponse, InventorySourceKind, InventorySourceState, NodeInfo,
};
use synapsed_core::StateStore;
use synapsed_ddc::{
    DdcClient, LazyDdc,
    test_support::{FakeBackend, FakeState, FakeStateHandle},
};
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost, capabilities::ddc::DdcCapabilityFactory,
};
use tempfile::tempdir;
use uuid::Uuid;

fn node() -> NodeInfo {
    NodeInfo {
        node_id: Uuid::new_v4().to_string(),
        instance_id: Uuid::new_v4().to_string(),
        name: "Inventory test host".to_owned(),
    }
}

fn hardware() -> FakeStateHandle {
    Arc::new(Mutex::new(FakeState::default()))
}

#[tokio::test]
async fn inventory_route_validates_and_reads_only_cached_ddc_scan() {
    let directory = tempdir().unwrap();
    let packages = directory.path().join("drivers");
    std::fs::create_dir(&packages).unwrap();
    let store = StateStore::new(node()).unwrap();
    let hardware = hardware();
    let owner_hardware = hardware.clone();
    let client =
        DdcClient::start_with_backend(move || Ok(FakeBackend::new(owner_hardware))).unwrap();
    client.enumerate().await.unwrap();
    assert_eq!(hardware.lock().unwrap().scan_calls, 1);

    let ddc = LazyDdc::with_client(client.clone());
    let mut registry = CapabilityRegistry::default();
    registry
        .register(Arc::new(DdcCapabilityFactory::with_shared(ddc.clone())))
        .unwrap();
    let host = PluginHost::start(
        store.clone(),
        PluginConfig {
            directory: packages,
            directory_explicit: true,
            disabled: Vec::new(),
            discovery_interval_ms: 1000,
            settings: BTreeMap::new(),
        },
        registry,
    )
    .unwrap();
    let sources = Sources::with_sysfs_root(
        store.clone(),
        ddc,
        host.bindings(),
        host.catalog(),
        directory.path().join("empty-sysfs"),
    );
    let application = Application::new(store.clone(), host, LogRing::new(false), sources);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut server = RunningServer::start(
        listener,
        application,
        vec!["http://localhost:3000".to_owned()],
    )
    .unwrap();
    let http = reqwest::Client::new();
    let base = format!("http://{}", server.address);

    let response = http
        .get(format!("{base}/api/v1/inventory"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let inventory: InventoryResponse = response.json().await.unwrap();
    inventory.validate().unwrap();
    assert_eq!(inventory.instance_id, store.snapshot().node.instance_id);
    assert!(
        inventory
            .nodes
            .iter()
            .any(|node| node.kind == InventoryNodeKind::Host)
    );
    assert_eq!(
        inventory
            .sources
            .iter()
            .find(|source| source.kind == InventorySourceKind::Ddc)
            .unwrap()
            .state,
        InventorySourceState::Ok,
    );
    assert_eq!(hardware.lock().unwrap().scan_calls, 1);

    let options = http
        .request(reqwest::Method::OPTIONS, format!("{base}/api/v1/inventory"))
        .header("Origin", "http://localhost:3000")
        .header("Access-Control-Request-Method", "GET")
        .send()
        .await
        .unwrap();
    assert_eq!(options.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(hardware.lock().unwrap().scan_calls, 1);
    server.shutdown().await.unwrap();
    client
        .shutdown(Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
}

#[tokio::test]
async fn inventory_without_started_ddc_reports_not_scanned() {
    let directory = tempdir().unwrap();
    let store = StateStore::new(node()).unwrap();
    let (bindings, receiver) = tokio::sync::watch::channel(Arc::new(BTreeMap::new()));
    drop(bindings);
    let ddc = LazyDdc::new();
    let sources = Sources::with_sysfs_root(
        store,
        ddc.clone(),
        receiver,
        Arc::<[synapsed_plugins::DriverCatalogEntry]>::from(Vec::new()),
        directory.path().join("empty-sysfs"),
    );
    let inventory = synapsed::inventory::Inventory::collect(&sources).await;
    inventory.validate().unwrap();
    assert_eq!(
        inventory
            .sources
            .iter()
            .find(|source| source.kind == InventorySourceKind::Ddc)
            .unwrap()
            .state,
        InventorySourceState::NotScanned,
    );
    assert!(ddc.existing().is_none());
}
