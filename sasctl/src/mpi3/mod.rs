pub mod adapter;
pub mod cli;
pub mod config;
pub mod event;
pub mod inventory;
pub mod mpi;
pub mod nvme;
pub mod pages;
pub mod render;
pub mod scsi;
pub mod transport;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};

use crate::Ctx;

pub use cli::Args;
use cli::Command;
use render::Emit;

pub fn run(args: Args, ctx: &Ctx) -> Result<()> {
    let targets = adapter::enumerate(&ctx.sysfs);
    let output: Box<dyn Emit> = match args.command {
        Command::List => Box::new(inventory::list_adapters(&targets, adapter::open)?),
        command => {
            let index = args
                .controller
                .context("pass -c/--controller <id>, see `sasctl mpi3 list`")?;
            let target = adapter::select(targets, index)?;
            let t = adapter::open(&target)?;
            cli::execute(&command, ctx, &target, t.as_ref())?
        }
    };
    output.emit_to(ctx.format)
}
