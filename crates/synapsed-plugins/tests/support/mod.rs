use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use synapsed_api::{CharacteristicValue, NodeInfo, Snapshot};
use synapsed_core::StateStore;
use synapsed_ddc::test_support::{
    FakeBackend, FakeBarrier, FakeEndpoint, FakeState, FakeStateHandle,
};
use synapsed_ddc::{DdcClient, VcpValue};
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost, capabilities::ddc::DdcCapabilityFactory,
};
#[allow(dead_code)]
pub struct BarrierRelease(FakeBarrier);
impl Drop for BarrierRelease {
    fn drop(&mut self) {
        self.0.open();
    }
}
#[allow(dead_code)]
pub trait BarrierReleaseExt {
    fn release_on_drop(&self) -> BarrierRelease;
}
impl BarrierReleaseExt for FakeBarrier {
    fn release_on_drop(&self) -> BarrierRelease {
        BarrierRelease(self.clone())
    }
}
pub struct Hardware {
    pub state: FakeStateHandle,
    pub get_barrier: FakeBarrier,
    pub set_barrier: FakeBarrier,
}
impl Hardware {
    pub fn new(screens: Vec<Screen>) -> Arc<Self> {
        let endpoints = screens
            .into_iter()
            .map(|screen| FakeEndpoint {
                key: screen.key,
                manufacturer: Some("BNQ".into()),
                product: Some(1),
                product_name: screen.name,
                identity: screen.identity,
                replies: screen.values,
            })
            .collect();
        let state = FakeState {
            endpoints,
            ..FakeState::default()
        };
        let get_barrier = state.get_barrier.clone();
        let set_barrier = state.set_barrier.clone();
        Arc::new(Self {
            state: Arc::new(Mutex::new(state)),
            get_barrier,
            set_barrier,
        })
    }
    pub fn current(&self, code: u8) -> u16 {
        self.state.lock().unwrap().endpoints[0].replies[&code].current
    }
    #[allow(dead_code)]
    pub fn mutate(&self, code: u8, value: u16) {
        self.state.lock().unwrap().endpoints[0]
            .replies
            .get_mut(&code)
            .unwrap()
            .current = value;
    }
}
pub fn registry(hardware: Arc<Hardware>) -> CapabilityRegistry {
    let state = hardware.state.clone();
    let client = DdcClient::start_with_backend(move || Ok(FakeBackend::new(state))).unwrap();
    let mut registry = CapabilityRegistry::default();
    registry
        .register(Arc::new(DdcCapabilityFactory::new(client)))
        .unwrap();
    registry
}

#[derive(Clone)]
pub struct Screen {
    pub key: String,
    pub identity: Option<String>,
    pub name: Option<String>,
    pub values: BTreeMap<u8, VcpValue>,
}
impl Screen {
    pub fn benq() -> Self {
        let values = [
            (16, (40, 100)),
            (25, (2, 5)),
            (215, (0x0120, 0x0231)),
            (217, (0x0305, 0x070a)),
            (32, (125, 1000)),
            (33, (25, 1000)),
            (34, (0, 1)),
            (35, (0, 1)),
            (36, (0, 64)),
            (45, (0, 65535)),
        ]
        .into_iter()
        .chain(
            (37..=69)
                .filter(|code| *code != 45)
                .map(|code| (code, (0, 65535))),
        )
        .map(|(code, (current, maximum))| (code, VcpValue { current, maximum }))
        .collect();
        Self {
            key: "screen-a".into(),
            identity: Some("a".repeat(64)),
            name: Some("BenQ RD280U".into()),
            values,
        }
    }
}
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for item in std::fs::read_dir(source).unwrap() {
        let item = item.unwrap();
        if item.file_type().unwrap().is_dir() {
            copy_tree(&item.path(), &destination.join(item.file_name()));
        } else {
            std::fs::copy(item.path(), destination.join(item.file_name())).unwrap();
        }
    }
}
pub fn catalog(packages: &[&str]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for id in packages {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(id);
        let source = if fixture.is_dir() {
            fixture
        } else {
            root().join("drivers").join(id)
        };
        copy_tree(&source, &temp.path().join(id));
    }
    temp
}
pub struct Harness {
    pub host: PluginHost,
    pub store: StateStore,
    pub hardware: Arc<Hardware>,
}
impl Harness {
    pub fn start(path: &Path, hardware: Arc<Hardware>) -> Self {
        Self::settings(path, hardware, BTreeMap::new())
    }
    pub fn settings(
        path: &Path,
        hardware: Arc<Hardware>,
        settings: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self::with_interval(path, hardware, settings, 1000)
    }
    pub fn with_interval(
        path: &Path,
        hardware: Arc<Hardware>,
        settings: BTreeMap<String, serde_json::Value>,
        discovery_interval_ms: u64,
    ) -> Self {
        let registry = registry(hardware.clone());
        let store = StateStore::new(NodeInfo {
            node_id: "00000000-0000-4000-8000-000000000001".into(),
            instance_id: "00000000-0000-4000-8000-000000000002".into(),
            name: "Test".into(),
        })
        .unwrap();
        let host = PluginHost::start(
            store.clone(),
            PluginConfig {
                directory: path.into(),
                directory_explicit: true,
                disabled: vec![],
                discovery_interval_ms,
                settings,
            },
            registry,
        )
        .unwrap();
        Self {
            host,
            store,
            hardware,
        }
    }
    pub async fn until(&self, condition: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let mut changed = self.store.subscribe();
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let snapshot = changed.borrow_and_update().clone();
                if condition(&snapshot) {
                    return snapshot;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .expect("observable host transition")
    }
    pub async fn stop(&self) {
        self.hardware.get_barrier.open();
        self.hardware.set_barrier.open();
        self.host
            .shutdown(Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
    }
}
pub fn value<'a>(snapshot: &'a Snapshot, id: &str) -> &'a CharacteristicValue {
    snapshot.devices[0]
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == id)
        .unwrap()
        .value
        .as_ref()
        .unwrap()
}

#[allow(dead_code)]
pub fn transform_discovery(entry: &Path, transform: &str) {
    let source = std::fs::read_to_string(entry).unwrap();
    let wrapped = format!(
        r#"local driver = (function()
{source}
end)()
return {{
  configure = function(settings)
    local instance = driver.configure(settings)
    local discover = instance.discover
    instance.discover = function(ctx)
      local result = discover(ctx)
      for _, device in ipairs(result.devices or {{}}) do
        {transform}
      end
      return result
    end
    return instance
  end
}}
"#
    );
    std::fs::write(entry, wrapped).unwrap();
}
