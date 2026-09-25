use anyhow::{Result, bail};
use serde::Serialize;

use crate::bytes::{Le, LeMut};
use crate::mega::ctrl::get_time;
use crate::mega::dcmd::{dcmd_none, dcmd_read, dcmd_write};
use crate::mega::event::format_fw_time;
use crate::mega::mfi::{Mbox, op};
use crate::mega::transport::Transport;

pub const STATUS_LEN: usize = 16;
pub const PROPS_LEN: usize = 208;
pub const CONTINUOUS: u32 = 0xffff_ffff;

pub fn state_name(state: u8) -> String {
    match state {
        0 => "stopped".into(),
        1 => "ready".into(),
        2 => "active".into(),
        0xff => "aborted".into(),
        other => format!("{other}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Manual,
    Disabled,
}

impl Mode {
    pub fn code(self) -> u8 {
        match self {
            Mode::Auto => 0,
            Mode::Manual => 1,
            Mode::Disabled => 2,
        }
    }
}

pub fn mode_name(mode: u8) -> String {
    match mode {
        0 => "auto".into(),
        1 => "manual".into(),
        2 => "disabled".into(),
        other => format!("{other}"),
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub iterations: u32,
    pub state: u8,
    pub state_name: String,
    pub drives_done: u8,
}

impl Status {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            iterations: b.u32_at(0),
            state: b.u8_at(4),
            state_name: state_name(b.u8_at(4)),
            drives_done: b.u8_at(5),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Properties {
    #[serde(skip)]
    pub raw: Vec<u8>,
    pub mode: u8,
    pub mode_name: String,
    pub max_concurrent_drives: u8,
    pub excluded_volumes: Vec<u16>,
    pub current_drives: Vec<u16>,
    pub next_exec: u32,
    pub next_exec_time: String,
    pub exec_frequency_seconds: Option<u32>,
    pub continuous: bool,
    pub clear_frequency: u32,
}

fn bitmap(b: &[u8], off: usize) -> Vec<u16> {
    (0..256u16)
        .filter(|i| b.u8_at(off + usize::from(*i) / 8) >> (i % 8) & 1 == 1)
        .collect()
}

impl Properties {
    pub fn parse(b: &[u8]) -> Self {
        let count = usize::from(b.u8_at(3)).min(64);
        let freq = b.u32_at(200);
        Self {
            raw: b.to_vec(),
            mode: b.u8_at(0),
            mode_name: mode_name(b.u8_at(0)),
            max_concurrent_drives: b.u8_at(1),
            excluded_volumes: (0..count).map(|i| b.u16_at(4 + i * 2)).collect(),
            current_drives: bitmap(b, 132),
            next_exec: b.u32_at(196),
            next_exec_time: format_fw_time(b.u32_at(196)),
            exec_frequency_seconds: (freq != CONTINUOUS).then_some(freq),
            continuous: freq == CONTINUOUS,
            clear_frequency: b.u32_at(204),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub status: Status,
    pub properties: Properties,
    pub controller_time: Option<u32>,
    pub controller_time_text: Option<String>,
    pub next_run_in_seconds: Option<i64>,
}

pub fn report(t: &dyn Transport) -> Result<Report> {
    let status = Status::parse(&dcmd_read(t, op::PR_GET_STATUS, &Mbox::new(), STATUS_LEN)?);
    let properties = get_properties(t)?;
    let now = get_time(t).ok();
    Ok(Report {
        next_run_in_seconds: now.map(|n| i64::from(properties.next_exec) - i64::from(n)),
        controller_time_text: now.map(format_fw_time),
        controller_time: now,
        status,
        properties,
    })
}

pub fn get_properties(t: &dyn Transport) -> Result<Properties> {
    Ok(Properties::parse(&dcmd_read(
        t,
        op::PR_GET_PROPERTIES,
        &Mbox::new(),
        PROPS_LEN,
    )?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub mode: Mode,
    pub interval: Option<u32>,
    pub start_in: Option<u32>,
}

pub fn parse_interval(s: &str) -> Result<u32> {
    if s.eq_ignore_ascii_case("continuous") {
        return Ok(CONTINUOUS);
    }
    let v: u32 = s.parse()?;
    if v == CONTINUOUS {
        bail!("use continuous instead of {v}");
    }
    Ok(v)
}

pub fn configure(t: &dyn Transport, sched: Schedule) -> Result<Properties> {
    if sched.mode != Mode::Auto && (sched.interval.is_some() || sched.start_in.is_some()) {
        bail!("--interval and --start-in only apply to auto mode");
    }
    let current = get_properties(t)?;
    let mut raw = current.raw.clone();
    raw.resize(PROPS_LEN, 0);
    raw.put_u8(0, sched.mode.code());
    if let Some(freq) = sched.interval {
        raw.put_u32(200, freq);
    }
    if let Some(delay) = sched.start_in {
        let now = get_time(t)?;
        if now == 0 {
            bail!("controller time is not set, cannot schedule a start");
        }
        raw.put_u32(196, now.saturating_add(delay));
    }
    dcmd_write(t, op::PR_SET_PROPERTIES, &Mbox::new(), &raw)?;
    get_properties(t)
}

pub fn start(t: &dyn Transport) -> Result<()> {
    dcmd_none(t, op::PR_START, &Mbox::new())
}

pub fn stop(t: &dyn Transport) -> Result<()> {
    dcmd_none(t, op::PR_STOP, &Mbox::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mega::mock::Mock;

    fn props_bytes() -> Vec<u8> {
        let mut b = vec![0u8; PROPS_LEN];
        b[0] = 1;
        b[1] = 4;
        b[3] = 2;
        b.put_u16(4, 3);
        b.put_u16(6, 7);
        b[132] = 0b0000_0101;
        b[133] = 0b1000_0000;
        b.put_u32(196, 1000);
        b.put_u32(200, 604_800);
        b.put_u32(204, 9);
        b
    }

    #[test]
    fn parses_status_and_properties() {
        let mut s = vec![0u8; STATUS_LEN];
        s.put_u32(0, 12);
        s[4] = 2;
        s[5] = 3;
        let st = Status::parse(&s);
        assert_eq!(
            (st.iterations, st.state_name.as_str(), st.drives_done),
            (12, "active", 3)
        );
        let p = Properties::parse(&props_bytes());
        assert_eq!(p.mode_name, "manual");
        assert_eq!(p.max_concurrent_drives, 4);
        assert_eq!(p.excluded_volumes, vec![3, 7]);
        assert_eq!(p.current_drives, vec![0, 2, 15]);
        assert_eq!(p.exec_frequency_seconds, Some(604_800));
        assert!(!p.continuous);
    }

    #[test]
    fn configure_is_read_modify_write_with_controller_time() {
        let mut now = vec![0u8; 4];
        now.put_u32(0, 5000);
        let mock = Mock::new()
            .reply(op::PR_GET_PROPERTIES, props_bytes())
            .reply(op::TIME_SECS_GET, now)
            .reply(op::PR_SET_PROPERTIES, vec![]);
        configure(
            &mock,
            Schedule {
                mode: Mode::Auto,
                interval: Some(CONTINUOUS),
                start_in: Some(60),
            },
        )
        .unwrap();
        let write = mock
            .calls()
            .into_iter()
            .find(|c| c.opcode() == op::PR_SET_PROPERTIES)
            .unwrap();
        let mut expected = props_bytes();
        expected[0] = 0;
        expected.put_u32(200, CONTINUOUS);
        expected.put_u32(196, 5060);
        assert_eq!(write.bufs[0], expected);
    }

    #[test]
    fn schedule_options_need_auto_mode() {
        let mock = Mock::new();
        let r = configure(
            &mock,
            Schedule {
                mode: Mode::Manual,
                interval: Some(10),
                start_in: None,
            },
        );
        assert!(r.is_err());
        assert!(mock.calls().is_empty());
    }
}
