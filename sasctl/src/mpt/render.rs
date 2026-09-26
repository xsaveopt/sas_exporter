use anyhow::Result;
use serde::Serialize;

use super::cli::{DiagRead, Uploaded};
use super::diag::{DiagQuery, Event};
use super::inventory::{
    BootInfo, ControllerInfo, ControllerTemperature, Drive, DriveList, EnclosureList, FirmwareInfo,
    LogInfo, PhyErrorList, PhyList, Volume, VolumeList, VolumeStatus, VolumeStatusList,
};
use super::pages::BootDevice;
use crate::output::{Fields, Render, Table};

pub use crate::output::Emit;

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

impl Render for ControllerInfo {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Controller {}", self.index));
        f.add("Chip", &self.chip)
            .add("Generation", self.generation)
            .add("Chip revision", opt(&self.chip_revision))
            .add("Board", opt(&self.board_name))
            .add("Board assembly", opt(&self.board_assembly))
            .add("Board tracer", opt(&self.board_tracer_number))
            .add("SAS address", opt(&self.sas_address))
            .add("Firmware version", &self.firmware_version)
            .add("BIOS version", &self.bios_version)
            .add("NVDATA default", opt(&self.nvdata_version_default))
            .add("NVDATA persistent", opt(&self.nvdata_version_persistent))
            .add("MPI version", &self.mpi_version)
            .add("Driver", &self.driver_version)
            .add("Adapter type", self.adapter_type)
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
            .add("IOC number", self.ioc_number)
            .add("Ports", self.ports)
            .add("Max targets", self.max_targets)
            .add("Max enclosures", self.max_enclosures)
            .add("Max volumes", self.max_volumes)
            .add("Concurrent commands", self.concurrent_commands)
            .add("Product id", format!("0x{:04x}", self.product_id))
            .add("RAID support", yes_no(self.raid_support))
            .add("Capabilities", self.capabilities.join(", "));
        if self.ioc_exceptions != 0 {
            f.add("IOC exceptions", format!("0x{:04x}", self.ioc_exceptions));
        }
        if let Some(l) = &self.raid_limits {
            f.add(
                "RAID0 drives",
                format!("{} to {}", l.min_drives_raid0, l.max_drives_raid0),
            )
            .add(
                "RAID1 drives",
                format!("{} to {}", l.min_drives_raid1, l.max_drives_raid1),
            )
            .add(
                "RAID1E drives",
                format!("{} to {}", l.min_drives_raid1e, l.max_drives_raid1e),
            )
            .add(
                "RAID10 drives",
                format!("{} to {}", l.min_drives_raid10, l.max_drives_raid10),
            )
            .add("Max IR volumes", l.max_volumes)
            .add("Max IR disks", l.max_phys_disks)
            .add("Max hot spares", l.max_global_hot_spares);
        }
        f.render(out);
    }
}

fn size_text(d: &Drive) -> String {
    match (d.size_mb, d.last_lba) {
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
            "Address", "Handle", "Type", "State", "Size MB", "Model", "Serial", "Temp",
        ]);
        for d in &self.drives {
            t.row([
                d.address.clone(),
                format!("0x{:04x}", d.handle),
                d.drive_type.clone().unwrap_or_else(|| d.kind.to_string()),
                d.state.to_string(),
                opt(&d.size_mb),
                opt(&d.model),
                opt(&d.serial_number),
                d.temperature
                    .map_or_else(|| "-".to_string(), |x| format!("{}C", x.celsius)),
            ]);
        }
        render_table(&t, "No drives found", out);
    }
}

impl Render for Drive {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Drive {}", self.address));
        f.add("Device", self.kind)
            .add("Enclosure", self.enclosure)
            .add("Slot", self.slot)
            .add("Handle", format!("0x{:04x}", self.handle))
            .add("SAS address", &self.sas_address)
            .add("Device name", &self.device_name)
            .add("Phy", self.phy)
            .add("State", self.state)
            .add("RAID disk number", opt(&self.phys_disk_num))
            .add("Linux channel:target", opt(&self.linux_channel_target))
            .add("Size", size_text(self))
            .add("Block size", opt(&self.block_size))
            .add("Manufacturer", opt(&self.vendor))
            .add("Model", opt(&self.model))
            .add("Firmware revision", opt(&self.firmware_revision))
            .add("Serial number", opt(&self.serial_number))
            .add(
                "GUID",
                self.guid.clone().unwrap_or_else(|| "N/A".to_string()),
            )
            .add("Protocol", self.protocol)
            .add("Drive type", opt(&self.drive_type))
            .add("Rotation rate", opt(&self.rotation_rate))
            .add("Temperature", temperature_text(self));
        f.render(out);
    }
}

