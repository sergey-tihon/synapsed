use synapsed_api::*;

fn node() -> NodeInfo {
    NodeInfo {
        node_id: "a5bd64ea-1a0d-4d5e-9858-540f8f8fd9db".into(),
        instance_id: "3e0d5662-fbe7-4e50-a134-560b6278bfb2".into(),
        name: "Local node".into(),
    }
}
fn number(integer: bool, step: Option<f64>) -> Characteristic {
    Characteristic {
        id: "measurement".into(),
        label: "Measurement".into(),
        group: None,
        spec: CharacteristicSpec::Number {
            minimum: Some(0.0),
            maximum: Some(10.0),
            step,
            integer,
            unit: Some("cm".into()),
        },
        access: CharacteristicAccess {
            read: true,
            write: true,
            notify: true,
        },
        value: Some(CharacteristicValue::Number(2.0)),
        status: CharacteristicStatus::Ok,
        error: None,
    }
}
fn device() -> Device {
    Device {
        id: "desk".into(),
        driver: "fixture".into(),
        kind: "measurement".into(),
        label: "Desk".into(),
        status: DeviceStatus::Online,
        error: None,
        groups: vec![],
        characteristics: vec![number(false, Some(0.25))],
    }
}
fn snapshot() -> Snapshot {
    Snapshot {
        api_version: 1,
        node: node(),
        revision: "0".into(),
        drivers: vec![DriverStatus {
            id: "fixture".into(),
            version: "0.1.0".into(),
            state: DriverState::Online,
            error: None,
        }],
        devices: vec![device()],
    }
}

#[test]
fn identity_sequences_and_timestamps_have_real_boundaries() {
    assert!(node().validate().is_ok());
    for bad in [
        "",
        "00",
        "01",
        "-1",
        "1.0",
        " 1",
        "1\n",
        "18446744073709551616",
    ] {
        assert!(parse_revision(bad).is_err(), "{bad:?}");
    }
    assert_eq!(parse_revision("18446744073709551615").unwrap(), u64::MAX);
    assert!(parse_revision("10").unwrap() > parse_revision("9").unwrap());
    assert!(validate_uuid_v4("a5bd64ea1a0d4d5e9858540f8f8fd9db").is_err());
    assert!(validate_uuid_v4("a5bd64ea-1a0d-3d5e-9858-540f8f8fd9db").is_err());
    assert!(validate_uuid_v4("A5BD64EA-1A0D-4D5E-9858-540F8F8FD9DB").is_err());
    assert!(validate_timestamp("2024-02-29T23:59:59.123Z").is_ok());
    assert!(validate_timestamp("2024-02-29T23:59:59+00:00").is_ok());
    for bad in [
        "2023-02-29T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-01T25:00:00Z",
        "2024-01-01T00:00:00+01:00",
        "2024-01-01",
    ] {
        assert!(validate_timestamp(bad).is_err(), "{bad}");
    }
    for id in ["a", "desk_height", "moonhalo.power", "driver-1"] {
        assert!(validate_id(id).is_ok());
    }
    for id in ["", "1desk", "Desk", "desk/height", "desk height", "é"] {
        assert!(validate_id(id).is_err());
    }
    assert!(validate_id(&format!("a{}", "b".repeat(63))).is_ok());
    assert!(validate_id(&format!("a{}", "b".repeat(64))).is_err());
    assert!(validate_driver_id("generic-ddc").is_ok());
    assert!(validate_driver_id("generic.ddc").is_err());
    for version in ["0.1.0", "12.100.300"] {
        assert!(validate_driver_version(version).is_ok());
    }
    for version in ["0.1", "0.1.0.0", "v0.1.0", "0.1.x", "0..0"] {
        assert!(validate_driver_version(version).is_err());
    }
}

