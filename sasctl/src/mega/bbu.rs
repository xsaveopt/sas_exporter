use anyhow::Result;
use serde::Serialize;

use crate::bytes::Le;
use crate::mega::ctrl::flags;
use crate::mega::dcmd::{dcmd_none, dcmd_read};
use crate::mega::event::format_fw_time;
use crate::mega::mfi::{Mbox, op};
use crate::mega::transport::Transport;

pub const STATUS_LEN: usize = 64;
pub const CAPACITY_LEN: usize = 48;
pub const DESIGN_LEN: usize = 67;
pub const PROPS_LEN: usize = 32;

pub const FW_STATUS_FLAGS: &[&str] = &[
    "pack missing",
    "voltage low",
    "temperature high",
    "charging",
    "discharging",
    "learn cycle requested",
    "learn cycle active",
    "learn cycle failed",
    "learn cycle timeout",
    "i2c errors detected",
];

pub fn battery_type_name(t: u8) -> String {
    match t {
        0 => "none".into(),
        1 => "iBBU".into(),
        2 => "BBU".into(),
        other => format!("unknown ({other})"),
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct IbbuDetail {
    pub gas_gauge_status: u16,
    pub relative_charge_percent: u16,
    pub charger_system_state: u16,
    pub charger_system_ctrl: u16,
    pub charging_current_ma: u16,
    pub absolute_charge_percent: u16,
    pub max_error_percent: u16,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct BbuDetail {
    pub gas_gauge_status: u16,
    pub relative_charge_percent: u16,
    pub charger_status: u16,
    pub remaining_capacity_mah: u16,
    pub full_charge_capacity_mah: u16,
    pub state_of_health_good: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub battery_type: u8,
    pub battery_type_name: String,
    pub voltage_mv: u16,
    pub current_ma: i16,
    pub temperature_celsius: u16,
    pub fw_status: u32,
    pub fw_status_flags: Vec<&'static str>,
    pub ibbu: Option<IbbuDetail>,
    pub bbu: Option<BbuDetail>,
}

impl Status {
    pub fn parse(b: &[u8]) -> Self {
        let kind = b.u8_at(0);
        Self {
            battery_type: kind,
            battery_type_name: battery_type_name(kind),
            voltage_mv: b.u16_at(2),
            current_ma: b.i16_at(4),
            temperature_celsius: b.u16_at(6),
            fw_status: b.u32_at(8),
            fw_status_flags: flags(b, 8, FW_STATUS_FLAGS),
            ibbu: (kind == 1).then(|| IbbuDetail {
                gas_gauge_status: b.u16_at(32),
                relative_charge_percent: b.u16_at(34),
                charger_system_state: b.u16_at(36),
                charger_system_ctrl: b.u16_at(38),
                charging_current_ma: b.u16_at(40),
                absolute_charge_percent: b.u16_at(42),
                max_error_percent: b.u16_at(44),
            }),
            bbu: (kind == 2).then(|| BbuDetail {
                gas_gauge_status: b.u16_at(32),
                relative_charge_percent: b.u16_at(34),
                charger_status: b.u16_at(36),
                remaining_capacity_mah: b.u16_at(38),
                full_charge_capacity_mah: b.u16_at(40),
                state_of_health_good: b.u8_at(42) != 0,
            }),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub relative_charge_percent: u16,
    pub absolute_charge_percent: u16,
    pub remaining_capacity_mah: u16,
    pub full_charge_capacity_mah: u16,
    pub run_time_to_empty_min: u16,
    pub average_time_to_empty_min: u16,
    pub average_time_to_full_min: u16,
    pub cycle_count: u16,
    pub max_error_percent: u16,
    pub remaining_capacity_alarm_mah: u16,
    pub remaining_time_alarm_min: u16,
}

impl Capacity {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            relative_charge_percent: b.u16_at(0),
            absolute_charge_percent: b.u16_at(2),
            remaining_capacity_mah: b.u16_at(4),
            full_charge_capacity_mah: b.u16_at(6),
            run_time_to_empty_min: b.u16_at(8),
            average_time_to_empty_min: b.u16_at(10),
            average_time_to_full_min: b.u16_at(12),
            cycle_count: b.u16_at(14),
            max_error_percent: b.u16_at(16),
            remaining_capacity_alarm_mah: b.u16_at(18),
            remaining_time_alarm_min: b.u16_at(20),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Design {
    pub manufacture_date_raw: u32,
    pub manufacture_date: String,
    pub design_capacity_mah: u16,
    pub design_voltage_mv: u16,
    pub spec_info: u16,
    pub serial_number: u16,
    pub pack_stat_config: u16,
    pub manufacturer: String,
    pub device_name: String,
    pub device_chemistry: String,
    pub manufacturer_data: String,
}

impl Design {
    pub fn parse(b: &[u8]) -> Self {
        let date = b.u32_at(0);
        Self {
            manufacture_date_raw: date,
            manufacture_date: format!(
                "{:04}-{:02}-{:02}",
                1980 + (date >> 9 & 0x7f),
                date >> 5 & 0x0f,
                date & 0x1f
            ),
            design_capacity_mah: b.u16_at(4),
            design_voltage_mv: b.u16_at(6),
            spec_info: b.u16_at(8),
            serial_number: b.u16_at(10),
            pack_stat_config: b.u16_at(12),
            manufacturer: b.ascii_at(14, 12),
            device_name: b.ascii_at(26, 8),
            device_chemistry: b.ascii_at(34, 8),
            manufacturer_data: b.ascii_at(42, 8),
        }
    }
}

pub fn auto_learn_mode_name(mode: u8) -> String {
    match mode {
        0 => "enabled".into(),
        1 => "disabled".into(),
        2 => "warn via event".into(),
        other => format!("{other}"),
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Properties {
    pub auto_learn_period_seconds: u32,
    pub next_learn_time: u32,
    pub next_learn: String,
    pub learn_delay_interval_hours: u8,
    pub auto_learn_mode: u8,
    pub auto_learn_mode_name: String,
    pub bbu_mode: u8,
}

impl Properties {
    pub fn parse(b: &[u8]) -> Self {
        Self {
            auto_learn_period_seconds: b.u32_at(0),
            next_learn_time: b.u32_at(4),
            next_learn: format_fw_time(b.u32_at(4)),
            learn_delay_interval_hours: b.u8_at(8),
            auto_learn_mode: b.u8_at(9),
            auto_learn_mode_name: auto_learn_mode_name(b.u8_at(9)),
            bbu_mode: b.u8_at(10),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub kind: &'static str,
    pub present_in_controller_info: bool,
    pub status: Option<Status>,
    pub capacity: Option<Capacity>,
    pub design: Option<Design>,
    pub properties: Option<Properties>,
    pub error: Option<String>,
}

pub fn report(t: &dyn Transport, kind: &'static str, present: bool) -> Report {
    let status =
        dcmd_read(t, op::BBU_GET_STATUS, &Mbox::new(), STATUS_LEN).map(|b| Status::parse(&b));
    let error = status.as_ref().err().map(|e| format!("{e:#}"));
    let status = status.ok();
    let (capacity, design, properties) = if status.is_some() {
        (
            dcmd_read(t, op::BBU_GET_CAPACITY_INFO, &Mbox::new(), CAPACITY_LEN)
                .ok()
                .map(|b| Capacity::parse(&b)),
            dcmd_read(t, op::BBU_GET_DESIGN_INFO, &Mbox::new(), DESIGN_LEN)
                .ok()
                .map(|b| Design::parse(&b)),
            dcmd_read(t, op::BBU_GET_PROP, &Mbox::new(), PROPS_LEN)
                .ok()
                .map(|b| Properties::parse(&b)),
        )
    } else {
        (None, None, None)
    };
    let mut status = status;
    if kind == "cachevault"
        && let Some(s) = status.as_mut()
    {
        s.ibbu = None;
        s.bbu = None;
    }
    Report {
        kind,
        present_in_controller_info: present,
        status,
        capacity,
        design,
        properties,
        error,
    }
}

pub fn start_learn(t: &dyn Transport) -> Result<()> {
    dcmd_none(t, op::BBU_START_LEARN, &Mbox::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::LeMut;
    use crate::mega::mock::Mock;

    fn status_bytes(kind: u8) -> Vec<u8> {
        let mut b = vec![0u8; STATUS_LEN];
        b[0] = kind;
        b.put_u16(2, 4012);
        b.put_u16(4, (-12i16) as u16);
        b.put_u16(6, 31);
        b.put_u32(8, 1 << 3 | 1 << 6);
        b.put_u16(34, 97);
        b.put_u16(38, 1100);
        b.put_u16(40, 1200);
        b[42] = 1;
        b
    }

    #[test]
    fn parses_status_by_battery_type() {
        let s = Status::parse(&status_bytes(2));
        assert_eq!(s.battery_type_name, "BBU");
        assert_eq!(s.voltage_mv, 4012);
        assert_eq!(s.current_ma, -12);
        assert_eq!(s.temperature_celsius, 31);
        assert_eq!(s.fw_status_flags, vec!["charging", "learn cycle active"]);
        let d = s.bbu.unwrap();
        assert_eq!(d.relative_charge_percent, 97);
        assert_eq!(d.full_charge_capacity_mah, 1200);
        assert!(d.state_of_health_good);
        assert!(s.ibbu.is_none());
        let s = Status::parse(&status_bytes(1));
        assert_eq!(s.ibbu.unwrap().charger_system_ctrl, 1100);
        assert_eq!(
            Status::parse(&status_bytes(9)).battery_type_name,
            "unknown (9)"
        );
    }

    #[test]
    fn parses_capacity_design_and_properties() {
        let mut c = vec![0u8; CAPACITY_LEN];
        c.put_u16(0, 88);
        c.put_u16(14, 42);
        let cap = Capacity::parse(&c);
        assert_eq!((cap.relative_charge_percent, cap.cycle_count), (88, 42));

        let mut d = vec![0u8; DESIGN_LEN];
        d.put_u32(0, (44 << 9) | (7 << 5) | 15);
        d.put_u16(4, 1215);
        d[14..20].copy_from_slice(b"LSI\0\0\0");
        d[34..38].copy_from_slice(b"LION");
        let des = Design::parse(&d);
        assert_eq!(des.manufacture_date, "2024-07-15");
        assert_eq!(des.design_capacity_mah, 1215);
        assert_eq!(des.manufacturer, "LSI");
        assert_eq!(des.device_chemistry, "LION");

        let mut p = vec![0u8; PROPS_LEN];
        p.put_u32(0, 2_419_200);
        p.put_u32(4, 0);
        p[8] = 5;
        p[9] = 2;
        let props = Properties::parse(&p);
        assert_eq!(props.auto_learn_period_seconds, 2_419_200);
        assert_eq!(props.next_learn, "2000-01-01 00:00:00");
        assert_eq!(props.auto_learn_mode_name, "warn via event");
    }

    #[test]
    fn report_handles_a_missing_battery() {
        let mock = Mock::new().status(op::BBU_GET_STATUS, 0x22);
        let r = report(&mock, "bbu", false);
        assert!(r.status.is_none());
        assert!(r.error.unwrap().contains("MFI_STAT_NO_HW_PRESENT"));
        assert_eq!(mock.opcodes(), vec![op::BBU_GET_STATUS]);
    }

    #[test]
    fn cachevault_report_drops_the_battery_detail() {
        let mock = Mock::new().reply(op::BBU_GET_STATUS, status_bytes(2));
        let r = report(&mock, "cachevault", true);
        let s = r.status.unwrap();
        assert!(s.bbu.is_none() && s.ibbu.is_none());
        assert_eq!(s.temperature_celsius, 31);
    }
}
