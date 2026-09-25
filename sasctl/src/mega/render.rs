use std::fmt::Display;

use crate::mega::bbu;
use crate::mega::config::ConfigData;
use crate::mega::ctrl::{Component, CtrlProps};
use crate::mega::event::LogInfo;
use crate::mega::fw::FirmwareInfo;
use crate::mega::ld::LdProps;
use crate::mega::patrol;
use crate::mega::pd::Progress;
use crate::mega::report::*;
use crate::output::{Fields, Render, Table};

fn opt<T: Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
}

fn celsius(v: Option<u8>) -> String {
    v.map(|v| format!("{v} C"))
        .unwrap_or_else(|| "absent".into())
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn list(v: &[&str]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(", ")
    }
}

fn duration(secs: u64) -> String {
    format!(
        "{}h {:02}m {:02}s",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

pub fn progress_text(p: &Option<Progress>) -> String {
    match p {
        None => "not running".into(),
        Some(p) => match p.remaining_seconds {
            Some(r) => format!(
                "{:.2}% after {}, about {} left",
                p.percent,
                duration(u64::from(p.elapsed_seconds)),
                duration(r)
            ),
            None => format!(
                "{:.2}% after {}",
                p.percent,
                duration(u64::from(p.elapsed_seconds))
            ),
        },
    }
}

fn blank(out: &mut String) {
    out.push('\n');
}

fn components(title: &str, items: &[Component], out: &mut String) {
    if items.is_empty() {
        return;
    }
    blank(out);
    out.push_str(title);
    out.push_str(":\n");
    let mut t = Table::new(["Name", "Version", "Build date", "Build time"]);
    for c in items {
        t.row([&c.name, &c.version, &c.build_date, &c.build_time]);
    }
    t.render(out);
}

impl Render for ControllerList {
    fn render(&self, out: &mut String) {
        if self.controllers.is_empty() {
            out.push_str("No MegaRAID controllers found\n");
            return;
        }
        let mut t = Table::new([
            "Ctl", "Host", "PCI", "Model", "Serial", "Firmware", "VDs", "PDs", "ROC",
        ]);
        for c in &self.controllers {
            t.row([
                c.index.to_string(),
                c.host_no.to_string(),
                opt(c.pci_address.as_ref()),
                c.product_name
                    .clone()
                    .or(c.error.clone())
                    .unwrap_or_default(),
                opt(c.serial_number.as_ref()),
                opt(c.package_version.as_ref()),
                opt(c.volumes),
                opt(c.drives),
                c.roc_celsius
                    .map(|v| format!("{v} C"))
                    .unwrap_or_else(|| "-".into()),
            ]);
        }
        t.render(out);
    }
}

impl Render for ControllerSummary {
    fn render(&self, out: &mut String) {
        let i = &self.info;
        let mut f = Fields::titled(format!("Controller {}", self.index));
        f.add("Model", &i.product_name)
            .add("Serial number", &i.serial_number)
            .add("SCSI host", self.host_no)
            .add("PCI address", opt(self.pci_address.as_ref()))
            .add(
                "PCI ids",
                format!(
                    "{:04x}:{:04x} subsystem {:04x}:{:04x}",
                    i.vendor_id, i.device_id, i.sub_vendor_id, i.sub_device_id
                ),
            )
            .add("Firmware package", &i.package_version)
            .add("Firmware version", i.firmware_version())
            .add("Driver version", opt(self.driver_version.as_ref()))
            .add("Controller time", opt(self.controller_time.as_ref()))
            .add("Host interface", list(&i.host_interface))
            .add("Device interface", list(&i.device_interface))
            .add("SAS address", &i.sas_address)
            .add("Hardware present", list(&i.hw_present))
            .add("ROC temperature", celsius(i.temperatures.roc_celsius))
            .add(
                "Controller temperature",
                celsius(i.temperatures.controller_celsius),
            )
            .add(
                "Virtual drives",
                format!(
                    "{} ({} degraded, {} offline)",
                    i.ld_present, i.ld_degraded, i.ld_offline
                ),
            )
            .add(
                "Physical devices",
                format!(
                    "{} ({} disks, {} predictive failure, {} failed)",
                    i.pd_present, i.pd_disk_present, i.pd_disk_pred_failure, i.pd_disk_failed
                ),
            )
            .add("Memory size", i.memory_size)
            .add("NVRAM size", i.nvram_size)
            .add("Flash size", i.flash_size)
            .add(
                "Memory errors",
                format!(
                    "{} correctable, {} uncorrectable",
                    i.mem_correctable_errors, i.mem_uncorrectable_errors
                ),
            )
            .add("RAID levels", list(&i.raid_levels))
            .add("Adapter operations", list(&i.adapter_operations))
            .add("VD operations", list(&i.ld_operations))
            .add("PD operations", list(&i.pd_operations))
            .add(
                "Limits",
                format!(
                    "{} arms, {} spans, {} arrays, {} VDs, {} PDs",
                    i.max_arms, i.max_spans, i.max_arrays, i.max_lds, i.max_pds
                ),
            )
            .add("JBOD support", yes_no(i.support_jbod))
            .add("Extended configuration", yes_no(i.config_ext2_supported));
        f.render(out);
        components("Image components", &i.image_components, out);
        components("Pending image components", &i.pending_image_components, out);
    }
}

impl Render for CtrlProps {
    fn render(&self, out: &mut String) {
        let o = &self.on_off;
        let mut f = Fields::titled("Controller properties");
        f.add("seq_num", self.seq_num)
            .add("rebuild_rate", self.rebuild_rate)
            .add("patrol_read_rate", self.patrol_read_rate)
            .add("bgi_rate", self.bgi_rate)
            .add("cc_rate", self.cc_rate)
            .add("recon_rate", self.recon_rate)
            .add("cache_flush_interval", self.cache_flush_interval)
            .add("spinup_drive_count", self.spinup_drive_count)
            .add("spinup_delay", self.spinup_delay)
            .add("cluster_enable", self.cluster_enable)
            .add("coercion_mode", self.coercion_mode)
            .add("alarm_enable", self.alarm_enable)
            .add("disable_auto_rebuild", self.disable_auto_rebuild)
            .add("disable_battery_warn", self.disable_battery_warn)
            .add("ecc_bucket_size", self.ecc_bucket_size)
            .add("ecc_bucket_leak_rate", self.ecc_bucket_leak_rate)
            .add(
                "restore_hotspare_on_insertion",
                self.restore_hotspare_on_insertion,
            )
            .add("expose_encl_devices", self.expose_encl_devices)
            .add("maintain_pd_fail_history", self.maintain_pd_fail_history)
            .add(
                "disallow_host_request_reordering",
                self.disallow_host_request_reordering,
            )
            .add("abort_cc_on_error", self.abort_cc_on_error)
            .add("load_balance_mode", self.load_balance_mode)
            .add(
                "disable_auto_detect_backplane",
                self.disable_auto_detect_backplane,
            )
            .add("snap_vd_space", self.snap_vd_space)
            .add("pred_fail_poll_interval", self.pred_fail_poll_interval)
            .add("intr_throttle_count", self.intr_throttle_count)
            .add("intr_throttle_timeouts", self.intr_throttle_timeouts)
            .add("copyback_disabled", o.copyback_disabled)
            .add("smarter_enabled", o.smarter_enabled)
            .add(
                "pr_correct_unconfigured_areas",
                o.pr_correct_unconfigured_areas,
            )
            .add("use_fde_only", o.use_fde_only)
            .add("disable_ncq", o.disable_ncq)
            .add("ssd_smarter_enabled", o.ssd_smarter_enabled)
            .add("ssd_patrol_read_enabled", o.ssd_patrol_read_enabled)
            .add(
                "enable_spin_down_unconfigured",
                o.enable_spin_down_unconfigured,
            )
            .add("auto_enhanced_import", o.auto_enhanced_import)
            .add("enable_secret_key_control", o.enable_secret_key_control)
            .add("disable_online_ctrl_reset", o.disable_online_ctrl_reset)
            .add(
                "allow_boot_with_pinned_cache",
                o.allow_boot_with_pinned_cache,
            )
            .add(
                "disable_spin_down_hot_spares",
                o.disable_spin_down_hot_spares,
            )
            .add("enable_jbod", o.enable_jbod)
            .add("enable_snap_dump", self.enable_snap_dump)
            .add("spin_down_time", self.spin_down_time);
        f.render(out);
    }
}

impl Render for TemperatureReport {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Controller {} temperature", self.controller));
        f.add("ROC", celsius(self.roc_celsius))
            .add("Controller", celsius(self.controller_celsius));
        f.render(out);
    }
}

impl Render for TimeReport {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Controller time");
        f.add("Controller", &self.controller_time)
            .add("Host (UTC)", &self.host_time)
            .add("Offset", format!("{} s", self.offset_seconds));
        f.render(out);
    }
}