fn status_line(s: &VolumeStatus) -> String {
    let mut parts = vec![s.state.to_string()];
    if s.inactive {
        parts.insert(0, "Inactive".to_string());
    }
    parts.join(", ")
}

impl Render for VolumeList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "ID",
            "Name",
            "Level",
            "State",
            "Size MB",
            "Members",
            "Boot",
            "Operation",
        ]);
        for v in &self.volumes {
            t.row([
                v.id.to_string(),
                opt(&v.name),
                v.raid_level.to_string(),
                status_line(&v.status),
                v.size_mb.to_string(),
                v.members
                    .iter()
                    .map(|m| {
                        m.address
                            .clone()
                            .unwrap_or_else(|| format!("#{}", m.phys_disk_num))
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                opt(&v.boot),
                v.status.current_operation.to_string(),
            ]);
        }
        render_table(&t, "No volumes found", out);
    }
}

fn progress_fields(f: &mut Fields, s: &VolumeStatus) {
    f.add("Enabled", yes_no(s.enabled))
        .add(
            "Physical disk I/Os",
            if s.quiesced {
                "Quiesced"
            } else {
                "Not quiesced"
            },
        )
        .add("Current operation", s.current_operation);
    if let Some(p) = &s.progress {
        f.add("Volume size (sectors)", p.total_blocks)
            .add("Remaining sectors", p.blocks_remaining)
            .add("Percent complete", format!("{:.2}%", p.percent_complete));
        if let Some(secs) = p.elapsed_seconds {
            f.add("Elapsed seconds", secs);
        }
    }
}

impl Render for Volume {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Volume {}", self.id));
        f.add("Name", opt(&self.name))
            .add("WWID", opt(&self.wwid))
            .add("RAID level", self.raid_level)
            .add("State", status_line(&self.status))
            .add("Size", format!("{} MB", self.size_mb))
            .add("Max LBA", self.max_lba)
            .add("Block size", self.block_size)
            .add("Stripe size", self.stripe_size)
            .add("Boot", opt(&self.boot));
        progress_fields(&mut f, &self.status);
        f.render(out);
        out.push('\n');
        let mut t = Table::new(["Member", "Address", "Handle", "State"]);
        for m in &self.members {
            t.row([
                m.phys_disk_num.to_string(),
                opt(&m.address),
                m.handle
                    .map_or_else(|| "-".to_string(), |h| format!("0x{h:04x}")),
                opt(&m.state),
            ]);
        }
        t.render(out);
    }
}

impl Render for VolumeStatusList {
    fn render(&self, out: &mut String) {
        if self.volumes.is_empty() {
            out.push_str("No volumes\n");
            return;
        }
        for (i, s) in self.volumes.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let mut f = Fields::titled(format!("Volume {}", s.id));
            f.add("State", status_line(s));
            progress_fields(&mut f, s);
            f.render(out);
        }
    }
}

impl Render for EnclosureList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "ID",
            "Logical ID",
            "Slots",
            "Start",
            "Management",
            "SEP handle",
        ]);
        for e in &self.enclosures {
            t.row([
                e.id.to_string(),
                e.logical_id.clone(),
                e.num_slots.to_string(),
                e.start_slot.to_string(),
                e.management.to_string(),
                format!("0x{:04x}", e.sep_handle),
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
                    format!("0x{:04x}", p.attached_handle)
                },
                opt(&p.attached_sas_address),
                opt(&p.attached_device),
            ]);
        }
        t.render(out);
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
        t.render(out);
    }
}

impl Render for ControllerTemperature {
    fn render(&self, out: &mut String) {
        if self.sensors.is_empty() {
            out.push_str("The controller reports no temperature sensors\n");
            return;
        }
        let mut t = Table::new(["Sensor", "Celsius", "Reported"]);
        for s in &self.sensors {
            t.row([
                s.name.to_string(),
                format!("{:.1}", s.celsius),
                format!("{}{}", s.raw, s.unit),
            ]);
        }
        t.render(out);
    }
}

