use anyhow::{Result, bail};
use serde::Serialize;

use super::transport::{Command, Reply, Transport};
use crate::bytes::{Le, LeMut};

pub const FUNCTION_SCSI_IO: u8 = 0x00;
pub const FUNCTION_IOC_FACTS: u8 = 0x03;
pub const FUNCTION_CONFIG: u8 = 0x04;
pub const FUNCTION_FW_DOWNLOAD: u8 = 0x09;
pub const FUNCTION_FW_UPLOAD: u8 = 0x12;
pub const FUNCTION_RAID_ACTION: u8 = 0x15;
pub const FUNCTION_RAID_SCSI_IO_PASSTHROUGH: u8 = 0x16;
pub const FUNCTION_SEP: u8 = 0x18;
pub const FUNCTION_SAS_IO_UNIT_CONTROL: u8 = 0x1B;

pub const IOCSTATUS_SUCCESS: u16 = 0x0000;
pub const IOCSTATUS_CONFIG_INVALID_PAGE: u16 = 0x0022;
pub const IOCSTATUS_SCSI_RECOVERED_ERROR: u16 = 0x0040;
pub const IOCSTATUS_SCSI_DATA_UNDERRUN: u16 = 0x0045;

pub const TIMEOUT_SHORT: u32 = 30;
pub const TIMEOUT_RAID: u32 = 60;
pub const TIMEOUT_FIRMWARE: u32 = 300;

pub const CAPABILITY_INTEGRATED_RAID: u32 = 0x0000_1000;

const CAPABILITY_NAMES: &[(u32, &str)] = &[
    (0x0080_0000, "mctp-passthrough"),
    (0x0020_0000, "coredump"),
    (0x0010_0000, "pcie-sriov"),
    (0x0008_0000, "atomic-request"),
    (0x0004_0000, "rdpq-array"),
    (0x0002_0000, "fast-path"),
    (0x0001_0000, "host-based-discovery"),
    (0x0000_8000, "msi-x-index"),
    (0x0000_4000, "raid-accelerator"),
    (0x0000_2000, "event-replay"),
    (0x0000_1000, "integrated-raid"),
    (0x0000_0800, "tlr"),
    (0x0000_0100, "multicast"),
    (0x0000_0080, "bidirectional-target"),
    (0x0000_0040, "eedp"),
    (0x0000_0020, "extended-buffer"),
    (0x0000_0010, "snapshot-buffer"),
    (0x0000_0008, "diag-trace-buffer"),
    (0x0000_0004, "task-set-full-handling"),
];

pub struct Request {
    pub frame: Vec<u8>,
    pub sge_offset: u32,
    pub data_out: Vec<u8>,
    pub data_in_len: usize,
    pub timeout: u32,
}

impl Request {
    pub fn new(function: u8, sge_offset: u32) -> Self {
        let mut frame = vec![0u8; sge_offset as usize * 4];
        frame.put_u8(3, function);
        Self {
            frame,
            sge_offset,
            data_out: Vec::new(),
            data_in_len: 0,
            timeout: TIMEOUT_SHORT,
        }
    }

    pub fn send(&self, t: &dyn Transport) -> Result<Reply> {
        t.command(&Command {
            frame: &self.frame,
            sge_offset: self.sge_offset,
            data_out: &self.data_out,
            data_in_len: self.data_in_len,
            timeout: self.timeout,
        })
    }

    pub fn send_checked(&self, t: &dyn Transport, what: &str) -> Result<Reply> {
        let reply = self.send(t)?;
        ensure_success(&reply, what)?;
        Ok(reply)
    }
}

