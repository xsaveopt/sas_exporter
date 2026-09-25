use anyhow::{Result, bail};
use serde::Serialize;

use crate::bytes::{Le, LeMut};
use crate::mega::dcmd::{dcmd_none, dcmd_read, dcmd_write};
use crate::mega::mfi::{Mbox, op};
use crate::mega::transport::Transport;

pub const CTRL_INFO_LEN: usize = 2384;
pub const CTRL_PROPS_LEN: usize = 64;
pub const PROPS_IN_INFO: usize = 1536;
pub const TEMPERATURE_ROC_OFFSET: usize = 0x7c9;
pub const TEMPERATURE_CTRL_OFFSET: usize = 0x7ca;

const COMPONENTS: usize = 184;
const PENDING_COUNT: usize = 760;
const PENDING_COMPONENTS: usize = 764;
const COMPONENT_LEN: usize = 72;

pub fn flags(buf: &[u8], off: usize, names: &[&'static str]) -> Vec<&'static str> {
    let v = buf.u32_at(off);
    names
        .iter()
        .enumerate()
        .filter(|(i, _)| v >> i & 1 == 1)
        .map(|(_, n)| *n)
        .collect()
}

pub fn bit(buf: &[u8], off: usize, bit: u32) -> bool {
    buf.u8_at(off) >> bit & 1 == 1
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub version: String,
    pub build_date: String,
    pub build_time: String,
}

fn components(buf: &[u8], count_off: usize, first: usize) -> Vec<Component> {
    let count = (buf.u32_at(count_off) as usize).min(8);
    (0..count)
        .map(|i| {
            let at = first + i * COMPONENT_LEN;
            Component {
                name: buf.ascii_at(at, 8),
                version: buf.ascii_at(at + 8, 32),
                build_date: buf.ascii_at(at + 40, 16),
                build_time: buf.ascii_at(at + 56, 16),
            }
        })
        .collect()
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Temperatures {
    pub roc_celsius: Option<u8>,
    pub controller_celsius: Option<u8>,
}

impl Temperatures {
    pub fn parse(info: &[u8]) -> Self {
        let read = |off: usize| {
            if info.len() > off {
                Some(info[off]).filter(|v| *v != 0)
            } else {
                None
            }
        };
        Self {
            roc_celsius: read(TEMPERATURE_ROC_OFFSET),
            controller_celsius: read(TEMPERATURE_CTRL_OFFSET),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct CtrlInfo {
    pub product_name: String,
    pub serial_number: String,
    pub vendor_id: u16,
    pub device_id: u16,
    pub sub_vendor_id: u16,
    pub sub_device_id: u16,
    pub package_version: String,
    pub image_components: Vec<Component>,
    pub pending_image_components: Vec<Component>,
    pub host_interface: Vec<&'static str>,
    pub device_interface: Vec<&'static str>,
    pub device_port_count: u8,
    pub sas_address: String,
    pub hw_present: Vec<&'static str>,
    pub bbu_present: bool,
    pub alarm_present: bool,
    pub current_fw_time: u32,
    pub max_arms: u8,
    pub max_spans: u8,
    pub max_arrays: u8,
    pub max_lds: u8,
    pub max_pds: u16,
    pub max_dedicated_spares: u16,
    pub max_global_spares: u16,
    pub max_concurrent_cmds: u16,
    pub max_sge_count: u16,
    pub max_request_size: u32,
    pub max_strips_per_io: u16,
    pub ld_present: u16,
    pub ld_degraded: u16,
    pub ld_offline: u16,
    pub pd_present: u16,
    pub pd_disk_present: u16,
    pub pd_disk_pred_failure: u16,
    pub pd_disk_failed: u16,
    pub nvram_size: u16,
    pub memory_size: u16,
    pub flash_size: u16,
    pub cache_memory_size: u16,
    pub mem_correctable_errors: u16,
    pub mem_uncorrectable_errors: u16,
    pub ecc_bucket_count: u8,
    pub raid_levels: Vec<&'static str>,
    pub adapter_operations: Vec<&'static str>,
    pub ld_operations: Vec<&'static str>,
    pub pd_operations: Vec<&'static str>,
    pub stripe_size_min_code: u8,
    pub stripe_size_max_code: u8,
    pub expander_fw_version: String,
    pub support_jbod: bool,
    pub support_max_ext_lds: bool,
    pub config_ext2_supported: bool,
    pub temperatures: Temperatures,
    pub properties: CtrlProps,
}

impl CtrlInfo {
    pub fn parse(b: &[u8]) -> Self {
        let props_end = (PROPS_IN_INFO + CTRL_PROPS_LEN).min(b.len());
        let props = b
            .get(PROPS_IN_INFO.min(props_end)..props_end)
            .unwrap_or(&[]);
        Self {
            product_name: b.ascii_at(1344, 80),
            serial_number: b.ascii_at(1424, 32),
            vendor_id: b.u16_at(0),
            device_id: b.u16_at(2),
            sub_vendor_id: b.u16_at(4),
            sub_device_id: b.u16_at(6),
            package_version: b.ascii_at(1600, 96),
            image_components: components(b, 180, COMPONENTS),
            pending_image_components: components(b, PENDING_COUNT, PENDING_COMPONENTS),
            host_interface: flags(b, 32, &["PCI-X", "PCIe", "iSCSI", "SAS 3G", "SR-IOV"]),
            device_interface: flags(b, 104, &["SPI", "SAS 3G", "SATA 1.5G", "SATA 3G"]),
            device_port_count: b.u8_at(111),
            sas_address: format!("{:#018x}", b.u64_at(112)),
            hw_present: flags(b, 1456, &["BBU", "Alarm", "NVRAM", "UART"]),
            bbu_present: bit(b, 1456, 0),
            alarm_present: bit(b, 1456, 1),
            current_fw_time: b.u32_at(1460),
            max_arms: b.u8_at(1340),
            max_spans: b.u8_at(1341),
            max_arrays: b.u8_at(1342),
            max_lds: b.u8_at(1343),
            max_pds: b.u16_at(1920),
            max_dedicated_spares: b.u16_at(1922),
            max_global_spares: b.u16_at(1924),
            max_concurrent_cmds: b.u16_at(1464),
            max_sge_count: b.u16_at(1466),
            max_request_size: b.u32_at(1468),
            max_strips_per_io: b.u16_at(1498),
            ld_present: b.u16_at(1472),
            ld_degraded: b.u16_at(1474),
            ld_offline: b.u16_at(1476),
            pd_present: b.u16_at(1478),
            pd_disk_present: b.u16_at(1480),
            pd_disk_pred_failure: b.u16_at(1482),
            pd_disk_failed: b.u16_at(1484),
            nvram_size: b.u16_at(1486),
            memory_size: b.u16_at(1488),
            flash_size: b.u16_at(1490),
            cache_memory_size: b.u16_at(1954),
            mem_correctable_errors: b.u16_at(1492),
            mem_uncorrectable_errors: b.u16_at(1494),
            ecc_bucket_count: b.u8_at(1524),
            raid_levels: flags(b, 1500, &["RAID0", "RAID1", "RAID5", "RAID1E", "RAID6"]),
            adapter_operations: flags(
                b,
                1504,
                &[
                    "rebuild rate",
                    "cc rate",
                    "bgi rate",
                    "recon rate",
                    "patrol rate",
                    "alarm control",
                    "cluster",
                    "bbu",
                    "spanning",
                    "dedicated hot spares",
                    "revertible hot spares",
                    "foreign config import",
                    "self diagnostic",
                    "mixed redundancy arrays",
                    "global hot spares",
                ],
            ),
            ld_operations: flags(
                b,
                1508,
                &[
                    "read policy",
                    "write policy",
                    "io policy",
                    "access policy",
                    "disk cache policy",
                ],
            ),
            pd_operations: flags(b, 1516, &["force online", "force offline", "force rebuild"]),
            stripe_size_min_code: b.u8_at(1512),
            stripe_size_max_code: b.u8_at(1513),
            expander_fw_version: b.ascii_at(1940, 12),
            support_jbod: bit(b, 1957, 3),
            support_max_ext_lds: bit(b, 2024, 5),
            config_ext2_supported: bit(b, 2120, 0),
            temperatures: Temperatures::parse(b),
            properties: CtrlProps::parse(props),
        }
    }

    pub fn firmware_version(&self) -> String {
        self.image_components
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("APP"))
            .map(|c| c.version.clone())
            .unwrap_or_default()
    }
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct OnOffProperties {
    pub copyback_disabled: bool,
    pub smarter_enabled: bool,
    pub pr_correct_unconfigured_areas: bool,
    pub use_fde_only: bool,
    pub disable_ncq: bool,
    pub ssd_smarter_enabled: bool,
    pub ssd_patrol_read_enabled: bool,
    pub enable_spin_down_unconfigured: bool,
    pub auto_enhanced_import: bool,
    pub enable_secret_key_control: bool,
    pub disable_online_ctrl_reset: bool,
    pub allow_boot_with_pinned_cache: bool,
    pub disable_spin_down_hot_spares: bool,
    pub enable_jbod: bool,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CtrlProps {
    #[serde(skip)]
    pub raw: Vec<u8>,
    pub seq_num: u16,
    pub pred_fail_poll_interval: u16,
    pub intr_throttle_count: u16,
    pub intr_throttle_timeouts: u16,
    pub rebuild_rate: u8,
    pub patrol_read_rate: u8,
    pub bgi_rate: u8,
    pub cc_rate: u8,
    pub recon_rate: u8,
    pub cache_flush_interval: u8,
    pub spinup_drive_count: u8,
    pub spinup_delay: u8,
    pub cluster_enable: u8,
    pub coercion_mode: u8,
    pub alarm_enable: u8,
    pub disable_auto_rebuild: u8,
    pub disable_battery_warn: u8,
    pub ecc_bucket_size: u8,
    pub ecc_bucket_leak_rate: u16,
    pub restore_hotspare_on_insertion: u8,
    pub expose_encl_devices: u8,
    pub maintain_pd_fail_history: u8,
    pub disallow_host_request_reordering: u8,
    pub abort_cc_on_error: u8,
    pub load_balance_mode: u8,
    pub disable_auto_detect_backplane: u8,
    pub snap_vd_space: u8,
    pub on_off: OnOffProperties,
    pub enable_snap_dump: bool,
    pub spin_down_time: u16,
}

impl CtrlProps {
    pub fn parse(b: &[u8]) -> Self {
        let on = |byte: usize, bit_no: u32| bit(b, 32 + byte, bit_no);
        Self {
            raw: b.to_vec(),
            seq_num: b.u16_at(0),
            pred_fail_poll_interval: b.u16_at(2),
            intr_throttle_count: b.u16_at(4),
            intr_throttle_timeouts: b.u16_at(6),
            rebuild_rate: b.u8_at(8),
            patrol_read_rate: b.u8_at(9),
            bgi_rate: b.u8_at(10),
            cc_rate: b.u8_at(11),
            recon_rate: b.u8_at(12),
            cache_flush_interval: b.u8_at(13),
            spinup_drive_count: b.u8_at(14),
            spinup_delay: b.u8_at(15),
            cluster_enable: b.u8_at(16),
            coercion_mode: b.u8_at(17),
            alarm_enable: b.u8_at(18),
            disable_auto_rebuild: b.u8_at(19),
            disable_battery_warn: b.u8_at(20),
            ecc_bucket_size: b.u8_at(21),
            ecc_bucket_leak_rate: b.u16_at(22),
            restore_hotspare_on_insertion: b.u8_at(24),
            expose_encl_devices: b.u8_at(25),
            maintain_pd_fail_history: b.u8_at(26),
            disallow_host_request_reordering: b.u8_at(27),
            abort_cc_on_error: b.u8_at(28),
            load_balance_mode: b.u8_at(29),
            disable_auto_detect_backplane: b.u8_at(30),
            snap_vd_space: b.u8_at(31),
            on_off: OnOffProperties {
                copyback_disabled: on(0, 0),
                smarter_enabled: on(0, 1),
                pr_correct_unconfigured_areas: on(0, 2),
                use_fde_only: on(0, 3),
                disable_ncq: on(0, 4),
                ssd_smarter_enabled: on(0, 5),
                ssd_patrol_read_enabled: on(0, 6),
                enable_spin_down_unconfigured: on(0, 7),
                auto_enhanced_import: on(1, 0),
                enable_secret_key_control: on(1, 1),
                disable_online_ctrl_reset: on(1, 2),
                allow_boot_with_pinned_cache: on(1, 3),
                disable_spin_down_hot_spares: on(1, 4),
                enable_jbod: on(1, 5),
            },
            enable_snap_dump: bit(b, 36, 4),
            spin_down_time: b.u16_at(38),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Percent(usize),
    Byte(usize),
    Short(usize),
    Max(usize, u8),
    Bool(usize),
    NotBool(usize),
    Bit(usize, u8),
    NotBit(usize, u8),
    Choice(usize, &'static [(&'static str, u8)]),
}

pub const COERCION: &[(&str, u8)] = &[("none", 0), ("128m", 1), ("1g", 2)];

pub const PROPERTIES: &[(&str, Kind)] = &[
    ("smart-poll-interval", Kind::Short(2)),
    ("rebuild-rate", Kind::Percent(8)),
    ("patrol-rate", Kind::Percent(9)),
    ("bgi-rate", Kind::Percent(10)),
    ("cc-rate", Kind::Percent(11)),
    ("recon-rate", Kind::Percent(12)),
    ("cache-flush-interval", Kind::Byte(13)),
    ("spinup-drive-count", Kind::Byte(14)),
    ("spinup-delay", Kind::Byte(15)),
    ("coercion", Kind::Choice(17, COERCION)),
    ("alarm", Kind::Bool(18)),
    ("auto-rebuild", Kind::NotBool(19)),
    ("battery-warning", Kind::NotBool(20)),
    ("ecc-bucket-leak-rate", Kind::Short(22)),
    ("restore-hotspare", Kind::Bool(24)),
    ("expose-enclosure", Kind::Bool(25)),
    ("maintain-pd-fail-history", Kind::Bool(26)),
    ("abort-cc-on-error", Kind::Bool(28)),
    ("backplane-mode", Kind::Max(30, 3)),
    ("copyback", Kind::NotBit(32, 0x01)),
    ("copyback-smart-hdd", Kind::Bit(32, 0x02)),
    ("pr-correct-unconfigured", Kind::Bit(32, 0x04)),
    ("use-fde-only", Kind::Bit(32, 0x08)),
    ("ncq", Kind::NotBit(32, 0x10)),
    ("copyback-smart-ssd", Kind::Bit(32, 0x20)),
    ("ssd-patrol-read", Kind::Bit(32, 0x40)),
    ("spindown-unconfigured", Kind::Bit(32, 0x80)),
    ("foreign-auto-import", Kind::Bit(33, 0x01)),
    ("ocr", Kind::NotBit(33, 0x04)),
    ("boot-with-pinned-cache", Kind::Bit(33, 0x08)),
    ("spindown-hotspares", Kind::NotBit(33, 0x10)),
    ("jbod", Kind::Bit(33, 0x20)),
    ("spindown-time", Kind::Short(38)),
];

pub fn settable() -> impl Iterator<Item = &'static str> {
    PROPERTIES.iter().map(|(n, _)| *n)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtrlSetting {
    Byte(usize, u8),
    Short(usize, u16),
    Flag(usize, u8, bool),
}

fn on_off(name: &str, v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "on" | "1" | "yes" | "true" | "enable" | "enabled" => Ok(true),
        "off" | "0" | "no" | "false" | "disable" | "disabled" => Ok(false),
        _ => bail!("{name} takes on or off, not {v}"),
    }
}

impl CtrlSetting {
    pub fn parse(name: &str, value: &str) -> Result<Self> {
        let Some((_, kind)) = PROPERTIES.iter().find(|(n, _)| *n == name) else {
            bail!(
                "unknown controller property {name}, settable are {}",
                settable().collect::<Vec<_>>().join(", ")
            );
        };
        let byte = |max: u8| -> Result<u8> {
            match value.parse::<u8>() {
                Ok(v) if v <= max => Ok(v),
                _ => bail!("{name} takes a whole number from 0 to {max}, not {value}"),
            }
        };
        Ok(match *kind {
            Kind::Percent(off) => CtrlSetting::Byte(off, byte(100)?),
            Kind::Byte(off) => CtrlSetting::Byte(off, byte(u8::MAX)?),
            Kind::Max(off, max) => CtrlSetting::Byte(off, byte(max)?),
            Kind::Short(off) => match value.parse::<u16>() {
                Ok(v) => CtrlSetting::Short(off, v),
                Err(_) => bail!("{name} takes a whole number from 0 to 65535, not {value}"),
            },
            Kind::Bool(off) => CtrlSetting::Byte(off, u8::from(on_off(name, value)?)),
            Kind::NotBool(off) => CtrlSetting::Byte(off, u8::from(!on_off(name, value)?)),
            Kind::Bit(off, mask) => CtrlSetting::Flag(off, mask, on_off(name, value)?),
            Kind::NotBit(off, mask) => CtrlSetting::Flag(off, mask, !on_off(name, value)?),
            Kind::Choice(off, choices) => {
                let v = value.to_ascii_lowercase();
                match choices.iter().find(|(c, _)| *c == v) {
                    Some((_, code)) => CtrlSetting::Byte(off, *code),
                    None => bail!(
                        "{name} takes {}, not {value}",
                        choices
                            .iter()
                            .map(|(c, _)| *c)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            }
        })
    }

    pub fn apply(self, raw: &mut [u8]) {
        match self {
            CtrlSetting::Byte(off, v) => raw.put_u8(off, v),
            CtrlSetting::Short(off, v) => raw.put_u16(off, v),
            CtrlSetting::Flag(off, mask, true) => raw[off] |= mask,
            CtrlSetting::Flag(off, mask, false) => raw[off] &= !mask,
        }
    }

    pub fn enables_jbod(self) -> bool {
        self == CtrlSetting::Flag(33, 0x20, true)
    }
}

pub fn get_info(t: &dyn Transport) -> Result<CtrlInfo> {
    let buf = dcmd_read(t, op::CTRL_GET_INFO, &Mbox::new().byte(0, 1), CTRL_INFO_LEN)?;
    Ok(CtrlInfo::parse(&buf))
}

pub fn get_props(t: &dyn Transport) -> Result<CtrlProps> {
    let buf = dcmd_read(t, op::CTRL_GET_PROPS, &Mbox::new(), CTRL_PROPS_LEN)?;
    Ok(CtrlProps::parse(&buf))
}

pub fn set_property(t: &dyn Transport, setting: CtrlSetting) -> Result<CtrlProps> {
    let current = get_props(t)?;
    let mut raw = current.raw.clone();
    raw.resize(CTRL_PROPS_LEN, 0);
    setting.apply(&mut raw);
    dcmd_write(t, op::CTRL_SET_PROPS, &Mbox::new(), &raw)?;
    get_props(t)
}

pub fn get_time(t: &dyn Transport) -> Result<u32> {
    let buf = dcmd_read(t, op::TIME_SECS_GET, &Mbox::new(), 4)?;
    Ok(buf.u32_at(0))
}

pub fn shutdown(t: &dyn Transport, spin_down: bool) -> Result<()> {
    dcmd_none(
        t,
        op::CTRL_SHUTDOWN,
        &Mbox::new().byte(0, u8::from(spin_down)),
    )
}

pub fn flush_cache(t: &dyn Transport, include_disks: bool) -> Result<()> {
    let which = if include_disks { 0x03 } else { 0x01 };
    dcmd_none(t, op::CTRL_CACHE_FLUSH, &Mbox::new().byte(0, which))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmAction {
    Enable,
    Disable,
    Silence,
}

pub fn alarm(t: &dyn Transport, action: AlarmAction) -> Result<()> {
    let opcode = match action {
        AlarmAction::Enable => op::SPEAKER_ENABLE,
        AlarmAction::Disable => op::SPEAKER_DISABLE,
        AlarmAction::Silence => op::SPEAKER_SILENCE,
    };
    dcmd_none(t, opcode, &Mbox::new())
}

pub fn alarm_state(t: &dyn Transport) -> Result<u8> {
    Ok(dcmd_read(t, op::SPEAKER_GET, &Mbox::new(), 1)?[0])
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::mega::mock::Mock;

    fn put_str(b: &mut [u8], off: usize, s: &str) {
        b[off..off + s.len()].copy_from_slice(s.as_bytes());
    }

    pub fn ctrl_info_bytes(product: &str, serial: &str, roc: u8, chip: u8) -> Vec<u8> {
        let mut b = vec![0u8; CTRL_INFO_LEN];
        b.put_u16(0, 0x1000);
        b.put_u16(2, 0x0014);
        b.put_u16(4, 0x1000);
        b.put_u16(6, 0x9460);
        b[32] = 0b0000_0010;
        b[104] = 0b0000_0010;
        b[111] = 8;
        b.put_u64(112, 0x5000_0d1e_0123_4567);
        b.put_u32(180, 2);
        put_str(&mut b, 184, "APP");
        put_str(&mut b, 192, "5.140.00-3319");
        put_str(&mut b, 224, "Jan 01 2024");
        put_str(&mut b, 256, "12:00:00");
        put_str(&mut b, 184 + 72, "BIOS");
        put_str(&mut b, 192 + 72, "7.14.00.0");
        b.put_u32(760, 1);
        put_str(&mut b, 764, "APP");
        put_str(&mut b, 772, "5.200.00-1234");
        b[1340] = 32;
        b[1343] = 64;
        put_str(&mut b, 1344, product);
        put_str(&mut b, 1424, serial);
        b.put_u32(1456, 0b0011);
        b.put_u32(1460, 0x2d00_0000);
        b.put_u16(1472, 2);
        b.put_u16(1474, 1);
        b.put_u16(1478, 9);
        b.put_u16(1488, 4096);
        b.put_u32(1500, 0b1_0111);
        b.put_u32(1504, 1 | 1 << 14);
        b.put_u32(1516, 0b101);
        b[1536 + 8] = 30;
        b[1536 + 18] = 1;
        put_str(&mut b, 1600, "51.20.0-4567");
        b.put_u16(1920, 240);
        b[1957] = 1 << 3;
        b[TEMPERATURE_ROC_OFFSET] = roc;
        b[TEMPERATURE_CTRL_OFFSET] = chip;
        b[2024] = 1 << 5;
        b
    }

    #[test]
    fn parses_controller_info_fields() {
        let info = CtrlInfo::parse(&ctrl_info_bytes("MegaRAID 9460-8i", "SK1234", 52, 61));
        assert_eq!(info.product_name, "MegaRAID 9460-8i");
        assert_eq!(info.serial_number, "SK1234");
        assert_eq!((info.vendor_id, info.device_id), (0x1000, 0x0014));
        assert_eq!(info.sub_device_id, 0x9460);
        assert_eq!(info.host_interface, vec!["PCIe"]);
        assert_eq!(info.device_interface, vec!["SAS 3G"]);
        assert_eq!(info.sas_address, "0x50000d1e01234567");
        assert_eq!(info.image_components.len(), 2);
        assert_eq!(info.firmware_version(), "5.140.00-3319");
        assert_eq!(info.image_components[0].build_date, "Jan 01 2024");
        assert_eq!(info.pending_image_components[0].version, "5.200.00-1234");
        assert!(info.bbu_present && info.alarm_present);
        assert_eq!(info.max_lds, 64);
        assert_eq!(info.ld_present, 2);
        assert_eq!(info.ld_degraded, 1);
        assert_eq!(info.pd_present, 9);
        assert_eq!(info.raid_levels, vec!["RAID0", "RAID1", "RAID5", "RAID6"]);
        assert_eq!(
            info.adapter_operations,
            vec!["rebuild rate", "global hot spares"]
        );
        assert_eq!(info.pd_operations, vec!["force online", "force rebuild"]);
        assert_eq!(info.package_version, "51.20.0-4567");
        assert_eq!(info.max_pds, 240);
        assert!(info.support_jbod);
        assert!(info.support_max_ext_lds);
        assert!(!info.config_ext2_supported);
        assert_eq!(info.properties.rebuild_rate, 30);
        assert_eq!(info.properties.alarm_enable, 1);
    }

    #[test]
    fn temperatures_come_from_0x7c9_and_0x7ca() {
        let t = Temperatures::parse(&ctrl_info_bytes("x", "y", 52, 61));
        assert_eq!(t.roc_celsius, Some(52));
        assert_eq!(t.controller_celsius, Some(61));
        let t = Temperatures::parse(&ctrl_info_bytes("x", "y", 47, 0));
        assert_eq!(t.controller_celsius, None);
        let short = vec![0u8; 2048];
        assert_eq!(Temperatures::parse(&short[..1990]).roc_celsius, None);
    }

    #[test]
    fn get_info_sends_linux_mailbox_and_size() {
        let mock = Mock::new().reply(op::CTRL_GET_INFO, ctrl_info_bytes("x", "y", 1, 2));
        get_info(&mock).unwrap();
        let call = &mock.calls()[0];
        assert_eq!(call.mbox()[0], 1);
        assert_eq!(call.bufs[0].len(), 2384);
    }

    fn props_bytes() -> Vec<u8> {
        let mut p = vec![0u8; 64];
        p.put_u16(0, 17);
        p[8] = 30;
        p[9] = 20;
        p[18] = 1;
        p.put_u16(22, 1440);
        p[30] = 2;
        p[32] = 0b0000_0001;
        p[33] = 0b0010_0000;
        p[36] = 1 << 4;
        p.put_u16(38, 30);
        p[63] = 0x5a;
        p
    }

    #[test]
    fn parses_controller_properties() {
        let p = CtrlProps::parse(&props_bytes());
        assert_eq!(p.seq_num, 17);
        assert_eq!(p.rebuild_rate, 30);
        assert_eq!(p.patrol_read_rate, 20);
        assert_eq!(p.alarm_enable, 1);
        assert_eq!(p.ecc_bucket_leak_rate, 1440);
        assert_eq!(p.disable_auto_detect_backplane, 2);
        assert!(p.on_off.copyback_disabled);
        assert!(p.on_off.enable_jbod);
        assert!(!p.on_off.disable_ncq);
        assert!(p.enable_snap_dump);
        assert_eq!(p.spin_down_time, 30);
    }

    #[test]
    fn settings_parse_and_validate() {
        assert_eq!(
            CtrlSetting::parse("rebuild-rate", "60").unwrap(),
            CtrlSetting::Byte(8, 60)
        );
        for rate in [
            "rebuild-rate",
            "patrol-rate",
            "bgi-rate",
            "cc-rate",
            "recon-rate",
        ] {
            assert!(CtrlSetting::parse(rate, "100").is_ok(), "{rate}");
            assert!(CtrlSetting::parse(rate, "101").is_err(), "{rate}");
            assert!(CtrlSetting::parse(rate, "-1").is_err(), "{rate}");
        }
        assert!(CtrlSetting::parse("backplane-mode", "3").is_ok());
        assert!(CtrlSetting::parse("backplane-mode", "4").is_err());
        assert!(CtrlSetting::parse("spinup-delay", "255").is_ok());
        assert!(CtrlSetting::parse("spinup-delay", "256").is_err());
        assert_eq!(
            CtrlSetting::parse("smart-poll-interval", "65535").unwrap(),
            CtrlSetting::Short(2, 65535)
        );
        assert!(CtrlSetting::parse("ecc-bucket-leak-rate", "65536").is_err());
        assert!(CtrlSetting::parse("spindown-time", "x").is_err());
        assert_eq!(
            CtrlSetting::parse("coercion", "1G").unwrap(),
            CtrlSetting::Byte(17, 2)
        );
        assert!(CtrlSetting::parse("coercion", "2g").is_err());
        assert_eq!(
            CtrlSetting::parse("alarm", "off").unwrap(),
            CtrlSetting::Byte(18, 0)
        );
        assert_eq!(
            CtrlSetting::parse("auto-rebuild", "off").unwrap(),
            CtrlSetting::Byte(19, 1)
        );
        assert_eq!(
            CtrlSetting::parse("battery-warning", "on").unwrap(),
            CtrlSetting::Byte(20, 0)
        );
        assert_eq!(
            CtrlSetting::parse("ocr", "off").unwrap(),
            CtrlSetting::Flag(33, 0x04, true)
        );
        assert_eq!(
            CtrlSetting::parse("ncq", "on").unwrap(),
            CtrlSetting::Flag(32, 0x10, false)
        );
        assert_eq!(
            CtrlSetting::parse("spindown-hotspares", "on").unwrap(),
            CtrlSetting::Flag(33, 0x10, false)
        );
        assert!(CtrlSetting::parse("jbod", "on").unwrap().enables_jbod());
        assert!(!CtrlSetting::parse("jbod", "off").unwrap().enables_jbod());
        assert!(CtrlSetting::parse("jbod", "maybe").is_err());
        assert!(CtrlSetting::parse("load-balance-mode", "1").is_err());
        assert!(CtrlSetting::parse("snapdump", "on").is_err());
    }

    #[test]
    fn every_onoff_bit_lands_on_its_documented_position() {
        let expect: &[(&str, &str, usize, u8)] = &[
            ("copyback", "off", 32, 0x01),
            ("copyback-smart-hdd", "on", 32, 0x02),
            ("pr-correct-unconfigured", "on", 32, 0x04),
            ("use-fde-only", "on", 32, 0x08),
            ("ncq", "off", 32, 0x10),
            ("copyback-smart-ssd", "on", 32, 0x20),
            ("ssd-patrol-read", "on", 32, 0x40),
            ("spindown-unconfigured", "on", 32, 0x80),
            ("foreign-auto-import", "on", 33, 0x01),
            ("ocr", "off", 33, 0x04),
            ("boot-with-pinned-cache", "on", 33, 0x08),
            ("spindown-hotspares", "off", 33, 0x10),
            ("jbod", "on", 33, 0x20),
        ];
        for (name, value, off, mask) in expect {
            let mut raw = vec![0u8; 64];
            CtrlSetting::parse(name, value).unwrap().apply(&mut raw);
            let mut want = vec![0u8; 64];
            want[*off] = *mask;
            assert_eq!(raw, want, "{name}");
            let mut full = vec![0xffu8; 64];
            let flip = if *value == "on" { "off" } else { "on" };
            CtrlSetting::parse(name, flip).unwrap().apply(&mut full);
            assert_eq!(full[*off], !*mask, "{name}");
        }
        let names: Vec<&str> = settable().collect();
        assert!(
            !names
                .iter()
                .any(|n| n.contains("balance") || n.contains("snap"))
        );
        assert!(!names.contains(&"secret-key-control"));
    }

    #[test]
    fn byte_and_short_settings_write_their_offsets() {
        let cases: &[(&str, &str, usize, &[u8])] = &[
            ("smart-poll-interval", "300", 2, &[0x2c, 0x01]),
            ("patrol-rate", "25", 9, &[25]),
            ("bgi-rate", "40", 10, &[40]),
            ("cc-rate", "50", 11, &[50]),
            ("recon-rate", "60", 12, &[60]),
            ("cache-flush-interval", "4", 13, &[4]),
            ("spinup-drive-count", "2", 14, &[2]),
            ("spinup-delay", "6", 15, &[6]),
            ("coercion", "128m", 17, &[1]),
            ("ecc-bucket-leak-rate", "1440", 22, &[0xa0, 0x05]),
            ("restore-hotspare", "on", 24, &[1]),
            ("expose-enclosure", "on", 25, &[1]),
            ("maintain-pd-fail-history", "on", 26, &[1]),
            ("abort-cc-on-error", "on", 28, &[1]),
            ("backplane-mode", "2", 30, &[2]),
            ("spindown-time", "30", 38, &[30, 0]),
        ];
        for (name, value, off, bytes) in cases {
            let mut raw = vec![0u8; 64];
            CtrlSetting::parse(name, value).unwrap().apply(&mut raw);
            let mut want = vec![0u8; 64];
            want[*off..*off + bytes.len()].copy_from_slice(bytes);
            assert_eq!(raw, want, "{name}");
        }
    }

    #[test]
    fn set_property_is_read_modify_write() {
        let mock = Mock::new()
            .reply(op::CTRL_GET_PROPS, props_bytes())
            .reply(op::CTRL_SET_PROPS, vec![]);
        set_property(&mock, CtrlSetting::parse("copyback", "on").unwrap()).unwrap();
        set_property(&mock, CtrlSetting::parse("rebuild-rate", "75").unwrap()).unwrap();
        let writes: Vec<_> = mock
            .calls()
            .into_iter()
            .filter(|c| c.opcode() == op::CTRL_SET_PROPS)
            .collect();
        assert_eq!(writes.len(), 2);
        let mut expected = props_bytes();
        expected[32] &= !1;
        assert_eq!(writes[0].bufs[0], expected);
        assert_eq!(writes[0].frame.u16_at(0x10), 0x0008);
        let mut expected = props_bytes();
        expected[8] = 75;
        assert_eq!(writes[1].bufs[0], expected);
    }

    #[test]
    fn shutdown_flush_and_alarm_frames() {
        let mock = Mock::new()
            .reply(op::CTRL_SHUTDOWN, vec![])
            .reply(op::CTRL_CACHE_FLUSH, vec![])
            .reply(op::SPEAKER_SILENCE, vec![]);
        shutdown(&mock, true).unwrap();
        flush_cache(&mock, false).unwrap();
        flush_cache(&mock, true).unwrap();
        alarm(&mock, AlarmAction::Silence).unwrap();
        let calls = mock.calls();
        assert_eq!(calls[0].mbox()[0], 1);
        assert_eq!(calls[1].mbox()[0], 0x01);
        assert_eq!(calls[2].mbox()[0], 0x03);
        assert_eq!(calls[3].opcode(), 0x0103_0400);
        assert!(calls.iter().all(|c| c.bufs.is_empty()));
    }
}
