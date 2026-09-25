use anyhow::{Result, bail};

use super::mpi::{
    FUNCTION_CONFIG, IOCSTATUS_CONFIG_INVALID_PAGE, IOCSTATUS_SUCCESS, ioc_status_name,
};
use super::transport::{Request, Transport};
use crate::bytes::{Le, LeMut};

pub const ACTION_PAGE_HEADER: u8 = 0x00;
pub const ACTION_READ_CURRENT: u8 = 0x02;
pub const CONFIG_REQUEST_LEN: usize = 0x20;
pub const PAGE_HEADER_LEN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageId {
    pub name: &'static str,
    pub page_type: u8,
    pub number: u8,
}

const fn page(name: &'static str, page_type: u8, number: u8) -> PageId {
    PageId {
        name,
        page_type,
        number,
    }
}

pub const IO_UNIT_0: PageId = page("IO Unit page 0", 0x00, 0);
pub const IO_UNIT_4: PageId = page("IO Unit page 4", 0x00, 4);
pub const IO_UNIT_19: PageId = page("IO Unit page 19", 0x00, 19);
pub const MANUFACTURING_0: PageId = page("Manufacturing page 0", 0x01, 0);
pub const IOC_0: PageId = page("IOC page 0", 0x02, 0);
pub const ENCLOSURE_0: PageId = page("Enclosure page 0", 0x11, 0);
pub const DEVICE_0: PageId = page("Device page 0", 0x12, 0);
pub const SAS_IO_UNIT_0: PageId = page("SAS IO Unit page 0", 0x20, 0);
pub const SAS_PHY_0: PageId = page("SAS PHY page 0", 0x23, 0);
pub const SAS_PHY_1: PageId = page("SAS PHY page 1", 0x23, 1);

pub const DEVICE_FORM_HANDLE: u32 = 0x2000_0000;
pub const ENCLOSURE_FORM_HANDLE: u32 = 0x1000_0000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageHeader {
    pub version: u8,
    pub number: u8,
    pub attribute: u8,
    pub length: u16,
    pub page_type: u8,
}

impl PageHeader {
    pub fn parse(d: &[u8]) -> Self {
        Self {
            version: d.u8_at(0x00),
            number: d.u8_at(0x02),
            attribute: d.u8_at(0x03),
            length: d.u16_at(0x04),
            page_type: d.u8_at(0x06),
        }
    }
}

pub fn config_request(
    action: u8,
    page: PageId,
    address: u32,
    header: Option<&PageHeader>,
) -> Request {
    let mut r = Request::new(FUNCTION_CONFIG, CONFIG_REQUEST_LEN);
    r.frame.put_u8(0x0D, page.number);
    r.frame.put_u8(0x0E, page.page_type);
    r.frame.put_u8(0x0F, action);
    r.frame.put_u32(0x10, address);
    match header {
        Some(h) => {
            r.frame.put_u8(0x0C, h.version);
            r.frame.put_u16(0x14, h.length);
            r.data_in_len = h.length as usize * 4;
        }
        None => r.data_in_len = PAGE_HEADER_LEN,
    }
    r
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

pub fn read_header(t: &dyn Transport, page: PageId) -> Result<Option<PageHeader>> {
    let reply = config_request(ACTION_PAGE_HEADER, page, 0, None).send(t)?;
    let status = reply.ioc_status();
    if status == IOCSTATUS_CONFIG_INVALID_PAGE {
        return Ok(None);
    }
    check(page, status, reply.ioc_log_info())?;
    Ok(Some(PageHeader::parse(&reply.data_in)))
}

pub fn read_page(t: &dyn Transport, page: PageId, address: u32) -> Result<Option<Vec<u8>>> {
    let Some(header) = read_header(t, page)? else {
        return Ok(None);
    };
    if header.length == 0 {
        return Ok(None);
    }
    let reply = config_request(ACTION_READ_CURRENT, page, address, Some(&header)).send(t)?;
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
