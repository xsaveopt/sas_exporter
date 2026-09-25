use anyhow::{Result, bail};
use serde::Serialize;

use super::mpi::{event_name, hex};
use super::transport::{
    EVENTREPORT_ENCODED_LEN, Generation, HEADER_LEN, NR_DIAGQUERY, NR_DIAGREADBUFFER,
    NR_DIAGREGISTER, NR_DIAGRELEASE, NR_DIAGUNREGISTER, NR_EVENTENABLE, NR_EVENTQUERY,
    NR_EVENTREPORT, NR_HARDRESET, Transport,
};
use crate::bytes::{Le, LeMut};

pub const REGISTER_LEN: usize = 120;
pub const QUERY_LEN: usize = 124;
pub const RELEASE_LEN: usize = 16;
pub const UNREGISTER_LEN: usize = 16;
pub const READ_BUFFER_LEN: usize = 32;
pub const READ_BUFFER_DATA: usize = 0x1C;
pub const HARDRESET_LEN: usize = 12;
pub const EVENTQUERY_LEN: usize = 32;
pub const EVENTENABLE_LEN: usize = 28;
pub const EVENT_ENTRY_LEN: usize = 200;
pub const EVENT_LOG_ENTRIES: usize = 200;
pub const EVENT_DATA_LEN: usize = 192;
pub const READ_CHUNK: usize = 64 * 1024;

pub const MPT2_DEFAULT_UNIQUE_ID: u32 = 0x0707_5900;
pub const MPT3_DEFAULT_UNIQUE_ID: u32 = 0x4252_434D;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BufferType {
    Trace,
    Snapshot,
    Extended,
}

impl BufferType {
    pub fn code(self) -> u8 {
        match self {
            BufferType::Trace => 0,
            BufferType::Snapshot => 1,
            BufferType::Extended => 2,
        }
    }
}

pub fn default_unique_id(generation: Generation) -> u32 {
    match generation {
        Generation::Sas2 => MPT2_DEFAULT_UNIQUE_ID,
        Generation::Sas3 => MPT3_DEFAULT_UNIQUE_ID,
    }
}

pub fn register_buffer(
    buffer_type: BufferType,
    size: u32,
    unique_id: u32,
    diagnostic_flags: u32,
) -> Vec<u8> {
    let mut b = vec![0u8; REGISTER_LEN];
    b.put_u8(0x0D, buffer_type.code());
    b.put_u32(0x10, diagnostic_flags);
    b.put_u32(0x70, size);
    b.put_u32(0x74, unique_id);
    b
}

pub fn register(
    t: &dyn Transport,
    buffer_type: BufferType,
    size: u32,
    unique_id: u32,
    diagnostic_flags: u32,
) -> Result<()> {
    if size == 0 || !size.is_multiple_of(4) {
        bail!("buffer size must be a nonzero multiple of 4 bytes");
    }
    if unique_id == 0 {
        bail!("unique id must be nonzero");
    }
    let mut b = register_buffer(buffer_type, size, unique_id, diagnostic_flags);
    t.raw(NR_DIAGREGISTER, REGISTER_LEN, &mut b)
}

pub fn unique_id_buffer(len: usize, unique_id: u32) -> Vec<u8> {
    let mut b = vec![0u8; len];
    b.put_u32(0x0C, unique_id);
    b
}

pub fn release(t: &dyn Transport, unique_id: u32) -> Result<()> {
    let mut b = unique_id_buffer(RELEASE_LEN, unique_id);
    t.raw(NR_DIAGRELEASE, RELEASE_LEN, &mut b)
}

