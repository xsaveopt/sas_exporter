use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::bytes::{Le, LeMut};
use crate::mega::dcmd::{dcmd_none, dcmd_read, dcmd_write};
use crate::mega::mfi::{Mbox, op};
use crate::mega::pd::Progress;
use crate::mega::transport::Transport;

pub const LD_LIST_LEN: usize = 8 + 256 * 16;
pub const LD_PROPS_LEN: usize = 32;
pub const LD_CONFIG_LEN: usize = 256;
pub const LD_INFO_LEN: usize = 384;
pub const MAX_SPAN_DEPTH: usize = 8;

pub const CACHE_WRITE_BACK: u8 = 0x01;
pub const CACHE_READ_AHEAD: u8 = 0x04;
pub const CACHE_READ_ADAPTIVE: u8 = 0x08;
pub const CACHE_WRITE_CACHE_BAD_BBU: u8 = 0x10;
pub const CACHE_ALLOW_WRITE_CACHE: u8 = 0x20;
pub const CACHE_ALLOW_READ_CACHE: u8 = 0x40;

pub const DDF_RAID0: u8 = 0x00;
pub const DDF_RAID1: u8 = 0x01;
pub const DDF_RAID5: u8 = 0x05;
pub const DDF_RAID6: u8 = 0x06;
pub const DDF_RAID1E: u8 = 0x11;
pub const DDF_JBOD: u8 = 0x0f;
pub const DDF_CONCAT: u8 = 0x1f;

pub fn ld_state_name(state: u8) -> String {
    match state {
        0 => "OfLn".into(),
        1 => "Pdgd".into(),
        2 => "Dgrd".into(),
        3 => "Optl".into(),
        other => format!("{other:#04x}"),
    }
}

pub fn raid_name(primary: u8, secondary: u8, span_depth: u8) -> String {
    let spanned = secondary != 0 && span_depth > 1;
    match (primary, spanned) {
        (DDF_RAID0, false) => "RAID0".into(),
        (DDF_RAID1, false) => "RAID1".into(),
        (DDF_RAID1, true) => "RAID10".into(),
        (DDF_RAID5, false) => "RAID5".into(),
        (DDF_RAID5, true) => "RAID50".into(),
        (DDF_RAID6, false) => "RAID6".into(),
        (DDF_RAID6, true) => "RAID60".into(),
        (DDF_RAID1E, _) => "RAID1E".into(),
        (DDF_JBOD, _) => "JBOD".into(),
        (DDF_CONCAT, _) => "CONCAT".into(),
        (p, _) => format!("primary {p:#04x} secondary {secondary:#04x}"),
    }
}

pub fn cache_string(default: u8, current: u8) -> String {
    let read = if default & CACHE_READ_AHEAD != 0 {
        "R"
    } else {
        "NR"
    };
    let write = if default & CACHE_WRITE_BACK == 0 {
        "WT"
    } else if default & CACHE_WRITE_CACHE_BAD_BBU != 0 {
        "AWB"
    } else if current & CACHE_WRITE_BACK == 0 {
        "FWB"
    } else {
        "WB"
    };
    let io = if default & (CACHE_ALLOW_WRITE_CACHE | CACHE_ALLOW_READ_CACHE)
        == CACHE_ALLOW_WRITE_CACHE | CACHE_ALLOW_READ_CACHE
    {
        "C"
    } else {
        "D"
    };
    format!("{read}{write}{io}")
}

pub fn access_name(access: u8) -> String {
    match access & 3 {
        0 => "RW".into(),
        2 => "RO".into(),
        3 => "Blocked".into(),
        other => format!("{other}"),
    }
}

