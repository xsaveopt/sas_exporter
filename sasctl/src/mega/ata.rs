use anyhow::{Result, bail};
use serde::Serialize;

use crate::mega::scsi::{inquiry_cdb, pthru_read};
use crate::mega::transport::Transport;

pub const ATA_PASS_THROUGH_16: u8 = 0x85;
pub const PROTOCOL_PIO_DATA_IN: u8 = 4;
pub const SMART: u8 = 0xb0;
pub const SMART_READ_DATA: u8 = 0xd0;
pub const SMART_READ_THRESHOLDS: u8 = 0xd1;
pub const SMART_LBA_MID: u8 = 0x4f;
pub const SMART_LBA_HIGH: u8 = 0xc2;
pub const SECTOR: usize = 512;
pub const ATTRIBUTES: usize = 30;
pub const ENTRY_LEN: usize = 12;
pub const INQUIRY_LEN: usize = 36;
pub const SAT_VENDOR: &[u8; 8] = b"ATA     ";

const T_DIR_FROM_DEVICE: u8 = 1 << 3;
const BYTE_BLOCK: u8 = 1 << 2;
const T_LENGTH_SECTOR_COUNT: u8 = 2;
const PREFAILURE: u16 = 1 << 0;

pub fn smart_read_cdb(feature: u8) -> [u8; 16] {
    let mut cdb = [0u8; 16];
    cdb[0] = ATA_PASS_THROUGH_16;
    cdb[1] = PROTOCOL_PIO_DATA_IN << 1;
    cdb[2] = T_DIR_FROM_DEVICE | BYTE_BLOCK | T_LENGTH_SECTOR_COUNT;
    cdb[4] = feature;
    cdb[6] = 1;
    cdb[10] = SMART_LBA_MID;
    cdb[12] = SMART_LBA_HIGH;
    cdb[14] = SMART;
    cdb
}

pub fn is_sat(inquiry: &[u8]) -> bool {
    inquiry.get(8..16) == Some(SAT_VENDOR.as_slice())
}

