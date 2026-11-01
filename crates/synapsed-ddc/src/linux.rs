//! Linux i2c-dev transport. Only eligible adapters with validated EDID become endpoints.
use crate::{
    DdcBackend, DdcTransport, DriverError, ErrorCode, InventoryWithOwnedHandles, OwnedEndpoint,
    VcpValue, protocol,
};
use i2cdev::{
    core::{I2CMessage, I2CTransfer},
    linux::{LinuxI2CBus, LinuxI2CError, LinuxI2CMessage},
};
use std::{collections::HashSet, fs, io, path::Path, thread};

/// Linux i2c-dev backend that discovers eligible display endpoints.
pub struct LinuxBackend;
impl LinuxBackend {
    /// Create a Linux backend; native devices are scanned later on the owner thread.
    pub fn new() -> Result<Self, DriverError> {
        Ok(Self)
    }
}
/// Return whether an I2C adapter name is eligible for display DDC scanning.
pub fn eligible_adapter(name: &str) -> bool {
    !["SMBus", "soc:i2cdsi", "smu", "mac-io", "u4"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}
/// Adapters owned by a DRM connector the compositor disabled are not selectable display paths.
/// This also resolves connector `ddc` links used by AMDGPU hardware-I2C adapters.
/// Missing or unreadable attributes keep the adapter eligible so hidden candidates still block uniqueness.
fn disabled_drm_connector(sysfs_root: &Path, adapter_key: &str) -> bool {
    let Some(connector) =
        synapsed_inventory::linux::drm::connector_for_adapter(sysfs_root, adapter_key)
    else {
        return false;
    };
    fs::read_to_string(connector.join("enabled")).is_ok_and(|enabled| enabled.trim() == "disabled")
}
fn auxiliary_connectors(sysfs_root: &Path) -> HashSet<(String, String)> {
    let Ok(entries) = fs::read_dir(sysfs_root.join("class/i2c-dev")) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let adapter = entry.file_name();
            let adapter = adapter.to_str()?;
            let name = fs::read_to_string(entry.path().join("name")).ok()?;
            if !name.trim().starts_with("AMDGPU DM aux hw bus ") {
                return None;
            }
            let placement =
                synapsed_inventory::linux::drm::placement_for_adapter(sysfs_root, adapter)?;
            Some((placement.controller_key, placement.connector))
        })
        .collect()
}
fn redundant_amdgpu_hardware_path(
    adapter_name: &str,
    placement: Option<&synapsed_inventory::linux::drm::Placement>,
    auxiliary: &HashSet<(String, String)>,
) -> bool {
    if !adapter_name.starts_with("AMDGPU DM i2c hw bus ") {
        return false;
    }
    placement.is_some_and(|placement| {
        auxiliary.contains(&(
            placement.controller_key.clone(),
            placement.connector.clone(),
        ))
    })
}
fn io_error(error: &io::Error) -> DriverError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        DriverError::new(
            ErrorCode::PermissionDenied,
            "Access to an eligible I2C adapter was denied.",
        )
    } else {
        DriverError::new(
            ErrorCode::DeviceUnavailable,
            "An eligible I2C adapter could not be scanned.",
        )
    }
}
fn native_error(error: LinuxI2CError) -> DriverError {
    let error: io::Error = error.into();
    if error.kind() == io::ErrorKind::PermissionDenied {
        DriverError::new(
            ErrorCode::PermissionDenied,
            "Access to an eligible I2C adapter was denied.",
        )
    } else {
        protocol::ddc_error("Native I2C transaction failed.")
    }
}
fn no_edid_target(error: &LinuxI2CError) -> bool {
    let errno = match error {
        LinuxI2CError::Errno(errno) => Some(*errno),
        LinuxI2CError::Io(error) => error.raw_os_error(),
    };
    matches!(errno, Some(libc::ENXIO | libc::ENODEV | libc::EREMOTEIO))
}
fn read_edid<B>(bus: &mut B) -> Result<Option<protocol::EdidIdentity>, DriverError>
where
    for<'a> B: I2CTransfer<'a, Message = LinuxI2CMessage<'a>, Error = LinuxI2CError>,
{
    let offset = [0u8];
    let mut bytes = [0u8; 128];
    let mut messages = [
        LinuxI2CMessage::write(&offset).with_address(0x50),
        LinuxI2CMessage::read(&mut bytes).with_address(0x50),
    ];
    match bus.transfer(&mut messages) {
        Ok(completed) => protocol::check_transfer(completed, 2)?,
        Err(error) if no_edid_target(&error) => return Ok(None),
        Err(error) => return Err(native_error(error)),
    }
    protocol::parse_base_edid(&bytes).map(Some)
}
impl DdcBackend for LinuxBackend {
    fn scan(&mut self) -> Result<InventoryWithOwnedHandles, DriverError> {
        let entries = match fs::read_dir("/sys/class/i2c-dev") {
            Ok(entries) => entries,
            Err(error) => {
                return Ok(InventoryWithOwnedHandles {
                    endpoints: vec![],
                    errors: vec![io_error(&error)],
                });
            }
        };
        let mut endpoints = Vec::new();
        let mut errors = Vec::new();
        let auxiliary = auxiliary_connectors(Path::new("/sys"));
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    errors.push(io_error(&error));
                    continue;
                }
            };
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(number) = name.strip_prefix("i2c-") else {
                continue;
            };
            if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let adapter = match fs::read_to_string(entry.path().join("name")) {
                Ok(adapter) => adapter,
                Err(error) => {
                    errors.push(io_error(&error));
                    continue;
                }
            };
            if !eligible_adapter(adapter.trim()) {
                continue;
            }
            if disabled_drm_connector(Path::new("/sys"), name) {
                continue;
            }
            let placement =
                synapsed_inventory::linux::drm::placement_for_adapter(Path::new("/sys"), name);
            if redundant_amdgpu_hardware_path(adapter.trim(), placement.as_ref(), &auxiliary) {
                continue;
            }
            let path = Path::new("/dev").join(name);
            let mut bus = match LinuxI2CBus::new(&path) {
                Ok(bus) => bus,
                Err(error) => {
                    errors.push(native_error(error));
                    continue;
                }
            };
            let identity = match read_edid(&mut bus) {
                Ok(Some(identity)) => identity,
                Ok(None) => continue,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            endpoints.push(OwnedEndpoint {
                key: name.to_owned(),
                manufacturer: identity.manufacturer,
                product: Some(identity.product),
                product_name: identity.product_name,
                identity: Some(identity.identity),
                placement: placement.map(|placement| crate::Placement {
                    connector: placement.connector,
                    controller_key: placement.controller_key,
                }),
                transport: Box::new(LinuxTransport { bus }),
            });
        }
        Ok(InventoryWithOwnedHandles { endpoints, errors })
    }
}
struct LinuxTransport<B = LinuxI2CBus> {
    bus: B,
}
impl<B> LinuxTransport<B>
where
    for<'a> B: I2CTransfer<'a, Message = LinuxI2CMessage<'a>, Error = LinuxI2CError>,
{
    fn write(&mut self, frame: &[u8]) -> Result<(), DriverError> {
        let mut messages = [LinuxI2CMessage::write(frame).with_address(0x37)];
        let completed = self.bus.transfer(&mut messages).map_err(native_error)?;
        protocol::check_transfer(completed, 1)
    }
}
impl<B> DdcTransport for LinuxTransport<B>
where
    for<'a> B: I2CTransfer<'a, Message = LinuxI2CMessage<'a>, Error = LinuxI2CError>,
{
    fn get_vcp(&mut self, code: u8) -> Result<VcpValue, DriverError> {
        self.write(&protocol::linux_get_frame(code))?;
        thread::sleep(protocol::IO_DELAY);
        let mut bytes = [0u8; protocol::REPLY_BYTES];
        let mut messages = [LinuxI2CMessage::read(&mut bytes).with_address(0x37)];
        let completed = self.bus.transfer(&mut messages).map_err(native_error)?;
        protocol::check_transfer(completed, 1)?;
        protocol::parse_reply(code, &bytes)
    }
    fn set_vcp(&mut self, code: u8, value: u16) -> Result<(), DriverError> {
        self.write(&protocol::linux_set_frame(code, value))?;
        thread::sleep(protocol::IO_DELAY);
        Ok(())
    }
}

