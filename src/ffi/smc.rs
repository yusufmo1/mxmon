//! Sudoless AppleSMC access via IOKit for temperature sensors, fans, and
//! system power. Struct layout and command protocol per the well-known SMC
//! user-client interface (as used by macmon, iStats, smckit, …).

use std::io;

use super::iokit::{
    IOConnectCallStructMethod, IOServiceClose, IOServiceOpen, mach_task_self, service_iter,
};

const KERNEL_INDEX_SMC: u32 = 2;
const CMD_READ_BYTES: u8 = 5;
const CMD_READ_INDEX: u8 = 8;
const CMD_READ_KEYINFO: u8 = 9;
const RESULT_KEY_NOT_FOUND: u8 = 132;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct KeyDataVer {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PLimitData {
    version: u16,
    length: u16,
    cpu_p_limit: u32,
    gpu_p_limit: u32,
    mem_p_limit: u32,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyInfo {
    pub data_size: u32,
    pub data_type: u32,
    pub data_attributes: u8,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct KeyData {
    key: u32,
    vers: KeyDataVer,
    p_limit_data: PLimitData,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

/// Encode a 4-char SMC key as its big-endian u32 form.
fn encode_key(key: &str) -> u32 {
    debug_assert_eq!(key.len(), 4);
    key.bytes().fold(0u32, |acc, b| (acc << 8) | u32::from(b))
}

/// Decode a big-endian u32 into its 4-char form (lossy for non-ASCII).
fn decode_fourcc(v: u32) -> String {
    String::from_utf8_lossy(&v.to_be_bytes()).into_owned()
}

/// FourCC for the `flt ` (little-endian f32) SMC data type.
const TYPE_FLT: u32 = u32::from_be_bytes(*b"flt ");

/// An open connection to the AppleSMC keys endpoint.
pub struct Smc {
    conn: u32,
}

impl Smc {
    pub fn open() -> io::Result<Self> {
        for (device, name) in service_iter("AppleSMC")? {
            if name == "AppleSMCKeysEndpoint" {
                let mut conn = 0u32;
                let kr = unsafe { IOServiceOpen(device.raw(), mach_task_self(), 0, &raw mut conn) };
                if kr != 0 {
                    return Err(io::Error::other(format!(
                        "IOServiceOpen(AppleSMC): {kr:#x}"
                    )));
                }
                return Ok(Self { conn });
            }
        }
        Err(io::Error::other("AppleSMCKeysEndpoint not found"))
    }

    fn call(&self, input: &KeyData) -> io::Result<KeyData> {
        let mut output = KeyData::default();
        let mut out_size = size_of::<KeyData>();
        let kr = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                std::ptr::from_ref::<KeyData>(input).cast(),
                size_of::<KeyData>(),
                (&raw mut output).cast(),
                &raw mut out_size,
            )
        };
        if kr != 0 {
            return Err(io::Error::other(format!("SMC call failed: {kr:#x}")));
        }
        if output.result == RESULT_KEY_NOT_FOUND {
            return Err(io::Error::new(io::ErrorKind::NotFound, "SMC key not found"));
        }
        if output.result != 0 {
            return Err(io::Error::other(format!("SMC result {}", output.result)));
        }
        Ok(output)
    }

    /// Metadata (size/type) for a key.
    pub fn key_info(&self, key: &str) -> io::Result<KeyInfo> {
        let input = KeyData {
            key: encode_key(key),
            data8: CMD_READ_KEYINFO,
            ..Default::default()
        };
        Ok(self.call(&input)?.key_info)
    }

    /// Name of the `index`-th key (for enumeration via `#KEY`).
    pub fn key_by_index(&self, index: u32) -> io::Result<String> {
        let input = KeyData {
            data8: CMD_READ_INDEX,
            data32: index,
            ..Default::default()
        };
        Ok(decode_fourcc(self.call(&input)?.key))
    }

    /// Raw value bytes for a key whose `KeyInfo` is already known.
    fn read_bytes(&self, key: &str, info: KeyInfo) -> io::Result<[u8; 32]> {
        let input = KeyData {
            key: encode_key(key),
            key_info: info,
            data8: CMD_READ_BYTES,
            ..Default::default()
        };
        Ok(self.call(&input)?.bytes)
    }

    /// Read a `flt ` (LE f32) key. Errors if the key has another type.
    pub fn read_f32(&self, key: &str, info: KeyInfo) -> io::Result<f32> {
        if info.data_type != TYPE_FLT || info.data_size != 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SMC key {key} is not flt/4"),
            ));
        }
        let bytes = self.read_bytes(key, info)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Total number of keys (via the `#KEY` pseudo-key).
    pub fn key_count(&self) -> io::Result<u32> {
        let info = self.key_info("#KEY")?;
        let bytes = self.read_bytes("#KEY", info)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Enumerate every key name on this machine.
    pub fn all_keys(&self) -> io::Result<Vec<String>> {
        let count = self.key_count()?;
        Ok((0..count)
            .filter_map(|i| self.key_by_index(i).ok())
            .collect())
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.conn) };
    }
}