pub struct EventList {
    pub events: Vec<Event>,
}

impl Serialize for EventList {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.events.serialize(s)
    }
}

impl Render for EventList {
    fn render(&self, out: &mut String) {
        if self.events.is_empty() {
            out.push_str("No events logged\n");
            return;
        }
        let mut t = Table::new(["Context", "Code", "Event", "Data"]);
        for e in &self.events {
            t.row([
                e.context.to_string(),
                format!("0x{:04x}", e.code),
                e.name.to_string(),
                e.data.chars().take(48).collect::<String>(),
            ]);
        }
        t.render(out);
    }
}

pub fn boot_device_text(d: &BootDevice) -> String {
    match d {
        BootDevice::None => "none".to_string(),
        BootDevice::SasWwid { sas_address, lun } => {
            format!("SAS WWID {sas_address:016x} LUN {lun}")
        }
        BootDevice::EnclosureSlot {
            enclosure_logical_id,
            slot,
        } => format!("enclosure {enclosure_logical_id:016x} slot {slot}"),
        BootDevice::DeviceName { device_name, lun } => {
            format!("device name {device_name:016x} LUN {lun}")
        }
        BootDevice::Other { code } => format!("form 0x{code:02x}"),
    }
}

impl Render for BootInfo {
    fn render(&self, out: &mut String) {
        let mut t = Table::new(["Role", "Device", "Resolves to"]);
        for e in &self.entries {
            t.row([
                e.role.to_string(),
                boot_device_text(&e.device),
                opt(&e.resolved),
            ]);
        }
        t.render(out);
    }
}

impl Render for LogInfo {
    fn render(&self, out: &mut String) {
        if self.entries.is_empty() {
            out.push_str("No log entries\n");
            return;
        }
        let mut t = Table::new(["Sequence", "Timestamp", "Qualifier", "Data"]);
        for e in &self.entries {
            t.row([
                e.log_sequence.to_string(),
                e.time_stamp.to_string(),
                format!("0x{:04x}", e.log_entry_qualifier),
                e.log_data.clone(),
            ]);
        }
        t.render(out);
    }
}

impl Render for FirmwareInfo {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Firmware");
        f.add("Firmware version", &self.firmware_version)
            .add("BIOS version", &self.bios_version)
            .add("NVDATA default", opt(&self.nvdata_version_default))
            .add("NVDATA persistent", opt(&self.nvdata_version_persistent))
            .add("MPI version", &self.mpi_version)
            .add("Product id", format!("0x{:04x}", self.product_id))
            .add("Product type", self.product_type)
            .add("IR firmware", yes_no(self.ir_firmware));
        f.render(out);
    }
}

impl Render for Uploaded {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Upload");
        f.add("File", &self.file).add("Bytes", self.bytes);
        if let Some(h) = &self.header {
            f.add("Firmware version", &h.firmware_version)
                .add("NVDATA version", &h.nvdata_version)
                .add("Vendor id", format!("0x{:04x}", h.vendor_id))
                .add("Product id", format!("0x{:04x}", h.product_id))
                .add("Version name", &h.version_name);
        }
        f.render(out);
    }
}

impl Render for DiagQuery {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Diagnostic buffer");
        f.add("Buffer type", self.buffer_type)
            .add("Unique id", format!("0x{:08x}", self.unique_id))
            .add("Application owned", yes_no(self.app_owned))
            .add("Buffer valid", yes_no(self.buffer_valid))
            .add("Firmware access", yes_no(self.fw_buffer_access))
            .add(
                "Diagnostic flags",
                format!("0x{:08x}", self.diagnostic_flags),
            )
            .add("Total size", self.total_buffer_size)
            .add("Driver added size", self.driver_added_buffer_size);
        f.render(out);
    }
}

impl Render for DiagRead {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Diagnostic buffer read");
        f.add("File", &self.file)
            .add("Bytes", self.bytes)
            .add("Unique id", format!("0x{:08x}", self.query.unique_id));
        f.render(out);
    }
}