pub fn unregister(t: &dyn Transport, unique_id: u32) -> Result<()> {
    let mut b = unique_id_buffer(UNREGISTER_LEN, unique_id);
    t.raw(NR_DIAGUNREGISTER, UNREGISTER_LEN, &mut b)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DiagQuery {
    pub buffer_type: u8,
    pub application_flags: u16,
    pub app_owned: bool,
    pub buffer_valid: bool,
    pub fw_buffer_access: bool,
    pub dynamic_buffer_alloc: bool,
    pub diagnostic_flags: u32,
    pub total_buffer_size: u32,
    pub driver_added_buffer_size: u32,
    pub unique_id: u32,
}

pub fn parse_query(b: &[u8]) -> DiagQuery {
    let flags = b.u16_at(0x0E);
    DiagQuery {
        buffer_type: b.u8_at(0x0D),
        application_flags: flags,
        app_owned: flags & 0x0001 != 0,
        buffer_valid: flags & 0x0002 != 0,
        fw_buffer_access: flags & 0x0004 != 0,
        dynamic_buffer_alloc: flags & 0x0008 != 0,
        diagnostic_flags: b.u32_at(0x10),
        total_buffer_size: b.u32_at(0x70),
        driver_added_buffer_size: b.u32_at(0x74),
        unique_id: b.u32_at(0x78),
    }
}

pub fn query(t: &dyn Transport, buffer_type: BufferType) -> Result<DiagQuery> {
    let mut b = vec![0u8; QUERY_LEN];
    b.put_u8(0x0D, buffer_type.code());
    t.raw(NR_DIAGQUERY, QUERY_LEN, &mut b)?;
    Ok(parse_query(&b))
}

pub fn read_buffer_request(unique_id: u32, offset: u32, len: usize) -> Vec<u8> {
    let mut b = vec![0u8; READ_BUFFER_DATA + len.max(4)];
    b.put_u32(0x10, offset);
    b.put_u32(0x14, len as u32);
    b.put_u32(0x18, unique_id);
    b
}

pub fn read_all(t: &dyn Transport, unique_id: u32, total: u32) -> Result<Vec<u8>> {
    let total = total as usize & !3;
    let mut out = Vec::with_capacity(total);
    while out.len() < total {
        let len = (total - out.len()).min(READ_CHUNK);
        let mut b = read_buffer_request(unique_id, out.len() as u32, len);
        t.raw(NR_DIAGREADBUFFER, READ_BUFFER_LEN, &mut b)?;
        out.extend_from_slice(&b[READ_BUFFER_DATA..READ_BUFFER_DATA + len]);
    }
    Ok(out)
}

pub fn hard_reset(t: &dyn Transport) -> Result<()> {
    let mut b = vec![0u8; HARDRESET_LEN];
    t.raw(NR_HARDRESET, HARDRESET_LEN, &mut b)
}

pub fn event_query(t: &dyn Transport) -> Result<(u16, [u32; 4])> {
    let mut b = vec![0u8; EVENTQUERY_LEN];
    t.raw(NR_EVENTQUERY, EVENTQUERY_LEN, &mut b)?;
    let mask = [
        b.u32_at(0x10),
        b.u32_at(0x14),
        b.u32_at(0x18),
        b.u32_at(0x1C),
    ];
    Ok((b.u16_at(0x0C), mask))
}

pub fn event_enable_buffer(mask: [u32; 4]) -> Vec<u8> {
    let mut b = vec![0u8; EVENTENABLE_LEN];
    for (i, word) in mask.iter().enumerate() {
        b.put_u32(0x0C + i * 4, *word);
    }
    b
}

pub fn event_enable_all(t: &dyn Transport) -> Result<()> {
    let mut b = event_enable_buffer([u32::MAX; 4]);
    t.raw(NR_EVENTENABLE, EVENTENABLE_LEN, &mut b)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Event {
    pub context: u32,
    pub code: u32,
    pub name: &'static str,
    pub data: String,
}

pub fn event_report_buffer() -> Vec<u8> {
    let total = HEADER_LEN + EVENT_LOG_ENTRIES * EVENT_ENTRY_LEN;
    let mut b = vec![0u8; total];
    b.put_u32(0x08, total as u32);
    b
}

pub fn parse_events(b: &[u8]) -> Vec<Event> {
    let mut events: Vec<Event> = (0..EVENT_LOG_ENTRIES)
        .filter_map(|i| {
            let o = HEADER_LEN + i * EVENT_ENTRY_LEN;
            if o + EVENT_ENTRY_LEN > b.len() {
                return None;
            }
            let code = b.u32_at(o);
            let context = b.u32_at(o + 4);
            if code == 0 && context == 0 {
                return None;
            }
            let data = &b[o + 8..o + 8 + EVENT_DATA_LEN];
            let used = data.iter().rposition(|x| *x != 0).map_or(0, |p| p + 1);
            Some(Event {
                context,
                code,
                name: event_name(code),
                data: hex(&data[..used]),
            })
        })
        .collect();
    events.sort_by_key(|e| e.context);
    events
}

pub fn event_report(t: &dyn Transport) -> Result<Option<Vec<Event>>> {
    let (_, mask) = event_query(t)?;
    if mask.iter().all(|w| *w == 0) {
        return Ok(None);
    }
    let mut b = event_report_buffer();
    match t.raw(NR_EVENTREPORT, EVENTREPORT_ENCODED_LEN, &mut b) {
        Ok(()) => Ok(Some(parse_events(&b))),
        Err(e) if is_errno(&e, libc::ENODATA) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn is_errno(err: &anyhow::Error, errno: i32) -> bool {
    err.chain().any(|e| {
        e.downcast_ref::<std::io::Error>()
            .and_then(|io| io.raw_os_error())
            == Some(errno)
    })
}
