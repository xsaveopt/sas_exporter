use anyhow::{Result, bail};
use serde::Serialize;

use crate::mega::dcmd::MfiError;
use crate::mega::mfi::{
    Dir, HDR_STATUS, PTHRU_SENSE_OFFSET, PTHRU_SGL_OFFSET, STATUS_OK, STATUS_SCSI_DONE_WITH_ERROR,
    pthru_frame,
};
use crate::mega::transport::Transport;

pub const SENSE_LEN: usize = 96;
pub const LOG_SENSE: u8 = 0x4d;
pub const INQUIRY: u8 = 0x12;
pub const PAGE_TEMPERATURE: u8 = 0x0d;
pub const PAGE_INFORMATIONAL_EXCEPTIONS: u8 = 0x2f;
pub const VPD_SERIAL: u8 = 0x80;
pub const PASSTHROUGH_OPCODE: u32 = 0x0400_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
}

pub fn parse_sense(s: &[u8]) -> Option<Sense> {
    match s.first()? & 0x7f {
        0x70 | 0x71 if s.len() >= 14 => Some(Sense {
            key: s[2] & 0x0f,
            asc: s[12],
            ascq: s[13],
        }),
        0x72 | 0x73 if s.len() >= 4 => Some(Sense {
            key: s[1] & 0x0f,
            asc: s[2],
            ascq: s[3],
        }),
        _ => None,
    }
}

pub fn pthru_read(t: &dyn Transport, device_id: u16, cdb: &[u8], len: usize) -> Result<Vec<u8>> {
    let Ok(target) = u8::try_from(device_id) else {
        bail!(
            "device id {device_id} is above 255 and cannot be addressed by a SCSI passthrough frame"
        );
    };
    let mut frame = pthru_frame(target, cdb, Dir::Read, len as u32, SENSE_LEN as u8);
    let mut buf = vec![0u8; len];
    let mut sense = vec![0u8; SENSE_LEN];
    {
        let mut bufs: [&mut [u8]; 1] = [&mut buf];
        t.firmware(
            &mut frame,
            PTHRU_SGL_OFFSET,
            &mut bufs,
            Some((PTHRU_SENSE_OFFSET, &mut sense)),
        )?;
    }
    match frame[HDR_STATUS] {
        STATUS_OK => Ok(buf),
        STATUS_SCSI_DONE_WITH_ERROR => match parse_sense(&sense) {
            Some(s) => bail!(
                "SCSI command {:#04x} to device {device_id} failed, sense key {:#x} asc {:#04x} ascq {:#04x}",
                cdb[0],
                s.key,
                s.asc,
                s.ascq
            ),
            None => bail!(
                "SCSI command {:#04x} to device {device_id} failed without sense data",
                cdb[0]
            ),
        },
        status => Err(MfiError {
            opcode: PASSTHROUGH_OPCODE | u32::from(cdb[0]),
            status,
        }
        .into()),
    }
}

pub fn log_sense_cdb(page: u8, subpage: u8, alloc: u16) -> [u8; 10] {
    let [hi, lo] = alloc.to_be_bytes();
    [
        LOG_SENSE,
        0,
        0x40 | (page & 0x3f),
        subpage,
        0,
        0,
        0,
        hi,
        lo,
        0,
    ]
}

