mod support;
use std::{collections::BTreeMap, time::Duration};
use support::*;
use synapsed_api::{CharacteristicCommand, CharacteristicStatus, CharacteristicValue, DriverState};
use synapsed_core::ErrorCode;
fn settings(mode: &str) -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([("fault-fixture".into(), serde_json::json!({"mode":mode}))])
}

#[tokio::test(flavor = "current_thread")]
async fn lua_faults_exception_budget_memory_and_malformed_quarantine_once() {
    for mode in ["exception", "caught-budget", "memory", "malformed", "cycle"] {
        let packages = catalog(&["fault-fixture"]);
        let harness = Harness::settings(
            packages.path(),
            Hardware::new(vec![Screen::benq()]),
            settings(mode),
        );
        harness.until(|snapshot| !snapshot.devices.is_empty()).await;
        harness.hardware.mutate(16, 66);
        let snapshot = harness
            .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
            .await;
        assert_eq!(
            snapshot.devices[0].characteristics[0].status,
            CharacteristicStatus::Unavailable
        );
        assert!(
            !snapshot.drivers[0]
                .error
                .as_ref()
                .unwrap()
                .message
                .contains("secret")
        );
        let reads = harness.hardware.state.lock().unwrap().reads.len();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(harness.hardware.state.lock().unwrap().reads.len(), reads);
        harness.stop().await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn lua_faults_wrong_register_out_of_mask_and_caught_publish_denied() {
    for mode in [
        "wrong-register",
        "out-of-mask",
        "publish-write",
        "discovery-set",
    ] {
        let packages = catalog(&["fault-fixture"]);
        let harness = Harness::settings(
            packages.path(),
            Hardware::new(vec![Screen::benq()]),
            settings(mode),
        );
        if mode == "discovery-set" {
            harness
                .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
                .await;
        } else {
            harness.until(|snapshot| !snapshot.devices.is_empty()).await;
            assert!(
                harness
                    .host
                    .dispatch(
                        "fault",
                        "number",
                        CharacteristicCommand::Set {
                            value: CharacteristicValue::Number(51.0)
                        }
                    )
                    .await
                    .is_err()
            );
            harness
                .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
                .await;
        }
        assert!(harness.hardware.state.lock().unwrap().writes.is_empty());
        harness.stop().await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn lua_faults_cancellation_before_first_set_and_after_started_set() {
    for after_set in [false, true] {
        let packages = catalog(&["fault-fixture"]);
        let harness = Harness::settings(
            packages.path(),
            Hardware::new(vec![Screen::benq()]),
            settings("cancellation"),
        );
        harness.until(|snapshot| !snapshot.devices.is_empty()).await;
        let barrier = if after_set {
            &harness.hardware.set_barrier
        } else {
            &harness.hardware.get_barrier
        };
        barrier.arm();
        let _release = barrier.release_on_drop();
        let host = harness.host.clone();
        let request = tokio::spawn(async move {
            host.dispatch(
                "fault",
                "number",
                CharacteristicCommand::Set {
                    value: CharacteristicValue::Number(51.0),
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
            .await
            .unwrap();
        request.abort();
        let _ = request.await;
        barrier.open();
        if after_set {
            let snapshot = harness
                .until(|snapshot| {
                    snapshot.devices.first().is_some_and(|device| {
                        device.characteristics[0].value == Some(CharacteristicValue::Number(51.0))
                            && device.characteristics[0].status == CharacteristicStatus::Ok
                    })
                })
                .await;
            assert_eq!(
                value(&snapshot, "number"),
                &CharacteristicValue::Number(51.0)
            );
            assert_eq!(harness.hardware.current(16), 51);
        } else {
            harness
                .until(|snapshot| {
                    snapshot.devices.first().is_some_and(|device| {
                        device.status == synapsed_api::DeviceStatus::Unavailable
                    })
                })
                .await;
            assert!(harness.hardware.state.lock().unwrap().writes.is_empty());
        }
        harness.stop().await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn lua_faults_partial_native_failure_never_publishes_requested_string() {
    let packages = catalog(&["typed-fixture"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    harness.hardware.state.lock().unwrap().fail_set_number = Some(2);
    let error = harness
        .host
        .dispatch(
            "fixture",
            "text",
            CharacteristicCommand::Set {
                value: CharacteristicValue::String("abcdef".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PluginError);
    let snapshot = harness.store.snapshot();
    assert_eq!(
        value(&snapshot, "text"),
        &CharacteristicValue::String("".into())
    );
    assert_eq!(harness.hardware.current(37), 0x6162);
    assert_eq!(harness.hardware.current(38), 0);
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn lua_faults_quarantined_benq_recovers_only_through_fresh_generic_lua() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let entry = packages.path().join("benq-rd280u/driver.lua");
    transform_discovery(
        &entry,
        r#"local refresh = device.onRefresh
local endpoint = device.endpoints[1]
device.onRefresh = function(ctx)
  local sample = ctx.capabilities.ddc:get_vcp(endpoint, 16)
  if sample and sample.current == 66 then
    error("private fault")
  end
  return refresh(ctx)
end"#,
    );
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
        })
        .await;
    harness.hardware.mutate(16, 66);
    harness
        .until(|snapshot| {
            snapshot
                .drivers
                .iter()
                .any(|driver| driver.id == "benq-rd280u" && driver.state == DriverState::Error)
        })
        .await;
    let snapshot = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.driver == "generic-ddc"
                    && device.characteristics[0].value == Some(CharacteristicValue::Number(66.0))
            })
        })
        .await;
    assert_eq!(snapshot.devices[0].characteristics.len(), 1);
    let snapshot = harness
        .host
        .dispatch(
            "monitor",
            "brightness",
            CharacteristicCommand::Step { steps: 1 },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "brightness"),
        &CharacteristicValue::Number(71.0)
    );
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn lua_faults_timers_retire_with_device_epoch() {
    let packages = catalog(&["timer-fixture"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    harness.hardware.state.lock().unwrap().screens.clear();
    let snapshot = harness.until(|snapshot| snapshot.devices.is_empty()).await;
    let revision = snapshot.revision.clone();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(harness.store.snapshot().revision, revision);
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn lua_faults_deadline_drains_started_native_work_without_inferred_confirmation() {
    let packages = catalog(&["fault-fixture"]);
    let harness = Harness::settings(
        packages.path(),
        Hardware::new(vec![Screen::benq()]),
        settings("cancellation"),
    );
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    harness.hardware.set_barrier.arm();
    let _release = harness.hardware.set_barrier.release_on_drop();
    let host = harness.host.clone();
    let request = tokio::spawn(async move {
        host.dispatch(
            "fault",
            "number",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(51.0),
            },
        )
        .await
    });
    tokio::time::timeout(
        Duration::from_secs(2),
        harness.hardware.set_barrier.entered.notified(),
    )
    .await
    .unwrap();
    assert_eq!(request.await.unwrap().unwrap_err().code, ErrorCode::Timeout);
    let snapshot = harness
        .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
        .await;
    assert_eq!(
        value(&snapshot, "number"),
        &CharacteristicValue::Number(40.0)
    );
    assert_eq!(
        snapshot.devices[0].characteristics[0].status,
        CharacteristicStatus::Unavailable
    );
    harness.hardware.set_barrier.open();
    harness.stop().await;
    assert_eq!(harness.hardware.current(16), 51);
    assert_eq!(
        value(&harness.store.snapshot(), "number"),
        &CharacteristicValue::Number(40.0)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn lua_faults_partial_timer_recovery_does_not_restore_unobserved_fields() {
    let packages = catalog(&["timer-fixture"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    {
        let mut state = harness.hardware.state.lock().unwrap();
        state.fail_reads.insert(16);
        state.screens[0].values.get_mut(&32).unwrap().current = 177;
    }
    let recovered = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                let a = device
                    .characteristics
                    .iter()
                    .find(|item| item.id == "a")
                    .unwrap();
                let b = device
                    .characteristics
                    .iter()
                    .find(|item| item.id == "b")
                    .unwrap();
                a.status == CharacteristicStatus::Unavailable
                    && b.status == CharacteristicStatus::Ok
                    && b.value == Some(CharacteristicValue::Number(177.0))
            })
        })
        .await;
    assert_eq!(value(&recovered, "a"), &CharacteristicValue::Number(40.0));
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let snapshot = harness.store.snapshot();
    let a = snapshot.devices[0]
        .characteristics
        .iter()
        .find(|item| item.id == "a")
        .unwrap();
    assert_eq!(a.status, CharacteristicStatus::Unavailable);
    assert_eq!(a.value, Some(CharacteristicValue::Number(40.0)));
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn lua_faults_active_timer_and_queued_invoke_both_finish_and_remain_observed() {
    use std::{future::Future, task::Poll};
    let packages = catalog(&["typed-fixture"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    let first = harness
        .host
        .dispatch("fixture", "run", CharacteristicCommand::Invoke {})
        .await
        .unwrap();
    assert_eq!(
        value(&first, "invocations"),
        &CharacteristicValue::Number(1.0)
    );
    let before = harness
        .hardware
        .state
        .lock()
        .unwrap()
        .reads
        .iter()
        .filter(|code| **code == 32)
        .count();
    harness.hardware.get_barrier.arm();
    let _release = harness.hardware.get_barrier.release_on_drop();
    tokio::time::timeout(
        Duration::from_secs(2),
        harness.hardware.get_barrier.entered.notified(),
    )
    .await
    .unwrap();
    harness.hardware.mutate(32, 177);
    let mut request = Box::pin(harness.host.dispatch(
        "fixture",
        "run",
        CharacteristicCommand::Invoke {},
    ));
    // Poll exactly through the real bounded host mailbox admission while the native timer is paused.
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    harness.hardware.get_barrier.open();
    let second = request.await.unwrap();
    assert_eq!(
        value(&second, "invocations"),
        &CharacteristicValue::Number(2.0)
    );
    assert_eq!(
        value(&second, "temperature"),
        &CharacteristicValue::Number(17.7)
    );
    assert_eq!(harness.hardware.current(45), 2);
    assert!(
        harness
            .hardware
            .state
            .lock()
            .unwrap()
            .reads
            .iter()
            .filter(|code| **code == 32)
            .count()
            >= before + 2
    );
    harness.hardware.mutate(32, 191);
    let timer = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.characteristics.iter().any(|item| {
                    item.id == "temperature"
                        && item.status == CharacteristicStatus::Ok
                        && item.value == Some(CharacteristicValue::Number(19.1))
                })
            })
        })
        .await;
    assert_eq!(
        value(&timer, "invocations"),
        &CharacteristicValue::Number(2.0)
    );
    assert_eq!(timer.drivers[0].state, DriverState::Online);
    harness.stop().await;
}
