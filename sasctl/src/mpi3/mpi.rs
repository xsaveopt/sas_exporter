use anyhow::{Result, bail};
use serde::Serialize;

use super::transport::{Reply, Request, Transport};
use crate::bytes::{Le, LeMut};

pub const FUNCTION_IOC_FACTS: u8 = 0x01;
pub const FUNCTION_CI_UPLOAD: u8 = 0x07;
pub const FUNCTION_IO_UNIT_CONTROL: u8 = 0x08;
pub const FUNCTION_PERSISTENT_EVENT_LOG: u8 = 0x09;
pub const FUNCTION_CONFIG: u8 = 0x10;
pub const FUNCTION_SCSI_IO: u8 = 0x20;
pub const FUNCTION_NVME_ENCAPSULATED: u8 = 0x24;

pub const IOCSTATUS_SUCCESS: u16 = 0x0000;
pub const IOCSTATUS_CONFIG_INVALID_PAGE: u16 = 0x0022;
pub const IOCSTATUS_SCSI_RECOVERED_ERROR: u16 = 0x0040;
pub const IOCSTATUS_SCSI_DATA_UNDERRUN: u16 = 0x0045;

pub const IOC_FACTS_REQUEST_LEN: usize = 0x10;
pub const IOC_FACTS_DATA_LEN: usize = 0x70;

pub const CAPABILITY_RAID_SUPPORTED: u32 = 0x0000_0008;
pub const CAPABILITY_MULTIPATH_SUPPORTED: u32 = 0x0000_0002;

const CAPABILITY_NAMES: &[(u32, &str)] = &[
    (0x8000_0000, "non-supervisor"),
    (0x0000_0100, "complete-reset"),
    (0x0000_0080, "segmented-diag-trace"),
    (0x0000_0040, "segmented-diag-fw"),
    (0x0000_0020, "segmented-diag-driver"),
    (0x0000_0010, "advanced-host-pd"),
    (0x0000_0008, "raid"),
    (0x0000_0002, "multipath"),
    (0x0000_0001, "coalesce-control"),
];

const PROTOCOL_NAMES: &[(u8, &str)] = &[
    (0x10, "SAS"),
    (0x08, "SATA"),
    (0x04, "NVMe"),
    (0x02, "SCSI initiator"),
    (0x01, "SCSI target"),
];

pub fn ioc_status_name(status: u16) -> &'static str {
    match status {
        0x0000 => "SUCCESS",
        0x0001 => "INVALID_FUNCTION",
        0x0002 => "BUSY",
        0x0003 => "INVALID_SGL",
        0x0004 => "INTERNAL_ERROR",
        0x0006 => "INSUFFICIENT_RESOURCES",
        0x0007 => "INVALID_FIELD",
        0x0008 => "INVALID_STATE",
        0x0009 => "SHUTDOWN_ACTIVE",
        0x000A => "INSUFFICIENT_POWER",
        0x000B => "INVALID_CHANGE_COUNT",
        0x000C => "ALLOWED_CMD_BLOCK",
        0x000D => "SUPERVISOR_ONLY",
        0x001F => "FAILURE",
        0x0020 => "CONFIG_INVALID_ACTION",
        0x0021 => "CONFIG_INVALID_TYPE",
        0x0022 => "CONFIG_INVALID_PAGE",
        0x0023 => "CONFIG_INVALID_DATA",
        0x0024 => "CONFIG_NO_DEFAULTS",
        0x0025 => "CONFIG_CANT_COMMIT",
        0x0040 => "SCSI_RECOVERED_ERROR",
        0x0041 => "SCSI_TM_NOT_SUPPORTED",
        0x0042 => "SCSI_INVALID_DEVHANDLE",
        0x0043 => "SCSI_DEVICE_NOT_THERE",
        0x0044 => "SCSI_DATA_OVERRUN",
        0x0045 => "SCSI_DATA_UNDERRUN",
        0x0046 => "SCSI_IO_DATA_ERROR",
        0x0047 => "SCSI_PROTOCOL_ERROR",
        0x0048 => "SCSI_TASK_TERMINATED",
        0x0049 => "SCSI_RESIDUAL_MISMATCH",
        0x004A => "SCSI_TASK_MGMT_FAILED",
        0x004B => "SCSI_IOC_TERMINATED",
        0x004C => "SCSI_EXT_TERMINATED",
        0x004D => "EEDP_GUARD_ERROR",
        0x004E => "EEDP_REF_TAG_ERROR",
        0x004F => "EEDP_APP_TAG_ERROR",
        0x0090 => "SAS_SMP_REQUEST_FAILED",
        0x0091 => "SAS_SMP_DATA_OVERRUN",
        0x00A0 => "DIAGNOSTIC_RELEASED",
        0x00B0 => "CI_UNSUPPORTED",
        0x00B1 => "CI_UPDATE_SEQUENCE",
        0x00B2 => "CI_VALIDATION_FAILED",
        0x00B3 => "CI_KEY_UPDATE_PENDING",
        0x00B4 => "CI_KEY_UPDATE_NOT_POSSIBLE",
        0x00C0 => "SECURITY_KEY_REQUIRED",
        0x00C1 => "SECURITY_VIOLATION",
        _ => "UNKNOWN",
    }
}

