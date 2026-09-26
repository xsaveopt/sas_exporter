use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
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
use crate::cli::{
    BootAction, BufferKind, Command, ConfigAction, ControllerAction, DiagAction, DriveAction,
    DriveId, EventAction, EventFilter, FirmwareAction, LogAction, PhyAction, Switch, VolumeAction,
    VolumeId,
};
use crate::mega::config::parse_stripe;
use crate::output::Done;

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

fn buffer(kind: BufferKind) -> BufferType {
    match kind {
        BufferKind::Trace => BufferType::Trace,
        BufferKind::Snapshot => BufferType::Snapshot,
        BufferKind::Extended => BufferType::Extended,
    }
}

fn level(s: &str) -> Result<Level> {
    let l = s.to_ascii_lowercase();
    match l.strip_prefix("raid").unwrap_or(&l) {
        "0" => Ok(Level::Raid0),
        "1" => Ok(Level::Raid1),
        "1e" => Ok(Level::Raid1e),
        "10" => Ok(Level::Raid10),
        _ => bail!("Integrated RAID supports RAID 0, 1, 1E and 10, not {s}"),
    }
}

fn drive_address(id: &Option<DriveId>) -> Result<DriveAddress> {
    id.as_ref()
        .context("pass a drive as enclosure:slot")?
        .parse()
}

fn volume_id(id: &Option<VolumeId>) -> Result<u16> {
    Ok(id.context("pass a volume ID")?.id)
}

pub fn supports(command: &Command) -> bool {
    match command {
        Command::Controller { action, .. } => matches!(
            action,
            None | Some(ControllerAction::Reset { snapdump: false })
        ),
        Command::Drive { action, .. } => match action {
            None
            | Some(DriveAction::Locate { .. })
            | Some(DriveAction::Online)
            | Some(DriveAction::Offline)
            | Some(DriveAction::Unspare) => true,
            Some(DriveAction::Spare {
                volume,
                revertible,
                affinity,
                ..
            }) => volume.is_none() && !revertible && !affinity,
            _ => false,
        },
        Command::Volume { action, .. } => match action {
            None
            | Some(VolumeAction::Delete { .. })
            | Some(VolumeAction::Activate)
            | Some(VolumeAction::Check)
            | Some(VolumeAction::Progress) => true,
            Some(VolumeAction::Create { span, init, .. }) => span.is_none() && init.is_none(),
            _ => false,
        },
        Command::Config { action } => matches!(action, Some(ConfigAction::Clear { .. })),
        Command::Enclosure
        | Command::Phy { .. }
        | Command::Temperature
        | Command::Boot { .. }
        | Command::Log { .. }
        | Command::Firmware { .. }
        | Command::Diag { .. } => true,
        Command::Event { filter, action } => {
            filter.since.is_none()
                && filter.class.is_none()
                && filter.locale.is_none()
                && matches!(action, None | Some(EventAction::Enable))
        }
        _ => false,
    }
}

