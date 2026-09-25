use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::transport::{LinuxTransport, Transport, driver_read, driver_write, node_path};
use crate::bytes::{Le, LeMut};
use crate::sysfs::{self, ScsiHost};

pub const PROC_NAME: &str = "mpi3mr";

pub const OPCODE_ADPINFO: u8 = 1;
pub const OPCODE_ADPRESET: u8 = 2;
pub const OPCODE_ALLTGTDEVINFO: u8 = 4;

pub const ADPINFO_LEN: usize = 168;
pub const ADPRESET_LEN: usize = 4;
pub const TGTINFO_HEADER_LEN: usize = 8;
pub const DEVICE_MAP_LEN: usize = 12;

pub const ADP_STATE_OPERATIONAL: u8 = 1;
pub const RESET_SOFT: u8 = 1;
pub const RESET_DIAG_FAULT: u8 = 2;

#[derive(Clone, Debug)]
pub struct Target {
    pub index: u8,
    pub host: ScsiHost,
}

pub fn order_targets(hosts: Vec<ScsiHost>) -> Vec<Target> {
    let mut found: Vec<Target> = hosts
        .into_iter()
        .filter(|h| h.proc_name == PROC_NAME)
        .filter_map(|h| {
            let index = u8::try_from(h.unique_id?).ok()?;
            Some(Target { index, host: h })
        })
        .collect();
    found.sort_by_key(|t| (t.index, t.host.host_no));
    found.dedup_by_key(|t| t.index);
    found
}

pub fn enumerate(sysfs_root: &Path) -> Vec<Target> {
    order_targets(sysfs::scsi_hosts(sysfs_root, &[PROC_NAME]))
}

pub fn select(targets: Vec<Target>, index: u8) -> Result<Target> {
    let ids: Vec<String> = targets.iter().map(|t| t.index.to_string()).collect();
    match targets.into_iter().find(|t| t.index == index) {
        Some(t) => Ok(t),
        None if ids.is_empty() => bail!("no mpi3mr controllers found"),
        None => bail!(
            "controller {index} does not exist, valid controllers are {}",
            ids.join(", ")
        ),
    }
}

pub fn open(target: &Target) -> Result<Box<dyn Transport>> {
    let node = node_path(target.index);
    let t = LinuxTransport::open(&node, target.index)
        .with_context(|| format!("controller {}", target.index))?;
    Ok(Box::new(t))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AdpInfo {
    pub adp_type: u32,
    pub pci_dev_id: u32,
    pub pci_dev_hw_rev: u32,
    pub pci_subsys_dev_id: u32,
    pub pci_subsys_ven_id: u32,
    pub pci_seg_id: u32,
    pub pci_bus: u8,
    pub pci_dev: u8,
    pub pci_func: u8,
    pub app_intfc_ver: u32,
    pub adp_state: u8,
    pub driver_name: String,
    pub driver_version: String,
    pub driver_release_date: String,
    pub os_name: String,
    pub os_version: String,
}

impl AdpInfo {
    pub fn parse(b: &[u8]) -> Self {
        let bits = b.u8_at(0x18);
        let di = 0x30;
        Self {
            adp_type: b.u32_at(0x00),
            pci_dev_id: b.u32_at(0x08),
            pci_dev_hw_rev: b.u32_at(0x0C),
            pci_subsys_dev_id: b.u32_at(0x10),
            pci_subsys_ven_id: b.u32_at(0x14),
            pci_dev: bits & 0x1F,
            pci_func: bits >> 5,
            pci_bus: b.u8_at(0x19),
            pci_seg_id: b.u32_at(0x1C),
            app_intfc_ver: b.u32_at(0x20),
            adp_state: b.u8_at(0x24),
            os_name: b.ascii_at(di + 0x10, 16),
            os_version: b.ascii_at(di + 0x20, 12),
            driver_name: b.ascii_at(di + 0x2C, 20),
            driver_version: b.ascii_at(di + 0x40, 32),
            driver_release_date: b.ascii_at(di + 0x60, 20),
        }
    }

    pub fn pci_address(&self) -> String {
        format!(
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.pci_seg_id, self.pci_bus, self.pci_dev, self.pci_func
        )
    }

    pub fn operational(&self) -> bool {
        self.adp_state == ADP_STATE_OPERATIONAL
    }
}

pub fn adp_info(t: &dyn Transport) -> Result<AdpInfo> {
    Ok(AdpInfo::parse(&driver_read(
        t,
        OPCODE_ADPINFO,
        ADPINFO_LEN,
    )?))
}

pub fn adp_state_name(state: u8) -> &'static str {
    match state {
        1 => "operational",
        2 => "fault",
        3 => "reset in progress",
        4 => "unrecoverable",
        _ => "unknown",
    }
}

