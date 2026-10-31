#![allow(dead_code)]
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use synapsed::{
    config::{Config, ConfigPaths},
    inventory::Sources,
    logs::LogRing,
    server::{Application, RunningServer},
};
use synapsed_api::{NodeInfo, Snapshot};
use synapsed_core::StateStore;
use synapsed_ddc::{
    DdcClient, LazyDdc, VcpValue,
    test_support::{FakeBackend, FakeEndpoint, FakeState, FakeStateHandle},
};
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost, capabilities::ddc::DdcCapabilityFactory,
};
use tempfile::TempDir;
use uuid::Uuid;

pub type Hardware = FakeStateHandle;
fn fake_hardware() -> Hardware {
    let endpoint = FakeEndpoint {
        key: "isolated-native-key".into(),
        manufacturer: Some("BNQ".into()),
        product: Some(1),
        product_name: Some("BenQ RD280U".into()),
        identity: Some("a".repeat(64)),
        replies: BTreeMap::from([
            (
                0x10,
                VcpValue {
                    current: 40,
                    maximum: 100,
                },
            ),
            (
                0x19,
                VcpValue {
                    current: 2,
                    maximum: 5,
                },
            ),
            (
                0xd7,
                VcpValue {
                    current: 0x0120,
                    maximum: 0x0231,
                },
            ),
            (
                0xd9,
                VcpValue {
                    current: 0x0305,
                    maximum: 0x070a,
                },
            ),
        ]),
    };
    Arc::new(Mutex::new(FakeState {
        endpoints: vec![endpoint],
        ..FakeState::default()
    }))
}
pub struct TestServer {
    pub server: RunningServer,
    pub http: reqwest::Client,
    pub hardware: Hardware,
    pub logs: LogRing,
    pub ddc: LazyDdc,
    pub store: StateStore,
    pub directory: TempDir,
}
impl TestServer {
    pub fn base(&self) -> String {
        format!("http://{}", self.server.address)
    }
    pub fn websocket(&self) -> String {
        format!("ws://{}/api/v1/events", self.server.address)
    }
    pub async fn snapshot(&self) -> Snapshot {
        let snapshot: Snapshot = self
            .http
            .get(format!("{}/api/v1/devices", self.base()))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        snapshot.validate().unwrap();
        snapshot
    }
    pub async fn wait_for(&self, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let mut changed = self.store.subscribe();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let snapshot = changed.borrow_and_update().clone();
                if predicate(&snapshot) {
                    let snapshot = snapshot.as_ref().clone();
                    snapshot.validate().unwrap();
                    return snapshot;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .expect("real Lua host did not publish the expected observation")
    }
    pub async fn execute(
        &self,
        characteristic: &str,
        command: serde_json::Value,
    ) -> reqwest::Response {
        self.http
            .post(format!(
                "{}/api/v1/devices/monitor/characteristics/{}",
                self.base(),
                characteristic
            ))
            .json(&command)
            .send()
            .await
            .unwrap()
    }
}
pub async fn start(packages: &[&str]) -> TestServer {
    start_with_heartbeat(packages, Duration::from_secs(30)).await
}
pub async fn start_with_heartbeat(packages: &[&str], heartbeat_interval: Duration) -> TestServer {
    let directory = tempfile::tempdir().unwrap();
    let plugins = directory.path().join("drivers");
    std::fs::create_dir(&plugins).unwrap();
    for package in packages {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../drivers")
            .join(package);
        let target = plugins.join(package);
        std::fs::create_dir(&target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
    let hardware = fake_hardware();
    let store = StateStore::new(NodeInfo {
        node_id: Uuid::new_v4().to_string(),
        instance_id: Uuid::new_v4().to_string(),
        name: "Isolated daemon".into(),
    })
    .unwrap();
    let (host, ddc) = if packages.is_empty() {
        let paths = ConfigPaths {
            home: directory.path().into(),
            config_home: directory.path().into(),
            data_home: directory.path().into(),
            state_home: directory.path().into(),
        };
        let config = Config::from_text(
            &format!("[plugins]\ndirectory = {:?}\n", plugins),
            &directory.path().join("config.toml"),
            &paths,
        )
        .unwrap();
        synapsed::start_plugins(store.clone(), &config).unwrap()
    } else {
        let backend = hardware.clone();
        let client = DdcClient::start_with_backend(move || Ok(FakeBackend::new(backend))).unwrap();
        let ddc = LazyDdc::with_client(client);
        let mut registry = CapabilityRegistry::default();
        registry
            .register(Arc::new(DdcCapabilityFactory::with_shared(ddc.clone())))
            .unwrap();
        let host = PluginHost::start(
            store.clone(),
            PluginConfig {
                directory: plugins,
                directory_explicit: true,
                disabled: vec![],
                discovery_interval_ms: 1000,
                settings: BTreeMap::new(),
            },
            registry,
        )
        .unwrap();
        (host, ddc)
    };
    let logs = LogRing::new(false);
    let inventory = Sources::with_sysfs_root(
        store.clone(),
        ddc.clone(),
        host.bindings(),
        host.catalog(),
        directory.path().join("empty-sysfs"),
    );
    let application = Application::new_with_heartbeat_interval(
        store.clone(),
        host,
        logs.clone(),
        inventory,
        heartbeat_interval,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server =
        RunningServer::start(listener, application, vec!["http://localhost:3000".into()]).unwrap();
    TestServer {
        server,
        http: reqwest::Client::new(),
        hardware,
        logs,
        ddc,
        store,
        directory,
    }
}
pub fn value(snapshot: &Snapshot, id: &str) -> Option<synapsed_api::CharacteristicValue> {
    snapshot
        .devices
        .iter()
        .find(|device| device.id == "monitor")?
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == id)?
        .value
        .clone()
}
