use crate::{DriverError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Identifier and generation assigned to a native endpoint.
pub struct EndpointToken {
    /// Stable numeric identifier for the endpoint within the process.
    pub id: u64,
    /// Generation distinguishing the endpoint's current presence and identity.
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// Lifecycle state of an authorized native operation.
pub enum OperationState {
    /// The operation is waiting to begin.
    Queued,
    /// The operation is executing before any write has been admitted.
    Running,
    /// An irreversible native write has been admitted.
    WriteStarted,
    /// All native work for the operation has completed.
    Completed,
    /// The operation was cancelled before completion.
    Cancelled,
}

impl OperationState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Queued,
            1 => Self::Running,
            2 => Self::WriteStarted,
            3 => Self::Completed,
            4 => Self::Cancelled,
            _ => unreachable!("operation state is stored from the enum discriminant"),
        }
    }
}

#[derive(Debug, Clone)]
/// Result evidence recorded for a native transport write.
pub struct TransportOutcome {
    /// Endpoint on which the operation was attempted.
    pub endpoint: EndpointToken,
    /// VCP register code that was written.
    pub code: u8,
    /// Value requested by the caller.
    pub requested: u16,
    /// Observed current and maximum values after the write, when available.
    pub observed: Option<(u16, u16)>,
    /// Error from the write, readback, or authorization check, if any.
    pub error: Option<DriverError>,
}

