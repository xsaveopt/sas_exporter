use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Args as ClapArgs, Subcommand};
use serde::Serialize;

use super::adapter::Target;
use super::diag::{self, BufferType, DiagQuery};
use super::flash;
use super::fw::{self, ImageHeader};
use super::inventory::{self, BootTarget, DriveAddress, LogInfo};
use super::ircfg::{self, CreateSpec, Level};
use super::mpi;
use super::raid;
use super::render::Emit;
use super::transport::Transport;
use crate::Ctx;
use crate::output::Done;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(
        short = 'c',
        long = "controller",
        value_name = "INDEX",
        help = "Controller index from `sasctl mpt list`"
    )]
    pub controller: Option<usize>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "List SAS2 and SAS3 adapters")]
    List,
    #[command(about = "Controller information and reset")]
    Controller {
        #[command(subcommand)]
        action: ControllerAction,
    },
    #[command(about = "Physical drives, addressed as enclosure:slot")]
    Drive {
        #[command(subcommand)]
        action: DriveAction,
    },
    #[command(about = "Integrated RAID volumes")]
    Volume {
        #[command(subcommand)]
        action: VolumeAction,
    },
    #[command(about = "Integrated RAID hot spares")]
    Hotspare {
        #[command(subcommand)]
        action: HotspareAction,
    },
    #[command(about = "Enclosures")]
    Enclosure {
        #[command(subcommand)]
        action: EnclosureAction,
    },
    #[command(about = "Controller phys")]
    Phy {
        #[command(subcommand)]
        action: PhyAction,
    },
    #[command(about = "Controller temperature sensors")]
    Temperature {
        #[command(subcommand)]
        action: TemperatureAction,
    },
    #[command(about = "Driver event log")]
    Event {
        #[command(subcommand)]
        action: EventAction,
    },
    #[command(about = "BIOS boot devices")]
    Boot {
        #[command(subcommand)]
        action: BootAction,
    },
    #[command(about = "Controller persistent log")]
    Log {
        #[command(subcommand)]
        action: LogAction,
    },
    #[command(about = "Controller firmware")]
    Firmware {
        #[command(subcommand)]
        action: FirmwareAction,
    },
    #[command(about = "Controller BIOS")]
    Bios {
        #[command(subcommand)]
        action: BiosAction,
    },
    #[command(about = "Firmware diagnostic buffers")]
    Diag {
        #[command(subcommand)]
        action: DiagAction,
    },
}

#[derive(Subcommand)]
pub enum ControllerAction {
    #[command(about = "Show controller details")]
    Show,
    #[command(about = "Hard reset the controller")]
    Reset,
}

#[derive(Subcommand)]
pub enum DriveAction {
    #[command(about = "List drives and enclosure services devices")]
    List,
    #[command(about = "Show one drive")]
    Show {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
    },
    #[command(about = "Turn the locate LED of a slot on or off")]
    #[command(group(ArgGroup::new("led").required(true).args(["on", "off"])))]
    Locate {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
        #[arg(long, help = "Turn the LED on")]
        on: bool,
        #[arg(long, help = "Turn the LED off")]
        off: bool,
    },
    #[command(about = "Bring an Integrated RAID member online")]
    Online {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
    },
    #[command(about = "Take an Integrated RAID member offline")]
    Offline {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
    },
}