pub fn ensure_success(reply: &Reply, what: &str) -> Result<()> {
    let status = reply.ioc_status();
    if status != IOCSTATUS_SUCCESS {
        bail!(
            "{what} failed with IOCStatus 0x{status:04x} ({}), IOCLogInfo 0x{:08x}",
            ioc_status_name(status),
            reply.ioc_log_info()
        );
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImageVersion {
    pub gen_major: u8,
    pub gen_minor: u8,
    pub phase_major: u8,
    pub phase_minor: u8,
    pub customer_id: u16,
    pub build_num: u16,
}

impl ImageVersion {
    pub fn parse(b: &[u8], off: usize) -> Self {
        Self {
            build_num: b.u16_at(off),
            customer_id: b.u16_at(off + 2),
            phase_minor: b.u8_at(off + 4),
            phase_major: b.u8_at(off + 5),
            gen_minor: b.u8_at(off + 6),
            gen_major: b.u8_at(off + 7),
        }
    }
}

impl std::fmt::Display for ImageVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}.{:05}-{:05}",
            self.gen_major,
            self.gen_minor,
            self.phase_major,
            self.phase_minor,
            self.customer_id,
            self.build_num
        )
    }
}

pub fn ioc_facts_request() -> Request {
    let mut r = Request::new(FUNCTION_IOC_FACTS, IOC_FACTS_REQUEST_LEN);
    r.data_in_len = IOC_FACTS_DATA_LEN;
    r
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct IocFacts {
    pub data_length: u16,
    pub mpi_version: u32,
    pub fw_version: ImageVersion,
    pub ioc_capabilities: u32,
    pub ioc_number: u8,
    pub max_outstanding_requests: u16,
    pub product_id: u16,
    pub reply_frame_size: u16,
    pub ioc_exceptions: u16,
    pub max_persistent_id: u16,
    pub protocol_flags: u8,
    pub max_sas_initiators: u16,
    pub max_sas_expanders: u16,
    pub max_enclosures: u16,
    pub min_dev_handle: u16,
    pub max_dev_handle: u16,
    pub max_pcie_switches: u16,
    pub max_nvme: u16,
    pub max_vds: u16,
    pub max_host_pds: u16,
    pub max_adv_host_pds: u16,
    pub max_raid_pds: u16,
    pub flags: u32,
}

impl IocFacts {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            data_length: b.u16_at(0x00),
            mpi_version: b.u32_at(0x04),
            fw_version: ImageVersion::parse(b, 0x08),
            ioc_capabilities: b.u32_at(0x10),
            ioc_number: b.u8_at(0x14),
            max_outstanding_requests: b.u16_at(0x18),
            product_id: b.u16_at(0x1A),
            reply_frame_size: b.u16_at(0x1E),
            ioc_exceptions: b.u16_at(0x20),
            max_persistent_id: b.u16_at(0x22),
            protocol_flags: b.u8_at(0x27),
            max_sas_initiators: b.u16_at(0x28),
            max_sas_expanders: b.u16_at(0x2C),
            max_enclosures: b.u16_at(0x2E),
            min_dev_handle: b.u16_at(0x30),
            max_dev_handle: b.u16_at(0x32),
            max_pcie_switches: b.u16_at(0x34),
            max_nvme: b.u16_at(0x36),
            max_vds: b.u16_at(0x3A),
            max_host_pds: b.u16_at(0x3C),
            max_adv_host_pds: b.u16_at(0x3E),
            max_raid_pds: b.u16_at(0x40),
            flags: b.u32_at(0x44),
        }
    }

    pub fn personality(&self) -> &'static str {
        personality_name(self.flags)
    }

    pub fn raid_supported(&self) -> bool {
        self.ioc_capabilities & CAPABILITY_RAID_SUPPORTED != 0
    }

    pub fn capabilities(&self) -> Vec<&'static str> {
        CAPABILITY_NAMES
            .iter()
            .filter(|(bit, _)| self.ioc_capabilities & bit != 0)
            .map(|(_, n)| *n)
            .collect()
    }

    pub fn protocols(&self) -> Vec<&'static str> {
        PROTOCOL_NAMES
            .iter()
            .filter(|(bit, _)| self.protocol_flags & bit != 0)
            .map(|(_, n)| *n)
            .collect()
    }
}

