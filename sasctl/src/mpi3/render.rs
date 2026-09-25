use anyhow::Result;
use serde::Serialize;

use super::event::EventLog;
use super::inventory::{
    AdapterList, ControllerInfo, ControllerTemperature, Drive, DriveList, DriveSmart,
    EnclosureList, FirmwareInfo, PhyErrorList, PhyList, Volume, VolumeList,
};
use super::pages::Sensor;
use crate::output::{self, Fields, Format, Render, Table};

pub trait Emit {
    fn emit_to(&self, format: Format) -> Result<()>;
    fn text(&self) -> String;
}

impl<T: Serialize + Render> Emit for T {
    fn emit_to(&self, format: Format) -> Result<()> {
        match format {
            Format::Text => {
                print!("{}", self.text());
                Ok(())
            }
            Format::Json => output::emit(format, self),
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out
    }
}

fn opt<T: ToString>(v: &Option<T>) -> String {
    v.as_ref()
        .map_or_else(|| "-".to_string(), ToString::to_string)
}

fn render_table(t: &Table, empty: &str, out: &mut String) {
    if t.is_empty() {
        out.push_str(empty);
        out.push('\n');
    } else {
        t.render(out);
    }
}

fn yes_no(v: bool) -> &'static str {
    if v { "Yes" } else { "No" }
}

fn handle(h: u16) -> String {
    format!("0x{h:04x}")
}

impl Render for AdapterList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "ID",
            "Chip",
            "Device",
            "SubVendor",
            "SubDevice",
            "PCI",
            "Host",
            "State",
            "Personality",
            "Firmware",
        ]);
        for a in &self.adapters {
            t.row([
                a.index.to_string(),
                a.chip.clone(),
                format!("{:04x}", a.device_id),
                format!("{:04x}", a.subsystem_vendor_id),
                format!("{:04x}", a.subsystem_device_id),
                a.pci_address.clone(),
                format!("host{}", a.host),
                a.state.to_string(),
                opt(&a.personality),
                opt(&a.firmware_version),
            ]);
        }
        render_table(&t, "No mpi3mr controllers found", out);
    }
}

fn sensor_table(sensors: &[Sensor], out: &mut String) {
    let mut t = Table::new(["Sensor", "Location", "Source", "Valid", "Raw reading"]);
    for s in sensors {
        t.row([
            s.index.to_string(),
            s.location.to_string(),
            if s.internal {
                "IOC".to_string()
            } else {
                format!("ISTWI {} channel {}", s.istwi_index, s.channel)
            },
            yes_no(s.valid).to_string(),
            if s.valid {
                s.raw.to_string()
            } else {
                "-".to_string()
            },
        ]);
    }
    render_table(&t, "The controller reports no temperature sensors", out);
}

impl Render for ControllerInfo {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Controller {}", self.index));
        f.add("Chip", &self.chip)
            .add("Chip revision", opt(&self.chip_revision))
            .add("Product", opt(&self.product_name))
            .add("Board", opt(&self.board_name))
            .add("Board assembly", opt(&self.board_assembly))
            .add("Board tracer", opt(&self.board_tracer_number))
            .add("Board revision", opt(&self.board_revision))
            .add("Board manufactured", opt(&self.board_mfg_date))
            .add("State", self.state)
            .add("Personality", self.personality)
            .add("RAID support", yes_no(self.raid_supported))
            .add("Firmware version", &self.firmware_version)
            .add("Package version", opt(&self.package_version))
            .add("MPI version", &self.mpi_version)
            .add(
                "Driver",
                format!("{} {}", self.driver_name, self.driver_version),
            )
            .add("PCI address", &self.pci_address)
            .add(
                "PCI ids",
                format!(
                    "{}:{:04x} rev {:02x}, subsystem {:04x}:{:04x}",
                    self.vendor_id
                        .map_or_else(|| "-".to_string(), |v| format!("{v:04x}")),
                    self.device_id,
                    self.revision,
                    self.subsystem_vendor_id,
                    self.subsystem_device_id
                ),
            )
            .add("SCSI host", format!("host{}", self.host))
            .add("SAS transport", yes_no(self.sas_transport))
            .add("Protocols", self.protocols.join(", "))
            .add("Capabilities", self.capabilities.join(", "));
        if self.ioc_exceptions != 0 {
            f.add("IOC exceptions", format!("0x{:04x}", self.ioc_exceptions));
        }
        let l = &self.limits;
        f.add("Max virtual disks", l.max_vds)
            .add("Max RAID drives", l.max_raid_pds)
            .add("Max host drives", l.max_host_pds)
            .add("Max advanced host drives", l.max_adv_host_pds)
            .add("Max NVMe drives", l.max_nvme)
            .add("Max SAS initiators", l.max_sas_initiators)
            .add("Max SAS expanders", l.max_sas_expanders)
            .add("Max enclosures", l.max_enclosures)
            .add("Max PCIe switches", l.max_pcie_switches)
            .add("Max outstanding requests", l.max_outstanding_requests);
        let d = &self.devices;
        f.add("Drives", d.drives)
            .add("Virtual disks", d.virtual_disks)
            .add("Expanders", d.expanders)
            .add("Enclosures", d.enclosures)
            .add(
                "Devices by form",
                format!(
                    "{} SAS/SATA, {} PCIe, {} VD",
                    d.sas_sata, d.pcie, d.virtual_disks
                ),
            );
        f.render(out);
        out.push_str("\nTemperature sensors, raw readings in an undocumented unit\n");
        sensor_table(&self.sensors, out);
    }
}

