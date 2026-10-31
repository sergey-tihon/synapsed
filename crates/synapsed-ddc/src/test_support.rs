//! Configurable in-memory DDC backend for hardware-free tests.
use std::{
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, ThreadId},
};
use tokio::sync::Notify;

use crate::{
    DdcBackend, DdcTransport, DriverError, ErrorCode, InventoryWithOwnedHandles, OwnedEndpoint,
    VcpValue,
};

/// Shared state used to configure and inspect a [`FakeBackend`].
pub type FakeStateHandle = Arc<Mutex<FakeState>>;

/// One endpoint returned by a fake scan, including its per-VCP replies.
#[derive(Clone)]
pub struct FakeEndpoint {
    /// Opaque owner-local endpoint key used to associate transports with replies.
    pub key: String,
    /// Manufacturer code shown in public inventory.
    pub manufacturer: Option<String>,
    /// Product code shown in public inventory.
    pub product: Option<u16>,
    /// Display name shown in public inventory.
    pub product_name: Option<String>,
    /// Stable endpoint identity used by owner generation tracking.
    pub identity: Option<String>,
    /// VCP replies available from this endpoint.
    pub replies: BTreeMap<u8, VcpValue>,
}
impl FakeEndpoint {
    /// Creates an endpoint with the supplied owner-local key and no replies.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            manufacturer: None,
            product: None,
            product_name: None,
            identity: None,
            replies: BTreeMap::new(),
        }
    }
}

/// Configurable test state and call history shared with a [`FakeBackend`].
#[derive(Default)]
pub struct FakeState {
    /// Endpoints returned by successful scans.
    pub endpoints: Vec<FakeEndpoint>,
    /// Non-fatal adapter errors returned with the endpoint inventory.
    pub scan_errors: Vec<DriverError>,
    /// When set, every scan fails with this error.
    pub scan_failure: Option<DriverError>,
    /// Failures keyed by `(endpoint key, VCP code)`. The empty key matches any endpoint.
    pub get_failures: BTreeMap<(String, u8), DriverError>,
    /// Failures keyed by `(endpoint key, VCP code)`. The empty key matches any endpoint.
    pub set_failures: BTreeMap<(String, u8), DriverError>,
    /// Global VCP-code Get failures, in addition to endpoint-specific `get_failures`.
    pub fail_reads: BTreeSet<u8>,
    /// Global VCP-code Set failures, in addition to endpoint-specific `set_failures`.
    pub fail_writes: BTreeSet<u8>,
    /// Fail the Set with this one-based number in the ordered Set call log.
    pub fail_set_number: Option<usize>,
    /// Coerced readback values keyed by VCP code.
    pub coercion: BTreeMap<u8, u16>,
    /// Number of scans made by the backend.
    pub scan_calls: usize,
    /// Ordered `(endpoint key, VCP code)` Get calls.
    pub get_calls: Vec<(String, u8)>,
    /// Ordered `(endpoint key, VCP code, requested value)` Set calls.
    pub set_calls: Vec<(String, u8, u16)>,
    /// Barrier entered before the next armed Get.
    pub get_barrier: FakeBarrier,
    /// Barrier entered before the next armed Set.
    pub set_barrier: FakeBarrier,
    /// Thread on which the backend instance was created.
    pub backend_created_on: Option<ThreadId>,
    /// Thread on which the backend instance was dropped.
    pub backend_dropped_on: Option<ThreadId>,
}
impl FakeState {
    /// Fails reads of a VCP code on every endpoint.
    pub fn fail_get_code(&mut self, code: u8, error: DriverError) {
        self.get_failures.insert((String::new(), code), error);
    }

    /// Fails writes of a VCP code on every endpoint.
    pub fn fail_set_code(&mut self, code: u8, error: DriverError) {
        self.set_failures.insert((String::new(), code), error);
    }

    /// Returns the number of Get calls recorded so far.
    pub fn get_call_count(&self) -> usize {
        self.get_calls.len()
    }

    /// Returns the number of Set calls recorded so far.
    pub fn set_call_count(&self) -> usize {
        self.set_calls.len()
    }
}

/// A one-shot, blocking transport barrier that can be observed asynchronously by a test.
#[derive(Clone, Default)]
pub struct FakeBarrier {
    inner: Arc<FakeBarrierInner>,
}
struct FakeBarrierInner {
    armed: AtomicBool,
    entered: Notify,
    released: Mutex<bool>,
    release: Condvar,
}
impl Default for FakeBarrierInner {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            entered: Notify::new(),
            released: Mutex::new(false),
            release: Condvar::new(),
        }
    }
}
impl FakeBarrier {
    /// Arms the barrier for the next transport call.
    pub fn arm(&self) {
        *self.inner.released.lock().unwrap() = false;
        self.inner.armed.store(true, Ordering::SeqCst);
    }

