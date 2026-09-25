use std::collections::VecDeque;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::bytes::Le;
use crate::mega::dcmd::{dcmd_read, status_of};
use crate::mega::mfi::{Mbox, STATUS_NOT_FOUND, op};
use crate::mega::transport::Transport;

pub const LOG_INFO_LEN: usize = 20;
pub const EVENT_LEN: usize = 256;
pub const EVENTS_PER_READ: usize = 15;
pub const CONTROLLER_EPOCH: i64 = 946_684_800;
pub const LOCALE_ALL: u16 = 0xffff;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogInfo {
    pub newest_seq: u32,
    pub oldest_seq: u32,
    pub clear_seq: u32,
    pub shutdown_seq: u32,
    pub boot_seq: u32,
}

impl LogInfo {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            newest_seq: b.u32_at(0),
            oldest_seq: b.u32_at(4),
            clear_seq: b.u32_at(8),
            shutdown_seq: b.u32_at(12),
            boot_seq: b.u32_at(16),
        }
    }
}

pub fn class_name(class: i8) -> String {
    match class {
        -2 => "debug".into(),
        -1 => "progress".into(),
        0 => "info".into(),
        1 => "warning".into(),
        2 => "critical".into(),
        3 => "fatal".into(),
        4 => "dead".into(),
        other => format!("{other}"),
    }
}

pub fn parse_class(s: &str) -> Result<i8> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "debug" => -2,
        "progress" => -1,
        "info" => 0,
        "warning" => 1,
        "critical" => 2,
        "fatal" => 3,
        "dead" => 4,
        _ => bail!(
            "unknown event class {s}, use debug, progress, info, warning, critical, fatal or dead"
        ),
    })
}

pub const LOCALES: &[(&str, u16)] = &[
    ("ld", 0x0001),
    ("pd", 0x0002),
    ("enclosure", 0x0004),
    ("bbu", 0x0008),
    ("sas", 0x0010),
    ("controller", 0x0020),
    ("config", 0x0040),
    ("cluster", 0x0080),
    ("all", LOCALE_ALL),
];

