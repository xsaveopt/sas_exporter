use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::mega::ata::{self, AtaSmart};
use crate::mega::config::{self, ConfigData};
use crate::mega::ctrl::{self, CtrlInfo};
use crate::mega::event::{Event, format_fw_time, format_unix};
use crate::mega::ld::{self, LdInfo};
use crate::mega::pd::{self, DriveAddress, NO_ENCLOSURE, PdAddress, PdInfo, Progress};
use crate::mega::scsi::{self, InformationalExceptions, TemperatureLog};
use crate::mega::transport::Transport;
use crate::sysfs::read_trimmed;

pub const BLOCK: u64 = 512;

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.2} {}", UNITS[unit])
    }
}

pub fn blocks(n: u64) -> String {
    human_bytes(n.saturating_mul(BLOCK))
}

#[derive(Serialize, Clone, Debug)]
pub struct ControllerSummary {
    pub index: usize,
    pub host_no: u32,
    pub pci_address: Option<String>,
    pub driver_version: Option<String>,
    pub controller_time: Option<String>,
    pub info: CtrlInfo,
}

pub fn driver_version(sysfs: &Path) -> Option<String> {
    read_trimmed(&sysfs.join("bus/pci/drivers/megaraid_sas/version"))
}

#[derive(Serialize, Clone, Debug)]
pub struct TemperatureReport {
    pub controller: usize,
    pub roc_celsius: Option<u8>,
    pub controller_celsius: Option<u8>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Temperatures {
    #[serde(flatten)]
    pub controller: TemperatureReport,
    #[serde(flatten)]
    pub drives: DriveTemperatures,
}

#[derive(Serialize, Clone, Debug)]
pub struct TimeReport {
    pub controller_seconds: u32,
    pub controller_time: String,
    pub host_time: String,
    pub offset_seconds: i64,
}

pub fn time_report(controller_seconds: u32, host_unix: i64) -> TimeReport {
    let ctrl_unix = crate::mega::event::CONTROLLER_EPOCH + i64::from(controller_seconds);
    TimeReport {
        controller_seconds,
        controller_time: format_fw_time(controller_seconds),
        host_time: format_unix(host_unix),
        offset_seconds: ctrl_unix - host_unix,
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveRow {
    pub address: DriveAddress,
    pub device_id: u16,
    pub state: String,
    pub drive_group: Option<usize>,
    pub size_bytes: u64,
    pub size: String,
    pub interface_code: u8,
    pub media_type: u8,
    pub model: String,
    pub revision: String,
    pub temperature_celsius: Option<u8>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveList {
    pub drives: Vec<DriveRow>,
}

pub fn drive_list(t: &dyn Transport) -> Result<DriveList> {
    let list = pd::get_list(t)?;
    let cfg = config::read(t).ok();
    let mut drives: Vec<DriveRow> = pd::drives(&list)
        .map(|a| match pd::get_info(t, a.device_id) {
            Ok(info) => DriveRow {
                address: a.address(),
                device_id: a.device_id,
                state: info.state.clone(),
                drive_group: cfg.as_ref().and_then(|c| c.array_index_of(a.device_id)),
                size_bytes: info.coerced_blocks * BLOCK,
                size: blocks(info.coerced_blocks),
                interface_code: info.interface_code,
                media_type: info.media_type,
                model: info.model(),
                revision: info.revision.clone(),
                temperature_celsius: Some(info.temperature_celsius).filter(|v| *v != 0),
                error: None,
            },
            Err(e) => DriveRow {
                address: a.address(),
                device_id: a.device_id,
                state: String::new(),
                drive_group: None,
                size_bytes: 0,
                size: String::new(),
                interface_code: 0,
                media_type: 0,
                model: String::new(),
                revision: String::new(),
                temperature_celsius: None,
                error: Some(format!("{e:#}")),
            },
        })
        .collect();
    drives.sort_by_key(|d| d.address);
    Ok(DriveList { drives })
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveDetail {
    pub address: DriveAddress,
    pub serial_number: Option<String>,
    pub drive_group: Option<usize>,
    pub info: PdInfo,
}

pub fn locate_drive(t: &dyn Transport, addr: DriveAddress) -> Result<PdAddress> {
    let list = pd::get_list(t)?;
    Ok(pd::resolve(&list, addr)?.clone())
}

pub fn drive_detail(t: &dyn Transport, addr: DriveAddress) -> Result<DriveDetail> {
    let a = locate_drive(t, addr)?;
    let info = pd::get_info(t, a.device_id)?;
    let serial_number = scsi::serial_number(t, a.device_id).ok().flatten();
    let drive_group = config::read(t)
        .ok()
        .and_then(|c| c.array_index_of(a.device_id));
    Ok(DriveDetail {
        address: addr,
        serial_number,
        drive_group,
        info,
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct RebuildProgress {
    pub address: DriveAddress,
    pub state: String,
    pub rebuild: Option<Progress>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveSmart {
    pub address: DriveAddress,
    pub device_id: u16,
    pub media_errors: u32,
    pub other_errors: u32,
    pub predictive_failures: u32,
    pub last_predictive_failure_event: u32,
    pub smart_alert: bool,
    pub informational_exceptions: Option<InformationalExceptions>,
    pub passthrough_error: Option<String>,
    pub sata: Option<bool>,
    pub ata_smart: Option<AtaSmart>,
    pub ata_error: Option<String>,
}

pub fn drive_smart(t: &dyn Transport, addr: DriveAddress) -> Result<DriveSmart> {
    let a = locate_drive(t, addr)?;
    let info = pd::get_info(t, a.device_id)?;
    let ie = scsi::informational_exceptions(t, a.device_id);
    let sata = ata::probe_sat(t, a.device_id);
    let (ata_smart, ata_error) = match &sata {
        Ok(true) => match ata::read_smart(t, a.device_id) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        },
        Ok(false) => (None, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    Ok(DriveSmart {
        address: addr,
        device_id: a.device_id,
        media_errors: info.media_errors,
        other_errors: info.other_errors,
        predictive_failures: info.predictive_failures,
        last_predictive_failure_event: info.last_predictive_failure_event,
        smart_alert: info.predictive_failures > 0,
        passthrough_error: ie.as_ref().err().map(|e| format!("{e:#}")),
        informational_exceptions: ie.ok(),
        sata: sata.ok(),
        ata_smart,
        ata_error,
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct ClearProgress {
    pub address: DriveAddress,
    pub state: String,
    pub clear: Option<Progress>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveTemperature {
    pub address: DriveAddress,
    pub device_id: u16,
    pub controller_reported_celsius: Option<u8>,
    pub log_page: Option<TemperatureLog>,
    pub passthrough_error: Option<String>,
}

pub fn drive_temperature(t: &dyn Transport, addr: DriveAddress) -> Result<DriveTemperature> {
    let a = locate_drive(t, addr)?;
    let info = pd::get_info(t, a.device_id)?;
    let log = scsi::temperature(t, a.device_id);
    Ok(DriveTemperature {
        address: addr,
        device_id: a.device_id,
        controller_reported_celsius: Some(info.temperature_celsius).filter(|v| *v != 0),
        passthrough_error: log.as_ref().err().map(|e| format!("{e:#}")),
        log_page: log.ok(),
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveTemperatureRow {
    pub address: DriveAddress,
    pub device_id: u16,
    pub celsius: Option<u8>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DriveTemperatures {
    pub drives: Vec<DriveTemperatureRow>,
}

pub fn drive_temperatures(t: &dyn Transport) -> Result<DriveTemperatures> {
    let list = pd::get_list(t)?;
    let mut drives: Vec<DriveTemperatureRow> = pd::drives(&list)
        .map(|a| DriveTemperatureRow {
            address: a.address(),
            device_id: a.device_id,
            celsius: pd::get_info(t, a.device_id)
                .ok()
                .map(|i| i.temperature_celsius)
                .filter(|v| *v != 0),
        })
        .collect();
    drives.sort_by_key(|d| d.address);
    Ok(DriveTemperatures { drives })
}

#[derive(Serialize, Clone, Debug)]
pub struct VolumeRow {
    pub target_id: u8,
    pub drive_group: Option<usize>,
    pub state: String,
    pub raid: String,
    pub size_bytes: u64,
    pub size: String,
    pub cache: String,
    pub access: String,
    pub disk_cache: String,
    pub name: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct VolumeList {
    pub volumes: Vec<VolumeRow>,
}

fn extended_lds(t: &dyn Transport) -> bool {
    ctrl::get_info(t)
        .map(|i| i.support_max_ext_lds)
        .unwrap_or(false)
}

fn drive_group_of(cfg: Option<&ConfigData>, info: &LdInfo) -> Option<usize> {
    let cfg = cfg?;
    let first = info.config.spans.first()?;
    cfg.arrays
        .iter()
        .position(|a| a.array_ref == first.array_ref)
}

pub fn volume_list(t: &dyn Transport) -> Result<VolumeList> {
    let list = ld::get_list(t, extended_lds(t))?;
    let cfg = config::read(t).ok();
    let mut volumes = Vec::new();
    for entry in list {
        let info = ld::get_info(t, entry.target_id)?;
        let p = &info.config.properties;
        volumes.push(VolumeRow {
            target_id: entry.target_id,
            drive_group: drive_group_of(cfg.as_ref(), &info),
            state: entry.state_name.clone(),
            raid: info.config.params.raid.clone(),
            size_bytes: info.size_blocks * BLOCK,
            size: blocks(info.size_blocks),
            cache: p.cache.clone(),
            access: p.access.clone(),
            disk_cache: p.disk_cache.clone(),
            name: p.name.clone(),
        });
    }
    volumes.sort_by_key(|v| v.target_id);
    Ok(VolumeList { volumes })
}

#[derive(Serialize, Clone, Debug)]
pub struct VolumeMember {
    pub address: Option<DriveAddress>,
    pub device_id: u16,
    pub state: String,
    pub missing: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct VolumeDetail {
    pub target_id: u8,
    pub drive_group: Option<usize>,
    pub info: LdInfo,
    pub members: Vec<VolumeMember>,
}

pub fn volume_detail(t: &dyn Transport, target_id: u8) -> Result<VolumeDetail> {
    let info = ld::get_info(t, target_id)?;
    let cfg = config::read(t).ok();
    let pds = pd::get_list(t).unwrap_or_default();
    let members = cfg
        .as_ref()
        .map(|c| {
            c.volume_drives(target_id)
                .into_iter()
                .map(|m| VolumeMember {
                    address: pds
                        .iter()
                        .find(|p| p.device_id == m.device_id && !m.missing)
                        .map(PdAddress::address),
                    device_id: m.device_id,
                    state: if m.missing {
                        "Missing".into()
                    } else {
                        m.state.clone()
                    },
                    missing: m.missing,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(VolumeDetail {
        target_id,
        drive_group: drive_group_of(cfg.as_ref(), &info),
        info,
        members,
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct VolumeProgress {
    pub target_id: u8,
    pub operations: BTreeMap<&'static str, Option<Progress>>,
}

pub fn volume_init_progress(t: &dyn Transport, target_id: u8) -> Result<VolumeProgress> {
    let info = ld::get_info(t, target_id)?;
    let mut operations = BTreeMap::new();
    operations.insert("foreground_init", info.progress.foreground_init);
    operations.insert("background_init", info.progress.background_init);
    Ok(VolumeProgress {
        target_id,
        operations,
    })
}

pub fn volume_check_progress(t: &dyn Transport, target_id: u8) -> Result<VolumeProgress> {
    let info = ld::get_info(t, target_id)?;
    let mut operations = BTreeMap::new();
    operations.insert("consistency_check", info.progress.consistency_check);
    Ok(VolumeProgress {
        target_id,
        operations,
    })
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct EnclosureRow {
    pub enclosure_id: u16,
    pub listed_as_device: bool,
    pub sas_address: Option<String>,
    pub drives: usize,
    pub slots: Vec<u8>,
}

#[derive(Serialize, Clone, Debug)]
pub struct EnclosureList {
    pub enclosures: Vec<EnclosureRow>,
}

pub fn enclosures(list: &[PdAddress]) -> EnclosureList {
    let mut map: BTreeMap<u16, EnclosureRow> = BTreeMap::new();
    for e in list.iter().filter(|a| a.is_enclosure()) {
        map.insert(
            e.device_id,
            EnclosureRow {
                enclosure_id: e.device_id,
                listed_as_device: true,
                sas_address: e.sas_addresses.first().cloned(),
                drives: 0,
                slots: Vec::new(),
            },
        );
    }
    for d in pd::drives(list).filter(|d| d.encl_device_id != NO_ENCLOSURE) {
        let row = map.entry(d.encl_device_id).or_insert_with(|| EnclosureRow {
            enclosure_id: d.encl_device_id,
            listed_as_device: false,
            sas_address: None,
            drives: 0,
            slots: Vec::new(),
        });
        row.drives += 1;
        row.slots.push(d.slot);
    }
    let mut enclosures: Vec<EnclosureRow> = map.into_values().collect();
    for e in &mut enclosures {
        e.slots.sort_unstable();
    }
    EnclosureList { enclosures }
}

#[derive(Serialize, Clone, Debug)]
pub struct ForeignConfig {
    pub index: u8,
    pub config: Option<ConfigData>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ForeignReport {
    pub count: u32,
    pub view: &'static str,
    pub configs: Vec<ForeignConfig>,
}

pub fn foreign_report(t: &dyn Transport, preview: bool, only: Option<u8>) -> Result<ForeignReport> {
    let count = config::foreign_count(t)?;
    let configs = (0..count as u8)
        .filter(|i| only.is_none_or(|o| o == *i))
        .map(|i| {
            let r = if preview {
                config::foreign_preview(t, i)
            } else {
                config::foreign_display(t, i)
            };
            ForeignConfig {
                index: i,
                error: r.as_ref().err().map(|e| format!("{e:#}")),
                config: r.ok(),
            }
        })
        .collect();
    Ok(ForeignReport {
        count,
        view: if preview { "preview" } else { "display" },
        configs,
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct AlarmReport {
    pub present: bool,
    pub alarm_enable_property: u8,
    pub speaker_state: Option<u8>,
    pub speaker_error: Option<String>,
}

pub fn alarm_report(t: &dyn Transport) -> Result<AlarmReport> {
    let info = ctrl::get_info(t)?;
    let state = ctrl::alarm_state(t);
    Ok(AlarmReport {
        present: info.alarm_present,
        alarm_enable_property: info.properties.alarm_enable,
        speaker_error: state.as_ref().err().map(|e| format!("{e:#}")),
        speaker_state: state.ok(),
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct EventList {
    pub events: Vec<Event>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mega::pd::parse_pd_list;
    use crate::mega::pd::tests::pd_list_bytes;

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.50 KiB");
        assert_eq!(blocks(7_812_939_776), "3.64 TiB");
    }

    #[test]
    fn enclosures_come_from_the_pd_list() {
        let list = parse_pd_list(&pd_list_bytes(&[
            (252, 0xffff, 0, 0x0d),
            (8, 252, 1, 0),
            (9, 252, 0, 0),
            (10, 64, 3, 0),
            (11, 0xffff, 2, 0),
        ]));
        let e = enclosures(&list).enclosures;
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].enclosure_id, 64);
        assert!(!e[0].listed_as_device);
        assert_eq!(e[1].enclosure_id, 252);
        assert!(e[1].listed_as_device);
        assert_eq!(e[1].drives, 2);
        assert_eq!(e[1].slots, vec![0, 1]);
    }

    #[test]
    fn time_offset_is_controller_minus_host() {
        let r = time_report(100, crate::mega::event::CONTROLLER_EPOCH + 90);
        assert_eq!(r.offset_seconds, 10);
        assert_eq!(r.controller_time, "2000-01-01 00:01:40");
    }
}
