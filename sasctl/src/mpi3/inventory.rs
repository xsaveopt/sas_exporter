use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use super::adapter::{self, AdpInfo, DeviceMap, OsView, Target, adp_state_name, chip_of};
use super::config::{
    self, DEVICE_0, DEVICE_FORM_HANDLE, ENCLOSURE_0, ENCLOSURE_FORM_HANDLE, IO_UNIT_0, IO_UNIT_4,
    IO_UNIT_19, IOC_0, MANUFACTURING_0, SAS_IO_UNIT_0, SAS_PHY_0, SAS_PHY_1,
};
use super::event::{self, EventLog};
use super::mpi::{self, ImageVersion, IocFacts, Manifest, release_level_name, sas_link_rate_name};
use super::nvme::{self, SmartLog};
use super::pages::{
    Device0, DeviceForm, DeviceTemperature, Enclosure0, IoUnit0, Ioc0, Manufacturing0, Protocol,
    SasPhy0, SasPhy1, Sensor, access_status_name, parse_io_unit_4, parse_io_unit_19,
    parse_sas_io_unit_0, vd_media, vd_os_exposure, vd_state_name,
};
use super::scsi::{self, InformationalExceptions};
use super::transport::Transport;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DriveAddress {
    pub enclosure: u16,
    pub slot: u16,
}

impl FromStr for DriveAddress {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (e, sl) = s
            .split_once(':')
            .ok_or_else(|| format!("expected enclosure:slot, got {s:?}"))?;
        let enclosure = e
            .trim()
            .parse()
            .map_err(|_| format!("invalid enclosure {e:?}"))?;
        let slot = sl
            .trim()
            .parse()
            .map_err(|_| format!("invalid slot {sl:?}"))?;
        Ok(Self { enclosure, slot })
    }
}

impl fmt::Display for DriveAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.enclosure, self.slot)
    }
}

