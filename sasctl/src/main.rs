mod bytes;
mod ioctl;
mod mega;
mod mpi3;
mod mpt;
mod output;
mod sysfs;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::output::Format;

#[derive(Parser)]
#[command(
    name = "sasctl",
    version,
    about = "Manage LSI and Broadcom SAS HBAs and MegaRAID controllers"
)]
struct Cli {
    #[arg(long, global = true, help = "Print JSON instead of text")]
    json: bool,
    #[arg(
        long,
        short = 'y',
        global = true,
        help = "Confirm commands that change the controller"
    )]
    yes: bool,
    #[arg(long, global = true, default_value = "/sys", hide = true)]
    sysfs: PathBuf,
    #[command(subcommand)]
    family: Family,
}

#[derive(Subcommand)]
enum Family {
    #[command(about = "Fusion-MPT SAS2 and SAS3 HBAs (mpt2sas, mpt3sas)")]
    Mpt(mpt::Args),
    #[command(about = "MegaRAID controllers (megaraid_sas)")]
    Mega(mega::Args),
    #[command(about = "Broadcom 9600-series and Dell PERC12 controllers (mpi3mr)")]
    Mpi3(mpi3::Args),
}

pub struct Ctx {
    pub format: Format,
    pub yes: bool,
    pub sysfs: PathBuf,
}

impl Ctx {
    pub fn confirm(&self, what: &str) -> Result<()> {
        if !self.yes {
            bail!("{what} changes the controller, rerun with --yes to confirm");
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ctx = Ctx {
        format: if cli.json { Format::Json } else { Format::Text },
        yes: cli.yes,
        sysfs: cli.sysfs,
    };
    let result = match cli.family {
        Family::Mpt(args) => mpt::run(args, &ctx),
        Family::Mega(args) => mega::run(args, &ctx),
        Family::Mpi3(args) => mpi3::run(args, &ctx),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
