use crate::{
    capabilities::ddc::Endpoint,
    loader::{Package, module_source},
    manifest::invalid,
    registry::{CapabilityEventStream, CapabilityRegistry},
};
use mlua::{Function, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value, VmState};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value as Json;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    future::{Future, poll_fn},
    pin::pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use synapsed_api::{
    Characteristic, CharacteristicAccess, CharacteristicCommand, CharacteristicGroup,
    CharacteristicSpec, CharacteristicStatus, CharacteristicValue, Device, DeviceStatus,
};
use synapsed_core::{
    DeviceGrant, DriverError, EndpointToken, ErrorCode, OperationContext, StateStore,
};
use synapsed_ddc::EndpointDescriptor;
use tokio::sync::{mpsc, oneshot};
type CharacteristicWrites = BTreeMap<String, BTreeMap<u8, u16>>;
type ParsedCharacteristics = (Vec<Characteristic>, CharacteristicWrites);

pub(crate) const CALLBACK_LIMIT: Duration = Duration::from_secs(5);
pub(crate) fn exposed_error(error: &DriverError) -> DriverError {
    DriverError::new(error.code, StateStore::FAULT_MESSAGE)
}
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub error: DriverError,
    pub fatal: bool,
}
impl Failure {
    pub(crate) fn fatal(error: DriverError) -> Self {
        Self { error, fatal: true }
    }
    fn operation(error: DriverError) -> Self {
        Self {
            error,
            fatal: false,
        }
    }
}
impl From<mlua::Error> for Failure {
    fn from(error: mlua::Error) -> Self {
        let cause = error
            .chain()
            .take(8)
            .filter_map(|error| error.downcast_ref::<mlua::Error>())
            .last()
            .unwrap_or(&error);
        let kind = match cause {
            mlua::Error::MemoryError(_) => "memory_limit",
            mlua::Error::SyntaxError { .. } => "syntax",
            mlua::Error::FromLuaConversionError { .. }
            | mlua::Error::DeserializeError(_)
            | mlua::Error::SerializeError(_) => "value_conversion",
            mlua::Error::BadArgument { .. } => "argument",
            mlua::Error::RuntimeError(_) => "runtime_exception",
            _ => "callback",
        };
        let code = error.downcast_ref::<DriverError>().map(|error| error.code);
        tracing::error!(target: "synapsed::plugins", public_message = %"Guarded Lua callback failed.", kind, code = ?code);
        Self::fatal(DriverError::new(
            ErrorCode::PluginError,
            "Lua execution or exchanged data failed validation.",
        ))
    }
}
impl From<DriverError> for Failure {
    fn from(_: DriverError) -> Self {
        Self::fatal(DriverError::new(
            ErrorCode::PluginError,
            "Lua declaration or observation is invalid.",
        ))
    }
}

/// A shared latch also observed by the host deadline, so caught Lua errors cannot clear it.
pub struct InvocationGuard {
    violated: AtomicBool,
    live: AtomicBool,
    pub(crate) deadline: Instant,
}
impl InvocationGuard {
    pub(crate) fn new(deadline: Instant) -> Arc<Self> {
        Arc::new(Self {
            violated: AtomicBool::new(false),
            live: AtomicBool::new(true),
            deadline,
        })
    }
    pub fn trip(&self) {
        self.violated.store(true, Ordering::SeqCst);
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn check(&self) -> Result<(), DriverError> {
        if Instant::now() >= self.deadline {
            self.trip();
            return Err(DriverError::new(
                ErrorCode::Timeout,
                "Lua invocation exceeded its deadline.",
            ));
        }
        if self.violated.load(Ordering::SeqCst) || !self.live.load(Ordering::SeqCst) {
            Err(DriverError::new(
                ErrorCode::PluginError,
                "Lua invocation guard rejected an expired or failed callback.",
            ))
        } else {
            Ok(())
        }
    }
    fn close(&self) {
        self.live.store(false, Ordering::SeqCst);
    }
}
#[derive(Clone)]
pub struct InvocationContext {
    pub owner: Arc<str>,
    pub permissions: Json,
    pub operation: Option<OperationContext>,
    pub guard: Arc<InvocationGuard>,
    pub(crate) allowed_endpoints: Vec<EndpointToken>,
    pub(crate) write_allowed: bool,
    pub(crate) discovered: Rc<RefCell<BTreeMap<EndpointToken, EndpointDescriptor>>>,
}
impl InvocationContext {
    pub fn check(&self) -> Result<(), DriverError> {
        self.guard.check()
    }
    /// Register a transport-owned descriptor in this invocation without exposing a native handle.
    pub fn endpoint(
        &self,
        lua: &Lua,
        descriptor: EndpointDescriptor,
    ) -> mlua::Result<mlua::AnyUserData> {
        self.check().map_err(mlua::Error::external)?;
        synapsed_api::validate_driver_id(&descriptor.capability).map_err(mlua::Error::external)?;
        if self.operation.is_some() && !self.allowed_endpoints.contains(&descriptor.token) {
            self.guard.trip();
            return Err(mlua::Error::external(invalid()));
        }
        self.discovered
            .borrow_mut()
            .insert(descriptor.token, descriptor.clone());
        lua.create_userdata(Endpoint {
            descriptor,
            owner: self.owner.clone(),
        })
    }
    pub fn resolve_endpoint(
        &self,
        endpoint: &mlua::AnyUserData,
    ) -> Result<EndpointToken, DriverError> {
        self.check()?;
        let endpoint = endpoint.borrow::<Endpoint>().map_err(|_| invalid())?;
        let token = endpoint.descriptor.token;
        if endpoint.owner != self.owner
            || !(self.allowed_endpoints.contains(&token)
                || self.discovered.borrow().contains_key(&token))
        {
            self.guard.trip();
            return Err(invalid());
        }
        Ok(token)
    }
    pub(crate) fn validate_native(
        &self,
        owner: &str,
        token: EndpointToken,
        code: u8,
        write: bool,
        codes: &BTreeSet<u8>,
    ) -> Result<(), DriverError> {
        self.check()?;
        let permitted = owner == self.owner.as_ref()
            && codes.contains(&code)
            && (!write || self.write_allowed)
            && (self.operation.is_some()
                || (!write && self.discovered.borrow().contains_key(&token)));
        if !permitted {
            self.guard.trip();
            return Err(DriverError::new(
                ErrorCode::PermissionDenied,
                "Native operation is outside this callback's endpoint or register grant.",
            ));
        }
        if let Some(operation) = &self.operation
            && let Err(error) = operation.validate(token, code, write)
        {
            if error.code == ErrorCode::PermissionDenied {
                self.guard.trip();
            }
            return Err(error);
        }
        Ok(())
    }
    pub fn native_context(&self, reads: &BTreeSet<u8>) -> OperationContext {
        self.operation.clone().unwrap_or_else(|| {
            OperationContext::discovery(
                self.owner.clone(),
                self.discovered.borrow().keys().copied().collect(),
                reads.clone(),
                self.guard.deadline,
            )
        })
    }
}
pub(crate) fn lua_error(lua: &Lua, error: &DriverError) -> mlua::Result<Value> {
    lua.to_value(error)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub serial: u64,
    pub device: Device,
    pub endpoints: Vec<EndpointDescriptor>,
    pub same_identity: bool,
    pub poll_ms: Option<u32>,
    pub writes: BTreeMap<String, BTreeMap<u8, u16>>,
}
impl Candidate {
    pub fn equivalent(&self, other: &Self) -> bool {
        self.device.id == other.device.id
            && self.device.kind == other.device.kind
            && self.device.label == other.device.label
            && self.device.groups == other.device.groups
            && self
                .device
                .characteristics
                .iter()
                .map(|characteristic| {
                    (
                        &characteristic.id,
                        &characteristic.label,
                        &characteristic.spec,
                        &characteristic.group,
                        characteristic.access,
                    )
                })
                .eq(other.device.characteristics.iter().map(|characteristic| {
                    (
                        &characteristic.id,
                        &characteristic.label,
                        &characteristic.spec,
                        &characteristic.group,
                        characteristic.access,
                    )
                }))
            && self.endpoints == other.endpoints
            && self.same_identity == other.same_identity
            && self.poll_ms == other.poll_ms
            && self.writes == other.writes
    }
}
struct Definition {
    candidate: Candidate,
    start: Option<Function>,
    refresh: Function,
    write: Option<Function>,
    stop: Option<Function>,
}
#[derive(Clone)]
enum CallbackKind<'a> {
    Observe,
    Refresh,
    Write {
        characteristic: &'a str,
        command: CharacteristicCommand,
    },
    Stop,
    Event(Json),
}
impl CallbackKind<'_> {
    fn is_write(&self) -> bool {
        matches!(self, Self::Write { .. } | Self::Stop)
    }
    fn is_refresh(&self) -> bool {
        matches!(self, Self::Refresh)
    }
}
struct Timer {
    interval: Duration,
    next: Instant,
    callback: Function,
    cancelled: Arc<AtomicBool>,
}
struct Subscription {
    stream: CapabilityEventStream,
    callback: Function,
}
struct Active {
    definition: Definition,
    grant: DeviceGrant,
    device: Device,
    poll_at: Option<Instant>,
    timers: Vec<Timer>,
    subscriptions: Vec<Subscription>,
}
pub(crate) struct RuntimeHandle {
    pub sender: mpsc::Sender<RuntimeCommand>,
    pub ready: Option<oneshot::Receiver<Result<(), Failure>>>,
}
pub(crate) enum RuntimeCommand {
    Discover {
        guard: Arc<InvocationGuard>,
        reply: oneshot::Sender<Result<Vec<Candidate>, Failure>>,
    },
    Activate {
        serial: u64,
        grant: DeviceGrant,
        reply: oneshot::Sender<Result<Device, Failure>>,
    },
    Retire {
        id: String,
        deadline: Instant,
        reply: oneshot::Sender<Result<(), Failure>>,
    },
    Write {
        id: String,
        characteristic: String,
        command: CharacteristicCommand,
        grant: DeviceGrant,
        reads: BTreeSet<u8>,
        writes: BTreeMap<u8, u16>,
        guard: Arc<InvocationGuard>,
        cancellation: Arc<crate::Cancellation>,
        current: Arc<Mutex<Option<OperationContext>>>,
        reply: oneshot::Sender<Result<Device, Failure>>,
    },
    Shutdown {
        deadline: Instant,
        reply: oneshot::Sender<Result<(), Failure>>,
    },
}
pub(crate) struct RuntimeEvent {
    pub owner: String,
    pub id: String,
    pub epoch: u64,
    pub sequence: u64,
    pub result: Result<Device, Failure>,
}
struct Budget {
    guard: Arc<InvocationGuard>,
    instructions: Cell<u64>,
    active: Cell<Duration>,
    poll_start: Cell<Option<Instant>>,
}
struct Vm {
    lua: Lua,
    package: Package,
    registry: CapabilityRegistry,
    budget: Rc<RefCell<Option<Rc<Budget>>>>,
    discover: Function,
    stop: Option<Function>,
    pending: BTreeMap<u64, Definition>,
    active: BTreeMap<String, Active>,
    serial: u64,
}

