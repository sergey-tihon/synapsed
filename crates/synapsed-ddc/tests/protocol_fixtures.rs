//! Hardware-free checks against the actual owner. No test launches a native platform backend.
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{Arc, Mutex},
    task::Poll,
    thread,
    time::{Duration, Instant},
};
use synapsed_core::{DeviceGrant, EndpointGates, OperationContext};
use synapsed_ddc::test_support::{
    FakeBackend, FakeBarrier, FakeEndpoint, FakeState, FakeStateHandle,
};
use synapsed_ddc::{DdcClient, DriverError, EndpointToken, ErrorCode, VcpValue, protocol};

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

struct ReleaseOnDrop(FakeBarrier);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}
fn fake_endpoint(key: &str, identity: Option<&str>) -> FakeEndpoint {
    FakeEndpoint {
        key: key.into(),
        manufacturer: Some("BNQ".into()),
        product: Some(0x1234),
        product_name: None,
        identity: identity.map(str::to_owned),
        replies: BTreeMap::from([
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
    }
}
fn machine() -> (DdcClient, FakeStateHandle) {
    let machine = Arc::new(Mutex::new(FakeState {
        endpoints: vec![fake_endpoint(
            "native-path-never-exported",
            Some("identity-a"),
        )],
        ..FakeState::default()
    }));
    let owner_machine = machine.clone();
    let client =
        DdcClient::start_with_backend(move || Ok(FakeBackend::new(owner_machine))).unwrap();
    (client, machine)
}
fn get_codes(machine: &FakeStateHandle) -> Vec<u8> {
    machine
        .lock()
        .unwrap()
        .get_calls
        .iter()
        .map(|(_, code)| *code)
        .collect()
}
fn set_pairs(machine: &FakeStateHandle) -> Vec<(u8, u16)> {
    machine
        .lock()
        .unwrap()
        .set_calls
        .iter()
        .map(|(_, code, value)| (*code, *value))
        .collect()
}
async fn assert_pending<T>(future: impl Future<Output = T>) {
    let mut future = Box::pin(future);
    std::future::poll_fn(|context| {
        assert!(future.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
}
fn block_next_set(machine: &FakeStateHandle) -> FakeBarrier {
    let barrier = machine.lock().unwrap().set_barrier.clone();
    barrier.arm();
    barrier
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
        .scan_errors
        .push(error(ErrorCode::PermissionDenied));
    let inventory = client.enumerate().await.unwrap();
    assert_eq!(inventory.endpoints.len(), 1);
    assert_eq!(inventory.errors[0].code, ErrorCode::PermissionDenied);
    assert!(get_codes(&machine).is_empty());
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
    assert!(set_pairs(&machine).is_empty());
    context.finish();
    client.shutdown(deadline()).await.unwrap();
    let machine = machine.lock().unwrap();
    assert_eq!(machine.backend_created_on, machine.backend_dropped_on);
    assert_ne!(machine.backend_created_on, Some(thread::current().id()));
}
#[tokio::test]
async fn protocol_fixtures_generation_removal_identity_and_failed_scan() {
    let (client, machine) = machine();
    let first = client.enumerate().await.unwrap().endpoints[0].token;
    assert_eq!(client.enumerate().await.unwrap().endpoints[0].token, first);
    machine.lock().unwrap().endpoints[0].identity = Some("identity-b".into());
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
        .push(fake_endpoint("native-path-never-exported", None));
    let reconnected = client.enumerate().await.unwrap().endpoints[0].token;
    assert!(reconnected.generation > changed.generation);
    machine.lock().unwrap().scan_failure = Some(error(ErrorCode::PermissionDenied));
    assert_eq!(
        client.enumerate().await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    machine.lock().unwrap().scan_failure = None;
    let after_error = client.enumerate().await.unwrap().endpoints[0].token;
    assert!(after_error.generation > reconnected.generation);
    client.shutdown(deadline()).await.unwrap();
}

#[tokio::test]
async fn protocol_fixtures_owner_rejects_endpoint_inventory_over_limit() {
    let state = Arc::new(Mutex::new(FakeState {
        endpoints: (0..257)
            .map(|index| fake_endpoint(&format!("endpoint-{index}"), None))
            .collect(),
        ..FakeState::default()
    }));
    let owner_state = state.clone();
    let client = DdcClient::start_with_backend(move || Ok(FakeBackend::new(owner_state))).unwrap();
    assert_eq!(client.enumerate().await.unwrap_err().code, ErrorCode::Busy);
    client.shutdown(deadline()).await.unwrap();
}

#[tokio::test]
async fn protocol_fixtures_owner_rejects_endpoint_history_over_limit() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    let owner_state = state.clone();
    let client = DdcClient::start_with_backend(move || Ok(FakeBackend::new(owner_state))).unwrap();
    for batch in 0..16 {
        state.lock().unwrap().endpoints = (0..256)
            .map(|index| fake_endpoint(&format!("endpoint-{batch}-{index}"), None))
            .collect();
        assert_eq!(client.enumerate().await.unwrap().endpoints.len(), 256);
    }
    state.lock().unwrap().endpoints = vec![fake_endpoint("history-overflow", None)];
    assert_eq!(client.enumerate().await.unwrap_err().code, ErrorCode::Busy);
    client.shutdown(deadline()).await.unwrap();
}

#[tokio::test]
async fn protocol_fixtures_factory_error_is_reported_to_every_request() {
    let factory_error = error(ErrorCode::PermissionDenied);
    let client =
        DdcClient::start_with_backend::<FakeBackend, _>(move || Err(factory_error)).unwrap();
    for _ in 0..3 {
        assert_eq!(
            client.enumerate().await.unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }
    let endpoint = EndpointToken {
        id: u64::MAX,
        generation: 1,
    };
    let grant = grant(endpoint);
    let read = transaction(&grant, 0xffff).await;
    assert_eq!(
        client
            .get_vcp(read.clone(), endpoint, 0x10)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    read.finish();
    let write = transaction(&grant, 0xffff).await;
    assert_eq!(
        client
            .set_vcp(write.clone(), endpoint, 0x10, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    write.finish();
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
    assert!(set_pairs(&machine).is_empty());
    machine.lock().unwrap().coercion.insert(0xd7, 0x0230);
    let observed = client
        .set_vcp(context.clone(), endpoint, 0xd7, 0x0220)
        .await
        .unwrap();
    assert_eq!(observed.current, 0x0230);
    assert_eq!(context.outcomes()[0].observed, Some((0x0230, 0x0231)));
    assert_eq!(set_pairs(&machine), [(0xd7, 0x0220)]);
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
    assert!(set_pairs(&machine).is_empty());
    machine.lock().unwrap().fail_reads.insert(0x10);
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
    machine.lock().unwrap().fail_reads.remove(&0x10);
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
    assert!(set_pairs(&machine).is_empty());
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
    machine.lock().unwrap().fail_writes.insert(0x10);
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
    assert_eq!(
        machine.lock().unwrap().endpoints[0].replies[&0x10].current,
        40
    );
    machine.lock().unwrap().fail_writes.remove(&0x10);
    client
        .get_vcp(context.clone(), endpoint, 0x10)
        .await
        .unwrap();
    machine.lock().unwrap().fail_reads.insert(0x10);
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
    assert_eq!(
        machine.lock().unwrap().endpoints[0].replies[&0x10].current,
        60
    );
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
    let barrier = block_next_set(&machine);
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    machine.lock().unwrap().coercion.insert(0x10, 55);
    let operation_client = client.clone();
    let operation_context = context.clone();
    let operation = tokio::spawn(async move {
        operation_client
            .set_vcp(operation_context, endpoint, 0x10, 60)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.wait_until_entered())
        .await
        .unwrap();
    operation.abort();
    let _ = operation.await;
    context.cancel();
    context.finish();
    assert_pending(transaction(&grant, 0xffff)).await;
    assert_eq!(
        client
            .shutdown(Instant::now() + Duration::from_millis(20))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    barrier.open();
    client.shutdown(deadline()).await.unwrap();
    let outcomes = context.outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].requested, 60);
    assert_eq!(outcomes[0].observed, Some((55, 100)));
    assert_eq!(outcomes[0].error.as_ref().unwrap().code, ErrorCode::Timeout);
    assert_eq!(set_pairs(&machine), [(0x10, 60)]);
    assert_eq!(get_codes(&machine), [0x10, 0x10]);
}

#[tokio::test]
async fn protocol_fixtures_queued_cancellation_before_first_native_set() {
    use std::{future::Future, task::Poll};
    let (client, machine) = machine();
    machine
        .lock()
        .unwrap()
        .endpoints
        .push(fake_endpoint("second-native-key", Some("identity-c")));
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
    let barrier = block_next_set(&machine);
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    let worker_client = client.clone();
    let worker_context = first_context.clone();
    let started =
        tokio::spawn(async move { worker_client.set_vcp(worker_context, first, 0x10, 50).await });
    tokio::time::timeout(Duration::from_secs(1), barrier.wait_until_entered())
        .await
        .unwrap();
    let mut queued = Box::pin(client.set_vcp(second_context.clone(), second, 0x10, 60));
    std::future::poll_fn(|cx| {
        assert!(queued.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(second_context.abandon());
    second_context.finish();
    barrier.open();
    assert_eq!(queued.await.unwrap_err().code, ErrorCode::Timeout);
    assert_eq!(started.await.unwrap().unwrap().current, 50);
    first_context.finish();
    client.shutdown(deadline()).await.unwrap();
    assert_eq!(set_pairs(&machine), [(0x10, 50)]);
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
    let barrier = block_next_set(&machine);
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    let worker_client = client.clone();
    let worker_context = context.clone();
    let started = tokio::spawn(async move {
        worker_client
            .set_vcp(worker_context, endpoint, 0x10, 50)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.wait_until_entered())
        .await
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
    barrier.open();
    assert_eq!(started.await.unwrap().unwrap().current, 50);
    client.shutdown(deadline()).await.unwrap();
    assert_eq!(
        client.enumerate().await.unwrap_err().code,
        ErrorCode::DeviceUnavailable
    );
    assert_eq!(context.outcomes()[0].observed, Some((50, 100)));
    assert_eq!(set_pairs(&machine), [(0x10, 50)]);
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
    let barrier = block_next_set(&machine);
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    let worker_client = client.clone();
    let worker_context = context.clone();
    let started = tokio::spawn(async move {
        worker_client
            .set_vcp(worker_context, endpoint, 0x10, 50)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.wait_until_entered())
        .await
        .unwrap();
    started.abort();
    let _ = started.await;
    assert!(!context.abandon());
    barrier.open();
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
    let barrier = block_next_set(&machine);
    let _release_on_panic = ReleaseOnDrop(barrier.clone());
    let worker_client = client.clone();
    let worker_context = context.clone();
    let started = tokio::spawn(async move {
        worker_client
            .set_vcp(worker_context, endpoint, 0x10, 50)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.wait_until_entered())
        .await
        .unwrap();
    assert_eq!(started.await.unwrap().unwrap_err().code, ErrorCode::Timeout);
    context.finish();
    assert!(context.outcomes().is_empty());
    assert_pending(transaction(&grant, 0xffff)).await;
    barrier.open();
    client.shutdown(deadline()).await.unwrap();
    assert_eq!(context.outcomes()[0].observed, Some((50, 100)));
    assert_eq!(
        context.outcomes()[0].error.as_ref().unwrap().code,
        ErrorCode::Timeout
    );
    assert_eq!(get_codes(&machine), [0x10, 0x10]);
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
    assert!(set_pairs(&machine).is_empty());
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
    assert_eq!(set_pairs(&machine), [(0x10, 50)]);
    context.finish();
    client.shutdown(deadline()).await.unwrap();
}
#[tokio::test]
async fn last_scan_returns_current_cache_and_scan_errors_without_extra_scan() {
    let (client, machine) = machine();
    assert!(client.last_scan().await.unwrap().is_none());
    assert_eq!(machine.lock().unwrap().scan_calls, 0);

    let scan = client.enumerate().await.unwrap();
    assert_eq!(machine.lock().unwrap().scan_calls, 1);
    let cached = client.last_scan().await.unwrap().unwrap();
    assert_eq!(cached.endpoints.len(), scan.endpoints.len());
    assert_eq!(cached.endpoints[0].0, scan.endpoints[0]);
    assert!(cached.endpoints[0].1.is_none());
    assert!(cached.error.is_none());

    machine.lock().unwrap().scan_errors = vec![error(ErrorCode::PermissionDenied)];
    assert_eq!(client.enumerate().await.unwrap().errors.len(), 1);
    let partial = client.last_scan().await.unwrap().unwrap();
    assert_eq!(partial.endpoints.len(), 1);
    assert_eq!(partial.error.unwrap().code, ErrorCode::PermissionDenied);

    machine.lock().unwrap().scan_failure = Some(error(ErrorCode::PermissionDenied));
    assert_eq!(
        client.enumerate().await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        client.last_scan().await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );

    {
        let mut state = machine.lock().unwrap();
        state.scan_failure = None;
        state.scan_errors.clear();
    }
    let recovered = client.enumerate().await.unwrap();
    assert_eq!(
        client.last_scan().await.unwrap().unwrap().endpoints[0].0,
        recovered.endpoints[0]
    );

    machine.lock().unwrap().endpoints = vec![
        fake_endpoint("duplicate", None),
        fake_endpoint("duplicate", None),
    ];
    assert_eq!(
        client.enumerate().await.unwrap_err().code,
        ErrorCode::DdcError
    );
    assert_eq!(
        client.last_scan().await.unwrap_err().code,
        ErrorCode::DdcError
    );

    machine.lock().unwrap().endpoints = (0..257)
        .map(|index| fake_endpoint(&format!("endpoint-{index}"), None))
        .collect();
    assert_eq!(client.enumerate().await.unwrap_err().code, ErrorCode::Busy);
    assert_eq!(client.last_scan().await.unwrap_err().code, ErrorCode::Busy);
    client.shutdown(deadline()).await.unwrap();
}
