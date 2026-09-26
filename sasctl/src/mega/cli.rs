use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};

use crate::Ctx;
use crate::cli::{
    AlarmAction, BatteryAction, Command, ConfigAction, ControllerAction, DriveAction, DriveId,
    EventAction, EventFilter, FirmwareAction, ForeignAction, Operation, PatrolAction, PatrolMode,
    Switch, VolumeAction, VolumeId, VolumeInit,
};
use crate::mega::bbu;
use crate::mega::config::{
    self, INIT_FULL, INIT_NONE, INIT_QUICK, RaidLevel, SPARE_ENCL_AFFINITY, SPARE_REVERTIBLE,
    VolumeRequest, parse_stripe,
};
use crate::mega::ctrl::{self, AlarmAction as CtrlAlarm, CtrlSetting};
use crate::mega::event::{self, EventQuery, parse_class, parse_locale};
use crate::mega::fw::{self, FirmwareInfo};
use crate::mega::ld::{self, LdSetting};
use crate::mega::patrol::{self, Mode, Schedule, parse_interval};
use crate::mega::pd::{self, DriveAddress, STATE_HOT_SPARE};
use crate::mega::report::{
    self, ClearProgress, ControllerSummary, RebuildProgress, TemperatureReport, Temperatures,
};
use crate::mega::reset;
use crate::mega::transport::Transport;
use crate::output::{Done, Emit};
use crate::sysfs::{parse_pci_address, scsi_hosts};

pub const DRIVER: &str = "megaraid_sas";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerRef {
    pub index: usize,
    pub host_no: u32,
    pub pci_address: Option<String>,
}

pub fn controllers(sysfs: &Path) -> Vec<ControllerRef> {
    let mut hosts = scsi_hosts(sysfs, &[DRIVER]);
    hosts.sort_by_key(|h| {
        let pci = h.pci_address.as_deref().and_then(parse_pci_address);
        (pci.is_none(), pci.unwrap_or_default(), h.host_no)
    });
    hosts
        .into_iter()
        .enumerate()
        .map(|(index, h)| ControllerRef {
            index,
            host_no: h.host_no,
            pci_address: h.pci_address,
        })
        .collect()
}

fn host_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn done(message: String) -> Result<Box<dyn Emit>> {
    Ok(Box::new(Done::ok(message)))
}

fn boxed<T: Emit + 'static>(value: T) -> Result<Box<dyn Emit>> {
    Ok(Box::new(value))
}

fn unsupported() -> Result<Box<dyn Emit>> {
    Err(anyhow!("this is not available on megaraid_sas controllers"))
}

fn event_start(info: &event::LogInfo, since: &str) -> Result<u32> {
    Ok(match since.to_ascii_lowercase().as_str() {
        "boot" => info.boot_seq,
        "shutdown" => info.shutdown_seq,
        "clear" => info.clear_seq,
        "oldest" => info.oldest_seq,
        "newest" => info.newest_seq,
        other => other.parse().map_err(|_| {
            anyhow!("--since takes boot, shutdown, clear, oldest, newest or a sequence number")
        })?,
    })
}

fn refuse_extended_config(t: &dyn Transport) -> Result<()> {
    if ctrl::get_info(t)?.config_ext2_supported {
        bail!(
            "this controller uses the extended configuration format, which is not documented, so configuration changes are refused"
        );
    }
    Ok(())
}

fn drive_address(id: &Option<DriveId>) -> Result<DriveAddress> {
    id.as_ref()
        .context("pass a drive as enclosure:slot")?
        .parse()
}

fn volume_id(id: &Option<VolumeId>) -> Result<u8> {
    id.context("pass a volume ID")?.narrow()
}

fn init_code(init: VolumeInit) -> u8 {
    match init {
        VolumeInit::None => INIT_NONE,
        VolumeInit::Fast => INIT_QUICK,
        VolumeInit::Full => INIT_FULL,
    }
}

pub fn supports(command: &Command) -> bool {
    match command {
        Command::Controller { action, .. } => {
            !matches!(action, Some(ControllerAction::Reset { snapdump: true }))
        }
        Command::Drive { action, .. } => match action {
            Some(DriveAction::Spare { pool, .. }) => pool.is_none(),
            _ => true,
        },
        Command::Volume { action, .. } => match action {
            Some(VolumeAction::Activate | VolumeAction::Check) => false,
            Some(VolumeAction::Delete { zero_lba0 }) => !zero_lba0,
            Some(VolumeAction::Create { size, pool, .. }) => size.is_none() && pool.is_none(),
            _ => true,
        },
        Command::Config { action } => {
            !matches!(action, Some(ConfigAction::Clear { zero_lba0: true }))
        }
        Command::Firmware { action } => match action {
            None => true,
            Some(FirmwareAction::Flash { bios, .. }) => !bios,
            Some(FirmwareAction::Save { .. }) => false,
        },
        Command::Enclosure
        | Command::Temperature
        | Command::Battery { .. }
        | Command::Patrol { .. }
        | Command::Alarm { .. }
        | Command::Event { .. }
        | Command::Foreign { .. } => true,
        Command::Phy { .. } | Command::Boot { .. } | Command::Log { .. } | Command::Diag { .. } => {
            false
        }
    }
}