impl Render for DriveList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "EID:Slt", "DID", "State", "DG", "Size", "Model", "Rev", "Temp",
        ]);
        for d in &self.drives {
            t.row([
                d.address.to_string(),
                d.device_id.to_string(),
                d.error.clone().unwrap_or_else(|| d.state.clone()),
                opt(d.drive_group),
                d.size.clone(),
                d.model.clone(),
                d.revision.clone(),
                d.temperature_celsius
                    .map(|v| format!("{v} C"))
                    .unwrap_or_else(|| "-".into()),
            ]);
        }
        t.render(out);
    }
}

impl Render for DriveDetail {
    fn render(&self, out: &mut String) {
        let i = &self.info;
        let mut f = Fields::titled(format!("Drive {}", self.address));
        f.add("Device id", i.device_id)
            .add("Sequence number", i.seq_num)
            .add("State", &i.state)
            .add("Drive group", opt(self.drive_group))
            .add("Vendor", &i.vendor)
            .add("Product", &i.product)
            .add("Revision", &i.revision)
            .add("Serial number", opt(self.serial_number.as_ref()))
            .add("Raw size", blocks(i.raw_blocks))
            .add("Non coerced size", blocks(i.non_coerced_blocks))
            .add("Coerced size", blocks(i.coerced_blocks))
            .add("Enclosure", i.encl_device_id)
            .add("Enclosure index", i.encl_index)
            .add("Slot", i.slot)
            .add("SAS address 0", &i.sas_addresses[0])
            .add("SAS address 1", &i.sas_addresses[1])
            .add("Paths", i.path_count)
            .add("Media errors", i.media_errors)
            .add("Other errors", i.other_errors)
            .add("Predictive failures", i.predictive_failures)
            .add(
                "Temperature",
                celsius(Some(i.temperature_celsius).filter(|v| *v != 0)),
            )
            .add("In virtual drive", yes_no(i.in_vd))
            .add("Foreign", yes_no(i.is_foreign))
            .add("Interface code", i.interface_code)
            .add("Media type code", i.media_type)
            .add("Device speed code", i.device_speed)
            .add("Link speed code", i.link_speed)
            .add("Power state code", i.power_state)
            .add("NCQ", yes_no(i.ncq))
            .add("Write cache", yes_no(i.write_cache_enabled))
            .add("FDE capable", yes_no(i.security.fde_capable))
            .add("Secured", yes_no(i.security.secured))
            .add("Locked", yes_no(i.security.locked))
            .add("Rebuild", progress_text(&i.progress.rebuild))
            .add("Patrol read", progress_text(&i.progress.patrol))
            .add("Clear", progress_text(&i.progress.clear))
            .add("Copyback active", yes_no(i.progress.copyback_active))
            .add("Locate active", yes_no(i.progress.locate_active));
        f.render(out);
    }
}

