use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};

use crate::Ctx;
use crate::mega::bbu;
use crate::mega::config::{
    self, INIT_FULL, INIT_NONE, INIT_QUICK, RaidLevel, SPARE_ENCL_AFFINITY, SPARE_REVERTIBLE,
    VolumeRequest, parse_stripe,
};
use crate::mega::ctrl::{self, AlarmAction, CtrlSetting};
use crate::mega::event::{self, EventQuery, parse_class, parse_locale};
use crate::mega::fw::{self, FirmwareInfo};
use crate::mega::ld::{self, LD_SETTABLE, LdSetting};
use crate::mega::patrol::{self, Mode, Schedule, parse_interval};
use crate::mega::pd::{self, DriveAddress, STATE_HOT_SPARE};
use crate::mega::report::{
    self, ClearProgress, ControllerEntry, ControllerList, ControllerSummary, RebuildProgress,
    TemperatureReport,
};
use crate::mega::reset;
use crate::mega::transport::Transport;
use crate::output::{Done, emit};
use crate::sysfs::{parse_pci_address, scsi_hosts};

pub const DRIVER: &str = "megaraid_sas";

#[derive(ClapArgs)]
pub struct Args {
    #[arg(
        short = 'c',
        long = "controller",
        global = true,
        help = "Controller index from `mega list`"
    )]
    pub controller: Option<usize>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "List controllers in PCI order")]
    List,
    #[command(about = "Controller information, properties and actions")]
    Controller {
        #[command(subcommand)]
        cmd: ControllerCmd,
    },
    #[command(about = "Controller temperatures")]
    Temperature {
        #[command(subcommand)]
        cmd: ShowCmd,
    },
    #[command(about = "Physical drives, addressed as enclosure:slot")]
    Drive {
        #[command(subcommand)]
        cmd: DriveCmd,
    },
    #[command(about = "Virtual drives")]
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    #[command(about = "RAID configuration")]
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    #[command(about = "Foreign configurations")]
    Foreign {
        #[command(subcommand)]
        cmd: ForeignCmd,
    },
    #[command(about = "Battery backup unit")]
    Bbu {
        #[command(subcommand)]
        cmd: BbuCmd,
    },
    #[command(about = "CacheVault module")]
    Cachevault {
        #[command(subcommand)]
        cmd: ShowCmd,
    },
    #[command(about = "Patrol read")]
    Patrol {
        #[command(subcommand)]
        cmd: PatrolCmd,
    },
    #[command(about = "Controller event log")]
    Event {
        #[command(subcommand)]
        cmd: EventCmd,
    },
    #[command(about = "Enclosures seen in the drive list")]
    Enclosure {
        #[command(subcommand)]
        cmd: EnclosureCmd,
    },
    #[command(about = "Controller firmware")]
    Firmware {
        #[command(subcommand)]
        cmd: FirmwareCmd,
    },
    #[command(about = "Controller alarm")]
    Alarm {
        #[command(subcommand)]
        cmd: AlarmCmd,
    },
}

#[derive(Subcommand)]
pub enum ShowCmd {
    #[command(about = "Show")]
    Show,
}

#[derive(Subcommand)]
pub enum ControllerCmd {
    #[command(about = "Show controller information")]
    Show,
    #[command(about = "Show controller properties")]
    Props,
    #[command(about = "Change a controller property")]
    Set {
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(ctrl::settable()))]
        prop: String,
        value: String,
    },
    #[command(about = "Show the controller clock")]
    Time,
    #[command(about = "Reset the controller through the Linux driver's online controller reset")]
    Reset,
    #[command(about = "Shut the controller down")]
    Shutdown {
        #[arg(long, help = "Spin drives down as well")]
        spindown: bool,
    },
    #[command(about = "Flush the controller cache")]
    Flush {
        #[arg(long, help = "Flush drive caches as well")]
        disks: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum DriveState {
    Good,
    Offline,
    Online,
    Jbod,
}

impl DriveState {
    fn code(self) -> u16 {
        match self {
            DriveState::Good => pd::STATE_UNCONFIGURED_GOOD,
            DriveState::Offline => pd::STATE_OFFLINE,
            DriveState::Online => pd::STATE_ONLINE,
            DriveState::Jbod => pd::STATE_SYSTEM,
        }
    }

    fn name(self) -> &'static str {
        match self {
            DriveState::Good => "unconfigured good",
            DriveState::Offline => "offline",
            DriveState::Online => "online",
            DriveState::Jbod => "JBOD",
        }
    }
}

