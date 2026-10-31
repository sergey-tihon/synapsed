use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use synapsed_api::{CharacteristicValue, NodeInfo, Snapshot};
use synapsed_core::{DriverError, ErrorCode, StateStore};
use synapsed_ddc::{
    DdcBackend, DdcClient, DdcTransport, InventoryWithOwnedHandles, OwnedEndpoint, VcpValue,
};
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost, capabilities::ddc::DdcCapabilityFactory,
};

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
pub struct Barrier {
    armed: AtomicBool,
    pub entered: tokio::sync::Notify,
    released: Mutex<bool>,
    release: Condvar,
}
pub struct BarrierRelease<'a>(&'a Barrier);
impl Drop for BarrierRelease<'_> {
    fn drop(&mut self) {
        self.0.open();
    }
}
impl Barrier {
    fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            released: Mutex::new(false),
            release: Condvar::new(),
        }
    }
    pub fn arm(&self) {
        *self.released.lock().unwrap() = false;
        self.armed.store(true, Ordering::SeqCst);
    }
    pub fn release_on_drop(&self) -> BarrierRelease<'_> {
        BarrierRelease(self)
    }
    fn block(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.release.wait(released).unwrap();
            }
        }
    }
    pub fn open(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }
}
#[derive(Default)]
pub struct State {
    pub screens: Vec<Screen>,
    pub scan_errors: Vec<DriverError>,
    pub reads: Vec<u8>,
    pub writes: Vec<(u8, u16)>,
    pub fail_reads: BTreeSet<u8>,
    pub coercion: BTreeMap<u8, u16>,
    pub fail_set_number: Option<usize>,
}
pub struct Hardware {
    pub state: Mutex<State>,
    pub get_barrier: Barrier,
    pub set_barrier: Barrier,
}
impl Hardware {
    pub fn new(screens: Vec<Screen>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                screens,
                ..State::default()
            }),
            get_barrier: Barrier::new(),
            set_barrier: Barrier::new(),
        })
    }
    pub fn current(&self, code: u8) -> u16 {
        self.state.lock().unwrap().screens[0].values[&code].current
    }
    pub fn mutate(&self, code: u8, value: u16) {
        self.state.lock().unwrap().screens[0]
            .values
            .get_mut(&code)
            .unwrap()
            .current = value;
    }
}
struct Backend(Arc<Hardware>);
struct Transport {
    hardware: Arc<Hardware>,
    key: String,
}
impl DdcBackend for Backend {
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, DriverError> {
        let state = self.0.state.lock().unwrap();
        Ok(InventoryWithOwnedHandles {
            endpoints: state
                .screens
                .iter()
                .map(|screen| OwnedEndpoint {
                    key: screen.key.clone(),
                    manufacturer: Some("BNQ".into()),
                    product: Some(1),
                    product_name: screen.name.clone(),
                    identity: screen.identity.clone(),
                    transport: Box::new(Transport {
                        hardware: self.0.clone(),
                        key: screen.key.clone(),
                    }),
                })
                .collect(),
            errors: state.scan_errors.clone(),
        })
    }
}
impl DdcTransport for Transport {
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, DriverError> {
        self.hardware.get_barrier.block();
        let mut state = self.hardware.state.lock().unwrap();
        state.reads.push(code);
        if state.fail_reads.contains(&code) {
            return Err(DriverError::new(
                ErrorCode::DdcError,
                "Fixture register read failed.",
            ));
        }
        state
            .screens
            .iter()
            .find(|screen| screen.key == self.key)
            .and_then(|screen| screen.values.get(&code))
            .copied()
            .ok_or_else(|| DriverError::new(ErrorCode::DdcError, "Fixture register absent."))
    }
    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DriverError> {
        self.hardware.set_barrier.block();
        let mut state = self.hardware.state.lock().unwrap();
        state.writes.push((code, value));
        if state.fail_set_number == Some(state.writes.len()) {
            return Err(DriverError::new(ErrorCode::DdcError, "Fixture Set failed."));
        }
        let observed = state.coercion.get(&code).copied().unwrap_or(value);
        state
            .screens
            .iter_mut()
            .find(|screen| screen.key == self.key)
            .and_then(|screen| screen.values.get_mut(&code))
            .ok_or_else(|| DriverError::new(ErrorCode::DdcError, "Fixture register absent."))?
            .current = observed;
        Ok(())
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
pub fn registry(hardware: Arc<Hardware>) -> CapabilityRegistry {
    let client = DdcClient::start_with_backend(move || Ok(Backend(hardware))).unwrap();
    let mut registry = CapabilityRegistry::default();
    registry
        .register(Arc::new(DdcCapabilityFactory::new(client)))
        .unwrap();
    registry
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
