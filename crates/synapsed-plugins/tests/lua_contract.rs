mod support;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use support::*;
use synapsed_api::{
    CharacteristicCommand, CharacteristicStatus, CharacteristicValue, DeviceStatus, DriverState,
    MAX_SNAPSHOT_BYTES,
};
use synapsed_core::{ErrorCode, StateStore};
use synapsed_plugins::{CapabilityRegistry, capabilities::ddc::DdcCapabilityFactory};

#[tokio::test(flavor = "current_thread")]
async fn lua_contract_real_fractional_string_boolean_enum_action_and_timers() {
    let packages = catalog(&["typed-fixture"]);
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    let initial = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    assert_eq!(initial.devices[0].kind, "instrument");
    assert_eq!(
        value(&initial, "temperature"),
        &CharacteristicValue::Number(12.5)
    );
    assert_eq!(
        initial.devices[0]
            .characteristics
            .iter()
            .find(|item| item.id == "run")
            .unwrap()
            .status,
        CharacteristicStatus::Ok
    );
    assert_eq!(
        harness
            .host
            .dispatch(
                "fixture",
                "temperature",
                CharacteristicCommand::Set {
                    value: CharacteristicValue::Number(1.0)
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedOperation
    );
    let snapshot = harness
        .host
        .dispatch(
            "fixture",
            "measurement",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(3.7),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "measurement"),
        &CharacteristicValue::Number(3.7)
    );
    assert_eq!(harness.hardware.current(33), 37);
    let snapshot = harness
        .host
        .dispatch("fixture", "switch", CharacteristicCommand::Toggle {})
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "switch"),
        &CharacteristicValue::Boolean(true)
    );
    let snapshot = harness
        .host
        .dispatch(
            "fixture",
            "choice",
            CharacteristicCommand::Step { steps: 1 },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "choice"),
        &CharacteristicValue::String("second".into())
    );
    for text in ["hello", "", "Café", "𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀𐐀"]
    {
        let snapshot = harness
            .host
            .dispatch(
                "fixture",
                "text",
                CharacteristicCommand::Set {
                    value: CharacteristicValue::String(text.into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            value(&snapshot, "text"),
            &CharacteristicValue::String(text.into())
        );
    }
    let snapshot = harness
        .host
        .dispatch("fixture", "run", CharacteristicCommand::Invoke {})
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "invocations"),
        &CharacteristicValue::Number(1.0)
    );
    assert_eq!(harness.hardware.current(45), 1);
    harness.hardware.mutate(32, 177);
    let snapshot = harness
        .until(|snapshot| {
            snapshot.devices.first().is_some_and(|device| {
                device.characteristics.iter().any(|characteristic| {
                    characteristic.id == "temperature"
                        && characteristic.value == Some(CharacteristicValue::Number(17.7))
                })
            })
        })
        .await;
    assert_eq!(
        value(&snapshot, "temperature"),
        &CharacteristicValue::Number(17.7)
    );
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_sources_are_frozen_and_restart_loads_new_behavior() {
    let packages = catalog(&["typed-fixture"]);
    let hardware = Hardware::new(vec![Screen::benq()]);
    let harness = Harness::start(packages.path(), hardware.clone());
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    let entry = packages.path().join("typed-fixture/driver.lua");
    transform_discovery(
        &entry,
        r#"local refresh = device.onRefresh
device.onRefresh = function(ctx)
  local result = refresh(ctx)
  for _, update in ipairs(result.updates or {}) do
    if update.id == "measurement" and update.value ~= nil then
      update.value = update.value * 2
    end
  end
  return result
end"#,
    );
    let snapshot = harness
        .host
        .dispatch(
            "fixture",
            "measurement",
            CharacteristicCommand::Set {
                value: CharacteristicValue::Number(4.0),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        value(&snapshot, "measurement"),
        &CharacteristicValue::Number(4.0)
    );
    harness.stop().await;
    let restarted = Harness::start(packages.path(), hardware);
    let snapshot = restarted
        .until(|snapshot| !snapshot.devices.is_empty())
        .await;
    assert_eq!(
        value(&snapshot, "measurement"),
        &CharacteristicValue::Number(8.0)
    );
    restarted.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_malformed_package_isolated_and_manifest_strict() {
    for mutation in ["unknown", "capability", "dependency", "cycle"] {
        let packages = catalog(&["generic-ddc", "typed-fixture"]);
        let path = packages.path().join("typed-fixture/plugin.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match mutation {
            "unknown" => manifest["unexpected"] = true.into(),
            "capability" => manifest["capabilities"]["ddc"] = 2.into(),
            "dependency" => manifest["dependencies"] = serde_json::json!(["missing-driver"]),
            _ => manifest["supersedes"] = serde_json::json!(["typed-fixture"]),
        }
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
        let snapshot = harness
            .until(|snapshot| {
                snapshot
                    .devices
                    .first()
                    .is_some_and(|device| device.driver == "generic-ddc")
            })
            .await;
        assert!(
            snapshot
                .drivers
                .iter()
                .find(|driver| driver.id == "typed-fixture")
                .unwrap()
                .error
                .is_some()
        );
        harness.stop().await;
    }
    let packages = catalog(&["generic-ddc"]);
    std::fs::create_dir(packages.path().join("INVALID PACKAGE")).unwrap();
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    let snapshot = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    assert_eq!(
        snapshot
            .drivers
            .iter()
            .find(|driver| driver.id == "INVALID PACKAGE")
            .unwrap()
            .state,
        DriverState::Error
    );
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_module_allowlist_binary_traversal_and_cycles() {
    for expression in [
        "require('../outside')",
        "require('/tmp/outside')",
        "require('generic-ddc.monitor')",
        "require('cycle')",
    ] {
        let packages = catalog(&["typed-fixture"]);
        let path = packages.path().join("typed-fixture/driver.lua");
        let source = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{expression}\n{source}")).unwrap();
        std::fs::write(
            packages.path().join("typed-fixture/cycle.lua"),
            "return require('cycle')",
        )
        .unwrap();
        let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
        harness
            .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
            .await;
        harness.stop().await;
    }
    let packages = catalog(&["typed-fixture"]);
    let path = packages.path().join("typed-fixture/driver.lua");
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path,format!("assert(os==nil and io==nil and package==nil and debug==nil and load==nil and dofile==nil and coroutine==nil and string.dump==nil)\n{source}")).unwrap();
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    harness.stop().await;
    std::fs::write(path, b"\x1bLua").unwrap();
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    harness
        .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
        .await;
    harness.stop().await;
}
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_metadata_unknown_fields_duplicate_enum_and_fractional_integer_rejected() {
    for mutation in [
        "characteristics.text.spec.unknown = true",
        "characteristics.measurement.id = characteristics.temperature.id",
        "characteristics.choice.spec.options[2].value = characteristics.choice.spec.options[1].value",
        "characteristics.measurement.spec.integer = true",
        "characteristics.text.access.unknown = true",
    ] {
        let packages = catalog(&["typed-fixture"]);
        let path = packages.path().join("typed-fixture/driver.lua");
        transform_discovery(
            &path,
            &format!(
                r#"local characteristics = {{}}
for _, characteristic in ipairs(device.characteristics) do
  characteristics[characteristic.id] = characteristic
end
{mutation}"#
            ),
        );
        let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
        harness
            .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
            .await;
        assert!(harness.store.snapshot().devices.is_empty());
        harness.stop().await;
    }
    let packages = catalog(&["typed-fixture"]);
    let settings = BTreeMap::from([(
        "typed-fixture".into(),
        serde_json::json!({"unexpected":true}),
    )]);
    let harness = Harness::settings(
        packages.path(),
        Hardware::new(vec![Screen::benq()]),
        settings,
    );
    harness
        .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
        .await;
    harness.stop().await;
}
#[test]
fn lua_contract_duplicate_capability_registration_rejected() {
    let mut registry = CapabilityRegistry::default();
    registry
        .register(Arc::new(DdcCapabilityFactory::lazy_native()))
        .unwrap();
    assert!(
        registry
            .register(Arc::new(DdcCapabilityFactory::lazy_native()))
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn lua_contract_near_cap_fault_retains_all_prior_observations() {
    let packages = catalog(&["snapshot-fixture"]);
    let mut second = Screen::benq();
    second.key = "screen-b".into();
    second.identity = Some("b".repeat(64));
    let hardware = Hardware::new(vec![Screen::benq(), second]);
    let calibration = Harness::start(packages.path(), hardware.clone());
    let shape = calibration
        .until(|snapshot| {
            snapshot.devices.len() == 2
                && snapshot.devices.iter().all(|device| {
                    device
                        .characteristics
                        .iter()
                        .all(|item| item.status == CharacteristicStatus::Ok)
                })
        })
        .await;
    let mut candidate = (*shape).clone();
    let (mut lower, mut upper) = (1usize, 1024usize);
    while lower < upper {
        let length = (lower + upper).div_ceil(2);
        for device in &mut candidate.devices {
            for item in &mut device.characteristics {
                item.value = Some(CharacteristicValue::String("𐐀".repeat(length)));
            }
        }
        if StateStore::validate_snapshot(&candidate).is_ok() {
            lower = length;
        } else {
            upper = length - 1;
        }
    }
    calibration.stop().await;
    let failure_length = lower + 2;
    assert!(failure_length <= 1024);
    let settings = BTreeMap::from([(
        "snapshot-fixture".into(),
        serde_json::json!({"initialLength":lower,"failureLength":failure_length}),
    )]);
    let harness = Harness::settings(packages.path(), hardware, settings);
    let initial = harness
        .until(|snapshot| {
            snapshot.devices.len() == 2
                && snapshot.devices.iter().all(|device| {
                    device
                        .characteristics
                        .iter()
                        .all(|item| item.status == CharacteristicStatus::Ok)
                })
        })
        .await;
    candidate = (*initial).clone();
    for item in &mut candidate.devices[0].characteristics {
        item.value = Some(CharacteristicValue::String("𐐀".repeat(failure_length)));
    }
    assert!(candidate.validated_json().unwrap().len() <= MAX_SNAPSHOT_BYTES);
    assert!(StateStore::validate_snapshot(&candidate).is_err());
    harness.hardware.mutate(16, 66);
    let failed = harness
        .until(|snapshot| snapshot.drivers[0].state == DriverState::Error)
        .await;
    assert_eq!(failed.devices.len(), initial.devices.len());
    for previous in &initial.devices {
        let device = failed
            .devices
            .iter()
            .find(|device| device.id == previous.id)
            .unwrap();
        assert_eq!(device.status, DeviceStatus::Unavailable);
        for old in &previous.characteristics {
            let current = device
                .characteristics
                .iter()
                .find(|item| item.id == old.id)
                .unwrap();
            assert_eq!(current.value, old.value);
            assert_eq!(current.status, CharacteristicStatus::Unavailable);
            assert_eq!(current.error.as_ref().unwrap().code, ErrorCode::PluginError);
        }
    }
    assert!(harness.store.encoded_snapshot().len() <= MAX_SNAPSHOT_BYTES);
    harness.stop().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_symlinks_duplicate_fields_and_source_caps_are_isolated() {
    for failure in ["symlink", "duplicate", "source-size"] {
        let packages = catalog(&["generic-ddc", "typed-fixture"]);
        let package = packages.path().join("typed-fixture");
        match failure {
            "symlink" => {
                std::os::unix::fs::symlink(package.join("driver.lua"), package.join("alias.lua"))
                    .unwrap()
            }
            "duplicate" => {
                let path = package.join("plugin.json");
                let source = std::fs::read_to_string(&path).unwrap();
                std::fs::write(path, source.replacen("{", "{\"id\":\"typed-fixture\",", 1))
                    .unwrap();
            }
            _ => std::fs::write(package.join("large.lua"), vec![b' '; 256 * 1024 + 1]).unwrap(),
        }
        let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
        let snapshot = harness
            .until(|snapshot| {
                snapshot
                    .devices
                    .first()
                    .is_some_and(|device| device.driver == "generic-ddc")
            })
            .await;
        assert_eq!(
            snapshot
                .drivers
                .iter()
                .find(|driver| driver.id == "typed-fixture")
                .unwrap()
                .state,
            DriverState::Error
        );
        harness.stop().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_directory_diagnostics_preserve_distinct_invalid_names() {
    use std::ffi::OsString;
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    let packages = catalog(&["generic-ddc"]);
    let names = vec![
        OsString::from("INVALID-".to_owned() + &"a".repeat(80)),
        OsString::from("INVALID-".to_owned() + &"a".repeat(79) + "b"),
    ];
    #[cfg(target_os = "linux")]
    let names = {
        let mut names = names;
        names.extend([
            OsString::from_vec(vec![b'i', 0xff]),
            OsString::from_vec(vec![b'i', 0xfe]),
        ]);
        names
    };
    let invalid_count = names.len();
    for name in names {
        std::fs::create_dir(packages.path().join(name)).unwrap();
    }
    let harness = Harness::start(packages.path(), Hardware::new(vec![Screen::benq()]));
    let snapshot = harness.until(|snapshot| !snapshot.devices.is_empty()).await;
    assert_eq!(snapshot.drivers.len(), invalid_count + 1);
    let names: std::collections::BTreeSet<_> = snapshot
        .drivers
        .iter()
        .map(|driver| driver.id.as_str())
        .collect();
    assert_eq!(names.len(), invalid_count + 1);
    assert_eq!(
        snapshot
            .drivers
            .iter()
            .filter(|driver| driver.state == DriverState::Error)
            .count(),
        invalid_count
    );
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn lua_contract_empty_and_disabled_catalog_perform_no_native_access() {
    for disabled in [false, true] {
        let packages = if disabled {
            catalog(&["generic-ddc", "benq-rd280u"])
        } else {
            catalog(&[])
        };
        let mut registry = CapabilityRegistry::default();
        registry
            .register(Arc::new(DdcCapabilityFactory::lazy_native()))
            .unwrap();
        let store = StateStore::new(synapsed_api::NodeInfo {
            node_id: "00000000-0000-4000-8000-000000000001".into(),
            instance_id: "00000000-0000-4000-8000-000000000002".into(),
            name: "Empty".into(),
        })
        .unwrap();
        let host = synapsed_plugins::PluginHost::start(
            store.clone(),
            synapsed_plugins::PluginConfig {
                directory: packages.path().into(),
                directory_explicit: true,
                disabled: if disabled {
                    vec!["generic-ddc".into(), "benq-rd280u".into()]
                } else {
                    vec![]
                },
                discovery_interval_ms: 1000,
                settings: BTreeMap::new(),
            },
            registry.clone(),
        )
        .unwrap();
        assert!(store.snapshot().devices.is_empty());
        assert!(
            store
                .snapshot()
                .drivers
                .iter()
                .all(|driver| driver.state == DriverState::Disabled)
        );
        host.shutdown(Instant::now() + Duration::from_secs(5))
            .await
            .unwrap();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn lua_contract_quarantine_does_not_stop_other_packages_publishing() {
    let packages = catalog(&["fault-fixture"]);
    let round_package = packages.path().join("round-fixture");
    std::fs::create_dir_all(&round_package).unwrap();
    std::fs::write(
        round_package.join("plugin.json"),
        r#"{
  "apiVersion": 1,
  "id": "round-fixture",
  "version": "0.1.0",
  "entry": "driver.lua",
  "dependencies": [],
  "supersedes": [],
  "capabilities": {},
  "permissions": {}
}"#,
    )
    .unwrap();
    std::fs::write(
        round_package.join("driver.lua"),
        r#"return {
  configure = function(settings)
    local rounds = 0
    return {
      discover = function(ctx)
        rounds = rounds + 1
        return { devices = { {
          id = "round-status",
          kind = "instrument",
          label = "Round " .. rounds,
          endpoints = {},
          identityPolicy = "slot",
          characteristics = {
            {
              id = "round",
              label = "Round",
              spec = { kind = "number", minimum = 0, maximum = 100, step = 1, integer = true },
              access = { read = true, write = false, notify = false },
              writes = {},
            },
          },
          onRefresh = function(ctx)
            return { updates = { { id = "round", status = "ok", value = rounds } } }
          end,
        } } }
      end,
    }
  end,
}"#,
    )
    .unwrap();
    let settings = BTreeMap::from([(
        "fault-fixture".into(),
        serde_json::json!({"mode": "exception"}),
    )]);
    let harness = Harness::settings(
        packages.path(),
        Hardware::new(vec![Screen::benq()]),
        settings,
    );
    harness.until(|snapshot| snapshot.devices.len() == 2).await;

    harness.hardware.mutate(16, 66);
    let failed = harness
        .until(|snapshot| {
            snapshot
                .drivers
                .iter()
                .any(|driver| driver.id == "fault-fixture" && driver.state == DriverState::Error)
        })
        .await;
    assert!(
        failed
            .devices
            .iter()
            .any(|device| { device.id == "fault" && device.status == DeviceStatus::Unavailable })
    );
    assert!(
        failed
            .drivers
            .iter()
            .any(|driver| driver.id == "round-fixture" && driver.state == DriverState::Online)
    );
    let failed_round_device = failed
        .devices
        .iter()
        .find(|device| device.id == "round-status")
        .unwrap();
    let failed_round_label = failed_round_device.label.clone();
    let failed_round_value = failed_round_device
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == "round")
        .unwrap()
        .value
        .clone()
        .unwrap();

    let round = harness
        .host
        .complete_discovery_round_for_test()
        .await
        .unwrap();
    assert!(round > 0);
    let published = harness
        .until(|snapshot| {
            snapshot
                .devices
                .iter()
                .find(|device| device.id == "round-status")
                .is_some_and(|device| device.label != failed_round_label)
        })
        .await;
    assert!(
        published
            .drivers
            .iter()
            .any(|driver| driver.id == "fault-fixture" && driver.state == DriverState::Error)
    );
    assert!(
        published
            .drivers
            .iter()
            .any(|driver| driver.id == "round-fixture" && driver.state == DriverState::Online)
    );
    let round_device = published
        .devices
        .iter()
        .find(|device| device.id == "round-status")
        .unwrap();
    assert!(round_device.label != failed_round_label);
    assert_eq!(round_device.status, DeviceStatus::Online);
    let published_round_value = round_device
        .characteristics
        .iter()
        .find(|characteristic| characteristic.id == "round")
        .unwrap()
        .value
        .as_ref()
        .unwrap();
    assert_ne!(published_round_value, &failed_round_value);
    harness.stop().await;
}