fn size_text(size_mb: Option<u64>, last_lba: Option<u64>) -> String {
    match (size_mb, last_lba) {
        (Some(mb), Some(lba)) => format!("{mb} MB ({lba} last LBA)"),
        _ => "-".to_string(),
    }
}

fn temperature_text(d: &Drive) -> String {
    d.temperature.map_or_else(
        || "-".to_string(),
        |t| format!("{}C ({:.2}F)", t.celsius, t.fahrenheit),
    )
}

impl Render for DriveList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "Address", "Handle", "Type", "State", "Size MB", "Model", "Serial", "Temp", "OS",
        ]);
        for d in &self.drives {
            t.row([
                d.address.clone(),
                handle(d.handle),
                d.drive_type
                    .clone()
                    .unwrap_or_else(|| d.protocol.to_string()),
                d.state.to_string(),
                opt(&d.size_mb),
                opt(&d.model),
                opt(&d.serial_number),
                d.temperature
                    .map_or_else(|| "-".to_string(), |x| format!("{}C", x.celsius)),
                opt(&d.os_device),
            ]);
        }
        render_table(&t, "No drives found", out);
    }
}

impl Render for Drive {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Drive {}", self.address));
        f.add("Enclosure", self.enclosure)
            .add("Enclosure logical id", opt(&self.enclosure_logical_id))
            .add("Slot", self.slot)
            .add("Handle", handle(self.handle))
            .add("Persistent id", self.persistent_id)
            .add("Protocol", self.protocol)
            .add("Drive type", opt(&self.drive_type))
            .add("State", self.state)
            .add("Exposed to OS", yes_no(self.exposed))
            .add("Linux channel:target", opt(&self.linux_channel_target))
            .add("OS device", opt(&self.os_device))
            .add("WWID", &self.wwid)
            .add("SAS address", opt(&self.sas_address))
            .add("Phy", opt(&self.phy))
            .add("Link rate", opt(&self.link_rate))
            .add("Size", size_text(self.size_mb, self.last_lba))
            .add("Block size", opt(&self.block_size))
            .add("Manufacturer", opt(&self.vendor))
            .add("Model", opt(&self.model))
            .add("Firmware revision", opt(&self.firmware_revision))
            .add("Serial number", opt(&self.serial_number))
            .add("GUID", opt(&self.guid))
            .add("Rotation rate", opt(&self.rotation_rate))
            .add("Temperature", temperature_text(self));
        f.render(out);
    }
}

impl Render for DriveSmart {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("SMART {}", self.address));
        f.add("Protocol", self.protocol)
            .add("Healthy", yes_no(self.healthy))
            .add(
                "Temperature",
                self.temperature
                    .map_or_else(|| "-".to_string(), |t| format!("{}C", t.celsius)),
            );
        if let Some(ie) = &self.informational_exceptions {
            f.add(
                "Informational exception",
                format!("ASC 0x{:02x} ASCQ 0x{:02x}", ie.asc, ie.ascq),
            )
            .add("Failure predicted", yes_no(ie.failure_predicted()));
        }
        if let Some(n) = &self.nvme {
            f.add("Critical warning", format!("0x{:02x}", n.critical_warning))
                .add(
                    "Warnings",
                    if self.nvme_warnings.is_empty() {
                        "none".to_string()
                    } else {
                        self.nvme_warnings.join(", ")
                    },
                )
                .add("Available spare", format!("{}%", n.available_spare))
                .add(
                    "Spare threshold",
                    format!("{}%", n.available_spare_threshold),
                )
                .add("Percentage used", format!("{}%", n.percentage_used))
                .add("Power cycles", n.power_cycles)
                .add("Power on hours", n.power_on_hours)
                .add("Unsafe shutdowns", n.unsafe_shutdowns)
                .add("Media errors", n.media_errors)
                .add("Error log entries", n.error_log_entries);
        }
        f.render(out);
    }
}

impl Render for VolumeList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new(["ID", "Handle", "Level", "State", "Size MB", "Media", "OS"]);
        for v in &self.volumes {
            t.row([
                v.id.to_string(),
                handle(v.handle),
                v.raid_level.clone(),
                v.state.to_string(),
                opt(&v.size_mb),
                v.media.join(" "),
                opt(&v.os_device),
            ]);
        }
        render_table(&t, "No virtual disks found", out);
    }
}

