mod support;
use support::*;
use synapsed_api::{
    CharacteristicCommand, CharacteristicStatus, CharacteristicValue, DeviceStatus, DriverState,
};
use synapsed_core::ErrorCode;

#[tokio::test(flavor = "current_thread")]
async fn generic_features_declare_supported_controls_and_groups() {
    let packages = catalog(&["generic-ddc"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::mccs()]));
    let snapshot = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    let device = &snapshot.devices[0];
    assert_eq!(device.driver, "generic-ddc");
    assert_eq!(
        device
            .characteristics
            .iter()
            .map(|characteristic| characteristic.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "brightness",
            "contrast",
            "sharpness",
            "color_preset",
            "red_gain",
            "green_gain",
            "blue_gain",
            "red_black_level",
            "green_black_level",
            "blue_black_level",
            "volume",
            "mute",
            "input_source",
            "power_mode",
        ]
    );
    assert_eq!(
        device
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<Vec<_>>(),
        vec!["picture", "color", "audio", "system"]
    );
    assert_eq!(
        value(&snapshot, "color_preset"),
        &CharacteristicValue::String("6500k".into())
    );
    assert_eq!(
        value(&snapshot, "mute"),
        &CharacteristicValue::Boolean(false)
    );
    assert_eq!(
        value(&snapshot, "input_source"),
        &CharacteristicValue::String("dp1".into())
    );
    assert_eq!(
        value(&snapshot, "power_mode"),
        &CharacteristicValue::String("on".into())
    );
    assert!(
        !device
            .characteristics
            .iter()
            .find(|characteristic| characteristic.id == "power_mode")
            .unwrap()
            .access
            .write
    );
    harness.stop().await;

    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    let snapshot = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    let device = &snapshot.devices[0];
    assert_eq!(device.driver, "generic-ddc");
    assert_eq!(
        device
            .characteristics
            .iter()
            .map(|characteristic| characteristic.id.as_str())
            .collect::<Vec<_>>(),
        vec!["brightness", "contrast"]
    );
    assert_eq!(
        device
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<Vec<_>>(),
        vec!["picture"]
    );
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn generic_features_write_and_confirm_only_the_target() {
    let packages = catalog(&["generic-ddc"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::mccs()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;

    let get_count = harness.hardware.state.lock().unwrap().get_calls.len();
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
        &CharacteristicValue::Number(60.0)
    );
    assert!(
        harness
            .hardware
            .state
            .lock()
            .unwrap()
            .get_calls
            .iter()
            .skip(get_count)
            .all(|call| call.1 == 0x10)
    );

    let snapshot = harness
        .host
        .dispatch("monitor", "mute", CharacteristicCommand::Toggle {})
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "mute"),
        &CharacteristicValue::Boolean(true)
    );
    {
        let state = harness.hardware.state.lock().unwrap();
        let (screen, code, raw) = state.set_calls.last().unwrap();
        assert_eq!((screen.as_str(), *code, *raw), ("screen-a", 0x8D, 0x0101));
    }

    let snapshot = harness
        .host
        .dispatch(
            "monitor",
            "input_source",
            CharacteristicCommand::Set {
                value: CharacteristicValue::String("hdmi1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "input_source"),
        &CharacteristicValue::String("hdmi1".into())
    );
    {
        let state = harness.hardware.state.lock().unwrap();
        let (screen, code, raw) = state.set_calls.last().unwrap();
        assert_eq!((screen.as_str(), *code, *raw), ("screen-a", 0x60, 0x0111));
    }

    let snapshot = harness
        .host
        .dispatch(
            "monitor",
            "volume",
            CharacteristicCommand::Step { steps: -1 },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "volume"),
        &CharacteristicValue::Number(25.0)
    );

    let set_count = harness.hardware.state.lock().unwrap().set_calls.len();
    let error = harness
        .host
        .dispatch(
            "monitor",
            "input_source",
            CharacteristicCommand::Step { steps: 1 },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedOperation);
    assert_eq!(
        harness.hardware.state.lock().unwrap().set_calls.len(),
        set_count
    );
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn generic_features_unmatched_table_value_does_not_quarantine_driver() {
    let packages = catalog(&["generic-ddc"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::mccs()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    harness.hardware.mutate(0x60, 0x001B);
    let snapshot = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.characteristics.iter().any(|characteristic| {
                    characteristic.id == "input_source"
                        && characteristic.status == CharacteristicStatus::Error
                })
            })
        })
        .await;
    let device = &snapshot.devices[0];
    assert_eq!(device.status, DeviceStatus::Online);
    assert_eq!(
        device
            .characteristics
            .iter()
            .find(|characteristic| characteristic.id == "brightness")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    assert_ne!(snapshot.drivers[0].state, DriverState::Error);
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn generic_features_cache_probes_for_identified_monitor() {
    let packages = catalog(&["generic-ddc"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    let get_count = || {
        harness
            .hardware
            .state
            .lock()
            .unwrap()
            .get_calls
            .iter()
            .filter(|call| call.1 == 0x8D)
            .count()
    };
    assert_eq!(get_count(), 2);
    for _ in 0..2 {
        harness
            .host
            .complete_discovery_round_for_test()
            .await
            .unwrap();
    }
    assert_eq!(get_count(), 2);
    harness.stop().await;
}