pub fn ioc_status_name(status: u16) -> &'static str {
    match status {
        0x0000 => "SUCCESS",
        0x0001 => "INVALID_FUNCTION",
        0x0002 => "BUSY",
        0x0003 => "INVALID_SGL",
        0x0004 => "INTERNAL_ERROR",
        0x0005 => "INVALID_VPID",
        0x0006 => "INSUFFICIENT_RESOURCES",
        0x0007 => "INVALID_FIELD",
        0x0008 => "INVALID_STATE",
        0x0009 => "OP_STATE_NOT_SUPPORTED",
        0x000A => "INSUFFICIENT_POWER",
        0x000F => "FAILURE",
        0x0020 => "CONFIG_INVALID_ACTION",
        0x0021 => "CONFIG_INVALID_TYPE",
        0x0022 => "CONFIG_INVALID_PAGE",
        0x0023 => "CONFIG_INVALID_DATA",
        0x0024 => "CONFIG_NO_DEFAULTS",
        0x0025 => "CONFIG_CANT_COMMIT",
        0x0040 => "SCSI_RECOVERED_ERROR",
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
        0x0090 => "SAS_SMP_REQUEST_FAILED",
        0x0091 => "SAS_SMP_DATA_OVERRUN",
        0x00A0 => "DIAGNOSTIC_RELEASED",
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

pub fn capability_names(capabilities: u32) -> Vec<&'static str> {
    CAPABILITY_NAMES
        .iter()
        .filter(|(bit, _)| capabilities & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

pub fn version_string(word: u32) -> String {
    let b = word.to_be_bytes();
    format!("{:02}.{:02}.{:02}.{:02}", b[0], b[1], b[2], b[3])
}

pub fn bios_version_string(word: u32) -> String {
    let b = word.to_be_bytes();
    format!("{}.{:02}.{:02}.{:02}", b[0], b[1], b[2], b[3])
}

pub fn ioc_facts_request() -> Request {
    Request::new(FUNCTION_IOC_FACTS, 3)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct IocFacts {
    pub msg_version: u16,
    pub header_version: u16,
    pub ioc_number: u8,
    pub ioc_exceptions: u16,
    pub number_of_ports: u8,
    pub request_credit: u16,
    pub product_id: u16,
    pub ioc_capabilities: u32,
    pub fw_version: u32,
    pub ioc_request_frame_size: u16,
    pub max_initiators: u16,
    pub max_targets: u16,
    pub max_sas_expanders: u16,
    pub max_enclosures: u16,
    pub protocol_flags: u16,
    pub reply_frame_size: u8,
    pub max_volumes: u8,
    pub max_dev_handle: u16,
    pub max_persistent_entries: u16,
    pub min_dev_handle: u16,
}

impl IocFacts {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            msg_version: b.u16_at(0x00),
            header_version: b.u16_at(0x04),
            ioc_number: b.u8_at(0x06),
            ioc_exceptions: b.u16_at(0x0C),
            number_of_ports: b.u8_at(0x16),
            request_credit: b.u16_at(0x18),
            product_id: b.u16_at(0x1A),
            ioc_capabilities: b.u32_at(0x1C),
            fw_version: b.u32_at(0x20),
            ioc_request_frame_size: b.u16_at(0x24),
            max_initiators: b.u16_at(0x28),
            max_targets: b.u16_at(0x2A),
            max_sas_expanders: b.u16_at(0x2C),
            max_enclosures: b.u16_at(0x2E),
            protocol_flags: b.u16_at(0x30),
            reply_frame_size: b.u8_at(0x36),
            max_volumes: b.u8_at(0x37),
            max_dev_handle: b.u16_at(0x38),
            max_persistent_entries: b.u16_at(0x3A),
            min_dev_handle: b.u16_at(0x3C),
        }
    }

    pub fn raid_support(&self) -> bool {
        self.ioc_capabilities & CAPABILITY_INTEGRATED_RAID != 0
    }

    pub fn mpi_version(&self) -> String {
        format!("{:03x}.{:02x}", self.msg_version, self.header_version)
    }
}

pub fn ioc_facts(t: &dyn Transport) -> Result<IocFacts> {
    let reply = ioc_facts_request().send_checked(t, "IOC Facts")?;
    Ok(IocFacts::parse(&reply.reply))
}

pub fn sep_locate_request(enclosure_handle: u16, slot: u16, on: bool) -> Request {
    let mut r = Request::new(FUNCTION_SEP, 8);
    r.frame.put_u8(0x04, 0x00);
    r.frame.put_u8(0x05, 0x01);
    r.frame
        .put_u32(0x0C, if on { 0x0002_0000 } else { 0x0000_0000 });
    r.frame.put_u16(0x1C, slot);
    r.frame.put_u16(0x1E, enclosure_handle);
    r
}

pub const SAS_OP_PHY_LINK_RESET: u8 = 0x06;
pub const SAS_OP_PHY_HARD_RESET: u8 = 0x07;

pub fn phy_reset_request(phy: u8, hard: bool) -> Request {
    let mut r = Request::new(FUNCTION_SAS_IO_UNIT_CONTROL, 11);
    r.frame.put_u8(
        0x00,
        if hard {
            SAS_OP_PHY_HARD_RESET
        } else {
            SAS_OP_PHY_LINK_RESET
        },
    );
    r.frame.put_u8(0x0E, phy);
    r
}

pub fn link_rate_name(rate: u8) -> &'static str {
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

pub fn event_name(code: u32) -> &'static str {
    match code {
        0x0001 => "LOG_DATA",
        0x0002 => "STATE_CHANGE",
        0x0005 => "HARD_RESET_RECEIVED",
        0x000A => "EVENT_CHANGE",
        0x000E => "TASK_SET_FULL",
        0x000F => "SAS_DEVICE_STATUS_CHANGE",
        0x0014 => "IR_OPERATION_STATUS",
        0x0016 => "SAS_DISCOVERY",
        0x0017 => "SAS_BROADCAST_PRIMITIVE",
        0x0018 => "SAS_INIT_DEVICE_STATUS_CHANGE",
        0x0019 => "SAS_INIT_TABLE_OVERFLOW",
        0x001C => "SAS_TOPOLOGY_CHANGE_LIST",
        0x001D => "ENCL_DEVICE_STATUS_CHANGE",
        0x001E => "IR_VOLUME",
        0x001F => "IR_PHYSICAL_DISK",
        0x0020 => "IR_CONFIGURATION_CHANGE_LIST",
        0x0021 => "LOG_ENTRY_ADDED",
        0x0022 => "SAS_PHY_COUNTER",
        0x0023 => "GPIO_INTERRUPT",
        0x0024 => "HOST_BASED_DISCOVERY_PHY",
        0x0025 => "SAS_QUIESCE",
        0x0026 => "SAS_NOTIFY_PRIMITIVE",
        0x0027 => "TEMP_THRESHOLD",
        0x0028 => "HOST_MESSAGE",
        0x0029 => "POWER_PERFORMANCE_CHANGE",
        0x0030 => "PCIE_DEVICE_STATUS_CHANGE",
        0x0031 => "PCIE_ENUMERATION",
        0x0032 => "PCIE_TOPOLOGY_CHANGE_LIST",
        0x0033 => "PCIE_LINK_COUNTER",
        0x0034 => "ACTIVE_CABLE_EXCEPTION",
        0x0035 => "SAS_DEVICE_DISCOVERY_ERROR",
        _ => "UNKNOWN",
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