#[cfg(test)]
mod protocol_fixtures {
    use super::*;
    use std::collections::VecDeque;
    // Exercise the real addressed-message construction/completion paths, not a reimplementation.
    struct Bus {
        results: VecDeque<Result<u32, LinuxI2CError>>,
        message_counts: Vec<usize>,
    }
    impl Bus {
        fn completions(values: impl IntoIterator<Item = u32>) -> Self {
            Self {
                results: values.into_iter().map(Ok).collect(),
                message_counts: vec![],
            }
        }
    }
    impl<'a> I2CTransfer<'a> for Bus {
        type Error = LinuxI2CError;
        type Message = LinuxI2CMessage<'a>;
        fn transfer(&mut self, messages: &mut [Self::Message]) -> Result<u32, Self::Error> {
            self.message_counts.push(messages.len());
            self.results
                .pop_front()
                .expect("unexpected extra native transaction")
        }
    }
    #[test]
    fn incomplete_edid_is_not_an_empty_inventory() {
        for completed in [0, 1, 3] {
            let mut bus = Bus::completions([completed]);
            assert_eq!(read_edid(&mut bus).unwrap_err().code, ErrorCode::DdcError);
            assert_eq!(bus.message_counts, [2]);
        }
    }
    #[test]
    fn separate_ddc_transfers_require_exact_completion() {
        for completed in [0, 2] {
            let mut transport = LinuxTransport {
                bus: Bus::completions([completed]),
            };
            assert_eq!(
                transport.get_vcp(0x10).unwrap_err().code,
                ErrorCode::DdcError
            );
            assert_eq!(transport.bus.message_counts, [1]);
            let mut transport = LinuxTransport {
                bus: Bus::completions([1, completed]),
            };
            assert_eq!(
                transport.get_vcp(0x10).unwrap_err().code,
                ErrorCode::DdcError
            );
            assert_eq!(transport.bus.message_counts, [1, 1]);
            let mut transport = LinuxTransport {
                bus: Bus::completions([completed]),
            };
            assert_eq!(
                transport.set_vcp(0x10, 100).unwrap_err().code,
                ErrorCode::DdcError
            );
            assert_eq!(transport.bus.message_counts, [1]);
        }
    }
    #[test]
    fn denied_edid_candidate_is_preserved() {
        for errno in [libc::EACCES, libc::EPERM] {
            let mut bus = Bus {
                results: VecDeque::from([Err(LinuxI2CError::Errno(errno))]),
                message_counts: vec![],
            };
            assert_eq!(
                read_edid(&mut bus).unwrap_err().code,
                ErrorCode::PermissionDenied
            );
        }
        let mut absent = Bus {
            results: VecDeque::from([Err(LinuxI2CError::Errno(libc::ENXIO))]),
            message_counts: vec![],
        };
        assert!(read_edid(&mut absent).unwrap().is_none());
    }
}