pub fn parse_locale(s: &str) -> Result<u16> {
    let mut mask = 0u16;
    for part in s.split(',') {
        let part = part.trim().to_ascii_lowercase();
        match LOCALES.iter().find(|(n, _)| *n == part) {
            Some((_, bits)) => mask |= bits,
            None => bail!(
                "unknown event locale {part}, use {}",
                LOCALES
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    Ok(mask)
}

pub fn locale_names(locale: u16) -> Vec<&'static str> {
    if locale == LOCALE_ALL {
        return vec!["all"];
    }
    LOCALES
        .iter()
        .filter(|(n, bits)| *n != "all" && locale & bits != 0)
        .map(|(n, _)| *n)
        .collect()
}

pub fn class_locale_word(class: i8, locale: u16) -> u32 {
    u32::from(locale) | (u32::from(class as u8) << 24)
}

pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub fn format_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn format_fw_time(secs: u32) -> String {
    if secs >> 24 == 0xff {
        return format!("boot + {}s", secs & 0x00ff_ffff);
    }
    format_unix(CONTROLLER_EPOCH + i64::from(secs))
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub seq: u32,
    pub timestamp: u32,
    pub time: String,
    pub code: u32,
    pub locale: u16,
    pub locales: Vec<&'static str>,
    pub class: i8,
    pub class_name: String,
    pub arg_type: u8,
    pub description: String,
}

impl Event {
    pub fn parse(b: &[u8]) -> Self {
        let class = b.u8_at(15) as i8;
        let locale = b.u16_at(12);
        Self {
            seq: b.u32_at(0),
            timestamp: b.u32_at(4),
            time: format_fw_time(b.u32_at(4)),
            code: b.u32_at(8),
            locale,
            locales: locale_names(locale),
            class,
            class_name: class_name(class),
            arg_type: b.u8_at(16),
            description: b.ascii_at(128, 128),
        }
    }
}

pub fn parse_event_list(b: &[u8]) -> Vec<Event> {
    let count = b.u32_at(0) as usize;
    let fits = b.len().saturating_sub(8) / EVENT_LEN;
    (0..count.min(fits))
        .map(|i| Event::parse(&b[8 + i * EVENT_LEN..8 + (i + 1) * EVENT_LEN]))
        .collect()
}

pub fn log_info(t: &dyn Transport) -> Result<LogInfo> {
    Ok(LogInfo::parse(&dcmd_read(
        t,
        op::EVENT_GET_INFO,
        &Mbox::new(),
        LOG_INFO_LEN,
    )?))
}

#[derive(Clone, Copy, Debug)]
pub struct EventQuery {
    pub start: u32,
    pub stop: u32,
    pub class: i8,
    pub locale: u16,
    pub limit: usize,
}

pub fn fetch(t: &dyn Transport, q: &EventQuery) -> Result<Vec<Event>> {
    let word = class_locale_word(q.class, q.locale);
    let mut kept: VecDeque<Event> = VecDeque::new();
    let mut seq = q.start;
    let len = 8 + EVENT_LEN * EVENTS_PER_READ;
    for _ in 0..1_000_000 {
        let buf = match dcmd_read(
            t,
            op::EVENT_GET,
            &Mbox::new().word(0, seq).word(1, word),
            len,
        ) {
            Ok(buf) => buf,
            Err(e) if status_of(&e) == Some(STATUS_NOT_FOUND) => break,
            Err(e) => return Err(e),
        };
        let events = parse_event_list(&buf);
        let Some(last) = events.last().map(|e| e.seq) else {
            break;
        };
        let mut done = false;
        for ev in events {
            if ev.seq > q.stop && (q.start <= q.stop || ev.seq < q.start) {
                done = true;
                break;
            }
            kept.push_back(ev);
            if kept.len() > q.limit {
                kept.pop_front();
            }
        }
        if done || last == u32::MAX || last.wrapping_add(1) == seq {
            break;
        }
        seq = last.wrapping_add(1);
    }
    Ok(kept.into())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bytes::LeMut;
    use crate::mega::mock::Mock;

    pub fn event_bytes(
        seq: u32,
        time: u32,
        code: u32,
        class: i8,
        locale: u16,
        text: &str,
    ) -> Vec<u8> {
        let mut b = vec![0u8; EVENT_LEN];
        b.put_u32(0, seq);
        b.put_u32(4, time);
        b.put_u32(8, code);
        b.put_u16(12, locale);
        b[15] = class as u8;
        b[16] = 10;
        b[128..128 + text.len()].copy_from_slice(text.as_bytes());
        b
    }

    fn list_bytes(events: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0u8; 8];
        b.put_u32(0, events.len() as u32);
        for e in events {
            b.extend_from_slice(e);
        }
        b
    }

    #[test]
    fn parses_log_info() {
        let mut b = vec![0u8; 20];
        for (i, v) in [900u32, 1, 5, 700, 710].iter().enumerate() {
            b.put_u32(i * 4, *v);
        }
        let info = LogInfo::parse(&b);
        assert_eq!(info.newest_seq, 900);
        assert_eq!(info.boot_seq, 710);
        assert_eq!(info.shutdown_seq, 700);
    }

    #[test]
    fn parses_event_records() {
        let evs = parse_event_list(&list_bytes(&[
            event_bytes(5, 0, 0x71, 1, 0x0002, "PD 08(e0xfc/s0) removed"),
            event_bytes(6, 0xff00_0010, 0x51, -1, 0x0001, "VD 00 state change"),
        ]));
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].class_name, "warning");
        assert_eq!(evs[0].locales, vec!["pd"]);
        assert_eq!(evs[0].time, "2000-01-01 00:00:00");
        assert_eq!(evs[0].description, "PD 08(e0xfc/s0) removed");
        assert_eq!(evs[1].class, -1);
        assert_eq!(evs[1].time, "boot + 16s");
    }

    #[test]
    fn formats_controller_time() {
        assert_eq!(format_fw_time(0), "2000-01-01 00:00:00");
        assert_eq!(format_fw_time(815_130_061), "2025-10-30 09:01:01");
        assert_eq!(format_unix(951_782_400), "2000-02-29 00:00:00");
    }

    #[test]
    fn class_locale_word_puts_class_in_the_top_byte() {
        assert_eq!(class_locale_word(0, LOCALE_ALL), 0x0000_ffff);
        assert_eq!(class_locale_word(-1, 0x0002), 0xff00_0002);
        assert_eq!(parse_locale("pd,ld").unwrap(), 0x0003);
        assert!(parse_locale("nope").is_err());
        assert_eq!(parse_class("critical").unwrap(), 2);
    }

    #[test]
    fn fetch_walks_forward_until_not_found_and_keeps_the_newest() {
        let first: Vec<Vec<u8>> = (10..25)
            .map(|s| event_bytes(s, 0, 1, 0, 0x20, "x"))
            .collect();
        let second: Vec<Vec<u8>> = (25..28)
            .map(|s| event_bytes(s, 0, 1, 0, 0x20, "y"))
            .collect();
        let mock = Mock::new()
            .reply_mbox(op::EVENT_GET, &10u32.to_le_bytes(), list_bytes(&first))
            .reply_mbox(op::EVENT_GET, &25u32.to_le_bytes(), list_bytes(&second))
            .status(op::EVENT_GET, STATUS_NOT_FOUND);
        let q = EventQuery {
            start: 10,
            stop: 1000,
            class: 0,
            locale: LOCALE_ALL,
            limit: 5,
        };
        let evs = fetch(&mock, &q).unwrap();
        assert_eq!(
            evs.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![23, 24, 25, 26, 27]
        );
        let calls = mock.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].mbox()[4..8], [0xff, 0xff, 0x00, 0x00]);
        assert_eq!(calls[1].mbox()[..4], 25u32.to_le_bytes());
        assert_eq!(calls[0].bufs[0].len(), 8 + 256 * 15);
    }

    #[test]
    fn fetch_stops_at_the_stop_sequence() {
        let evs: Vec<Vec<u8>> = (10..20)
            .map(|s| event_bytes(s, 0, 1, 0, 0x20, "x"))
            .collect();
        let mock = Mock::new().reply(op::EVENT_GET, list_bytes(&evs));
        let q = EventQuery {
            start: 10,
            stop: 12,
            class: 0,
            locale: LOCALE_ALL,
            limit: 100,
        };
        let got = fetch(&mock, &q).unwrap();
        assert_eq!(
            got.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
        assert_eq!(mock.calls().len(), 1);
    }
}
