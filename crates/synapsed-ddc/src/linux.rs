//! Linux i2c-dev transport. Only eligible adapters with validated EDID become endpoints.
use crate::{
    DdcBackend, DdcTransport, DriverError, ErrorCode, InventoryWithOwnedHandles, OwnedEndpoint,
    VcpValue, protocol,
};
use i2cdev::{
    core::{I2CMessage, I2CTransfer},
    linux::{LinuxI2CBus, LinuxI2CError, LinuxI2CMessage},
};
use std::{fs, io, path::Path, thread};

pub struct LinuxBackend;
impl LinuxBackend {
    pub fn new() -> Result<Self, DriverError> {
        Ok(Self)
    }
}
pub fn eligible_adapter(name: &str) -> bool {
    !["SMBus", "soc:i2cdsi", "smu", "mac-io", "u4"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
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
