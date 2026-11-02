mod support;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use support::*;
use synapsed_api::{CharacteristicCommand, CharacteristicValue, DeviceStatus, DriverState};
use synapsed_core::{DriverError, ErrorCode};

const GENERIC_CODES: [u8; 14] = [
    0x10, 0x12, 0x14, 0x16, 0x18, 0x1A, 0x60, 0x62, 0x6C, 0x6E, 0x70, 0x87, 0x8D, 0xD6,
];
#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_zero_one_multiple_and_denied() {
    for count in [0, 1, 2] {
        let packages = catalog(&["generic-ddc"]);
        let screens = (0..count)
            .map(|index| {
                let mut screen = Screen::benq();
                screen.key = format!("screen-{index}");
                screen
            })
            .collect();
        let harness = Harness::start(packages.path(), Hardware::new(screens));
        let snapshot = harness
            .until(|snapshot| snapshot.drivers[0].state != DriverState::Disconnected)
            .await;
        if count == 1 {
            assert_eq!(snapshot.devices.len(), 1);
            assert_eq!(snapshot.devices[0].driver, "generic-ddc");
            assert_eq!(harness.hardware.current(16), 40);
        } else {
            assert!(snapshot.devices.is_empty());
        }
        assert!(
            harness
                .hardware
                .state
                .lock()
                .unwrap()
                .get_calls
                .iter()
                .all(|call| GENERIC_CODES.contains(&call.1))
        );
        harness.stop().await;
    }
    let packages = catalog(&["generic-ddc"]);
    let hardware = Hardware::new(vec![Screen::benq()]);
    hardware
        .state
        .lock()
        .unwrap()
        .scan_errors
        .push(DriverError::new(
            ErrorCode::PermissionDenied,
            "An eligible adapter is denied.",
        ));
    let harness = Harness::start(packages.path(), hardware);
    let snapshot = harness
        .until(|snapshot| snapshot.drivers[0].state == DriverState::PermissionDenied)
        .await;
    assert!(snapshot.devices.is_empty());
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_fingerprint_name_and_supersedes() {
    for (name, valid_maximum, expected) in [
        ("BenQ RD280U", true, "benq-rd280u"),
        ("  rd280u  ", true, "benq-rd280u"),
        ("Other monitor", true, "generic-ddc"),
        ("BenQ RD280U", false, "generic-ddc"),
    ] {
        let packages = catalog(&["generic-ddc", "benq-rd280u"]);
        let mut screen = Screen::benq();
        screen.name = Some(name.into());
        if !valid_maximum {
            screen.values.get_mut(&215).unwrap().maximum = 0x231 + 1;
        }
        let harness = Harness::start(packages.path(), Hardware::new(vec![screen]));
        let snapshot = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
        assert_eq!(snapshot.devices.len(), 1);
        assert_eq!(snapshot.devices[0].id, "monitor");
        assert_eq!(snapshot.devices[0].driver, expected);
        harness.stop().await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_conflict_never_activates_writer() {
    let packages = catalog(&["generic-ddc"]);
    let competing = packages.path().join("competing-ddc");
    copy_tree(&packages.path().join("generic-ddc"), &competing);
    let manifest = competing.join("plugin.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    value["id"] = "competing-ddc".into();
    std::fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    let snapshot = harness
        .until(|snapshot| {
            snapshot
                .drivers
                .iter()
                .all(|driver| driver.state == DriverState::Ambiguous)
        })
        .await;
    assert_eq!(snapshot.devices.len(), 1);
    assert_eq!(snapshot.devices[0].status, DeviceStatus::Unavailable);
    assert_eq!(
        snapshot.devices[0].error.as_ref().unwrap().code,
        ErrorCode::DriverConflict
    );
    let error = harness
        .host
        .dispatch(
            "monitor",
            "brightness",
            CharacteristicCommand::Step { steps: 1 },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DeviceUnavailable);
    assert!(harness.hardware.state.lock().unwrap().set_calls.is_empty());
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_same_identity_restores_but_changed_or_missing_does_not() {
    for changed in [false, true] {
        let packages = catalog(&["generic-ddc", "benq-rd280u"]);
        let hardware = Hardware::new(vec![Screen::benq()]);
        let harness = Harness::start(packages.path(), hardware.clone());
        harness
            .until(|snapshot| {
                snapshot.devices.first().is_some_and(|device| {
                    device.driver == "benq-rd280u" && device.status == DeviceStatus::Online
                })
            })
            .await;
        let old = hardware.state.lock().unwrap().endpoints.remove(0);
        harness.until(|snapshot| snapshot.devices.is_empty()).await;
        let mut new = old;
        if changed {
            new.identity = Some("b".repeat(64));
        }
        hardware.state.lock().unwrap().endpoints.push(new);
        let snapshot = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
        assert_eq!(
            snapshot.devices[0].status,
            if changed {
                DeviceStatus::Unavailable
            } else {
                DeviceStatus::Online
            }
        );
        harness.stop().await;
    }
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let mut screen = Screen::benq();
    screen.identity = None;
    let hardware = Hardware::new(vec![screen]);
    let harness = Harness::start(packages.path(), hardware.clone());
    harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
        })
        .await;
    let screen = hardware.state.lock().unwrap().endpoints.remove(0);
    harness.until(|snapshot| snapshot.devices.is_empty()).await;
    hardware.state.lock().unwrap().endpoints.push(screen);
    harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.status == DeviceStatus::Unavailable)
        })
        .await;
    harness
        .host
        .shutdown(Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
}

