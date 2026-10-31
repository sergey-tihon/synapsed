//! Hardware-free checks against the actual owner. No test launches a native platform backend.
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    thread::{self, ThreadId},
    time::{Duration, Instant},
};
use synapsed_core::{DeviceGrant, EndpointGates, OperationContext};
use synapsed_ddc::{
    DdcBackend, DdcClient, DdcTransport, DriverError, EndpointToken, ErrorCode,
    InventoryWithOwnedHandles, OwnedEndpoint, VcpValue, protocol,
};
use tokio::sync::oneshot;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn error(code: ErrorCode) -> DriverError {
    DriverError::new(code, "Isolated transport failure.")
}
fn edid() -> [u8; 128] {
    let mut bytes = [0; 128];
    bytes[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
    bytes[8..12].copy_from_slice(&[0x09, 0xd1, 0x34, 0x12]);
    bytes[18..20].copy_from_slice(&[1, 4]);
    bytes[54..59].copy_from_slice(&[0, 0, 0, 0xfc, 0]);
    bytes[59..72].copy_from_slice(b"BenQ RD280U\n ");
    bytes[127] = 0xb0;
    bytes
}
#[test]
fn protocol_fixtures_independent_frames_and_strict_replies() {
    assert_eq!(protocol::macos_get_frame(0x10), [0x82, 0x01, 0x10, 0xfd]);
    assert_eq!(
        protocol::linux_get_frame(0x10),
        [0x51, 0x82, 0x01, 0x10, 0xac]
    );
    assert_eq!(
        protocol::macos_set_frame(0x10, 100),
        [0x84, 0x03, 0x10, 0, 0x64, 0xcc]
    );
    assert_eq!(
        protocol::linux_set_frame(0x10, 100),
        [0x51, 0x84, 0x03, 0x10, 0, 0x64, 0xcc]
    );
    let reply = [0x6e, 0x88, 0x02, 0, 0x10, 0, 0, 100, 0, 40, 0xe8];
    assert_eq!(
        protocol::parse_reply(0x10, &reply).unwrap(),
        VcpValue {
            current: 40,
            maximum: 100
        }
    );
    let packed = [0x6e, 0x88, 0x02, 0, 0xd7, 0, 0x02, 0x31, 0x01, 0x31, 0x60];
    assert_eq!(
        protocol::parse_reply(0xd7, &packed).unwrap(),
        VcpValue {
            current: 0x0131,
            maximum: 0x0231
        }
    );
    for index in [0, 1, 2, 3, 4] {
        let mut invalid = reply;
        invalid[index] ^= 1;
        // Independent checksum adjustment isolates structural checks from checksum failure.
        invalid[10] = invalid[..10].iter().fold(0x50, |sum, byte| sum ^ byte);
        assert_eq!(
            protocol::parse_reply(0x10, &invalid).unwrap_err().code,
            ErrorCode::DdcError
        );
    }
    let mut bad_checksum = reply;
    bad_checksum[10] ^= 1;
    assert!(protocol::parse_reply(0x10, &bad_checksum).is_err());
    assert!(protocol::parse_reply(0x10, &reply[..10]).is_err());
    let mut too_long = reply.to_vec();
    too_long.push(0);
    assert!(protocol::parse_reply(0x10, &too_long).is_err());
    assert!(protocol::parse_reply(0xd7, &reply).is_err());
}
#[test]
fn protocol_fixtures_edid_header_checksum_identity_and_safe_fields() {
    let valid = edid();
    let identity = protocol::parse_base_edid(&valid).unwrap();
    assert_eq!(identity.manufacturer.as_deref(), Some("BNQ"));
    assert_eq!(identity.product, 0x1234);
    assert_eq!(identity.product_name.as_deref(), Some("BenQ RD280U"));
    assert_eq!(
        identity.identity,
        "eeb2f2177cdd6caff9e0377bc09b9361fb150c0826af3b3b4aa028fd7123c033"
    );
    assert!(protocol::parse_base_edid(&valid[..127]).is_err());
    let mut extension = valid.to_vec();
    extension.extend_from_slice(&[0; 128]);
    assert!(protocol::parse_base_edid(&extension).is_err());
    let mut bad = valid;
    bad[20] ^= 1;
    assert!(protocol::parse_base_edid(&bad).is_err());
    let mut bad_header = valid;
    bad_header[0] = 1;
    bad_header[127] = bad_header[127].wrapping_sub(1);
    assert!(protocol::parse_base_edid(&bad_header).is_err());
    let mut changed_serial = valid;
    changed_serial[12] = 1;
    changed_serial[127] = changed_serial[127].wrapping_sub(1);
    let changed = protocol::parse_base_edid(&changed_serial).unwrap();
    assert_ne!(identity.identity, changed.identity);
    assert_eq!(identity.product_name, changed.product_name);
}
#[test]
fn protocol_fixtures_exact_i2c_completion() {
    assert!(protocol::check_transfer(2, 2).is_ok());
    assert!(protocol::check_transfer(1, 1).is_ok());
    for (actual, expected) in [(0, 1), (2, 1), (0, 2), (1, 2), (3, 2)] {
        assert_eq!(
            protocol::check_transfer(actual, expected).unwrap_err().code,
            ErrorCode::DdcError
        );
    }
}
#[cfg(target_os = "linux")]
#[test]
fn protocol_fixtures_linux_adapter_exclusions() {
    for name in [
        "SMBus I801",
        "soc:i2cdsi",
        "smu bus",
        "mac-io",
        "u4 adapter",
    ] {
        assert!(!synapsed_ddc::linux::eligible_adapter(name));
    }
    for name in ["AMDGPU DM i2c hw bus", "i915 gmbus dpc", "DP AUX channel"] {
        assert!(synapsed_ddc::linux::eligible_adapter(name));
    }
}

struct NativeBarrier {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    released: Mutex<bool>,
    release: Condvar,
}
impl NativeBarrier {
    fn new() -> (Arc<Self>, oneshot::Receiver<()>) {
        let (sender, receiver) = oneshot::channel();
        (
            Arc::new(Self {
                entered: Mutex::new(Some(sender)),
                released: Mutex::new(false),
                release: Condvar::new(),
            }),
            receiver,
        )
    }
    fn block(&self) {
        if let Some(sender) = self.entered.lock().unwrap().take() {
            let _ = sender.send(());
        }
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.release.wait(released).unwrap();
        }
    }
    fn unblock(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }
}
struct ReleaseOnDrop(Arc<NativeBarrier>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.unblock();
    }
}
#[derive(Default)]
struct Machine {
    endpoints: Vec<(String, Option<String>)>,
    errors: Vec<DriverError>,
    scan_error: Option<DriverError>,
    registers: BTreeMap<u8, VcpValue>,
    gets: Vec<u8>,
    sets: Vec<(u8, u16)>,
    failed_get: Option<u8>,
    failed_set: bool,
    coerced: Option<u16>,
    barrier: Option<Arc<NativeBarrier>>,
    owner: Option<ThreadId>,
    destroyed_on: Option<ThreadId>,
}
struct Backend {
    machine: Arc<Mutex<Machine>>,
    _not_send: Rc<()>,
}
impl DdcBackend for Backend {
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, DriverError> {
        let machine = self.machine.lock().unwrap();
        assert_eq!(machine.owner, Some(thread::current().id()));
        if let Some(error) = &machine.scan_error {
            return Err(error.clone());
        }
        let endpoints = machine
            .endpoints
            .iter()
            .map(|(key, identity)| OwnedEndpoint {
                key: key.clone(),
                manufacturer: Some("BNQ".into()),
                product: Some(0x1234),
                product_name: None,
                identity: identity.clone(),
                transport: Box::new(Transport {
                    machine: self.machine.clone(),
                    _not_send: Rc::new(()),
                }),
            })
            .collect();
        Ok(InventoryWithOwnedHandles {
            endpoints,
            errors: machine.errors.clone(),
        })
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.machine.lock().unwrap().destroyed_on = Some(thread::current().id());
    }
}
struct Transport {
    machine: Arc<Mutex<Machine>>,
    _not_send: Rc<()>,
}
impl DdcTransport for Transport {
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, DriverError> {
        let mut machine = self.machine.lock().unwrap();
        assert_eq!(machine.owner, Some(thread::current().id()));
        machine.gets.push(code);
        if machine.failed_get == Some(code) {
            return Err(error(ErrorCode::DdcError));
        }
        machine
            .registers
            .get(&code)
            .copied()
            .ok_or_else(|| error(ErrorCode::DdcError))
    }
    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DriverError> {
        let barrier = {
            let mut machine = self.machine.lock().unwrap();
            assert_eq!(machine.owner, Some(thread::current().id()));
            machine.sets.push((code, value));
            if machine.failed_set {
                return Err(error(ErrorCode::DdcError));
            }
            let value = machine.coerced.unwrap_or(value);
            machine.registers.get_mut(&code).unwrap().current = value;
            machine.barrier.clone()
        };
        if let Some(barrier) = barrier {
            barrier.block();
        }
        Ok(())
    }
}
fn machine() -> (DdcClient, Arc<Mutex<Machine>>) {
    let machine = Arc::new(Mutex::new(Machine {
        endpoints: vec![(
            "native-path-never-exported".into(),
            Some("identity-a".into()),
        )],
        registers: BTreeMap::from([
            (
                0x10,
                VcpValue {
                    current: 40,
                    maximum: 100,
                },
            ),
            (
                0xd7,
                VcpValue {
                    current: 0x0120,
                    maximum: 0x0231,
                },
            ),
        ]),
        ..Machine::default()
    }));
    let owner_machine = machine.clone();
    let client = DdcClient::start_with_backend(move || {
        owner_machine.lock().unwrap().owner = Some(thread::current().id());
        Ok(Backend {
            machine: owner_machine,
            _not_send: Rc::new(()),
        })
    })
    .unwrap();
    (client, machine)
}
fn grant(endpoint: EndpointToken) -> DeviceGrant {
    EndpointGates::default()
        .activate("fixture", "arbitrary-device", 1, vec![endpoint])
        .unwrap()
}
async fn transaction(grant: &DeviceGrant, mask: u16) -> OperationContext {
    grant
        .begin(
            BTreeSet::from([0x10, 0xd7]),
            BTreeMap::from([(0x10, 0xffff), (0xd7, mask)]),
            deadline(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn protocol_fixtures_native_owner_no_autonomous_probes_and_permissions() {
    let (client, machine) = machine();
    machine
        .lock()
        .unwrap()
        .errors
        .push(error(ErrorCode::PermissionDenied));
    let inventory = client.enumerate().await.unwrap();
    assert_eq!(inventory.endpoints.len(), 1);
    assert_eq!(inventory.errors[0].code, ErrorCode::PermissionDenied);
    assert!(machine.lock().unwrap().gets.is_empty());
    let encoded = serde_json::to_string(&inventory).unwrap();
    assert!(!encoded.contains("native-path-never-exported"));
    assert!(!encoded.contains("serial"));
    assert!(!encoded.contains("edid"));
    let endpoint = inventory.endpoints[0].token;
    let context = OperationContext::discovery(
        "fixture",
        vec![endpoint],
        BTreeSet::from([0x10]),
        deadline(),
    );
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        client
            .get_vcp(context.clone(), endpoint, 0xd7)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(machine.lock().unwrap().sets.is_empty());
    context.finish();
    client.shutdown(deadline()).await.unwrap();
    let machine = machine.lock().unwrap();
    assert_eq!(machine.owner, machine.destroyed_on);
    assert_ne!(machine.owner, Some(thread::current().id()));
}
#[tokio::test]
async fn protocol_fixtures_generation_removal_identity_and_failed_scan() {
    let (client, machine) = machine();
    let first = client.enumerate().await.unwrap().endpoints[0].token;
    assert_eq!(client.enumerate().await.unwrap().endpoints[0].token, first);
    machine.lock().unwrap().endpoints[0].1 = Some("identity-b".into());
    let changed = client.enumerate().await.unwrap().endpoints[0].token;
    assert_eq!(changed.id, first.id);
    assert!(changed.generation > first.generation);
    let stale =
        OperationContext::discovery("fixture", vec![first], BTreeSet::from([0x10]), deadline());
    assert_eq!(
        client
            .get_vcp(stale.clone(), first, 0x10)
            .await
            .unwrap_err()
            .code,
        ErrorCode::DeviceUnavailable
    );
    stale.finish();
    machine.lock().unwrap().endpoints.clear();
    assert!(client.enumerate().await.unwrap().endpoints.is_empty());
    machine
        .lock()
        .unwrap()
        .endpoints
        .push(("native-path-never-exported".into(), None));
    let reconnected = client.enumerate().await.unwrap().endpoints[0].token;
    assert!(reconnected.generation > changed.generation);
    machine.lock().unwrap().scan_error = Some(error(ErrorCode::PermissionDenied));
    assert_eq!(
        client.enumerate().await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    machine.lock().unwrap().scan_error = None;
    let after_error = client.enumerate().await.unwrap().endpoints[0].token;
    assert!(after_error.generation > reconnected.generation);
    client.shutdown(deadline()).await.unwrap();
}
#[tokio::test]
async fn protocol_fixtures_fresh_read_mask_and_observed_not_requested() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xff00).await;
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0xd7, 0x0220)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    client
        .get_vcp(context.clone(), endpoint, 0xd7)
        .await
        .unwrap();
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0xd7, 0x0221)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(machine.lock().unwrap().sets.is_empty());
    machine.lock().unwrap().coerced = Some(0x0230);
    let observed = client
        .set_vcp(context.clone(), endpoint, 0xd7, 0x0220)
        .await
        .unwrap();
    assert_eq!(observed.current, 0x0230);
    assert_eq!(context.outcomes()[0].observed, Some((0x0230, 0x0231)));
    assert_eq!(machine.lock().unwrap().sets, [(0xd7, 0x0220)]);
    context.finish();
    let next = transaction(&grant, 0xff00).await;
    assert_eq!(
        client
            .set_vcp(next.clone(), endpoint, 0xd7, 0x0130)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    next.finish();
    client.shutdown(deadline()).await.unwrap();
}
#[tokio::test]
async fn protocol_fixtures_failed_fresh_get_unchanged_cancel_and_revocation() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xffff).await;
    let value = client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, value.current)
            .await
            .unwrap(),
        value
    );
    assert!(machine.lock().unwrap().sets.is_empty());
    machine.lock().unwrap().failed_get = Some(0x10);
    assert_eq!(
        client
            .get_vcp(context.clone(), endpoint, 0x10)
            .await
            .unwrap_err()
            .code,
        ErrorCode::DdcError
    );
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    machine.lock().unwrap().failed_get = None;
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    context.cancel();
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    context.finish();
    let revoked = transaction(&grant, 0xffff).await;
    client
        .get_vcp(revoked.clone(), endpoint, 0x10)
        .await
        .unwrap();
    grant.revoke();
    assert_eq!(
        client
            .set_vcp(revoked.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::DeviceUnavailable
    );
    revoked.finish();
    assert!(machine.lock().unwrap().sets.is_empty());
    client.shutdown(deadline()).await.unwrap();
}
#[tokio::test]
async fn protocol_fixtures_failed_set_and_failed_readback_record_no_fake_observation() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xffff).await;
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    machine.lock().unwrap().failed_set = true;
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::DdcError
    );
    assert_eq!(context.outcomes()[0].observed, Some((40, 100)));
    assert_eq!(
        context.outcomes()[0].error.as_ref().unwrap().code,
        ErrorCode::DdcError
    );
    assert_eq!(machine.lock().unwrap().registers[&0x10].current, 40);
    machine.lock().unwrap().failed_set = false;
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    machine.lock().unwrap().failed_get = Some(0x10);
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, 60)
            .await
            .unwrap_err()
            .code,
        ErrorCode::DdcError
    );
    assert_eq!(context.outcomes()[1].observed, None);
    assert_eq!(
        context.outcomes()[1].error.as_ref().unwrap().code,
        ErrorCode::DdcError
    );
    assert_eq!(machine.lock().unwrap().registers[&0x10].current, 60);
    context.finish();
    client.shutdown(deadline()).await.unwrap();
}
#[tokio::test]
async fn protocol_fixtures_abandoned_started_set_drains_gate_and_records_readback() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xffff).await;
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    let (barrier, entered) = NativeBarrier::new();
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    {
        let mut machine = machine.lock().unwrap();
        machine.barrier = Some(barrier.clone());
        machine.coerced = Some(55);
    }
    let operation_client = client.clone();
    let operation_context = context.clone();
    let operation = tokio::spawn(async move {
        operation_client
            .set_vcp(operation_context, endpoint, 0x10, 60)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    operation.abort();
    let _ = operation.await;
    context.cancel();
    context.finish();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), transaction(&grant, 0xffff))
            .await
            .is_err()
    );
    assert_eq!(
        client
            .shutdown(Instant::now() + Duration::from_millis(20))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    barrier.unblock();
    client.shutdown(deadline()).await.unwrap();
    let outcomes = context.outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].requested, 60);
    assert_eq!(outcomes[0].observed, Some((55, 100)));
    assert_eq!(outcomes[0].error.as_ref().unwrap().code, ErrorCode::Timeout);
    let machine = machine.lock().unwrap();
    assert_eq!(machine.sets, [(0x10, 60)]);
    assert_eq!(machine.gets, [0x10, 0x10]);
}

