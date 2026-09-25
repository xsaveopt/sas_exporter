use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use super::adapter::{self, IocInfo, Target, chip_name, generation_name};
use super::config::{
    self, BIOS_2, BIOS_3, ENCLOSURE_FORM_HANDLE, IO_UNIT_0, IO_UNIT_7, IOC_0, IOC_6, LOG_0,
    MANUFACTURING_0, PHYSDISK_FORM_DEVHANDLE, PHYSDISK_FORM_NUMBER, RAID_PHYS_DISK_0,
    RAID_VOLUME_0, RAID_VOLUME_1, RAID_VOLUME_FORM_HANDLE, SAS_DEVICE_0, SAS_DEVICE_FORM_HANDLE,
    SAS_ENCLOSURE_0, SAS_IO_UNIT_0, SAS_PHY_0, SAS_PHY_1,
};
use super::mpi::{self, IocFacts, bios_version_string, capability_names, version_string};
use super::pages::{
    Bios2, Bios3, BootDevice, Enclosure0, IoUnit0, IoUnit7, Ioc0, Ioc6, LogEntry, Manufacturing0,
    RaidPhysDisk0, RaidVolume0, RaidVolume1, SasDevice0, SasPhy0, SasPhy1, parse_log_0,
    parse_sas_io_unit_0,
};
use super::raid::{self, Progress};
use super::scsi::{self, Path};
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