impl Render for Volume {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Virtual disk {}", self.id));
        f.add("Handle", handle(self.handle))
            .add("RAID level", &self.raid_level)
            .add("State", self.state)
            .add("Access status", self.access_status)
            .add("Media", self.media.join(", "))
            .add("OS exposure hint", self.os_exposure_hint)
            .add("WWID", &self.wwid)
            .add("Linux channel:target", opt(&self.linux_channel_target))
            .add("OS device", opt(&self.os_device))
            .add("Size", size_text(self.size_mb, self.last_lba))
            .add("Block size", opt(&self.block_size))
            .add("IO throttle group", self.io_throttle_group)
            .add(
                "IO throttle watermarks",
                format!(
                    "{} MiB low, {} MiB high",
                    self.io_throttle_low_mib, self.io_throttle_high_mib
                ),
            )
            .add(
                "Abort timeout",
                self.abort_timeout_seconds
                    .map_or_else(|| "-".to_string(), |s| format!("{s} s")),
            )
            .add(
                "Reset timeout",
                self.reset_timeout_seconds
                    .map_or_else(|| "-".to_string(), |s| format!("{s} s")),
            );
        f.render(out);
    }
}

impl Render for EnclosureList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "ID",
            "Logical ID",
            "Slots",
            "Type",
            "Management",
            "SEP handle",
            "Vendor",
            "Product",
        ]);
        for e in &self.enclosures {
            t.row([
                e.id.to_string(),
                e.logical_id.clone(),
                e.num_slots.to_string(),
                e.enclosure_type.to_string(),
                e.management.to_string(),
                e.sep_handle.map_or_else(|| "-".to_string(), handle),
                opt(&e.vendor),
                opt(&e.product),
            ]);
        }
        render_table(&t, "No enclosures found", out);
    }
}

impl Render for PhyList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "Phy",
            "Port",
            "Enabled",
            "Link",
            "HW max",
            "Attached",
            "SAS address",
            "Device",
        ]);
        for p in &self.phys {
            t.row([
                p.phy.to_string(),
                p.port.to_string(),
                yes_no(p.enabled).to_string(),
                p.link_rate.to_string(),
                opt(&p.hw_max_rate),
                if p.attached_handle == 0 {
                    "-".to_string()
                } else {
                    handle(p.attached_handle)
                },
                opt(&p.attached_sas_address),
                opt(&p.attached_device),
            ]);
        }
        render_table(&t, "The controller reports no phys", out);
    }
}

impl Render for PhyErrorList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "Phy",
            "Invalid dwords",
            "Disparity errors",
            "Loss of dword sync",
            "Reset problems",
        ]);
        for p in &self.phys {
            match &p.counters {
                Some(c) => t.row([
                    p.phy.to_string(),
                    c.invalid_dword_count.to_string(),
                    c.running_disparity_error_count.to_string(),
                    c.loss_dword_synch_count.to_string(),
                    c.phy_reset_problem_count.to_string(),
                ]),
                None => t.row([
                    p.phy.to_string(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                ]),
            }
        }
        render_table(&t, "The controller reports no phys", out);
    }
}

impl Render for ControllerTemperature {
    fn render(&self, out: &mut String) {
        out.push_str("Raw readings, the unit is not documented\n\n");
        sensor_table(&self.sensors, out);
        if self.drives.is_empty() {
            return;
        }
        out.push('\n');
        let mut t = Table::new(["Address", "Handle", "Persistent id", "Raw reading"]);
        for d in &self.drives {
            t.row([
                opt(&d.address),
                handle(d.handle),
                d.persistent_id.to_string(),
                d.raw.to_string(),
            ]);
        }
        t.render(out);
    }
}

impl Render for EventLog {
    fn render(&self, out: &mut String) {
        let s = &self.sequence;
        out.push_str(&format!(
            "Sequence numbers: oldest {}, newest {}, boot {}, shutdown {}, clear {}\n\n",
            s.oldest, s.newest, s.boot, s.shutdown, s.clear
        ));
        if self.events.is_empty() {
            out.push_str("No events logged\n");
            return;
        }
        let mut t = Table::new(["Sequence", "Timestamp", "Class", "Locale", "Code", "Data"]);
        for e in &self.events {
            t.row([
                e.sequence.to_string(),
                e.time_stamp.to_string(),
                e.class_name.to_string(),
                if e.locale_names.is_empty() {
                    format!("0x{:04x}", e.locale)
                } else {
                    e.locale_names.join(",")
                },
                format!("0x{:04x}", e.log_code),
                e.info.chars().take(48).collect::<String>(),
            ]);
        }
        t.render(out);
    }
}

impl Render for FirmwareInfo {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Firmware");
        f.add("Firmware version", &self.firmware_version)
            .add("Package version", opt(&self.package_version))
            .add("Package release", opt(&self.package_release_level))
            .add("Package ids", opt(&self.package_ids))
            .add("NVDATA default", opt(&self.nvdata_version_default))
            .add("NVDATA persistent", opt(&self.nvdata_version_persistent))
            .add("MPI version", &self.mpi_version)
            .add("Product id", format!("0x{:04x}", self.product_id))
            .add("Personality", self.personality);
        f.render(out);
    }
}