#[test]
fn finite_fractional_and_integer_numeric_metadata_are_distinct() {
    let metric = number(false, Some(0.25));
    assert!(metric.validate().is_ok());
    assert!(
        metric
            .validate_command(&CharacteristicCommand::Set {
                value: CharacteristicValue::Number(2.75)
            })
            .is_ok()
    );
    let integer = number(true, Some(1.0));
    assert!(
        integer
            .validate_command(&CharacteristicCommand::Set {
                value: CharacteristicValue::Number(2.75)
            })
            .is_err()
    );
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(CharacteristicValue::Number(value).validate().is_err());
        let spec = CharacteristicSpec::Number {
            minimum: Some(value),
            maximum: None,
            step: None,
            integer: false,
            unit: None,
        };
        assert!(spec.validate().is_err());
    }
    for step in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        assert!(number(false, Some(step)).validate().is_err());
    }
    assert!(number(true, Some(0.5)).validate().is_err());
    let backwards = CharacteristicSpec::Number {
        minimum: Some(10.0),
        maximum: Some(0.0),
        step: None,
        integer: false,
        unit: None,
    };
    assert!(backwards.validate().is_err());
    let fractional_bound = CharacteristicSpec::Number {
        minimum: Some(0.5),
        maximum: None,
        step: Some(1.0),
        integer: true,
        unit: None,
    };
    assert!(fractional_bound.validate().is_err());
    assert!(
        metric
            .spec
            .validate_value(&CharacteristicValue::Number(10.0))
            .is_ok()
    );
    assert!(
        metric
            .spec
            .validate_value(&CharacteristicValue::Number(10.01))
            .is_err()
    );
    // A step is relative-command metadata, not a requirement that every observed
    // measurement be an exact multiple of the declared step.
    assert!(
        metric
            .spec
            .validate_value(&CharacteristicValue::Number(0.1))
            .is_ok()
    );
}

#[test]
fn unicode_strings_and_enum_options_are_bounded_without_byte_truncation() {
    assert!(validate_label(&"🌙".repeat(128)).is_ok());
    assert!(validate_label(&"🌙".repeat(129)).is_err());
    assert!(validate_string("", 0).is_ok());
    assert!(validate_string(&"🌙".repeat(1024), 1024).is_ok());
    assert!(validate_string(&"🌙".repeat(1025), 1024).is_err());
    assert!(validate_string("", 1025).is_err());
    let string = CharacteristicSpec::String { max_length: 3 };
    assert!(
        string
            .validate_value(&CharacteristicValue::String("🌙月a".into()))
            .is_ok()
    );
    assert!(
        string
            .validate_value(&CharacteristicValue::String("🌙月ab".into()))
            .is_err()
    );
    let option = EnumOption {
        value: "exact".into(),
        label: "Exact".into(),
    };
    assert!(
        CharacteristicSpec::Enum { options: vec![] }
            .validate()
            .is_err()
    );
    assert!(
        CharacteristicSpec::Enum {
            options: vec![option.clone(), option.clone()]
        }
        .validate()
        .is_err()
    );
    let options = (0..32)
        .map(|index| EnumOption {
            value: index.to_string(),
            label: index.to_string(),
        })
        .collect();
    assert!(CharacteristicSpec::Enum { options }.validate().is_ok());
    let options = (0..33)
        .map(|index| EnumOption {
            value: index.to_string(),
            label: index.to_string(),
        })
        .collect();
    assert!(CharacteristicSpec::Enum { options }.validate().is_err());
    let spec = CharacteristicSpec::Enum {
        options: vec![option],
    };
    assert!(
        spec.validate_value(&CharacteristicValue::String("exact".into()))
            .is_ok()
    );
    assert!(
        spec.validate_value(&CharacteristicValue::String("Exact".into()))
            .is_err()
    );
    assert!(
        spec.validate_value(&CharacteristicValue::Boolean(true))
            .is_err()
    );
}

