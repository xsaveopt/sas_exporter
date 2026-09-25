use anyhow::{Result, bail};
use serde::Serialize;

use super::mpi::{
    FUNCTION_SCSI_IO, IOCSTATUS_SCSI_DATA_UNDERRUN, IOCSTATUS_SCSI_RECOVERED_ERROR,
    IOCSTATUS_SUCCESS, hex, ioc_status_name,
};
use super::transport::{Request, TIMEOUT_DEVICE, Transport};
use crate::bytes::{Le, LeMut};

pub const SCSI_IO_REQUEST_LEN: usize = 0x40;
pub const FLAGS_DATA_READ: u32 = 0x0008_0000;
const CDB_OFFSET: usize = 0x20;
const SCSI_STATE_SENSE_MASK: u8 = 0x03;
const SCSI_STATE_SENSE_VALID: u8 = 0x00;

pub const INQUIRY_LEN: usize = 36;
pub const VPD_LEN: usize = 255;
pub const READ_CAPACITY_LEN: usize = 32;
pub const LOG_SENSE_LEN: usize = 252;

pub const VPD_SERIAL: u8 = 0x80;
pub const VPD_DEVICE_ID: u8 = 0x83;
pub const VPD_BLOCK_CHARACTERISTICS: u8 = 0xB1;
pub const LOG_PAGE_TEMPERATURE: u8 = 0x0D;
pub const LOG_PAGE_INFORMATIONAL_EXCEPTIONS: u8 = 0x2F;

pub fn inquiry_cdb() -> Vec<u8> {
    vec![0x12, 0x00, 0x00, 0x00, INQUIRY_LEN as u8, 0x00]
}

pub fn vpd_cdb(page: u8) -> Vec<u8> {
    vec![0x12, 0x01, page, 0x00, VPD_LEN as u8, 0x00]
}

pub fn read_capacity16_cdb() -> Vec<u8> {
    let mut cdb = vec![0u8; 16];
    cdb[0] = 0x9E;
    cdb[1] = 0x10;
    cdb[10..14].copy_from_slice(&(READ_CAPACITY_LEN as u32).to_be_bytes());
    cdb
}

pub fn log_sense_cdb(page: u8) -> Vec<u8> {
    let mut cdb = vec![0u8; 10];
    cdb[0] = 0x4D;
    cdb[2] = 0x40 | (page & 0x3F);
    cdb[7..9].copy_from_slice(&(LOG_SENSE_LEN as u16).to_be_bytes());
    cdb
}

pub fn scsi_read_request(handle: u16, cdb: &[u8], len: usize) -> Request {
    let mut r = Request::new(FUNCTION_SCSI_IO, SCSI_IO_REQUEST_LEN);
    r.frame.put_u16(0x0A, handle);
    r.frame.put_u32(0x0C, FLAGS_DATA_READ);
    r.frame.put_u32(0x14, len as u32);
    r.frame.put_bytes(CDB_OFFSET, cdb);
    r.data_in_len = len;
    r.sense = true;
    r.timeout = TIMEOUT_DEVICE;
    r
}

pub fn scsi_read(t: &dyn Transport, handle: u16, cdb: &[u8], len: usize) -> Result<Vec<u8>> {
    let reply = scsi_read_request(handle, cdb, len).send(t)?;
    let status = reply.ioc_status();
    if !reply.is_address() {
        if status != IOCSTATUS_SUCCESS {
            bail!(
                "SCSI command 0x{:02x} to handle 0x{handle:04x} failed: IOCStatus 0x{status:04x} ({})",
                cdb.first().copied().unwrap_or(0),
                ioc_status_name(status)
            );
        }
        return Ok(reply.data_in);
    }
    let f = &reply.frame;
    let scsi_status = f.u8_at(0x10);
    let scsi_state = f.u8_at(0x11);
    let ok_status = matches!(
        status,
        IOCSTATUS_SUCCESS | IOCSTATUS_SCSI_RECOVERED_ERROR | IOCSTATUS_SCSI_DATA_UNDERRUN
    );
    if !ok_status || scsi_status != 0 {
        let sense =
            if scsi_state & SCSI_STATE_SENSE_MASK == SCSI_STATE_SENSE_VALID && f.u32_at(0x18) > 0 {
                sense_summary(&reply.sense)
            } else {
                String::new()
            };
        bail!(
            "SCSI command 0x{:02x} to handle 0x{handle:04x} failed: IOCStatus 0x{status:04x} ({}), SCSI status 0x{scsi_status:02x}{sense}",
            cdb.first().copied().unwrap_or(0),
            ioc_status_name(status)
        );
    }
    let count = (f.u32_at(0x14) as usize).min(reply.data_in.len());
    let mut data = reply.data_in;
    data.truncate(count);
    Ok(data)
}

