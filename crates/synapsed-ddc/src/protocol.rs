//! DDC/CI framing and base EDID validation. No device or VCP-code policy.
use crate::{DriverError, ErrorCode, VcpValue};
use sha2::{Digest, Sha256};
use std::fmt::Write;

pub const IO_DELAY: std::time::Duration = std::time::Duration::from_millis(50);
pub const REPLY_BYTES: usize = 11;
pub fn checksum(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |sum, byte| sum ^ byte)
}
pub fn macos_get_frame(code: u8) -> [u8; 4] {
    [0x82, 0x01, code, 0x6e ^ 0x82 ^ 0x01 ^ code]
}
pub fn linux_get_frame(code: u8) -> [u8; 5] {
    [0x51, 0x82, 0x01, code, 0x6e ^ 0x51 ^ 0x82 ^ 0x01 ^ code]
}
pub fn macos_set_frame(code: u8, value: u16) -> [u8; 6] {
    let [hi, lo] = value.to_be_bytes();
    [
        0x84,
        0x03,
        code,
        hi,
        lo,
        0x6e ^ 0x51 ^ 0x84 ^ 0x03 ^ code ^ hi ^ lo,
    ]
}
pub fn linux_set_frame(code: u8, value: u16) -> [u8; 7] {
    let frame = macos_set_frame(code, value);
    [
        0x51, frame[0], frame[1], frame[2], frame[3], frame[4], frame[5],
    ]
}
pub fn parse_reply(code: u8, bytes: &[u8]) -> Result<VcpValue, DriverError> {
    if bytes.len() != REPLY_BYTES
        || bytes[0] != 0x6e
        || bytes[1] != 0x88
        || bytes[2] != 0x02
        || bytes[3] != 0
        || bytes[4] != code
        || checksum(0x50, &bytes[..10]) != bytes[10]
    {
        return Err(ddc_error("DDC/CI returned an invalid response."));
    }
    Ok(VcpValue {
        maximum: u16::from_be_bytes([bytes[6], bytes[7]]),
        current: u16::from_be_bytes([bytes[8], bytes[9]]),
    })
}
pub fn check_transfer(completed: u32, expected: u32) -> Result<(), DriverError> {
    if completed != expected {
        return Err(ddc_error("I2C transfer did not complete every message."));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdidIdentity {
    pub manufacturer: Option<String>,
    pub product: u16,
    pub product_name: Option<String>,
    pub identity: String,
}
/// Hash only a validated 128-byte base block. Never retain or export serials/raw data.
pub fn parse_base_edid(bytes: &[u8]) -> Result<EdidIdentity, DriverError> {
    if bytes.len() != 128
        || bytes[..8] != [0, 255, 255, 255, 255, 255, 255, 0]
        || bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0
    {
        return Err(ddc_error("Display returned an invalid base EDID."));
    }
    let manufacturer_bits = u16::from_be_bytes([bytes[8], bytes[9]]);
    let letters = [
        (manufacturer_bits >> 10) & 31,
        (manufacturer_bits >> 5) & 31,
        manufacturer_bits & 31,
    ];
    let manufacturer = if manufacturer_bits & 0x8000 == 0
        && letters.iter().all(|letter| (1..=26).contains(letter))
    {
        Some(
            letters
                .iter()
                .map(|letter| char::from(b'A' + *letter as u8 - 1))
                .collect(),
        )
    } else {
        None
    };
    let mut product_name = None;
    for descriptor in bytes[54..126].chunks_exact(18) {
        if descriptor[..5] == [0, 0, 0, 0xfc, 0] {
            let text = &descriptor[5..18];
            let end = text
                .iter()
                .position(|byte| *byte == 0 || *byte == b'\n')
                .unwrap_or(text.len());
            if text[..end].iter().all(|byte| (0x20..=0x7e).contains(byte)) {
                let name = std::str::from_utf8(&text[..end])
                    .expect("validated ASCII")
                    .trim();
                if !name.is_empty() {
                    product_name = Some(name.to_owned());
                }
            }
            break;
        }
    }
    let hash = Sha256::digest(bytes);
    let mut identity = String::with_capacity(64);
    for byte in hash {
        write!(&mut identity, "{byte:02x}").expect("string formatting");
    }
    Ok(EdidIdentity {
        manufacturer,
        product: u16::from_le_bytes([bytes[10], bytes[11]]),
        product_name,
        identity,
    })
}
pub(crate) fn ddc_error(message: &'static str) -> DriverError {
    DriverError::new(ErrorCode::DdcError, message)
}