pub fn wwn(value: u64) -> String {
    format!("{value:016x}")
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Temperature {
    pub celsius: i32,
    pub fahrenheit: f64,
}

impl Temperature {
    pub fn from_celsius(celsius: i32) -> Self {
        Self {
            celsius,
            fahrenheit: scsi::fahrenheit(celsius),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterRow {
    pub index: u8,
    pub host: u32,
    pub chip: String,
    pub device_id: u32,
    pub revision: u32,
    pub subsystem_vendor_id: u32,
    pub subsystem_device_id: u32,
    pub pci_address: String,
    pub state: &'static str,
    pub firmware_version: Option<String>,
    pub personality: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterList {
    pub adapters: Vec<AdapterRow>,
}

pub fn adapter_row(target: &Target, t: &dyn Transport) -> Result<AdapterRow> {
    let info = adapter::adp_info(t)?;
    let facts = if info.operational() {
        Some(mpi::ioc_facts(t)?)
    } else {
        None
    };
    Ok(AdapterRow {
        index: target.index,
        host: target.host.host_no,
        chip: chip_of(info.pci_dev_id, None),
        device_id: info.pci_dev_id,
        revision: info.pci_dev_hw_rev,
        subsystem_vendor_id: info.pci_subsys_ven_id,
        subsystem_device_id: info.pci_subsys_dev_id,
        pci_address: info.pci_address(),
        state: adp_state_name(info.adp_state),
        firmware_version: facts.as_ref().map(|f| f.fw_version.to_string()),
        personality: facts.as_ref().map(IocFacts::personality),
    })
}

pub fn list_adapters(
    targets: &[Target],
    opener: impl Fn(&Target) -> Result<Box<dyn Transport>>,
) -> Result<AdapterList> {
    let adapters = targets
        .iter()
        .map(|target| {
            let t = opener(target)?;
            adapter_row(target, t.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(AdapterList { adapters })
}

fn operational(t: &dyn Transport) -> Result<AdpInfo> {
    let info = adapter::adp_info(t)?;
    if !info.operational() {
        bail!(
            "controller {} is {} (adapter state {})",
            t.mrioc_id(),
            adp_state_name(info.adp_state),
            info.adp_state
        );
    }
    Ok(info)
}

pub struct Topology {
    pub devices: Vec<(DeviceMap, Device0)>,
    pub enclosures: Vec<Enclosure0>,
}

impl Topology {
    pub fn scan(t: &dyn Transport) -> Result<Self> {
        let mut devices = Vec::new();
        for map in adapter::all_target_info(t)? {
            if let Some(p) = config::read_page(t, DEVICE_0, DEVICE_FORM_HANDLE | map.handle as u32)?
            {
                devices.push((map, Device0::parse(&p)));
            }
        }
        let handles: BTreeSet<u16> = devices
            .iter()
            .map(|(_, d)| d.enclosure_handle)
            .filter(|h| *h != 0)
            .collect();
        let mut enclosures = Vec::new();
        for h in handles {
            if let Some(p) = config::read_page(t, ENCLOSURE_0, ENCLOSURE_FORM_HANDLE | h as u32)? {
                enclosures.push(Enclosure0::parse(&p));
            }
        }
        Ok(Self {
            devices,
            enclosures,
        })
    }

    pub fn is_sep(&self, handle: u16) -> bool {
        self.enclosures.iter().any(|e| e.sep() == Some(handle))
    }

    pub fn enclosure(&self, handle: u16) -> Option<&Enclosure0> {
        self.enclosures
            .iter()
            .find(|e| e.enclosure_handle == handle)
    }

    pub fn drives(&self) -> impl Iterator<Item = &(DeviceMap, Device0)> {
        self.devices
            .iter()
            .filter(|(_, d)| d.protocol().is_some() && !self.is_sep(d.dev_handle))
    }

    pub fn find_drive(&self, address: DriveAddress) -> Result<&(DeviceMap, Device0)> {
        self.drives()
            .find(|(_, d)| d.enclosure_handle == address.enclosure && d.slot == address.slot)
            .ok_or_else(|| anyhow!("no drive at enclosure:slot {address}"))
    }

    pub fn volumes(&self) -> impl Iterator<Item = &(DeviceMap, Device0)> {
        self.devices.iter().filter(|(_, d)| d.vd().is_some())
    }

    pub fn device(&self, handle: u16) -> Option<&Device0> {
        self.devices
            .iter()
            .map(|(_, d)| d)
            .find(|d| d.dev_handle == handle)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DeviceCounts {
    pub sas_sata: usize,
    pub pcie: usize,
    pub virtual_disks: usize,
    pub drives: usize,
    pub expanders: usize,
    pub enclosures: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Limits {
    pub max_vds: u16,
    pub max_raid_pds: u16,
    pub max_host_pds: u16,
    pub max_adv_host_pds: u16,
    pub max_nvme: u16,
    pub max_sas_initiators: u16,
    pub max_sas_expanders: u16,
    pub max_enclosures: u16,
    pub max_pcie_switches: u16,
    pub max_outstanding_requests: u16,
    pub max_persistent_id: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControllerInfo {
    pub index: u8,
    pub host: u32,
    pub state: &'static str,
    pub chip: String,
    pub chip_revision: Option<String>,
    pub board_name: Option<String>,
    pub board_assembly: Option<String>,
    pub board_tracer_number: Option<String>,
    pub board_revision: Option<String>,
    pub board_mfg_date: Option<String>,
    pub product_name: Option<String>,
    pub firmware_version: String,
    pub package_version: Option<String>,
    pub mpi_version: String,
    pub driver_name: String,
    pub driver_version: String,
    pub pci_address: String,
    pub vendor_id: Option<u16>,
    pub device_id: u32,
    pub revision: u32,
    pub subsystem_vendor_id: u32,
    pub subsystem_device_id: u32,
    pub personality: &'static str,
    pub raid_supported: bool,
    pub sas_transport: bool,
    pub capabilities: Vec<&'static str>,
    pub protocols: Vec<&'static str>,
    pub ioc_exceptions: u16,
    pub limits: Limits,
    pub devices: DeviceCounts,
    pub sensors: Vec<Sensor>,
}

fn text(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

pub fn controller_info(target: &Target, t: &dyn Transport) -> Result<ControllerInfo> {
    let info = operational(t)?;
    let facts = mpi::ioc_facts(t)?;
    let man0 = config::read_page(t, MANUFACTURING_0, 0)?.map(|p| Manufacturing0::parse(&p));
    let ioc0 = config::read_page(t, IOC_0, 0)?.map(|p| Ioc0::parse(&p));
    let manifest = mpi::manifest(t);
    let sensors = config::read_page(t, IO_UNIT_4, 0)?
        .map(|p| parse_io_unit_4(&p))
        .unwrap_or_default();
    let topo = Topology::scan(t)?;
    let mut devices = DeviceCounts {
        enclosures: topo.enclosures.len(),
        drives: topo.drives().count(),
        ..Default::default()
    };
    for (_, d) in &topo.devices {
        match d.form {
            DeviceForm::SasSata(_) => devices.sas_sata += 1,
            DeviceForm::Pcie(_) => devices.pcie += 1,
            DeviceForm::Vd(_) => devices.virtual_disks += 1,
            DeviceForm::Other { .. } => {}
        }
        if d.is_expander() {
            devices.expanders += 1;
        }
    }
    let m = man0.as_ref();
    Ok(ControllerInfo {
        index: target.index,
        host: target.host.host_no,
        state: adp_state_name(info.adp_state),
        chip: chip_of(info.pci_dev_id, m.map(|m| m.chip_name.as_str())),
        chip_revision: m.and_then(|m| text(&m.chip_revision)),
        board_name: m.and_then(|m| text(&m.board_name)),
        board_assembly: m.and_then(|m| text(&m.board_assembly)),
        board_tracer_number: m.and_then(|m| text(&m.board_tracer_number)),
        board_revision: m.and_then(|m| text(&m.board_revision)),
        board_mfg_date: m.and_then(|m| m.board_mfg_date.clone()),
        product_name: m.and_then(|m| text(&m.product_name)),
        firmware_version: facts.fw_version.to_string(),
        package_version: manifest.as_ref().map(|x| x.package_version.to_string()),
        mpi_version: format!("0x{:08x}", facts.mpi_version),
        driver_name: info.driver_name.clone(),
        driver_version: info.driver_version.clone(),
        pci_address: info.pci_address(),
        vendor_id: ioc0.as_ref().map(|p| p.vendor_id),
        device_id: info.pci_dev_id,
        revision: info.pci_dev_hw_rev,
        subsystem_vendor_id: info.pci_subsys_ven_id,
        subsystem_device_id: info.pci_subsys_dev_id,
        personality: facts.personality(),
        raid_supported: facts.raid_supported(),
        sas_transport: facts.ioc_capabilities & mpi::CAPABILITY_MULTIPATH_SUPPORTED == 0,
        capabilities: facts.capabilities(),
        protocols: facts.protocols(),
        ioc_exceptions: facts.ioc_exceptions,
        limits: Limits {
            max_vds: facts.max_vds,
            max_raid_pds: facts.max_raid_pds,
            max_host_pds: facts.max_host_pds,
            max_adv_host_pds: facts.max_adv_host_pds,
            max_nvme: facts.max_nvme,
            max_sas_initiators: facts.max_sas_initiators,
            max_sas_expanders: facts.max_sas_expanders,
            max_enclosures: facts.max_enclosures,
            max_pcie_switches: facts.max_pcie_switches,
            max_outstanding_requests: facts.max_outstanding_requests,
            max_persistent_id: facts.max_persistent_id,
        },
        devices,
        sensors,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct Drive {
    pub address: String,
    pub enclosure: u16,
    pub slot: u16,
    pub handle: u16,
    pub persistent_id: u16,
    pub protocol: &'static str,
    pub drive_type: Option<String>,
    pub state: &'static str,
    pub access_status: u8,
    pub exposed: bool,
    pub wwid: String,
    pub sas_address: Option<String>,
    pub phy: Option<u8>,
    pub link_rate: Option<&'static str>,
    pub enclosure_logical_id: Option<String>,
    pub linux_channel_target: Option<String>,
    pub os_device: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub firmware_revision: Option<String>,
    pub serial_number: Option<String>,
    pub guid: Option<String>,
    pub last_lba: Option<u64>,
    pub block_size: Option<u32>,
    pub size_mb: Option<u64>,
    pub rotation_rate: Option<u16>,
    pub temperature: Option<Temperature>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DriveList {
    pub drives: Vec<Drive>,
}

#[derive(Default)]
struct Probe {
    vendor: Option<String>,
    model: Option<String>,
    revision: Option<String>,
    serial: Option<String>,
    guid: Option<String>,
    capacity: Option<scsi::Capacity>,
    rotation: Option<u16>,
    temperature: Option<Temperature>,
}

fn probe_scsi(t: &dyn Transport, handle: u16, protocol: Protocol) -> Probe {
    let read = |cdb: Vec<u8>, len: usize| scsi::scsi_read(t, handle, &cdb, len).ok();
    let inquiry = read(scsi::inquiry_cdb(), scsi::INQUIRY_LEN).map(|d| scsi::parse_inquiry(&d));
    Probe {
        vendor: inquiry.as_ref().map(|i| i.vendor.clone()),
        model: inquiry.as_ref().map(|i| i.product.clone()),
        revision: inquiry.as_ref().map(|i| i.revision.clone()),
        serial: read(scsi::vpd_cdb(scsi::VPD_SERIAL), scsi::VPD_LEN)
            .and_then(|d| scsi::parse_serial_vpd(&d)),
        guid: read(scsi::vpd_cdb(scsi::VPD_DEVICE_ID), scsi::VPD_LEN)
            .and_then(|d| scsi::parse_naa_vpd(&d)),
        capacity: read(scsi::read_capacity16_cdb(), scsi::READ_CAPACITY_LEN)
            .and_then(|d| scsi::parse_read_capacity16(&d)),
        rotation: read(
            scsi::vpd_cdb(scsi::VPD_BLOCK_CHARACTERISTICS),
            scsi::VPD_LEN,
        )
        .and_then(|d| scsi::parse_rotation_rate(&d)),
        temperature: if protocol == Protocol::Sas {
            read(
                scsi::log_sense_cdb(scsi::LOG_PAGE_TEMPERATURE),
                scsi::LOG_SENSE_LEN,
            )
            .and_then(|d| scsi::parse_temperature_log(&d))
            .map(|c| Temperature::from_celsius(c as i32))
        } else {
            None
        },
    }
}

fn probe_nvme(t: &dyn Transport, handle: u16) -> Probe {
    let id = nvme::admin(t, &nvme::identify_controller_request(handle))
        .ok()
        .map(|d| nvme::parse_identify_controller(&d));
    let smart = nvme::admin(t, &nvme::smart_log_request(handle))
        .ok()
        .map(|d| nvme::parse_smart_log(&d));
    Probe {
        vendor: None,
        model: id.as_ref().map(|i| i.model.clone()),
        revision: id.as_ref().map(|i| i.firmware.clone()),
        serial: id.as_ref().map(|i| i.serial.clone()),
        temperature: smart
            .and_then(|s| s.celsius())
            .map(Temperature::from_celsius),
        ..Default::default()
    }
}

pub fn drive_state(d: &Device0) -> &'static str {
    access_status_name(d.access_status)
}

pub fn describe_drive(
    t: &dyn Transport,
    topo: &Topology,
    map: &DeviceMap,
    d: &Device0,
    os: &OsView,
) -> Drive {
    let protocol = d.protocol().unwrap_or(Protocol::Sas);
    let probe = match protocol {
        Protocol::Nvme => probe_nvme(t, d.dev_handle),
        _ => probe_scsi(t, d.dev_handle, protocol),
    };
    let solid_state = protocol == Protocol::Nvme || probe.rotation == Some(1);
    let media = if probe.rotation.is_some() || protocol == Protocol::Nvme {
        Some(if solid_state { "SSD" } else { "HDD" })
    } else {
        None
    };
    let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
    Drive {
        address: format!("{}:{}", d.enclosure_handle, d.slot),
        enclosure: d.enclosure_handle,
        slot: d.slot,
        handle: d.dev_handle,
        persistent_id: d.persistent_id,
        protocol: protocol.name(),
        drive_type: media.map(|m| format!("{}_{m}", protocol.name())),
        state: drive_state(d),
        access_status: d.access_status,
        exposed: !d.hidden(),
        wwid: wwn(d.wwid),
        sas_address: d.sas().map(|s| wwn(s.sas_address)),
        phy: d.sas().map(|s| s.phy_num),
        link_rate: d.link_rate(),
        enclosure_logical_id: topo
            .enclosure(d.enclosure_handle)
            .map(|e| wwn(e.enclosure_logical_id)),
        linux_channel_target: map
            .exposed()
            .then(|| format!("{}:{}", map.bus_id, map.target_id)),
        os_device: os.block_device(map),
        vendor: nonempty(probe.vendor),
        model: nonempty(probe.model),
        firmware_revision: nonempty(probe.revision),
        serial_number: nonempty(probe.serial),
        guid: probe.guid,
        last_lba: probe.capacity.map(|c| c.last_lba),
        block_size: probe.capacity.map(|c| c.block_size),
        size_mb: probe.capacity.map(scsi::size_mb),
        rotation_rate: probe.rotation,
        temperature: probe.temperature,
    }
}

pub fn drives(t: &dyn Transport, os: &OsView) -> Result<DriveList> {
    let topo = Topology::scan(t)?;
    let drives = topo
        .drives()
        .map(|(m, d)| describe_drive(t, &topo, m, d, os))
        .collect();
    Ok(DriveList { drives })
}

pub fn drive(t: &dyn Transport, os: &OsView, address: DriveAddress) -> Result<Drive> {
    let topo = Topology::scan(t)?;
    let (m, d) = topo.find_drive(address)?;
    Ok(describe_drive(t, &topo, m, d, os))
}

#[derive(Clone, Debug, Serialize)]
pub struct DriveSmart {
    pub address: String,
    pub protocol: &'static str,
    pub healthy: bool,
    pub informational_exceptions: Option<InformationalExceptions>,
    pub nvme: Option<SmartLog>,
    pub nvme_warnings: Vec<&'static str>,
    pub temperature: Option<Temperature>,
}

pub fn drive_smart(t: &dyn Transport, address: DriveAddress) -> Result<DriveSmart> {
    let topo = Topology::scan(t)?;
    let (_, d) = topo.find_drive(address)?;
    let handle = d.dev_handle;
    match d.protocol() {
        Some(Protocol::Sas) => {
            let ie = scsi::scsi_read(
                t,
                handle,
                &scsi::log_sense_cdb(scsi::LOG_PAGE_INFORMATIONAL_EXCEPTIONS),
                scsi::LOG_SENSE_LEN,
            )
            .map(|d| scsi::parse_ie_log(&d))?
            .ok_or_else(|| anyhow!("{address} returned no informational exceptions parameter"))?;
            let temperature = scsi::scsi_read(
                t,
                handle,
                &scsi::log_sense_cdb(scsi::LOG_PAGE_TEMPERATURE),
                scsi::LOG_SENSE_LEN,
            )
            .ok()
            .and_then(|d| scsi::parse_temperature_log(&d))
            .map(|c| Temperature::from_celsius(c as i32));
            Ok(DriveSmart {
                address: address.to_string(),
                protocol: "SAS",
                healthy: ie.asc == 0 && !ie.failure_predicted(),
                informational_exceptions: Some(ie),
                nvme: None,
                nvme_warnings: Vec::new(),
                temperature,
            })
        }
        Some(Protocol::Nvme) => {
            let log = nvme::parse_smart_log(&nvme::admin(t, &nvme::smart_log_request(handle))?);
            let warnings = nvme::critical_warnings(log.critical_warning);
            Ok(DriveSmart {
                address: address.to_string(),
                protocol: "NVMe",
                healthy: log.critical_warning == 0,
                temperature: log.celsius().map(Temperature::from_celsius),
                informational_exceptions: None,
                nvme: Some(log),
                nvme_warnings: warnings,
            })
        }
        _ => bail!(
            "SMART data for SATA drive {address} needs ATA passthrough through the controller SATL, which is not documented"
        ),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Volume {
    pub id: u16,
    pub handle: u16,
    pub raid_level: String,
    pub state: &'static str,
    pub access_status: &'static str,
    pub media: Vec<&'static str>,
    pub os_exposure_hint: &'static str,
    pub wwid: String,
    pub linux_channel_target: Option<String>,
    pub os_device: Option<String>,
    pub size_mb: Option<u64>,
    pub last_lba: Option<u64>,
    pub block_size: Option<u32>,
    pub io_throttle_group: u16,
    pub io_throttle_low_mib: u16,
    pub io_throttle_high_mib: u16,
    pub abort_timeout_seconds: Option<u8>,
    pub reset_timeout_seconds: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VolumeList {
    pub volumes: Vec<Volume>,
}

pub fn describe_volume(t: &dyn Transport, map: &DeviceMap, d: &Device0, os: &OsView) -> Volume {
    let vd = d.vd().cloned().unwrap_or_default();
    let capacity = scsi::scsi_read(
        t,
        d.dev_handle,
        &scsi::read_capacity16_cdb(),
        scsi::READ_CAPACITY_LEN,
    )
    .ok()
    .and_then(|b| scsi::parse_read_capacity16(&b));
    Volume {
        id: d.persistent_id,
        handle: d.dev_handle,
        raid_level: format!("RAID{}", vd.raid_level),
        state: vd_state_name(vd.vd_state),
        access_status: access_status_name(d.access_status),
        media: vd_media(vd.device_info),
        os_exposure_hint: vd_os_exposure(vd.flags),
        wwid: wwn(d.wwid),
        linux_channel_target: map
            .exposed()
            .then(|| format!("{}:{}", map.bus_id, map.target_id)),
        os_device: os.block_device(map),
        size_mb: capacity.map(scsi::size_mb),
        last_lba: capacity.map(|c| c.last_lba),
        block_size: capacity.map(|c| c.block_size),
        io_throttle_group: vd.io_throttle_group,
        io_throttle_low_mib: vd.io_throttle_group_low,
        io_throttle_high_mib: vd.io_throttle_group_high,
        abort_timeout_seconds: (vd.vd_abort_to != 0).then_some(vd.vd_abort_to),
        reset_timeout_seconds: (vd.vd_reset_to != 0).then_some(vd.vd_reset_to),
    }
}

pub fn volumes(t: &dyn Transport, os: &OsView) -> Result<VolumeList> {
    let topo = Topology::scan(t)?;
    let volumes = topo
        .volumes()
        .map(|(m, d)| describe_volume(t, m, d, os))
        .collect();
    Ok(VolumeList { volumes })
}

pub fn volume(t: &dyn Transport, os: &OsView, id: u16) -> Result<Volume> {
    let topo = Topology::scan(t)?;
    let (m, d) = topo
        .volumes()
        .find(|(_, d)| d.persistent_id == id)
        .ok_or_else(|| anyhow!("no virtual disk with id {id}"))?;
    Ok(describe_volume(t, m, d, os))
}

#[derive(Clone, Debug, Serialize)]
pub struct EnclosureInfo {
    pub id: u16,
    pub logical_id: String,
    pub num_slots: u16,
    pub enclosure_type: &'static str,
    pub management: &'static str,
    pub sep_handle: Option<u16>,
    pub enclosure_level: u8,
    pub chassis_slot: Option<u8>,
    pub vendor: Option<String>,
    pub product: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnclosureList {
    pub enclosures: Vec<EnclosureInfo>,
}

pub fn enclosure_list(t: &dyn Transport) -> Result<EnclosureList> {
    let topo = Topology::scan(t)?;
    let enclosures = topo
        .enclosures
        .iter()
        .map(|e| {
            let inquiry = e.sep().and_then(|h| {
                scsi::scsi_read(t, h, &scsi::inquiry_cdb(), scsi::INQUIRY_LEN)
                    .ok()
                    .map(|d| scsi::parse_inquiry(&d))
            });
            EnclosureInfo {
                id: e.enclosure_handle,
                logical_id: wwn(e.enclosure_logical_id),
                num_slots: e.num_slots,
                enclosure_type: e.enclosure_type(),
                management: e.management(),
                sep_handle: e.sep(),
                enclosure_level: e.enclosure_level,
                chassis_slot: e.chassis_slot_valid().then_some(e.chassis_slot),
                vendor: inquiry.as_ref().and_then(|i| text(&i.vendor)),
                product: inquiry.as_ref().and_then(|i| text(&i.product)),
            }
        })
        .collect();
    Ok(EnclosureList { enclosures })
}

#[derive(Clone, Debug, Serialize)]
pub struct PhyInfo {
    pub phy: u8,
    pub port: u8,
    pub enabled: bool,
    pub link_rate: &'static str,
    pub hw_max_rate: Option<&'static str>,
    pub hw_min_rate: Option<&'static str>,
    pub attached_handle: u16,
    pub attached_sas_address: Option<String>,
    pub attached_device: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PhyList {
    pub phys: Vec<PhyInfo>,
}

fn attached_kind(d: &Device0) -> String {
    match d.protocol() {
        Some(p) => format!("{} end device", p.name()),
        None if d.is_expander() => "expander".to_string(),
        None => match &d.form {
            DeviceForm::Vd(_) => "virtual disk".to_string(),
            DeviceForm::Pcie(_) => "PCIe device".to_string(),
            _ => "other".to_string(),
        },
    }
}

pub fn phy_list(t: &dyn Transport) -> Result<PhyList> {
    let page = config::require_page(t, SAS_IO_UNIT_0, 0)?;
    let phys = parse_sas_io_unit_0(&page)
        .into_iter()
        .map(|p| {
            let phy0 = config::read_page(t, SAS_PHY_0, p.phy as u32)?.map(|b| SasPhy0::parse(&b));
            let attached = if p.attached_dev_handle != 0 {
                config::read_page(
                    t,
                    DEVICE_0,
                    DEVICE_FORM_HANDLE | p.attached_dev_handle as u32,
                )?
                .map(|b| Device0::parse(&b))
            } else {
                None
            };
            Ok(PhyInfo {
                phy: p.phy,
                port: p.io_unit_port,
                enabled: !p.disabled(),
                link_rate: sas_link_rate_name(p.negotiated_link_rate),
                hw_max_rate: phy0
                    .as_ref()
                    .map(|x| sas_link_rate_name(x.hw_link_rate >> 4)),
                hw_min_rate: phy0.as_ref().map(|x| sas_link_rate_name(x.hw_link_rate)),
                attached_handle: p.attached_dev_handle,
                attached_sas_address: attached
                    .as_ref()
                    .and_then(|d| d.sas().map(|s| wwn(s.sas_address))),
                attached_device: attached.as_ref().map(attached_kind),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PhyList { phys })
}

#[derive(Clone, Debug, Serialize)]
pub struct PhyErrors {
    pub phy: u8,
    pub counters: Option<SasPhy1>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PhyErrorList {
    pub phys: Vec<PhyErrors>,
}

pub fn phy_errors(t: &dyn Transport) -> Result<PhyErrorList> {
    let page = config::require_page(t, SAS_IO_UNIT_0, 0)?;
    let phys = parse_sas_io_unit_0(&page)
        .iter()
        .map(|p| {
            Ok(PhyErrors {
                phy: p.phy,
                counters: config::read_page(t, SAS_PHY_1, p.phy as u32)?
                    .map(|b| SasPhy1::parse(&b)),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PhyErrorList { phys })
}

pub fn phy_exists(t: &dyn Transport, phy: u8) -> Result<()> {
    let page = config::require_page(t, SAS_IO_UNIT_0, 0)?;
    let count = parse_sas_io_unit_0(&page).len();
    if phy as usize >= count {
        bail!("phy {phy} does not exist, the controller has {count} phys");
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct DriveTemperature {
    pub handle: u16,
    pub persistent_id: u16,
    pub address: Option<String>,
    pub raw: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControllerTemperature {
    pub sensors: Vec<Sensor>,
    pub drives: Vec<DriveTemperature>,
}

pub fn temperature(t: &dyn Transport) -> Result<ControllerTemperature> {
    let sensors = parse_io_unit_4(&config::require_page(t, IO_UNIT_4, 0)?);
    let devices: Vec<DeviceTemperature> = config::read_page(t, IO_UNIT_19, 0)?
        .map(|p| parse_io_unit_19(&p))
        .unwrap_or_default();
    let topo = if devices.is_empty() {
        None
    } else {
        Some(Topology::scan(t)?)
    };
    let drives = devices
        .into_iter()
        .map(|d| DriveTemperature {
            handle: d.dev_handle,
            persistent_id: d.persistent_id,
            address: topo
                .as_ref()
                .and_then(|x| x.device(d.dev_handle))
                .map(|x| format!("{}:{}", x.enclosure_handle, x.slot)),
            raw: d.raw,
        })
        .collect();
    Ok(ControllerTemperature { sensors, drives })
}

pub fn events(t: &dyn Transport, latest: Option<u32>) -> Result<EventLog> {
    event::read_log(t, latest)
}

#[derive(Clone, Debug, Serialize)]
pub struct FirmwareInfo {
    pub firmware_version: String,
    pub package_version: Option<String>,
    pub package_release_level: Option<&'static str>,
    pub package_ids: Option<String>,
    pub nvdata_version_default: Option<String>,
    pub nvdata_version_persistent: Option<String>,
    pub mpi_version: String,
    pub product_id: u16,
    pub personality: &'static str,
}

fn package_ids(m: &Manifest) -> String {
    format!(
        "{:04x}:{:04x} subsystem {:04x}:{:04x}",
        m.vendor_id, m.device_id, m.subsystem_vendor_id, m.subsystem_id
    )
}

pub fn firmware_info(t: &dyn Transport) -> Result<FirmwareInfo> {
    let facts = mpi::ioc_facts(t)?;
    let manifest = mpi::manifest(t);
    let io0 = config::read_page(t, IO_UNIT_0, 0)?.map(|p| IoUnit0::parse(&p));
    let version = |v: ImageVersion| v.to_string();
    Ok(FirmwareInfo {
        firmware_version: version(facts.fw_version),
        package_version: manifest.as_ref().map(|m| version(m.package_version)),
        package_release_level: manifest
            .as_ref()
            .map(|m| release_level_name(m.release_level)),
        package_ids: manifest.as_ref().map(package_ids),
        nvdata_version_default: io0
            .as_ref()
            .map(|p| format!("{:08x}", p.nvdata_version_default)),
        nvdata_version_persistent: io0
            .as_ref()
            .map(|p| format!("{:08x}", p.nvdata_version_persistent)),
        mpi_version: format!("0x{:08x}", facts.mpi_version),
        product_id: facts.product_id,
        personality: facts.personality(),
    })
}