#[tokio::test]
async fn protocol_fixtures_queued_cancellation_before_first_native_set() {
    use std::{future::Future, task::Poll};
    let (client, machine) = machine();
    machine
        .lock()
        .unwrap()
        .endpoints
        .push(("second-native-key".into(), Some("identity-c".into())));
    let endpoints = client.enumerate().await.unwrap().endpoints;
    let first = endpoints[0].token;
    let second = endpoints[1].token;
    let first_grant = grant(first);
    let second_grant = grant(second);
    let first_context = transaction(&first_grant, 0xffff).await;
    let second_context = transaction(&second_grant, 0xffff).await;
    client
        .get_vcp(first_context.clone(), first, 0x10)
        .await
        .unwrap();
    client
        .get_vcp(second_context.clone(), second, 0x10)
        .await
        .unwrap();
    let (barrier, entered) = NativeBarrier::new();
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    machine.lock().unwrap().barrier = Some(barrier.clone());
    let worker_client = client.clone();
    let worker_context = first_context.clone();
    let started =
        tokio::spawn(async move { worker_client.set_vcp(worker_context, first, 0x10, 50).await });
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    let mut queued = Box::pin(client.set_vcp(second_context.clone(), second, 0x10, 60));
    std::future::poll_fn(|cx| {
        assert!(queued.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(second_context.abandon());
    second_context.finish();
    barrier.unblock();
    assert_eq!(queued.await.unwrap_err().code, ErrorCode::Timeout);
    assert_eq!(started.await.unwrap().unwrap().current, 50);
    first_context.finish();
    client.shutdown(deadline()).await.unwrap();
    assert_eq!(machine.lock().unwrap().sets, [(0x10, 50)]);
    assert!(second_context.outcomes().is_empty());
}

#[tokio::test]
async fn protocol_fixtures_bounded_nonblocking_queue_and_closed_owner() {
    use std::{future::Future, task::Poll};
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xffff).await;
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    let (barrier, entered) = NativeBarrier::new();
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    machine.lock().unwrap().barrier = Some(barrier.clone());
    let worker_client = client.clone();
    let worker_context = context.clone();
    let started = tokio::spawn(async move {
        worker_client
            .set_vcp(worker_context, endpoint, 0x10, 50)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    let mut pending = Vec::new();
    for _ in 0..64 {
        let mut future = Box::pin(client.get_vcp(context.clone(), endpoint, 0x10));
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        pending.push(future);
    }
    assert_eq!(
        client
            .get_vcp(context.clone(), endpoint, 0x10)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    drop(pending);
    context.finish();
    barrier.unblock();
    assert_eq!(started.await.unwrap().unwrap().current, 50);
    client.shutdown(deadline()).await.unwrap();
    assert_eq!(
        client.enumerate().await.unwrap_err().code,
        ErrorCode::DeviceUnavailable
    );
    assert_eq!(context.outcomes()[0].observed, Some((50, 100)));
    assert_eq!(machine.lock().unwrap().sets, [(0x10, 50)]);
}

#[tokio::test]
async fn protocol_fixtures_receiver_loss_after_admission_keeps_healthy_confirmation() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xffff).await;
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    let (barrier, entered) = NativeBarrier::new();
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    machine.lock().unwrap().barrier = Some(barrier.clone());
    let worker_client = client.clone();
    let worker_context = context.clone();
    let started = tokio::spawn(async move {
        worker_client
            .set_vcp(worker_context, endpoint, 0x10, 50)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    started.abort();
    let _ = started.await;
    assert!(!context.abandon());
    barrier.unblock();
    // The next read is a real confirming observation in the healthy, still-live transaction.
    let confirmed = client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    assert_eq!(confirmed.current, 50);
    assert_eq!(context.outcomes()[0].observed, Some((50, 100)));
    assert!(context.outcomes()[0].error.is_none());
    context.finish();
    client.shutdown(deadline()).await.unwrap();
}

#[tokio::test]
async fn protocol_fixtures_overall_deadline_denies_result_but_drains_admitted_readback() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = grant
        .begin(
            BTreeSet::from([0x10]),
            BTreeMap::from([(0x10, 0xffff)]),
            Instant::now() + Duration::from_millis(150),
        )
        .await
        .unwrap();
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    let (barrier, entered) = NativeBarrier::new();
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    machine.lock().unwrap().barrier = Some(barrier.clone());
    let worker_client = client.clone();
    let worker_context = context.clone();
    let started = tokio::spawn(async move {
        worker_client
            .set_vcp(worker_context, endpoint, 0x10, 50)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.await.unwrap().unwrap_err().code, ErrorCode::Timeout);
    context.finish();
    assert!(context.outcomes().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), transaction(&grant, 0xffff))
            .await
            .is_err()
    );
    barrier.unblock();
    client.shutdown(deadline()).await.unwrap();
    assert_eq!(context.outcomes()[0].observed, Some((50, 100)));
    assert_eq!(
        context.outcomes()[0].error.as_ref().unwrap().code,
        ErrorCode::Timeout
    );
    assert_eq!(machine.lock().unwrap().gets, [0x10, 0x10]);
}

#[tokio::test]
async fn protocol_fixtures_read_only_confirmation_cannot_reuse_write_grants() {
    let (client, machine) = machine();
    let endpoint = client.enumerate().await.unwrap().endpoints[0].token;
    let grant = grant(endpoint);
    let context = transaction(&grant, 0xffff).await;
    let observation = context.read_only();
    assert_eq!(observation.sequence(), context.sequence());
    assert_eq!(observation.change_mask(0x10), None);
    assert_eq!(
        client
            .get_vcp(observation.clone(), endpoint, 0x10)
            .await
            .unwrap()
            .current,
        40
    );
    assert_eq!(
        client
            .set_vcp(observation.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(machine.lock().unwrap().sets.is_empty());
    assert_eq!(
        client
            .set_vcp(context.clone(), endpoint, 0x10, 50)
            .await
            .unwrap()
            .current,
        50
    );
    assert_eq!(
        client
            .get_vcp(observation.clone(), endpoint, 0x10)
            .await
            .unwrap()
            .current,
        50
    );
    assert_eq!(
        client
            .set_vcp(observation.clone(), endpoint, 0x10, 60)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(observation.outcomes()[0].observed, Some((50, 100)));
    assert_eq!(machine.lock().unwrap().sets, [(0x10, 50)]);
    context.finish();
    client.shutdown(deadline()).await.unwrap();
}
