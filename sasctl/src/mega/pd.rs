use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::bytes::Le;
use crate::mega::ctrl::bit;
use crate::mega::dcmd::{dcmd_none, dcmd_read, dcmd_variable};
use crate::mega::mfi::{Mbox, op};
use crate::mega::transport::Transport;

pub const PD_INFO_LEN: usize = 512;
pub const PD_ENTRY_LEN: usize = 24;
pub const NO_ENCLOSURE: u16 = 0xffff;
pub const SCSI_TYPE_ENCLOSURE: u8 = 0x0d;

pub const STATE_UNCONFIGURED_GOOD: u16 = 0x00;
pub const STATE_UNCONFIGURED_BAD: u16 = 0x01;
pub const STATE_HOT_SPARE: u16 = 0x02;
pub const STATE_OFFLINE: u16 = 0x10;
pub const STATE_FAILED: u16 = 0x11;
pub const STATE_REBUILD: u16 = 0x14;
pub const STATE_ONLINE: u16 = 0x18;
pub const STATE_COPYBACK: u16 = 0x20;
pub const STATE_SYSTEM: u16 = 0x40;

pub fn state_name(fw_state: u16, global_spare: bool) -> String {
    match fw_state {
        STATE_UNCONFIGURED_GOOD => "UGood".into(),
        STATE_UNCONFIGURED_BAD => "UBad".into(),
        STATE_HOT_SPARE if global_spare => "GHS".into(),
        STATE_HOT_SPARE => "DHS".into(),
        STATE_OFFLINE => "Offln".into(),
        STATE_FAILED => "Failed".into(),
        STATE_REBUILD => "Rbld".into(),
        STATE_ONLINE => "Onln".into(),
        STATE_COPYBACK => "Cpybck".into(),
        STATE_SYSTEM => "JBOD".into(),
        other => format!("{other:#04x}"),
    }
}

pub fn sas_address(v: u64) -> String {
    format!("{v:#018x}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DriveAddress {
    pub enclosure: u16,
    pub slot: u8,
}

impl FromStr for DriveAddress {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let bad = || {
            anyhow!(
                "drive address {s:?} is not enclosure:slot (or slot for a drive without enclosure)"
            )
        };
        match s.split_once(':') {
            Some((e, slot)) => Ok(Self {
                enclosure: if e.is_empty() {
                    NO_ENCLOSURE
                } else {
                    e.parse().map_err(|_| bad())?
                },
                slot: slot.parse().map_err(|_| bad())?,
            }),
            None => Ok(Self {
                enclosure: NO_ENCLOSURE,
                slot: s.parse().map_err(|_| bad())?,
            }),
        }
    }
}

impl fmt::Display for DriveAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.enclosure == NO_ENCLOSURE {
            write!(f, ":{}", self.slot)
        } else {
            write!(f, "{}:{}", self.enclosure, self.slot)
        }
    }
}

impl Serialize for DriveAddress {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct PdAddress {
    pub device_id: u16,
    pub encl_device_id: u16,
    pub encl_index: u8,
    pub slot: u8,
    pub scsi_dev_type: u8,
    pub connected_ports: u8,
    pub sas_addresses: Vec<String>,
}

impl PdAddress {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            device_id: b.u16_at(0),
            encl_device_id: b.u16_at(2),
            encl_index: b.u8_at(4),
            slot: b.u8_at(5),
            scsi_dev_type: b.u8_at(6),
            connected_ports: b.u8_at(7),
            sas_addresses: vec![sas_address(b.u64_at(8)), sas_address(b.u64_at(16))],
        }
    }

    pub fn address(&self) -> DriveAddress {
        DriveAddress {
            enclosure: self.encl_device_id,
            slot: self.slot,
        }
    }

    pub fn is_enclosure(&self) -> bool {
        self.scsi_dev_type == SCSI_TYPE_ENCLOSURE
    }
}

