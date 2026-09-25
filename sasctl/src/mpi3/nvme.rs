use anyhow::{Result, bail};
use serde::Serialize;

use super::mpi::{FUNCTION_NVME_ENCAPSULATED, IOCSTATUS_SUCCESS, ioc_status_name};
use super::transport::{Request, TIMEOUT_DEVICE, Transport};
use crate::bytes::{Le, LeMut};

pub const NVME_COMMAND_LEN: usize = 64;
pub const NVME_REQUEST_LEN: usize = 0x20 + NVME_COMMAND_LEN;
pub const FLAGS_SUBMISSIONQ_ADMIN: u16 = 0x0001;
const COMMAND: usize = 0x20;

pub const OPCODE_GET_LOG_PAGE: u8 = 0x02;
pub const OPCODE_IDENTIFY: u8 = 0x06;
pub const CNS_CONTROLLER: u32 = 0x01;
pub const IDENTIFY_LEN: usize = 4096;
pub const SMART_LOG_LEN: usize = 512;
pub const SMART_LOG_CDW10: u32 = 0x007F_0002;
pub const NSID_ALL: u32 = 0xFFFF_FFFF;

pub fn admin_request(handle: u16, opcode: u8, nsid: u32, cdw10: u32, len: usize) -> Request {
    let mut r = Request::new(FUNCTION_NVME_ENCAPSULATED, NVME_REQUEST_LEN);
    r.frame.put_u16(0x0A, handle);
    r.frame.put_u16(0x0C, NVME_COMMAND_LEN as u16);
    r.frame.put_u16(0x0E, FLAGS_SUBMISSIONQ_ADMIN);
    r.frame.put_u32(0x10, len as u32);
    r.frame.put_u8(COMMAND, opcode);
    r.frame.put_u32(COMMAND + 4, nsid);
    r.frame.put_u32(COMMAND + 40, cdw10);
    r.data_in_len = len;
    r.timeout = TIMEOUT_DEVICE;
    r
}

pub fn identify_controller_request(handle: u16) -> Request {
    admin_request(handle, OPCODE_IDENTIFY, 0, CNS_CONTROLLER, IDENTIFY_LEN)
}

pub fn smart_log_request(handle: u16) -> Request {
    admin_request(
        handle,
        OPCODE_GET_LOG_PAGE,
        NSID_ALL,
        SMART_LOG_CDW10,
        SMART_LOG_LEN,
    )
}

pub fn admin(t: &dyn Transport, r: &Request) -> Result<Vec<u8>> {
    let reply = r.send(t)?;
    let status = reply.ioc_status();
    let opcode = r.frame.u8_at(COMMAND);
    let nvme_status = if reply.is_address() {
        (reply.frame.u16_at(0x1E) >> 1) & 0x7FFF
    } else {
        0
    };
    if status != IOCSTATUS_SUCCESS || nvme_status != 0 {
        bail!(
            "NVMe admin command 0x{opcode:02x} failed: IOCStatus 0x{status:04x} ({}), NVMe status 0x{nvme_status:04x}",
            ioc_status_name(status)
        );
    }
    Ok(reply.data_in)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentifyController {
    pub serial: String,
    pub model: String,
    pub firmware: String,
}

pub fn parse_identify_controller(d: &[u8]) -> IdentifyController {
    IdentifyController {
        serial: d.ascii_at(4, 20),
        model: d.ascii_at(24, 40),
        firmware: d.ascii_at(64, 8),
    }
}

fn counter_at(d: &[u8], off: usize) -> u64 {
    if d.u64_at(off + 8) != 0 {
        u64::MAX
    } else {
        d.u64_at(off)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SmartLog {
    pub critical_warning: u8,
    pub composite_temperature_kelvin: u16,
    pub available_spare: u8,
    pub available_spare_threshold: u8,
    pub percentage_used: u8,
    pub power_cycles: u64,
    pub power_on_hours: u64,
    pub unsafe_shutdowns: u64,
    pub media_errors: u64,
    pub error_log_entries: u64,
}

impl SmartLog {
    pub fn celsius(&self) -> Option<i32> {
        (self.composite_temperature_kelvin != 0)
            .then(|| self.composite_temperature_kelvin as i32 - 273)
    }
}

pub fn parse_smart_log(d: &[u8]) -> SmartLog {
    SmartLog {
        critical_warning: d.u8_at(0),
        composite_temperature_kelvin: d.u16_at(1),
        available_spare: d.u8_at(3),
        available_spare_threshold: d.u8_at(4),
        percentage_used: d.u8_at(5),
        power_cycles: counter_at(d, 112),
        power_on_hours: counter_at(d, 128),
        unsafe_shutdowns: counter_at(d, 144),
        media_errors: counter_at(d, 160),
        error_log_entries: counter_at(d, 176),
    }
}

pub fn critical_warnings(bits: u8) -> Vec<&'static str> {
    [
        (0x01, "spare below threshold"),
        (0x02, "temperature threshold"),
        (0x04, "reliability degraded"),
        (0x08, "read only"),
        (0x10, "volatile backup failed"),
        (0x20, "persistent memory read only"),
    ]
    .into_iter()
    .filter(|(bit, _)| bits & bit != 0)
    .map(|(_, n)| n)
    .collect()
}
