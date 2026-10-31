use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use synapsed_core::{EndpointGates, EndpointToken, ErrorCode, OperationState};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn pending<F: Future>(future: Pin<&mut F>) -> bool {
    matches!(
        future.poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    )
}

#[tokio::test]
async fn cancelled_before_first_write_cannot_admit_set() {
    let token = EndpointToken {
        id: 1,
        generation: 1,
    };
    let grant = EndpointGates::default()
        .activate("fixture", "declared-device", 7, vec![token])
        .unwrap();
    let operation = grant
        .begin(
            BTreeSet::from([16]),
            BTreeMap::from([(16, 0xffff)]),
            deadline(),
        )
        .await
        .unwrap();
    let native = operation.begin_native(token, 16, true).unwrap();
    assert!(operation.abandon());
    assert_eq!(operation.state(), OperationState::Cancelled);
    assert_eq!(
        operation.admit_write(token, 16).unwrap_err().code,
        ErrorCode::Timeout
    );
    operation.finish();
    drop(native);
}

#[tokio::test]
async fn client_disconnect_after_write_admission_does_not_cancel_confirmation() {
    let token = EndpointToken {
        id: 3,
        generation: 1,
    };
    let grant = EndpointGates::default()
        .activate("fixture", "device", 1, vec![token])
        .unwrap();
    let operation = grant
        .begin(
            BTreeSet::from([16]),
            BTreeMap::from([(16, 0xffff)]),
            deadline(),
        )
        .await
        .unwrap();
    let native = operation.begin_native(token, 16, true).unwrap();
    operation.admit_write(token, 16).unwrap();
    assert!(!operation.abandon());
    assert_eq!(operation.state(), OperationState::WriteStarted);
    assert!(operation.validate(token, 16, false).is_ok());
    drop(native);
    let confirming_read = operation.begin_native(token, 16, false).unwrap();
    drop(confirming_read);
    operation.finish();
    assert_eq!(operation.state(), OperationState::Completed);
}

#[tokio::test]
async fn started_native_work_holds_all_gates_after_abandonment_until_drain() {
    let tokens = vec![
        EndpointToken {
            id: 2,
            generation: 1,
        },
        EndpointToken {
            id: 1,
            generation: 1,
        },
    ];
    let gates = EndpointGates::default();
    let grant = gates
        .activate("fixture", "device", 1, tokens.clone())
        .unwrap();
    let operation = grant
        .begin(
            BTreeSet::from([16]),
            BTreeMap::from([(16, 0xffff)]),
            deadline(),
        )
        .await
        .unwrap();
    let native = operation.begin_native(tokens[0], 16, true).unwrap();
    operation.admit_write(tokens[0], 16).unwrap();
    assert_eq!(operation.state(), OperationState::WriteStarted);
    operation.cancel();
    operation.finish();
    assert_eq!(operation.state(), OperationState::WriteStarted);
    grant.revoke();
    let mut drained = Box::pin(grant.drain(deadline()));
    assert!(
        pending(drained.as_mut()),
        "Revoked owner drained before native work finished"
    );
    let successor = gates.activate("replacement", "device", 2, tokens).unwrap();
    let mut next = Box::pin(successor.begin(BTreeSet::from([16]), BTreeMap::new(), deadline()));
    assert!(
        pending(next.as_mut()),
        "Started native operation released its endpoint gates too early"
    );
    drop(native);
    drained.await.unwrap();
    assert_eq!(operation.state(), OperationState::Completed);
    let next = next.await.unwrap();
    next.finish();
}

#[tokio::test]
async fn stale_epochs_generations_and_ungranted_registers_are_rejected() {
    let token = EndpointToken {
        id: 1,
        generation: 2,
    };
    let grant = EndpointGates::default()
        .activate("fixture", "device", 3, vec![token])
        .unwrap();
    let operation = grant
        .begin(
            BTreeSet::from([16]),
            BTreeMap::from([(16, 0xff)]),
            deadline(),
        )
        .await
        .unwrap();
    assert_eq!(
        operation
            .validate(
                EndpointToken {
                    generation: 1,
                    ..token
                },
                16,
                false
            )
            .unwrap_err()
            .code,
        ErrorCode::DeviceUnavailable
    );
    assert_eq!(
        operation.validate(token, 25, false).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(operation.change_mask(16), Some(0xff));
    grant.revoke();
    assert_eq!(
        operation.validate(token, 16, true).unwrap_err().code,
        ErrorCode::DeviceUnavailable
    );
    operation.finish();
    assert!(
        grant
            .begin(BTreeSet::new(), BTreeMap::new(), deadline())
            .await
            .is_err()
    );
}