pub fn execute(
    command: &Command,
    ctx: &Ctx,
    target: &Target,
    t: &dyn Transport,
) -> Result<Box<dyn Emit>> {
    let n = target.index;
    match command {
        Command::Controller { action, .. } => match action {
            None => boxed(inventory::controller_info(target, t)?),
            Some(ControllerAction::Reset { .. }) => {
                ctx.confirm(&format!("a hard reset of controller {n}"))?;
                diag::hard_reset(t)?;
                done(format!("controller {n} reset requested"))
            }
            Some(_) => unsupported(),
        },
        Command::Drive { id, action } => match action {
            None => match id {
                None => boxed(inventory::drives(t)?),
                Some(_) => boxed(inventory::drive(t, drive_address(id)?)?),
            },
            Some(DriveAction::Locate { state }) => {
                let address = drive_address(id)?;
                let on = *state == Switch::On;
                let word = if on { "on" } else { "off" };
                ctx.confirm(&format!("turning the locate LED of {address} {word}"))?;
                mpi::sep_locate_request(address.enclosure, address.slot, on)
                    .send_checked(t, "SCSI enclosure processor request")?;
                done(format!("locate LED of {address} turned {word}"))
            }
            Some(DriveAction::Online) => {
                let address = drive_address(id)?;
                ctx.confirm(&format!("bringing {address} online"))?;
                let pd = inventory::require_physdisk(t, address)?;
                raid::physdisk_action_request(raid::ACTION_PHYSDISK_ONLINE, pd.phys_disk_num)
                    .send_checked(t, "PHYSDISK_ONLINE")?;
                done(format!("{address} brought online"))
            }
            Some(DriveAction::Offline) => {
                let address = drive_address(id)?;
                ctx.confirm(&format!("taking {address} offline"))?;
                let pd = inventory::require_physdisk(t, address)?;
                raid::physdisk_action_request(raid::ACTION_PHYSDISK_OFFLINE, pd.phys_disk_num)
                    .send_checked(t, "PHYSDISK_OFFLINE")?;
                done(format!("{address} taken offline"))
            }
            Some(DriveAction::Spare { pool, .. }) => {
                let address = drive_address(id)?;
                let pool = pool.unwrap_or(0);
                ctx.confirm(&format!("adding hot spare {address} to pool {pool}"))?;
                ircfg::add_hot_spare(t, address, pool)?;
                done(format!("{address} added as a hot spare in pool {pool}"))
            }
            Some(DriveAction::Unspare) => {
                let address = drive_address(id)?;
                ctx.confirm(&format!("removing hot spare {address}"))?;
                let pd = inventory::require_physdisk(t, address)?;
                if pd.phys_disk_state != raid::PD_STATE_HOT_SPARE {
                    bail!("{address} is not a hot spare");
                }
                raid::physdisk_action_request(raid::ACTION_DELETE_HOT_SPARE, pd.phys_disk_num)
                    .send_checked(t, "DELETE_HOT_SPARE")?;
                done(format!("hot spare {address} removed"))
            }
            Some(_) => unsupported(),
        },
        Command::Volume { id, action } => match action {
            None => match id {
                None => boxed(inventory::volumes(t)?),
                Some(v) => boxed(inventory::volume(t, v.id)?),
            },
            Some(VolumeAction::Progress) => {
                let mut list = inventory::volume_statuses(t)?;
                if let Some(v) = id {
                    list.volumes.retain(|s| s.id == v.id);
                    if list.volumes.is_empty() {
                        bail!("volume {} does not exist", v.id);
                    }
                }
                boxed(list)
            }
            Some(VolumeAction::Activate) => {
                let id = volume_id(id)?;
                ctx.confirm(&format!("activating volume {id}"))?;
                raid::volume_action_request(raid::ACTION_ACTIVATE_VOLUME, id)
                    .send_checked(t, "ACTIVATE_VOLUME")?;
                done(format!("volume {id} activated"))
            }
            Some(VolumeAction::Check) => {
                let id = volume_id(id)?;
                ctx.confirm(&format!("starting a consistency check on volume {id}"))?;
                raid::consistency_check_request(id).send_checked(t, "START_RAID_FUNCTION")?;
                done(format!("consistency check started on volume {id}"))
            }
            Some(VolumeAction::Create {
                level: raid,
                drives,
                size,
                name,
                stripe,
                pool,
                ..
            }) => {
                let level = level(raid)?;
                let members = drives
                    .iter()
                    .map(|d| d.parse())
                    .collect::<Result<Vec<DriveAddress>>>()?;
                let stripe_kb = stripe
                    .as_deref()
                    .map(|s| parse_stripe(s).map(|b| b / 1024))
                    .transpose()?;
                let list: Vec<String> = members.iter().map(ToString::to_string).collect();
                ctx.confirm(&format!(
                    "creating a {} volume from {}",
                    level.name(),
                    list.join(" ")
                ))?;
                boxed(ircfg::create_volume(
                    t,
                    &CreateSpec {
                        level,
                        members,
                        size_mb: *size,
                        name: name.clone(),
                        stripe_kb,
                        pool: pool.unwrap_or(0),
                    },
                )?)
            }
            Some(VolumeAction::Delete { zero_lba0 }) => {
                let id = volume_id(id)?;
                ctx.confirm(&format!("deleting volume {id}, destroying its data"))?;
                boxed(ircfg::delete_volume(t, id, *zero_lba0)?)
            }
            Some(_) => unsupported(),
        },
        Command::Config {
            action: Some(ConfigAction::Clear { zero_lba0 }),
        } => {
            ctx.confirm("deleting every volume and hot spare, destroying their data")?;
            boxed(ircfg::delete_all(t, *zero_lba0)?)
        }
        Command::Enclosure => boxed(inventory::enclosure_list(t)?),
        Command::Phy { id, action } => match action {
            None => boxed(inventory::phy_list(t)?),
            Some(PhyAction::Errors) => boxed(inventory::phy_errors(t)?),
            Some(PhyAction::Reset { hard }) => {
                let phy = id.context("pass a phy number")?.phy;
                let kind = if *hard { "hard" } else { "link" };
                ctx.confirm(&format!("a {kind} reset of phy {phy}"))?;
                mpi::phy_reset_request(phy, *hard).send_checked(t, "SAS IO unit control")?;
                done(format!("phy {phy} {kind} reset"))
            }
        },
        Command::Temperature => boxed(inventory::temperature(t)?),
        Command::Event {
            filter: EventFilter { count, .. },
            action,
        } => match action {
            None => match diag::event_report(t)? {
                Some(mut events) => {
                    if let Some(count) = count {
                        let skip = events.len().saturating_sub(*count);
                        events.drain(..skip);
                    }
                    boxed(super::render::EventList { events })
                }
                None => bail!(
                    "event logging is off on controller {n}, turn it on with sasctl -c {n} event enable"
                ),
            },
            Some(EventAction::Enable) => {
                ctx.confirm("enabling driver event logging")?;
                diag::event_enable_all(t)?;
                done(format!("event logging enabled on controller {n}"))
            }
            Some(_) => unsupported(),
        },
        Command::Boot { action } => match action {
            None => boxed(inventory::boot_info(t)?),
            Some(BootAction::Set {
                drive,
                volume,
                alternate,
            }) => {
                let target = match (drive, volume) {
                    (Some(d), _) => BootTarget::Drive(d.parse()?),
                    (None, Some(v)) => BootTarget::Volume(v.id),
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
            None => boxed(inventory::log(t)?),
            Some(LogAction::Save { file }) => {
                let info: LogInfo = inventory::log(t)?;
                write_new(file, &info.raw)?;
                boxed(info)
            }
            Some(LogAction::Clear) => {
                ctx.confirm("clearing the persistent log")?;
                let cleared = ircfg::clear_log(t)?;
                done(format!("{cleared} log entries cleared"))
            }
        },
        Command::Firmware { action } => match action {
            None => boxed(inventory::firmware_info(t)?),
            Some(FirmwareAction::Save { file, bios }) => {
                let image_type = if *bios {
                    fw::UPLOAD_TYPE_BIOS_FLASH
                } else {
                    fw::UPLOAD_TYPE_FW_FLASH
                };
                let data = fw::upload(t, image_type)?;
                write_new(file, &data)?;
                boxed(Uploaded {
                    file: file.display().to_string(),
                    bytes: data.len(),
                    header: if *bios {
                        None
                    } else {
                        fw::parse_image_header(&data).ok()
                    },
                })
            }
            Some(FirmwareAction::Flash { file, bios: false }) => {
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
            Some(FirmwareAction::Flash { file, bios: true }) => {
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
                kind,
                size,
                unique_id,
                diagnostic_flags,
            } => {
                ctx.confirm("registering a diagnostic buffer")?;
                let id = unique_id.unwrap_or_else(|| diag::default_unique_id(t.generation()));
                diag::register(t, buffer(*kind), *size, id, *diagnostic_flags)?;
                done(format!(
                    "diagnostic buffer registered with unique id 0x{id:08x}"
                ))
            }
            DiagAction::Query { kind } => boxed(diag::query(t, buffer(*kind))?),
            DiagAction::Read { kind, file } => {
                let query = diag::query(t, buffer(*kind))?;
                let data = diag::read_all(t, query.unique_id, query.total_buffer_size)?;
                write_new(file, &data)?;
                boxed(DiagRead {
                    file: file.display().to_string(),
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
        _ => unsupported(),
    }
}

fn unsupported() -> Result<Box<dyn Emit>> {
    Err(anyhow!(
        "this is not available on mpt2sas and mpt3sas controllers"
    ))
}
