use serde::Serialize;

use crate::bytes::Le;

pub const DEVICE_INFO_SEP: u32 = 0x0000_4000;
pub const DEVICE_INFO_SSP_TARGET: u32 = 0x0000_0400;
pub const DEVICE_INFO_STP_TARGET: u32 = 0x0000_0200;
pub const DEVICE_INFO_SMP_TARGET: u32 = 0x0000_0100;
pub const DEVICE_INFO_SATA_DEVICE: u32 = 0x0000_0080;
pub const DEVICE_INFO_TYPE_MASK: u32 = 0x0000_0007;
pub const DEVICE_TYPE_END_DEVICE: u32 = 0x1;

fn bounded_count(page: &[u8], start: usize, stride: usize, declared: usize) -> usize {
    let fits = page.len().saturating_sub(start) / stride;
    declared.min(fits)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Manufacturing0 {
    pub chip_name: String,
    pub chip_revision: String,
    pub board_name: String,
    pub board_assembly: String,
    pub board_tracer_number: String,
}

impl Manufacturing0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            chip_name: p.ascii_at(0x04, 16),
            chip_revision: p.ascii_at(0x14, 8),
            board_name: p.ascii_at(0x1C, 16),
            board_assembly: p.ascii_at(0x2C, 16),
            board_tracer_number: p.ascii_at(0x3C, 16),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct IoUnit0 {
    pub unique_value: u64,
    pub nvdata_version_default: u32,
    pub nvdata_version_persistent: u32,
}

impl IoUnit0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            unique_value: p.u64_at(0x04),
            nvdata_version_default: p.u32_at(0x0C),
            nvdata_version_persistent: p.u32_at(0x10),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct IoUnit7 {
    pub pcie_width: u8,
    pub pcie_speed: u8,
    pub ioc_temperature: i16,
    pub ioc_temperature_units: u8,
    pub board_temperature: i16,
    pub board_temperature_units: u8,
}

impl IoUnit7 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            pcie_width: p.u8_at(0x06),
            pcie_speed: p.u8_at(0x07),
            ioc_temperature: p.i16_at(0x10),
            ioc_temperature_units: p.u8_at(0x12),
            board_temperature: p.i16_at(0x14),
            board_temperature_units: p.u8_at(0x16),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Ioc0 {
    pub vendor_id: u16,
    pub device_id: u16,
    pub revision_id: u8,
    pub class_code: u32,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
}

impl Ioc0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            vendor_id: p.u16_at(0x0C),
            device_id: p.u16_at(0x0E),
            revision_id: p.u8_at(0x10),
            class_code: p.u32_at(0x14),
            subsystem_vendor_id: p.u16_at(0x18),
            subsystem_id: p.u16_at(0x1A),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Ioc6 {
    pub capabilities_flags: u32,
    pub max_drives_raid0: u8,
    pub max_drives_raid1: u8,
    pub max_drives_raid1e: u8,
    pub max_drives_raid10: u8,
    pub min_drives_raid0: u8,
    pub min_drives_raid1: u8,
    pub min_drives_raid1e: u8,
    pub min_drives_raid10: u8,
    pub max_global_hot_spares: u8,
    pub max_phys_disks: u8,
    pub max_volumes: u8,
    pub stripe_map_raid0: u32,
    pub stripe_map_raid1e: u32,
    pub stripe_map_raid10: u32,
}

impl Ioc6 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            capabilities_flags: p.u32_at(0x04),
            max_drives_raid0: p.u8_at(0x08),
            max_drives_raid1: p.u8_at(0x09),
            max_drives_raid1e: p.u8_at(0x0A),
            max_drives_raid10: p.u8_at(0x0B),
            min_drives_raid0: p.u8_at(0x0C),
            min_drives_raid1: p.u8_at(0x0D),
            min_drives_raid1e: p.u8_at(0x0E),
            min_drives_raid10: p.u8_at(0x0F),
            max_global_hot_spares: p.u8_at(0x14),
            max_phys_disks: p.u8_at(0x15),
            max_volumes: p.u8_at(0x16),
            stripe_map_raid0: p.u32_at(0x1C),
            stripe_map_raid1e: p.u32_at(0x20),
            stripe_map_raid10: p.u32_at(0x24),
        }
    }
}