#[test]
fn commands_are_strict_typed_bounded_and_metadata_driven() {
    for json in [
        r#"{"op":"toggle","value":true}"#,
        r#"{"op":"invoke","extra":0}"#,
        r#"{"op":"set","value":{}}"#,
        r#"{"op":"step","steps":1.5}"#,
        r#"{"op":"step","steps":2147483648}"#,
        r#"{"op":"set","value":1,"value":2}"#,
    ] {
        assert!(
            CharacteristicCommand::from_json(json.as_bytes()).is_err(),
            "{json}"
        );
    }
    for steps in [-100, 0, 100] {
        assert!(CharacteristicCommand::Step { steps }.validate().is_ok());
    }
    for steps in [-101, 101, i32::MIN, i32::MAX] {
        assert!(CharacteristicCommand::Step { steps }.validate().is_err());
    }
    let mut bytes = br#"{"op":"toggle"}"#.to_vec();
    bytes.resize(MAX_COMMAND_BYTES, b' ');
    assert!(CharacteristicCommand::from_json(&bytes).is_ok());
    bytes.push(b' ');
    assert_eq!(
        CharacteristicCommand::from_json(&bytes).unwrap_err().code,
        ErrorCode::InvalidRequest
    );
    let no_step = number(false, None);
    assert_eq!(
        no_step
            .validate_command(&CharacteristicCommand::Step { steps: 0 })
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedOperation
    );
    let mut readonly = number(false, Some(0.25));
    readonly.access.write = false;
    assert!(
        readonly
            .validate_command(&CharacteristicCommand::Set {
                value: CharacteristicValue::Number(3.0)
            })
            .is_err()
    );
    let mut boolean = number(false, None);
    boolean.spec = CharacteristicSpec::Boolean;
    boolean.value = Some(CharacteristicValue::Boolean(false));
    assert!(
        boolean
            .validate_command(&CharacteristicCommand::Toggle {})
            .is_ok()
    );
    assert!(
        boolean
            .validate_command(&CharacteristicCommand::Invoke {})
            .is_err()
    );
    let mut action = boolean;
    action.spec = CharacteristicSpec::Action;
    action.access = CharacteristicAccess {
        read: false,
        write: true,
        notify: false,
    };
    action.value = None;
    assert!(
        action
            .validate_command(&CharacteristicCommand::Invoke {})
            .is_ok()
    );
    assert!(
        action
            .validate_command(&CharacteristicCommand::Set {
                value: CharacteristicValue::Boolean(true)
            })
            .is_err()
    );
    action.access.read = true;
    assert!(action.validate().is_err());
    action.access.read = false;
    action.value = Some(CharacteristicValue::Boolean(false));
    assert!(action.validate().is_err());
}

#[test]
fn first_observation_and_stale_values_are_not_manufactured() {
    let mut characteristic = number(false, Some(0.25));
    characteristic.value = None;
    assert!(characteristic.validate().is_err());
    characteristic.status = CharacteristicStatus::Unavailable;
    assert!(characteristic.validate().is_ok());
    characteristic.value = Some(CharacteristicValue::Number(99.0));
    assert!(characteristic.validate().is_ok());
    characteristic.status = CharacteristicStatus::Ok;
    assert!(characteristic.validate().is_err());
    characteristic.status = CharacteristicStatus::Error;
    characteristic.value = Some(CharacteristicValue::Number(f64::NAN));
    assert!(characteristic.validate().is_err());
    characteristic.value = None;
    characteristic.access.read = false;
    assert!(characteristic.validate().is_err()); // notify requires read
}

#[test]
fn snapshots_validate_unique_ownership_cardinality_and_encoded_size() {
    let mut state = snapshot();
    assert!(state.validate().is_ok());
    assert_eq!(
        state.validated_json().unwrap(),
        serde_json::to_vec(&state).unwrap()
    );
    state.api_version = 2;
    assert!(state.validate().is_err());
    state.api_version = 1;
    state.devices.push(device());
    assert!(state.validate().is_err());
    state.devices.pop();
    state.devices[0].driver = "missing".into();
    assert!(state.validate().is_err());
    state.devices[0].driver = "fixture".into();
    state.devices[0]
        .characteristics
        .push(number(false, Some(0.25)));
    assert!(state.validate().is_err());
    state.devices[0].characteristics.pop();
    state.drivers.push(state.drivers[0].clone());
    assert!(state.validate().is_err());
    state.drivers.pop();
    state.devices = (0..64)
        .map(|index| {
            let mut d = device();
            d.id = format!("device{index}");
            d
        })
        .collect();
    assert!(state.validate().is_ok());
    let mut extra = device();
    extra.id = "extra".into();
    state.devices.push(extra);
    assert!(state.validate().is_err());
    state.devices = (0..8)
        .map(|index| {
            let mut d = device();
            d.id = format!("device{index}");
            d.characteristics = (0..64)
                .map(|index| {
                    let mut c = number(false, Some(0.25));
                    c.id = format!("value{index}");
                    c
                })
                .collect();
            d
        })
        .collect();
    assert!(state.validate().is_ok()); // 512 total, 64 per device
    let mut extra = device();
    extra.id = "extra".into();
    state.devices.push(extra);
    assert!(state.validate().is_err()); // 513 total
    state.devices.pop();
    for d in &mut state.devices {
        for c in &mut d.characteristics {
            c.spec = CharacteristicSpec::String { max_length: 1024 };
            c.value = Some(CharacteristicValue::String("🌙".repeat(1024)));
        }
    }
    assert!(state.validate_structure().is_ok());
    assert!(state.validate().is_err());
    assert!(state.validated_json().is_err());
    let encoded = serde_json::to_vec(&42).unwrap();
    assert!(validate_encoded_size(&42, encoded.len()).is_ok());
    assert!(validate_encoded_size(&42, encoded.len() - 1).is_err());
}

