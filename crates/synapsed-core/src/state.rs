use crate::{DriverError, ErrorCode};
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock, Mutex},
    time::Instant,
};
use synapsed_api::{
    API_VERSION, CharacteristicStatus, Device, DeviceStatus, DriverState, DriverStatus,
    MAX_SNAPSHOT_BYTES, NodeInfo, Snapshot, StatusResponse,
};
use tokio::sync::watch;

#[derive(Clone)]
/// Validated, immutable snapshots and driver reports shared with subscribers.
pub struct StateStore {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<State>,
    changed: watch::Sender<Arc<PublishedSnapshot>>,
    started: Instant,
}
struct State {
    reports: BTreeMap<String, (DriverStatus, Vec<Device>)>,
    snapshot: Arc<Snapshot>,
    encoded: Arc<[u8]>,
    revision: u64,
}

/// A validated snapshot and its cached encoding from the same atomic publication.
#[derive(Debug, Clone)]
pub struct PublishedSnapshot {
    snapshot: Arc<Snapshot>,
    encoded: Arc<[u8]>,
}
impl PublishedSnapshot {
    /// Return the immutable public snapshot from this publication.
    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }
    /// Return the compact JSON representation of this publication.
    pub fn encoded(&self) -> &Arc<[u8]> {
        &self.encoded
    }
}
impl StateStore {
    /// Fixed fault text keeps retained observations publishable at the wire-size limit.
    pub const FAULT_MESSAGE: &'static str = "Driver unavailable.";
    /// Validate without copying values; reserve space for the longest revision and compact fault errors.
    pub fn validate_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>, DriverError> {
        static FAULT_ERROR: LazyLock<DriverError> = LazyLock::new(|| {
            DriverError::new(ErrorCode::UnsupportedCapability, StateStore::FAULT_MESSAGE)
        });
        let encoded = snapshot.validated_json()?;
        let error_size = json_size(&*FAULT_ERROR)?;
        let driver_state_size = json_size(&DriverState::PermissionDenied)?;
        let device_state_size = json_size(&DeviceStatus::Unavailable)?;
        let characteristic_state_size = json_size(&CharacteristicStatus::Unavailable)?;
        let mut projected = encoded.len() as i64 + 20 - snapshot.revision.len() as i64;
        for driver in &snapshot.drivers {
            projected += driver_state_size - json_size(&driver.state)?
                + (error_size - json_size(&driver.error)?).max(0);
        }
        for device in &snapshot.devices {
            projected += device_state_size - json_size(&device.status)?
                + (error_size - json_size(&device.error)?).max(0);
            for characteristic in &device.characteristics {
                projected += characteristic_state_size - json_size(&characteristic.status)?
                    + (error_size - json_size(&characteristic.error)?).max(0);
            }
        }
        if projected > MAX_SNAPSHOT_BYTES as i64 {
            return Err(DriverError::new(
                ErrorCode::InvalidValue,
                "Snapshot lacks space to retain observations after a fault.",
            ));
        }
        Ok(encoded)
    }
    /// Create a state store initialized with the supplied node identity.
    pub fn new(node: NodeInfo) -> Result<Self, DriverError> {
        let snapshot = Snapshot {
            api_version: API_VERSION,
            node,
            revision: "0".into(),
            drivers: vec![],
            devices: vec![],
        };
        let encoded: Arc<[u8]> = Arc::from(Self::validate_snapshot(&snapshot)?);
        let snapshot = Arc::new(snapshot);
        let publication = Arc::new(PublishedSnapshot {
            snapshot: snapshot.clone(),
            encoded: encoded.clone(),
        });
        let (changed, _) = watch::channel(publication);
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    reports: BTreeMap::new(),
                    snapshot,
                    encoded,
                    revision: 0,
                }),
                changed,
                started: Instant::now(),
            }),
        })
    }
    /// Return the current immutable published snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.inner
            .state
            .lock()
            .expect("state mutex poisoned")
            .snapshot
            .clone()
    }
    /// Return the compact JSON encoding of the current published snapshot.
    pub fn encoded_snapshot(&self) -> Arc<[u8]> {
        self.inner
            .state
            .lock()
            .expect("state mutex poisoned")
            .encoded
            .clone()
    }
    /// Subscribe to atomic snapshot publications and their cached encodings.
    pub fn subscribe(&self) -> watch::Receiver<Arc<PublishedSnapshot>> {
        self.inner.changed.subscribe()
    }
    /// Return a status response with the current revision, drivers, and uptime.
    pub fn status(&self) -> StatusResponse {
        let snapshot = self.snapshot();
        StatusResponse {
            api_version: API_VERSION,
            node: snapshot.node.clone(),
            revision: snapshot.revision.clone(),
            uptime_seconds: u32::try_from(self.inner.started.elapsed().as_secs())
                .unwrap_or(u32::MAX),
            drivers: snapshot.drivers.clone(),
        }
    }
    /// Replace one driver's report and publish the resulting snapshot atomically.
    pub fn publish_driver_state(
        &self,
        id: &str,
        status: DriverStatus,
        devices: Vec<Device>,
    ) -> Result<Arc<Snapshot>, DriverError> {
        if id != status.id || devices.iter().any(|device| device.driver != id) {
            return Err(DriverError::new(
                ErrorCode::PluginError,
                "Driver report has invalid ownership.",
            ));
        }
        self.publish_driver_states(vec![(status, devices)])
    }
    /// Apply related ownership changes together; a failed prospective snapshot is never visible.
    pub fn publish_driver_states(
        &self,
        reports: Vec<(DriverStatus, Vec<Device>)>,
    ) -> Result<Arc<Snapshot>, DriverError> {
        let mut state = self.inner.state.lock().expect("state mutex poisoned");
        let mut replacements = BTreeMap::new();
        for (status, devices) in reports {
            if devices.iter().any(|device| device.driver != status.id) {
                return Err(DriverError::new(
                    ErrorCode::PluginError,
                    "Driver report has invalid ownership.",
                ));
            }
            if replacements
                .insert(status.id.clone(), (status, devices))
                .is_some()
            {
                return Err(DriverError::new(
                    ErrorCode::PluginError,
                    "Driver report IDs must be unique.",
                ));
            }
        }
        if replacements
            .iter()
            .all(|(id, report)| state.reports.get(id) == Some(report))
        {
            return Ok(state.snapshot.clone());
        }
        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| DriverError::new(ErrorCode::PluginError, "State revision exhausted."))?;
        let snapshot = {
            // Overlay borrowed reports; only the new immutable public snapshot
            // owns a deep copy of unchanged devices, not a second prospective store.
            let mut prospective: BTreeMap<_, _> = state
                .reports
                .iter()
                .map(|(id, report)| (id.as_str(), report))
                .collect();
            for (id, report) in &replacements {
                prospective.insert(id.as_str(), report);
            }
            Snapshot {
                api_version: API_VERSION,
                node: state.snapshot.node.clone(),
                revision: revision.to_string(),
                drivers: prospective
                    .values()
                    .map(|(driver, _)| driver.clone())
                    .collect(),
                devices: prospective
                    .values()
                    .flat_map(|(_, devices)| devices.iter().cloned())
                    .collect(),
            }
        };
        let encoded: Arc<[u8]> = Arc::from(Self::validate_snapshot(&snapshot)?);
        let snapshot = Arc::new(snapshot);
        let publication = Arc::new(PublishedSnapshot {
            snapshot: snapshot.clone(),
            encoded: encoded.clone(),
        });
        state.reports.extend(replacements);
        state.revision = revision;
        state.encoded = encoded;
        state.snapshot = snapshot.clone();
        self.inner.changed.send_replace(publication);
        Ok(snapshot)
    }
}

/// Count small status/error replacements; their JSON escaping must match the wire.
fn json_size(value: &impl serde::Serialize) -> Result<i64, DriverError> {
    struct Counter(i64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as i64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| {
        DriverError::new(ErrorCode::InvalidValue, "Snapshot error cannot be encoded.")
    })?;
    Ok(counter.0)
}