pub fn inquiry_cdb(vpd_page: Option<u8>, alloc: u16) -> [u8; 6] {
    let [hi, lo] = alloc.to_be_bytes();
    match vpd_page {
        Some(p) => [INQUIRY, 1, p, hi, lo, 0],
        None => [INQUIRY, 0, 0, hi, lo, 0],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogParam {
    pub code: u16,
    pub data: Vec<u8>,
}

pub fn parse_log_page(b: &[u8]) -> (u8, Vec<LogParam>) {
    if b.len() < 4 {
        return (0, Vec::new());
    }
    let page = b[0] & 0x3f;
    let end = (4 + usize::from(u16::from_be_bytes([b[2], b[3]]))).min(b.len());
    let mut params = Vec::new();
    let mut at = 4;
    while at + 4 <= end {
        let code = u16::from_be_bytes([b[at], b[at + 1]]);
        let len = usize::from(b[at + 3]);
        let data_end = (at + 4 + len).min(end);
        params.push(LogParam {
            code,
            data: b[at + 4..data_end].to_vec(),
        });
        at += 4 + len;
    }
    (page, params)
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemperatureLog {
    pub current_celsius: Option<u8>,
    pub reference_celsius: Option<u8>,
}

pub fn parse_temperature_page(b: &[u8]) -> Result<TemperatureLog> {
    let (page, params) = parse_log_page(b);
    if page != PAGE_TEMPERATURE {
        bail!("expected log page 0x0d, got {page:#04x}");
    }
    let value = |code: u16| {
        params
            .iter()
            .find(|p| p.code == code)
            .and_then(|p| p.data.get(1).copied())
            .filter(|v| *v != 0xff)
    };
    Ok(TemperatureLog {
        current_celsius: value(0x0000),
        reference_celsius: value(0x0001),
    })
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct InformationalExceptions {
    pub asc: u8,
    pub ascq: u8,
    pub failure_predicted: bool,
    pub temperature_celsius: Option<u8>,
}

pub fn parse_ie_page(b: &[u8]) -> Result<InformationalExceptions> {
    let (page, params) = parse_log_page(b);
    if page != PAGE_INFORMATIONAL_EXCEPTIONS {
        bail!("expected log page 0x2f, got {page:#04x}");
    }
    let Some(p) = params.iter().find(|p| p.code == 0) else {
        bail!("log page 0x2f has no parameter 0");
    };
    let asc = p.data.first().copied().unwrap_or(0);
    let ascq = p.data.get(1).copied().unwrap_or(0);
    Ok(InformationalExceptions {
        asc,
        ascq,
        failure_predicted: asc == 0x5d,
        temperature_celsius: p.data.get(2).copied().filter(|v| *v != 0 && *v != 0xff),
    })
}

pub fn parse_vpd_serial(b: &[u8]) -> Option<String> {
    if b.len() < 4 || b[1] != VPD_SERIAL {
        return None;
    }
    let len = usize::from(b[3]).min(b.len() - 4);
    let s: String = b[4..4 + len]
        .iter()
        .filter(|c| c.is_ascii_graphic() || **c == b' ')
        .map(|c| *c as char)
        .collect();
    Some(s.trim().to_string())
}

pub fn temperature(t: &dyn Transport, device_id: u16) -> Result<TemperatureLog> {
    let b = pthru_read(t, device_id, &log_sense_cdb(PAGE_TEMPERATURE, 0, 64), 64)?;
    parse_temperature_page(&b)
}

pub fn informational_exceptions(
    t: &dyn Transport,
    device_id: u16,
) -> Result<InformationalExceptions> {
    let b = pthru_read(
        t,
        device_id,
        &log_sense_cdb(PAGE_INFORMATIONAL_EXCEPTIONS, 0, 64),
        64,
    )?;
    parse_ie_page(&b)
}

pub fn serial_number(t: &dyn Transport, device_id: u16) -> Result<Option<String>> {
    let b = pthru_read(t, device_id, &inquiry_cdb(Some(VPD_SERIAL), 64), 64)?;
    Ok(parse_vpd_serial(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mega::dcmd::status_of;
    use crate::mega::mock::Mock;

    fn temp_page(current: u8, reference: u8) -> Vec<u8> {
        vec![
            0x0d, 0, 0, 12, 0, 0, 3, 2, 0, current, 0, 1, 3, 2, 0, reference,
        ]
    }

    #[test]
    fn builds_standard_cdbs() {
        assert_eq!(
            log_sense_cdb(0x0d, 0, 64),
            [0x4d, 0, 0x4d, 0, 0, 0, 0, 0, 64, 0]
        );
        assert_eq!(
            log_sense_cdb(0x2f, 0, 0x0200),
            [0x4d, 0, 0x6f, 0, 0, 0, 0, 2, 0, 0]
        );
        assert_eq!(inquiry_cdb(Some(0x80), 64), [0x12, 1, 0x80, 0, 64, 0]);
    }

    #[test]
    fn parses_log_pages() {
        let t = parse_temperature_page(&temp_page(38, 65)).unwrap();
        assert_eq!(t.current_celsius, Some(38));
        assert_eq!(t.reference_celsius, Some(65));
        assert_eq!(
            parse_temperature_page(&temp_page(0xff, 65))
                .unwrap()
                .current_celsius,
            None
        );
        let ie = parse_ie_page(&[0x2f, 0, 0, 8, 0, 0, 3, 4, 0x5d, 0x10, 40, 0]).unwrap();
        assert!(ie.failure_predicted);
        assert_eq!(
            (ie.asc, ie.ascq, ie.temperature_celsius),
            (0x5d, 0x10, Some(40))
        );
        assert!(parse_ie_page(&temp_page(1, 2)).is_err());
    }

    #[test]
    fn parses_serial_vpd_and_sense() {
        assert_eq!(
            parse_vpd_serial(&[0, 0x80, 0, 6, b' ', b'Z', b'1', b'2', b'3', 0]).unwrap(),
            "Z123"
        );
        let fixed = [0x70, 0, 0x05, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x24, 0x00];
        assert_eq!(
            parse_sense(&fixed),
            Some(Sense {
                key: 5,
                asc: 0x24,
                ascq: 0
            })
        );
        assert_eq!(
            parse_sense(&[0x72, 0x06, 0x29, 0x01]),
            Some(Sense {
                key: 6,
                asc: 0x29,
                ascq: 1
            })
        );
    }

    #[test]
    fn passthrough_frame_carries_sense_pointer_and_sgl_offsets() {
        let mock = Mock::new().scsi(8, &[0x4d, 0, 0x4d], temp_page(41, 60));
        let t = temperature(&mock, 8).unwrap();
        assert_eq!(t.current_celsius, Some(41));
        let call = &mock.calls()[0];
        assert_eq!(call.sgl_off, 0x30);
        assert_eq!(call.sense_off, Some(0x18));
        assert_eq!(call.frame[0], 0x04);
        assert_eq!(call.frame[1], 96);
        assert_eq!(call.frame[4], 8);
        assert_eq!(call.frame[6], 10);
    }

    #[test]
    fn passthrough_errors_carry_sense_or_status() {
        let sense = vec![0x70, 0, 0x05, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x24, 0x00];
        let mock = Mock::new()
            .scsi_error(8, &[0x4d], STATUS_SCSI_DONE_WITH_ERROR, sense)
            .scsi_error(9, &[0x4d], 0x0c, vec![]);
        let e = temperature(&mock, 8).unwrap_err();
        assert!(e.to_string().contains("sense key 0x5 asc 0x24"));
        let e = temperature(&mock, 9).unwrap_err();
        assert_eq!(status_of(&e), Some(0x0c));
        assert!(temperature(&mock, 300).is_err());
        assert_eq!(mock.calls().len(), 2);
    }
}