#[cfg(test)]
mod sysfs_policy {
    use super::{disabled_drm_connector, redundant_amdgpu_hardware_path};
    use std::{
        collections::HashSet,
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn adapter(
        root: &Path,
        name: &str,
        subsystem: &str,
        connector_name: &str,
        enabled: Option<&str>,
    ) -> PathBuf {
        let device = if subsystem == "drm" {
            root.join("devices/pci0000:00/0000:00:02.0/drm/card0")
                .join(connector_name)
        } else {
            root.join("devices/pci0000:00/0000:00:02.0").join(name)
        };
        fs::create_dir_all(&device).unwrap();
        let subsystem_path = root.join("bus").join(subsystem);
        fs::create_dir_all(&subsystem_path).unwrap();
        symlink(subsystem_path, device.join("subsystem")).unwrap();
        if let Some(enabled) = enabled {
            fs::write(device.join("enabled"), enabled).unwrap();
        }
        let adapter_path = root.join("class/i2c-dev").join(name);
        fs::create_dir_all(&adapter_path).unwrap();
        if enabled.is_some() {
            let i2c_device = device.join("i2c-9");
            fs::create_dir_all(&i2c_device).unwrap();
            symlink(i2c_device, adapter_path.join("device")).unwrap();
        }
        adapter_path
    }

    #[test]
    fn adapters_of_disabled_drm_connectors_are_skipped() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("synapsed-ddc-sysfs-{}-{nonce}", std::process::id()));
        let disabled = adapter(&root, "i2c-9", "drm", "card0-DP-3", Some("disabled\n"));
        let enabled = adapter(&root, "i2c-10", "drm", "card0-DP-1", Some("enabled\n"));
        let pci = adapter(&root, "i2c-11", "pci", "", Some("disabled\n"));
        let missing = adapter(&root, "i2c-12", "drm", "card0-DP-2", None);