pub fn parse_pd_list(b: &[u8]) -> Vec<PdAddress> {
    let count = b.u32_at(4) as usize;
    let fits = b.len().saturating_sub(8) / PD_ENTRY_LEN;
    (0..count.min(fits))
        .map(|i| PdAddress::parse(&b[8 + i * PD_ENTRY_LEN..8 + (i + 1) * PD_ENTRY_LEN]))
        .collect()
}

pub fn drives(list: &[PdAddress]) -> impl Iterator<Item = &PdAddress> {
    list.iter().filter(|a| !a.is_enclosure())
}

pub fn resolve(list: &[PdAddress], addr: DriveAddress) -> Result<&PdAddress> {
    drives(list)
        .find(|a| a.address() == addr)
        .ok_or_else(|| anyhow!("no drive at {addr}"))
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub raw: u16,
    pub percent: f64,
    pub elapsed_seconds: u16,
    pub remaining_seconds: Option<u64>,
}

impl Progress {
    pub fn parse(b: &[u8], off: usize) -> Self {
        let raw = b.u16_at(off);
        let elapsed = b.u16_at(off + 2);
        let remaining = if raw > 0 {
            let total = 65536u64 * u64::from(elapsed) / u64::from(raw);
            Some(total.saturating_sub(u64::from(elapsed)))
        } else {
            None
        };
        Self {
            raw,
            percent: (f64::from(raw) * 100.0 / 65535.0 * 100.0).round() / 100.0,
            elapsed_seconds: elapsed,
            remaining_seconds: remaining,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PdProgress {
    pub rebuild: Option<Progress>,
    pub patrol: Option<Progress>,
    pub clear: Option<Progress>,
    pub erase: Option<Progress>,
    pub copyback_active: bool,
    pub locate_active: bool,
    pub paused: Vec<&'static str>,
}

impl PdProgress {
    pub fn parse(b: &[u8], off: usize) -> Self {
        let active = |bit_no: u32| bit(b, off, bit_no);
        let when = |bit_no: u32, at: usize| active(bit_no).then(|| Progress::parse(b, off + at));
        Self {
            rebuild: when(0, 4),
            patrol: when(1, 8),
            clear: when(2, 12),
            erase: when(4, 12),
            copyback_active: active(3),
            locate_active: active(5),
            paused: crate::mega::ctrl::flags(
                b,
                off + 16,
                &["rebuild", "patrol", "clear", "copyback", "erase"],
            ),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Security {
    pub fde_capable: bool,
    pub fde_enabled: bool,
    pub secured: bool,
    pub locked: bool,
    pub foreign: bool,
    pub needs_ekm: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PdInfo {
    pub device_id: u16,
    pub seq_num: u16,
    pub vendor: String,
    pub product: String,
    pub revision: String,
    pub scsi_dev_type: u8,
    pub connected_ports: u8,
    pub device_speed: u8,
    pub link_speed: u8,
    pub media_errors: u32,
    pub other_errors: u32,
    pub predictive_failures: u32,
    pub last_predictive_failure_event: u32,
    pub fw_state: u16,
    pub state: String,
    pub in_vd: bool,
    pub is_global_spare: bool,
    pub is_spare: bool,
    pub is_foreign: bool,
    pub interface_code: u8,
    pub path_count: u8,
    pub sas_addresses: Vec<String>,
    pub raw_blocks: u64,
    pub non_coerced_blocks: u64,
    pub coerced_blocks: u64,
    pub encl_device_id: u16,
    pub encl_index: u8,
    pub slot: u8,
    pub progress: PdProgress,
    pub bad_block_table_full: bool,
    pub unusable_in_current_config: u8,
    pub power_state: u8,
    pub encl_position: u8,
    pub allowed_ops: u32,
    pub security: Security,
    pub media_type: u8,
    pub interface_type: u8,
    pub temperature_celsius: u8,
    pub emulated_block_size: u8,
    pub user_data_block_size: u16,
    pub ncq: bool,
    pub write_cache_enabled: bool,
    pub shield_counter: u8,
    pub bbm_error_count: Option<u32>,
}

impl PdInfo {
    pub fn parse(b: &[u8]) -> Self {
        let fw_state = b.u16_at(184);
        let is_global_spare = bit(b, 188, 2);
        let bbm = b.u32_at(424);
        Self {
            device_id: b.u16_at(0),
            seq_num: b.u16_at(2),
            vendor: b.ascii_at(4 + 8, 8),
            product: b.ascii_at(4 + 16, 16),
            revision: b.ascii_at(4 + 32, 4),
            scsi_dev_type: b.u8_at(165),
            connected_ports: b.u8_at(166),
            device_speed: b.u8_at(167),
            link_speed: b.u8_at(187),
            media_errors: b.u32_at(168),
            other_errors: b.u32_at(172),
            predictive_failures: b.u32_at(176),
            last_predictive_failure_event: b.u32_at(180),
            fw_state,
            state: state_name(fw_state, is_global_spare),
            in_vd: bit(b, 188, 1),
            is_global_spare,
            is_spare: bit(b, 188, 3),
            is_foreign: bit(b, 188, 4),
            interface_code: b.u8_at(189) >> 4,
            path_count: b.u8_at(192),
            sas_addresses: vec![sas_address(b.u64_at(200)), sas_address(b.u64_at(208))],
            raw_blocks: b.u64_at(232),
            non_coerced_blocks: b.u64_at(240),
            coerced_blocks: b.u64_at(248),
            encl_device_id: b.u16_at(256),
            encl_index: b.u8_at(258),
            slot: b.u8_at(259),
            progress: PdProgress::parse(b, 260),
            bad_block_table_full: b.u8_at(292) != 0,
            unusable_in_current_config: b.u8_at(293),
            power_state: b.u8_at(358),
            encl_position: b.u8_at(359),
            allowed_ops: b.u32_at(360),
            security: Security {
                fde_capable: bit(b, 368, 0),
                fde_enabled: bit(b, 368, 1),
                secured: bit(b, 368, 2),
                locked: bit(b, 368, 3),
                foreign: bit(b, 368, 4),
                needs_ekm: bit(b, 368, 5),
            },
            media_type: b.u8_at(370),
            interface_type: b.u8_at(401),
            temperature_celsius: b.u8_at(402),
            emulated_block_size: b.u8_at(403),
            user_data_block_size: b.u16_at(404),
            ncq: bit(b, 408, 5),
            write_cache_enabled: bit(b, 408, 6),
            shield_counter: b.u8_at(420),
            bbm_error_count: (bbm & 1 == 1).then_some(bbm >> 1),
        }
    }

    pub fn address(&self) -> DriveAddress {
        DriveAddress {
            enclosure: self.encl_device_id,
            slot: self.slot,
        }
    }

    pub fn model(&self) -> String {
        [self.vendor.as_str(), self.product.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn get_list(t: &dyn Transport) -> Result<Vec<PdAddress>> {
    Ok(parse_pd_list(&dcmd_variable(
        t,
        op::PD_GET_LIST,
        &Mbox::new(),
    )?))
}

pub fn get_info(t: &dyn Transport, device_id: u16) -> Result<PdInfo> {
    let buf = dcmd_read(
        t,
        op::PD_GET_INFO,
        &Mbox::new().short(0, device_id),
        PD_INFO_LEN,
    )?;
    Ok(PdInfo::parse(&buf))
}

pub fn set_state(t: &dyn Transport, device_id: u16, state: u16) -> Result<()> {
    let info = get_info(t, device_id)?;
    let mbox = Mbox::pd_ref(info.device_id, info.seq_num).short(2, state);
    dcmd_none(t, op::PD_STATE_SET, &mbox)
}

pub fn locate(t: &dyn Transport, device_id: u16, on: bool) -> Result<()> {
    let opcode = if on {
        op::PD_LOCATE_START
    } else {
        op::PD_LOCATE_STOP
    };
    dcmd_none(t, opcode, &Mbox::new().short(0, device_id))
}

pub fn rebuild_start(t: &dyn Transport, device_id: u16) -> Result<()> {
    let mut info = get_info(t, device_id)?;
    if info.fw_state != STATE_REBUILD {
        set_state(t, device_id, STATE_REBUILD)?;
        info = get_info(t, device_id)?;
        if info.fw_state != STATE_REBUILD {
            bail!(
                "drive {} did not enter the rebuild state (now {})",
                info.address(),
                info.state
            );
        }
    }
    dcmd_none(
        t,
        op::PD_REBUILD_START,
        &Mbox::pd_ref(info.device_id, info.seq_num),
    )
}

pub fn rebuild_stop(t: &dyn Transport, device_id: u16) -> Result<()> {
    let info = get_info(t, device_id)?;
    if info.fw_state != STATE_REBUILD && info.progress.rebuild.is_none() {
        bail!(
            "drive {} is not rebuilding (state {})",
            info.address(),
            info.state
        );
    }
    dcmd_none(
        t,
        op::PD_REBUILD_ABORT,
        &Mbox::pd_ref(info.device_id, info.seq_num),
    )
}

pub fn clear_start(t: &dyn Transport, device_id: u16) -> Result<()> {
    let info = get_info(t, device_id)?;
    dcmd_none(
        t,
        op::PD_CLEAR_START,
        &Mbox::pd_ref(info.device_id, info.seq_num),
    )
}

pub fn clear_stop(t: &dyn Transport, device_id: u16) -> Result<()> {
    let info = get_info(t, device_id)?;
    dcmd_none(
        t,
        op::PD_CLEAR_ABORT,
        &Mbox::pd_ref(info.device_id, info.seq_num),
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bytes::LeMut;
    use crate::mega::mock::Mock;

    pub fn pd_list_bytes(entries: &[(u16, u16, u8, u8)]) -> Vec<u8> {
        let mut b = vec![0u8; 8 + entries.len() * PD_ENTRY_LEN];
        let size = b.len() as u32;
        b.put_u32(0, size);
        b.put_u32(4, entries.len() as u32);
        for (i, (dev, encl, slot, ty)) in entries.iter().enumerate() {
            let at = 8 + i * PD_ENTRY_LEN;
            b.put_u16(at, *dev);
            b.put_u16(at + 2, *encl);
            b[at + 4] = 0;
            b[at + 5] = *slot;
            b[at + 6] = *ty;
            b[at + 7] = 1;
            b.put_u64(at + 8, 0x5000_c500_0000_0000 | u64::from(*dev));
        }
        b
    }

    pub fn pd_info_bytes(dev: u16, seq: u16, state: u16, encl: u16, slot: u8, temp: u8) -> Vec<u8> {
        let mut b = vec![0u8; PD_INFO_LEN];
        b.put_u16(0, dev);
        b.put_u16(2, seq);
        b[4..4 + 36].copy_from_slice(b"\0\0\0\0\0\0\0\0SEAGATE ST4000NM0025    E004");
        b[165] = 0;
        b.put_u32(168, 3);
        b.put_u32(172, 5);
        b.put_u32(176, 1);
        b.put_u16(184, state);
        b.put_u16(188, 0b0000_0110 | 0x1000);
        b[192] = 2;
        b.put_u64(200, 0x5000_c500_1234_5678);
        b.put_u64(232, 7_814_037_168);
        b.put_u64(240, 7_813_988_016);
        b.put_u64(248, 7_812_939_776);
        b.put_u16(256, encl);
        b[259] = slot;
        b.put_u32(260, 0b1);
        b.put_u16(264, 32768);
        b.put_u16(266, 600);
        b[368] = 0b1;
        b[370] = 0;
        b[402] = temp;
        b.put_u16(404, 512);
        b[408] = 1 << 5;
        b.put_u32(424, 7 << 1 | 1);
        b
    }

    #[test]
    fn parses_pd_list_entries() {
        let list = parse_pd_list(&pd_list_bytes(&[
            (8, 252, 0, 0),
            (9, 252, 1, 0),
            (252, 0xffff, 0, 0x0d),
        ]));
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].device_id, 9);
        assert_eq!(
            list[1].address(),
            DriveAddress {
                enclosure: 252,
                slot: 1
            }
        );
        assert!(list[2].is_enclosure());
        assert_eq!(drives(&list).count(), 2);
        assert_eq!(list[0].sas_addresses[0], "0x5000c50000000008");
    }

    #[test]
    fn pd_list_count_is_bounded_by_the_buffer() {
        let mut b = pd_list_bytes(&[(8, 252, 0, 0)]);
        b.put_u32(4, 99);
        assert_eq!(parse_pd_list(&b).len(), 1);
    }

    #[test]
    fn parses_pd_info() {
        let info = PdInfo::parse(&pd_info_bytes(8, 3, STATE_HOT_SPARE, 252, 4, 38));
        assert_eq!(info.device_id, 8);
        assert_eq!(info.seq_num, 3);
        assert_eq!(info.vendor, "SEAGATE");
        assert_eq!(info.product, "ST4000NM0025");
        assert_eq!(info.revision, "E004");
        assert_eq!(info.model(), "SEAGATE ST4000NM0025");
        assert_eq!(info.media_errors, 3);
        assert_eq!(info.other_errors, 5);
        assert_eq!(info.predictive_failures, 1);
        assert_eq!(info.state, "GHS");
        assert!(info.in_vd && info.is_global_spare && !info.is_spare);
        assert_eq!(info.interface_code, 1);
        assert_eq!(info.coerced_blocks, 7_812_939_776);
        assert_eq!(info.address().to_string(), "252:4");
        assert_eq!(info.temperature_celsius, 38);
        assert!(info.security.fde_capable);
        assert!(info.ncq && !info.write_cache_enabled);
        assert_eq!(info.bbm_error_count, Some(7));
        let rb = info.progress.rebuild.unwrap();
        assert_eq!(rb.raw, 32768);
        assert_eq!(rb.percent, 50.0);
        assert_eq!(rb.elapsed_seconds, 600);
        assert_eq!(rb.remaining_seconds, Some(600));
        assert!(info.progress.patrol.is_none());
    }

    #[test]
    fn state_names_follow_the_storcli_table() {
        assert_eq!(state_name(0x00, false), "UGood");
        assert_eq!(state_name(0x02, false), "DHS");
        assert_eq!(state_name(0x18, false), "Onln");
        assert_eq!(state_name(0x40, false), "JBOD");
        assert_eq!(state_name(0x33, false), "0x33");
    }

    #[test]
    fn drive_addresses_parse_and_print() {
        assert_eq!(
            "252:3".parse::<DriveAddress>().unwrap(),
            DriveAddress {
                enclosure: 252,
                slot: 3
            }
        );
        assert_eq!("5".parse::<DriveAddress>().unwrap().enclosure, NO_ENCLOSURE);
        assert_eq!(":5".parse::<DriveAddress>().unwrap().to_string(), ":5");
        assert!("x:1".parse::<DriveAddress>().is_err());
        assert!("252:300".parse::<DriveAddress>().is_err());
    }

    #[test]
    fn resolve_skips_enclosures() {
        let list = parse_pd_list(&pd_list_bytes(&[(252, 252, 0, 0x0d), (8, 252, 0, 0)]));
        assert_eq!(
            resolve(&list, "252:0".parse().unwrap()).unwrap().device_id,
            8
        );
        assert!(resolve(&list, "252:9".parse().unwrap()).is_err());
    }

    #[test]
    fn state_set_uses_fresh_pdref_and_state_in_s2() {
        let mock = Mock::new()
            .reply(
                op::PD_GET_INFO,
                pd_info_bytes(0x0108, 0x0a0b, STATE_UNCONFIGURED_GOOD, 252, 0, 30),
            )
            .reply(op::PD_STATE_SET, vec![]);
        set_state(&mock, 0x0108, STATE_SYSTEM).unwrap();
        let calls = mock.calls();
        assert_eq!(calls[0].mbox()[..2], [0x08, 0x01]);
        assert_eq!(calls[1].opcode(), op::PD_STATE_SET);
        assert_eq!(calls[1].mbox()[..6], [0x08, 0x01, 0x0b, 0x0a, 0x40, 0x00]);
        assert!(calls[1].bufs.is_empty());
    }

    #[test]
    fn locate_sends_device_id_with_zero_padding() {
        let mock = Mock::new()
            .reply(op::PD_LOCATE_START, vec![])
            .reply(op::PD_LOCATE_STOP, vec![]);
        locate(&mock, 0x0203, true).unwrap();
        locate(&mock, 0x0203, false).unwrap();
        let calls = mock.calls();
        assert_eq!(calls[0].opcode(), 0x0207_0100);
        assert_eq!(calls[1].opcode(), 0x0207_0200);
        assert_eq!(calls[0].mbox()[..4], [0x03, 0x02, 0, 0]);
    }

    #[test]
    fn rebuild_start_on_a_rebuild_state_drive_starts_directly() {
        let mock = Mock::new()
            .reply(
                op::PD_GET_INFO,
                pd_info_bytes(9, 4, STATE_REBUILD, 252, 1, 30),
            )
            .reply(op::PD_REBUILD_START, vec![]);
        rebuild_start(&mock, 9).unwrap();
        assert_eq!(mock.opcodes(), vec![op::PD_GET_INFO, op::PD_REBUILD_START]);
        assert_eq!(mock.calls()[1].mbox()[..4], [9, 0, 4, 0]);
    }

    #[test]
    fn clear_start_and_stop_send_a_fresh_pdref_like_mfiutil() {
        let mock = Mock::new()
            .reply(
                op::PD_GET_INFO,
                pd_info_bytes(0x0112, 0x0304, STATE_UNCONFIGURED_GOOD, 252, 2, 30),
            )
            .reply(op::PD_CLEAR_START, vec![])
            .reply(op::PD_CLEAR_ABORT, vec![]);
        clear_start(&mock, 0x0112).unwrap();
        clear_stop(&mock, 0x0112).unwrap();
        let calls = mock.calls();
        assert_eq!(
            mock.opcodes(),
            vec![op::PD_GET_INFO, 0x0205_0100, op::PD_GET_INFO, 0x0205_0200]
        );
        for c in [&calls[1], &calls[3]] {
            assert_eq!(c.mbox(), &[0x12, 0x01, 0x04, 0x03, 0, 0, 0, 0, 0, 0, 0, 0]);
            assert!(c.bufs.is_empty());
            assert_eq!(c.frame[7], 0);
            assert_eq!(c.frame.u16_at(0x10), 0);
            assert_eq!(c.frame.u32_at(0x14), 0);
        }
    }

    #[test]
    fn clear_progress_comes_from_pd_info_active_bit_two() {
        let mut b = pd_info_bytes(9, 4, STATE_UNCONFIGURED_GOOD, 252, 1, 30);
        b.put_u32(260, 0b100);
        b.put_u16(272, 16384);
        b.put_u16(274, 120);
        let p = PdInfo::parse(&b).progress;
        assert!(p.rebuild.is_none());
        let c = p.clear.unwrap();
        assert_eq!(c.percent, 25.0);
        assert_eq!(c.elapsed_seconds, 120);
    }

    #[test]
    fn rebuild_stop_refuses_a_drive_that_is_not_rebuilding() {
        let mut b = pd_info_bytes(9, 4, STATE_ONLINE, 252, 1, 30);
        b.put_u32(260, 0);
        let mock = Mock::new().reply(op::PD_GET_INFO, b);
        assert!(rebuild_stop(&mock, 9).is_err());
        assert_eq!(mock.opcodes(), vec![op::PD_GET_INFO]);
    }
}