pub const MAN4_NO_MIX_SAS_SATA: u32 = 0x0000_0001;
pub const MAN4_MIX_SSD_AND_NON_SSD: u32 = 0x0000_4000;
pub const MAN4_MIX_SSD_SAS_SATA: u32 = 0x0000_8000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Manufacturing4 {
    pub flags: u32,
    pub raid0_volume_settings: u32,
    pub raid1e_volume_settings: u32,
    pub raid1_volume_settings: u32,
    pub raid10_volume_settings: u32,
    pub resync_rate: u8,
    pub data_scrub_duration: u16,
    pub max_phys_disks_per_vol: u8,
    pub max_phys_disks: u8,
    pub max_volumes: u8,
}

impl Manufacturing4 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            flags: p.u32_at(0x08),
            raid0_volume_settings: p.u32_at(0x48),
            raid1e_volume_settings: p.u32_at(0x4C),
            raid1_volume_settings: p.u32_at(0x50),
            raid10_volume_settings: p.u32_at(0x54),
            resync_rate: p.u8_at(0x65),
            data_scrub_duration: p.u16_at(0x66),
            max_phys_disks_per_vol: p.u8_at(0x69),
            max_phys_disks: p.u8_at(0x6A),
            max_volumes: p.u8_at(0x6B),
        }
    }
}

pub const RAIDCONFIG_ELEMENT_TYPE_MASK: u16 = 0x000F;
pub const RAIDCONFIG_ELEMENT_VOLUME: u16 = 0x0000;
pub const RAIDCONFIG_ELEMENT_HOT_SPARE: u16 = 0x0002;

#[derive(Clone, Debug, Default, Serialize)]
pub struct RaidConfigElement {
    pub element_flags: u16,
    pub vol_dev_handle: u16,
    pub hot_spare_pool: u8,
    pub phys_disk_num: u8,
    pub phys_disk_dev_handle: u16,
}

impl RaidConfigElement {
    pub fn element_type(&self) -> u16 {
        self.element_flags & RAIDCONFIG_ELEMENT_TYPE_MASK
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RaidConfig0 {
    pub num_hot_spares: u8,
    pub num_phys_disks: u8,
    pub num_volumes: u8,
    pub config_num: u8,
    pub elements: Vec<RaidConfigElement>,
}

impl RaidConfig0 {
    pub fn parse(p: &[u8]) -> Self {
        let count = bounded_count(p, 0x30, 8, p.u8_at(0x2C) as usize);
        Self {
            num_hot_spares: p.u8_at(0x08),
            num_phys_disks: p.u8_at(0x09),
            num_volumes: p.u8_at(0x0A),
            config_num: p.u8_at(0x0B),
            elements: (0..count)
                .map(|i| {
                    let o = 0x30 + i * 8;
                    RaidConfigElement {
                        element_flags: p.u16_at(o),
                        vol_dev_handle: p.u16_at(o + 2),
                        hot_spare_pool: p.u8_at(o + 4),
                        phys_disk_num: p.u8_at(o + 5),
                        phys_disk_dev_handle: p.u16_at(o + 6),
                    }
                })
                .collect(),
        }
    }

    pub fn elements_of(&self, element_type: u16) -> impl Iterator<Item = &RaidConfigElement> {
        self.elements
            .iter()
            .filter(move |e| e.element_type() == element_type)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Bios3 {
    pub global_flags: u32,
    pub bios_version: u32,
}

impl Bios3 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            global_flags: p.u32_at(0x04),
            bios_version: p.u32_at(0x08),
        }
    }
}

pub const BOOT_FORM_NONE: u8 = 0x00;
pub const BOOT_FORM_SAS_WWID: u8 = 0x05;
pub const BOOT_FORM_ENCLOSURE_SLOT: u8 = 0x06;
pub const BOOT_FORM_DEVICE_NAME: u8 = 0x07;

pub const BIOS2_REQ_BOOT_FORM: usize = 0x1C;
pub const BIOS2_REQ_BOOT_DEVICE: usize = 0x20;
pub const BIOS2_REQ_ALT_BOOT_FORM: usize = 0x38;
pub const BIOS2_REQ_ALT_BOOT_DEVICE: usize = 0x3C;
pub const BIOS2_CURRENT_BOOT_FORM: usize = 0x54;
pub const BIOS2_CURRENT_BOOT_DEVICE: usize = 0x58;
pub const BOOT_DEVICE_LEN: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum BootDevice {
    None,
    SasWwid {
        sas_address: u64,
        lun: u64,
    },
    EnclosureSlot {
        enclosure_logical_id: u64,
        slot: u16,
    },
    DeviceName {
        device_name: u64,
        lun: u64,
    },
    Other {
        code: u8,
    },
}

impl BootDevice {
    pub fn parse(form: u8, d: &[u8]) -> Self {
        match form & 0x0F {
            BOOT_FORM_NONE => BootDevice::None,
            BOOT_FORM_SAS_WWID => BootDevice::SasWwid {
                sas_address: d.u64_at(0x00),
                lun: u64::from_be_bytes(lun_bytes(d)),
            },
            BOOT_FORM_ENCLOSURE_SLOT => BootDevice::EnclosureSlot {
                enclosure_logical_id: d.u64_at(0x00),
                slot: d.u16_at(0x10),
            },
            BOOT_FORM_DEVICE_NAME => BootDevice::DeviceName {
                device_name: d.u64_at(0x00),
                lun: u64::from_be_bytes(lun_bytes(d)),
            },
            other => BootDevice::Other { code: other },
        }
    }