#[derive(Subcommand)]
pub enum VolumeAction {
    #[command(about = "List volumes")]
    List,
    #[command(about = "Show one volume")]
    Show {
        #[arg(value_name = "ID", help = "Volume id from `volume list`")]
        id: u16,
    },
    #[command(about = "Show volume state and running operations")]
    Status,
    #[command(about = "Activate an inactive volume")]
    Activate {
        #[arg(value_name = "ID", help = "Volume id from `volume list`")]
        id: u16,
    },
    #[command(about = "Start a consistency check")]
    Check {
        #[arg(value_name = "ID", help = "Volume id from `volume list`")]
        id: u16,
    },
    #[command(about = "Create a volume from unconfigured drives")]
    Create {
        #[arg(long, value_enum)]
        level: Level,
        #[arg(value_name = "ENCLOSURE:SLOT", required = true, num_args = 1..)]
        drives: Vec<DriveAddress>,
        #[arg(
            long,
            value_name = "MB",
            help = "Volume size, defaults to the largest possible"
        )]
        size: Option<u64>,
        #[arg(long, help = "Volume name, up to 15 ASCII characters")]
        name: Option<String>,
        #[arg(
            long,
            value_name = "KB",
            help = "Stripe size, defaults to 128 KB within the supported range"
        )]
        stripe: Option<u32>,
        #[arg(long, default_value = "0", help = "Hot spare pool 0 to 7")]
        pool: u8,
    },
    #[command(about = "Delete a volume, or every volume and hot spare")]
    #[command(group(ArgGroup::new("target").required(true).args(["id", "all"])))]
    Delete {
        #[arg(value_name = "ID", help = "Volume id from `volume list`")]
        id: Option<u16>,
        #[arg(long, help = "Delete every volume, then every remaining hot spare")]
        all: bool,
        #[arg(long = "zero-lba0", help = "Zero block 0 of every member")]
        zero_lba0: bool,
    },
}

#[derive(Subcommand)]
pub enum HotspareAction {
    #[command(about = "Make an unconfigured drive a hot spare")]
    Add {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
        #[arg(long, default_value = "0", help = "Hot spare pool 0 to 7")]
        pool: u8,
    },
    #[command(about = "Remove a hot spare")]
    Remove {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
    },
}

#[derive(Subcommand)]
pub enum EnclosureAction {
    #[command(about = "List enclosures")]
    List,
}

#[derive(Subcommand)]
pub enum PhyAction {
    #[command(about = "List phys and link rates")]
    List,
    #[command(about = "Show phy error counters")]
    Errors,
    #[command(about = "Reset a phy")]
    Reset {
        #[arg(help = "Phy number from `phy list`")]
        phy: u8,
        #[arg(long, help = "Hard reset instead of link reset")]
        hard: bool,
    },
}

#[derive(Subcommand)]
pub enum TemperatureAction {
    #[command(about = "Show controller temperatures")]
    Show,
}

#[derive(Subcommand)]
pub enum EventAction {
    #[command(about = "List the events the driver has logged")]
    List,
    #[command(about = "Enable driver event logging for every event type")]
    Enable,
}

#[derive(Subcommand)]
pub enum BootAction {
    #[command(about = "Show requested and current boot devices")]
    Show,
    #[command(about = "Set the requested boot device")]
    #[command(group(ArgGroup::new("device").required(true).args(["drive", "volume"])))]
    Set {
        #[arg(long, help = "Set the alternate boot device instead of the primary")]
        alternate: bool,
        #[arg(long, value_name = "ENCLOSURE:SLOT")]
        drive: Option<DriveAddress>,
        #[arg(long, value_name = "ID")]
        volume: Option<u16>,
    },
}

#[derive(Subcommand)]
pub enum LogAction {
    #[command(about = "Read the persistent log, optionally saving the raw page")]
    Upload {
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },
    #[command(about = "Erase every persistent log entry")]
    Clear,
}

