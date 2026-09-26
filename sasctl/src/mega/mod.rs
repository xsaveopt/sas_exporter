pub mod ata;
pub mod bbu;
pub mod cli;
pub mod config;
pub mod ctrl;
pub mod dcmd;
pub mod event;
pub mod fw;
pub mod ld;
pub mod mfi;
pub mod patrol;
pub mod pd;
pub mod render;
pub mod report;
pub mod reset;
pub mod scsi;
pub mod transport;

#[cfg(test)]
pub mod mock;

use std::path::Path;

use anyhow::{Context, Result};

use crate::mega::transport::{DEFAULT_NODE, LinuxTransport, Transport, ensure_node};

pub fn open_host(host_no: u32) -> Result<Box<dyn Transport>> {
    let host =
        u16::try_from(host_no).with_context(|| format!("SCSI host {host_no} is out of range"))?;
    let node = ensure_node(Path::new(DEFAULT_NODE))?;
    Ok(Box::new(LinuxTransport::open(&node, host)?))
}