pub(crate) fn start(
    package: Package,
    packages: BTreeMap<String, Package>,
    registry: CapabilityRegistry,
    settings: Json,
    events: mpsc::Sender<RuntimeEvent>,
    accepting: Arc<AtomicBool>,
) -> Result<RuntimeHandle, DriverError> {
    let (sender, receiver) = mpsc::channel(64);
    let (ready, readiness) = oneshot::channel();
    let owner = package.manifest.id.clone();
    std::thread::Builder::new()
        .name(format!("lua-{owner}"))
        .spawn(move || {
            let runner = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build();
            let Ok(runner) = runner else {
                let _ = ready.send(Err(Failure::fatal(DriverError::new(
                    ErrorCode::PluginError,
                    "Lua executor initialization failed.",
                ))));
                return;
            };
            let local = tokio::task::LocalSet::new();
            runner.block_on(local.run_until(run_vm(
                package, packages, registry, settings, events, accepting, receiver, ready, owner,
            )));
        })
        .map_err(|_| {
            DriverError::new(
                ErrorCode::PluginError,
                "Lua runtime thread could not start.",
            )
        })?;
    Ok(RuntimeHandle {
        sender,
        ready: Some(readiness),
    })
}
#[allow(clippy::too_many_arguments)]
async fn run_vm(
    package: Package,
    packages: BTreeMap<String, Package>,
    registry: CapabilityRegistry,
    settings: Json,
    events: mpsc::Sender<RuntimeEvent>,
    accepting: Arc<AtomicBool>,
    mut receiver: mpsc::Receiver<RuntimeCommand>,
    ready: oneshot::Sender<Result<(), Failure>>,
    owner: String,
) {
    let mut vm = match Vm::initialize(package, packages, registry, settings).await {
        Ok(vm) => {
            let _ = ready.send(Ok(()));
            vm
        }
        Err(failure) => {
            let _ = ready.send(Err(failure.clone()));
            while let Some(command) = receiver.recv().await {
                match command {
                    RuntimeCommand::Discover { reply, .. } => {
                        let _ = reply.send(Err(failure.clone()));
                    }
                    RuntimeCommand::Shutdown { reply, .. } => {
                        let _ = reply.send(Ok(()));
                        break;
                    }
                    _ => {}
                }
            }
            return;
        }
    };
    let mut quarantined = None;
    loop {
        // Due observations are considered before every command, not only when the mailbox empties.
        if quarantined.is_none()
            && accepting.load(Ordering::SeqCst)
            && let Some((id, sequence, result)) = vm.observe_due().await
        {
            let epoch = vm.active.get(&id).map_or(0, |active| active.grant.epoch());
            latch_failure(&mut quarantined, &mut vm, &result);
            let event = RuntimeEvent {
                owner: owner.clone(),
                id,
                epoch,
                sequence,
                result,
            };
            if event.result.as_ref().is_err_and(|failure| failure.fatal) {
                let _ = events.send(event).await;
            } else {
                let _ = events.try_send(event);
            }
        }
        let wake = vm.next_wake();
        let command = tokio::select! {
            command = receiver.recv() => match command {
                Some(command) => command,
                None => break,
            },
            _ = tokio::time::sleep_until(wake.into()), if quarantined.is_none() && accepting.load(Ordering::SeqCst) => {
                continue;
            }
        };
        match command {
            RuntimeCommand::Discover { guard, reply } => {
                let result = match &quarantined {
                    Some(failure) => Err(failure.clone()),
                    None => vm.discovery(guard).await,
                };
                latch_failure(&mut quarantined, &mut vm, &result);
                let _ = reply.send(result);
            }
            RuntimeCommand::Activate {
                serial,
                grant,
                reply,
            } => {
                let result = match &quarantined {
                    Some(failure) => Err(failure.clone()),
                    None => vm.activate(serial, grant).await,
                };
                latch_failure(&mut quarantined, &mut vm, &result);
                let _ = reply.send(result);
            }
            RuntimeCommand::Retire {
                id,
                deadline,
                reply,
            } => {
                let result = vm.retire(&id, deadline).await;
                latch_failure(&mut quarantined, &mut vm, &result);
                let _ = reply.send(result);
            }
            RuntimeCommand::Write {
                id,
                characteristic,
                command,
                grant,
                reads,
                writes,
                guard,
                cancellation,
                current,
                reply,
            } => {
                // A queued command must not hold gates needed by observations checked before dequeue.
                let result = match &quarantined {
                    Some(failure) => Err(failure.clone()),
                    None => {
                        vm.admitted_write(
                            &id,
                            &characteristic,
                            command,
                            grant,
                            reads,
                            writes,
                            guard,
                            cancellation,
                            current,
                            &accepting,
                        )
                        .await
                    }
                };
                latch_failure(&mut quarantined, &mut vm, &result);
                let _ = reply.send(result);
            }
            RuntimeCommand::Shutdown { deadline, reply } => {
                let result = vm.shutdown(deadline).await;
                let _ = reply.send(result);
                break;
            }
        }
    }
    vm.revoke();
}
fn latch_failure<T>(quarantined: &mut Option<Failure>, vm: &mut Vm, result: &Result<T, Failure>) {
    if let Err(failure) = result
        && failure.fatal
    {
        *quarantined = Some(failure.clone());
        vm.revoke();
    }
}
impl Vm {
    async fn initialize(
        package: Package,
        packages: BTreeMap<String, Package>,
        registry: CapabilityRegistry,
        settings: Json,
    ) -> Result<Self, Failure> {
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE,
            LuaOptions::default(),
        )?;
        lua.set_memory_limit(16 * 1024 * 1024)?;
        let original = lua.globals();
        let environment = lua.create_table()?;
        for name in [
            "assert", "error", "ipairs", "pairs", "next", "pcall", "xpcall", "select", "tonumber",
            "tostring", "type", "rawequal", "table", "string", "math", "utf8",
        ] {
            environment.raw_set(name, original.raw_get::<Value>(name)?)?;
        }
        environment
            .raw_get::<Table>("string")?
            .raw_set("dump", Value::Nil)?;
        environment.raw_set("_G", environment.clone())?;
        // Keep mlua's private globals, including coroutine.yield, for async wrappers.
        // Every package/module instead receives this isolated allowlisted _ENV.
        let budget: Rc<RefCell<Option<Rc<Budget>>>> = Rc::new(RefCell::new(None));
        let hook_budget = budget.clone();
        lua.set_global_hook(
            HookTriggers::new().every_nth_instruction(1000),
            move |_, debug| {
                if let Some(budget) = hook_budget.borrow().as_ref() {
                    budget.instructions.set(budget.instructions.get() + 1000);
                    let active = budget.active.get()
                        + budget
                            .poll_start
                            .get()
                            .map_or(Duration::ZERO, |start| start.elapsed());
                    if budget.instructions.get() > 1_000_000 || active > Duration::from_millis(100)
                    {
                        budget.guard.trip();
                    }
                    budget.guard.check().map_err(mlua::Error::external)?;
                }
                // Hook-yield only package Lua. Yielding inside mlua's async polling
                // trampoline can be mistaken for its pending-I/O yield and lose a wakeup.
                Ok(match debug.source().source.as_deref() {
                    Some("plugin-entry" | "plugin-module") => VmState::Yield,
                    _ => VmState::Continue,
                })
            },
        )?;
        let sources = Arc::new(packages);
        let cache = Rc::new(RefCell::new(BTreeMap::<String, Value>::new()));
        let loading = Rc::new(RefCell::new(BTreeSet::new()));
        install_require(
            &lua,
            environment.clone(),
            package.manifest.id.clone(),
            sources,
            cache,
            loading,
        )?;
        let source = package
            .sources
            .get(&package.manifest.entry)
            .ok_or_else(invalid)?;
        let guard = InvocationGuard::new(Instant::now() + CALLBACK_LIMIT);
        let entry: Value = guarded(
            &budget,
            guard.clone(),
            lua.load(source.as_ref())
                .set_name("plugin-entry")
                .set_environment(environment)
                .into_function()?
                .call_async(()),
        )
        .await?;
        let entry = object(entry, &["configure"])?;
        let configure: Function = entry.raw_get("configure")?;
        let configured = guarded(
            &budget,
            guard,
            configure.call_async::<Value>(lua.to_value(&settings)?),
        )
        .await?;
        let configured = object(configured, &["discover", "stop"])?;
        let discover: Function = configured.raw_get("discover")?;
        let stop = configured.raw_get("stop")?;
        Ok(Self {
            lua,
            package,
            registry,
            budget,
            discover,
            stop,
            pending: BTreeMap::new(),
            active: BTreeMap::new(),
            serial: 0,
        })
    }
    fn context(
        &self,
        operation: Option<OperationContext>,
        guard: Arc<InvocationGuard>,
    ) -> InvocationContext {
        InvocationContext {
            owner: Arc::from(self.package.manifest.id.as_str()),
            permissions: Json::Null,
            allowed_endpoints: operation.as_ref().map_or_else(Vec::new, |operation| {
                self.active
                    .values()
                    .find(|active| {
                        active.grant.epoch() == operation.epoch()
                            && active.device.id == operation.device()
                    })
                    .map_or_else(Vec::new, |active| active.grant.endpoints().to_vec())
            }),
            operation,
            guard,
            write_allowed: false,
            discovered: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }
    async fn discovery(&mut self, guard: Arc<InvocationGuard>) -> Result<Vec<Candidate>, Failure> {
        let invocation = self.context(None, guard.clone());
        let ctx = self
            .registry
            .install(&self.lua, &self.package.manifest, invocation.clone())?;
        let context = self.lua.create_table()?;
        context.raw_set("capabilities", ctx)?;
        let result = guarded(
            &self.budget,
            guard.clone(),
            self.discover.call_async::<Value>(context),
        )
        .await;
        guard.close();
        let result = result?;
        let table = object(result, &["devices", "error"])?;
        if let Some(error) = reported_error(&table)? {
            return Err(Failure::operation(error));
        }
        let definitions = array(table.raw_get("devices")?)?;
        if definitions.len() > 64 {
            return Err(invalid().into());
        }
        self.pending.clear();
        let mut candidates = Vec::with_capacity(definitions.len());
        let mut ids = BTreeSet::new();
        for definition in definitions {
            self.serial += 1;
            let definition = parse_definition(
                definition,
                self.serial,
                &self.package,
                &self.registry,
                &invocation,
            )?;
            if !ids.insert(definition.candidate.device.id.clone()) {
                return Err(invalid().into());
            }
            candidates.push(definition.candidate.clone());
            self.pending.insert(self.serial, definition);
        }
        Ok(candidates)
    }
    async fn activate(&mut self, serial: u64, grant: DeviceGrant) -> Result<Device, Failure> {
        let definition = self.pending.remove(&serial).ok_or_else(invalid)?;
        let id = definition.candidate.device.id.clone();
        let device = definition.candidate.device.clone();
        let poll_at = definition
            .candidate
            .poll_ms
            .map(|ms| Instant::now() + Duration::from_millis(u64::from(ms)));
        self.active.insert(
            id.clone(),
            Active {
                definition,
                grant,
                device,
                poll_at,
                timers: vec![],
                subscriptions: vec![],
            },
        );
        let operation = match self
            .read_operation(&id, Instant::now() + CALLBACK_LIMIT)
            .await
        {
            Ok(operation) => operation,
            Err(failure) => {
                if let Some(active) = self.active.remove(&id) {
                    active.grant.revoke();
                }
                return Err(failure);
            }
        };
        let result = async {
            if let Some(start) = self.active[&id].definition.start.clone() {
                self.callback(&id, start, &operation, CallbackKind::Observe)
                    .await?;
            }
            self.refresh(&id, &operation).await
        }
        .await;
        if result.is_err() {
            operation.cancel();
        }
        operation.finish();
        if result.is_err() {
            if let Err(failure) = self.retire(&id, operation.deadline()).await
                && failure.fatal
            {
                return Err(failure);
            }
        } else {
            let active = self.active.get_mut(&id).expect("activated device");
            active.poll_at = active
                .definition
                .candidate
                .poll_ms
                .map(|ms| Instant::now() + Duration::from_millis(u64::from(ms)));
        }
        result
    }
    async fn read_operation(
        &self,
        id: &str,
        deadline: Instant,
    ) -> Result<OperationContext, Failure> {
        self.active
            .get(id)
            .ok_or_else(invalid)?
            .grant
            .begin(
                self.registry.reads(&self.package.manifest)?,
                BTreeMap::new(),
                deadline,
            )
            .await
            .map_err(Failure::operation)
    }
    async fn callback(
        &mut self,
        id: &str,
        function: Function,
        operation: &OperationContext,
        kind: CallbackKind<'_>,
    ) -> Result<Value, Failure> {
        let guard = InvocationGuard::new(operation.deadline());
        self.callback_with_guard(id, function, operation, kind, guard)
            .await
    }
    async fn callback_with_guard(
        &mut self,
        id: &str,
        function: Function,
        operation: &OperationContext,
        kind: CallbackKind<'_>,
        guard: Arc<InvocationGuard>,
    ) -> Result<Value, Failure> {
        let write = kind.is_write();
        let mut invocation = self.context(
            Some(if write {
                operation.clone()
            } else {
                operation.read_only()
            }),
            guard.clone(),
        );
        invocation.write_allowed = write;
        let context = self.lua.create_table()?;
        context.raw_set(
            "capabilities",
            self.registry
                .install(&self.lua, &self.package.manifest, invocation.clone())?,
        )?;
        let published = Rc::new(RefCell::new(Vec::<Value>::new()));
        self.install_publish(&context, &guard, write, &published)?;
        let registered = Rc::new(RefCell::new(Vec::<Timer>::new()));
        self.install_every(id, &context, &guard, write, &registered)?;
        let subscriptions = Rc::new(RefCell::new(Vec::<Subscription>::new()));
        self.install_subscribe(id, &context, invocation, &guard, write, &subscriptions)?;
        let result = match &kind {
            CallbackKind::Write {
                characteristic,
                command,
            } => {
                guarded(
                    &self.budget,
                    guard.clone(),
                    function.call_async::<Value>((
                        context,
                        *characteristic,
                        self.lua.to_value(command)?,
                    )),
                )
                .await
            }
            CallbackKind::Event(event) => {
                guarded(
                    &self.budget,
                    guard.clone(),
                    function.call_async::<Value>((context, self.lua.to_value(event)?)),
                )
                .await
            }
            CallbackKind::Observe | CallbackKind::Refresh | CallbackKind::Stop => {
                guarded(
                    &self.budget,
                    guard.clone(),
                    function.call_async::<Value>(context),
                )
                .await
            }
        };
        guard.close();
        self.apply_result(id, result?, &kind, published, registered, subscriptions)
    }
    fn install_publish(
        &self,
        context: &Table,
        guard: &Arc<InvocationGuard>,
        write: bool,
        published: &Rc<RefCell<Vec<Value>>>,
    ) -> Result<(), Failure> {
        let staged = published.clone();
        let publish_guard = guard.clone();
        context.raw_set(
            "publish",
            self.lua.create_function(move |_, updates: Value| {
                publish_guard.check().map_err(mlua::Error::external)?;
                if write {
                    publish_guard.trip();
                    return Err(mlua::Error::external(invalid()));
                }
                staged.borrow_mut().push(updates);
                Ok(())
            })?,
        )?;
        Ok(())
    }
    fn install_every(
        &self,
        id: &str,
        context: &Table,
        guard: &Arc<InvocationGuard>,
        write: bool,
        registered: &Rc<RefCell<Vec<Timer>>>,
    ) -> Result<(), Failure> {
        let timers = registered.clone();
        let timer_guard = guard.clone();
        let existing = self.active[id].timers.len();
        context.raw_set(
            "every",
            self.lua
                .create_function(move |lua, (ms, callback): (u32, Function)| {
                    timer_guard.check().map_err(mlua::Error::external)?;
                    if write || ms < 500 || existing + timers.borrow().len() >= 32 {
                        timer_guard.trip();
                        return Err(mlua::Error::external(invalid()));
                    }
                    let cancelled = Arc::new(AtomicBool::new(false));
                    timers.borrow_mut().push(Timer {
                        interval: Duration::from_millis(u64::from(ms)),
                        next: Instant::now() + Duration::from_millis(u64::from(ms)),
                        callback,
                        cancelled: cancelled.clone(),
                    });
                    let handle = lua.create_table()?;
                    handle.raw_set(
                        "cancel",
                        lua.create_function(move |_, _: mlua::MultiValue| {
                            cancelled.store(true, Ordering::SeqCst);
                            Ok(())
                        })?,
                    )?;
                    Ok(handle)
                })?,
        )?;
        Ok(())
    }
    fn install_subscribe(
        &self,
        id: &str,
        context: &Table,
        invocation: InvocationContext,
        guard: &Arc<InvocationGuard>,
        write: bool,
        subscriptions: &Rc<RefCell<Vec<Subscription>>>,
    ) -> Result<(), Failure> {
        let staged_subscriptions = subscriptions.clone();
        let subscribe_guard = guard.clone();
        let factories = self.registry.clone();
        let manifest = self.package.manifest.clone();
        let existing_subscriptions = self.active[id].subscriptions.len();
        context.raw_set(
            "subscribe",
            self.lua.create_function(
                move |lua,
                      (capability, endpoint, event, callback): (
                    String,
                    mlua::AnyUserData,
                    String,
                    Function,
                )| {
                    subscribe_guard.check().map_err(mlua::Error::external)?;
                    if write || existing_subscriptions + staged_subscriptions.borrow().len() >= 32 {
                        subscribe_guard.trip();
                        return Err(mlua::Error::external(invalid()));
                    }
                    let endpoint = endpoint.borrow::<Endpoint>()?;
                    if endpoint.owner != invocation.owner
                        || !invocation
                            .allowed_endpoints
                            .contains(&endpoint.descriptor.token)
                    {
                        subscribe_guard.trip();
                        return Err(mlua::Error::external(invalid()));
                    }
                    if endpoint.descriptor.capability != capability {
                        subscribe_guard.trip();
                        return Err(mlua::Error::external(invalid()));
                    }
                    let Some(permissions) = manifest.permissions.get(&capability) else {
                        return Ok((
                            Value::Nil,
                            lua_error(
                                lua,
                                &DriverError::new(
                                    ErrorCode::UnsupportedOperation,
                                    "Event capability is not declared by this package.",
                                ),
                            )?,
                        ));
                    };
                    let factory = factories
                        .factory(&capability)
                        .map_err(mlua::Error::external)?;
                    let mut event_context = invocation.clone();
                    event_context.permissions = permissions.clone();
                    match factory.subscribe_events(event_context, endpoint.descriptor.token, &event)
                    {
                        Ok(stream) => {
                            let cancelled = stream.cancelled.clone();
                            staged_subscriptions
                                .borrow_mut()
                                .push(Subscription { stream, callback });
                            let handle = lua.create_table()?;
                            handle.raw_set(
                                "cancel",
                                lua.create_function(move |_, _: mlua::MultiValue| {
                                    cancelled.store(true, Ordering::SeqCst);
                                    Ok(())
                                })?,
                            )?;
                            Ok((Value::Table(handle), Value::Nil))
                        }
                        Err(error) => Ok((Value::Nil, lua_error(lua, &error)?)),
                    }
                },
            )?,
        )?;
        Ok(())
    }
    fn apply_result(
        &mut self,
        id: &str,
        value: Value,
        kind: &CallbackKind<'_>,
        published: Rc<RefCell<Vec<Value>>>,
        registered: Rc<RefCell<Vec<Timer>>>,
        subscriptions: Rc<RefCell<Vec<Subscription>>>,
    ) -> Result<Value, Failure> {
        let refresh = kind.is_refresh();
        let write = kind.is_write();
        if !matches!(value, Value::Nil) {
            let table = object(
                value.clone(),
                if refresh {
                    &["updates", "error"]
                } else {
                    &["error"]
                },
            )?;
            if let Some(error) = reported_error(&table)? {
                return Err(Failure::operation(error));
            }
            if refresh {
                published.borrow_mut().push(table.raw_get("updates")?);
            } else {
                return Err(invalid().into());
            }
        } else if refresh {
            return Err(invalid().into());
        }
        let mut device = self.active[id].device.clone();
        let mut observed = BTreeSet::new();
        for updates in published.borrow().iter() {
            apply_updates(&mut device, updates.clone(), &mut observed)?;
        }
        if refresh
            && device.characteristics.iter().any(|characteristic| {
                characteristic.access.read && !observed.contains(&characteristic.id)
            })
        {
            return Err(invalid().into());
        }
        if !write {
            device.status = DeviceStatus::Online;
            device.error = None;
            for characteristic in &mut device.characteristics {
                if matches!(characteristic.spec, CharacteristicSpec::Action) {
                    characteristic.status = CharacteristicStatus::Ok;
                    characteristic.error = None;
                }
            }
        }
        device.validate()?;
        let active = self.active.get_mut(id).ok_or_else(invalid)?;
        active.device = device;
        active.timers.extend(registered.borrow_mut().drain(..));
        active
            .subscriptions
            .extend(subscriptions.borrow_mut().drain(..));
        Ok(value)
    }
    async fn refresh(&mut self, id: &str, operation: &OperationContext) -> Result<Device, Failure> {
        let refresh = self.active[id].definition.refresh.clone();
        match self
            .callback(id, refresh, operation, CallbackKind::Refresh)
            .await
        {
            Ok(_) => {
                let active = self.active.get_mut(id).ok_or_else(invalid)?;
                active.device.status = DeviceStatus::Online;
                active.device.error = None;
                Ok(active.device.clone())
            }
            Err(failure) => {
                self.mark_failed(id, &failure.error);
                Err(failure)
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn admitted_write(
        &mut self,
        id: &str,
        characteristic: &str,
        command: CharacteristicCommand,
        grant: DeviceGrant,
        reads: BTreeSet<u8>,
        writes: BTreeMap<u8, u16>,
        guard: Arc<InvocationGuard>,
        cancellation: Arc<crate::Cancellation>,
        current: Arc<Mutex<Option<OperationContext>>>,
        accepting: &AtomicBool,
    ) -> Result<Device, Failure> {
        guard.check().map_err(Failure::operation)?;
        if cancellation.abandoned.load(Ordering::SeqCst) {
            return Err(Failure::operation(crate::timeout_error()));
        }
        let operation = grant
            .begin(reads, writes, guard.deadline)
            .await
            .map_err(Failure::operation)?;
        let _current = crate::CurrentOperation {
            current: current.clone(),
            sequence: operation.sequence(),
        };
        {
            let mut active = current.lock().expect("current operation");
            *active = Some(operation.clone());
            if !accepting.load(Ordering::SeqCst) {
                operation.cancel();
                operation.finish();
                return Err(Failure::operation(crate::unavailable_error()));
            }
        }
        {
            let mut pending = cancellation
                .operation
                .lock()
                .expect("operation cancellation");
            *pending = Some(operation.clone());
            if cancellation.abandoned.load(Ordering::SeqCst) {
                operation.abandon();
                operation.finish();
                return Err(Failure::operation(crate::timeout_error()));
            }
        }
        let result = self
            .write(
                id,
                grant.epoch(),
                characteristic,
                command,
                operation.clone(),
                guard,
            )
            .await;
        if result.is_err() {
            operation.cancel();
        }
        operation.finish();
        result
    }
    async fn write(
        &mut self,
        id: &str,
        epoch: u64,
        characteristic: &str,
        command: CharacteristicCommand,
        operation: OperationContext,
        guard: Arc<InvocationGuard>,
    ) -> Result<Device, Failure> {
        let active = self.active.get(id).ok_or_else(|| {
            Failure::operation(DriverError::new(
                ErrorCode::DeviceUnavailable,
                "Device handler has retired.",
            ))
        })?;
        if active.grant.epoch() != epoch || !active.grant.is_active() {
            return Err(Failure::operation(DriverError::new(
                ErrorCode::DeviceUnavailable,
                "Device handler has retired.",
            )));
        }
        let function = active.definition.write.clone().ok_or_else(|| {
            Failure::operation(DriverError::new(
                ErrorCode::UnsupportedOperation,
                "Device has no write callback.",
            ))
        })?;
        let result = self
            .callback_with_guard(
                id,
                function,
                &operation,
                CallbackKind::Write {
                    characteristic,
                    command: command.clone(),
                },
                guard.clone(),
            )
            .await;
        if let Err(failure) = result {
            self.mark_failed(id, &failure.error);
            return Err(failure);
        }
        if !matches!(command, CharacteristicCommand::Invoke {})
            && let Some(selected) = self.active.get_mut(id).and_then(|active| {
                active
                    .device
                    .characteristics
                    .iter_mut()
                    .find(|item| item.id == characteristic)
            })
        {
            selected.status = CharacteristicStatus::Unavailable;
        }
        let device = self.refresh(id, &operation).await?;
        if !matches!(command, CharacteristicCommand::Invoke {})
            && device
                .characteristics
                .iter()
                .find(|item| item.id == characteristic)
                .is_none_or(|item| item.status != CharacteristicStatus::Ok || item.value.is_none())
        {
            return Err(Failure::operation(DriverError::new(
                ErrorCode::DeviceUnavailable,
                "Write could not be confirmed by an observed characteristic.",
            )));
        }
        Ok(device)
    }
    fn mark_failed(&mut self, id: &str, error: &DriverError) {
        if let Some(active) = self.active.get_mut(id) {
            unavailable(&mut active.device, error);
        }
    }
    fn next_wake(&self) -> Instant {
        let scheduled = self
            .active
            .values()
            .filter(|active| active.grant.is_active())
            .flat_map(|active| {
                active.poll_at.into_iter().chain(
                    active
                        .timers
                        .iter()
                        .filter(|timer| !timer.cancelled.load(Ordering::SeqCst))
                        .map(|timer| timer.next),
                )
            })
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        if self.active.values().any(|active| {
            active.grant.is_active()
                && active
                    .subscriptions
                    .iter()
                    .any(|subscription| !subscription.stream.cancelled.load(Ordering::SeqCst))
        }) {
            scheduled.min(Instant::now() + Duration::from_millis(20))
        } else {
            scheduled
        }
    }
    async fn observe_due(&mut self) -> Option<(String, u64, Result<Device, Failure>)> {
        let now = Instant::now();
        self.prune_scheduled_work();
        if !self.has_due_timer_or_poll(now)
            && let Some((id, callback, event)) = self.take_subscription_event()
        {
            return Some(self.observe_subscription(id, callback, event, now).await);
        }
        self.observe_timer_or_poll(now).await
    }
    fn prune_scheduled_work(&mut self) {
        for active in self.active.values_mut() {
            if !active.grant.is_active() {
                active.poll_at = None;
                active.timers.clear();
                active.subscriptions.clear();
                continue;
            }
            active
                .timers
                .retain(|timer| !timer.cancelled.load(Ordering::SeqCst));
            active
                .subscriptions
                .retain(|subscription| !subscription.stream.cancelled.load(Ordering::SeqCst));
        }
    }
    fn has_due_timer_or_poll(&self, now: Instant) -> bool {
        self.active.values().any(|active| {
            active.poll_at.is_some_and(|at| at <= now)
                || active.timers.iter().any(|timer| timer.next <= now)
        })
    }
    fn take_subscription_event(&mut self) -> Option<(String, Function, Json)> {
        self.active.iter_mut().find_map(|(id, active)| {
            active.subscriptions.iter_mut().find_map(|subscription| {
                subscription
                    .stream
                    .receiver
                    .try_recv()
                    .ok()
                    .map(|event| (id.clone(), subscription.callback.clone(), event))
            })
        })
    }
    async fn observe_subscription(
        &mut self,
        id: String,
        callback: Function,
        event: Json,
        now: Instant,
    ) -> (String, u64, Result<Device, Failure>) {
        let operation = match self.read_operation(&id, now + CALLBACK_LIMIT).await {
            Ok(operation) => operation,
            Err(failure) => {
                self.mark_failed(&id, &failure.error);
                return (id, 0, Err(failure));
            }
        };
        let guard = InvocationGuard::new(operation.deadline());
        let exchanged = self
            .lua
            .to_value(&event)
            .map_err(Failure::from)
            .and_then(json)
            .and_then(|event| {
                synapsed_api::validate_encoded_size(&event, 16 * 1024)?;
                Ok(event)
            });
        let result = match exchanged {
            Ok(event) => self
                .callback_with_guard(&id, callback, &operation, CallbackKind::Event(event), guard)
                .await
                .map(|_| self.active[&id].device.clone()),
            Err(failure) => Err(failure),
        };
        if let Err(failure) = &result {
            self.mark_failed(&id, &failure.error);
            operation.cancel();
        }
        operation.finish();
        (id, operation.sequence(), result)
    }
    async fn observe_timer_or_poll(
        &mut self,
        now: Instant,
    ) -> Option<(String, u64, Result<Device, Failure>)> {
        let id = self
            .active
            .iter()
            .filter_map(|(id, active)| {
                let at = active
                    .poll_at
                    .into_iter()
                    .chain(
                        active
                            .timers
                            .iter()
                            .filter(|timer| !timer.cancelled.load(Ordering::SeqCst))
                            .map(|timer| timer.next),
                    )
                    .filter(|at| *at <= now)
                    .min()?;
                Some((id, at))
            })
            .min_by_key(|(_, at)| *at)
            .map(|(id, _)| id.clone())?;
        let operation = match self.read_operation(&id, now + CALLBACK_LIMIT).await {
            Ok(operation) => operation,
            Err(failure) => {
                self.mark_failed(&id, &failure.error);
                return Some((id, 0, Err(failure)));
            }
        };
        let timer_at = self.active[&id]
            .timers
            .iter()
            .filter(|timer| timer.next <= now && !timer.cancelled.load(Ordering::SeqCst))
            .map(|timer| timer.next)
            .min();
        let poll = self.active[&id]
            .poll_at
            .is_some_and(|at| at <= now && timer_at.is_none_or(|timer| at <= timer));
        let result = if poll {
            self.observe_poll(&id, &operation).await
        } else {
            self.observe_timer(&id, &operation, now).await
        };
        if let Err(failure) = &result {
            self.mark_failed(&id, &failure.error);
            operation.cancel();
        }
        operation.finish();
        Some((id, operation.sequence(), result))
    }
    async fn observe_poll(
        &mut self,
        id: &str,
        operation: &OperationContext,
    ) -> Result<Device, Failure> {
        let ms = self.active[id]
            .definition
            .candidate
            .poll_ms
            .expect("poll interval");
        let result = self.refresh(id, operation).await;
        self.active.get_mut(id).expect("active device").poll_at =
            Some(Instant::now() + Duration::from_millis(u64::from(ms)));
        result
    }
    async fn observe_timer(
        &mut self,
        id: &str,
        operation: &OperationContext,
        now: Instant,
    ) -> Result<Device, Failure> {
        let index = self.active[id]
            .timers
            .iter()
            .enumerate()
            .filter(|(_, timer)| timer.next <= now && !timer.cancelled.load(Ordering::SeqCst))
            .min_by_key(|(_, timer)| timer.next)
            .map(|(index, _)| index)
            .expect("due timer");
        let callback = self.active[id].timers[index].callback.clone();
        let interval = self.active[id].timers[index].interval;
        let result = self
            .callback(id, callback, operation, CallbackKind::Observe)
            .await
            .map(|_| self.active[id].device.clone());
        self.active.get_mut(id).expect("active device").timers[index].next =
            Instant::now() + interval;
        result
    }
    async fn retire(&mut self, id: &str, deadline: Instant) -> Result<(), Failure> {
        let Some(active) = self.active.get(id) else {
            return Ok(());
        };
        let grant = active.grant.clone();
        let stop = active.definition.stop.clone();
        grant.revoke();
        let result = if let Some(stop) = stop {
            match grant
                .begin_retirement(self.registry.reads(&self.package.manifest)?, deadline)
                .await
            {
                Ok(operation) => {
                    let result = self
                        .callback(id, stop, &operation, CallbackKind::Stop)
                        .await
                        .map(|_| ());
                    if result.is_err() {
                        operation.cancel();
                    }
                    operation.finish();
                    result
                }
                Err(error) => Err(Failure::operation(error)),
            }
        } else {
            Ok(())
        };
        self.active.remove(id);
        result
    }
    fn revoke(&mut self) {
        for active in self.active.values_mut() {
            active.grant.revoke();
            active.poll_at = None;
            active.timers.clear();
            active.subscriptions.clear();
        }
    }
    async fn shutdown(&mut self, deadline: Instant) -> Result<(), Failure> {
        let ids: Vec<_> = self.active.keys().cloned().collect();
        for id in ids {
            if Instant::now() >= deadline {
                self.revoke();
                return Err(Failure::operation(DriverError::new(
                    ErrorCode::Timeout,
                    "Lua shutdown deadline expired.",
                )));
            }
            self.retire(&id, deadline).await?;
        }
        if let Some(stop) = self.stop.clone() {
            let guard = InvocationGuard::new(deadline);
            let invocation = self.context(None, guard.clone());
            let context = self.lua.create_table()?;
            context.raw_set(
                "capabilities",
                self.registry
                    .install(&self.lua, &self.package.manifest, invocation)?,
            )?;
            let result = guarded(
                &self.budget,
                guard.clone(),
                stop.call_async::<Value>(context),
            )
            .await;
            guard.close();
            if !matches!(result?, Value::Nil) {
                return Err(invalid().into());
            }
        }
        Ok(())
    }
}
async fn guarded<T>(
    slot: &Rc<RefCell<Option<Rc<Budget>>>>,
    guard: Arc<InvocationGuard>,
    future: impl Future<Output = mlua::Result<T>>,
) -> Result<T, Failure> {
    let budget = Rc::new(Budget {
        guard: guard.clone(),
        instructions: Cell::new(0),
        active: Cell::new(Duration::ZERO),
        poll_start: Cell::new(None),
    });
    *slot.borrow_mut() = Some(budget.clone());
    let mut future = pin!(future);
    let measured = poll_fn(|cx| {
        let start = Instant::now();
        budget.poll_start.set(Some(start));
        let result = future.as_mut().poll(cx);
        budget.active.set(budget.active.get() + start.elapsed());
        budget.poll_start.set(None);
        result
    });
    let result = tokio::time::timeout(
        guard.deadline.saturating_duration_since(Instant::now()),
        measured,
    )
    .await;
    *slot.borrow_mut() = None;
    let result = match result {
        Ok(result) => result.map_err(Failure::from),
        Err(_) => {
            guard.trip();
            Err(Failure::fatal(DriverError::new(
                ErrorCode::Timeout,
                "Lua callback exceeded its deadline.",
            )))
        }
    };
    if budget.active.get() > Duration::from_millis(100) {
        guard.trip();
    }
    guard.check().map_err(Failure::fatal)?;
    result
}
fn install_require(
    lua: &Lua,
    environment: Table,
    current: String,
    packages: Arc<BTreeMap<String, Package>>,
    cache: Rc<RefCell<BTreeMap<String, Value>>>,
    loading: Rc<RefCell<BTreeSet<String>>>,
) -> mlua::Result<()> {
    let env = environment.clone();
    environment.raw_set(
        "require",
        lua.create_function(move |lua, name: String| {
            let (owner, path, source) =
                module_source(&current, &name, &packages).map_err(mlua::Error::external)?;
            let key = format!("{owner}:{path}");
            if let Some(value) = cache.borrow().get(&key) {
                return Ok(value.clone());
            }
            if !loading.borrow_mut().insert(key.clone()) {
                return Err(mlua::Error::external(invalid()));
            }
            let module_env = lua.create_table()?;
            for item in env.clone().pairs::<Value, Value>() {
                let (key, value) = item?;
                module_env.raw_set(key, value)?;
            }
            module_env.raw_set("_G", module_env.clone())?;
            install_require(
                lua,
                module_env.clone(),
                owner.into(),
                packages.clone(),
                cache.clone(),
                loading.clone(),
            )?;
            let result = lua
                .load(source)
                .set_name("plugin-module")
                .set_environment(module_env)
                .eval::<Value>();
            loading.borrow_mut().remove(&key);
            let result = result?;
            if matches!(result, Value::Nil) {
                return Err(mlua::Error::external(invalid()));
            }
            cache.borrow_mut().insert(key, result.clone());
            Ok(result)
        })?,
    )
}
fn object(value: Value, allowed: &[&str]) -> Result<Table, Failure> {
    let Value::Table(table) = value else {
        return Err(invalid().into());
    };
    if table.metatable().is_some() {
        return Err(invalid().into());
    }
    for item in table.clone().pairs::<Value, Value>() {
        let (key, _) = item?;
        let Value::String(key) = key else {
            return Err(invalid().into());
        };
        if !allowed.contains(&key.to_str()?.as_ref()) {
            return Err(invalid().into());
        }
    }
    Ok(table)
}
fn array(value: Value) -> Result<Vec<Value>, Failure> {
    let Value::Table(table) = value else {
        return Err(invalid().into());
    };
    if table.metatable().is_some() {
        return Err(invalid().into());
    }
    let length = table.raw_len();
    let mut count = 0;
    for item in table.clone().pairs::<Value, Value>() {
        let (key, _) = item?;
        if !matches!(
            key,
            Value::Integer(index)
                if index > 0 && usize::try_from(index).is_ok_and(|index| index <= length)
        ) {
            return Err(invalid().into());
        }
        count += 1;
    }
    if count != length || length > 4096 {
        return Err(invalid().into());
    }
    (1..=length)
        .map(|index| table.raw_get(index).map_err(Failure::from))
        .collect()
}
fn plain(
    value: Value,
    depth: usize,
    seen: &mut BTreeSet<usize>,
    count: &mut usize,
) -> Result<Json, Failure> {
    *count += 1;
    if depth > 8 || *count > 4096 {
        return Err(invalid().into());
    }
    match value {
        Value::Nil => Ok(Json::Null),
        Value::Boolean(value) => Ok(Json::Bool(value)),
        Value::Integer(value) => Ok(value.into()),
        Value::Number(value) => serde_json::Number::from_f64(value)
            .map(Json::Number)
            .ok_or_else(|| invalid().into()),
        Value::String(value) => {
            let string = value.to_str()?.to_owned();
            if string.chars().count() > 1024 {
                return Err(invalid().into());
            }
            Ok(Json::String(string))
        }
        Value::Table(table) => {
            let pointer = table.to_pointer().addr();
            if table.metatable().is_some() || !seen.insert(pointer) {
                return Err(invalid().into());
            }
            let result = if table.raw_len() > 0 {
                Json::Array(
                    array(Value::Table(table.clone()))?
                        .into_iter()
                        .map(|value| plain(value, depth + 1, seen, count))
                        .collect::<Result<_, _>>()?,
                )
            } else {
                let mut object = serde_json::Map::new();
                for item in table.clone().pairs::<Value, Value>() {
                    let (key, value) = item?;
                    let Value::String(key) = key else {
                        return Err(invalid().into());
                    };
                    object.insert(
                        key.to_str()?.to_owned(),
                        plain(value, depth + 1, seen, count)?,
                    );
                }
                Json::Object(object)
            };
            seen.remove(&pointer);
            Ok(result)
        }
        _ => Err(invalid().into()),
    }
}
fn json(value: Value) -> Result<Json, Failure> {
    plain(value, 0, &mut BTreeSet::new(), &mut 0)
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T, Failure> {
    serde_json::from_value(json(value)?).map_err(|_| invalid().into())
}
fn reported_error(table: &Table) -> Result<Option<DriverError>, Failure> {
    let error: Value = table.raw_get("error")?;
    if matches!(error, Value::Nil) {
        return Ok(None);
    }
    let error = object(error, &["code", "message"])?;
    let parsed: DriverError = decode(Value::Table(error))?;
    parsed.validate()?;
    Ok(Some(DriverError::new(
        parsed.code,
        "Plugin reported an operation failure.",
    )))
}
fn parse_definition(
    value: Value,
    serial: u64,
    package: &Package,
    registry: &CapabilityRegistry,
    invocation: &InvocationContext,
) -> Result<Definition, Failure> {
    let table = object(
        value,
        &[
            "id",
            "kind",
            "label",
            "endpoints",
            "identityPolicy",
            "characteristics",
            "groups",
            "pollIntervalMs",
            "onStart",
            "onRefresh",
            "onWrite",
            "onStop",
        ],
    )?;
    let mut endpoints = Vec::new();
    let mut tokens = BTreeSet::new();
    for value in array(table.raw_get("endpoints")?)? {
        let Value::UserData(endpoint) = value else {
            return Err(invalid().into());
        };
        let endpoint = endpoint.borrow::<Endpoint>()?;
        if endpoint.owner != invocation.owner
            || !invocation
                .discovered
                .borrow()
                .contains_key(&endpoint.descriptor.token)
            || !tokens.insert(endpoint.descriptor.token)
        {
            return Err(invalid().into());
        }
        endpoints.push(endpoint.descriptor.clone());
    }
    endpoints.sort_by_key(|endpoint| endpoint.token);
    let policy: String = decode(table.raw_get("identityPolicy")?)?;
    if policy != "same" && policy != "slot" {
        return Err(invalid().into());
    }
    let poll_ms: Option<u32> = decode(table.raw_get("pollIntervalMs")?)?;
    if poll_ms.is_some_and(|ms| ms < 500) {
        return Err(invalid().into());
    }
    let mut groups = Vec::new();
    let groups_value: Value = table.raw_get("groups")?;
    if !matches!(groups_value, Value::Nil) {
        for value in array(groups_value)? {
            let group = object(value, &["id", "label"])?;
            groups.push(CharacteristicGroup {
                id: decode(group.raw_get("id")?)?,
                label: decode(group.raw_get("label")?)?,
            });
        }
    }
    let (characteristics, writes) =
        parse_characteristics(table.raw_get("characteristics")?, package, registry)?;
    let device = Device {
        id: decode(table.raw_get("id")?)?,
        driver: package.manifest.id.clone(),
        kind: decode(table.raw_get("kind")?)?,
        label: decode(table.raw_get("label")?)?,
        status: DeviceStatus::Online,
        error: None,
        groups,
        characteristics,
    };
    device.validate()?;
    let write: Option<Function> = table.raw_get("onWrite")?;
    if device
        .characteristics
        .iter()
        .any(|characteristic| characteristic.access.write)
        && write.is_none()
    {
        return Err(invalid().into());
    }
    Ok(Definition {
        candidate: Candidate {
            serial,
            device,
            endpoints,
            same_identity: policy == "same",
            poll_ms,
            writes,
        },
        start: table.raw_get("onStart")?,
        refresh: table.raw_get("onRefresh")?,
        write,
        stop: table.raw_get("onStop")?,
    })
}
fn parse_characteristics(
    value: Value,
    package: &Package,
    registry: &CapabilityRegistry,
) -> Result<ParsedCharacteristics, Failure> {
    let mut characteristics = Vec::new();
    let mut writes = BTreeMap::new();
    for value in array(value)? {
        let characteristic = object(value, &["id", "label", "spec", "access", "writes", "group"])?;
        let spec_table: Value = characteristic.raw_get("spec")?;
        let Value::Table(spec_object) = &spec_table else {
            return Err(invalid().into());
        };
        let kind: String = decode(spec_object.raw_get("kind")?)?;
        object(
            spec_table.clone(),
            match kind.as_str() {
                "number" => &["kind", "minimum", "maximum", "step", "integer", "unit"],
                "boolean" | "action" => &["kind"],
                "enum" => &["kind", "options"],
                "string" => &["kind", "maxLength"],
                _ => return Err(invalid().into()),
            },
        )?;
        if kind == "enum" {
            for option in array(spec_object.raw_get("options")?)? {
                object(option, &["value", "label"])?;
            }
        }
        let access: CharacteristicAccess = decode(Value::Table(object(
            characteristic.raw_get("access")?,
            &["read", "write", "notify"],
        )?))?;
        let spec: CharacteristicSpec = decode(spec_table)?;
        let id: String = decode(characteristic.raw_get("id")?)?;
        let bindings = parse_write_bindings(
            characteristic.raw_get("writes")?,
            access.write,
            package,
            registry,
        )?;
        if writes.insert(id.clone(), bindings).is_some() {
            return Err(invalid().into());
        }
        let status = if matches!(spec, CharacteristicSpec::Action) {
            CharacteristicStatus::Ok
        } else {
            CharacteristicStatus::Unavailable
        };
        characteristics.push(Characteristic {
            id,
            label: decode(characteristic.raw_get("label")?)?,
            group: decode(characteristic.raw_get("group")?)?,
            spec,
            access,
            value: None,
            status,
            error: None,
        });
    }
    Ok((characteristics, writes))
}
fn parse_write_bindings(
    value: Value,
    writable: bool,
    package: &Package,
    registry: &CapabilityRegistry,
) -> Result<BTreeMap<u8, u16>, Failure> {
    let private: Json = json(value)?;
    let private = private.as_object().ok_or_else(invalid)?;
    let mut bindings = BTreeMap::new();
    for (capability, value) in private {
        let value = if value.as_object().is_some_and(|object| object.is_empty()) {
            Json::Array(vec![])
        } else {
            value.clone()
        };
        let permissions = package
            .manifest
            .permissions
            .get(capability)
            .ok_or_else(invalid)?;
        let accepted = registry
            .factory(capability)?
            .validate_bindings(permissions, &value)?;
        for (code, mask) in accepted {
            if bindings.insert(code, mask).is_some() {
                return Err(invalid().into());
            }
        }
    }
    if !writable && !private.is_empty() {
        return Err(invalid().into());
    }
    Ok(bindings)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    id: String,
    status: CharacteristicStatus,
    value: Option<CharacteristicValue>,
    error: Option<DriverError>,
}
fn apply_updates(
    device: &mut Device,
    updates: Value,
    observed: &mut BTreeSet<String>,
) -> Result<(), Failure> {
    for value in array(updates)? {
        let update: Update = decode(value)?;
        if !observed.insert(update.id.clone()) {
            return Err(invalid().into());
        }
        let characteristic = device
            .characteristics
            .iter_mut()
            .find(|characteristic| characteristic.id == update.id)
            .ok_or_else(invalid)?;
        if matches!(characteristic.spec, CharacteristicSpec::Action)
            || (!characteristic.access.read && !characteristic.access.write)
        {
            return Err(invalid().into());
        }
        if update.status == CharacteristicStatus::Ok {
            if update.error.is_some() {
                return Err(invalid().into());
            }
            let value = update.value.ok_or_else(invalid)?;
            characteristic.spec.validate_value(&value)?;
            characteristic.value = Some(value);
            characteristic.error = None;
        } else {
            if let Some(value) = update.value {
                value.validate()?;
            }
            if let Some(error) = update.error {
                error.validate()?;
                characteristic.error = Some(DriverError::new(
                    error.code,
                    "Plugin reported an observation failure.",
                ));
            } else {
                characteristic.error = None;
            }
        }
        characteristic.status = update.status;
    }
    Ok(())
}
pub(crate) fn unavailable(device: &mut Device, error: &DriverError) {
    let error = exposed_error(error);
    device.status = DeviceStatus::Unavailable;
    device.error = Some(error.clone());
    for characteristic in &mut device.characteristics {
        characteristic.status = CharacteristicStatus::Unavailable;
        characteristic.error = Some(error.clone());
    }
}
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
