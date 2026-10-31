mod support;
use support::*;
use synapsed_api::{CharacteristicCommand, CharacteristicStatus, CharacteristicValue};
use synapsed_core::ErrorCode;

#[tokio::test(flavor = "current_thread")]
async fn moonhalo_packed_fields_independent_fixtures_and_confirmed_coercion() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
        })
        .await;
    for (raw, id, command, expected) in [
        (
            0x0120,
            "moonhalo.power",
            CharacteristicCommand::Toggle {},
            0x0110,
        ),
        (
            0x0131,
            "moonhalo.power",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Boolean(false),
            },
            0x0111,
        ),
        (
            0x0120,
            "moonhalo.light_mode",
            CharacteristicCommand::Step { steps: 1 },
            0x0220,
        ),
        (
            0x0220,
            "moonhalo.light_mode",
            CharacteristicCommand::Step { steps: -1 },
            0x0120,
        ),
    ] {
        harness.hardware.mutate(215, raw);
        harness.host.dispatch("monitor", id, command).await.unwrap();
        assert_eq!(harness.hardware.current(215), expected);
    }
    for (raw, id, steps, expected) in [
        (0x0305, "moonhalo.brightness", 1, 0x0306),
        (0x0305, "moonhalo.color_temperature", 1, 0x0405),
        (0x0405, "moonhalo.color_temperature", -1, 0x0305),
    ] {
        harness.hardware.mutate(217, raw);
        harness
            .host
            .dispatch("monitor", id, CharacteristicCommand::Step { steps })
            .await
            .unwrap();
        assert_eq!(harness.hardware.current(217), expected);
    }
    harness
        .hardware
        .state
        .lock()
        .unwrap()
        .coercion
        .insert(16, 57);
    let snapshot = harness
        .host
        .dispatch(
            "monitor",
            "brightness",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(60.0),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "brightness"),
        &CharacteristicValue::Number(57.0)
    );
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn moonhalo_packed_fields_failed_fresh_read_and_noops_never_set() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
        })
        .await;
    for command in [
        CharacteristicCommand::Step { steps: 0 },
        CharacteristicCommand::Set {
            value: CharacteristicValue::Number(40.0),
        },
    ] {
        harness
            .host
            .dispatch("monitor", "brightness", command)
            .await
            .unwrap();
    }
    harness.hardware.mutate(16, 100);
    harness
        .host
        .dispatch(
            "monitor",
            "brightness",
            CharacteristicCommand::Step { steps: 100 },
        )
        .await
        .unwrap();
    assert!(harness.hardware.state.lock().unwrap().set_calls.is_empty());
    harness
        .hardware
        .state
        .lock()
        .unwrap()
        .fail_reads
        .insert(217);
    let error = harness
        .host
        .dispatch(
            "monitor",
            "moonhalo.brightness",
            CharacteristicCommand::Step { steps: 1 },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DdcError);
    assert!(harness.hardware.state.lock().unwrap().set_calls.is_empty());
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn moonhalo_packed_fields_partial_vendor_error_preserves_live_brightness() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let harness = Harness::with_interval(
        packages.path(),
        Hardware::new(vec![Screen::benq()]),
        std::collections::BTreeMap::new(),
        5000,
    );
    harness
        .until(|snapshot| {
            snapshot
                .devices
                .first()
                .is_some_and(|device| device.driver == "benq-rd280u")
        })
        .await;
    harness
        .hardware
        .state
        .lock()
        .unwrap()
        .fail_reads
        .insert(217);
    harness.hardware.mutate(16, 55);
    let snapshot = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.characteristics.iter().any(|characteristic| {
                    characteristic.id == "brightness"
                        && characteristic.value == Some(CharacteristicValue::Number(55.0))
                })
            })
        })
        .await;
    let chars = &snapshot.devices[0].characteristics;
    assert_eq!(
        chars
            .iter()
            .find(|item| item.id == "brightness")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    assert_ne!(
        chars
            .iter()
            .find(|item| item.id == "moonhalo.brightness")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    assert_eq!(snapshot.devices[0].driver, "benq-rd280u");
    harness.stop().await;
}
