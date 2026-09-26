use anyhow::{Context, Result, anyhow};

use super::adapter::{self, OsView, RESET_DIAG_FAULT, RESET_SOFT, Target};
use super::inventory::{self, DriveAddress};
use super::mpi;
use super::render::Emit;
use super::transport::Transport;
use crate::Ctx;
use crate::cli::{Command, ControllerAction, DriveAction, DriveId, EventFilter, PhyAction};
use crate::output::Done;

fn done(message: impl Into<String>) -> Result<Box<dyn Emit>> {
    Ok(Box::new(Done::ok(message)))
}

fn boxed<T: Emit + 'static>(value: T) -> Result<Box<dyn Emit>> {
    Ok(Box::new(value))
}

fn drive_address(id: &Option<DriveId>) -> Result<DriveAddress> {
    id.as_ref()
        .context("pass a drive as enclosure:slot")?
        .parse()
}

pub fn supports(command: &Command) -> bool {
    match command {
        Command::Controller { action, .. } => {
            matches!(action, None | Some(ControllerAction::Reset { .. }))
        }
        Command::Drive { action, .. } => matches!(action, None | Some(DriveAction::Smart)),
        Command::Volume { action, .. } => action.is_none(),
        Command::Enclosure | Command::Phy { .. } | Command::Temperature => true,
        Command::Event { filter, action } => {
            action.is_none()
                && filter.since.is_none()
                && filter.class.is_none()
                && filter.locale.is_none()
        }
        Command::Firmware { action } => action.is_none(),
        _ => false,
    }
}

pub fn execute(
    command: &Command,
    ctx: &Ctx,
    target: &Target,
    t: &dyn Transport,
) -> Result<Box<dyn Emit>> {
    let n = target.id;
    let os = OsView {
        sysfs: &ctx.sysfs,
        host: target.host.host_no,
    };
    match command {
        Command::Controller { action, .. } => match action {
            None => boxed(inventory::controller_info(target, t)?),
            Some(ControllerAction::Reset { snapdump }) => {
                let (kind, reset_type) = if *snapdump {
                    ("diagnostic fault reset", RESET_DIAG_FAULT)
                } else {
                    ("soft reset", RESET_SOFT)
                };
                ctx.confirm(&format!("a {kind} of controller {n}"))?;
                adapter::reset(t, reset_type)?;
                done(format!("controller {n} {kind} completed"))
            }
            Some(_) => unsupported(),
        },
        Command::Drive { id, action } => match action {
            None => match id {
                None => boxed(inventory::drives(t, &os)?),
                Some(_) => boxed(inventory::drive(t, &os, drive_address(id)?)?),
            },
            Some(DriveAction::Smart) => boxed(inventory::drive_smart(t, drive_address(id)?)?),
            Some(_) => unsupported(),
        },
        Command::Volume { id, action: None } => match id {
            None => boxed(inventory::volumes(t, &os)?),
            Some(v) => boxed(inventory::volume(t, &os, v.id)?),
        },
        Command::Enclosure => boxed(inventory::enclosure_list(t)?),
        Command::Phy { id, action } => match action {
            None => boxed(inventory::phy_list(t)?),
            Some(PhyAction::Errors) => boxed(inventory::phy_errors(t)?),
            Some(PhyAction::Reset { hard }) => {
                let phy = id.context("pass a phy number")?.phy;
                let kind = if *hard { "hard" } else { "link" };
                ctx.confirm(&format!("a {kind} reset of phy {phy}"))?;
                inventory::phy_exists(t, phy)?;
                mpi::phy_reset_request(phy, *hard).send_checked(t, "SAS phy control")?;
                done(format!("phy {phy} {kind} reset"))
            }
        },
        Command::Temperature => boxed(inventory::temperature(t)?),
        Command::Event {
            filter: EventFilter { count, .. },
            action: None,
        } => {
            let latest = count.map(|c| u32::try_from(c).unwrap_or(u32::MAX));
            boxed(inventory::events(t, latest)?)
        }
        Command::Firmware { action: None } => boxed(inventory::firmware_info(t)?),
        _ => unsupported(),
    }
}

fn unsupported() -> Result<Box<dyn Emit>> {
    Err(anyhow!("this is not available on mpi3mr controllers"))
}