pub fn checksum_ok(sector: &[u8]) -> bool {
    sector.len() == SECTOR && sector.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub id: u8,
    pub flags: u16,
    pub prefailure: bool,
    pub value: u8,
    pub worst: u8,
    pub threshold: Option<u8>,
    pub raw: u64,
    pub failing_now: bool,
    pub failed_in_past: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AtaSmart {
    pub health: &'static str,
    pub data_checksum_ok: bool,
    pub thresholds_checksum_ok: bool,
    pub attributes: Vec<Attribute>,
}

pub fn parse_thresholds(sector: &[u8]) -> Vec<(u8, u8)> {
    (0..ATTRIBUTES)
        .filter_map(|i| {
            let at = 2 + i * ENTRY_LEN;
            let e = sector.get(at..at + ENTRY_LEN)?;
            (e[0] != 0).then_some((e[0], e[1]))
        })
        .collect()
}

pub fn parse_smart(data: &[u8], thresholds: &[u8]) -> Result<AtaSmart> {
    if data.len() < SECTOR || thresholds.len() < SECTOR {
        bail!("SMART data and thresholds must each be one 512-byte sector");
    }
    let limits = parse_thresholds(thresholds);
    let attributes: Vec<Attribute> = (0..ATTRIBUTES)
        .filter_map(|i| {
            let at = 2 + i * ENTRY_LEN;
            let e = &data[at..at + ENTRY_LEN];
            if e[0] == 0 {
                return None;
            }
            let flags = u16::from_le_bytes([e[1], e[2]]);
            let mut raw = [0u8; 8];
            raw[..6].copy_from_slice(&e[5..11]);
            let threshold = limits.iter().find(|(id, _)| *id == e[0]).map(|(_, t)| *t);
            let live = threshold.filter(|t| *t != 0);
            Some(Attribute {
                id: e[0],
                flags,
                prefailure: flags & PREFAILURE != 0,
                value: e[3],
                worst: e[4],
                threshold,
                raw: u64::from_le_bytes(raw),
                failing_now: live.is_some_and(|t| e[3] <= t),
                failed_in_past: live.is_some_and(|t| e[4] <= t),
            })
        })
        .collect();
    let health = if attributes.iter().any(|a| a.prefailure && a.failing_now) {
        "FAILED"
    } else {
        "PASSED"
    };
    Ok(AtaSmart {
        health,
        data_checksum_ok: checksum_ok(&data[..SECTOR]),
        thresholds_checksum_ok: checksum_ok(&thresholds[..SECTOR]),
        attributes,
    })
}

pub fn probe_sat(t: &dyn Transport, device_id: u16) -> Result<bool> {
    let inq = pthru_read(
        t,
        device_id,
        &inquiry_cdb(None, INQUIRY_LEN as u16),
        INQUIRY_LEN,
    )?;
    Ok(is_sat(&inq))
}

pub fn read_smart(t: &dyn Transport, device_id: u16) -> Result<AtaSmart> {
    let data = pthru_read(t, device_id, &smart_read_cdb(SMART_READ_DATA), SECTOR)?;
    let thresholds = pthru_read(t, device_id, &smart_read_cdb(SMART_READ_THRESHOLDS), SECTOR)?;
    parse_smart(&data, &thresholds)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bytes::Le;
    use crate::mega::mock::Mock;

    pub fn seal(mut s: Vec<u8>) -> Vec<u8> {
        s[511] = 0;
        let sum = s.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        s[511] = sum.wrapping_neg();
        s
    }

    pub fn smart_data(attrs: &[(u8, u16, u8, u8, u64)]) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR];
        s[0] = 0x10;
        for (i, (id, flags, value, worst, raw)) in attrs.iter().enumerate() {
            let at = 2 + i * ENTRY_LEN;
            s[at] = *id;
            s[at + 1..at + 3].copy_from_slice(&flags.to_le_bytes());
            s[at + 3] = *value;
            s[at + 4] = *worst;
            s[at + 5..at + 11].copy_from_slice(&raw.to_le_bytes()[..6]);
        }
        seal(s)
    }

    pub fn smart_thresholds(limits: &[(u8, u8)]) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR];
        s[0] = 0x10;
        for (i, (id, t)) in limits.iter().enumerate() {
            s[2 + i * ENTRY_LEN] = *id;
            s[3 + i * ENTRY_LEN] = *t;
        }
        seal(s)
    }

    pub fn sat_inquiry() -> Vec<u8> {
        let mut b = vec![0u8; INQUIRY_LEN];
        b[8..16].copy_from_slice(SAT_VENDOR);
        b[16..32].copy_from_slice(b"Samsung SSD 870 ");
        b
    }

    #[test]
    fn smart_read_cdbs_follow_the_sat_layout() {
        assert_eq!(
            smart_read_cdb(SMART_READ_DATA),
            [
                0x85, 0x08, 0x0e, 0, 0xd0, 0, 1, 0, 0, 0, 0x4f, 0, 0xc2, 0, 0xb0, 0
            ]
        );
        assert_eq!(smart_read_cdb(SMART_READ_THRESHOLDS)[4], 0xd1);
        assert_eq!(smart_read_cdb(SMART_READ_DATA)[2] & 0x20, 0);
    }

    #[test]
    fn detects_sat_from_the_inquiry_vendor() {
        assert!(is_sat(&sat_inquiry()));
        let mut sas = sat_inquiry();
        sas[8..16].copy_from_slice(b"SEAGATE ");
        assert!(!is_sat(&sas));
        assert!(!is_sat(&[0u8; 10]));
    }

    #[test]
    fn parses_attributes_and_thresholds() {
        let data = smart_data(&[
            (5, 0x0033, 100, 100, 12),
            (9, 0x0032, 95, 95, 0x0000_0102_0304),
            (194, 0x0022, 64, 40, 0x0028_0014_0024),
            (190, 0x0022, 30, 20, 30),
        ]);
        let thr = smart_thresholds(&[(5, 10), (9, 0), (194, 45), (190, 45)]);
        let s = parse_smart(&data, &thr).unwrap();
        assert!(s.data_checksum_ok && s.thresholds_checksum_ok);
        assert_eq!(s.health, "PASSED");
        assert_eq!(s.attributes.len(), 4);
        let a = &s.attributes[0];
        assert_eq!(
            (a.id, a.value, a.worst, a.threshold, a.raw),
            (5, 100, 100, Some(10), 12)
        );
        assert!(a.prefailure && !a.failing_now);
        assert_eq!(s.attributes[1].raw, 0x0102_0304);
        assert!(!s.attributes[1].failing_now);
        let t = &s.attributes[2];
        assert!(!t.prefailure && !t.failing_now && t.failed_in_past);
        let airflow = &s.attributes[3];
        assert!(airflow.failing_now && !airflow.prefailure);
    }

    #[test]
    fn a_prefailure_attribute_at_threshold_fails_health() {
        let data = smart_data(&[(5, 0x0033, 10, 10, 4000)]);
        let thr = smart_thresholds(&[(5, 10)]);
        let s = parse_smart(&data, &thr).unwrap();
        assert_eq!(s.health, "FAILED");
        assert!(s.attributes[0].failing_now);
        let mut bad = data.clone();
        bad[100] ^= 1;
        assert!(!parse_smart(&bad, &thr).unwrap().data_checksum_ok);
        assert!(parse_smart(&data[..100], &thr).is_err());
    }

    #[test]
    fn reads_smart_through_the_scsi_passthrough_frame() {
        let data = smart_data(&[(5, 0x0033, 100, 100, 0)]);
        let thr = smart_thresholds(&[(5, 10)]);
        let mock = Mock::new()
            .scsi(12, &[0x12, 0], sat_inquiry())
            .scsi(12, &[0x85, 0x08, 0x0e, 0, 0xd0], data)
            .scsi(12, &[0x85, 0x08, 0x0e, 0, 0xd1], thr);
        assert!(probe_sat(&mock, 12).unwrap());
        let s = read_smart(&mock, 12).unwrap();
        assert_eq!(s.attributes[0].threshold, Some(10));
        let calls = mock.calls();
        assert_eq!(calls.len(), 3);
        let inq = &calls[0];
        assert_eq!(inq.frame[6], 6);
        assert_eq!(inq.frame.u32_at(0x14), 36);
        for c in &calls[1..] {
            assert_eq!(c.frame[0], 0x04);
            assert_eq!(c.frame[2], 0xff);
            assert_eq!(c.frame[4], 12);
            assert_eq!(c.frame[5], 0);
            assert_eq!(c.frame[6], 16);
            assert_eq!(c.frame[7], 1);
            assert_eq!(c.frame.u16_at(0x10), 0x0010);
            assert_eq!(c.frame.u16_at(0x12), 0);
            assert_eq!(c.frame.u32_at(0x14), 512);
            assert_eq!(c.sgl_off, 0x30);
            assert_eq!(c.bufs[0].len(), 512);
        }
        assert_eq!(&calls[1].frame[0x20..0x30], &smart_read_cdb(0xd0));
        assert_eq!(&calls[2].frame[0x20..0x30], &smart_read_cdb(0xd1));
    }
}