pub fn personality_name(flags: u32) -> &'static str {
    match flags & 0x0F {
        0x0 => "eHBA",
        0x2 => "RAID",
        _ => "unknown",
    }
}

pub fn ioc_facts(t: &dyn Transport) -> Result<IocFacts> {
    let reply = ioc_facts_request().send_checked(t, "IOC Facts")?;
    Ok(IocFacts::parse(&reply.data_in))
}

pub const CTRL_OP_SAS_PHY_CONTROL: u8 = 0x21;
pub const CTRL_ACTION_LINK_RESET: u8 = 0x01;
pub const CTRL_ACTION_HARD_RESET: u8 = 0x02;
pub const IO_UNIT_CONTROL_LEN: usize = 0x40;

pub fn phy_reset_request(phy: u8, hard: bool) -> Request {
    let mut r = Request::new(FUNCTION_IO_UNIT_CONTROL, IO_UNIT_CONTROL_LEN);
    r.frame.put_u8(0x0B, CTRL_OP_SAS_PHY_CONTROL);
    r.frame.put_u8(
        0x38,
        if hard {
            CTRL_ACTION_HARD_RESET
        } else {
            CTRL_ACTION_LINK_RESET
        },
    );
    r.frame.put_u8(0x39, phy);
    r
}

pub const SIGNATURE1_MANIFEST: u32 = 0x464E_414D;
pub const IMAGE_HEADER_SIZE: u32 = 0x100;
pub const MANIFEST_LEN: usize = 0xB0;
pub const CI_REQUEST_LEN: usize = 0x20;
pub const MANIFEST_TYPE_MPI: u8 = 0;

pub fn manifest_request() -> Request {
    let mut r = Request::new(FUNCTION_CI_UPLOAD, CI_REQUEST_LEN);
    r.frame.put_u32(0x0C, SIGNATURE1_MANIFEST);
    r.frame.put_u32(0x14, IMAGE_HEADER_SIZE);
    r.frame.put_u32(0x18, MANIFEST_LEN as u32);
    r.data_in_len = MANIFEST_LEN;
    r
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Manifest {
    pub release_level: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub package_version: ImageVersion,
}

pub fn parse_manifest(d: &[u8]) -> Option<Manifest> {
    if d.len() < MANIFEST_LEN || d.u8_at(0x00) != MANIFEST_TYPE_MPI {
        return None;
    }
    Some(Manifest {
        release_level: d.u8_at(0x11),
        vendor_id: d.u16_at(0x20),
        device_id: d.u16_at(0x22),
        subsystem_vendor_id: d.u16_at(0x24),
        subsystem_id: d.u16_at(0x26),
        package_version: ImageVersion::parse(d, 0x38),
    })
}

pub fn manifest(t: &dyn Transport) -> Option<Manifest> {
    let reply = manifest_request().send_checked(t, "CI upload").ok()?;
    parse_manifest(&reply.data_in)
}

pub fn release_level_name(level: u8) -> &'static str {
    match level {
        0x00 => "development",
        0x10 => "pre-alpha",
        0x20 => "alpha",
        0x30 => "beta",
        0x40 => "release candidate",
        0x50 => "GCA",
        0x60 => "point release",
        _ => "unknown",
    }
}

pub fn sas_link_rate_name(rate: u8) -> &'static str {
    match rate & 0x0F {
        0x00 => "unknown",
        0x01 => "disabled",
        0x02 => "negotiation failed",
        0x03 => "SATA OOB complete",
        0x04 => "port selector",
        0x05 => "SMP reset in progress",
        0x06 => "unsupported phy",
        0x08 => "1.5 Gb/s",
        0x09 => "3.0 Gb/s",
        0x0A => "6.0 Gb/s",
        0x0B => "12.0 Gb/s",
        0x0C => "22.5 Gb/s",
        _ => "reserved",
    }
}

pub fn pcie_link_rate_name(rate: u8) -> &'static str {
    match rate & 0x0F {
        0x00 => "unknown",
        0x01 => "disabled",
        0x02 => "2.5 GT/s",
        0x03 => "5.0 GT/s",
        0x04 => "8.0 GT/s",
        0x05 => "16.0 GT/s",
        0x06 => "32.0 GT/s",
        _ => "reserved",
    }
}