#[derive(Subcommand)]
pub enum DriveCmd {
    #[command(about = "List drives")]
    List,
    #[command(about = "Show one drive")]
    Show { drive: DriveAddress },
    #[command(about = "Blink the drive's locate LED")]
    Locate {
        drive: DriveAddress,
        #[arg(long, conflicts_with = "off", required_unless_present = "off")]
        on: bool,
        #[arg(long)]
        off: bool,
    },
    #[command(about = "Change the drive state")]
    State {
        drive: DriveAddress,
        state: DriveState,
    },
    #[command(about = "Drive rebuild")]
    Rebuild {
        #[command(subcommand)]
        cmd: RebuildCmd,
    },
    #[command(about = "Hot spares")]
    Hotspare {
        #[command(subcommand)]
        cmd: HotspareCmd,
    },
    #[command(about = "Clear a drive by overwriting it")]
    Clear {
        #[command(subcommand)]
        cmd: ClearCmd,
    },
    #[command(about = "Error counters and the drive's own failure prediction")]
    Smart { drive: DriveAddress },
    #[command(about = "Drive temperatures")]
    Temperature { drive: Option<DriveAddress> },
}

#[derive(Subcommand)]
pub enum RebuildCmd {
    #[command(about = "Start a rebuild")]
    Start { drive: DriveAddress },
    #[command(about = "Abort a rebuild")]
    Stop { drive: DriveAddress },
    #[command(about = "Show rebuild progress")]
    Progress { drive: DriveAddress },
}

#[derive(Subcommand)]
pub enum ClearCmd {
    #[command(about = "Start clearing a drive")]
    Start { drive: DriveAddress },
    #[command(about = "Abort a running clear")]
    Stop { drive: DriveAddress },
    #[command(about = "Show clear progress")]
    Progress { drive: DriveAddress },
}

#[derive(Subcommand)]
pub enum HotspareCmd {
    #[command(about = "Make an unconfigured good drive a hot spare")]
    Add {
        drive: DriveAddress,
        #[arg(long, help = "Dedicate the spare to this virtual drive")]
        volume: Option<u8>,
        #[arg(long, help = "Mark the spare revertible")]
        revertible: bool,
        #[arg(long, help = "Mark the spare with enclosure affinity")]
        affinity: bool,
    },
    #[command(about = "Remove a hot spare")]
    Remove { drive: DriveAddress },
}

#[derive(Subcommand)]
pub enum ProgressCmd {
    #[command(about = "Show progress")]
    Progress { vd: u8 },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum VolumeInit {
    None,
    Fast,
    Full,
}

impl VolumeInit {
    fn code(self) -> u8 {
        match self {
            VolumeInit::None => INIT_NONE,
            VolumeInit::Fast => INIT_QUICK,
            VolumeInit::Full => INIT_FULL,
        }
    }
}

#[derive(Subcommand)]
pub enum VolumeCmd {
    #[command(about = "List virtual drives")]
    List,
    #[command(about = "Show one virtual drive")]
    Show { vd: u8 },
    #[command(about = "Create a RAID 0, 1, 5, 6, 10, 50 or 60 virtual drive")]
    Create {
        #[arg(long, help = "RAID level: 0, 1, 5, 6, 10, 50 or 60")]
        raid: String,
        #[arg(
            long,
            value_delimiter = ',',
            required = true,
            help = "Drives as e:s,e:s,..."
        )]
        drives: Vec<DriveAddress>,
        #[arg(long, default_value = "64k", help = "Strip size, a power of two")]
        stripe: String,
        #[arg(long, help = "Volume name, up to 15 characters")]
        name: Option<String>,
        #[arg(long, help = "Drives per array for RAID 10, 50 and 60")]
        pd_per_array: Option<usize>,
        #[arg(
            long,
            value_enum,
            default_value = "none",
            help = "Initialize at creation"
        )]
        init: VolumeInit,
    },
    #[command(about = "Delete a virtual drive")]
    Delete { vd: u8 },
    #[command(about = "Show virtual drive properties")]
    Props { vd: u8 },
    #[command(about = "Change a virtual drive property")]
    Set {
        vd: u8,
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(LD_SETTABLE))]
        prop: String,
        value: String,
    },
    #[command(about = "Initialization")]
    Init {
        #[command(subcommand)]
        cmd: ProgressCmd,
    },
    #[command(about = "Consistency check")]
    Check {
        #[command(subcommand)]
        cmd: ProgressCmd,
    },
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    #[command(about = "Show arrays, virtual drives and spares")]
    Show,
    #[command(about = "Delete every array and virtual drive")]
    Clear,
    #[command(about = "Save the raw configuration data to a file")]
    Save { file: PathBuf },
}