#[test]
fn characteristic_groups_validate_references_bounds_and_wire_shape() {
    let ungrouped = device();
    assert!(ungrouped.validate().is_ok());

    let mut invalid = ungrouped.clone();
    invalid.characteristics[0].group = Some("general".into());
    assert_eq!(
        invalid.validate().unwrap_err().message,
        "Characteristic group requires declared device groups."
    );

    let mut invalid = ungrouped.clone();
    invalid.groups = vec![CharacteristicGroup {
        id: "general".into(),
        label: "General".into(),
    }];
    assert_eq!(
        invalid.validate().unwrap_err().message,
        "Every characteristic must reference a declared device group."
    );

    let mut invalid = ungrouped.clone();
    invalid.groups = vec![CharacteristicGroup {
        id: "general".into(),
        label: "General".into(),
    }];
    invalid.characteristics[0].group = Some("missing".into());
    assert_eq!(
        invalid.validate().unwrap_err().message,
        "Every characteristic must reference a declared device group."
    );

    let mut invalid = ungrouped.clone();
    invalid.groups = vec![
        CharacteristicGroup {
            id: "general".into(),
            label: "General".into(),
        },
        CharacteristicGroup {
            id: "general".into(),
            label: "Duplicate".into(),
        },
    ];
    assert_eq!(
        invalid.validate().unwrap_err().message,
        "Characteristic group IDs must be unique within a device."
    );

    let mut invalid = ungrouped.clone();
    invalid.groups = vec![CharacteristicGroup {
        id: "general".into(),
        label: "General".into(),
    }];
    invalid.characteristics.clear();
    assert_eq!(
        invalid.validate().unwrap_err().message,
        "Characteristic groups must not be empty."
    );

    let mut invalid = ungrouped.clone();
    invalid.groups = (0..17)
        .map(|index| CharacteristicGroup {
            id: format!("group{index}"),
            label: format!("Group {index}"),
        })
        .collect();
    assert_eq!(
        invalid.validate().unwrap_err().message,
        "Device has too many characteristic groups."
    );

    let mut grouped = device();
    grouped.groups = vec![
        CharacteristicGroup {
            id: "general".into(),
            label: "General".into(),
        },
        CharacteristicGroup {
            id: "sensors".into(),
            label: "Sensors".into(),
        },
    ];
    let mut first = number(false, Some(0.25));
    first.id = "sensor_first".into();
    first.group = Some("sensors".into());
    let mut second = number(false, Some(0.25));
    second.id = "control".into();
    second.group = Some("general".into());
    let mut third = number(false, Some(0.25));
    third.id = "sensor_second".into();
    third.group = Some("sensors".into());
    grouped.characteristics = vec![first, second, third];
    assert!(grouped.validate().is_ok());
    let sections = grouped.characteristic_sections();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].0.unwrap().id, "general");
    assert_eq!(sections[0].1, vec![1]);
    assert_eq!(sections[1].0.unwrap().id, "sensors");
    assert_eq!(sections[1].1, vec![0, 2]);

    let mut unmatched = grouped.clone();
    unmatched.characteristics[0].group = Some("missing".into());
    let sections = unmatched.characteristic_sections();
    assert_eq!(sections.len(), 3);
    assert_eq!(sections[2].0, None);
    assert_eq!(sections[2].1, vec![0]);

    let wire = serde_json::to_value(&grouped).unwrap();
    assert_eq!(
        wire["groups"],
        serde_json::json!([
            {"id":"general", "label":"General"},
            {"id":"sensors", "label":"Sensors"}
        ])
    );
    assert_eq!(wire["characteristics"][0]["group"], "sensors");
    assert_eq!(
        serde_json::to_value(&ungrouped).unwrap()["characteristics"][0]["group"],
        serde_json::Value::Null
    );
    assert_eq!(ungrouped.characteristic_sections()[0].0, None);
    assert_eq!(ungrouped.characteristic_sections()[0].1, vec![0]);
}

