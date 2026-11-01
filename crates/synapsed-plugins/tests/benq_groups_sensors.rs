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
