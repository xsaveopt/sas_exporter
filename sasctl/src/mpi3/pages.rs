use serde::Serialize;

use crate::bytes::Le;

pub const SAS_DEVICE_INFO_SSP_TARGET: u16 = 0x0100;
pub const SAS_DEVICE_INFO_STP_SATA_TARGET: u16 = 0x0080;
pub const SAS_DEVICE_INFO_TYPE_MASK: u16 = 0x0007;
pub const SAS_DEVICE_TYPE_END_DEVICE: u16 = 0x0001;
pub const SAS_DEVICE_TYPE_EXPANDER: u16 = 0x0002;
pub const PCIE_DEVICE_INFO_TYPE_MASK: u16 = 0x0007;
pub const PCIE_DEVICE_TYPE_NVME: u16 = 0x0001;

pub const DEVICE_FLAGS_HIDDEN: u16 = 0x0008;
pub const DEVICE_FORM_SAS_SATA: u8 = 0x00;
pub const DEVICE_FORM_PCIE: u8 = 0x01;
pub const DEVICE_FORM_VD: u8 = 0x02;
const DEVICE_SPECIFIC: usize = 0x28;
const SAS_NEGOTIATED_LINK_RATE: usize = DEVICE_SPECIFIC + 0x13;

pub const ENCLOSURE_FLAGS_DEV_PRESENT: u16 = 0x0010;
pub const ENCLOSURE_FLAGS_CHASSIS_SLOT_VALID: u16 = 0x0020;

pub const IOUNIT4_TEMP_VALID: u8 = 0x01;
pub const IOUNIT4_ISTWI_INTERNAL: u16 = 0xFFFF;
pub const IOUNIT19_TEMP_UNAVAILABLE: u16 = 0x8000;

