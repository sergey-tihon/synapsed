mod support;
use support::*;
use synapsed_api::{CharacteristicCommand, CharacteristicStatus, CharacteristicValue};
use synapsed_core::ErrorCode;

fn benq_snapshot(snapshot: &synapsed_api::Snapshot) -> bool {
    snapshot
        .devices
        .first()
        .is_some_and(|device| device.driver == "benq-rd280u")
}

#[tokio::test(flavor = "current_thread")]
async fn benq_groups_contrast_and_ambient_sensor_are_published() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    let snapshot = harness.until(benq_snapshot).await;
    let device = &snapshot.devices[0];
    assert_eq!(
        device
            .groups
            .iter()
            .map(|g| (g.id.as_str(), g.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("general", "General"),
            ("moonhalo", "MoonHalo"),
            ("sensors", "Sensors")
        ]
    );
    for (id, group) in [
        ("brightness", "general"),
        ("contrast", "general"),
        ("low_blue_light", "general"),
        ("moonhalo.power", "moonhalo"),
        ("moonhalo.brightness", "moonhalo"),
        ("moonhalo.color_temperature", "moonhalo"),
        ("moonhalo.light_mode", "moonhalo"),
        ("ambient_light", "sensors"),
    ] {
        assert_eq!(
            device
                .characteristics
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .group
                .as_deref(),
            Some(group),
            "{id}"
        );
    }
    let ambient = device
        .characteristics
        .iter()
        .find(|c| c.id == "ambient_light")
        .unwrap();
    assert!(!ambient.access.write);
    assert_eq!(ambient.status, CharacteristicStatus::Ok);
    assert_eq!(ambient.value, Some(CharacteristicValue::Number(80.0)));
    let contrast = device
        .characteristics
        .iter()
        .find(|c| c.id == "contrast")
        .unwrap();
    let (maximum, step) = match &contrast.spec {
        synapsed_api::CharacteristicSpec::Number { maximum, step, .. } => (*maximum, *step),
        _ => panic!("contrast must be numeric"),
    };
    assert_eq!(maximum, Some(100.0));
    assert_eq!(step, Some(5.0));
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn benq_contrast_writes_and_ambient_is_read_only_and_updates() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(benq_snapshot).await;
    harness
        .host
        .dispatch(
            "monitor",
            "contrast",
            CharacteristicCommand::Step { steps: 1 },
        )
        .await
        .unwrap();
    assert_eq!(harness.hardware.current(18), 55);
    harness
        .host
        .dispatch(
            "monitor",
            "contrast",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(70.0),
            },
        )
        .await
        .unwrap();
    assert_eq!(harness.hardware.current(18), 70);
    let before = harness.hardware.state.lock().unwrap().set_calls.clone();
    assert_eq!(
        before.iter().map(|(_, code, _)| *code).collect::<Vec<_>>(),
        vec![18, 18]
    );
    let error = harness
        .host
        .dispatch(
            "monitor",
            "ambient_light",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(24.0),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedOperation);
    assert_eq!(harness.hardware.state.lock().unwrap().set_calls, before);
    harness.hardware.mutate(225, 24);
    let snapshot = harness
        .until(|s| {
            benq_snapshot(s) && value(s, "ambient_light") == &CharacteristicValue::Number(24.0)
        })
        .await;
    assert_eq!(
        value(&snapshot, "ambient_light"),
        &CharacteristicValue::Number(24.0)
    );
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn benq_ambient_range_failure_isolated_and_contrast_is_fingerprint() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(benq_snapshot).await;
    harness.hardware.state.lock().unwrap().endpoints[0]
        .replies
        .get_mut(&225)
        .unwrap()
        .maximum = 0xFFFE;
    let snapshot = harness
        .until(|s| {
            benq_snapshot(s)
                && s.devices[0]
                    .characteristics
                    .iter()
                    .find(|c| c.id == "ambient_light")
                    .is_some_and(|c| c.status == CharacteristicStatus::Error)
        })
        .await;
    let device = &snapshot.devices[0];
    assert_eq!(
        device
            .characteristics
            .iter()
            .find(|c| c.id == "brightness")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    harness.stop().await;

    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let mut screen = Screen::benq();
    screen.values.remove(&18);
    let harness = Harness::start(packages.path(), Hardware::new(vec![screen]));
    let snapshot = harness.until(|s| !s.devices.is_empty()).await;
    assert_eq!(snapshot.devices[0].driver, "generic-ddc");
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn benq_extends_the_generic_mccs_catalog() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let mut screen = Screen::mccs();
    screen.name = Some("BenQ RD280U".into());
    screen.values.extend(Screen::benq().values);
    let harness = Harness::start(packages.path(), Hardware::new(vec![screen]));
    let snapshot = harness.until(benq_snapshot).await;
    let device = &snapshot.devices[0];
    assert_eq!(
        device
            .characteristics
            .iter()
            .map(|characteristic| characteristic.id.as_str())
            .take(14)
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
        vec![
            "general", "moonhalo", "sensors", "picture", "color", "audio", "system"
        ]
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
    assert_eq!(
        value(&snapshot, "moonhalo.power"),
        &CharacteristicValue::Boolean(true)
    );
    assert_eq!(
        value(&snapshot, "ambient_light"),
        &CharacteristicValue::Number(80.0)
    );
    let get_count = harness.hardware.state.lock().unwrap().get_calls.len();
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
    {
        let state = harness.hardware.state.lock().unwrap();
        let (screen, code, raw) = state.set_calls.last().unwrap();
        assert_eq!((screen.as_str(), *code, *raw), ("screen-a", 0x62, 25));
        assert!(
            state
                .get_calls
                .iter()
                .skip(get_count)
                .all(|call| call.1 == 0x62)
        );
    }
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn benq_retries_transiently_unavailable_optional_features() {
    let packages = catalog(&["generic-ddc", "benq-rd280u"]);
    let mut screen = Screen::mccs();
    screen.name = Some("BenQ RD280U".into());
    screen.values.extend(Screen::benq().values);
    let hardware = Hardware::new(vec![screen]);
    hardware.state.lock().unwrap().fail_reads.insert(0x8D);
    let harness = Harness::with_interval(
        packages.path(),
        hardware.clone(),
        Default::default(),
        60_000,
    );
    let snapshot = harness.until(benq_snapshot).await;
    assert!(
        !snapshot.devices[0]
            .characteristics
            .iter()
            .any(|characteristic| characteristic.id == "mute")
    );
    let get_count = || {
        hardware
            .state
            .lock()
            .unwrap()
            .get_calls
            .iter()
            .filter(|call| call.1 == 0x8D)
            .count()
    };
    assert_eq!(get_count(), 4);

    hardware.state.lock().unwrap().fail_reads.remove(&0x8D);
    harness
        .host
        .complete_discovery_round_for_test()
        .await
        .unwrap();
    harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.driver == "benq-rd280u"
                    && device
                        .characteristics
                        .iter()
                        .any(|characteristic| characteristic.id == "mute")
            })
        })
        .await;
    assert_eq!(get_count(), 7);
    harness.stop().await;
}