#[derive(Clone, Debug, Serialize)]
pub struct AdapterRow {
    pub index: usize,
    pub generation: &'static str,
    pub host: u32,
    pub ioc_number: u32,
    pub chip: String,
    pub vendor_id: u16,
    pub device_id: u32,
    pub subsystem_vendor_id: u32,
    pub subsystem_device_id: u32,
    pub pci_address: String,
    pub firmware_version: String,
    pub bios_version: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterList {
    pub adapters: Vec<AdapterRow>,
}

fn chip_of(info: &IocInfo, man0: Option<&Manufacturing0>) -> String {
    chip_name(info.pci_id)
        .map(str::to_string)
        .or_else(|| man0.map(|m| m.chip_name.clone()).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| format!("unknown (0x{:04x})", info.pci_id))
}

pub fn adapter_row(target: &Target, t: &dyn Transport) -> Result<AdapterRow> {
    let info = adapter::ioc_info(t)?;
    let man0 = config::read_page(t, MANUFACTURING_0, 0)?.map(|p| Manufacturing0::parse(&p));
    let ioc0 = config::read_page(t, IOC_0, 0)?.map(|p| Ioc0::parse(&p));
    Ok(AdapterRow {
        index: target.index,
        generation: generation_name(target.generation),
        host: target.host.host_no,
        ioc_number: target.ioc_number,
        chip: chip_of(&info, man0.as_ref()),
        vendor_id: ioc0.as_ref().map_or(0x1000, |p| p.vendor_id),
        device_id: info.pci_id,
        subsystem_vendor_id: info.subsystem_vendor,
        subsystem_device_id: info.subsystem_device,
        pci_address: info.pci_address(),
        firmware_version: version_string(info.firmware_version),
        bios_version: bios_version_string(info.bios_version),
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

#[derive(Clone, Debug, Serialize)]
pub struct ControllerInfo {
    pub index: usize,
    pub generation: &'static str,
    pub host: u32,
    pub ioc_number: u32,
    pub chip: String,
    pub chip_revision: Option<String>,
    pub board_name: Option<String>,
    pub board_assembly: Option<String>,
    pub board_tracer_number: Option<String>,
    pub sas_address: Option<String>,
    pub firmware_version: String,
    pub bios_version: String,
    pub nvdata_version_default: Option<String>,
    pub nvdata_version_persistent: Option<String>,
    pub mpi_version: String,
    pub driver_version: String,
    pub adapter_type: &'static str,
    pub pci_address: String,
    pub vendor_id: Option<u16>,
    pub device_id: u32,
    pub revision: u32,
    pub subsystem_vendor_id: u32,
    pub subsystem_device_id: u32,
    pub ports: u8,
    pub max_targets: u16,
    pub max_enclosures: u16,
    pub max_volumes: u8,
    pub max_dev_handle: u16,
    pub concurrent_commands: u16,
    pub product_id: u16,
    pub raid_support: bool,
    pub capabilities: Vec<&'static str>,
    pub ioc_exceptions: u16,
    pub raid_limits: Option<Ioc6>,
}

pub fn controller_info(target: &Target, t: &dyn Transport) -> Result<ControllerInfo> {
    let info = adapter::ioc_info(t)?;
    let facts = mpi::ioc_facts(t)?;
    let man0 = config::read_page(t, MANUFACTURING_0, 0)?.map(|p| Manufacturing0::parse(&p));
    let io0 = config::read_page(t, IO_UNIT_0, 0)?.map(|p| IoUnit0::parse(&p));
    let ioc0 = config::read_page(t, IOC_0, 0)?.map(|p| Ioc0::parse(&p));
    let bios3 = config::read_page(t, BIOS_3, 0)?.map(|p| Bios3::parse(&p));
    let ioc6 = if facts.raid_support() {
        config::read_page(t, IOC_6, 0)?.map(|p| Ioc6::parse(&p))
    } else {
        None
    };
    let text = |s: String| (!s.is_empty()).then_some(s);
    Ok(ControllerInfo {
        index: target.index,
        generation: generation_name(target.generation),
        host: target.host.host_no,
        ioc_number: t.ioc_number(),
        chip: chip_of(&info, man0.as_ref()),
        chip_revision: man0.as_ref().and_then(|m| text(m.chip_revision.clone())),
        board_name: man0.as_ref().and_then(|m| text(m.board_name.clone())),
        board_assembly: man0.as_ref().and_then(|m| text(m.board_assembly.clone())),
        board_tracer_number: man0
            .as_ref()
            .and_then(|m| text(m.board_tracer_number.clone())),
        sas_address: target.host.attr("host_sas_address"),
        firmware_version: version_string(facts.fw_version),
        bios_version: bios_version_string(bios3.map_or(info.bios_version, |b| b.bios_version)),
        nvdata_version_default: io0
            .as_ref()
            .map(|p| format!("{:08x}", p.nvdata_version_default)),
        nvdata_version_persistent: io0
            .as_ref()
            .map(|p| format!("{:08x}", p.nvdata_version_persistent)),
        mpi_version: facts.mpi_version(),
        driver_version: info.driver_version.clone(),
        adapter_type: adapter::adapter_type_name(info.adapter_type),
        pci_address: info.pci_address(),
        vendor_id: ioc0.as_ref().map(|p| p.vendor_id),
        device_id: info.pci_id,
        revision: info.hw_rev,
        subsystem_vendor_id: info.subsystem_vendor,
        subsystem_device_id: info.subsystem_device,
        ports: facts.number_of_ports,
        max_targets: facts.max_targets,
        max_enclosures: facts.max_enclosures,
        max_volumes: facts.max_volumes,
        max_dev_handle: facts.max_dev_handle,
        concurrent_commands: facts.request_credit,
        product_id: facts.product_id,
        raid_support: facts.raid_support(),
        capabilities: capability_names(facts.ioc_capabilities),
        ioc_exceptions: facts.ioc_exceptions,
        raid_limits: ioc6,
    })
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
pub struct Drive {
    pub address: String,
    pub enclosure: u16,
    pub slot: u16,
    pub handle: u16,
    pub kind: &'static str,
    pub protocol: &'static str,
    pub drive_type: Option<String>,
    pub sas_address: String,
    pub device_name: String,
    pub phy: u8,
    pub state: &'static str,
    pub phys_disk_num: Option<u8>,
    pub linux_channel_target: Option<String>,
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

pub fn physdisk_by_handle(t: &dyn Transport, handle: u16) -> Result<Option<RaidPhysDisk0>> {
    Ok(
        config::read_page(t, RAID_PHYS_DISK_0, PHYSDISK_FORM_DEVHANDLE | handle as u32)?
            .map(|p| RaidPhysDisk0::parse(&p)),
    )
}

pub fn physdisk_by_number(t: &dyn Transport, number: u8) -> Result<Option<RaidPhysDisk0>> {
    Ok(
        config::read_page(t, RAID_PHYS_DISK_0, PHYSDISK_FORM_NUMBER | number as u32)?
            .map(|p| RaidPhysDisk0::parse(&p)),
    )
}

pub fn sas_device_by_handle(t: &dyn Transport, handle: u16) -> Result<Option<SasDevice0>> {
    Ok(
        config::read_page(t, SAS_DEVICE_0, SAS_DEVICE_FORM_HANDLE | handle as u32)?
            .map(|p| SasDevice0::parse(&p)),
    )
}

pub fn listed_devices(t: &dyn Transport) -> Result<Vec<SasDevice0>> {
    Ok(config::sas_devices(t)?
        .iter()
        .map(|p| SasDevice0::parse(p))
        .filter(SasDevice0::is_listed_device)
        .collect())
}

pub fn find_device(t: &dyn Transport, address: DriveAddress) -> Result<SasDevice0> {
    listed_devices(t)?
        .into_iter()
        .find(|d| d.enclosure_handle == address.enclosure && d.slot == address.slot)
        .ok_or_else(|| anyhow!("no device at enclosure:slot {address}"))
}

pub fn describe_drive(t: &dyn Transport, dev: &SasDevice0) -> Result<Drive> {
    let pd = physdisk_by_handle(t, dev.dev_handle)?;
    let path = if pd.as_ref().is_some_and(raid::is_hidden_member) {
        Path::RaidMember
    } else {
        Path::Direct
    };
    let probe = |cdb: Vec<u8>, len: usize| scsi::scsi_read(t, dev.dev_handle, path, &cdb, len).ok();
    let inquiry = probe(scsi::inquiry_cdb(), scsi::INQUIRY_LEN).map(|d| scsi::parse_inquiry(&d));
    let serial = probe(scsi::vpd_cdb(scsi::VPD_SERIAL), scsi::VPD_LEN)
        .and_then(|d| scsi::parse_serial_vpd(&d));
    let guid = probe(scsi::vpd_cdb(scsi::VPD_DEVICE_ID), scsi::VPD_LEN)
        .and_then(|d| scsi::parse_naa_vpd(&d));
    let is_disk = dev.is_disk();
    let (capacity, rotation, temperature) = if is_disk {
        let capacity = probe(scsi::read_capacity16_cdb(), scsi::READ_CAPACITY_LEN)
            .and_then(|d| scsi::parse_read_capacity16(&d))
            .or_else(|| {
                pd.as_ref()
                    .filter(|p| p.block_size != 0)
                    .map(|p| scsi::Capacity {
                        last_lba: p.device_max_lba,
                        block_size: p.block_size as u32,
                    })
            });
        let rotation = probe(
            scsi::vpd_cdb(scsi::VPD_BLOCK_CHARACTERISTICS),
            scsi::VPD_LEN,
        )
        .and_then(|d| scsi::parse_rotation_rate(&d));
        let temperature = if dev.is_ssp_target() && !dev.is_sata() {
            probe(
                scsi::log_sense_cdb(scsi::LOG_PAGE_TEMPERATURE),
                scsi::LOG_SENSE_LEN,
            )
            .and_then(|d| scsi::parse_temperature_log(&d))
            .map(|c| Temperature::from_celsius(c as i32))
        } else {
            None
        };
        (capacity, rotation, temperature)
    } else {
        (None, None, None)
    };
    let from_pd =
        |f: fn(&RaidPhysDisk0) -> &String| pd.as_ref().map(f).filter(|s| !s.is_empty()).cloned();
    let solid_state = rotation == Some(1) || pd.as_ref().is_some_and(RaidPhysDisk0::is_ssd);
    let drive_type = is_disk.then(|| {
        format!(
            "{}_{}",
            dev.protocol(),
            if solid_state { "SSD" } else { "HDD" }
        )
    });
    let kind = if dev.is_sep() {
        "enclosure services"
    } else if is_disk {
        "disk"
    } else {
        "other"
    };
    Ok(Drive {
        address: format!("{}:{}", dev.enclosure_handle, dev.slot),
        enclosure: dev.enclosure_handle,
        slot: dev.slot,
        handle: dev.dev_handle,
        kind,
        protocol: dev.protocol(),
        drive_type,
        sas_address: wwn(dev.sas_address),
        device_name: wwn(dev.device_name),
        phy: dev.phy_num,
        state: raid::drive_state(pd.as_ref(), is_disk),
        phys_disk_num: pd.as_ref().map(|p| p.phys_disk_num),
        linux_channel_target: adapter::scsi_target_of(t, dev.dev_handle)
            .map(|(bus, id)| format!("{bus}:{id}")),
        vendor: inquiry
            .as_ref()
            .map(|i| i.vendor.clone())
            .or_else(|| from_pd(|p| &p.vendor_id)),
        model: inquiry
            .as_ref()
            .map(|i| i.product.clone())
            .or_else(|| from_pd(|p| &p.product_id)),
        firmware_revision: inquiry
            .as_ref()
            .map(|i| i.revision.clone())
            .or_else(|| from_pd(|p| &p.product_revision)),
        serial_number: serial.or_else(|| from_pd(|p| &p.serial_number)),
        guid,
        last_lba: capacity.map(|c| c.last_lba),
        block_size: capacity.map(|c| c.block_size),
        size_mb: capacity.map(|c| scsi::size_mb(c.last_lba, c.block_size)),
        rotation_rate: rotation,
        temperature,
    })
}

pub fn drives(t: &dyn Transport) -> Result<DriveList> {
    let drives = listed_devices(t)?
        .iter()
        .map(|d| describe_drive(t, d))
        .collect::<Result<Vec<_>>>()?;
    Ok(DriveList { drives })
}

pub fn drive(t: &dyn Transport, address: DriveAddress) -> Result<Drive> {
    let dev = find_device(t, address)?;
    describe_drive(t, &dev)
}

pub fn require_physdisk(t: &dyn Transport, address: DriveAddress) -> Result<RaidPhysDisk0> {
    let dev = find_device(t, address)?;
    physdisk_by_handle(t, dev.dev_handle)?
        .ok_or_else(|| anyhow!("{address} is not known to the Integrated RAID firmware"))
}

#[derive(Clone, Debug, Serialize)]
pub struct Member {
    pub phys_disk_num: u8,
    pub address: Option<String>,
    pub handle: Option<u16>,
    pub state: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VolumeStatus {
    pub id: u16,
    pub state: &'static str,
    pub enabled: bool,
    pub quiesced: bool,
    pub inactive: bool,
    pub current_operation: &'static str,
    pub progress: Option<Progress>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Volume {
    pub id: u16,
    pub name: Option<String>,
    pub wwid: Option<String>,
    pub raid_level: &'static str,
    pub state: &'static str,
    pub size_mb: u64,
    pub max_lba: u64,
    pub block_size: u16,
    pub stripe_size: u32,
    pub boot: Option<&'static str>,
    pub members: Vec<Member>,
    pub status: VolumeStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct VolumeList {
    pub volumes: Vec<Volume>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VolumeStatusList {
    pub volumes: Vec<VolumeStatus>,
}

pub fn volume_status(t: &dyn Transport, v: &RaidVolume0) -> VolumeStatus {
    let flags = v.volume_status_flags;
    let operation = raid::current_operation(flags);
    let progress = if operation == "None" {
        None
    } else {
        raid::volume_action_request(raid::ACTION_INDICATOR_STRUCT, v.dev_handle)
            .send_checked(t, "RAID indicator")
            .ok()
            .map(|r| raid::parse_indicator(&r.reply))
    };
    VolumeStatus {
        id: v.dev_handle,
        state: raid::volume_state_name(v.volume_state),
        enabled: flags & raid::STATUS_FLAG_ENABLED != 0,
        quiesced: flags & raid::STATUS_FLAG_QUIESCED != 0,
        inactive: flags & raid::STATUS_FLAG_VOLUME_INACTIVE != 0,
        current_operation: operation,
        progress,
    }
}

fn member(t: &dyn Transport, number: u8) -> Result<Member> {
    let pd = physdisk_by_number(t, number)?;
    let dev = match &pd {
        Some(p) => sas_device_by_handle(t, p.dev_handle)?,
        None => None,
    };
    Ok(Member {
        phys_disk_num: number,
        address: dev
            .as_ref()
            .map(|d| format!("{}:{}", d.enclosure_handle, d.slot)),
        handle: pd.as_ref().map(|p| p.dev_handle),
        state: pd.as_ref().map(|p| raid::drive_state(Some(p), true)),
    })
}

pub fn boot_role(bios2: Option<&Bios2>, wwid: u64) -> Option<&'static str> {
    let b = bios2?;
    if wwid == 0 {
        return None;
    }
    if b.current.identifier() == Some(wwid) {
        Some("primary")
    } else if b.requested_alternate.identifier() == Some(wwid) {
        Some("alternate")
    } else {
        None
    }
}

pub fn describe_volume(
    t: &dyn Transport,
    v: &RaidVolume0,
    bios2: Option<&Bios2>,
) -> Result<Volume> {
    let v1 = config::read_page(
        t,
        RAID_VOLUME_1,
        RAID_VOLUME_FORM_HANDLE | v.dev_handle as u32,
    )?
    .map(|p| RaidVolume1::parse(&p));
    let members = v
        .members
        .iter()
        .map(|m| member(t, m.phys_disk_num))
        .collect::<Result<Vec<_>>>()?;
    let wwid = v1.as_ref().map_or(0, |p| p.wwid);
    Ok(Volume {
        id: v.dev_handle,
        name: v1
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty()),
        wwid: v1.as_ref().map(|p| wwn(p.wwid)),
        raid_level: raid::volume_type_name(v.volume_type),
        state: raid::volume_state_name(v.volume_state),
        size_mb: scsi::size_mb(v.max_lba, v.block_size as u32),
        max_lba: v.max_lba,
        block_size: v.block_size,
        stripe_size: v.stripe_size,
        boot: boot_role(bios2, wwid),
        members,
        status: volume_status(t, v),
    })
}

fn raw_volumes(t: &dyn Transport) -> Result<Vec<RaidVolume0>> {
    Ok(config::raid_volumes(t)?
        .iter()
        .map(|p| RaidVolume0::parse(p))
        .collect())
}

pub fn volumes(t: &dyn Transport) -> Result<VolumeList> {
    let bios2 = config::read_page(t, BIOS_2, 0)?.map(|p| Bios2::parse(&p));
    let volumes = raw_volumes(t)?
        .iter()
        .map(|v| describe_volume(t, v, bios2.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    Ok(VolumeList { volumes })
}

pub fn volume(t: &dyn Transport, id: u16) -> Result<Volume> {
    let page = config::read_page(t, RAID_VOLUME_0, RAID_VOLUME_FORM_HANDLE | id as u32)?
        .ok_or_else(|| anyhow!("no RAID volume with id {id}"))?;
    let bios2 = config::read_page(t, BIOS_2, 0)?.map(|p| Bios2::parse(&p));
    describe_volume(t, &RaidVolume0::parse(&page), bios2.as_ref())
}

pub fn volume_statuses(t: &dyn Transport) -> Result<VolumeStatusList> {
    Ok(VolumeStatusList {
        volumes: raw_volumes(t)?
            .iter()
            .map(|v| volume_status(t, v))
            .collect(),
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct EnclosureInfo {
    pub id: u16,
    pub logical_id: String,
    pub num_slots: u16,
    pub start_slot: u16,
    pub management: &'static str,
    pub sep_handle: u16,
    pub enclosure_level: Option<u8>,
    pub chassis_slot: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnclosureList {
    pub enclosures: Vec<EnclosureInfo>,
}

pub fn raw_enclosures(t: &dyn Transport) -> Result<Vec<Enclosure0>> {
    Ok(config::enclosures(t)?
        .iter()
        .map(|p| Enclosure0::parse(p))
        .collect())
}

pub fn enclosure_list(t: &dyn Transport) -> Result<EnclosureList> {
    let enclosures = raw_enclosures(t)?
        .into_iter()
        .map(|e| EnclosureInfo {
            id: e.enclosure_handle,
            logical_id: wwn(e.enclosure_logical_id),
            num_slots: e.num_slots,
            start_slot: e.start_slot,
            management: e.management(),
            sep_handle: e.sep_dev_handle,
            enclosure_level: e.enclosure_level_valid().then_some(e.enclosure_level),
            chassis_slot: e.chassis_slot_valid().then_some(e.chassis_slot),
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

pub fn phy_list(t: &dyn Transport) -> Result<PhyList> {
    let page = config::require_page(t, SAS_IO_UNIT_0, 0)?;
    let phys = parse_sas_io_unit_0(&page)
        .into_iter()
        .map(|p| {
            let phy0 = config::read_page(t, SAS_PHY_0, p.phy as u32)?.map(|b| SasPhy0::parse(&b));
            let attached = if p.attached_dev_handle != 0 {
                sas_device_by_handle(t, p.attached_dev_handle)?
            } else {
                None
            };
            Ok(PhyInfo {
                phy: p.phy,
                port: p.port,
                enabled: !p.disabled(),
                link_rate: mpi::link_rate_name(p.negotiated_link_rate),
                hw_max_rate: phy0
                    .as_ref()
                    .map(|x| mpi::link_rate_name(x.hw_link_rate >> 4)),
                hw_min_rate: phy0.as_ref().map(|x| mpi::link_rate_name(x.hw_link_rate)),
                attached_handle: p.attached_dev_handle,
                attached_sas_address: attached.as_ref().map(|d| wwn(d.sas_address)),
                attached_device: attached.as_ref().map(|d| {
                    if d.is_sep() {
                        "enclosure services".to_string()
                    } else if d.is_end_device() {
                        format!("{} end device", d.protocol())
                    } else {
                        d.device_type_name().to_string()
                    }
                }),
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

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sensor {
    pub name: &'static str,
    pub raw: i16,
    pub unit: &'static str,
    pub celsius: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControllerTemperature {
    pub sensors: Vec<Sensor>,
}

pub fn sensor(name: &'static str, raw: i16, units: u8) -> Option<Sensor> {
    match units {
        0x01 => Some(Sensor {
            name,
            raw,
            unit: "F",
            celsius: ((raw as f64 - 32.0) * 5.0 / 9.0 * 10.0).round() / 10.0,
        }),
        0x02 => Some(Sensor {
            name,
            raw,
            unit: "C",
            celsius: raw as f64,
        }),
        _ => None,
    }
}

pub fn temperature(t: &dyn Transport) -> Result<ControllerTemperature> {
    let page = config::require_page(t, IO_UNIT_7, 0)?;
    let p = IoUnit7::parse(&page);
    let sensors = [
        sensor("IOC", p.ioc_temperature, p.ioc_temperature_units),
        sensor("Board", p.board_temperature, p.board_temperature_units),
    ]
    .into_iter()
    .flatten()
    .collect();
    Ok(ControllerTemperature { sensors })
}

#[derive(Clone, Debug, Serialize)]
pub struct BootEntry {
    pub role: &'static str,
    pub device: BootDevice,
    pub resolved: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootInfo {
    pub entries: Vec<BootEntry>,
}

fn resolve_boot(
    device: &BootDevice,
    enclosures: &[Enclosure0],
    volumes: &[(u16, u64)],
) -> Option<String> {
    match device {
        BootDevice::EnclosureSlot {
            enclosure_logical_id,
            slot,
        } => enclosures
            .iter()
            .find(|e| e.enclosure_logical_id == *enclosure_logical_id)
            .map(|e| format!("drive {}:{slot}", e.enclosure_handle)),
        other => {
            let id = other.identifier()?;
            volumes
                .iter()
                .find(|(_, wwid)| *wwid == id)
                .map(|(handle, _)| format!("volume {handle}"))
        }
    }
}

fn volume_wwids(t: &dyn Transport) -> Result<Vec<(u16, u64)>> {
    raw_volumes(t)?
        .iter()
        .map(|v| {
            let wwid = config::read_page(
                t,
                RAID_VOLUME_1,
                RAID_VOLUME_FORM_HANDLE | v.dev_handle as u32,
            )?
            .map_or(0, |p| RaidVolume1::parse(&p).wwid);
            Ok((v.dev_handle, wwid))
        })
        .collect()
}

pub fn boot_info(t: &dyn Transport) -> Result<BootInfo> {
    let page = config::require_page(t, BIOS_2, 0)?;
    let b = Bios2::parse(&page);
    let enclosures = raw_enclosures(t)?;
    let volumes = volume_wwids(t)?;
    let entries = [
        ("requested primary", b.requested),
        ("requested alternate", b.requested_alternate),
        ("current", b.current),
    ]
    .into_iter()
    .map(|(role, device)| BootEntry {
        role,
        resolved: resolve_boot(&device, &enclosures, &volumes),
        device,
    })
    .collect();
    Ok(BootInfo { entries })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootTarget {
    Drive(DriveAddress),
    Volume(u16),
}

pub fn boot_device_for(t: &dyn Transport, target: BootTarget) -> Result<(u8, [u8; 24])> {
    let mut d = [0u8; super::pages::BOOT_DEVICE_LEN];
    match target {
        BootTarget::Drive(a) => {
            let page = config::read_page(
                t,
                SAS_ENCLOSURE_0,
                ENCLOSURE_FORM_HANDLE | a.enclosure as u32,
            )?
            .ok_or_else(|| anyhow!("no enclosure {}", a.enclosure))?;
            let e = Enclosure0::parse(&page);
            d[0..8].copy_from_slice(&e.enclosure_logical_id.to_le_bytes());
            d[0x10..0x12].copy_from_slice(&a.slot.to_le_bytes());
            Ok((super::pages::BOOT_FORM_ENCLOSURE_SLOT, d))
        }
        BootTarget::Volume(id) => {
            let page = config::read_page(t, RAID_VOLUME_1, RAID_VOLUME_FORM_HANDLE | id as u32)?
                .ok_or_else(|| anyhow!("no RAID volume with id {id}"))?;
            let wwid = RaidVolume1::parse(&page).wwid;
            if wwid == 0 {
                bail!("RAID volume {id} reports no WWID");
            }
            d[0..8].copy_from_slice(&wwid.to_le_bytes());
            Ok((super::pages::BOOT_FORM_SAS_WWID, d))
        }
    }
}

pub fn apply_boot_device(
    page: &mut [u8],
    alternate: bool,
    form: u8,
    device: &[u8; 24],
) -> Result<()> {
    use super::pages::{
        BIOS2_REQ_ALT_BOOT_DEVICE, BIOS2_REQ_ALT_BOOT_FORM, BIOS2_REQ_BOOT_DEVICE,
        BIOS2_REQ_BOOT_FORM, BOOT_DEVICE_LEN,
    };
    let (form_off, dev_off) = if alternate {
        (BIOS2_REQ_ALT_BOOT_FORM, BIOS2_REQ_ALT_BOOT_DEVICE)
    } else {
        (BIOS2_REQ_BOOT_FORM, BIOS2_REQ_BOOT_DEVICE)
    };
    if page.len() < dev_off + BOOT_DEVICE_LEN {
        bail!(
            "BIOS page 2 is too short ({} bytes) to hold a boot device",
            page.len()
        );
    }
    page[form_off] = (page[form_off] & 0xF0) | (form & 0x0F);
    page[dev_off..dev_off + BOOT_DEVICE_LEN].copy_from_slice(device);
    Ok(())
}

pub fn set_boot(t: &dyn Transport, alternate: bool, target: BootTarget) -> Result<()> {
    let (form, device) = boot_device_for(t, target)?;
    let mut page = config::require_page(t, BIOS_2, 0)?;
    apply_boot_device(&mut page, alternate, form, &device)?;
    config::write_page(t, BIOS_2, 0, config::ACTION_WRITE_NVRAM, &page)
}

#[derive(Clone, Debug, Serialize)]
pub struct LogInfo {
    pub entries: Vec<LogEntry>,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

pub fn log(t: &dyn Transport) -> Result<LogInfo> {
    let raw = config::require_page(t, LOG_0, 0)?;
    Ok(LogInfo {
        entries: parse_log_0(&raw),
        raw,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct FirmwareInfo {
    pub firmware_version: String,
    pub bios_version: String,
    pub nvdata_version_default: Option<String>,
    pub nvdata_version_persistent: Option<String>,
    pub mpi_version: String,
    pub product_id: u16,
    pub product_type: &'static str,
    pub ir_firmware: bool,
}

pub fn product_type(product_id: u16) -> &'static str {
    match product_id & 0x0F00 {
        0x0000 => "A",
        0x0200 => "target/initiator SCSI",
        0x0700 => "IR SCSI",
        _ => "other",
    }
}

pub fn firmware_info(t: &dyn Transport) -> Result<FirmwareInfo> {
    let info = adapter::ioc_info(t)?;
    let facts: IocFacts = mpi::ioc_facts(t)?;
    let bios3 = config::read_page(t, BIOS_3, 0)?.map(|p| Bios3::parse(&p));
    let io0 = config::read_page(t, IO_UNIT_0, 0)?.map(|p| IoUnit0::parse(&p));
    Ok(FirmwareInfo {
        firmware_version: version_string(facts.fw_version),
        bios_version: bios_version_string(bios3.map_or(info.bios_version, |b| b.bios_version)),
        nvdata_version_default: io0
            .as_ref()
            .map(|p| format!("{:08x}", p.nvdata_version_default)),
        nvdata_version_persistent: io0
            .as_ref()
            .map(|p| format!("{:08x}", p.nvdata_version_persistent)),
        mpi_version: facts.mpi_version(),
        product_id: facts.product_id,
        product_type: product_type(facts.product_id),
        ir_firmware: facts.raid_support(),
    })
}