impl Render for RebuildProgress {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Drive {} rebuild", self.address));
        f.add("State", &self.state)
            .add("Rebuild", progress_text(&self.rebuild));
        f.render(out);
    }
}

impl Render for DriveSmart {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Drive {} health", self.address));
        f.add("Media errors", self.media_errors)
            .add("Other errors", self.other_errors)
            .add("Predictive failures", self.predictive_failures)
            .add(
                "Last predictive failure event",
                self.last_predictive_failure_event,
            )
            .add("SMART alert", yes_no(self.smart_alert));
        match (&self.informational_exceptions, &self.passthrough_error) {
            (Some(ie), _) => {
                f.add(
                    "Drive reports",
                    if ie.failure_predicted {
                        format!(
                            "failure predicted (asc {:#04x} ascq {:#04x})",
                            ie.asc, ie.ascq
                        )
                    } else if ie.asc == 0 {
                        "OK".to_string()
                    } else {
                        format!("asc {:#04x} ascq {:#04x}", ie.asc, ie.ascq)
                    },
                );
            }
            (None, Some(e)) => {
                f.add("Drive reports", format!("unavailable: {e}"));
            }
            (None, None) => {}
        }
        f.render(out);
        match (&self.ata_smart, &self.ata_error) {
            (Some(s), _) => {
                blank(out);
                let mut h = Fields::titled("ATA SMART");
                h.add("Health from attributes", s.health)
                    .add(
                        "Data checksum",
                        if s.data_checksum_ok { "ok" } else { "bad" },
                    )
                    .add(
                        "Threshold checksum",
                        if s.thresholds_checksum_ok {
                            "ok"
                        } else {
                            "bad"
                        },
                    );
                h.render(out);
                blank(out);
                let mut t = Table::new([
                    "ID",
                    "Flags",
                    "Type",
                    "Value",
                    "Worst",
                    "Thresh",
                    "Raw",
                    "When failed",
                ]);
                for a in &s.attributes {
                    t.row([
                        a.id.to_string(),
                        format!("{:#06x}", a.flags),
                        if a.prefailure { "Pre-fail" } else { "Old_age" }.to_string(),
                        format!("{:03}", a.value),
                        format!("{:03}", a.worst),
                        opt(a.threshold.map(|v| format!("{v:03}"))),
                        a.raw.to_string(),
                        if a.failing_now {
                            "FAILING_NOW"
                        } else if a.failed_in_past {
                            "In_the_past"
                        } else {
                            "-"
                        }
                        .to_string(),
                    ]);
                }
                t.render(out);
            }
            (None, Some(e)) => {
                blank(out);
                out.push_str(&format!("ATA SMART unavailable: {e}\n"));
            }
            (None, None) => {}
        }
    }
}

