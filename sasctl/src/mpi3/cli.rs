use anyhow::{Result, bail};
use clap::{Args as ClapArgs, Subcommand};

use super::adapter::{self, OsView, RESET_DIAG_FAULT, RESET_SOFT, Target};
use super::inventory::{self, DriveAddress};
use super::mpi;
use super::render::Emit;
use super::transport::Transport;
use crate::Ctx;
use crate::output::Done;

#[derive(ClapArgs)]
pub struct Args {
    #[arg(
        short = 'c',
        long = "controller",
        value_name = "ID",
        help = "Controller id from `sasctl mpi3 list`"
    )]
    pub controller: Option<u8>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "List mpi3mr controllers")]
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
    #[command(about = "Virtual disks, read only")]
    Volume {
        #[command(subcommand)]
        action: VolumeAction,
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
    #[command(about = "Controller and drive temperature readings")]
    Temperature {
        #[command(subcommand)]
        action: TemperatureAction,
    },
    #[command(about = "Controller persistent event log")]
    Event {
        #[command(subcommand)]
        action: EventAction,
    },
    #[command(about = "Controller firmware")]
    Firmware {
        #[command(subcommand)]
        action: FirmwareAction,
    },
}

#[derive(Subcommand)]
pub enum ControllerAction {
    #[command(about = "Show controller details")]
    Show,
    #[command(about = "Soft reset the controller through the driver")]
    Reset {
        #[arg(long, help = "Have the firmware save a snapdump before the reset")]
        snapdump: bool,
    },
}

#[derive(Subcommand)]
pub enum DriveAction {
    #[command(about = "List drives")]
    List,
    #[command(about = "Show one drive")]
    Show {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
    },
    #[command(about = "Show SMART health of a SAS or NVMe drive")]
    Smart {
        #[arg(value_name = "ENCLOSURE:SLOT")]
        address: DriveAddress,
    },
}

#[derive(Subcommand)]
pub enum VolumeAction {
    #[command(about = "List virtual disks")]
    List,
    #[command(about = "Show one virtual disk")]
    Show {
        #[arg(value_name = "ID", help = "Virtual disk id from `volume list`")]
        id: u16,
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
    #[command(about = "Reset a controller phy")]
    Reset {
        #[arg(help = "Phy number from `phy list`")]
        phy: u8,
        #[arg(long, help = "Hard reset instead of link reset")]
        hard: bool,
    },
}

#[derive(Subcommand)]
pub enum TemperatureAction {
    #[command(about = "Show raw temperature readings")]
    Show,
}

#[derive(Subcommand)]
pub enum EventAction {
    #[command(about = "List persistent event log entries")]
    List {
        #[arg(long, value_name = "N", help = "Only the newest N entries")]
        latest: Option<u32>,
    },
}

#[derive(Subcommand)]
pub enum FirmwareAction {
    #[command(about = "Show firmware, package and NVDATA versions")]
    Show,
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
    let os = OsView {
        sysfs: &ctx.sysfs,
        host: target.host.host_no,
    };
    match command {
        Command::List => bail!("list does not take a controller"),
        Command::Controller { action } => match action {
            ControllerAction::Show => boxed(inventory::controller_info(target, t)?),
            ControllerAction::Reset { snapdump } => {
                let (kind, reset_type) = if *snapdump {
                    ("diagnostic fault reset", RESET_DIAG_FAULT)
                } else {
                    ("soft reset", RESET_SOFT)
                };
                ctx.confirm(&format!("a {kind} of controller {n}"))?;
                adapter::reset(t, reset_type)?;
                done(format!("controller {n} {kind} completed"))
            }
        },
        Command::Drive { action } => match action {
            DriveAction::List => boxed(inventory::drives(t, &os)?),
            DriveAction::Show { address } => boxed(inventory::drive(t, &os, *address)?),
            DriveAction::Smart { address } => boxed(inventory::drive_smart(t, *address)?),
        },
        Command::Volume { action } => match action {
            VolumeAction::List => boxed(inventory::volumes(t, &os)?),
            VolumeAction::Show { id } => boxed(inventory::volume(t, &os, *id)?),
        },
        Command::Enclosure { action } => match action {
            EnclosureAction::List => boxed(inventory::enclosure_list(t)?),
        },
        Command::Phy { action } => match action {
            PhyAction::List => boxed(inventory::phy_list(t)?),
            PhyAction::Errors => boxed(inventory::phy_errors(t)?),
            PhyAction::Reset { phy, hard } => {
                let kind = if *hard { "hard" } else { "link" };
                ctx.confirm(&format!("a {kind} reset of phy {phy}"))?;
                inventory::phy_exists(t, *phy)?;
                mpi::phy_reset_request(*phy, *hard).send_checked(t, "SAS phy control")?;
                done(format!("phy {phy} {kind} reset"))
            }
        },
        Command::Temperature { action } => match action {
            TemperatureAction::Show => boxed(inventory::temperature(t)?),
        },
        Command::Event { action } => match action {
            EventAction::List { latest } => boxed(inventory::events(t, *latest)?),
        },
        Command::Firmware { action } => match action {
            FirmwareAction::Show => boxed(inventory::firmware_info(t)?),
        },
    }
}