pub fn execute(
    command: &Command,
    c: &ControllerRef,
    t: &dyn Transport,
    ctx: &Ctx,
) -> Result<Box<dyn Emit>> {
    let n = c.index;
    match command {
        Command::Controller { action, .. } => match action {
            None => {
                let info = ctrl::get_info(t)?;
                boxed(ControllerSummary {
                    index: n,
                    host_no: u32::from(t.host_no()),
                    pci_address: c.pci_address.clone(),
                    driver_version: report::driver_version(&ctx.sysfs),
                    controller_time: ctrl::get_time(t).ok().map(event::format_fw_time),
                    info,
                })
            }
            Some(ControllerAction::Settings) => boxed(ctrl::get_props(t)?),
            Some(ControllerAction::Set { setting, value }) => {
                let parsed = CtrlSetting::parse(setting, value)?;
                ctx.confirm(&format!("setting {setting} to {value} on controller {n}"))?;
                if parsed.enables_jbod() && !ctrl::get_info(t)?.support_jbod {
                    bail!("controller {n} does not support JBOD");
                }
                boxed(ctrl::set_property(t, parsed)?)
            }
            Some(ControllerAction::Reset { .. }) => {
                ctx.confirm(&format!(
                    "resetting controller {n} through the driver, which drops outstanding IO"
                ))?;
                reset::check_allowed(&ctrl::get_props(t)?)?;
                t.reset_host(&ctx.sysfs)?;
                done(format!("controller {n} reset"))
            }
            Some(ControllerAction::Time) => {
                boxed(report::time_report(ctrl::get_time(t)?, host_now()))
            }
            Some(ControllerAction::Shutdown { spindown }) => {
                ctx.confirm(&format!("shutting down controller {n}"))?;
                ctrl::shutdown(t, *spindown)?;
                done(format!("controller {n} shut down"))
            }
            Some(ControllerAction::Flush { disks }) => {
                ctx.confirm(&format!("flushing the cache of controller {n}"))?;
                ctrl::flush_cache(t, *disks)?;
                done(format!("controller {n} cache flushed"))
            }
        },
        Command::Temperature => {
            let info = ctrl::get_info(t)?;
            boxed(Temperatures {
                controller: TemperatureReport {
                    controller: n,
                    roc_celsius: info.temperatures.roc_celsius,
                    controller_celsius: info.temperatures.controller_celsius,
                },
                drives: report::drive_temperatures(t)?,
            })
        }
        Command::Drive { id, action } => drive(id, action.as_ref(), t, ctx),
        Command::Volume { id, action } => volume(id, action.as_ref(), t, ctx),
        Command::Config { action } => match action {
            None => boxed(config::read(t)?),
            Some(ConfigAction::Clear { .. }) => {
                ctx.confirm(&format!("deleting every virtual drive on controller {n}"))?;
                config::clear(t)?;
                done("configuration cleared".into())
            }
            Some(ConfigAction::Save { file }) => {
                let raw = config::read_raw(t)?;
                std::fs::write(file, &raw)
                    .with_context(|| format!("writing {}", file.display()))?;
                done(format!(
                    "saved {} bytes of configuration to {}",
                    raw.len(),
                    file.display()
                ))
            }
        },
        Command::Foreign { action } => match action {
            None => boxed(report::foreign_report(t, false, None)?),
            Some(ForeignAction::Preview { index }) => {
                boxed(report::foreign_report(t, true, *index)?)
            }
            Some(ForeignAction::Import { index: None }) => {
                ctx.confirm("importing every foreign configuration")?;
                config::foreign_import_all(t)?;
                done("foreign configurations imported".into())
            }
            Some(ForeignAction::Import { index: Some(i) }) => {
                ctx.confirm(&format!("importing foreign configuration {i}"))?;
                config::foreign_import(t, *i)?;
                done(format!("foreign configuration {i} imported"))
            }
            Some(ForeignAction::Clear) => {
                ctx.confirm("discarding every foreign configuration")?;
                config::foreign_clear(t)?;
                done("foreign configurations cleared".into())
            }
        },
        Command::Battery { action } => match action {
            None => {
                let present = ctrl::get_info(t).map(|i| i.bbu_present).unwrap_or(false);
                let probe = bbu::report(t, "bbu", present);
                let kind = match probe.status.as_ref().map(|s| s.battery_type) {
                    Some(1 | 2) | None => return boxed(probe),
                    Some(_) => "cachevault",
                };
                boxed(bbu::report(t, kind, present))
            }
            Some(BatteryAction::Learn) => {
                ctx.confirm("starting a battery learn cycle")?;
                bbu::start_learn(t)?;
                done("learn cycle started".into())
            }
        },
        Command::Patrol { action } => match action {
            None => boxed(patrol::report(t)?),
            Some(PatrolAction::Start) => {
                ctx.confirm("starting patrol read")?;
                patrol::start(t)?;
                done("patrol read started".into())
            }
            Some(PatrolAction::Stop) => {
                ctx.confirm("stopping patrol read")?;
                patrol::stop(t)?;
                done("patrol read stopped".into())
            }
            Some(PatrolAction::Set {
                mode,
                interval,
                start_in,
            }) => {
                let sched = Schedule {
                    mode: match mode {
                        PatrolMode::Auto => Mode::Auto,
                        PatrolMode::Manual => Mode::Manual,
                        PatrolMode::Off => Mode::Disabled,
                    },
                    interval: interval.as_deref().map(parse_interval).transpose()?,
                    start_in: *start_in,
                };
                ctx.confirm("changing the patrol read schedule")?;
                boxed(patrol::configure(t, sched)?)
            }
        },
        Command::Event {
            filter:
                EventFilter {
                    count,
                    since,
                    class,
                    locale,
                },
            action,
        } => match action {
            Some(EventAction::Info) => boxed(event::log_info(t)?),
            Some(EventAction::Enable) => unsupported(),
            None => {
                let class = parse_class(class.as_deref().unwrap_or("info"))?;
                let locale = parse_locale(locale.as_deref().unwrap_or("all"))?;
                let info = event::log_info(t)?;
                let q = EventQuery {
                    start: event_start(&info, since.as_deref().unwrap_or("boot"))?,
                    stop: info.newest_seq,
                    class,
                    locale,
                    limit: count.unwrap_or(100),
                };
                boxed(report::EventList {
                    events: event::fetch(t, &q)?,
                })
            }
        },
        Command::Enclosure => boxed(report::enclosures(&pd::get_list(t)?)),
        Command::Firmware { action } => match action {
            None => boxed(FirmwareInfo::from_info(&ctrl::get_info(t)?)),
            Some(FirmwareAction::Flash { file, .. }) => {
                let image =
                    std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
                fw::validate(&image)?;
                ctx.confirm(&format!("flashing {} onto controller {n}", file.display()))?;
                fw::flash(t, &image)?;
                boxed(FirmwareInfo::from_info(&ctrl::get_info(t)?))
            }
            Some(FirmwareAction::Save { .. }) => unsupported(),
        },
        Command::Alarm { action } => {
            let (verb, a) = match action {
                None => return boxed(report::alarm_report(t)?),
                Some(AlarmAction::On) => ("enabling", CtrlAlarm::Enable),
                Some(AlarmAction::Off) => ("disabling", CtrlAlarm::Disable),
                Some(AlarmAction::Silence) => ("silencing", CtrlAlarm::Silence),
            };
            ctx.confirm(&format!("{verb} the alarm on controller {n}"))?;
            ctrl::alarm(t, a)?;
            done("alarm updated".into())
        }
        Command::Phy { .. } | Command::Boot { .. } | Command::Log { .. } | Command::Diag { .. } => {
            unsupported()
        }
    }
}