    /// Waits until a call has reached the armed barrier.
    pub async fn wait_until_entered(&self) {
        self.inner.entered.notified().await;
    }

    /// Releases the current call and leaves the barrier open for cleanup.
    pub fn open(&self) {
        *self.inner.released.lock().unwrap() = true;
        self.inner.release.notify_all();
    }

    fn block(&self) {
        if self.inner.armed.swap(false, Ordering::SeqCst) {
            self.inner.entered.notify_one();
            let mut released = self.inner.released.lock().unwrap();
            while !*released {
                released = self.inner.release.wait(released).unwrap();
            }
        }
    }
}

/// A configurable in-memory backend with fake inventory, replies, counters, failures, and barriers.
pub struct FakeBackend {
    state: FakeStateHandle,
    _not_send: PhantomData<Rc<()>>,
}
impl FakeBackend {
    /// Creates a backend over shared test state. Call from the DDC owner-thread factory.
    pub fn new(state: FakeStateHandle) -> Self {
        state.lock().unwrap().backend_created_on = Some(thread::current().id());
        Self {
            state,
            _not_send: PhantomData,
        }
    }
}
impl DdcBackend for FakeBackend {
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, DriverError> {
        let mut state = self.state.lock().unwrap();
        state.scan_calls += 1;
        if let Some(error) = &state.scan_failure {
            return Err(error.clone());
        }
        let endpoints = state
            .endpoints
            .iter()
            .map(|endpoint| OwnedEndpoint {
                key: endpoint.key.clone(),
                manufacturer: endpoint.manufacturer.clone(),
                product: endpoint.product,
                product_name: endpoint.product_name.clone(),
                identity: endpoint.identity.clone(),
                placement: None,
                transport: Box::new(FakeTransport {
                    state: self.state.clone(),
                    key: endpoint.key.clone(),
                    _not_send: PhantomData,
                }),
            })
            .collect();
        Ok(InventoryWithOwnedHandles {
            endpoints,
            errors: state.scan_errors.clone(),
        })
    }
}
impl Drop for FakeBackend {
    fn drop(&mut self) {
        self.state.lock().unwrap().backend_dropped_on = Some(thread::current().id());
    }
}

struct FakeTransport {
    state: FakeStateHandle,
    key: String,
    _not_send: PhantomData<Rc<()>>,
}
impl DdcTransport for FakeTransport {
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, DriverError> {
        let barrier = self.state.lock().unwrap().get_barrier.clone();
        barrier.block();
        let mut state = self.state.lock().unwrap();
        state.get_calls.push((self.key.clone(), code));
        if state.fail_reads.contains(&code) {
            return Err(DriverError::new(
                ErrorCode::DdcError,
                "Fake VCP read failed.",
            ));
        }
        if let Some(error) = state
            .get_failures
            .get(&(self.key.clone(), code))
            .or_else(|| state.get_failures.get(&(String::new(), code)))
        {
            return Err(error.clone());
        }
        state
            .endpoints
            .iter()
            .find(|endpoint| endpoint.key == self.key)
            .and_then(|endpoint| endpoint.replies.get(&code))
            .copied()
            .ok_or_else(|| DriverError::new(ErrorCode::DdcError, "Fake VCP reply is unavailable."))
    }

    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DriverError> {
        let barrier = self.state.lock().unwrap().set_barrier.clone();
        barrier.block();
        let mut state = self.state.lock().unwrap();
        state.set_calls.push((self.key.clone(), code, value));
        if state.fail_writes.contains(&code) {
            return Err(DriverError::new(
                ErrorCode::DdcError,
                "Fake VCP Set failed.",
            ));
        }
        if state.fail_set_number == Some(state.set_calls.len()) {
            return Err(DriverError::new(
                ErrorCode::DdcError,
                "Fake VCP Set failed.",
            ));
        }
        if let Some(error) = state
            .set_failures
            .get(&(self.key.clone(), code))
            .or_else(|| state.set_failures.get(&(String::new(), code)))
        {
            return Err(error.clone());
        }
        let observed = state.coercion.get(&code).copied().unwrap_or(value);
        state
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.key == self.key)
            .and_then(|endpoint| endpoint.replies.get_mut(&code))
            .ok_or_else(|| DriverError::new(ErrorCode::DdcError, "Fake VCP reply is unavailable."))?
            .current = observed;
        Ok(())
    }
}