#[test]
fn log_ring_and_error_status_mappings_are_complete() {
    let entries: Vec<_> = (0..200)
        .map(|id| LogEntry {
            id: id.to_string(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            level: LogLevel::Info,
            target: "host".into(),
            message: "Observed state.".into(),
        })
        .collect();
    let mut response = LogsResponse {
        node: node(),
        entries,
    };
    assert!(response.validate().is_ok());
    response.entries.push(response.entries[0].clone());
    assert!(response.validate().is_err());
    response.entries.pop();
    response.entries[1].id = "0".into();
    assert!(response.validate().is_err());
    for (code, status) in [
        (ErrorCode::InvalidRequest, 400),
        (ErrorCode::ForbiddenOrigin, 403),
        (ErrorCode::ForbiddenHost, 403),
        (ErrorCode::UnknownDevice, 404),
        (ErrorCode::UnknownCharacteristic, 404),
        (ErrorCode::AmbiguousDevice, 409),
        (ErrorCode::DriverConflict, 409),
        (ErrorCode::UnsupportedOperation, 422),
        (ErrorCode::InvalidValue, 422),
        (ErrorCode::PermissionDenied, 503),
        (ErrorCode::DeviceUnavailable, 503),
        (ErrorCode::DdcError, 503),
        (ErrorCode::PluginError, 503),
        (ErrorCode::UnsupportedCapability, 503),
        (ErrorCode::Busy, 503),
        (ErrorCode::Timeout, 504),
    ] {
        assert_eq!(code.http_status(), status);
    }
    assert!(
        ApiError::new(ErrorCode::PluginError, "e".repeat(256))
            .validate()
            .is_ok()
    );
    assert!(
        ApiError::new(ErrorCode::PluginError, "e".repeat(257))
            .validate()
            .is_err()
    );
}

#[test]
fn exact_wire_names_and_response_additions_survive_round_trip() {
    let state = snapshot();
    let mut json = serde_json::to_value(&state).unwrap();
    assert_eq!(json["apiVersion"], 1);
    assert!(json.get("api_version").is_none());
    assert!(json["node"].get("nodeId").is_some());
    json.as_object_mut()
        .unwrap()
        .insert("futureField".into(), true.into());
    assert_eq!(serde_json::from_value::<Snapshot>(json).unwrap(), state);
    let string = serde_json::to_value(CharacteristicSpec::String { max_length: 5 }).unwrap();
    assert_eq!(string, serde_json::json!({"kind":"string", "maxLength":5}));
    assert_eq!(
        serde_json::to_value(CharacteristicCommand::Step { steps: -3 }).unwrap(),
        serde_json::json!({"op":"step", "steps":-3})
    );
    assert_eq!(
        serde_json::to_value(ServerEvent::Log {
            entry: LogEntry {
                id: "1".into(),
                timestamp: "2024-01-01T00:00:00Z".into(),
                level: LogLevel::Warn,
                target: "host".into(),
                message: "Warning.".into()
            }
        })
        .unwrap()["type"],
        "log"
    );
    assert!(serde_json::from_value::<StatusResponse>(serde_json::json!({"apiVersion":1,"node":node(),"revision":"0","uptimeSeconds":4294967296_u64,"drivers":[]})).is_err());
}

#[test]
fn characteristic_changed_event_uses_camel_case_wire_shape() {
    let event = ServerEvent::CharacteristicChanged {
        change: CharacteristicChange {
            revision: "42".into(),
            timestamp: "2026-11-01T09:08:29.123Z".into(),
            device_id: "monitor".into(),
            characteristic_id: "ambient_light".into(),
            value: Some(CharacteristicValue::Number(26.0)),
            status: CharacteristicStatus::Ok,
            error: None,
        },
    };
    event.validate().unwrap();
    assert_eq!(
        serde_json::to_value(event).unwrap(),
        serde_json::json!({
            "type": "characteristic_changed",
            "change": {
                "revision": "42",
                "timestamp": "2026-11-01T09:08:29.123Z",
                "deviceId": "monitor",
                "characteristicId": "ambient_light",
                "value": 26.0,
                "status": "ok",
                "error": null
            }
        })
    );
}