fn clone_claim_package(
    directory: &std::path::Path,
    id: &str,
    supersedes: &[&str],
    dependencies: &[&str],
) {
    let destination = directory.join(id);
    copy_tree(&directory.join("claim-fixture"), &destination);
    let path = destination.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["id"] = id.into();
    manifest["supersedes"] = serde_json::json!(supersedes);
    manifest["dependencies"] = serde_json::json!(dependencies);
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_transitive_superseder_is_the_only_activated_writer() {
    let packages = catalog(&["claim-fixture"]);
    clone_claim_package(
        packages.path(),
        "claim-b",
        &["claim-fixture"],
        &["claim-fixture"],
    );
    clone_claim_package(packages.path(), "claim-c", &["claim-b"], &["claim-b"]);
    let settings = BTreeMap::from([
        (
            "claim-fixture".into(),
            serde_json::json!({"failStart":true}),
        ),
        ("claim-b".into(), serde_json::json!({"failStart":true})),
    ]);
    let harness = Harness::settings(
        packages.path(),
        Hardware::new(vec![Screen::benq()]),
        settings,
    );
    let snapshot = harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.status == DeviceStatus::Online)
        })
        .await;
    assert_eq!(snapshot.devices.len(), 1);
    assert_eq!(snapshot.devices[0].driver, "claim-c");
    assert!(
        snapshot
            .drivers
            .iter()
            .all(|driver| driver.state == DriverState::Online)
    );
    let snapshot = harness
        .host
        .dispatch("claimed", "level", CharacteristicCommand::Step { steps: 1 })
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "level"),
        &CharacteristicValue::Number(41.0)
    );
    assert_eq!(harness.hardware.current(16), 41);
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_multi_endpoint_claim_never_gets_a_partial_grant() {
    let packages = catalog(&["claim-fixture"]);
    clone_claim_package(packages.path(), "claim-b", &["claim-fixture"], &[]);
    clone_claim_package(packages.path(), "claim-c", &[], &[]);
    let settings = BTreeMap::from([
        (
            "claim-fixture".into(),
            serde_json::json!({"deviceId":"pair","endpoints":[1,2],"failStart":true}),
        ),
        (
            "claim-b".into(),
            serde_json::json!({"deviceId":"left","endpoints":[1]}),
        ),
        (
            "claim-c".into(),
            serde_json::json!({"deviceId":"right","endpoints":[2],"failStart":true}),
        ),
    ]);
    let mut second = Screen::benq();
    second.key = "screen-b".into();
    second.identity = Some("b".repeat(64));
    let harness = Harness::settings(
        packages.path(),
        Hardware::new(vec![Screen::benq(), second]),
        settings,
    );
    let snapshot = harness.until(|snapshot| snapshot.devices.len() == 3).await;
    assert_eq!(
        snapshot
            .devices
            .iter()
            .filter(|device| device.status == DeviceStatus::Online)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .devices
            .iter()
            .find(|device| device.id == "left")
            .unwrap()
            .driver,
        "claim-b"
    );
    assert_eq!(
        harness
            .host
            .dispatch("pair", "level", CharacteristicCommand::Step { steps: 1 })
            .await
            .unwrap_err()
            .code,
        ErrorCode::DeviceUnavailable
    );
    harness
        .host
        .dispatch("left", "level", CharacteristicCommand::Step { steps: 1 })
        .await
        .unwrap();
    {
        let state = harness.hardware.state.lock().unwrap();
        assert_eq!(state.set_calls.len(), 1);
        assert_eq!(state.endpoints[1].replies[&16].current, 40);
    }
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_distinct_endpoints_with_same_public_id_conflict() {
    let packages = catalog(&["claim-fixture"]);
    clone_claim_package(packages.path(), "claim-b", &["claim-fixture"], &[]);
    let settings = BTreeMap::from([
        (
            "claim-fixture".into(),
            serde_json::json!({"endpoints":[1],"failStart":true}),
        ),
        (
            "claim-b".into(),
            serde_json::json!({"endpoints":[2],"failStart":true}),
        ),
    ]);
    let mut second = Screen::benq();
    second.key = "screen-b".into();
    second.identity = Some("b".repeat(64));
    let harness = Harness::settings(
        packages.path(),
        Hardware::new(vec![Screen::benq(), second]),
        settings,
    );
    let snapshot = harness
        .until(|snapshot| {
            snapshot
                .drivers
                .iter()
                .all(|driver| driver.state == DriverState::Ambiguous)
        })
        .await;
    assert_eq!(snapshot.devices.len(), 1);
    assert_eq!(snapshot.devices[0].status, DeviceStatus::Unavailable);
    assert_eq!(
        harness
            .host
            .dispatch("claimed", "level", CharacteristicCommand::Step { steps: 1 })
            .await
            .unwrap_err()
            .code,
        ErrorCode::DeviceUnavailable
    );
    assert!(harness.hardware.state.lock().unwrap().set_calls.is_empty());
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn monitor_selection_slot_rebinds_changed_identity_after_fresh_observation() {
    let packages = catalog(&["generic-ddc"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    let mut replacement = harness.hardware.state.lock().unwrap().endpoints.remove(0);
    harness.until(|snapshot| snapshot.devices.is_empty()).await;
    replacement.identity = Some("b".repeat(64));
    replacement.replies.get_mut(&16).unwrap().current = 55;
    harness
        .hardware
        .state
        .lock()
        .unwrap()
        .endpoints
        .push(replacement);
    let snapshot = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.status == DeviceStatus::Online
                    && device.characteristics[0].value == Some(CharacteristicValue::Number(55.0))
            })
        })
        .await;
    assert_eq!(snapshot.devices[0].driver, "generic-ddc");
    assert!(
        harness
            .hardware
            .state
            .lock()
            .unwrap()
            .get_calls
            .iter()
            .all(|call| GENERIC_CODES.contains(&call.1))
    );
    harness.stop().await;
}
