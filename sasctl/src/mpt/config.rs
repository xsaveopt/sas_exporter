use anyhow::{Result, bail};

use super::mpi::{
    FUNCTION_CONFIG, IOCSTATUS_CONFIG_INVALID_PAGE, IOCSTATUS_SUCCESS, Request, ioc_status_name,
};
use super::transport::Transport;
use crate::bytes::{Le, LeMut};

pub const ACTION_PAGE_HEADER: u8 = 0x00;
pub const ACTION_READ_CURRENT: u8 = 0x01;
pub const ACTION_WRITE_CURRENT: u8 = 0x02;
pub const ACTION_WRITE_NVRAM: u8 = 0x04;

const PAGETYPE_EXTENDED: u8 = 0x0F;
const PAGEATTR_MASK: u8 = 0xF0;
const PAGEATTR_PERSISTENT: u8 = 0x20;
const PAGEATTR_RO_PERSISTENT: u8 = 0x30;
const WALK_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageId {
    pub name: &'static str,
    pub page_type: u8,
    pub number: u8,
    pub ext_type: u8,
    pub version: u8,
}

const fn standard(name: &'static str, page_type: u8, number: u8, version: u8) -> PageId {
    PageId {
        name,
        page_type,
        number,
        ext_type: 0,
        version,
    }
}

const fn extended(name: &'static str, ext_type: u8, number: u8, version: u8) -> PageId {
    PageId {
        name,
        page_type: PAGETYPE_EXTENDED,
        number,
        ext_type,
        version,
    }
}

pub const MANUFACTURING_0: PageId = standard("Manufacturing page 0", 0x09, 0, 0x00);
pub const IO_UNIT_0: PageId = standard("IO Unit page 0", 0x00, 0, 0x02);
pub const IO_UNIT_7: PageId = standard("IO Unit page 7", 0x00, 7, 0x05);
pub const MANUFACTURING_4: PageId = standard("Manufacturing page 4", 0x09, 4, 0x0A);
pub const IOC_0: PageId = standard("IOC page 0", 0x01, 0, 0x02);
pub const IOC_6: PageId = standard("IOC page 6", 0x01, 6, 0x05);
pub const BIOS_2: PageId = standard("BIOS page 2", 0x02, 2, 0x04);
pub const BIOS_3: PageId = standard("BIOS page 3", 0x02, 3, 0x01);
pub const RAID_VOLUME_0: PageId = standard("RAID Volume page 0", 0x08, 0, 0x0A);
pub const RAID_VOLUME_1: PageId = standard("RAID Volume page 1", 0x08, 1, 0x03);
pub const RAID_PHYS_DISK_0: PageId = standard("RAID Physical Disk page 0", 0x0A, 0, 0x05);
pub const SAS_IO_UNIT_0: PageId = extended("SAS IO Unit page 0", 0x10, 0, 0x05);
pub const SAS_DEVICE_0: PageId = extended("SAS Device page 0", 0x12, 0, 0x09);
pub const SAS_PHY_0: PageId = extended("SAS PHY page 0", 0x13, 0, 0x03);
pub const SAS_PHY_1: PageId = extended("SAS PHY page 1", 0x13, 1, 0x01);
pub const LOG_0: PageId = extended("Log page 0", 0x14, 0, 0x02);
pub const SAS_ENCLOSURE_0: PageId = extended("SAS Enclosure page 0", 0x15, 0, 0x04);
pub const RAID_CONFIG_0: PageId = extended("RAID Configuration page 0", 0x16, 0, 0x00);

pub const FORM_GET_NEXT: u32 = 0x0000_0000;
pub const RAID_VOLUME_FORM_HANDLE: u32 = 0x1000_0000;
pub const PHYSDISK_FORM_NUMBER: u32 = 0x1000_0000;
pub const PHYSDISK_FORM_DEVHANDLE: u32 = 0x2000_0000;
pub const SAS_DEVICE_FORM_HANDLE: u32 = 0x2000_0000;
pub const ENCLOSURE_FORM_HANDLE: u32 = 0x1000_0000;
pub const HANDLE_START: u32 = 0x0000_FFFF;
pub const RAID_CONFIG_FORM_CONFIGNUM: u32 = 0x1000_0000;
pub const RAID_CONFIG_FORM_ACTIVE: u32 = 0x2000_0000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageHeader {
    pub bytes: [u8; 4],
    pub ext_length: u16,
    pub ext_type: u8,
}

impl PageHeader {
    pub fn write_action(&self) -> u8 {
        match self.bytes[3] & PAGEATTR_MASK {
            PAGEATTR_PERSISTENT | PAGEATTR_RO_PERSISTENT => ACTION_WRITE_NVRAM,
            _ => ACTION_WRITE_CURRENT,
        }
    }

    pub fn length_bytes(&self, page: PageId) -> usize {
        if page.page_type == PAGETYPE_EXTENDED {
            self.ext_length as usize * 4
        } else {
            self.bytes[1] as usize * 4
        }
    }
}