impl Render for ClearProgress {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Drive {} clear", self.address));
        f.add("State", &self.state)
            .add("Clear", progress_text(&self.clear));
        f.render(out);
    }
}

impl Render for DriveTemperature {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Drive {} temperature", self.address));
        f.add(
            "Controller reported",
            celsius(self.controller_reported_celsius),
        );
        match (&self.log_page, &self.passthrough_error) {
            (Some(l), _) => {
                f.add("Drive current", celsius(l.current_celsius))
                    .add("Drive reference", celsius(l.reference_celsius));
            }
            (None, Some(e)) => {
                f.add("Drive log page", format!("unavailable: {e}"));
            }
            (None, None) => {}
        }
        f.render(out);
    }
}

impl Render for DriveTemperatures {
    fn render(&self, out: &mut String) {
        let mut t = Table::new(["EID:Slt", "DID", "Temp"]);
        for d in &self.drives {
            t.row([
                d.address.to_string(),
                d.device_id.to_string(),
                celsius(d.celsius),
            ]);
        }
        t.render(out);
    }
}

impl Render for VolumeList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new([
            "VD", "DG", "Type", "State", "Access", "Cache", "sCC", "Size", "Name",
        ]);
        for v in &self.volumes {
            t.row([
                v.target_id.to_string(),
                opt(v.drive_group),
                v.raid.clone(),
                v.state.clone(),
                v.access.clone(),
                v.cache.clone(),
                v.disk_cache.clone(),
                v.size.clone(),
                v.name.clone(),
            ]);
        }
        t.render(out);
    }
}