#[derive(Subcommand)]
pub enum ForeignCmd {
    #[command(about = "Scan for foreign configurations")]
    Scan,
    #[command(about = "Show what an import would produce")]
    Preview {
        #[arg(long)]
        index: Option<u8>,
    },
    #[command(about = "Import every foreign configuration, or one by index")]
    Import {
        #[arg(long)]
        index: Option<u8>,
    },
    #[command(about = "Discard every foreign configuration")]
    Clear,
}

#[derive(Subcommand)]
pub enum BbuCmd {
    #[command(about = "Show battery status")]
    Show,
    #[command(about = "Start a learn cycle")]
    Learn,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum PatrolMode {
    Auto,
    Manual,
    Off,
}

#[derive(Subcommand)]
pub enum PatrolCmd {
    #[command(about = "Show patrol read status and schedule")]
    Show,
    #[command(about = "Start patrol read")]
    Start,
    #[command(about = "Stop patrol read")]
    Stop,
    #[command(about = "Change patrol read mode and schedule")]
    Set {
        #[arg(long)]
        mode: PatrolMode,
        #[arg(long, help = "Seconds between runs, or continuous (auto mode)")]
        interval: Option<String>,
        #[arg(long, help = "Seconds from now until the next run (auto mode)")]
        start_in: Option<u32>,
    },
}

#[derive(Subcommand)]
pub enum EventCmd {
    #[command(about = "Show event log sequence numbers")]
    Info,
    #[command(about = "List events")]
    List {
        #[arg(
            long,
            default_value = "info",
            help = "Lowest class: debug, progress, info, warning, critical, fatal, dead"
        )]
        class: String,
        #[arg(
            long,
            default_value = "all",
            help = "Locales: ld, pd, enclosure, bbu, sas, controller, config, cluster, all"
        )]
        locale: String,
        #[arg(
            long,
            default_value = "boot",
            help = "Start at boot, shutdown, clear, oldest, newest or a sequence number"
        )]
        since: String,
        #[arg(
            long,
            default_value_t = 100,
            help = "Show at most this many of the newest matching events"
        )]
        count: usize,
    },
}

#[derive(Subcommand)]
pub enum EnclosureCmd {
    #[command(about = "List enclosures")]
    List,
}

#[derive(Subcommand)]
pub enum FirmwareCmd {
    #[command(about = "Show firmware versions")]
    Show,
    #[command(about = "Flash a firmware image")]
    Flash { file: PathBuf },
}

#[derive(Subcommand)]
pub enum AlarmCmd {
    #[command(about = "Show alarm state")]
    Show,
    #[command(about = "Enable the alarm")]
    On,
    #[command(about = "Disable the alarm")]
    Off,
    #[command(about = "Silence a sounding alarm")]
    Silence,
}

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

pub fn select(ctrls: &[ControllerRef], index: Option<usize>) -> Result<&ControllerRef> {
    match index {
        Some(i) => ctrls
            .get(i)
            .ok_or_else(|| anyhow!("controller {i} does not exist, {} found", ctrls.len())),
        None => match ctrls.len() {
            0 => bail!("no MegaRAID controllers found"),
            1 => Ok(&ctrls[0]),
            n => bail!("{n} controllers found, pick one with -c"),
        },
    }
}

pub type Opener<'a> = &'a dyn Fn(u32) -> Result<Box<dyn Transport>>;

pub fn list(ctrls: &[ControllerRef], open: Opener<'_>) -> ControllerList {
    let controllers = ctrls
        .iter()
        .map(|c| {
            let info = open(c.host_no).and_then(|t| ctrl::get_info(t.as_ref()));
            let mut e = ControllerEntry {
                index: c.index,
                host_no: c.host_no,
                pci_address: c.pci_address.clone(),
                product_name: None,
                serial_number: None,
                package_version: None,
                firmware_version: None,
                volumes: None,
                drives: None,
                roc_celsius: None,
                error: None,
            };
            match info {
                Ok(i) => {
                    e.firmware_version = Some(i.firmware_version());
                    e.product_name = Some(i.product_name);
                    e.serial_number = Some(i.serial_number);
                    e.package_version = Some(i.package_version);
                    e.volumes = Some(i.ld_present);
                    e.drives = Some(i.pd_disk_present);
                    e.roc_celsius = i.temperatures.roc_celsius;
                }
                Err(err) => e.error = Some(format!("{err:#}")),
            }
            e
        })
        .collect();
    ControllerList { controllers }
}

