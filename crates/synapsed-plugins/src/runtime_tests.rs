use super::*;
use crate::capabilities::ddc::DdcCapabilityFactory;
use crate::{Coordinator, PluginConfig, Route, loader};
use synapsed_api::{CharacteristicStatus, DriverState, NodeInfo};
use synapsed_core::EndpointGates;
use synapsed_ddc::test_support::{FakeBackend, FakeEndpoint, FakeState, FakeStateHandle};
use synapsed_ddc::{DdcClient, VcpValue};

fn hardware() -> FakeStateHandle {
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
    let endpoint = FakeEndpoint {
        key: "screen-a".into(),
        manufacturer: Some("BNQ".into()),
        product: Some(1),
        product_name: Some("BenQ RD280U".into()),
        identity: Some("a".repeat(64)),
        replies: values,
    };
    Arc::new(Mutex::new(FakeState {
        endpoints: vec![endpoint],
        ..FakeState::default()
    }))
}
fn test_registry(state: &FakeStateHandle) -> CapabilityRegistry {
    let owner_state = state.clone();
    let client = DdcClient::start_with_backend(move || Ok(FakeBackend::new(owner_state))).unwrap();
    let mut registry = CapabilityRegistry::default();
    registry
        .register(Arc::new(DdcCapabilityFactory::new(client)))
        .unwrap();
    registry
}
fn current(state: &FakeStateHandle, code: u8) -> u16 {
    state.lock().unwrap().endpoints[0].replies[&code].current
}
fn mutate(state: &FakeStateHandle, code: u8, value: u16) {
    state.lock().unwrap().endpoints[0]
        .replies
        .get_mut(&code)
        .unwrap()
        .current = value;
}
async fn configured() -> (
    Vm,
    Candidate,
    DeviceGrant,
    CapabilityRegistry,
    FakeStateHandle,
    loader::Catalog,
) {
    let hardware = hardware();
    let registry = test_registry(&hardware);
    let packages = tempfile::tempdir().unwrap();
    let source =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typed-fixture");
    let target = packages.path().join("typed-fixture");
    std::fs::create_dir(&target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
    }
    let config = PluginConfig {
        directory: packages.path().into(),
        directory_explicit: true,
        disabled: vec![],
        discovery_interval_ms: 1000,
        settings: BTreeMap::new(),
    };
    let catalog = loader::load(&config, &registry).unwrap();
    let package = catalog.packages["typed-fixture"].clone();
    let mut vm = Vm::initialize(
        package,
        catalog.packages.clone(),
        registry.clone(),
        serde_json::json!({}),
    )
    .await
    .unwrap();
    let candidate = vm
        .discovery(InvocationGuard::new(Instant::now() + CALLBACK_LIMIT))
        .await
        .unwrap()
        .remove(0);
    let grant = EndpointGates::default()
        .activate(
            "typed-fixture",
            "fixture",
            1,
            candidate
                .endpoints
                .iter()
                .map(|endpoint| endpoint.token)
                .collect(),
        )
        .unwrap();
    vm.activate(candidate.serial, grant.clone()).await.unwrap();
    (vm, candidate, grant, registry, hardware, catalog)
}
fn cancellation() -> Arc<crate::Cancellation> {
    Arc::new(crate::Cancellation {
        abandoned: AtomicBool::new(false),
        operation: Mutex::new(None),
    })
}
fn observed(device: &Device, id: &str) -> CharacteristicValue {
    let characteristic = device
        .characteristics
        .iter()
        .find(|item| item.id == id)
        .unwrap();
    assert_eq!(characteristic.status, CharacteristicStatus::Ok);
    characteristic.value.clone().unwrap()
}
async fn invoke(vm: &mut Vm, candidate: &Candidate, grant: &DeviceGrant) -> (Device, u64) {
    let cancellation = cancellation();
    let accepting = AtomicBool::new(true);
    let device = vm
        .admitted_write(
            "fixture",
            "run",
            CharacteristicCommand::Invoke {},
            grant.clone(),
            vm.registry.reads(&vm.package.manifest).unwrap(),
            candidate.writes["run"].clone(),
            InvocationGuard::new(Instant::now() + CALLBACK_LIMIT),
            cancellation.clone(),
            Arc::new(Mutex::new(None)),
            &accepting,
        )
        .await
        .unwrap();
    let sequence = cancellation
        .operation
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .sequence();
    (device, sequence)
}