fn bounded_count(page: &[u8], start: usize, stride: usize, declared: usize) -> usize {
    declared.min(page.len().saturating_sub(start) / stride)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Manufacturing0 {
    pub chip_revision: String,
    pub chip_name: String,
    pub board_name: String,
    pub board_assembly: String,
    pub board_tracer_number: String,
    pub board_mfg_date: Option<String>,
    pub board_revision: String,
    pub product_name: String,
}

impl Manufacturing0 {
    pub fn parse(p: &[u8]) -> Self {
        let (day, month, year) = (p.u8_at(0xA0), p.u8_at(0xA1), p.u16_at(0xA2));
        Self {
            chip_revision: p.ascii_at(0x08, 8),
            chip_name: p.ascii_at(0x10, 32),
            board_name: p.ascii_at(0x30, 32),
            board_assembly: p.ascii_at(0x50, 32),
            board_tracer_number: p.ascii_at(0x70, 32),
            board_mfg_date: (year != 0).then(|| format!("{year:04}-{month:02}-{day:02}")),
            board_revision: p.ascii_at(0xA8, 8),
            product_name: p.ascii_at(0xC0, 256),
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
            unique_value: p.u64_at(0x08),
            nvdata_version_default: p.u32_at(0x10),
            nvdata_version_persistent: p.u32_at(0x14),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Sensor {
    pub index: u8,
    pub raw: u16,
    pub valid: bool,
    pub celsius: Option<i16>,
    pub location: &'static str,
    pub internal: bool,
    pub istwi_index: u16,
    pub channel: u8,
}

pub fn sensor_location(code: u8) -> &'static str {
    match code {
        0 => "internal",
        1 => "inlet",
        2 => "outlet",
        3 => "DRAM",
        _ => "reserved",
    }
}

pub fn parse_io_unit_4(p: &[u8]) -> Vec<Sensor> {
    let count = bounded_count(p, 0x10, 16, p.u8_at(0x0C) as usize);
    (0..count)
        .map(|i| {
            let o = 0x10 + i * 16;
            let flags = p.u8_at(o + 4);
            let istwi = p.u16_at(o + 8);
            let raw = p.u16_at(o);
            let valid = flags & IOUNIT4_TEMP_VALID != 0;
            Sensor {
                index: i as u8,
                raw,
                valid,
                celsius: valid.then_some(raw as i16),
                location: sensor_location((flags >> 5) & 0x07),
                internal: istwi == IOUNIT4_ISTWI_INTERNAL,
                istwi_index: istwi,
                channel: p.u8_at(o + 0x0A),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DeviceTemperature {
    pub raw: u16,
    pub dev_handle: u16,
    pub persistent_id: u16,
}

pub fn parse_io_unit_19(p: &[u8]) -> Vec<DeviceTemperature> {
    let count = bounded_count(p, 0x10, 8, p.u16_at(0x08) as usize);
    (0..count)
        .map(|i| {
            let o = 0x10 + i * 8;
            DeviceTemperature {
                raw: p.u16_at(o),
                dev_handle: p.u16_at(o + 2),
                persistent_id: p.u16_at(o + 4),
            }
        })
        .filter(|d| d.raw != IOUNIT19_TEMP_UNAVAILABLE)
        .collect()
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
pub struct SasIoUnitPhy {
    pub phy: u8,
    pub io_unit_port: u8,
    pub port_flags: u8,
    pub phy_flags: u8,
    pub negotiated_link_rate: u8,
    pub controller_phy_device_info: u16,
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
                io_unit_port: p.u8_at(o),
                port_flags: p.u8_at(o + 1),
                phy_flags: p.u8_at(o + 2),
                negotiated_link_rate: p.u8_at(o + 3),
                controller_phy_device_info: p.u16_at(o + 4),
                attached_dev_handle: p.u16_at(o + 8),
                controller_dev_handle: p.u16_at(o + 0x0A),
                discovery_status: p.u32_at(o + 0x0C),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SasPhy0 {
    pub owner_dev_handle: u16,
    pub attached_dev_handle: u16,
    pub attached_phy_identifier: u8,
    pub programmed_link_rate: u8,
    pub hw_link_rate: u8,
    pub change_count: u8,
    pub phy_info: u32,
    pub negotiated_link_rate: u8,
}

impl SasPhy0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            owner_dev_handle: p.u16_at(0x08),
            attached_dev_handle: p.u16_at(0x0C),
            attached_phy_identifier: p.u8_at(0x0E),
            programmed_link_rate: p.u8_at(0x14),
            hw_link_rate: p.u8_at(0x15),
            change_count: p.u8_at(0x16),
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
pub struct Enclosure0 {
    pub enclosure_logical_id: u64,
    pub flags: u16,
    pub enclosure_handle: u16,
    pub num_slots: u16,
    pub io_unit_port: u8,
    pub enclosure_level: u8,
    pub sep_dev_handle: u16,
    pub chassis_slot: u8,
}

impl Enclosure0 {
    pub fn parse(p: &[u8]) -> Self {
        Self {
            enclosure_logical_id: p.u64_at(0x08),
            flags: p.u16_at(0x10),
            enclosure_handle: p.u16_at(0x12),
            num_slots: p.u16_at(0x14),
            io_unit_port: p.u8_at(0x18),
            enclosure_level: p.u8_at(0x19),
            sep_dev_handle: p.u16_at(0x1A),
            chassis_slot: p.u8_at(0x1C),
        }
    }

    pub fn sep(&self) -> Option<u16> {
        (self.flags & ENCLOSURE_FLAGS_DEV_PRESENT != 0).then_some(self.sep_dev_handle)
    }

    pub fn chassis_slot_valid(&self) -> bool {
        self.flags & ENCLOSURE_FLAGS_CHASSIS_SLOT_VALID != 0
    }

    pub fn enclosure_type(&self) -> &'static str {
        match (self.flags >> 14) & 0x03 {
            0 => "virtual",
            1 => "SAS",
            2 => "PCIe",
            _ => "reserved",
        }
    }

    pub fn management(&self) -> &'static str {
        match self.flags & 0x000F {
            0 => "unknown",
            1 => "IOC SES",
            2 => "SES enclosure",
            _ => "reserved",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SasForm {
    pub sas_address: u64,
    pub flags: u16,
    pub device_info: u16,
    pub phy_num: u8,
    pub attached_phy_identifier: u8,
    pub negotiated_link_rate: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PcieForm {
    pub supported_link_rates: u8,
    pub max_port_width: u8,
    pub negotiated_port_width: u8,
    pub negotiated_link_rate: u8,
    pub port_num: u8,
    pub device_info: u16,
    pub maximum_data_transfer_size: u32,
    pub capabilities: u32,
    pub page_size: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VdForm {
    pub vd_state: u8,
    pub raid_level: u8,
    pub device_info: u16,
    pub flags: u16,
    pub io_throttle_group: u16,
    pub io_throttle_group_low: u16,
    pub io_throttle_group_high: u16,
    pub vd_abort_to: u8,
    pub vd_reset_to: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum DeviceForm {
    SasSata(SasForm),
    Pcie(PcieForm),
    Vd(VdForm),
    Other { code: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Device0 {
    pub dev_handle: u16,
    pub parent_dev_handle: u16,
    pub slot: u16,
    pub enclosure_handle: u16,
    pub wwid: u64,
    pub persistent_id: u16,
    pub io_unit_port: u8,
    pub access_status: u8,
    pub flags: u16,
    pub slot_index: u16,
    pub queue_depth: u16,
    pub form: DeviceForm,
}

impl Device0 {
    pub fn parse(p: &[u8]) -> Self {
        let s = DEVICE_SPECIFIC;
        let form = match p.u8_at(0x27) {
            DEVICE_FORM_SAS_SATA => DeviceForm::SasSata(SasForm {
                sas_address: p.u64_at(s),
                flags: p.u16_at(s + 0x08),
                device_info: p.u16_at(s + 0x0A),
                phy_num: p.u8_at(s + 0x0C),
                attached_phy_identifier: p.u8_at(s + 0x0D),
                negotiated_link_rate: (p.len() > SAS_NEGOTIATED_LINK_RATE)
                    .then(|| p.u8_at(SAS_NEGOTIATED_LINK_RATE)),
            }),
            DEVICE_FORM_PCIE => DeviceForm::Pcie(PcieForm {
                supported_link_rates: p.u8_at(s),
                max_port_width: p.u8_at(s + 1),
                negotiated_port_width: p.u8_at(s + 2),
                negotiated_link_rate: p.u8_at(s + 3),
                port_num: p.u8_at(s + 4),
                device_info: p.u16_at(s + 6),
                maximum_data_transfer_size: p.u32_at(s + 8),
                capabilities: p.u32_at(s + 0x0C),
                page_size: p.u8_at(s + 0x13),
            }),
            DEVICE_FORM_VD => DeviceForm::Vd(VdForm {
                vd_state: p.u8_at(s),
                raid_level: p.u8_at(s + 1),
                device_info: p.u16_at(s + 2),
                flags: p.u16_at(s + 4),
                io_throttle_group: p.u16_at(s + 6),
                io_throttle_group_low: p.u16_at(s + 8),
                io_throttle_group_high: p.u16_at(s + 0x0A),
                vd_abort_to: p.u8_at(s + 0x0C),
                vd_reset_to: p.u8_at(s + 0x0D),
            }),
            code => DeviceForm::Other { code },
        };
        Self {
            dev_handle: p.u16_at(0x08),
            parent_dev_handle: p.u16_at(0x0A),
            slot: p.u16_at(0x0C),
            enclosure_handle: p.u16_at(0x0E),
            wwid: p.u64_at(0x10),
            persistent_id: p.u16_at(0x18),
            io_unit_port: p.u8_at(0x1A),
            access_status: p.u8_at(0x1B),
            flags: p.u16_at(0x1C),
            slot_index: p.u16_at(0x20),
            queue_depth: p.u16_at(0x22),
            form,
        }
    }

    pub fn hidden(&self) -> bool {
        self.flags & DEVICE_FLAGS_HIDDEN != 0
    }

    pub fn sas(&self) -> Option<&SasForm> {
        match &self.form {
            DeviceForm::SasSata(s) => Some(s),
            _ => None,
        }
    }

    pub fn vd(&self) -> Option<&VdForm> {
        match &self.form {
            DeviceForm::Vd(v) => Some(v),
            _ => None,
        }
    }

    pub fn protocol(&self) -> Option<Protocol> {
        match &self.form {
            DeviceForm::SasSata(s) => {
                if s.device_info & SAS_DEVICE_INFO_TYPE_MASK != SAS_DEVICE_TYPE_END_DEVICE {
                    None
                } else if s.device_info & SAS_DEVICE_INFO_SSP_TARGET != 0 {
                    Some(Protocol::Sas)
                } else if s.device_info & SAS_DEVICE_INFO_STP_SATA_TARGET != 0 {
                    Some(Protocol::Sata)
                } else {
                    None
                }
            }
            DeviceForm::Pcie(p) => (p.device_info & PCIE_DEVICE_INFO_TYPE_MASK
                == PCIE_DEVICE_TYPE_NVME)
                .then_some(Protocol::Nvme),
            _ => None,
        }
    }

    pub fn is_expander(&self) -> bool {
        self.sas()
            .is_some_and(|s| s.device_info & SAS_DEVICE_INFO_TYPE_MASK == SAS_DEVICE_TYPE_EXPANDER)
    }

    pub fn link_rate(&self) -> Option<&'static str> {
        match &self.form {
            DeviceForm::SasSata(s) => s
                .negotiated_link_rate
                .map(|r| super::mpi::sas_link_rate_name(r & 0x0F)),
            DeviceForm::Pcie(p) => Some(super::mpi::pcie_link_rate_name(p.negotiated_link_rate)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Protocol {
    #[serde(rename = "SAS")]
    Sas,
    #[serde(rename = "SATA")]
    Sata,
    #[serde(rename = "NVMe")]
    Nvme,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Sas => "SAS",
            Protocol::Sata => "SATA",
            Protocol::Nvme => "NVMe",
        }
    }
}

pub fn access_status_name(code: u8) -> &'static str {
    match code {
        0x00 => "healthy",
        0x01 => "needs initialization",
        0x02 => "capability unsupported",
        0x03 => "device blocked",
        0x04 => "unauthorized",
        0x05 => "device missing delay",
        0x06 => "prepare",
        0x07 => "safe mode",
        0x10 => "SAS unknown",
        0x11 => "route not addressable",
        0x12 => "SMP error not addressable",
        0x20 => "SATA init unknown",
        0x21 => "SATA affiliation conflict",
        0x22 => "SATA diagnostic",
        0x23 => "SATA identification",
        0x24 => "SATA check power",
        0x25 => "SATA PIO SN",
        0x26 => "SATA MDMA SN",
        0x27 => "SATA UDMA SN",
        0x28 => "SATA zoning violation",
        0x29 => "SATA not addressable",
        0x30 => "PCIe unknown",
        0x31 => "PCIe memory space access",
        0x32 => "PCIe unsupported",
        0x33 => "PCIe MSI-X required",
        0x34 => "PCIe ECRC required",
        0x40 => "NVMe unknown",
        0x41 => "NVMe ready timeout",
        0x42 => "NVMe device config unsupported",
        0x43 => "NVMe identify failed",
        0x44 => "NVMe queue config failed",
        0x45 => "NVMe queue creation failed",
        0x46 => "NVMe event config failed",
        0x47 => "NVMe get feature status failed",
        0x48 => "NVMe idle timeout",
        0x49 => "NVMe controller failure status",
        0x4A => "NVMe insufficient power",
        0x4B => "NVMe doorbell stride",
        0x4C => "NVMe memory page min size",
        0x4D => "NVMe memory allocation",
        0x4E => "NVMe completion time",
        0x4F => "NVMe BAR",
        0x50 => "NVMe namespace descriptor",
        0x51 => "NVMe incompatible settings",
        0x52 => "NVMe too many errors",
        0x80..=0x8F => "VD unknown",
        _ => "unknown",
    }
}

pub fn vd_state_name(state: u8) -> &'static str {
    match state {
        0 => "Offline",
        1 => "Partially degraded",
        2 => "Degraded",
        3 => "Optimal",
        _ => "unknown",
    }
}

pub fn vd_media(device_info: u16) -> Vec<&'static str> {
    [
        (0x0010, "HDD"),
        (0x0008, "SSD"),
        (0x0004, "NVMe"),
        (0x0002, "SATA"),
        (0x0001, "SAS"),
    ]
    .into_iter()
    .filter(|(bit, _)| device_info & bit != 0)
    .map(|(_, n)| n)
    .collect()
}

pub fn vd_os_exposure(flags: u16) -> &'static str {
    match flags & 0x0003 {
        0 => "HDD",
        1 => "SSD",
        2 => "no guidance",
        _ => "reserved",
    }
}