pub fn run(args: Args, ctx: &Ctx, open: Opener<'_>) -> Result<()> {
    let ctrls = controllers(&ctx.sysfs);
    if let Command::List = args.command {
        return emit(ctx.format, &list(&ctrls, open));
    }
    let c = select(&ctrls, args.controller)?;
    let t = open(c.host_no)?;
    execute(args.command, c, t.as_ref(), ctx)
}

fn host_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn done(ctx: &Ctx, message: String) -> Result<()> {
    emit(ctx.format, &Done::ok(message))
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

pub fn execute(command: Command, c: &ControllerRef, t: &dyn Transport, ctx: &Ctx) -> Result<()> {
    let f = ctx.format;
    match command {
        Command::List => unreachable!("list is handled before a controller is opened"),
        Command::Controller { cmd } => match cmd {
            ControllerCmd::Show => {
                let info = ctrl::get_info(t)?;
                let summary = ControllerSummary {
                    index: c.index,
                    host_no: u32::from(t.host_no()),
                    pci_address: c.pci_address.clone(),
                    driver_version: report::driver_version(&ctx.sysfs),
                    controller_time: ctrl::get_time(t).ok().map(event::format_fw_time),
                    info,
                };
                emit(f, &summary)
            }
            ControllerCmd::Props => emit(f, &ctrl::get_props(t)?),
            ControllerCmd::Set { prop, value } => {
                let setting = CtrlSetting::parse(&prop, &value)?;
                ctx.confirm(&format!(
                    "setting {prop} to {value} on controller {}",
                    c.index
                ))?;
                if setting.enables_jbod() && !ctrl::get_info(t)?.support_jbod {
                    bail!("controller {} does not support JBOD", c.index);
                }
                emit(f, &ctrl::set_property(t, setting)?)
            }
            ControllerCmd::Reset => {
                ctx.confirm(&format!(
                    "resetting controller {} through the driver, which drops outstanding IO",
                    c.index
                ))?;
                reset::check_allowed(&ctrl::get_props(t)?)?;
                t.reset_host(&ctx.sysfs)?;
                done(ctx, format!("controller {} reset", c.index))
            }
            ControllerCmd::Time => emit(f, &report::time_report(ctrl::get_time(t)?, host_now())),
            ControllerCmd::Shutdown { spindown } => {
                ctx.confirm(&format!("shutting down controller {}", c.index))?;
                ctrl::shutdown(t, spindown)?;
                done(ctx, format!("controller {} shut down", c.index))
            }
            ControllerCmd::Flush { disks } => {
                ctx.confirm(&format!("flushing the cache of controller {}", c.index))?;
                ctrl::flush_cache(t, disks)?;
                done(ctx, format!("controller {} cache flushed", c.index))
            }
        },
        Command::Temperature { cmd: ShowCmd::Show } => {
            let info = ctrl::get_info(t)?;
            emit(
                f,
                &TemperatureReport {
                    controller: c.index,
                    roc_celsius: info.temperatures.roc_celsius,
                    controller_celsius: info.temperatures.controller_celsius,
                },
            )
        }
        Command::Drive { cmd } => drive(cmd, t, ctx),
        Command::Volume { cmd } => volume(cmd, t, ctx),
        Command::Config { cmd } => match cmd {
            ConfigCmd::Show => emit(f, &config::read(t)?),
            ConfigCmd::Clear => {
                ctx.confirm(&format!(
                    "deleting every virtual drive on controller {}",
                    c.index
                ))?;
                config::clear(t)?;
                done(ctx, "configuration cleared".into())
            }
            ConfigCmd::Save { file } => {
                let raw = config::read_raw(t)?;
                std::fs::write(&file, &raw)
                    .with_context(|| format!("writing {}", file.display()))?;
                done(
                    ctx,
                    format!(
                        "saved {} bytes of configuration to {}",
                        raw.len(),
                        file.display()
                    ),
                )
            }
        },
        Command::Foreign { cmd } => match cmd {
            ForeignCmd::Scan => emit(f, &report::foreign_report(t, false, None)?),
            ForeignCmd::Preview { index } => emit(f, &report::foreign_report(t, true, index)?),
            ForeignCmd::Import { index: None } => {
                ctx.confirm("importing every foreign configuration")?;
                config::foreign_import_all(t)?;
                done(ctx, "foreign configurations imported".into())
            }
            ForeignCmd::Import { index: Some(i) } => {
                ctx.confirm(&format!("importing foreign configuration {i}"))?;
                config::foreign_import(t, i)?;
                done(ctx, format!("foreign configuration {i} imported"))
            }
            ForeignCmd::Clear => {
                ctx.confirm("discarding every foreign configuration")?;
                config::foreign_clear(t)?;
                done(ctx, "foreign configurations cleared".into())
            }
        },
        Command::Bbu { cmd } => match cmd {
            BbuCmd::Show => {
                let present = ctrl::get_info(t).map(|i| i.bbu_present).unwrap_or(false);
                emit(f, &bbu::report(t, "bbu", present))
            }
            BbuCmd::Learn => {
                ctx.confirm("starting a battery learn cycle")?;
                bbu::start_learn(t)?;
                done(ctx, "learn cycle started".into())
            }
        },
        Command::Cachevault { cmd: ShowCmd::Show } => {
            let present = ctrl::get_info(t).map(|i| i.bbu_present).unwrap_or(false);
            emit(f, &bbu::report(t, "cachevault", present))
        }
        Command::Patrol { cmd } => match cmd {
            PatrolCmd::Show => emit(f, &patrol::report(t)?),
            PatrolCmd::Start => {
                ctx.confirm("starting patrol read")?;
                patrol::start(t)?;
                done(ctx, "patrol read started".into())
            }
            PatrolCmd::Stop => {
                ctx.confirm("stopping patrol read")?;
                patrol::stop(t)?;
                done(ctx, "patrol read stopped".into())
            }
            PatrolCmd::Set {
                mode,
                interval,
                start_in,
            } => {
                let sched = Schedule {
                    mode: match mode {
                        PatrolMode::Auto => Mode::Auto,
                        PatrolMode::Manual => Mode::Manual,
                        PatrolMode::Off => Mode::Disabled,
                    },
                    interval: interval.as_deref().map(parse_interval).transpose()?,
                    start_in,
                };
                ctx.confirm("changing the patrol read schedule")?;
                emit(f, &patrol::configure(t, sched)?)
            }
        },
        Command::Event { cmd } => match cmd {
            EventCmd::Info => emit(f, &event::log_info(t)?),
            EventCmd::List {
                class,
                locale,
                since,
                count,
            } => {
                let class = parse_class(&class)?;
                let locale = parse_locale(&locale)?;
                let info = event::log_info(t)?;
                let q = EventQuery {
                    start: event_start(&info, &since)?,
                    stop: info.newest_seq,
                    class,
                    locale,
                    limit: count,
                };
                emit(
                    f,
                    &report::EventList {
                        events: event::fetch(t, &q)?,
                    },
                )
            }
        },
        Command::Enclosure {
            cmd: EnclosureCmd::List,
        } => emit(f, &report::enclosures(&pd::get_list(t)?)),
        Command::Firmware { cmd } => match cmd {
            FirmwareCmd::Show => emit(f, &FirmwareInfo::from_info(&ctrl::get_info(t)?)),
            FirmwareCmd::Flash { file } => {
                let image =
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
                fw::validate(&image)?;
                ctx.confirm(&format!(
                    "flashing {} onto controller {}",
                    file.display(),
                    c.index
                ))?;
                fw::flash(t, &image)?;
                emit(f, &FirmwareInfo::from_info(&ctrl::get_info(t)?))
            }
        },
        Command::Alarm { cmd } => {
            let action = match cmd {
                AlarmCmd::Show => return emit(f, &report::alarm_report(t)?),
                AlarmCmd::On => AlarmAction::Enable,
                AlarmCmd::Off => AlarmAction::Disable,
                AlarmCmd::Silence => AlarmAction::Silence,
            };
            ctx.confirm("changing the controller alarm")?;
            ctrl::alarm(t, action)?;
            done(ctx, "alarm updated".into())
        }
    }
}

fn drive(cmd: DriveCmd, t: &dyn Transport, ctx: &Ctx) -> Result<()> {
    let f = ctx.format;
    match cmd {
        DriveCmd::List => emit(f, &report::drive_list(t)?),
        DriveCmd::Show { drive } => emit(f, &report::drive_detail(t, drive)?),
        DriveCmd::Locate { drive, on, off } => {
            let on = on && !off;
            ctx.confirm(&format!(
                "turning the locate LED of drive {drive} {}",
                if on { "on" } else { "off" }
            ))?;
            let a = report::locate_drive(t, drive)?;
            pd::locate(t, a.device_id, on)?;
            done(
                ctx,
                format!(
                    "locate {} for drive {drive}",
                    if on { "started" } else { "stopped" }
                ),
            )
        }
        DriveCmd::State { drive, state } => {
            ctx.confirm(&format!("setting drive {drive} {}", state.name()))?;
            let a = report::locate_drive(t, drive)?;
            pd::set_state(t, a.device_id, state.code())?;
            done(ctx, format!("drive {drive} set {}", state.name()))
        }
        DriveCmd::Rebuild { cmd } => match cmd {
            RebuildCmd::Start { drive } => {
                ctx.confirm(&format!("starting a rebuild on drive {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                pd::rebuild_start(t, a.device_id)?;
                done(ctx, format!("rebuild started on drive {drive}"))
            }
            RebuildCmd::Stop { drive } => {
                ctx.confirm(&format!("aborting the rebuild on drive {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                pd::rebuild_stop(t, a.device_id)?;
                done(ctx, format!("rebuild stopped on drive {drive}"))
            }
            RebuildCmd::Progress { drive } => {
                let a = report::locate_drive(t, drive)?;
                let info = pd::get_info(t, a.device_id)?;
                emit(
                    f,
                    &RebuildProgress {
                        address: drive,
                        state: info.state,
                        rebuild: info.progress.rebuild,
                    },
                )
            }
        },
        DriveCmd::Hotspare { cmd } => match cmd {
            HotspareCmd::Add {
                drive,
                volume,
                revertible,
                affinity,
            } => {
                let what = match volume {
                    Some(v) => {
                        format!("making drive {drive} a dedicated hot spare for virtual drive {v}")
                    }
                    None => format!("making drive {drive} a global hot spare"),
                };
                ctx.confirm(&what)?;
                refuse_extended_config(t)?;
                let a = report::locate_drive(t, drive)?;
                let info = pd::get_info(t, a.device_id)?;
                let cfg = config::read(t)?;
                let mut flags = 0;
                if revertible {
                    flags |= SPARE_REVERTIBLE;
                }
                if affinity {
                    flags |= SPARE_ENCL_AFFINITY;
                }
                let data = config::build_spare(&cfg, &info, volume, flags)?;
                config::make_spare(t, &data)?;
                done(ctx, format!("drive {drive} is now a hot spare"))
            }
            HotspareCmd::Remove { drive } => {
                ctx.confirm(&format!("removing hot spare {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                let info = pd::get_info(t, a.device_id)?;
                if info.fw_state != STATE_HOT_SPARE {
                    bail!("drive {drive} is {} and not a hot spare", info.state);
                }
                config::remove_spare(t, info.device_id, info.seq_num)?;
                done(ctx, format!("drive {drive} is no longer a hot spare"))
            }
        },
        DriveCmd::Clear { cmd } => match cmd {
            ClearCmd::Start { drive } => {
                ctx.confirm(&format!(
                    "clearing drive {drive}, which erases everything on it"
                ))?;
                let a = report::locate_drive(t, drive)?;
                pd::clear_start(t, a.device_id)?;
                done(ctx, format!("clear started on drive {drive}"))
            }
            ClearCmd::Stop { drive } => {
                ctx.confirm(&format!("aborting the clear on drive {drive}"))?;
                let a = report::locate_drive(t, drive)?;
                pd::clear_stop(t, a.device_id)?;
                done(ctx, format!("clear stopped on drive {drive}"))
            }
            ClearCmd::Progress { drive } => {
                let a = report::locate_drive(t, drive)?;
                let info = pd::get_info(t, a.device_id)?;
                emit(
                    f,
                    &ClearProgress {
                        address: drive,
                        state: info.state,
                        clear: info.progress.clear,
                    },
                )
            }
        },
        DriveCmd::Smart { drive } => emit(f, &report::drive_smart(t, drive)?),
        DriveCmd::Temperature { drive: Some(drive) } => {
            emit(f, &report::drive_temperature(t, drive)?)
        }
        DriveCmd::Temperature { drive: None } => emit(f, &report::drive_temperatures(t)?),
    }
}

fn volume(cmd: VolumeCmd, t: &dyn Transport, ctx: &Ctx) -> Result<()> {
    let f = ctx.format;
    match cmd {
        VolumeCmd::List => emit(f, &report::volume_list(t)?),
        VolumeCmd::Show { vd } => emit(f, &report::volume_detail(t, vd)?),
        VolumeCmd::Create {
            raid,
            drives,
            stripe,
            name,
            pd_per_array,
            init,
        } => {
            let level = RaidLevel::parse(&raid)?;
            let stripe_bytes = parse_stripe(&stripe)?;
            let list: Vec<String> = drives.iter().map(DriveAddress::to_string).collect();
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
            let infos = drives
                .iter()
                .map(|d| pd::get_info(t, pd::resolve(&pds, *d)?.device_id))
                .collect::<Result<Vec<_>>>()?;
            let cfg = config::read(t)?;
            let data = config::build_volume(
                &cfg,
                &VolumeRequest {
                    level,
                    drives: &infos,
                    drives_per_array: pd_per_array,
                    stripe_bytes,
                    name: name.as_deref(),
                    init_state: init.code(),
                },
            )?;
            let target = config::new_target_id(&data);
            config::add(t, &data)?;
            done(ctx, format!("created virtual drive {target}"))
        }
        VolumeCmd::Delete { vd } => {
            ctx.confirm(&format!("deleting virtual drive {vd} and its data"))?;
            ld::delete(t, vd)?;
            done(ctx, format!("virtual drive {vd} deleted"))
        }
        VolumeCmd::Props { vd } => emit(f, &ld::get_props(t, vd)?),
        VolumeCmd::Set { vd, prop, value } => {
            let setting = LdSetting::parse(&prop, &value)?;
            ctx.confirm(&format!("setting {prop} to {value} on virtual drive {vd}"))?;
            emit(f, &ld::set_property(t, vd, &setting)?)
        }
        VolumeCmd::Init {
            cmd: ProgressCmd::Progress { vd },
        } => emit(f, &report::volume_init_progress(t, vd)?),
        VolumeCmd::Check {
            cmd: ProgressCmd::Progress { vd },
        } => emit(f, &report::volume_check_progress(t, vd)?),
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

    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        args: Args,
    }

    fn parse(argv: &[&str]) -> Args {
        let mut full = vec!["mega"];
        full.extend_from_slice(argv);
        Harness::try_parse_from(full).unwrap().args
    }

    fn ctx(yes: bool) -> Ctx {
        Ctx {
            format: Format::Json,
            yes,
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
            &["drive", "locate", "252:0", "--on"],
            &["drive", "state", "252:0", "offline"],
            &["drive", "rebuild", "start", "252:0"],
            &["drive", "rebuild", "stop", "252:0"],
            &["drive", "hotspare", "add", "252:0"],
            &["drive", "hotspare", "remove", "252:0"],
            &["volume", "create", "--raid", "1", "--drives", "252:0,252:1"],
            &["volume", "delete", "0"],
            &["volume", "set", "0", "write-cache", "wb"],
            &["config", "clear"],
            &["foreign", "import"],
            &["foreign", "clear"],
            &["bbu", "learn"],
            &["patrol", "start"],
            &["patrol", "stop"],
            &["patrol", "set", "--mode", "manual"],
            &["alarm", "on"],
            &["alarm", "off"],
            &["alarm", "silence"],
            &["controller", "set", "bgi-rate", "30"],
            &["controller", "set", "jbod", "on"],
            &["controller", "set", "coercion", "1g"],
            &["controller", "reset"],
            &["drive", "clear", "start", "252:0"],
            &["drive", "clear", "stop", "252:0"],
            &[
                "drive",
                "hotspare",
                "add",
                "252:0",
                "--revertible",
                "--affinity",
            ],
            &[
                "volume",
                "create",
                "--raid",
                "10",
                "--drives",
                "252:0,252:1,252:2,252:3",
                "--pd-per-array",
                "2",
                "--init",
                "full",
            ],
            &["volume", "set", "0", "autobgi", "off"],
            &["foreign", "import", "--index", "1"],
        ];
        for argv in changing {
            let mock = busy_mock();
            let err = execute(parse(argv).command, &one(), &mock, &ctx(false)).unwrap_err();
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
            parse(&["firmware", "flash", bad.to_str().unwrap()]).command,
            &one(),
            &mock,
            &ctx(false),
        )
        .unwrap_err();
        assert!(e.to_string().contains("multiple of 1024"));
        let e = execute(
            parse(&["firmware", "flash", good.to_str().unwrap()]).command,
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
            parse(&["drive", "state", "252:0", "jbod"]).command,
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
            parse(&["volume", "create", "--raid", "0", "--drives", "252:0"]).command,
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
        let e = execute(
            parse(&["controller", "reset"]).command,
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap_err();
        assert!(e.to_string().contains("refused"), "{e}");
        assert_eq!(mock.opcodes(), vec![op::CTRL_GET_PROPS]);
        assert_eq!(mock.resets(), 0);
    }

    #[test]
    fn reset_goes_through_the_host_reset_when_allowed() {
        let mock = Mock::new().reply(op::CTRL_GET_PROPS, props_with(33, 0x20));
        execute(
            parse(&["controller", "reset"]).command,
            &one(),
            &mock,
            &ctx(true),
        )
        .unwrap();
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
            parse(&["controller", "set", "jbod", "on"]).command,
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
            parse(&["controller", "set", "bgi-rate", "45"]).command,
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
            parse(&["controller", "set", "bgi-rate", "101"]).command,
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
            parse(&[
                "volume",
                "create",
                "--raid",
                "10",
                "--drives",
                "252:0,252:1,252:2,252:3",
                "--pd-per-array",
                "2",
                "--init",
                "fast",
            ])
            .command,
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
            parse(&[
                "volume",
                "create",
                "--raid",
                "10",
                "--drives",
                "252:0,252:1,252:2,252:3",
                "--pd-per-array",
                "2",
            ])
            .command,
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
            parse(&[
                "drive",
                "hotspare",
                "add",
                "252:1",
                "--revertible",
                "--affinity",
            ])
            .command,
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
            parse(&["foreign", "import", "--index", "1"]).command,
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
            parse(&["drive", "clear", "start", "252:0"]).command,
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
            parse(&["drive", "clear", "progress", "252:0"]).command,
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
        execute(
            parse(&["temperature", "show"]).command,
            &one(),
            &mock,
            &ctx(false),
        )
        .unwrap();
        execute(
            parse(&["enclosure", "list"]).command,
            &one(),
            &mock,
            &ctx(false),
        )
        .unwrap();
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
        assert_eq!(select(&ctrls, Some(2)).unwrap().host_no, 0);
        assert!(select(&ctrls, Some(4)).is_err());
        assert!(select(&ctrls, None).is_err());
        assert_eq!(select(&ctrls[..1], None).unwrap().host_no, 1);
    }

    #[test]
    fn list_reports_each_controller_and_keeps_going_on_errors() {
        let ctrls = vec![
            ControllerRef {
                index: 0,
                host_no: 4,
                pci_address: Some("0000:03:00.0".into()),
            },
            ControllerRef {
                index: 1,
                host_no: 7,
                pci_address: None,
            },
        ];
        let open = |host: u32| -> Result<Box<dyn Transport>> {
            if host == 4 {
                Ok(Box::new(Mock::new().reply(
                    op::CTRL_GET_INFO,
                    ctrl_info_bytes("MegaRAID 9460-8i", "SK1", 52, 61),
                )))
            } else {
                bail!("no such host")
            }
        };
        let l = list(&ctrls, &open);
        assert_eq!(
            l.controllers[0].product_name.as_deref(),
            Some("MegaRAID 9460-8i")
        );
        assert_eq!(l.controllers[0].roc_celsius, Some(52));
        assert_eq!(l.controllers[0].volumes, Some(2));
        assert!(
            l.controllers[1]
                .error
                .as_deref()
                .unwrap()
                .contains("no such host")
        );
    }

    #[test]
    fn drive_addresses_and_volume_drives_parse_on_the_command_line() {
        let a = parse(&[
            "-c",
            "1",
            "volume",
            "create",
            "--raid",
            "5",
            "--drives",
            "252:0,252:1,:4",
        ]);
        assert_eq!(a.controller, Some(1));
        let Command::Volume {
            cmd: VolumeCmd::Create { drives, stripe, .. },
        } = a.command
        else {
            panic!("wrong command");
        };
        assert_eq!(drives.len(), 3);
        assert_eq!(drives[2].enclosure, pd::NO_ENCLOSURE);
        assert_eq!(stripe, "64k");
        assert!(Harness::try_parse_from(["mega", "drive", "locate", "252:0"]).is_err());
        assert!(
            Harness::try_parse_from(["mega", "controller", "set", "load-balance-mode", "1"])
                .is_err()
        );
        assert!(Harness::try_parse_from(["mega", "controller", "set", "bgi-rate", "30"]).is_ok());
        assert!(
            Harness::try_parse_from([
                "mega", "volume", "create", "--raid", "0", "--drives", "1:1", "--init", "quick"
            ])
            .is_err()
        );
        assert!(Harness::try_parse_from(["mega", "controller", "time", "set"]).is_err());
        assert!(Harness::try_parse_from(["mega", "event", "clear"]).is_err());
    }
}