pub fn chip_name(device_id: u32) -> Option<&'static str> {
    Some(match device_id {
        0x00A5 => "SAS4116",
        0x00B3 => "SAS5116 MPI",
        0x00B4 => "SAS5116 NVMe",
        0x00B5 => "SAS5116 MPI MGMT",
        0x00B6 => "SAS5116 NVMe MGMT",
        0x00B8 => "SAS5116 PCIe switch",
        0x00F0 => "SAS5248 MPI",
        0x00F1 => "SAS5248 MPI NS",
        0x00F2 => "SAS5248 PCIe switch",
        _ => return None,
    })
}

pub fn chip_of(device_id: u32, fallback: Option<&str>) -> String {
    chip_name(device_id)
        .map(str::to_string)
        .or_else(|| fallback.filter(|n| !n.is_empty()).map(str::to_string))
        .unwrap_or_else(|| format!("unknown (0x{device_id:04x})"))
}

pub fn reset_request(reset_type: u8) -> Vec<u8> {
    let mut b = vec![0u8; ADPRESET_LEN];
    b.put_u8(0x00, reset_type);
    b
}

pub fn reset(t: &dyn Transport, reset_type: u8) -> Result<()> {
    driver_write(t, OPCODE_ADPRESET, &reset_request(reset_type))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceMap {
    pub handle: u16,
    pub persistent_id: u16,
    pub target_id: u32,
    pub bus_id: u8,
}

impl DeviceMap {
    pub fn exposed(&self) -> bool {
        self.target_id != u32::MAX && self.bus_id != 0xFF
    }
}

pub fn parse_device_map(b: &[u8]) -> Vec<DeviceMap> {
    let declared = b.u16_at(0x00) as usize;
    let fits = b.len().saturating_sub(TGTINFO_HEADER_LEN) / DEVICE_MAP_LEN;
    (0..declared.min(fits))
        .map(|i| {
            let o = TGTINFO_HEADER_LEN + i * DEVICE_MAP_LEN;
            DeviceMap {
                handle: b.u16_at(o),
                persistent_id: b.u16_at(o + 2),
                target_id: b.u32_at(o + 4),
                bus_id: b.u8_at(o + 8),
            }
        })
        .collect()
}

pub fn all_target_info(t: &dyn Transport) -> Result<Vec<DeviceMap>> {
    let head = driver_read(t, OPCODE_ALLTGTDEVINFO, TGTINFO_HEADER_LEN)?;
    let count = head.u16_at(0x00) as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    let full = driver_read(
        t,
        OPCODE_ALLTGTDEVINFO,
        TGTINFO_HEADER_LEN + DEVICE_MAP_LEN * count,
    )?;
    Ok(parse_device_map(&full))
}

pub struct OsView<'a> {
    pub sysfs: &'a Path,
    pub host: u32,
}

impl OsView<'_> {
    pub fn block_device(&self, map: &DeviceMap) -> Option<String> {
        if !map.exposed() {
            return None;
        }
        let dir = self.sysfs.join(format!(
            "class/scsi_device/{}:{}:{}:0/device/block",
            self.host, map.bus_id, map.target_id
        ));
        let mut names: Vec<String> = fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names.into_iter().next().map(|n| format!("/dev/{n}"))
    }
}