    pub fn identifier(&self) -> Option<u64> {
        match self {
            BootDevice::SasWwid { sas_address, .. } => Some(*sas_address),
            BootDevice::DeviceName { device_name, .. } => Some(*device_name),
            _ => None,
        }
    }
}

fn lun_bytes(d: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (i, b) in out.iter_mut().enumerate() {
        *b = d.u8_at(0x08 + i);
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Bios2 {
    pub requested: BootDevice,
    pub requested_alternate: BootDevice,
    pub current: BootDevice,
}

impl Bios2 {
    pub fn parse(p: &[u8]) -> Self {
        let device = |form_off: usize, dev_off: usize| {
            let dev = p.get(dev_off..).unwrap_or(&[]);
            BootDevice::parse(p.u8_at(form_off), dev)
        };
        Self {
            requested: device(BIOS2_REQ_BOOT_FORM, BIOS2_REQ_BOOT_DEVICE),
            requested_alternate: device(BIOS2_REQ_ALT_BOOT_FORM, BIOS2_REQ_ALT_BOOT_DEVICE),
            current: device(BIOS2_CURRENT_BOOT_FORM, BIOS2_CURRENT_BOOT_DEVICE),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SasIoUnitPhy {
    pub phy: u8,
    pub port: u8,
    pub port_flags: u8,
    pub phy_flags: u8,
    pub negotiated_link_rate: u8,
    pub controller_phy_device_info: u32,
    pub attached_dev_handle: u16,
    pub controller_dev_handle: u16,
    pub discovery_status: u32,
}

impl SasIoUnitPhy {
    pub fn disabled(&self) -> bool {
        self.phy_flags & 0x08 != 0
    }
}

pub fn parse_sas_io_unit_0(p: &[u8]) -> Vec<SasIoUnitPhy> {
    let count = bounded_count(p, 0x10, 20, p.u8_at(0x0C) as usize);
    (0..count)
        .map(|i| {
            let o = 0x10 + i * 20;
            SasIoUnitPhy {
                phy: i as u8,
                port: p.u8_at(o),
                port_flags: p.u8_at(o + 1),
                phy_flags: p.u8_at(o + 2),
                negotiated_link_rate: p.u8_at(o + 3),
                controller_phy_device_info: p.u32_at(o + 4),
                attached_dev_handle: p.u16_at(o + 8),
                controller_dev_handle: p.u16_at(o + 0x0A),
                discovery_status: p.u32_at(o + 0x0C),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SasDevice0 {
    pub slot: u16,
    pub enclosure_handle: u16,
    pub sas_address: u64,
    pub parent_dev_handle: u16,
    pub phy_num: u8,
    pub access_status: u8,
    pub dev_handle: u16,
    pub attached_phy_identifier: u8,
    pub device_info: u32,
    pub flags: u16,
    pub physical_port: u8,
    pub device_name: u64,
    pub enclosure_level: u8,
    pub connector_name: String,
}

impl SasDevice0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            slot: p.u16_at(0x08),
            enclosure_handle: p.u16_at(0x0A),
            sas_address: p.u64_at(0x0C),
            parent_dev_handle: p.u16_at(0x14),
            phy_num: p.u8_at(0x16),
            access_status: p.u8_at(0x17),
            dev_handle: p.u16_at(0x18),
            attached_phy_identifier: p.u8_at(0x1A),
            device_info: p.u32_at(0x1C),
            flags: p.u16_at(0x20),
            physical_port: p.u8_at(0x22),
            device_name: p.u64_at(0x24),
            enclosure_level: p.u8_at(0x2F),
            connector_name: p.ascii_at(0x30, 4),
        }
    }

    pub fn is_end_device(&self) -> bool {
        self.device_info & DEVICE_INFO_TYPE_MASK == DEVICE_TYPE_END_DEVICE
    }

    pub fn is_sep(&self) -> bool {
        self.device_info & DEVICE_INFO_SEP != 0
    }

    pub fn is_sata(&self) -> bool {
        self.device_info & DEVICE_INFO_SATA_DEVICE != 0
    }

    pub fn is_ssp_target(&self) -> bool {
        self.device_info & DEVICE_INFO_SSP_TARGET != 0
    }

    pub fn is_disk(&self) -> bool {
        !self.is_sep() && (self.is_ssp_target() || self.is_sata())
    }

    pub fn is_target(&self) -> bool {
        self.device_info
            & (DEVICE_INFO_SSP_TARGET
                | DEVICE_INFO_STP_TARGET
                | DEVICE_INFO_SMP_TARGET
                | DEVICE_INFO_SATA_DEVICE)
            != 0
    }

    pub fn is_listed_device(&self) -> bool {
        self.parent_dev_handle != 0 && self.is_end_device() && self.is_target()
    }

    pub fn protocol(&self) -> &'static str {
        if self.is_sata() {
            "SATA"
        } else if self.is_ssp_target() {
            "SAS"
        } else {
            "other"
        }
    }

    pub fn device_type_name(&self) -> &'static str {
        match self.device_info & DEVICE_INFO_TYPE_MASK {
            0 => "none",
            1 => "end device",
            2 => "edge expander",
            3 => "fanout expander",
            _ => "reserved",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Enclosure0 {
    pub enclosure_logical_id: u64,
    pub flags: u16,
    pub enclosure_handle: u16,
    pub num_slots: u16,
    pub start_slot: u16,
    pub chassis_slot: u8,
    pub enclosure_level: u8,
    pub sep_dev_handle: u16,
}

impl Enclosure0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            enclosure_logical_id: p.u64_at(0x0C),
            flags: p.u16_at(0x14),
            enclosure_handle: p.u16_at(0x16),
            num_slots: p.u16_at(0x18),
            start_slot: p.u16_at(0x1A),
            chassis_slot: p.u8_at(0x1C),
            enclosure_level: p.u8_at(0x1D),
            sep_dev_handle: p.u16_at(0x1E),
        }
    }

    pub fn management(&self) -> &'static str {
        match self.flags & 0x000F {
            0x0 => "unknown",
            0x1 => "IOC SES",
            0x2 => "IOC SGPIO",
            0x3 => "expander SGPIO",
            0x4 => "SES enclosure",
            0x5 => "IOC GPIO",
            _ => "reserved",
        }
    }

    pub fn chassis_slot_valid(&self) -> bool {
        self.flags & 0x0020 != 0
    }

    pub fn enclosure_level_valid(&self) -> bool {
        self.flags & 0x0010 != 0
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SasPhy0 {
    pub owner_dev_handle: u16,
    pub attached_dev_handle: u16,
    pub attached_phy_identifier: u8,
    pub attached_phy_info: u32,
    pub programmed_link_rate: u8,
    pub hw_link_rate: u8,
    pub change_count: u8,
    pub flags: u8,
    pub phy_info: u32,
    pub negotiated_link_rate: u8,
}

impl SasPhy0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            owner_dev_handle: p.u16_at(0x08),
            attached_dev_handle: p.u16_at(0x0C),
            attached_phy_identifier: p.u8_at(0x0E),
            attached_phy_info: p.u32_at(0x10),
            programmed_link_rate: p.u8_at(0x14),
            hw_link_rate: p.u8_at(0x15),
            change_count: p.u8_at(0x16),
            flags: p.u8_at(0x17),
            phy_info: p.u32_at(0x18),
            negotiated_link_rate: p.u8_at(0x1C),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SasPhy1 {
    pub invalid_dword_count: u32,
    pub running_disparity_error_count: u32,
    pub loss_dword_synch_count: u32,
    pub phy_reset_problem_count: u32,
}

impl SasPhy1 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            invalid_dword_count: p.u32_at(0x0C),
            running_disparity_error_count: p.u32_at(0x10),
            loss_dword_synch_count: p.u32_at(0x14),
            phy_reset_problem_count: p.u32_at(0x18),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VolumeMember {
    pub raid_set_num: u8,
    pub phys_disk_map: u8,
    pub phys_disk_num: u8,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RaidVolume0 {
    pub dev_handle: u16,
    pub volume_state: u8,
    pub volume_type: u8,
    pub volume_status_flags: u32,
    pub settings: u16,
    pub hot_spare_pool: u8,
    pub max_lba: u64,
    pub stripe_size: u32,
    pub block_size: u16,
    pub supported_phys_disks: u8,
    pub resync_rate: u8,
    pub data_scrub_duration: u16,
    pub inactive_status: u8,
    pub members: Vec<VolumeMember>,
}

impl RaidVolume0 {
    pub fn parse(p: &[u8]) -> Self {
        let count = bounded_count(p, 0x28, 4, p.u8_at(0x24) as usize);
        Self {
            dev_handle: p.u16_at(0x04),
            volume_state: p.u8_at(0x06),
            volume_type: p.u8_at(0x07),
            volume_status_flags: p.u32_at(0x08),
            settings: p.u16_at(0x0C),
            hot_spare_pool: p.u8_at(0x0E),
            max_lba: p.u64_at(0x10),
            stripe_size: p.u32_at(0x18),
            block_size: p.u16_at(0x1C),
            supported_phys_disks: p.u8_at(0x20),
            resync_rate: p.u8_at(0x21),
            data_scrub_duration: p.u16_at(0x22),
            inactive_status: p.u8_at(0x27),
            members: (0..count)
                .map(|i| {
                    let o = 0x28 + i * 4;
                    VolumeMember {
                        raid_set_num: p.u8_at(o),
                        phys_disk_map: p.u8_at(o + 1),
                        phys_disk_num: p.u8_at(o + 2),
                    }
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RaidVolume1 {
    pub dev_handle: u16,
    pub guid: String,
    pub name: String,
    pub wwid: u64,
}

impl RaidVolume1 {
    pub fn parse(p: &[u8]) -> Self {
        let guid: Vec<u8> = (0..24).map(|i| p.u8_at(0x08 + i)).collect();
        Self {
            dev_handle: p.u16_at(0x04),
            guid: super::mpi::hex(&guid),
            name: p.ascii_at(0x20, 16),
            wwid: p.u64_at(0x30),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RaidPhysDisk0 {
    pub dev_handle: u16,
    pub phys_disk_num: u8,
    pub hot_spare_pool: u8,
    pub vendor_id: String,
    pub product_id: String,
    pub product_revision: String,
    pub serial_number: String,
    pub phys_disk_state: u8,
    pub offline_reason: u8,
    pub incompatible_reason: u8,
    pub phys_disk_attributes: u8,
    pub phys_disk_status_flags: u32,
    pub device_max_lba: u64,
    pub host_max_lba: u64,
    pub coerced_max_lba: u64,
    pub block_size: u16,
}

impl RaidPhysDisk0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            dev_handle: p.u16_at(0x04),
            phys_disk_num: p.u8_at(0x07),
            hot_spare_pool: p.u8_at(0x0A),
            vendor_id: p.ascii_at(0x10, 8),
            product_id: p.ascii_at(0x18, 16),
            product_revision: p.ascii_at(0x28, 4),
            serial_number: p.ascii_at(0x2C, 32),
            phys_disk_state: p.u8_at(0x50),
            offline_reason: p.u8_at(0x51),
            incompatible_reason: p.u8_at(0x52),
            phys_disk_attributes: p.u8_at(0x53),
            phys_disk_status_flags: p.u32_at(0x54),
            device_max_lba: p.u64_at(0x58),
            host_max_lba: p.u64_at(0x60),
            coerced_max_lba: p.u64_at(0x68),
            block_size: p.u16_at(0x70),
        }
    }

    pub fn is_ssd(&self) -> bool {
        self.phys_disk_attributes & 0x0C == 0x08
    }
}

pub const LOG_DATA_LENGTH: usize = 0x30 - 0x14;

#[derive(Clone, Debug, Default, Serialize)]
pub struct LogEntry {
    pub time_stamp: u64,
    pub log_sequence: u16,
    pub log_entry_qualifier: u16,
    pub log_data: String,
}

pub fn parse_log_0(p: &[u8]) -> Vec<LogEntry> {
    let count = bounded_count(p, 0x14, 0x30, p.u16_at(0x10) as usize);
    (0..count)
        .map(|i| {
            let o = 0x14 + i * 0x30;
            let data: Vec<u8> = (0..LOG_DATA_LENGTH)
                .map(|j| p.u8_at(o + 0x14 + j))
                .collect();
            LogEntry {
                time_stamp: p.u64_at(o),
                log_sequence: p.u16_at(o + 0x0C),
                log_entry_qualifier: p.u16_at(o + 0x0E),
                log_data: super::mpi::hex(&data),
            }
        })
        .filter(|e| e.log_entry_qualifier != 0)
        .collect()
}