pub fn disk_cache_name(policy: u8) -> String {
    match policy {
        0 => "Default".into(),
        1 => "On".into(),
        2 => "Off".into(),
        other => format!("{other}"),
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LdListEntry {
    pub target_id: u8,
    pub seq: u16,
    pub state: u8,
    pub state_name: String,
    pub size_blocks: u64,
}

pub fn parse_ld_list(b: &[u8]) -> Vec<LdListEntry> {
    let count = b.u32_at(0) as usize;
    let fits = b.len().saturating_sub(8) / 16;
    (0..count.min(fits))
        .map(|i| {
            let at = 8 + i * 16;
            LdListEntry {
                target_id: b.u8_at(at),
                seq: b.u16_at(at + 2),
                state: b.u8_at(at + 4),
                state_name: ld_state_name(b.u8_at(at + 4)),
                size_blocks: b.u64_at(at + 8),
            }
        })
        .collect()
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LdProps {
    #[serde(skip)]
    pub raw: Vec<u8>,
    pub target_id: u8,
    pub seq: u16,
    pub name: String,
    pub default_cache_policy: u8,
    pub current_cache_policy: u8,
    pub cache: String,
    pub access_policy: u8,
    pub access: String,
    pub disk_cache_policy: u8,
    pub disk_cache: String,
    pub no_bgi: bool,
}

impl LdProps {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            raw: b.get(..LD_PROPS_LEN).unwrap_or(b).to_vec(),
            target_id: b.u8_at(0),
            seq: b.u16_at(2),
            name: b.ascii_at(4, 16),
            default_cache_policy: b.u8_at(20),
            current_cache_policy: b.u8_at(23),
            cache: cache_string(b.u8_at(20), b.u8_at(23)),
            access_policy: b.u8_at(21),
            access: access_name(b.u8_at(21)),
            disk_cache_policy: b.u8_at(22),
            disk_cache: disk_cache_name(b.u8_at(22)),
            no_bgi: b.u8_at(24) != 0,
        }
    }

    pub fn ld_ref(&self) -> u32 {
        self.raw.u32_at(0)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LdParams {
    pub primary_raid_level: u8,
    pub raid_level_qualifier: u8,
    pub secondary_raid_level: u8,
    pub raid: String,
    pub stripe_size_code: u8,
    pub stripe_size_bytes: u64,
    pub drives_per_span: u8,
    pub span_depth: u8,
    pub state: u8,
    pub state_name: String,
    pub init_state: u8,
    pub is_consistent: bool,
}

impl LdParams {
    pub fn parse(b: &[u8]) -> Self {
        let stripe = b.u8_at(3);
        Self {
            primary_raid_level: b.u8_at(0),
            raid_level_qualifier: b.u8_at(1),
            secondary_raid_level: b.u8_at(2),
            raid: raid_name(b.u8_at(0), b.u8_at(2), b.u8_at(5)),
            stripe_size_code: stripe,
            stripe_size_bytes: 512u64.checked_shl(u32::from(stripe)).unwrap_or(0),
            drives_per_span: b.u8_at(4),
            span_depth: b.u8_at(5),
            state: b.u8_at(6),
            state_name: ld_state_name(b.u8_at(6)),
            init_state: b.u8_at(7),
            is_consistent: b.u8_at(8) != 0,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start_block: u64,
    pub num_blocks: u64,
    pub array_ref: u16,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct LdConfig {
    pub properties: LdProps,
    pub params: LdParams,
    pub spans: Vec<Span>,
}

impl LdConfig {
    pub fn parse(b: &[u8]) -> Self {
        let params = LdParams::parse(b.get(32..64).unwrap_or(&[]));
        let depth = usize::from(params.span_depth).min(MAX_SPAN_DEPTH);
        let spans = (0..depth)
            .map(|i| {
                let at = 64 + i * 24;
                Span {
                    start_block: b.u64_at(at),
                    num_blocks: b.u64_at(at + 8),
                    array_ref: b.u16_at(at + 16),
                }
            })
            .collect();
        Self {
            properties: LdProps::parse(b.get(..32).unwrap_or(b)),
            params,
            spans,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LdProgress {
    pub consistency_check: Option<Progress>,
    pub background_init: Option<Progress>,
    pub foreground_init: Option<Progress>,
    pub reconstruction: Option<Progress>,
}

impl LdProgress {
    pub fn parse(b: &[u8], off: usize) -> Self {
        let active = b.u32_at(off);
        let when =
            |bit: u32, at: usize| (active >> bit & 1 == 1).then(|| Progress::parse(b, off + at));
        Self {
            consistency_check: when(0, 4),
            background_init: when(1, 8),
            foreground_init: when(2, 12),
            reconstruction: when(3, 16),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LdInfo {
    pub config: LdConfig,
    pub size_blocks: u64,
    pub progress: LdProgress,
    pub cluster_owner: u16,
    pub reconstruct_active: bool,
}

impl LdInfo {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            config: LdConfig::parse(b.get(..LD_CONFIG_LEN).unwrap_or(b)),
            size_blocks: b.u64_at(256),
            progress: LdProgress::parse(b, 264),
            cluster_owner: b.u16_at(300),
            reconstruct_active: b.u8_at(302) != 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LdSetting {
    WriteCache(WritePolicy),
    ReadAhead(bool),
    IoCached(bool),
    DiskCache(u8),
    Access(u8),
    Name(String),
    AutoBgi(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePolicy {
    WriteThrough,
    WriteBack,
    AlwaysWriteBack,
}

pub const LD_SETTABLE: &[&str] = &[
    "write-cache",
    "read-cache",
    "io-policy",
    "disk-cache",
    "access",
    "name",
    "autobgi",
];

impl LdSetting {
    pub fn parse(name: &str, value: &str) -> Result<Self> {
        let v = value.to_ascii_lowercase();
        let wrong = |choices: &str| anyhow!("{name} takes {choices}, not {value}");
        Ok(match name {
            "write-cache" => LdSetting::WriteCache(match v.as_str() {
                "wt" => WritePolicy::WriteThrough,
                "wb" => WritePolicy::WriteBack,
                "awb" => WritePolicy::AlwaysWriteBack,
                _ => return Err(wrong("wt, wb or awb")),
            }),
            "read-cache" => LdSetting::ReadAhead(match v.as_str() {
                "ra" => true,
                "nora" => false,
                _ => return Err(wrong("ra or nora")),
            }),
            "io-policy" => LdSetting::IoCached(match v.as_str() {
                "cached" => true,
                "direct" => false,
                _ => return Err(wrong("cached or direct")),
            }),
            "disk-cache" => LdSetting::DiskCache(match v.as_str() {
                "default" => 0,
                "on" => 1,
                "off" => 2,
                _ => return Err(wrong("on, off or default")),
            }),
            "access" => LdSetting::Access(match v.as_str() {
                "rw" => 0,
                "ro" => 2,
                "blocked" => 3,
                _ => return Err(wrong("rw, ro or blocked")),
            }),
            "name" => {
                if value.len() > 15 || !value.is_ascii() {
                    bail!("name must be at most 15 ASCII characters");
                }
                LdSetting::Name(value.to_string())
            }
            "autobgi" => LdSetting::AutoBgi(match v.as_str() {
                "on" => true,
                "off" => false,
                _ => return Err(wrong("on or off")),
            }),
            other => bail!(
                "unknown volume property {other}, settable are {}",
                LD_SETTABLE.join(", ")
            ),
        })
    }

    pub fn apply(&self, raw: &mut [u8]) {
        let policy = &mut raw[20];
        match self {
            LdSetting::WriteCache(WritePolicy::WriteThrough) => {
                *policy &= !(CACHE_WRITE_BACK | CACHE_WRITE_CACHE_BAD_BBU);
            }
            LdSetting::WriteCache(WritePolicy::WriteBack) => {
                *policy = (*policy | CACHE_WRITE_BACK) & !CACHE_WRITE_CACHE_BAD_BBU;
            }
            LdSetting::WriteCache(WritePolicy::AlwaysWriteBack) => {
                *policy |= CACHE_WRITE_BACK | CACHE_WRITE_CACHE_BAD_BBU;
            }
            LdSetting::ReadAhead(true) => {
                *policy = (*policy | CACHE_READ_AHEAD) & !CACHE_READ_ADAPTIVE;
            }
            LdSetting::ReadAhead(false) => {
                *policy &= !(CACHE_READ_AHEAD | CACHE_READ_ADAPTIVE);
            }
            LdSetting::IoCached(true) => {
                *policy |= CACHE_ALLOW_WRITE_CACHE | CACHE_ALLOW_READ_CACHE;
            }
            LdSetting::IoCached(false) => {
                *policy &= !(CACHE_ALLOW_WRITE_CACHE | CACHE_ALLOW_READ_CACHE);
            }
            LdSetting::DiskCache(v) => raw[22] = *v,
            LdSetting::Access(v) => raw[21] = (raw[21] & !3) | v,
            LdSetting::AutoBgi(on) => raw[24] = u8::from(!*on),
            LdSetting::Name(name) => {
                raw[4..20].fill(0);
                raw.put_bytes(4, name.as_bytes());
            }
        }
    }
}

pub fn get_list(t: &dyn Transport, extended: bool) -> Result<Vec<LdListEntry>> {
    let buf = dcmd_read(
        t,
        op::LD_GET_LIST,
        &Mbox::new().byte(0, u8::from(extended)),
        LD_LIST_LEN,
    )?;
    Ok(parse_ld_list(&buf))
}

pub fn get_info(t: &dyn Transport, target_id: u8) -> Result<LdInfo> {
    let buf = dcmd_read(
        t,
        op::LD_GET_INFO,
        &Mbox::new().byte(0, target_id),
        LD_INFO_LEN,
    )?;
    Ok(LdInfo::parse(&buf))
}

pub fn get_props(t: &dyn Transport, target_id: u8) -> Result<LdProps> {
    let buf = dcmd_read(
        t,
        op::LD_GET_PROPERTIES,
        &Mbox::new().byte(0, target_id),
        LD_PROPS_LEN,
    )?;
    Ok(LdProps::parse(&buf))
}

pub fn set_property(t: &dyn Transport, target_id: u8, setting: &LdSetting) -> Result<LdProps> {
    let current = get_props(t, target_id)?;
    if current.target_id != target_id {
        bail!(
            "controller returned properties for volume {} instead of {target_id}",
            current.target_id
        );
    }
    let mut raw = current.raw.clone();
    raw.resize(LD_PROPS_LEN, 0);
    setting.apply(&mut raw);
    dcmd_write(
        t,
        op::LD_SET_PROP,
        &Mbox::new().word(0, current.ld_ref()),
        &raw,
    )?;
    get_props(t, target_id)
}

pub fn delete(t: &dyn Transport, target_id: u8) -> Result<()> {
    let props = get_props(t, target_id)?;
    if props.target_id != target_id {
        bail!(
            "controller returned properties for volume {} instead of {target_id}",
            props.target_id
        );
    }
    dcmd_none(t, op::LD_DELETE, &Mbox::new().word(0, props.ld_ref()))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::mega::mock::Mock;

    pub fn ld_props_bytes(target: u8, seq: u16, name: &str, cache: u8) -> Vec<u8> {
        let mut b = vec![0u8; LD_PROPS_LEN];
        b[0] = target;
        b.put_u16(2, seq);
        b[4..4 + name.len()].copy_from_slice(name.as_bytes());
        b[20] = cache;
        b[21] = 0;
        b[22] = 2;
        b[23] = cache;
        b
    }

    pub fn ld_config_bytes(
        target: u8,
        primary: u8,
        secondary: u8,
        drives: u8,
        spans: &[(u64, u16)],
    ) -> Vec<u8> {
        let mut b = vec![0u8; LD_CONFIG_LEN];
        b[..32].copy_from_slice(&ld_props_bytes(target, 1, "data", 0x65));
        b[32] = primary;
        b[33] = if primary >= 5 { 3 } else { 0 };
        b[34] = secondary;
        b[35] = 7;
        b[36] = drives;
        b[37] = spans.len() as u8;
        b[38] = 3;
        b[40] = 1;
        for (i, (blocks, aref)) in spans.iter().enumerate() {
            let at = 64 + i * 24;
            b.put_u64(at + 8, *blocks);
            b.put_u16(at + 16, *aref);
        }
        b
    }

    pub fn ld_info_bytes(target: u8, primary: u8, drives: u8, spans: &[(u64, u16)]) -> Vec<u8> {
        let mut b = vec![0u8; LD_INFO_LEN];
        b[..LD_CONFIG_LEN].copy_from_slice(&ld_config_bytes(target, primary, 0, drives, spans));
        b.put_u64(256, spans.iter().map(|s| s.0).sum());
        b.put_u32(264, 0b0101);
        b.put_u16(268, 16384);
        b.put_u16(270, 100);
        b.put_u16(276, 65535);
        b.put_u16(278, 50);
        b
    }

    pub fn ld_list_bytes(entries: &[(u8, u8, u64)]) -> Vec<u8> {
        let mut b = vec![0u8; LD_LIST_LEN];
        b.put_u32(0, entries.len() as u32);
        for (i, (t, state, size)) in entries.iter().enumerate() {
            let at = 8 + i * 16;
            b[at] = *t;
            b.put_u16(at + 2, 1);
            b[at + 4] = *state;
            b.put_u64(at + 8, *size);
        }
        b
    }

    #[test]
    fn parses_ld_list() {
        let list = parse_ld_list(&ld_list_bytes(&[(0, 3, 1000), (1, 2, 2000)]));
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].target_id, 1);
        assert_eq!(list[1].state_name, "Dgrd");
        assert_eq!(list[1].size_blocks, 2000);
    }

    #[test]
    fn parses_ld_props_and_cache_strings() {
        let p = LdProps::parse(&ld_props_bytes(2, 9, "fast", 0x65));
        assert_eq!(p.target_id, 2);
        assert_eq!(p.seq, 9);
        assert_eq!(p.name, "fast");
        assert_eq!(p.cache, "RWBC");
        assert_eq!(p.access, "RW");
        assert_eq!(p.disk_cache, "Off");
        assert_eq!(p.ld_ref(), 0x0009_0002);
        assert_eq!(cache_string(0x00, 0x00), "NRWTD");
        assert_eq!(cache_string(0x31, 0x31), "NRAWBD");
        assert_eq!(cache_string(0x21, 0x20), "NRFWBD");
    }

    #[test]
    fn raid_levels_follow_the_ddf_table() {
        assert_eq!(raid_name(0x00, 0, 1), "RAID0");
        assert_eq!(raid_name(0x01, 3, 2), "RAID10");
        assert_eq!(raid_name(0x05, 0, 1), "RAID5");
        assert_eq!(raid_name(0x06, 3, 3), "RAID60");
        assert_eq!(raid_name(0x1f, 0, 1), "CONCAT");
    }

    #[test]
    fn parses_ld_info_with_spans_and_progress() {
        let info = LdInfo::parse(&ld_info_bytes(1, DDF_RAID5, 4, &[(3000, 0), (3000, 1)]));
        assert_eq!(info.config.properties.target_id, 1);
        assert_eq!(info.config.params.raid, "RAID5");
        assert_eq!(info.config.params.stripe_size_bytes, 64 * 1024);
        assert_eq!(info.config.params.drives_per_span, 4);
        assert_eq!(info.config.params.state_name, "Optl");
        assert_eq!(info.config.spans.len(), 2);
        assert_eq!(info.config.spans[1].array_ref, 1);
        assert_eq!(info.size_blocks, 6000);
        let cc = info.progress.consistency_check.unwrap();
        assert_eq!(cc.percent, 25.0);
        assert_eq!(cc.remaining_seconds, Some(300));
        assert!(info.progress.background_init.is_none());
        assert_eq!(info.progress.foreground_init.unwrap().percent, 100.0);
    }

    #[test]
    fn settings_change_only_their_bits() {
        let mut raw = ld_props_bytes(0, 1, "x", 0x65);
        LdSetting::parse("write-cache", "wt")
            .unwrap()
            .apply(&mut raw);
        assert_eq!(raw[20], 0x64);
        LdSetting::parse("write-cache", "awb")
            .unwrap()
            .apply(&mut raw);
        assert_eq!(raw[20], 0x75);
        LdSetting::parse("read-cache", "nora")
            .unwrap()
            .apply(&mut raw);
        assert_eq!(raw[20], 0x71);
        LdSetting::parse("io-policy", "direct")
            .unwrap()
            .apply(&mut raw);
        assert_eq!(raw[20], 0x11);
        LdSetting::parse("disk-cache", "on")
            .unwrap()
            .apply(&mut raw);
        assert_eq!(raw[22], 1);
        LdSetting::parse("access", "blocked")
            .unwrap()
            .apply(&mut raw);
        assert_eq!(raw[21], 3);
        LdSetting::parse("name", "backup").unwrap().apply(&mut raw);
        assert_eq!(raw.ascii_at(4, 16), "backup");
        assert!(LdSetting::parse("name", "a-name-that-is-too-long").is_err());
        LdSetting::parse("autobgi", "off").unwrap().apply(&mut raw);
        assert_eq!(raw[24], 1);
        LdSetting::parse("autobgi", "on").unwrap().apply(&mut raw);
        assert_eq!(raw[24], 0);
        assert!(LdSetting::parse("autobgi", "maybe").is_err());
    }

    #[test]
    fn set_property_writes_props_with_the_ldref_mailbox() {
        let mock = Mock::new()
            .reply(op::LD_GET_PROPERTIES, ld_props_bytes(3, 0x0102, "x", 0x65))
            .reply(op::LD_SET_PROP, vec![]);
        set_property(&mock, 3, &LdSetting::ReadAhead(false)).unwrap();
        let calls = mock.calls();
        assert_eq!(calls[0].mbox()[0], 3);
        let write = &calls[1];
        assert_eq!(write.opcode(), op::LD_SET_PROP);
        assert_eq!(write.mbox()[..4], [3, 0, 0x02, 0x01]);
        let mut expected = ld_props_bytes(3, 0x0102, "x", 0x65);
        expected[20] = 0x61;
        assert_eq!(write.bufs[0], expected);
    }

    #[test]
    fn autobgi_off_sets_no_bgi_in_the_props_write() {
        let mock = Mock::new()
            .reply(op::LD_GET_PROPERTIES, ld_props_bytes(5, 0x0203, "x", 0x65))
            .reply(op::LD_SET_PROP, vec![]);
        set_property(&mock, 5, &LdSetting::parse("autobgi", "off").unwrap()).unwrap();
        let write = &mock.calls()[1];
        assert_eq!(write.opcode(), 0x0304_0000);
        assert_eq!(write.mbox()[..4], [5, 0, 0x03, 0x02]);
        assert_eq!(write.frame.u16_at(0x10), 0x0008);
        assert_eq!(write.frame.u32_at(0x14), 32);
        let mut expected = ld_props_bytes(5, 0x0203, "x", 0x65);
        expected[24] = 1;
        assert_eq!(write.bufs[0], expected);
    }

    #[test]
    fn delete_uses_the_ldref() {
        let mock = Mock::new()
            .reply(op::LD_GET_PROPERTIES, ld_props_bytes(4, 7, "x", 0))
            .reply(op::LD_DELETE, vec![]);
        delete(&mock, 4).unwrap();
        let calls = mock.calls();
        assert_eq!(calls[1].opcode(), 0x0309_0000);
        assert_eq!(calls[1].mbox()[..4], [4, 0, 7, 0]);
        assert!(calls[1].bufs.is_empty());
    }

    #[test]
    fn ld_list_mailbox_selects_extended_lists() {
        let mock = Mock::new().reply(op::LD_GET_LIST, ld_list_bytes(&[]));
        get_list(&mock, true).unwrap();
        assert_eq!(mock.calls()[0].mbox()[0], 1);
        assert_eq!(mock.calls()[0].bufs[0].len(), LD_LIST_LEN);
    }
}
