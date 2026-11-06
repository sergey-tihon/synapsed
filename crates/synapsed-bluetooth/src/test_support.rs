//! In-memory Bluetooth backend for deterministic owner tests.

use crate::{BackendEvent, BackendFuture, BluetoothBackend, Uuid};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use synapsed_core::{DriverError, ErrorCode};
use tokio::sync::mpsc;

/// Shared fake state inspected and controlled by tests.
pub type FakeStateHandle = Arc<Mutex<FakeState>>;

/// Advertisement and characteristic values for one fake peripheral.
pub struct FakePeripheral {
    /// Owner-local key assigned to the peripheral.
    pub key: String,
    /// Advertised local name.
    pub name: Option<String>,
    /// Advertised service UUIDs.
    pub services: Vec<Uuid>,
    /// Characteristic values returned by reads and accepted by subscriptions.
    pub values: HashMap<Uuid, Vec<u8>>,
}

/// One characteristic write recorded by the fake backend.
pub struct FakeWrite {
    /// Peripheral receiving the write.
    pub key: String,
    /// Characteristic receiving the write.
    pub characteristic: Uuid,
    /// Bytes written.
    pub bytes: Vec<u8>,
    /// Monotonic time at which the write was observed.
    pub at: Instant,
}

/// Mutable behavior and observations for [`FakeBackend`].
#[derive(Default)]
pub struct FakeState {
    /// Advertised fake peripherals.
    pub peripherals: Vec<FakePeripheral>,
    /// Characteristic writes recorded in order.
    pub writes: Vec<FakeWrite>,
    /// Number of connect attempts.
    pub connects: usize,
    /// Make connect attempts fail.
    pub fail_connect: bool,
    /// Make characteristic writes fail.
    pub fail_writes: bool,
    /// Event channel installed when scanning starts.
    pub events: Option<mpsc::Sender<BackendEvent>>,
}

/// Fake backend that records writes and allows tests to inject link events.
pub struct FakeBackend {
    state: FakeStateHandle,
}
impl FakeBackend {
    /// Create a fake backend sharing the supplied test state.
    pub fn new(state: FakeStateHandle) -> Self {
        Self { state }
    }
}

impl BluetoothBackend for FakeBackend {
    fn start_scan(&mut self, events: mpsc::Sender<BackendEvent>) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            let peripherals = {
                let mut state = lock(&state);
                state.events = Some(events.clone());
                state
                    .peripherals
                    .iter()
                    .map(|peripheral| {
                        (
                            peripheral.key.clone(),
                            peripheral.name.clone(),
                            peripheral.services.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            for (key, name, services) in peripherals {
                let _ = events.try_send(BackendEvent::Advertisement {
                    key,
                    name,
                    services,
                });
            }
            Ok(())
        })
    }

    fn connect(&mut self, _key: &str) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = lock(&state);
            state.connects += 1;
            if state.fail_connect {
                Err(DriverError::new(
                    ErrorCode::BluetoothError,
                    "Fake Bluetooth connection failed.",
                ))
            } else {
                Ok(())
            }
        })
    }

    fn disconnect(&mut self, _key: &str) -> BackendFuture<()> {
        Box::pin(async { Ok(()) })
    }

    fn read(&mut self, key: &str, characteristic: Uuid) -> BackendFuture<Vec<u8>> {
        let state = self.state.clone();
        let key = key.to_owned();
        Box::pin(async move {
            let state = lock(&state);
            state
                .peripherals
                .iter()
                .find(|peripheral| peripheral.key == key)
                .and_then(|peripheral| peripheral.values.get(&characteristic))
                .cloned()
                .ok_or_else(|| {
                    DriverError::new(
                        ErrorCode::UnsupportedOperation,
                        "Fake characteristic is not available.",
                    )
                })
        })
    }

    fn write(&mut self, key: &str, characteristic: Uuid, bytes: Vec<u8>) -> BackendFuture<()> {
        let state = self.state.clone();
        let key = key.to_owned();
        Box::pin(async move {
            let mut state = lock(&state);
            if state.fail_writes {
                return Err(DriverError::new(
                    ErrorCode::BluetoothError,
                    "Fake Bluetooth write failed.",
                ));
            }
            state.writes.push(FakeWrite {
                key,
                characteristic,
                bytes,
                at: Instant::now(),
            });
            Ok(())
        })
    }

    fn subscribe(&mut self, key: &str, characteristic: Uuid) -> BackendFuture<()> {
        let state = self.state.clone();
        let key = key.to_owned();
        Box::pin(async move {
            let state = lock(&state);
            if state.peripherals.iter().any(|peripheral| {
                peripheral.key == key && peripheral.values.contains_key(&characteristic)
            }) {
                Ok(())
            } else {
                Err(DriverError::new(
                    ErrorCode::UnsupportedOperation,
                    "Fake characteristic is not available.",
                ))
            }
        })
    }
}

/// Update a fake characteristic and forward a notification to the owner.
pub fn notify(state: &FakeStateHandle, key: &str, characteristic: Uuid, bytes: Vec<u8>) {
    let events = {
        let mut state = lock(state);
        if let Some(peripheral) = state
            .peripherals
            .iter_mut()
            .find(|peripheral| peripheral.key == key)
        {
            peripheral.values.insert(characteristic, bytes.clone());
        }
        state.events.clone()
    };
    if let Some(events) = events {
        let _ = events.try_send(BackendEvent::Notification {
            key: key.to_owned(),
            characteristic,
            bytes,
        });
    }
}

/// Simulate a peripheral disconnecting without running session final writes.
pub fn drop_link(state: &FakeStateHandle, key: &str) {
    let events = lock(state).events.clone();
    if let Some(events) = events {
        let _ = events.try_send(BackendEvent::Disconnected {
            key: key.to_owned(),
        });
    }
}

fn lock(state: &FakeStateHandle) -> std::sync::MutexGuard<'_, FakeState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