#[derive(Subcommand)]
pub enum FirmwareAction {
    #[command(about = "Show firmware, BIOS and NVDATA versions")]
    Show,
    #[command(about = "Read the flashed firmware or BIOS image into a file")]
    Upload {
        #[arg(help = "File to create")]
        output: PathBuf,
        #[arg(long, help = "Upload the BIOS image instead of the firmware")]
        bios: bool,
    },
    #[command(about = "Validate, flash and verify a firmware image")]
    Flash {
        #[arg(help = "Firmware image file")]
        file: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum BiosAction {
    #[command(about = "Validate and flash an option ROM file, replacing the whole BIOS region")]
    Flash {
        #[arg(help = "Option ROM file with x86, FCode or EFI images")]
        file: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum DiagAction {
    #[command(about = "Register and post a diagnostic buffer")]
    Register {
        #[arg(long = "type", value_enum)]
        buffer_type: BufferType,
        #[arg(long, help = "Buffer size in bytes, a multiple of 4")]
        size: u32,
        #[arg(long, value_parser = parse_u32, help = "Unique id, defaults to the driver's trace id")]
        unique_id: Option<u32>,
        #[arg(long, value_parser = parse_u32, default_value = "0")]
        diagnostic_flags: u32,
    },
    #[command(about = "Query a diagnostic buffer")]
    Query {
        #[arg(long = "type", value_enum)]
        buffer_type: BufferType,
    },
    #[command(about = "Read a diagnostic buffer into a file")]
    Read {
        #[arg(long = "type", value_enum)]
        buffer_type: BufferType,
        #[arg(help = "File to create")]
        output: PathBuf,
    },
    #[command(about = "Release a diagnostic buffer so the firmware stops writing")]
    Release {
        #[arg(long, value_parser = parse_u32)]
        unique_id: u32,
    },
    #[command(about = "Free a released diagnostic buffer")]
    Unregister {
        #[arg(long, value_parser = parse_u32)]
        unique_id: u32,
    },
}

fn parse_u32(s: &str) -> Result<u32, String> {
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|e| format!("invalid number {s:?}: {e}"))
}

#[derive(Serialize)]
pub struct Uploaded {
    pub file: String,
    pub bytes: usize,
    pub header: Option<ImageHeader>,
}

#[derive(Serialize)]
pub struct DiagRead {
    pub file: String,
    pub bytes: usize,
    pub query: DiagQuery,
}

fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(data)
        .with_context(|| format!("writing {}", path.display()))
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

fn done(message: impl Into<String>) -> Result<Box<dyn Emit>> {
    Ok(Box::new(Done::ok(message)))
}

fn boxed<T: Emit + 'static>(value: T) -> Result<Box<dyn Emit>> {
    Ok(Box::new(value))
}

pub fn execute(
    command: &Command,
    ctx: &Ctx,
    target: &Target,
    t: &dyn Transport,
) -> Result<Box<dyn Emit>> {
    let n = target.index;
    match command {
        Command::List => bail!("list does not take a controller"),
        Command::Controller { action } => match action {
            ControllerAction::Show => boxed(inventory::controller_info(target, t)?),
            ControllerAction::Reset => {
                ctx.confirm(&format!("resetting controller {n}"))?;
                diag::hard_reset(t)?;
                done(format!("controller {n} reset requested"))
            }
        },
        Command::Drive { action } => match action {
            DriveAction::List => boxed(inventory::drives(t)?),
            DriveAction::Show { address } => boxed(inventory::drive(t, *address)?),
            DriveAction::Locate { address, on, .. } => {
                let state = if *on { "on" } else { "off" };
                ctx.confirm(&format!("turning the locate LED of {address} {state}"))?;
                mpi::sep_locate_request(address.enclosure, address.slot, *on)
                    .send_checked(t, "SCSI enclosure processor request")?;
                done(format!("locate LED of {address} turned {state}"))
            }
            DriveAction::Online { address } => {
                ctx.confirm(&format!("bringing {address} online"))?;
                let pd = inventory::require_physdisk(t, *address)?;
                raid::physdisk_action_request(raid::ACTION_PHYSDISK_ONLINE, pd.phys_disk_num)
                    .send_checked(t, "PHYSDISK_ONLINE")?;
                done(format!("{address} brought online"))
            }
            DriveAction::Offline { address } => {
                ctx.confirm(&format!("taking {address} offline"))?;
                let pd = inventory::require_physdisk(t, *address)?;
                raid::physdisk_action_request(raid::ACTION_PHYSDISK_OFFLINE, pd.phys_disk_num)
                    .send_checked(t, "PHYSDISK_OFFLINE")?;
                done(format!("{address} taken offline"))
            }
        },
        Command::Volume { action } => match action {
            VolumeAction::List => boxed(inventory::volumes(t)?),
            VolumeAction::Show { id } => boxed(inventory::volume(t, *id)?),
            VolumeAction::Status => boxed(inventory::volume_statuses(t)?),
            VolumeAction::Activate { id } => {
                ctx.confirm(&format!("activating volume {id}"))?;
                raid::volume_action_request(raid::ACTION_ACTIVATE_VOLUME, *id)
                    .send_checked(t, "ACTIVATE_VOLUME")?;
                done(format!("volume {id} activated"))
            }
            VolumeAction::Check { id } => {
                ctx.confirm(&format!("starting a consistency check on volume {id}"))?;
                raid::consistency_check_request(*id).send_checked(t, "START_RAID_FUNCTION")?;
                done(format!("consistency check started on volume {id}"))
            }
            VolumeAction::Create {
                level,
                drives,
                size,
                name,
                stripe,
                pool,
            } => {
                let list: Vec<String> = drives.iter().map(ToString::to_string).collect();
                ctx.confirm(&format!(
                    "creating a {} volume from {}",
                    level.name(),
                    list.join(" ")
                ))?;
                boxed(ircfg::create_volume(
                    t,
                    &CreateSpec {
                        level: *level,
                        members: drives.clone(),
                        size_mb: *size,
                        name: name.clone(),
                        stripe_kb: *stripe,
                        pool: *pool,
                    },
                )?)
            }
            VolumeAction::Delete { id, all, zero_lba0 } => match (id, all) {
                (_, true) => {
                    ctx.confirm("deleting every volume and hot spare, destroying their data")?;
                    boxed(ircfg::delete_all(t, *zero_lba0)?)
                }
                (Some(id), false) => {
                    ctx.confirm(&format!("deleting volume {id}, destroying its data"))?;
                    boxed(ircfg::delete_volume(t, *id, *zero_lba0)?)
                }
                (None, false) => bail!("pass a volume id or --all"),
            },
        },
        Command::Hotspare { action } => match action {
            HotspareAction::Add { address, pool } => {
                ctx.confirm(&format!("adding hot spare {address} to pool {pool}"))?;
                ircfg::add_hot_spare(t, *address, *pool)?;
                done(format!("{address} added as a hot spare in pool {pool}"))
            }
            HotspareAction::Remove { address } => {
                ctx.confirm(&format!("removing hot spare {address}"))?;
                let pd = inventory::require_physdisk(t, *address)?;
                if pd.phys_disk_state != raid::PD_STATE_HOT_SPARE {
                    bail!("{address} is not a hot spare");
                }
                raid::physdisk_action_request(raid::ACTION_DELETE_HOT_SPARE, pd.phys_disk_num)
                    .send_checked(t, "DELETE_HOT_SPARE")?;
                done(format!("hot spare {address} removed"))
            }
        },
        Command::Enclosure { action } => match action {
            EnclosureAction::List => boxed(inventory::enclosure_list(t)?),
        },
        Command::Phy { action } => match action {
            PhyAction::List => boxed(inventory::phy_list(t)?),
            PhyAction::Errors => boxed(inventory::phy_errors(t)?),
            PhyAction::Reset { phy, hard } => {
                let kind = if *hard { "hard" } else { "link" };
                ctx.confirm(&format!("{kind} reset of phy {phy}"))?;
                mpi::phy_reset_request(*phy, *hard).send_checked(t, "SAS IO unit control")?;
                done(format!("phy {phy} {kind} reset"))
            }
        },
        Command::Temperature { action } => match action {
            TemperatureAction::Show => boxed(inventory::temperature(t)?),
        },
        Command::Event { action } => match action {
            EventAction::List => match diag::event_report(t)? {
                Some(events) => boxed(super::render::EventList { events }),
                None => bail!(
                    "event logging is not enabled on controller {n}, run `sasctl mpt -c {n} event enable --yes` first"
                ),
            },
            EventAction::Enable => {
                ctx.confirm("enabling driver event logging")?;
                diag::event_enable_all(t)?;
                done(format!("event logging enabled on controller {n}"))
            }
        },
        Command::Boot { action } => match action {
            BootAction::Show => boxed(inventory::boot_info(t)?),
            BootAction::Set {
                alternate,
                drive,
                volume,
            } => {
                let target = match (drive, volume) {
                    (Some(d), _) => BootTarget::Drive(*d),
                    (None, Some(v)) => BootTarget::Volume(*v),
                    (None, None) => bail!("pass --drive or --volume"),
                };
                let role = if *alternate { "alternate" } else { "primary" };
                let what = match target {
                    BootTarget::Drive(d) => format!("drive {d}"),
                    BootTarget::Volume(v) => format!("volume {v}"),
                };
                ctx.confirm(&format!("setting the {role} boot device to {what}"))?;
                inventory::set_boot(t, *alternate, target)?;
                done(format!("{role} boot device set to {what}"))
            }
        },
        Command::Log { action } => match action {
            LogAction::Upload { output } => {
                let info: LogInfo = inventory::log(t)?;
                if let Some(path) = output {
                    write_new(path, &info.raw)?;
                }
                boxed(info)
            }
            LogAction::Clear => {
                ctx.confirm("clearing the persistent log")?;
                let cleared = ircfg::clear_log(t)?;
                done(format!("{cleared} log entries cleared"))
            }
        },
        Command::Firmware { action } => match action {
            FirmwareAction::Show => boxed(inventory::firmware_info(t)?),
            FirmwareAction::Upload { output, bios } => {
                let image_type = if *bios {
                    fw::UPLOAD_TYPE_BIOS_FLASH
                } else {
                    fw::UPLOAD_TYPE_FW_FLASH
                };
                let data = fw::upload(t, image_type)?;
                write_new(output, &data)?;
                boxed(Uploaded {
                    file: output.display().to_string(),
                    bytes: data.len(),
                    header: if *bios {
                        None
                    } else {
                        fw::parse_image_header(&data).ok()
                    },
                })
            }
            FirmwareAction::Flash { file } => {
                ctx.confirm(&format!(
                    "flashing firmware {} on controller {n}",
                    file.display()
                ))?;
                let data = read_file(file)?;
                boxed(flash::flash_firmware(
                    t,
                    &file.display().to_string(),
                    &data,
                )?)
            }
        },
        Command::Bios { action } => match action {
            BiosAction::Flash { file } => {
                ctx.confirm(&format!(
                    "replacing the BIOS region of controller {n} with {}",
                    file.display()
                ))?;
                let data = read_file(file)?;
                boxed(flash::flash_bios(t, &file.display().to_string(), &data)?)
            }
        },
        Command::Diag { action } => match action {
            DiagAction::Register {
                buffer_type,
                size,
                unique_id,
                diagnostic_flags,
            } => {
                ctx.confirm("registering a diagnostic buffer")?;
                let id = unique_id.unwrap_or_else(|| diag::default_unique_id(t.generation()));
                diag::register(t, *buffer_type, *size, id, *diagnostic_flags)?;
                done(format!(
                    "diagnostic buffer registered with unique id 0x{id:08x}"
                ))
            }
            DiagAction::Query { buffer_type } => boxed(diag::query(t, *buffer_type)?),
            DiagAction::Read {
                buffer_type,
                output,
            } => {
                let query = diag::query(t, *buffer_type)?;
                let data = diag::read_all(t, query.unique_id, query.total_buffer_size)?;
                write_new(output, &data)?;
                boxed(DiagRead {
                    file: output.display().to_string(),
                    bytes: data.len(),
                    query,
                })
            }
            DiagAction::Release { unique_id } => {
                ctx.confirm("releasing a diagnostic buffer")?;
                diag::release(t, *unique_id)?;
                done(format!("diagnostic buffer 0x{unique_id:08x} released"))
            }
            DiagAction::Unregister { unique_id } => {
                ctx.confirm("unregistering a diagnostic buffer")?;
                diag::unregister(t, *unique_id)?;
                done(format!("diagnostic buffer 0x{unique_id:08x} unregistered"))
            }
        },
    }
}