impl Render for VolumeDetail {
    fn render(&self, out: &mut String) {
        let c = &self.info.config;
        let p = &c.properties;
        let mut f = Fields::titled(format!("Virtual drive {}", self.target_id));
        f.add("Name", &p.name)
            .add("RAID", &c.params.raid)
            .add("State", &c.params.state_name)
            .add("Size", blocks(self.info.size_blocks))
            .add("Strip size", human_bytes(c.params.stripe_size_bytes))
            .add("Drives per span", c.params.drives_per_span)
            .add("Span depth", c.params.span_depth)
            .add("Drive group", opt(self.drive_group))
            .add("Cache", &p.cache)
            .add("Access", &p.access)
            .add("Disk cache", &p.disk_cache)
            .add("Background init disabled", yes_no(p.no_bgi))
            .add("Consistent", yes_no(c.params.is_consistent))
            .add(
                "Consistency check",
                progress_text(&self.info.progress.consistency_check),
            )
            .add(
                "Background init",
                progress_text(&self.info.progress.background_init),
            )
            .add(
                "Foreground init",
                progress_text(&self.info.progress.foreground_init),
            )
            .add(
                "Reconstruction",
                progress_text(&self.info.progress.reconstruction),
            );
        f.render(out);
        if !self.members.is_empty() {
            blank(out);
            let mut t = Table::new(["EID:Slt", "DID", "State"]);
            for m in &self.members {
                t.row([opt(m.address), m.device_id.to_string(), m.state.clone()]);
            }
            t.render(out);
        }
    }
}

impl Render for LdProps {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Virtual drive {} properties", self.target_id));
        f.add("Name", &self.name)
            .add("Cache", &self.cache)
            .add(
                "Default cache policy",
                format!("{:#04x}", self.default_cache_policy),
            )
            .add(
                "Current cache policy",
                format!("{:#04x}", self.current_cache_policy),
            )
            .add("Access", &self.access)
            .add("Disk cache", &self.disk_cache)
            .add("Background init disabled", yes_no(self.no_bgi));
        f.render(out);
    }
}

impl Render for VolumeProgress {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("Virtual drive {} progress", self.target_id));
        for (name, p) in &self.operations {
            f.add(name.replace('_', " "), progress_text(p));
        }
        f.render(out);
    }
}

fn render_config(cfg: &ConfigData, out: &mut String) {
    let mut t = Table::new(["DG", "Array", "Size", "Drives", "Members"]);
    for (i, a) in cfg.arrays.iter().enumerate() {
        let members: Vec<String> = a
            .members
            .iter()
            .map(|m| {
                if m.missing {
                    "missing".to_string()
                } else {
                    format!("{}({})", m.device_id, m.state)
                }
            })
            .collect();
        t.row([
            i.to_string(),
            a.array_ref.to_string(),
            blocks(a.size_blocks),
            a.num_drives.to_string(),
            members.join(" "),
        ]);
    }
    out.push_str("Arrays:\n");
    t.render(out);
    blank(out);
    let mut t = Table::new(["VD", "Type", "State", "Spans", "Cache", "Name"]);
    for v in &cfg.volumes {
        let spans: Vec<String> = v.spans.iter().map(|s| s.array_ref.to_string()).collect();
        t.row([
            v.properties.target_id.to_string(),
            v.params.raid.clone(),
            v.params.state_name.clone(),
            spans.join(","),
            v.properties.cache.clone(),
            v.properties.name.clone(),
        ]);
    }
    out.push_str("Virtual drives:\n");
    t.render(out);
    if !cfg.spares.is_empty() {
        blank(out);
        let mut t = Table::new(["DID", "Type", "Arrays"]);
        for s in &cfg.spares {
            let arrays: Vec<String> = s.array_refs.iter().map(u16::to_string).collect();
            t.row([
                s.device_id.to_string(),
                if s.dedicated {
                    "dedicated".into()
                } else {
                    "global".to_string()
                },
                arrays.join(","),
            ]);
        }
        out.push_str("Hot spares:\n");
        t.render(out);
    }
}