#[derive(Clone, Default)]
/// Provides stable per-endpoint transaction gates for device ownership grants.
pub struct EndpointGates {
    inner: Arc<Mutex<BTreeMap<u64, Arc<AsyncMutex<()>>>>>,
}
#[derive(Clone)]
/// Active ownership grant restricting native operations to one device's endpoints.
pub struct DeviceGrant {
    owner: Arc<str>,
    device: Arc<str>,
    epoch: u64,
    endpoints: Arc<[EndpointToken]>,
    active: Arc<AtomicBool>,
    gates: EndpointGates,
}
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
impl EndpointGates {
    /// Create an active grant for one device and its uniquely identified endpoints.
    pub fn activate(
        &self,
        owner: impl Into<Arc<str>>,
        device: impl Into<Arc<str>>,
        epoch: u64,
        mut endpoints: Vec<EndpointToken>,
    ) -> Result<DeviceGrant, DriverError> {
        endpoints.sort_unstable();
        if endpoints.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(DriverError::new(
                ErrorCode::PluginError,
                "Device endpoints must be unique.",
            ));
        }
        Ok(DeviceGrant {
            owner: owner.into(),
            device: device.into(),
            epoch,
            endpoints: endpoints.into(),
            active: Arc::new(AtomicBool::new(true)),
            gates: self.clone(),
        })
    }
}
impl DeviceGrant {
    /// Return the driver identifier that owns this grant.
    pub fn owner(&self) -> &str {
        &self.owner
    }
    /// Return the device identifier this grant protects.
    pub fn device(&self) -> &str {
        &self.device
    }
    /// Return the ownership epoch associated with this grant.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Revoke this grant so no new operations can be admitted.
    pub fn revoke(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
    /// Return whether this grant still permits new operations.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    /// Return the endpoints covered by this grant.
    pub fn endpoints(&self) -> &[EndpointToken] {
        &self.endpoints
    }
    /// Begin an operation holding this grant's endpoint gates until native work drains.
    pub async fn begin(
        &self,
        reads: BTreeSet<u8>,
        writes: BTreeMap<u8, u16>,
        deadline: Instant,
    ) -> Result<OperationContext, DriverError> {
        if !self.is_active() {
            return Err(unavailable());
        }
        let guards = self.acquire(deadline).await?;
        if !self.is_active() {
            return Err(unavailable());
        }
        Ok(OperationContext::new(
            self.owner.clone(),
            self.device.clone(),
            self.epoch,
            self.endpoints.clone(),
            self.active.clone(),
            reads,
            writes,
            deadline,
            guards,
        ))
    }
    /// Revoke first. The stable gate barrier includes native work surviving callback abandonment.
    pub async fn drain(&self, deadline: Instant) -> Result<(), DriverError> {
        let guards = self.acquire(deadline).await?;
        drop(guards);
        Ok(())
    }
    /// Stop hooks observe the retired target without reviving any old writer.
    pub async fn begin_retirement(
        &self,
        reads: BTreeSet<u8>,
        deadline: Instant,
    ) -> Result<OperationContext, DriverError> {
        if self.is_active() {
            return Err(DriverError::new(
                ErrorCode::PermissionDenied,
                "Retirement requires revoked ownership.",
            ));
        }
        let guards = self.acquire(deadline).await?;
        Ok(OperationContext::new(
            self.owner.clone(),
            self.device.clone(),
            self.epoch,
            self.endpoints.clone(),
            Arc::new(AtomicBool::new(true)),
            reads,
            BTreeMap::new(),
            deadline,
            guards,
        )
        .read_only())
    }
    async fn acquire(&self, deadline: Instant) -> Result<Vec<OwnedMutexGuard<()>>, DriverError> {
        let gates: Vec<_> = {
            let mut all = self.gates.inner.lock().expect("endpoint gates poisoned");
            self.endpoints
                .iter()
                .map(|endpoint| {
                    all.entry(endpoint.id)
                        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                        .clone()
                })
                .collect()
        };
        let mut guards = Vec::with_capacity(gates.len());
        for gate in gates {
            if Instant::now() >= deadline {
                return Err(timeout());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let guard = tokio::time::timeout(remaining, gate.lock_owned())
                .await
                .map_err(|_| {
                    DriverError::new(ErrorCode::Timeout, "Endpoint transaction timed out.")
                })?;
            guards.push(guard);
        }
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        Ok(guards)
    }
}

#[derive(Clone)]
/// Authorization and lifecycle context shared by callbacks and native DDC work.
pub struct OperationContext {
    inner: Arc<Operation>,
    write_allowed: bool,
}
struct Operation {
    owner: Arc<str>,
    device: Arc<str>,
    epoch: u64,
    sequence: u64,
    endpoints: Arc<[EndpointToken]>,
    active: Arc<AtomicBool>,
    reads: BTreeSet<u8>,
    writes: BTreeMap<u8, u16>,
    deadline: Instant,
    state: AtomicU8,
    abandoned: AtomicBool,
    lease: Mutex<Lease>,
    outcomes: Mutex<Vec<TransportOutcome>>,
}
struct Lease {
    guards: Vec<OwnedMutexGuard<()>>,
    native: usize,
    finished: bool,
}
/// Lease representing native work that retains an operation's endpoint gates.
pub struct NativeWork {
    context: OperationContext,
}
impl OperationContext {
    #[allow(clippy::too_many_arguments)]
    fn new(
        owner: Arc<str>,
        device: Arc<str>,
        epoch: u64,
        endpoints: Arc<[EndpointToken]>,
        active: Arc<AtomicBool>,
        reads: BTreeSet<u8>,
        writes: BTreeMap<u8, u16>,
        deadline: Instant,
        guards: Vec<OwnedMutexGuard<()>>,
    ) -> Self {
        Self {
            inner: Arc::new(Operation {
                owner,
                device,
                epoch,
                sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed),
                endpoints,
                active,
                reads,
                writes,
                deadline,
                state: AtomicU8::new(OperationState::Queued as u8),
                abandoned: AtomicBool::new(false),
                lease: Mutex::new(Lease {
                    guards,
                    native: 0,
                    finished: false,
                }),
                outcomes: Mutex::new(Vec::new()),
            }),
            write_allowed: true,
        }
    }
    /// Create a read-only discovery scope without a claimed device or write permission.
    pub fn discovery(
        owner: impl Into<Arc<str>>,
        endpoints: Vec<EndpointToken>,
        reads: BTreeSet<u8>,
        deadline: Instant,
    ) -> Self {
        Self::new(
            owner.into(),
            Arc::from(""),
            0,
            endpoints.into(),
            Arc::new(AtomicBool::new(true)),
            reads,
            BTreeMap::new(),
            deadline,
            vec![],
        )
    }
    /// Share the transaction, evidence, and gates through a view that cannot authorize writes.
    pub fn read_only(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            write_allowed: false,
        }
    }
    /// Return the driver identifier that owns this operation.
    pub fn owner(&self) -> &str {
        &self.inner.owner
    }
    /// Return the device identifier protected by this operation.
    pub fn device(&self) -> &str {
        &self.inner.device
    }
    /// Return the ownership epoch associated with this operation.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }
    /// Return the unique sequence number assigned to this operation.
    pub fn sequence(&self) -> u64 {
        self.inner.sequence
    }
    /// Return the absolute deadline for this operation.
    pub fn deadline(&self) -> Instant {
        self.inner.deadline
    }
    /// Return the current lifecycle state of this operation.
    pub fn state(&self) -> OperationState {
        OperationState::from_u8(self.inner.state.load(Ordering::SeqCst))
    }
    /// Check endpoint, register, deadline, and read/write authorization.
    pub fn validate(
        &self,
        endpoint: EndpointToken,
        code: u8,
        write: bool,
    ) -> Result<(), DriverError> {
        if !self.inner.active.load(Ordering::SeqCst) || !self.inner.endpoints.contains(&endpoint) {
            return Err(unavailable());
        }
        if Instant::now() >= self.inner.deadline {
            self.cancel();
            return Err(timeout());
        }
        if self.inner.abandoned.load(Ordering::SeqCst)
            || matches!(
                self.state(),
                OperationState::Cancelled | OperationState::Completed
            )
        {
            return Err(timeout());
        }
        if !self.inner.reads.contains(&code)
            || (write && (!self.write_allowed || !self.inner.writes.contains_key(&code)))
        {
            return Err(DriverError::new(
                ErrorCode::PermissionDenied,
                "Native register operation is not granted.",
            ));
        }
        Ok(())
    }
    /// Return the granted write-change mask for a register, if writes are allowed.
    pub fn change_mask(&self, code: u8) -> Option<u16> {
        if self.write_allowed {
            self.inner.writes.get(&code).copied()
        } else {
            None
        }
    }
    /// Start authorized native work and retain its endpoint gates until it finishes.
    pub fn begin_native(
        &self,
        endpoint: EndpointToken,
        code: u8,
        write: bool,
    ) -> Result<NativeWork, DriverError> {
        let mut lease = self.inner.lease.lock().expect("operation lease poisoned");
        self.validate(endpoint, code, write)?;
        if lease.finished {
            return Err(unavailable());
        }
        self.inner
            .state
            .compare_exchange(
                OperationState::Queued as u8,
                OperationState::Running as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .ok();
        if self.state() == OperationState::Cancelled {
            return Err(timeout());
        }
        lease.native += 1;
        Ok(NativeWork {
            context: self.clone(),
        })
    }
    /// Competes atomically with abandonment before the first irreversible Set.
    pub fn admit_write(&self, endpoint: EndpointToken, code: u8) -> Result<(), DriverError> {
        self.validate(endpoint, code, true)?;
        loop {
            let state = self.inner.state.load(Ordering::SeqCst);
            match state {
                0 | 1 => {
                    if self
                        .inner
                        .state
                        .compare_exchange(
                            state,
                            OperationState::WriteStarted as u8,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                2 => {
                    return if self.inner.abandoned.load(Ordering::SeqCst) {
                        Err(timeout())
                    } else {
                        Ok(())
                    };
                }
                _ => return Err(timeout()),
            }
        }
    }
    /// Mark callback abandonment; admission may be cancelled, but admitted native writes complete.
    pub fn abandon(&self) -> bool {
        loop {
            let state = self.inner.state.load(Ordering::SeqCst);
            if state > OperationState::Running as u8 {
                return state == OperationState::Cancelled as u8;
            }
            if self
                .inner
                .state
                .compare_exchange(
                    state,
                    OperationState::Cancelled as u8,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                self.inner.abandoned.store(true, Ordering::SeqCst);
                return true;
            }
        }
    }
    /// Cancel further work after a deadline or host revocation, including after a write starts.
    pub fn cancel(&self) {
        self.inner.abandoned.store(true, Ordering::SeqCst);
        loop {
            let state = self.inner.state.load(Ordering::SeqCst);
            if state > OperationState::Running as u8 {
                break;
            }
            if self
                .inner
                .state
                .compare_exchange(
                    state,
                    OperationState::Cancelled as u8,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                break;
            }
        }
    }
    /// Mark the callback finished; native work retains gates until its completion.
    pub fn finish(&self) {
        let mut lease = self.inner.lease.lock().expect("operation lease poisoned");
        lease.finished = true;
        if lease.native == 0 {
            if self.state() != OperationState::Cancelled {
                self.inner
                    .state
                    .store(OperationState::Completed as u8, Ordering::SeqCst);
            }
            lease.guards.clear();
        }
    }
    /// Record the observed result of a native transport write.
    pub fn record_outcome(&self, outcome: TransportOutcome) {
        self.inner
            .outcomes
            .lock()
            .expect("operation outcomes poisoned")
            .push(outcome);
    }
    /// Return all transport outcomes recorded by this operation.
    pub fn outcomes(&self) -> Vec<TransportOutcome> {
        self.inner
            .outcomes
            .lock()
            .expect("operation outcomes poisoned")
            .clone()
    }
}
impl Drop for NativeWork {
    fn drop(&mut self) {
        let mut lease = self
            .context
            .inner
            .lease
            .lock()
            .expect("operation lease poisoned");
        lease.native -= 1;
        if lease.native == 0 && lease.finished {
            if self.context.state() != OperationState::Cancelled {
                self.context
                    .inner
                    .state
                    .store(OperationState::Completed as u8, Ordering::SeqCst);
            }
            lease.guards.clear();
        }
    }
}
fn unavailable() -> DriverError {
    DriverError::new(
        ErrorCode::DeviceUnavailable,
        "Endpoint ownership is no longer available.",
    )
}
fn timeout() -> DriverError {
    DriverError::new(
        ErrorCode::Timeout,
        "Operation is cancelled or past its deadline.",
    )
}
