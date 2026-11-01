use synapsed_api::{
    Characteristic, CharacteristicAccess, CharacteristicSpec, CharacteristicStatus,
    CharacteristicValue, Device, DeviceStatus, DriverState, DriverStatus, NodeInfo,
};
use synapsed_core::StateStore;
fn store() -> StateStore {
    StateStore::new(NodeInfo {
        node_id: "00000000-0000-4000-8000-000000000001".into(),
        instance_id: "00000000-0000-4000-8000-000000000002".into(),
        name: "Isolated state".into(),
    })
    .unwrap()
}
fn report(value: f64) -> (DriverStatus, Vec<Device>) {
    (
        DriverStatus {
            id: "fixture".into(),
            version: "0.1.0".into(),
            state: DriverState::Online,
            error: None,
        },
        vec![Device {
            id: "ordinary.device".into(),
            driver: "fixture".into(),
            kind: "fractional-metric".into(),
            label: "Actual declaration".into(),
            status: DeviceStatus::Online,
            error: None,
            groups: vec![],
            characteristics: vec![Characteristic {
                id: "measurement".into(),
                label: "Measurement".into(),
                group: None,
                spec: CharacteristicSpec::Number {
                    minimum: None,
                    maximum: None,
                    step: None,
                    integer: false,
                    unit: Some("ratio".into()),
                },
                access: CharacteristicAccess {
                    read: true,
                    write: false,
                    notify: true,
                },
                value: Some(CharacteristicValue::Number(value)),
                status: CharacteristicStatus::Ok,
                error: None,
            }],
        }],
    )
}
#[test]
fn rejected_report_cannot_publish_partial_state_or_increment_revision() {
    let store = store();
    let first = store.publish_driver_states(vec![report(0.125)]).unwrap();
    assert_eq!(first.revision, "1");
    let mut bad = report(0.5);
    bad.1[0].characteristics[0].value = Some(CharacteristicValue::Number(f64::NAN));
    assert!(store.publish_driver_states(vec![bad]).is_err());
    assert_eq!(store.snapshot().as_ref(), first.as_ref());
    let unchanged = store.publish_driver_states(vec![report(0.125)]).unwrap();
    assert_eq!(unchanged.revision, "1");
    let changed = store.publish_driver_states(vec![report(0.75)]).unwrap();
    assert_eq!(changed.revision, "2");
    assert_eq!(
        changed.devices[0].characteristics[0].value,
        Some(CharacteristicValue::Number(0.75))
    );
}
#[test]
fn conflicting_ids_and_false_owner_reports_do_not_replace_valid_state() {
    let store = store();
    let first = store.publish_driver_states(vec![report(1.25)]).unwrap();
    let mut conflict = report(2.0);
    conflict.0.id = "competitor".into();
    conflict.1[0].driver = "competitor".into();
    assert!(store.publish_driver_states(vec![conflict]).is_err());
    assert_eq!(store.snapshot().as_ref(), first.as_ref());
    let (status, devices) = report(9.0);
    assert!(
        store
            .publish_driver_state("wrong-owner", status, devices)
            .is_err()
    );
    assert_eq!(store.snapshot().as_ref(), first.as_ref());
}

#[test]
fn near_limit_report_cannot_prevent_later_fault_publication() {
    use synapsed_api::{ApiError, ErrorCode, MAX_SNAPSHOT_BYTES, Snapshot};
    let store = store();
    let make_report = |length: usize| {
        let (status, prototype) = report(0.125);
        let devices = (0..8)
            .map(|device_index| {
                let mut device = prototype[0].clone();
                device.id = format!("device.{device_index}");
                device.characteristics = (0..64)
                    .map(|index| Characteristic {
                        id: format!("text.{index}"),
                        label: "Observed text".into(),
                        group: None,
                        spec: CharacteristicSpec::String { max_length: 1024 },
                        access: CharacteristicAccess {
                            read: true,
                            write: false,
                            notify: true,
                        },
                        value: Some(CharacteristicValue::String("x".repeat(length))),
                        status: CharacteristicStatus::Ok,
                        error: None,
                    })
                    .collect();
                device
            })
            .collect::<Vec<_>>();
        (status, devices)
    };
    let snapshot_for = |report: &(DriverStatus, Vec<Device>)| Snapshot {
        api_version: 1,
        node: store.snapshot().node.clone(),
        revision: "1".into(),
        drivers: vec![report.0.clone()],
        devices: report.1.clone(),
    };
    let largest_length = |with_headroom: bool| {
        let (mut low, mut high) = (0, 1025);
        while high - low > 1 {
            let middle = (low + high) / 2;
            let snapshot = snapshot_for(&make_report(middle));
            let fits = if with_headroom {
                StateStore::validate_snapshot(&snapshot).is_ok()
            } else {
                snapshot.validated_json().is_ok()
            };
            if fits {
                low = middle;
            } else {
                high = middle;
            }
        }
        low
    };
    let retained = make_report(largest_length(true));
    let first = store.publish_driver_states(vec![retained.clone()]).unwrap();
    let near_limit = make_report(largest_length(false));
    assert!(snapshot_for(&near_limit).validated_json().is_ok());
    assert!(store.publish_driver_states(vec![near_limit]).is_err());
    assert_eq!(store.snapshot().as_ref(), first.as_ref());

    let mut fault = retained;
    let error = ApiError::new(ErrorCode::UnsupportedCapability, StateStore::FAULT_MESSAGE);
    fault.0.state = DriverState::PermissionDenied;
    fault.0.error = Some(error.clone());
    for device in &mut fault.1 {
        device.status = DeviceStatus::Unavailable;
        device.error = Some(error.clone());
        for characteristic in &mut device.characteristics {
            characteristic.status = CharacteristicStatus::Unavailable;
            characteristic.error = Some(error.clone());
        }
    }
    let unavailable = store.publish_driver_states(vec![fault]).unwrap();
    assert_eq!(unavailable.revision, "2");
    assert!(store.encoded_snapshot().len() <= MAX_SNAPSHOT_BYTES);
    for (previous, current) in first.devices.iter().zip(&unavailable.devices) {
        assert_eq!(current.status, DeviceStatus::Unavailable);
        for (previous, current) in previous
            .characteristics
            .iter()
            .zip(&current.characteristics)
        {
            assert_eq!(current.value, previous.value);
            assert_eq!(current.status, CharacteristicStatus::Unavailable);
            assert_eq!(
                current.error.as_ref().unwrap().code,
                ErrorCode::UnsupportedCapability
            );
        }
    }
}