impl Render for ConfigData {
    fn render(&self, out: &mut String) {
        render_config(self, out);
    }
}

impl Render for ForeignReport {
    fn render(&self, out: &mut String) {
        out.push_str(&format!("Foreign configurations: {}\n", self.count));
        for c in &self.configs {
            blank(out);
            out.push_str(&format!(
                "Foreign configuration {} ({}):\n",
                c.index, self.view
            ));
            match (&c.config, &c.error) {
                (Some(cfg), _) => render_config(cfg, out),
                (None, Some(e)) => out.push_str(&format!("unavailable: {e}\n")),
                (None, None) => {}
            }
        }
    }
}

impl Render for bbu::Report {
    fn render(&self, out: &mut String) {
        let title = if self.kind == "cachevault" {
            "CacheVault"
        } else {
            "Battery backup unit"
        };
        let mut f = Fields::titled(title);
        f.add(
            "Reported by controller info",
            yes_no(self.present_in_controller_info),
        );
        let Some(s) = &self.status else {
            f.add(
                "Status",
                format!("not present ({})", opt(self.error.as_ref())),
            );
            f.render(out);
            return;
        };
        f.add("Type", &s.battery_type_name)
            .add("Voltage", format!("{} mV", s.voltage_mv))
            .add("Current", format!("{} mA", s.current_ma))
            .add("Temperature", format!("{} C", s.temperature_celsius))
            .add("Firmware status", list(&s.fw_status_flags));
        if let Some(d) = &s.bbu {
            f.add("Relative charge", format!("{}%", d.relative_charge_percent))
                .add(
                    "Remaining capacity",
                    format!("{} mAh", d.remaining_capacity_mah),
                )
                .add(
                    "Full charge capacity",
                    format!("{} mAh", d.full_charge_capacity_mah),
                )
                .add(
                    "State of health",
                    if d.state_of_health_good {
                        "good"
                    } else {
                        "bad"
                    },
                );
        }
        if let Some(d) = &s.ibbu {
            f.add("Relative charge", format!("{}%", d.relative_charge_percent))
                .add("Absolute charge", format!("{}%", d.absolute_charge_percent))
                .add("Charging current", format!("{} mA", d.charging_current_ma));
        }
        if let Some(c) = &self.capacity {
            f.add(
                "Charge",
                format!(
                    "{}% relative, {}% absolute",
                    c.relative_charge_percent, c.absolute_charge_percent
                ),
            )
            .add(
                "Capacity",
                format!(
                    "{} of {} mAh",
                    c.remaining_capacity_mah, c.full_charge_capacity_mah
                ),
            )
            .add("Cycle count", c.cycle_count);
        }
        if let Some(d) = &self.design {
            f.add("Manufacturer", &d.manufacturer)
                .add("Device name", &d.device_name)
                .add("Chemistry", &d.device_chemistry)
                .add("Manufacture date", &d.manufacture_date)
                .add("Design capacity", format!("{} mAh", d.design_capacity_mah))
                .add("Design voltage", format!("{} mV", d.design_voltage_mv))
                .add("Serial number", d.serial_number);
        }
        if let Some(p) = &self.properties {
            f.add(
                "Auto learn period",
                format!("{} s", p.auto_learn_period_seconds),
            )
            .add("Next learn", &p.next_learn)
            .add("Learn delay", format!("{} h", p.learn_delay_interval_hours))
            .add("Auto learn mode", &p.auto_learn_mode_name);
        }
        f.render(out);
    }
}

