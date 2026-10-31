#![allow(dead_code)]
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use synapsed::{
    config::{Config, ConfigPaths},
    logs::LogRing,
    server::{Application, RunningServer},
};
use synapsed_api::{ApiError, ErrorCode, NodeInfo, Snapshot};
use synapsed_core::StateStore;
use synapsed_ddc::{
    DdcBackend, DdcClient, DdcTransport, InventoryWithOwnedHandles, OwnedEndpoint, VcpValue,
};
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost, capabilities::ddc::DdcCapabilityFactory,
};
use tempfile::TempDir;
use uuid::Uuid;

pub struct Hardware {
    pub registers: BTreeMap<u8, VcpValue>,
    pub failures: BTreeSet<u8>,
    pub writes: Vec<(u8, u16)>,
    pub coerce: Option<u16>,
}
impl Default for Hardware {
    fn default() -> Self {
        Self {
            registers: BTreeMap::from([
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
            failures: BTreeSet::new(),
            writes: vec![],
            coerce: None,
        }
    }
}
struct Backend(Arc<Mutex<Hardware>>);
struct Transport(Arc<Mutex<Hardware>>);
impl DdcBackend for Backend {
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, ApiError> {
        Ok(InventoryWithOwnedHandles {
            endpoints: vec![OwnedEndpoint {
                key: "isolated-native-key".into(),
                manufacturer: Some("BNQ".into()),
                product: Some(1),
                product_name: Some("BenQ RD280U".into()),
                identity: Some("a".repeat(64)),
                transport: Box::new(Transport(self.0.clone())),
            }],
            errors: vec![],
        })
    }
}
impl DdcTransport for Transport {
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, ApiError> {
        let hardware = self.0.lock().unwrap();
        if hardware.failures.contains(&code) {
            return Err(ApiError::new(
                ErrorCode::DdcError,
                "Isolated transport read failed.",
            ));
        }
        hardware
            .registers
            .get(&code)
            .copied()
            .ok_or_else(|| ApiError::new(ErrorCode::DdcError, "Isolated register is unavailable."))
    }
    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), ApiError> {
        let mut hardware = self.0.lock().unwrap();
        hardware.writes.push((code, value));
        let observed = hardware.coerce.unwrap_or(value);
        hardware
            .registers
            .get_mut(&code)
            .ok_or_else(|| ApiError::new(ErrorCode::DdcError, "Isolated register is unavailable."))?
            .current = observed;
        Ok(())
    }
}
pub struct TestServer {
    pub server: RunningServer,
    pub http: reqwest::Client,
    pub hardware: Arc<Mutex<Hardware>>,
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
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let snapshot = self.snapshot().await;
                if predicate(&snapshot) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
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
    let hardware = Arc::new(Mutex::new(Hardware::default()));
    let store = StateStore::new(NodeInfo {
        node_id: Uuid::new_v4().to_string(),
        instance_id: Uuid::new_v4().to_string(),
        name: "Isolated daemon".into(),
    })
    .unwrap();
    let host = if packages.is_empty() {
        // Exercise the same production lazy-native composition as the binary, with no hardware access.
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
        // Only tests inject an in-memory transport, constructed on the real native owner thread.
        let backend = hardware.clone();
        let client = DdcClient::start_with_backend(move || Ok(Backend(backend))).unwrap();
        let mut registry = CapabilityRegistry::default();
        registry
            .register(Arc::new(DdcCapabilityFactory::new(client)))
            .unwrap();
        PluginHost::start(
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
        .unwrap()
    };
    let application = Application::new(store, host, LogRing::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server =
        RunningServer::start(listener, application, vec!["http://localhost:3000".into()]).unwrap();
    TestServer {
        server,
        http: reqwest::Client::new(),
        hardware,
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