pub fn config_request(
    action: u8,
    page: PageId,
    address: u32,
    header: Option<&PageHeader>,
) -> Request {
    let mut r = Request::new(FUNCTION_CONFIG, 7);
    r.frame.put_u8(0x00, action);
    match header {
        Some(h) => {
            r.frame.put_bytes(0x14, &h.bytes);
            r.frame.put_u16(0x04, h.ext_length);
            r.frame.put_u8(0x06, h.ext_type);
        }
        None => {
            r.frame.put_u8(0x14, page.version);
            r.frame.put_u8(0x16, page.number);
            r.frame.put_u8(0x17, page.page_type);
            r.frame.put_u8(0x06, page.ext_type);
        }
    }
    r.frame.put_u32(0x18, address);
    r
}

pub fn read_header(t: &dyn Transport, page: PageId, address: u32) -> Result<Option<PageHeader>> {
    let reply = config_request(ACTION_PAGE_HEADER, page, address, None).send(t)?;
    let status = reply.ioc_status();
    if status == IOCSTATUS_CONFIG_INVALID_PAGE {
        return Ok(None);
    }
    check(page, status, reply.ioc_log_info())?;
    let r = &reply.reply;
    let bytes = [r.u8_at(0x14), r.u8_at(0x15), r.u8_at(0x16), r.u8_at(0x17)];
    Ok(Some(PageHeader {
        bytes,
        ext_length: r.u16_at(0x04),
        ext_type: r.u8_at(0x06),
    }))
}

pub fn read_page(t: &dyn Transport, page: PageId, address: u32) -> Result<Option<Vec<u8>>> {
    let Some(header) = read_header(t, page, address)? else {
        return Ok(None);
    };
    let len = header.length_bytes(page);
    if len == 0 {
        return Ok(None);
    }
    let mut req = config_request(ACTION_READ_CURRENT, page, address, Some(&header));
    req.data_in_len = len;
    let reply = req.send(t)?;
    let status = reply.ioc_status();
    if status == IOCSTATUS_CONFIG_INVALID_PAGE {
        return Ok(None);
    }
    check(page, status, reply.ioc_log_info())?;
    Ok(Some(reply.data_in))
}

pub fn require_page(t: &dyn Transport, page: PageId, address: u32) -> Result<Vec<u8>> {
    match read_page(t, page, address)? {
        Some(p) => Ok(p),
        None => bail!(
            "{} (address 0x{address:08x}) is not available on this controller",
            page.name
        ),
    }
}

pub fn write_page(
    t: &dyn Transport,
    page: PageId,
    address: u32,
    action: u8,
    data: &[u8],
) -> Result<()> {
    let Some(header) = read_header(t, page, address)? else {
        bail!("{} is not available on this controller", page.name);
    };
    let mut req = config_request(action, page, address, Some(&header));
    req.data_out = data.to_vec();
    let reply = req.send(t)?;
    check(page, reply.ioc_status(), reply.ioc_log_info())
}

pub fn write_page_by_attribute(
    t: &dyn Transport,
    page: PageId,
    address: u32,
    data: &[u8],
) -> Result<u8> {
    let Some(header) = read_header(t, page, address)? else {
        bail!("{} is not available on this controller", page.name);
    };
    let action = header.write_action();
    let mut req = config_request(action, page, address, Some(&header));
    req.data_out = data.to_vec();
    let reply = req.send(t)?;
    check(page, reply.ioc_status(), reply.ioc_log_info())?;
    Ok(action)
}

fn check(page: PageId, status: u16, log_info: u32) -> Result<()> {
    if status != IOCSTATUS_SUCCESS {
        bail!(
            "reading {} failed with IOCStatus 0x{status:04x} ({}), IOCLogInfo 0x{log_info:08x}",
            page.name,
            ioc_status_name(status)
        );
    }
    Ok(())
}

pub fn walk(
    t: &dyn Transport,
    page: PageId,
    start: u32,
    key_of: impl Fn(&[u8]) -> u32,
) -> Result<Vec<Vec<u8>>> {
    let mut pages = Vec::new();
    let mut address = start;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..WALK_LIMIT {
        let Some(p) = read_page(t, page, FORM_GET_NEXT | address)? else {
            break;
        };
        let key = key_of(&p);
        if !seen.insert(key) {
            break;
        }
        pages.push(p);
        address = key;
    }
    Ok(pages)
}

pub fn sas_devices(t: &dyn Transport) -> Result<Vec<Vec<u8>>> {
    walk(t, SAS_DEVICE_0, HANDLE_START, |p| p.u16_at(0x18) as u32)
}

pub fn enclosures(t: &dyn Transport) -> Result<Vec<Vec<u8>>> {
    walk(t, SAS_ENCLOSURE_0, HANDLE_START, |p| p.u16_at(0x16) as u32)
}

pub fn raid_volumes(t: &dyn Transport) -> Result<Vec<Vec<u8>>> {
    walk(t, RAID_VOLUME_0, HANDLE_START, |p| p.u16_at(0x04) as u32)
}
