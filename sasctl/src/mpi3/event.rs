use anyhow::{Result, bail};
use serde::Serialize;

use super::mpi::{FUNCTION_PERSISTENT_EVENT_LOG, hex};
use super::transport::{Reply, Request, Transport};
use crate::bytes::{Le, LeMut};

pub const PEL_REQUEST_LEN: usize = 0x20;
pub const ACTION_GET_SEQNUM: u8 = 0x01;
pub const ACTION_GET_LOG: u8 = 0x03;
pub const SEQ_LEN: usize = 24;
pub const ENTRY_LEN: usize = 128;
pub const LIST_HEADER_LEN: usize = 8;
pub const ENTRIES_PER_READ: usize = 32;
pub const LOCALE_ALL: u16 = 0x03FF;
pub const CLASS_ALL: u8 = 0x00;
pub const STATUS_SUCCESS: u16 = 0;
pub const STATUS_NOT_FOUND: u16 = 1;
const READ_LIMIT: usize = 4096;

const LOCALE_NAMES: &[(u16, &str)] = &[
    (0x0200, "non-blocking boot"),
    (0x0100, "blocking boot"),
    (0x0080, "PCIe"),
    (0x0040, "configuration"),
    (0x0020, "controller"),
    (0x0010, "SAS"),
    (0x0008, "energy pack"),
    (0x0004, "enclosure"),
    (0x0002, "PD"),
    (0x0001, "VD"),
];

pub fn seqnum_request() -> Request {
    let mut r = Request::new(FUNCTION_PERSISTENT_EVENT_LOG, PEL_REQUEST_LEN);
    r.frame.put_u8(0x0A, ACTION_GET_SEQNUM);
    r.data_in_len = SEQ_LEN;
    r
}

pub fn get_log_request(start: u32, locale: u16, class: u8, entries: usize) -> Request {
    let mut r = Request::new(FUNCTION_PERSISTENT_EVENT_LOG, PEL_REQUEST_LEN);
    r.frame.put_u8(0x0A, ACTION_GET_LOG);
    r.frame.put_u32(0x0C, start);
    r.frame.put_u16(0x10, locale);
    r.frame.put_u8(0x12, class);
    r.data_in_len = LIST_HEADER_LEN + ENTRY_LEN * entries;
    r
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Sequence {
    pub newest: u32,
    pub oldest: u32,
    pub clear: u32,
    pub shutdown: u32,
    pub boot: u32,
    pub last_acknowledged: u32,
}

impl Sequence {
    pub fn parse(d: &[u8]) -> Self {
        Self {
            newest: d.u32_at(0x00),
            oldest: d.u32_at(0x04),
            clear: d.u32_at(0x08),
            shutdown: d.u32_at(0x0C),
            boot: d.u32_at(0x10),
            last_acknowledged: d.u32_at(0x14),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PelEntry {
    pub sequence: u32,
    pub time_stamp: u64,
    pub log_code: u16,
    pub arg_type: u16,
    pub locale: u16,
    pub locale_names: Vec<&'static str>,
    pub class: u8,
    pub class_name: &'static str,
    pub flags: u8,
    pub info: String,
}

pub fn class_name(class: u8) -> &'static str {
    match class {
        0x00 => "debug",
        0x01 => "progress",
        0x02 => "info",
        0x03 => "warning",
        0x04 => "critical",
        0x05 => "fatal",
        0x06 => "fault",
        _ => "unknown",
    }
}

pub fn locale_names(locale: u16) -> Vec<&'static str> {
    LOCALE_NAMES
        .iter()
        .filter(|(bit, _)| locale & bit != 0)
        .map(|(_, n)| *n)
        .collect()
}

pub fn parse_entry(e: &[u8]) -> PelEntry {
    let info = e.get(0x20..ENTRY_LEN).unwrap_or(&[]);
    let used = info.len() - info.iter().rev().take_while(|b| **b == 0).count();
    PelEntry {
        sequence: e.u32_at(0x08),
        time_stamp: e.u64_at(0x00),
        log_code: e.u16_at(0x0C),
        arg_type: e.u16_at(0x0E),
        locale: e.u16_at(0x10),
        locale_names: locale_names(e.u16_at(0x10)),
        class: e.u8_at(0x12),
        class_name: class_name(e.u8_at(0x12)),
        flags: e.u8_at(0x13),
        info: hex(&info[..used.next_multiple_of(4).min(info.len())]),
    }
}

pub fn parse_list(d: &[u8]) -> Vec<PelEntry> {
    let declared = d.u32_at(0x00) as usize;
    let fits = d.len().saturating_sub(LIST_HEADER_LEN) / ENTRY_LEN;
    (0..declared.min(fits))
        .map(|i| {
            let o = LIST_HEADER_LEN + i * ENTRY_LEN;
            parse_entry(&d[o..o + ENTRY_LEN])
        })
        .collect()
}

fn pel_status(reply: &Reply) -> u16 {
    if reply.is_address() {
        reply.frame.u16_at(0x14)
    } else {
        STATUS_SUCCESS
    }
}

pub fn sequence(t: &dyn Transport) -> Result<Sequence> {
    let reply = seqnum_request().send_checked(t, "PEL get sequence numbers")?;
    let status = pel_status(&reply);
    if status != STATUS_SUCCESS {
        bail!("PEL get sequence numbers returned PEL status {status}");
    }
    Ok(Sequence::parse(&reply.data_in))
}

#[derive(Clone, Debug, Serialize)]
pub struct EventLog {
    pub sequence: Sequence,
    pub events: Vec<PelEntry>,
}

pub fn read_log(t: &dyn Transport, latest: Option<u32>) -> Result<EventLog> {
    let seq = sequence(t)?;
    let mut start = match latest {
        Some(n) => seq
            .newest
            .saturating_add(1)
            .saturating_sub(n)
            .max(seq.oldest),
        None => seq.oldest,
    };
    let mut events: Vec<PelEntry> = Vec::new();
    for _ in 0..READ_LIMIT {
        if start > seq.newest || latest == Some(0) {
            break;
        }
        let reply = get_log_request(start, LOCALE_ALL, CLASS_ALL, ENTRIES_PER_READ)
            .send_checked(t, "PEL get log")?;
        match pel_status(&reply) {
            STATUS_SUCCESS => {}
            STATUS_NOT_FOUND => break,
            other => bail!("PEL get log returned PEL status {other}"),
        }
        let batch = parse_list(&reply.data_in);
        let Some(last) = batch.last().map(|e| e.sequence) else {
            break;
        };
        events.extend(batch.into_iter().filter(|e| e.sequence >= start));
        if last < start {
            break;
        }
        start = last.saturating_add(1);
    }
    Ok(EventLog {
        sequence: seq,
        events,
    })
}