pub fn sense_summary(sense: &[u8]) -> String {
    let (key, asc, ascq) = match sense.u8_at(0) & 0x7F {
        0x70 | 0x71 => (sense.u8_at(2) & 0x0F, sense.u8_at(12), sense.u8_at(13)),
        0x72 | 0x73 => (sense.u8_at(1) & 0x0F, sense.u8_at(2), sense.u8_at(3)),
        _ => return String::new(),
    };
    format!(", sense key 0x{key:x} ASC 0x{asc:02x} ASCQ 0x{ascq:02x}")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inquiry {
    pub peripheral_type: u8,
    pub vendor: String,
    pub product: String,
    pub revision: String,
}

pub fn parse_inquiry(d: &[u8]) -> Inquiry {
    Inquiry {
        peripheral_type: d.u8_at(0) & 0x1F,
        vendor: d.ascii_at(8, 8),
        product: d.ascii_at(16, 16),
        revision: d.ascii_at(32, 4),
    }
}

pub fn parse_serial_vpd(d: &[u8]) -> Option<String> {
    if d.u8_at(1) != VPD_SERIAL {
        return None;
    }
    let s = d.ascii_at(4, d.u8_at(3) as usize);
    (!s.is_empty()).then_some(s)
}

pub fn parse_naa_vpd(d: &[u8]) -> Option<String> {
    if d.u8_at(1) != VPD_DEVICE_ID {
        return None;
    }
    let end = (4 + u16::from_be_bytes([d.u8_at(2), d.u8_at(3)]) as usize).min(d.len());
    let mut o = 4;
    while o + 4 <= end {
        let association = (d.u8_at(o + 1) >> 4) & 0x03;
        let designator_type = d.u8_at(o + 1) & 0x0F;
        let len = d.u8_at(o + 3) as usize;
        let body = d.get(o + 4..(o + 4 + len).min(end)).unwrap_or(&[]);
        if association == 0 && designator_type == 0x3 && !body.is_empty() {
            return Some(hex(body));
        }
        o += 4 + len;
    }
    None
}

pub fn parse_rotation_rate(d: &[u8]) -> Option<u16> {
    if d.u8_at(1) != VPD_BLOCK_CHARACTERISTICS || d.len() < 6 {
        return None;
    }
    Some(u16::from_be_bytes([d.u8_at(4), d.u8_at(5)]))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capacity {
    pub last_lba: u64,
    pub block_size: u32,
}

pub fn parse_read_capacity16(d: &[u8]) -> Option<Capacity> {
    if d.len() < 12 {
        return None;
    }
    let mut lba = [0u8; 8];
    lba.copy_from_slice(&d[0..8]);
    let mut bs = [0u8; 4];
    bs.copy_from_slice(&d[8..12]);
    Some(Capacity {
        last_lba: u64::from_be_bytes(lba),
        block_size: u32::from_be_bytes(bs),
    })
}

pub fn size_mb(c: Capacity) -> u64 {
    ((c.last_lba as u128 + 1) * c.block_size as u128 / 1_048_576) as u64
}

fn log_parameters(d: &[u8], page: u8) -> Vec<(u16, &[u8])> {
    if d.u8_at(0) & 0x3F != page {
        return Vec::new();
    }
    let end = (4 + u16::from_be_bytes([d.u8_at(2), d.u8_at(3)]) as usize).min(d.len());
    let mut out = Vec::new();
    let mut o = 4;
    while o + 4 <= end {
        let code = u16::from_be_bytes([d.u8_at(o), d.u8_at(o + 1)]);
        let len = d.u8_at(o + 3) as usize;
        out.push((code, d.get(o + 4..(o + 4 + len).min(end)).unwrap_or(&[])));
        o += 4 + len;
    }
    out
}

pub fn parse_temperature_log(d: &[u8]) -> Option<u8> {
    log_parameters(d, LOG_PAGE_TEMPERATURE)
        .into_iter()
        .find(|(code, _)| *code == 0x0000)
        .and_then(|(_, body)| body.get(1).copied())
        .filter(|t| *t != 0xFF)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct InformationalExceptions {
    pub asc: u8,
    pub ascq: u8,
    pub most_recent_temperature: Option<u8>,
}

impl InformationalExceptions {
    pub fn failure_predicted(&self) -> bool {
        self.asc == 0x5D
    }
}

pub fn parse_ie_log(d: &[u8]) -> Option<InformationalExceptions> {
    let (_, body) = log_parameters(d, LOG_PAGE_INFORMATIONAL_EXCEPTIONS)
        .into_iter()
        .find(|(code, _)| *code == 0x0000)?;
    if body.len() < 2 {
        return None;
    }
    Some(InformationalExceptions {
        asc: body[0],
        ascq: body[1],
        most_recent_temperature: body.get(2).copied().filter(|t| *t != 0xFF),
    })
}

pub fn fahrenheit(celsius: i32) -> f64 {
    celsius as f64 * 9.0 / 5.0 + 32.0
}