fn set_state(
    t: &dyn Transport,
    ctx: &Ctx,
    drive: DriveAddress,
    code: u16,
    name: &str,
) -> Result<Box<dyn Emit>> {
    ctx.confirm(&format!("setting drive {drive} {name}"))?;
    let a = report::locate_drive(t, drive)?;
    pd::set_state(t, a.device_id, code)?;
    done(format!("drive {drive} set {name}"))
}

fn drive(
    id: &Option<DriveId>,
    action: Option<&DriveAction>,
    t: &dyn Transport,
    ctx: &Ctx,
) -> Result<Box<dyn Emit>> {
    let Some(action) = action else {
        return match id {
            None => boxed(report::drive_list(t)?),
            Some(_) => boxed(report::drive_detail(t, drive_address(id)?)?),
        };
    };
    let drive = drive_address(id)?;
    match action {
        DriveAction::Locate { state } => {
            let on = *state == Switch::On;
            ctx.confirm(&format!(
                "turning the locate LED of drive {drive} {}",
                if on { "on" } else { "off" }
            ))?;
            let a = report::locate_drive(t, drive)?;
            pd::locate(t, a.device_id, on)?;
            done(format!(
                "locate {} for drive {drive}",
                if on { "started" } else { "stopped" }
            ))
        }
        DriveAction::Smart => boxed(report::drive_smart(t, drive)?),
        DriveAction::Temperature => boxed(report::drive_temperature(t, drive)?),
        DriveAction::Online => set_state(t, ctx, drive, pd::STATE_ONLINE, "online"),
        DriveAction::Offline => set_state(t, ctx, drive, pd::STATE_OFFLINE, "offline"),
        DriveAction::Good => set_state(
            t,
            ctx,
            drive,
            pd::STATE_UNCONFIGURED_GOOD,
            "unconfigured good",
        ),
        DriveAction::Jbod => set_state(t, ctx, drive, pd::STATE_SYSTEM, "JBOD"),
        DriveAction::Rebuild { operation } => match operation {
            None => {
                let a = report::locate_drive(t, drive)?;
                let info = pd::get_info(t, a.device_id)?;
                boxed(RebuildProgress {
                    address: drive,
                    state: info.state,
                    rebuild: info.progress.rebuild,
                })
            }
            Some(Operation::Start) => {
                ctx.confirm(&format!("starting a rebuild on drive {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                pd::rebuild_start(t, a.device_id)?;
                done(format!("rebuild started on drive {drive}"))
            }
            Some(Operation::Stop) => {
                ctx.confirm(&format!("aborting the rebuild on drive {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                pd::rebuild_stop(t, a.device_id)?;
                done(format!("rebuild stopped on drive {drive}"))
            }
        },
        DriveAction::Erase { operation } => match operation {
            None => {
                let a = report::locate_drive(t, drive)?;
                let info = pd::get_info(t, a.device_id)?;
                boxed(ClearProgress {
                    address: drive,
                    state: info.state,
                    clear: info.progress.clear,
                })
            }
            Some(Operation::Start) => {
                ctx.confirm(&format!(
                    "erasing drive {drive}, which overwrites everything on it"
                ))?;
                let a = report::locate_drive(t, drive)?;
                pd::clear_start(t, a.device_id)?;
                done(format!("erase started on drive {drive}"))
            }
            Some(Operation::Stop) => {
                ctx.confirm(&format!("aborting the erase on drive {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                pd::clear_stop(t, a.device_id)?;
                done(format!("erase stopped on drive {drive}"))
            }
        },
        DriveAction::Spare {
            volume,
            revertible,
            affinity,
            ..
        } => {
            let what = match volume {
                Some(v) => {
                    format!("making drive {drive} a dedicated hot spare for volume {v}")
                }
                None => format!("making drive {drive} a global hot spare"),
            };
            ctx.confirm(&what)?;
            refuse_extended_config(t)?;
            let a = report::locate_drive(t, drive)?;
            let info = pd::get_info(t, a.device_id)?;
            let cfg = config::read(t)?;
            let mut flags = 0;
            if *revertible {
                flags |= SPARE_REVERTIBLE;
            }
            if *affinity {
                flags |= SPARE_ENCL_AFFINITY;
            }
            let data = config::build_spare(&cfg, &info, *volume, flags)?;
            config::make_spare(t, &data)?;
            done(format!("drive {drive} is now a hot spare"))
        }
        DriveAction::Unspare => {
            ctx.confirm(&format!("removing hot spare {drive}"))?;
            let a = report::locate_drive(t, drive)?;
            let info = pd::get_info(t, a.device_id)?;
            if info.fw_state != STATE_HOT_SPARE {
                bail!("drive {drive} is {} and not a hot spare", info.state);
            }
            config::remove_spare(t, info.device_id, info.seq_num)?;
            done(format!("drive {drive} is no longer a hot spare"))
        }
    }
}

fn volume(
    id: &Option<VolumeId>,
    action: Option<&VolumeAction>,
    t: &dyn Transport,
    ctx: &Ctx,
) -> Result<Box<dyn Emit>> {
    match action {
        None => match id {
            None => boxed(report::volume_list(t)?),
            Some(_) => boxed(report::volume_detail(t, volume_id(id)?)?),
        },
        Some(VolumeAction::Create {
            level,
            drives,
            name,
            stripe,
            span,
            init,
            ..
        }) => {
            let raid = level.to_ascii_lowercase();
            let level = RaidLevel::parse(raid.strip_prefix("raid").unwrap_or(&raid))?;
            let stripe_bytes = parse_stripe(stripe.as_deref().unwrap_or("64k"))?;
            let members = drives
                .iter()
                .map(|d| d.parse())
                .collect::<Result<Vec<DriveAddress>>>()?;
            let list: Vec<String> = members.iter().map(DriveAddress::to_string).collect();
            ctx.confirm(&format!(
                "creating a {} volume on drives {}",
                level.name(),
                list.join(",")
            ))?;
            let info = ctrl::get_info(t)?;
            if info.config_ext2_supported {
                bail!(
                    "this controller uses the extended configuration format, which is not documented, so configuration changes are refused"
                );
            }
            if !info.raid_levels.contains(&level.ctrl_capability()) {
                bail!("controller does not support {}", level.ctrl_capability());
            }
            if level.spanned() && !info.adapter_operations.contains(&"spanning") {
                bail!("controller does not support spanned volumes");
            }
            let pds = pd::get_list(t)?;
            let infos = members
                .iter()
                .map(|d| pd::get_info(t, pd::resolve(&pds, *d)?.device_id))
                .collect::<Result<Vec<_>>>()?;
            let cfg = config::read(t)?;
            let data = config::build_volume(
                &cfg,
                &VolumeRequest {
                    level,
                    drives: &infos,
                    drives_per_array: *span,
                    stripe_bytes,
                    name: name.as_deref(),
                    init_state: init_code(init.unwrap_or(VolumeInit::None)),
                },
            )?;
            let target = config::new_target_id(&data);
            config::add(t, &data)?;
            done(format!("created volume {target}"))
        }
        Some(VolumeAction::Delete { .. }) => {
            let vd = volume_id(id)?;
            ctx.confirm(&format!("deleting volume {vd} and its data"))?;
            ld::delete(t, vd)?;
            done(format!("volume {vd} deleted"))
        }
        Some(VolumeAction::Settings) => boxed(ld::get_props(t, volume_id(id)?)?),
        Some(VolumeAction::Set { setting, value }) => {
            let vd = volume_id(id)?;
            let parsed = LdSetting::parse(setting, value)?;
            ctx.confirm(&format!("setting {setting} to {value} on volume {vd}"))?;
            boxed(ld::set_property(t, vd, &parsed)?)
        }
        Some(VolumeAction::Progress) => {
            let vd = volume_id(id)?;
            let mut progress = report::volume_init_progress(t, vd)?;
            progress
                .operations
                .extend(report::volume_check_progress(t, vd)?.operations);
            boxed(progress)
        }
        Some(VolumeAction::Activate | VolumeAction::Check) => unsupported(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Le;
    use crate::mega::ctrl::tests::ctrl_info_bytes;
    use crate::mega::mfi::op;
    use crate::mega::mock::Mock;
    use crate::mega::pd::tests::{pd_info_bytes, pd_list_bytes};
    use crate::output::Format;
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;

    use crate::cli::{Cli, VolumeAction, parse};
    use crate::mega::ctrl::CtrlSetting;

    fn ctx(yes: bool) -> Ctx {
        Ctx {
            format: Format::Json,
            yes,
            interactive: false,
            sysfs: PathBuf::new(),
        }
    }

    fn one() -> ControllerRef {
        ControllerRef {
            index: 0,
            host_no: 0,
            pci_address: None,
        }
    }

    fn busy_mock() -> Mock {
        Mock::new()
            .reply(op::CTRL_GET_INFO, ctrl_info_bytes("x", "y", 40, 0))
            .reply(op::PD_GET_LIST, pd_list_bytes(&[(8, 252, 0, 0)]))
            .reply(
                op::PD_GET_INFO,
                pd_info_bytes(8, 1, pd::STATE_ONLINE, 252, 0, 30),
            )
    }

    #[test]
    fn every_state_change_is_refused_without_yes_before_anything_is_sent() {
        let changing: &[&[&str]] = &[
            &["controller", "set", "rebuild-rate", "30"],
            &["controller", "shutdown"],
            &["controller", "flush"],
            &["drive", "252:0", "locate"],
            &["drive", "252:0", "offline"],
            &["drive", "252:0", "rebuild", "start"],
            &["drive", "252:0", "rebuild", "stop"],
            &["drive", "252:0", "spare"],
            &["drive", "252:0", "unspare"],
            &["volume", "create", "1", "252:0", "252:1"],
            &["volume", "0", "delete"],
            &["volume", "0", "set", "write-cache", "wb"],
            &["config", "clear"],
            &["foreign", "import"],
            &["foreign", "clear"],
            &["battery", "learn"],
            &["patrol", "start"],
            &["patrol", "stop"],
            &["patrol", "set", "manual"],
            &["alarm", "on"],
            &["alarm", "off"],
            &["alarm", "silence"],
            &["controller", "set", "bgi-rate", "30"],
            &["controller", "set", "jbod", "on"],
            &["controller", "set", "coercion", "1g"],
            &["controller", "reset"],
            &["drive", "252:0", "erase", "start"],
            &["drive", "252:0", "erase", "stop"],
            &["drive", "252:0", "spare", "--revertible", "--affinity"],
            &[
                "volume", "create", "10", "252:0", "252:1", "252:2", "252:3", "--span", "2",
                "--init", "full",
            ],
            &["volume", "0", "set", "autobgi", "off"],
            &["foreign", "import", "1"],
        ];
        for argv in changing {
            let mock = busy_mock();
            let err = execute(&parse(argv), &one(), &mock, &ctx(false)).unwrap_err();
            assert!(err.to_string().contains("--yes"), "{argv:?}: {err}");
            assert!(
                mock.calls().is_empty(),
                "{argv:?} sent a frame before confirming"
            );
            assert_eq!(mock.resets(), 0, "{argv:?} reset before confirming");
        }
    }

    #[test]
    fn firmware_flash_validates_before_confirming() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-mega-fw");
        fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.rom");
        let good = dir.join("good.rom");
        fs::write(&bad, vec![0u8; 1500]).unwrap();
        fs::write(&good, vec![0u8; 2048]).unwrap();
        let mock = Mock::new();
        let e = execute(
            &parse(&["firmware", "flash", bad.to_str().unwrap()]),
            &one(),
            &mock,
            &ctx(false),
        )
        .unwrap_err();
        assert!(e.to_string().contains("multiple of 1024"));
        let e = execute(
            &parse(&["firmware", "flash", good.to_str().unwrap()]),
            &one(),
            &mock,
            &ctx(false),
        )
        .unwrap_err();
        assert!(e.to_string().contains("--yes"));
        assert!(mock.calls().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn confirmed_state_change_reaches_the_controller() {
        let mock = busy_mock().reply(op::PD_STATE_SET, vec![]);
        execute(
            &parse(&["drive", "252:0", "jbod"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
        let set = mock
            .calls()
            .into_iter()
            .find(|c| c.opcode() == op::PD_STATE_SET)
            .unwrap();
        assert_eq!(set.mbox()[..6], [8, 0, 1, 0, 0x40, 0]);
    }

    #[test]
    fn create_is_refused_on_extended_configuration_controllers() {
        let mut info = ctrl_info_bytes("x", "y", 40, 0);
        info[2120] = 1;
        let mock = Mock::new().reply(op::CTRL_GET_INFO, info);
        let e = execute(
            &parse(&["volume", "create", "0", "252:0"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap_err();
        assert!(e.to_string().contains("extended configuration"));
        assert_eq!(mock.opcodes(), vec![op::CTRL_GET_INFO]);
    }

    fn props_with(byte: usize, value: u8) -> Vec<u8> {
        let mut p = vec![0u8; ctrl::CTRL_PROPS_LEN];
        p[byte] = value;
        p
    }

    #[test]
    fn reset_is_refused_when_online_controller_reset_is_disabled() {
        let mock = Mock::new().reply(op::CTRL_GET_PROPS, props_with(33, 0x04));
        let e = execute(&parse(&["controller", "reset"]), &one(), &mock, &ctx(true)).unwrap_err();
        assert!(e.to_string().contains("refused"), "{e}");
        assert_eq!(mock.opcodes(), vec![op::CTRL_GET_PROPS]);
        assert_eq!(mock.resets(), 0);
    }

    #[test]
    fn reset_goes_through_the_host_reset_when_allowed() {
        let mock = Mock::new().reply(op::CTRL_GET_PROPS, props_with(33, 0x20));
        execute(&parse(&["controller", "reset"]), &one(), &mock, &ctx(true)).unwrap();
        assert_eq!(mock.resets(), 1);
        assert_eq!(mock.opcodes(), vec![op::CTRL_GET_PROPS]);
    }

    #[test]
    fn jbod_is_refused_on_controllers_without_jbod_support() {
        let mut info = ctrl_info_bytes("x", "y", 40, 0);
        info[1957] = 0;
        let mock = Mock::new()
            .reply(op::CTRL_GET_INFO, info)
            .reply(op::CTRL_GET_PROPS, props_with(8, 30))
            .reply(op::CTRL_SET_PROPS, vec![]);
        let e = execute(
            &parse(&["controller", "set", "jbod", "on"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap_err();
        assert!(e.to_string().contains("JBOD"));
        assert!(!mock.opcodes().contains(&op::CTRL_SET_PROPS));
    }

    #[test]
    fn controller_set_writes_the_new_rate_into_the_props_buffer() {
        let mock = Mock::new()
            .reply(op::CTRL_GET_PROPS, props_with(8, 30))
            .reply(op::CTRL_SET_PROPS, vec![]);
        execute(
            &parse(&["controller", "set", "bgi-rate", "45"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
        let write = mock
            .calls()
            .into_iter()
            .find(|c| c.opcode() == op::CTRL_SET_PROPS)
            .unwrap();
        let mut want = props_with(8, 30);
        want[10] = 45;
        assert_eq!(write.bufs[0], want);
        assert_eq!(write.mbox(), &[0u8; 12]);
        let e = execute(
            &parse(&["controller", "set", "bgi-rate", "101"]),
            &one(),
            &Mock::new(),
            &ctx(true),
        )
        .unwrap_err();
        assert!(e.to_string().contains("0 to 100"));
    }

    fn four_ugood_mock(info: Vec<u8>) -> Mock {
        let mut mock = Mock::new()
            .reply(op::CTRL_GET_INFO, info)
            .reply(
                op::PD_GET_LIST,
                pd_list_bytes(&[
                    (20, 252, 0, 0),
                    (21, 252, 1, 0),
                    (22, 252, 2, 0),
                    (23, 252, 3, 0),
                ]),
            )
            .reply(
                op::CFG_READ,
                crate::mega::config::tests::config_bytes(&[], &[], &[]),
            )
            .reply(op::CFG_ADD, vec![])
            .reply(op::CFG_MAKE_SPARE, vec![]);
        for dev in 20u16..24 {
            mock = mock.reply_mbox(
                op::PD_GET_INFO,
                &dev.to_le_bytes(),
                pd_info_bytes(
                    dev,
                    dev + 1,
                    pd::STATE_UNCONFIGURED_GOOD,
                    252,
                    (dev - 20) as u8,
                    30,
                ),
            );
        }
        mock
    }

    fn spanning_info() -> Vec<u8> {
        let mut info = ctrl_info_bytes("x", "y", 40, 0);
        info[1504 + 1] |= 1;
        info
    }

    #[test]
    fn raid10_create_sends_two_arrays_and_a_spanned_ld() {
        let mock = four_ugood_mock(spanning_info());
        execute(
            &parse(&[
                "volume", "create", "10", "252:0", "252:1", "252:2", "252:3", "--span", "2",
                "--init", "fast",
            ]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
        let add = mock
            .calls()
            .into_iter()
            .find(|c| c.opcode() == op::CFG_ADD)
            .unwrap();
        let b = &add.bufs[0];
        assert_eq!(add.frame.u16_at(0x10), 0x0008);
        assert_eq!(add.frame.u32_at(0x14), b.len() as u32);
        assert_eq!(add.mbox(), &[0u8; 12]);
        assert_eq!(b.len(), 32 + 2 * 288 + 256);
        assert_eq!((b.u16_at(4), b.u16_at(8)), (2, 1));
        assert_eq!((b.u16_at(32 + 32), b.u16_at(32 + 40)), (20, 21));
        assert_eq!((b.u16_at(320 + 32), b.u16_at(320 + 40)), (22, 23));
        let l = 32 + 2 * 288;
        assert_eq!((b[l + 32], b[l + 34], b[l + 36], b[l + 37]), (1, 3, 2, 2));
        assert_eq!(b[l + 39], 1);
        assert_eq!((b.u16_at(l + 80), b.u16_at(l + 104)), (0, 1));
    }

    #[test]
    fn spanned_create_needs_the_spanning_capability() {
        let mock = four_ugood_mock(ctrl_info_bytes("x", "y", 40, 0));
        let e = execute(
            &parse(&[
                "volume", "create", "10", "252:0", "252:1", "252:2", "252:3", "--span", "2",
            ]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap_err();
        assert!(e.to_string().contains("spanned"));
        assert_eq!(mock.opcodes(), vec![op::CTRL_GET_INFO]);
    }

    #[test]
    fn hotspare_flags_reach_the_spare_record() {
        let mock = four_ugood_mock(spanning_info());
        execute(
            &parse(&["drive", "252:1", "spare", "--revertible", "--affinity"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
        let spare = mock
            .calls()
            .into_iter()
            .find(|c| c.opcode() == op::CFG_MAKE_SPARE)
            .unwrap();
        let b = &spare.bufs[0];
        assert_eq!(b.len(), 40);
        assert_eq!((b.u16_at(0), b.u16_at(2), b[4], b[7]), (21, 22, 0x06, 0));
    }

    #[test]
    fn foreign_import_with_index_scans_then_imports_one() {
        let mut scan = vec![0u8; config::FOREIGN_SCAN_LEN];
        scan[0] = 2;
        let mock = Mock::new()
            .reply(op::CFG_FOREIGN_SCAN, scan)
            .reply(op::CFG_FOREIGN_IMPORT, vec![]);
        execute(
            &parse(&["foreign", "import", "1"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
        let calls = mock.calls();
        assert_eq!(
            mock.opcodes(),
            vec![op::CFG_FOREIGN_SCAN, op::CFG_FOREIGN_IMPORT]
        );
        assert_eq!(calls[1].mbox()[..4], [1, 0, 0, 0]);
    }

    #[test]
    fn drive_clear_start_and_progress_run_end_to_end() {
        let mut info = pd_info_bytes(8, 5, pd::STATE_UNCONFIGURED_GOOD, 252, 0, 30);
        info[260..264].copy_from_slice(&4u32.to_le_bytes());
        info[272..274].copy_from_slice(&32768u16.to_le_bytes());
        let mock = Mock::new()
            .reply(op::PD_GET_LIST, pd_list_bytes(&[(8, 252, 0, 0)]))
            .reply(op::PD_GET_INFO, info)
            .reply(op::PD_CLEAR_START, vec![]);
        execute(
            &parse(&["drive", "252:0", "erase", "start"]),
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
        let start = mock
            .calls()
            .into_iter()
            .find(|c| c.opcode() == op::PD_CLEAR_START)
            .unwrap();
        assert_eq!(start.mbox()[..4], [8, 0, 5, 0]);
        execute(
            &parse(&["drive", "252:0", "erase"]),
            &one(),
            &mock,
            &ctx(false),
        )
        .unwrap();
    }

    #[test]
    fn smart_on_a_sata_drive_reads_ata_attributes() {
        use crate::mega::ata::tests::{sat_inquiry, smart_data, smart_thresholds};
        let mock = Mock::new()
            .reply(op::PD_GET_LIST, pd_list_bytes(&[(8, 252, 0, 0)]))
            .reply(
                op::PD_GET_INFO,
                pd_info_bytes(8, 1, pd::STATE_ONLINE, 252, 0, 30),
            )
            .scsi(8, &[0x12, 0], sat_inquiry())
            .scsi(
                8,
                &[0x85, 0x08, 0x0e, 0, 0xd0],
                smart_data(&[(5, 0x0033, 9, 9, 800)]),
            )
            .scsi(
                8,
                &[0x85, 0x08, 0x0e, 0, 0xd1],
                smart_thresholds(&[(5, 10)]),
            );
        let s = report::drive_smart(&mock, "252:0".parse().unwrap()).unwrap();
        assert_eq!(s.sata, Some(true));
        let ata = s.ata_smart.unwrap();
        assert_eq!(ata.health, "FAILED");
        assert_eq!(ata.attributes[0].raw, 800);
        let sas = Mock::new()
            .reply(op::PD_GET_LIST, pd_list_bytes(&[(8, 252, 0, 0)]))
            .reply(
                op::PD_GET_INFO,
                pd_info_bytes(8, 1, pd::STATE_ONLINE, 252, 0, 30),
            )
            .scsi(8, &[0x12, 0], vec![0u8; 36]);
        let s = report::drive_smart(&sas, "252:0".parse().unwrap()).unwrap();
        assert_eq!(s.sata, Some(false));
        assert!(s.ata_smart.is_none() && s.ata_error.is_none());
        assert!(!sas.calls().iter().any(|c| c.frame[0x20] == 0x85));
    }

    #[test]
    fn read_commands_run_without_yes() {
        let mock = busy_mock();
        execute(&parse(&["temperature"]), &one(), &mock, &ctx(false)).unwrap();
        execute(&parse(&["enclosure"]), &one(), &mock, &ctx(false)).unwrap();
    }

    fn fake_sysfs(name: &str, hosts: &[(u32, &str)]) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(name);
        let _ = fs::remove_dir_all(&root);
        for (n, pci) in hosts {
            let dev = root.join(format!("devices/pci0000:00/{pci}/host{n}"));
            fs::create_dir_all(&dev).unwrap();
            let h = root.join(format!("class/scsi_host/host{n}"));
            fs::create_dir_all(&h).unwrap();
            fs::write(h.join("proc_name"), "megaraid_sas\n").unwrap();
            std::os::unix::fs::symlink(&dev, h.join("device")).unwrap();
        }
        let other = root.join("class/scsi_host/host9");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("proc_name"), "mpt3sas\n").unwrap();
        root
    }

    #[test]
    fn controllers_are_ordered_by_pci_address_not_host_number() {
        let root = fake_sysfs(
            "test-mega-sysfs",
            &[
                (0, "0000:81:00.0"),
                (1, "0000:03:00.0"),
                (2, "0001:01:00.0"),
                (3, "0000:03:00.1"),
            ],
        );
        let ctrls = controllers(&root);
        fs::remove_dir_all(&root).unwrap();
        let order: Vec<u32> = ctrls.iter().map(|c| c.host_no).collect();
        assert_eq!(order, vec![1, 3, 0, 2]);
        assert_eq!(ctrls[0].index, 0);
        assert_eq!(ctrls[0].pci_address.as_deref(), Some("0000:03:00.0"));
    }

    #[test]
    fn drive_addresses_and_volume_drives_parse_on_the_command_line() {
        let command = parse(&["-c", "1", "volume", "create", "5", "252:0", "252:1", ":4"]);
        let Command::Volume {
            action: Some(VolumeAction::Create { drives, stripe, .. }),
            ..
        } = command
        else {
            panic!("wrong command");
        };
        assert_eq!(drives.len(), 3);
        let last: DriveAddress = drives[2].parse().unwrap();
        assert_eq!(last.enclosure, pd::NO_ENCLOSURE);
        assert_eq!(stripe, None);
        assert!(CtrlSetting::parse("load-balance-mode", "1").is_err());
        assert!(CtrlSetting::parse("bgi-rate", "30").is_ok());
        let fails = |argv: &[&str]| {
            let mut full = vec!["sasctl"];
            full.extend_from_slice(argv);
            Cli::try_parse_from(full).is_err()
        };
        assert!(fails(&["volume", "create", "0", "1:1", "--init", "quick"]));
        assert!(fails(&["controller", "time", "set"]));
        assert!(fails(&["event", "clear"]));
    }

    fn reading_mock() -> Mock {
        use crate::mega::config::tests::config_bytes;
        use crate::mega::ld::tests::{ld_info_bytes, ld_list_bytes, ld_props_bytes};
        busy_mock()
            .reply(op::CTRL_GET_PROPS, vec![])
            .reply(op::TIME_SECS_GET, 100u32.to_le_bytes().to_vec())
            .reply(op::LD_GET_LIST, ld_list_bytes(&[(0, 3, 1000)]))
            .reply(
                op::LD_GET_INFO,
                ld_info_bytes(0, crate::mega::ld::DDF_RAID1, 2, &[(1000, 0)]),
            )
            .reply(op::LD_GET_PROPERTIES, ld_props_bytes(0, 1, "data", 0x65))
            .reply(op::CFG_READ, config_bytes(&[], &[], &[]))
            .reply(op::CFG_FOREIGN_SCAN, vec![])
            .reply(op::PR_GET_STATUS, vec![])
            .reply(op::PR_GET_PROPERTIES, vec![])
            .reply(op::EVENT_GET_INFO, vec![])
            .reply(op::EVENT_GET, vec![])
            .reply(op::SPEAKER_GET, vec![])
    }

    #[test]
    fn every_read_renders_json_and_text() {
        let reads: &[&[&str]] = &[
            &["controller"],
            &["controller", "settings"],
            &["controller", "time"],
            &["temperature"],
            &["drive"],
            &["drive", "252:0"],
            &["drive", "252:0", "smart"],
            &["drive", "252:0", "temperature"],
            &["drive", "252:0", "rebuild"],
            &["drive", "252:0", "erase"],
            &["volume"],
            &["volume", "0"],
            &["volume", "0", "settings"],
            &["volume", "0", "progress"],
            &["config"],
            &["foreign"],
            &["foreign", "preview"],
            &["battery"],
            &["patrol"],
            &["alarm"],
            &["event"],
            &["event", "info"],
            &["enclosure"],
            &["firmware"],
        ];
        for argv in reads {
            let mock = reading_mock();
            let out = execute(&parse(argv), &one(), &mock, &ctx(false))
                .unwrap_or_else(|e| panic!("{argv:?}: {e:#}"));
            let json = out
                .json()
                .unwrap_or_else(|e| panic!("{argv:?} json: {e:#}"));
            assert!(json.is_object(), "{argv:?} gave {json}");
            assert!(!out.text().trim().is_empty(), "{argv:?} rendered nothing");
            assert!(
                !mock.opcodes().iter().any(|o| [
                    op::CTRL_SET_PROPS,
                    op::PD_STATE_SET,
                    op::LD_SET_PROP,
                    op::CFG_ADD,
                    op::CFG_CLEAR,
                    op::PR_START,
                    op::PR_STOP,
                    op::BBU_START_LEARN
                ]
                .contains(o)),
                "{argv:?} sent a write"
            );
            assert_eq!(mock.resets(), 0, "{argv:?}");
        }
    }

    #[test]
    fn temperature_json_matches_the_go_fixture() {
        use crate::tests::{assert_matches_fixture, enveloped, go_fixture};
        let mock = reading_mock();
        let real = enveloped(
            1,
            DRIVER,
            execute(&parse(&["temperature"]), &one(), &mock, &ctx(false)),
        );
        assert!(real[0]["error"].is_null(), "{real}");
        assert_matches_fixture(
            &real,
            &go_fixture("sasctl_1_temperature.json"),
            "sasctl_1_temperature.json",
        );
        assert_eq!(real[0]["controller"], 1);
        assert_eq!(real[0]["drives"][0]["celsius"], 30);
    }

    #[test]
    fn commands_megaraid_lacks_are_refused_by_name() {
        let mock = reading_mock();
        for argv in [
            &["phy"][..],
            &["boot"],
            &["log"],
            &["event", "enable"],
            &["volume", "0", "check"],
            &["firmware", "save", "out.bin"],
        ] {
            let err = execute(&parse(argv), &one(), &mock, &ctx(true)).unwrap_err();
            assert!(err.to_string().contains("megaraid_sas"), "{argv:?}: {err}");
        }
        assert!(!supports(&parse(&["phy"])));
        assert!(!supports(&parse(&["volume", "0", "check"])));
        assert!(!supports(&parse(&["controller", "reset", "--snapdump"])));
        assert!(supports(&parse(&["controller", "reset"])));
    }

    #[test]
    fn volume_ids_above_255_are_refused_before_any_command() {
        let mock = reading_mock();
        let err = execute(&parse(&["volume", "256"]), &one(), &mock, &ctx(false)).unwrap_err();
        assert_eq!(err.to_string(), "volume 256 is out of range");
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn event_since_takes_names_and_numbers() {
        let info = event::LogInfo::parse(&{
            let mut b = vec![0u8; 20];
            for (i, v) in [900u32, 1, 5, 700, 710].iter().enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            b
        });
        assert_eq!(event_start(&info, "boot").unwrap(), 710);
        assert_eq!(event_start(&info, "SHUTDOWN").unwrap(), 700);
        assert_eq!(event_start(&info, "clear").unwrap(), 5);
        assert_eq!(event_start(&info, "oldest").unwrap(), 1);
        assert_eq!(event_start(&info, "newest").unwrap(), 900);
        assert_eq!(event_start(&info, "42").unwrap(), 42);
        assert!(event_start(&info, "yesterday").is_err());
    }
}