impl Render for patrol::Report {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Patrol read");
        f.add("State", &self.status.state_name)
            .add("Iterations", self.status.iterations)
            .add("Drives done", self.status.drives_done);
        render_patrol_props(&self.properties, &mut f);
        f.add("Controller time", opt(self.controller_time_text.as_ref()));
        if let Some(n) = self.next_run_in_seconds {
            f.add("Next run in", format!("{n} s"));
        }
        f.render(out);
    }
}

fn render_patrol_props(p: &patrol::Properties, f: &mut Fields) {
    let excluded: Vec<String> = p.excluded_volumes.iter().map(u16::to_string).collect();
    f.add("Mode", &p.mode_name)
        .add("Max concurrent drives", p.max_concurrent_drives)
        .add(
            "Excluded VDs",
            if excluded.is_empty() {
                "-".into()
            } else {
                excluded.join(",")
            },
        )
        .add("Next execution", &p.next_exec_time)
        .add(
            "Interval",
            if p.continuous {
                "continuous".to_string()
            } else {
                format!("{} s", opt(p.exec_frequency_seconds))
            },
        );
}

impl Render for patrol::Properties {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Patrol read properties");
        render_patrol_props(self, &mut f);
        f.render(out);
    }
}

impl Render for LogInfo {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Event log");
        f.add("Newest", self.newest_seq)
            .add("Oldest", self.oldest_seq)
            .add("Clear", self.clear_seq)
            .add("Shutdown", self.shutdown_seq)
            .add("Boot", self.boot_seq);
        f.render(out);
    }
}

impl Render for EventList {
    fn render(&self, out: &mut String) {
        if self.events.is_empty() {
            out.push_str("No matching events\n");
            return;
        }
        let mut t = Table::new(["Seq", "Time", "Class", "Locale", "Code", "Description"]);
        for e in &self.events {
            t.row([
                e.seq.to_string(),
                e.time.clone(),
                e.class_name.clone(),
                e.locales.join(","),
                format!("{:#06x}", e.code),
                e.description.clone(),
            ]);
        }
        t.render(out);
    }
}

impl Render for EnclosureList {
    fn render(&self, out: &mut String) {
        let mut t = Table::new(["EID", "Listed", "Drives", "Slots", "SAS address"]);
        for e in &self.enclosures {
            let slots: Vec<String> = e.slots.iter().map(u8::to_string).collect();
            t.row([
                e.enclosure_id.to_string(),
                yes_no(e.listed_as_device).to_string(),
                e.drives.to_string(),
                slots.join(","),
                opt(e.sas_address.as_ref()),
            ]);
        }
        t.render(out);
    }
}

impl Render for FirmwareInfo {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Firmware");
        f.add("Model", &self.product_name)
            .add("Package", &self.package_version)
            .add("Firmware version", &self.firmware_version);
        f.render(out);
        components("Image components", &self.components, out);
        if self.pending_components.is_empty() {
            out.push_str("\nNo pending images\n");
        } else {
            components("Pending image components", &self.pending_components, out);
        }
    }
}

impl Render for AlarmReport {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Alarm");
        f.add("Present", yes_no(self.present))
            .add("alarm_enable property", self.alarm_enable_property);
        match (self.speaker_state, &self.speaker_error) {
            (Some(s), _) => f.add("Speaker state", s),
            (None, Some(e)) => f.add("Speaker state", format!("unavailable: {e}")),
            (None, None) => &mut f,
        };
        f.render(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_text_includes_eta() {
        let p = Progress {
            raw: 32768,
            percent: 50.0,
            elapsed_seconds: 3661,
            remaining_seconds: Some(3661),
        };
        assert_eq!(
            progress_text(&Some(p)),
            "50.00% after 1h 01m 01s, about 1h 01m 01s left"
        );
        assert_eq!(progress_text(&None), "not running");
    }

    #[test]
    fn temperature_report_renders_absent_sensors() {
        let r = TemperatureReport {
            controller: 0,
            roc_celsius: Some(52),
            controller_celsius: None,
        };
        let mut out = String::new();
        r.render(&mut out);
        assert!(out.contains("ROC        : 52 C"));
        assert!(out.contains("Controller : absent"));
    }
}