        assert!(disabled_drm_connector(&root, "i2c-9"));
        assert!(!disabled_drm_connector(&root, "i2c-10"));
        assert!(!disabled_drm_connector(&root, "i2c-11"));
        assert!(!disabled_drm_connector(&root, "i2c-12"));
        assert!(disabled.exists() && enabled.exists() && pci.exists() && missing.exists());

        let auxiliary = HashSet::from([("0000:00:02.0".into(), "DP-1".into())]);
        let placement = synapsed_inventory::linux::drm::Placement {
            connector: "DP-1".into(),
            controller_key: "0000:00:02.0".into(),
        };
        assert!(redundant_amdgpu_hardware_path(
            "AMDGPU DM i2c hw bus 0",
            Some(&placement),
            &auxiliary
        ));
        assert!(!redundant_amdgpu_hardware_path(
            "AMDGPU DM i2c hw bus 1",
            None,
            &auxiliary
        ));
        assert!(!redundant_amdgpu_hardware_path(
            "AMDGPU DM i2c hw bus 1",
            Some(&synapsed_inventory::linux::drm::Placement {
                connector: "HDMI-A-1".into(),
                controller_key: "0000:00:02.0".into(),
            }),
            &auxiliary
        ));
        assert!(!redundant_amdgpu_hardware_path(
            "AMDGPU DM aux hw bus 0",
            Some(&placement),
            &auxiliary
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ddc_linked_adapter_inherits_disabled_connector_state() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "synapsed-ddc-ddc-link-{}-{nonce}",
            std::process::id()
        ));
        let connector = root.join("devices/pci0000:00/0000:00:02.0/drm/card0/card0-DP-3");
        fs::create_dir_all(&connector).unwrap();
        fs::write(connector.join("enabled"), "disabled\n").unwrap();
        let drm_bus = root.join("bus/drm");
        fs::create_dir_all(&drm_bus).unwrap();
        symlink(&drm_bus, connector.join("subsystem")).unwrap();

        let device = root.join("devices/pci0000:00/0000:00:02.0/i2c-0");
        fs::create_dir_all(&device).unwrap();
        let adapter = root.join("class/i2c-dev/i2c-0");
        fs::create_dir_all(&adapter).unwrap();
        symlink(&device, adapter.join("device")).unwrap();
        symlink(&device, connector.join("ddc")).unwrap();
        let drm_class = root.join("class/drm");
        fs::create_dir_all(&drm_class).unwrap();
        symlink(&connector, drm_class.join("card0-DP-3")).unwrap();

        assert!(disabled_drm_connector(&root, "i2c-0"));
        fs::remove_dir_all(root).unwrap();
    }
}