#[tokio::test(flavor = "current_thread")]
async fn lua_faults_due_timer_precedes_queued_write_without_reserving_its_gates() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut vm, candidate, grant, registry, hardware, _) = configured().await;
            let (first, _) = invoke(&mut vm, &candidate, &grant).await;
            assert_eq!(
                observed(&first, "invocations"),
                CharacteristicValue::Number(1.0)
            );
            mutate(&hardware, 32, 177);
            vm.active.get_mut("fixture").unwrap().timers[0].next = Instant::now();
            let cancellation = cancellation();
            let operation_state = Arc::new(Mutex::new(None));
            let accepting = AtomicBool::new(true);
            let (reply, response) = oneshot::channel();
            let (sender, mut receiver) = mpsc::channel(64);
            sender
                .try_send(RuntimeCommand::Write {
                    id: "fixture".into(),
                    characteristic: "run".into(),
                    command: CharacteristicCommand::Invoke {},
                    grant: grant.clone(),
                    reads: registry.reads(&vm.package.manifest).unwrap(),
                    writes: candidate.writes["run"].clone(),
                    guard: InvocationGuard::new(Instant::now() + CALLBACK_LIMIT),
                    cancellation: cancellation.clone(),
                    current: operation_state.clone(),
                    reply,
                })
                .unwrap();
            // Force the production scheduler's exact observation-before-dequeue ordering without clock sleeps.
            let (_, timer_sequence, timer) =
                tokio::time::timeout(Duration::from_secs(1), vm.observe_due())
                    .await
                    .expect("queued write must not lock out the due timer")
                    .unwrap();
            assert_eq!(
                observed(&timer.unwrap(), "temperature"),
                CharacteristicValue::Number(17.7)
            );
            assert!(cancellation.operation.lock().unwrap().is_none());
            let RuntimeCommand::Write {
                id,
                characteristic,
                command,
                grant,
                reads,
                writes,
                guard,
                cancellation,
                current: current_operation,
                reply,
            } = receiver.recv().await.unwrap()
            else {
                panic!("expected queued write")
            };
            let result = vm
                .admitted_write(
                    &id,
                    &characteristic,
                    command,
                    grant,
                    reads,
                    writes,
                    guard,
                    cancellation.clone(),
                    current_operation.clone(),
                    &accepting,
                )
                .await;
            reply.send(result).unwrap();
            let second = response.await.unwrap().unwrap();
            assert_eq!(
                observed(&second, "invocations"),
                CharacteristicValue::Number(2.0)
            );
            assert_eq!(
                observed(&second, "temperature"),
                CharacteristicValue::Number(17.7)
            );
            assert_eq!(current(&hardware, 45), 2);
            assert_eq!(
                hardware
                    .lock()
                    .unwrap()
                    .set_calls
                    .iter()
                    .filter(|(_, code, _)| *code == 45)
                    .count(),
                2
            );
            assert!(
                cancellation
                    .operation
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .sequence()
                    > timer_sequence
            );
            assert!(operation_state.lock().unwrap().is_none());
            vm.shutdown(Instant::now() + CALLBACK_LIMIT).await.unwrap();
            registry
                .shutdown(Instant::now() + CALLBACK_LIMIT)
                .await
                .unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lua_faults_older_timer_report_cannot_replace_confirmation_but_fatal_latch_still_quarantines()
 {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut vm, candidate, grant, registry, hardware, catalog) = configured().await;
            invoke(&mut vm, &candidate, &grant).await;
            vm.active.get_mut("fixture").unwrap().timers[0].next = Instant::now();
            let (_, old_sequence, older) = vm.observe_due().await.unwrap();
            let older = older.unwrap();
            let (confirmed, sequence) = invoke(&mut vm, &candidate, &grant).await;
            assert!(sequence > old_sequence);
            assert_eq!(current(&hardware, 45), 2);
            let store = StateStore::new(NodeInfo {
                node_id: "00000000-0000-4000-8000-000000000001".into(),
                instance_id: "00000000-0000-4000-8000-000000000002".into(),
                name: "Ordering".into(),
            })
            .unwrap();
            let mut statuses = catalog.statuses;
            statuses.get_mut("typed-fixture").unwrap().state = DriverState::Online;
            let mut host = Coordinator {
                store: store.clone(),
                registry: registry.clone(),
                packages: catalog.packages,
                statuses,
                runtimes: BTreeMap::new(),
                candidates: BTreeMap::from([("typed-fixture".into(), vec![candidate.clone()])]),
                failed: BTreeMap::new(),
                quarantined: BTreeSet::new(),
                restart_blocked: BTreeSet::new(),
                routes: BTreeMap::from([(
                    "fixture".into(),
                    Route {
                        owner: "typed-fixture".into(),
                        candidate,
                        grant: grant.clone(),
                        device: confirmed.clone(),
                        sequence,
                    },
                )]),
                history: BTreeMap::new(),
                gates: EndpointGates::default(),
                epoch: 1,
                reports: BTreeMap::from([("typed-fixture".into(), vec![confirmed])]),
                last: BTreeMap::new(),
                dominance: BTreeMap::new(),
                current: Arc::new(Mutex::new(None)),
                binding_updates: tokio::sync::watch::channel(Arc::new(BTreeMap::new())).0,
                events: mpsc::channel(64).0,
                accepting: Arc::new(AtomicBool::new(true)),
                settings: BTreeMap::new(),
                #[cfg(debug_assertions)]
                discovery_round: 0,
            };
            host.publish().unwrap();
            let revision = store.snapshot().revision.clone();
            host.observation(RuntimeEvent {
                owner: "typed-fixture".into(),
                id: "fixture".into(),
                epoch: 1,
                sequence: old_sequence,
                result: Ok(older),
            })
            .await;
            assert_eq!(store.snapshot().revision, revision);
            assert_eq!(
                observed(&store.snapshot().devices[0], "invocations"),
                CharacteristicValue::Number(2.0)
            );
            host.observation(RuntimeEvent {
                owner: "typed-fixture".into(),
                id: "fixture".into(),
                epoch: 1,
                sequence: old_sequence,
                result: Err(Failure::operation(DriverError::new(
                    ErrorCode::DdcError,
                    "Prior sample failed.",
                ))),
            })
            .await;
            assert_eq!(store.snapshot().revision, revision);
            host.observation(RuntimeEvent {
                owner: "typed-fixture".into(),
                id: "fixture".into(),
                epoch: 1,
                sequence: old_sequence,
                result: Err(Failure::fatal(DriverError::new(
                    ErrorCode::PluginError,
                    "Latched callback failure.",
                ))),
            })
            .await;
            assert!(!grant.is_active());
            let failed = store.snapshot();
            assert_eq!(failed.drivers[0].state, DriverState::Error);
            let invocations = failed.devices[0]
                .characteristics
                .iter()
                .find(|item| item.id == "invocations")
                .unwrap();
            assert_eq!(invocations.value, Some(CharacteristicValue::Number(2.0)));
            assert_eq!(invocations.status, CharacteristicStatus::Unavailable);
            vm.shutdown(Instant::now() + CALLBACK_LIMIT).await.unwrap();
            registry
                .shutdown(Instant::now() + CALLBACK_LIMIT)
                .await
                .unwrap();
        })
        .await;
}
