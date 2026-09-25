use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::bytes::{Le, LeMut};
use crate::ioctl::{Device, iowr};

pub const MAGIC: u8 = b'L';
pub const NR_IOCINFO: u8 = 17;
pub const NR_COMMAND: u8 = 20;
pub const NR_EVENTQUERY: u8 = 21;
pub const NR_EVENTENABLE: u8 = 22;
pub const NR_EVENTREPORT: u8 = 23;
pub const NR_HARDRESET: u8 = 24;
pub const NR_DIAGREGISTER: u8 = 26;
pub const NR_DIAGRELEASE: u8 = 27;
pub const NR_DIAGUNREGISTER: u8 = 28;
pub const NR_DIAGQUERY: u8 = 29;
pub const NR_DIAGREADBUFFER: u8 = 30;
pub const NR_BTDHMAPPING: u8 = 31;

pub const HEADER_LEN: usize = 12;
pub const EVENTREPORT_ENCODED_LEN: usize = 212;

#[cfg(target_pointer_width = "64")]
const PTR: usize = 8;
#[cfg(target_pointer_width = "32")]
const PTR: usize = 4;

const CMD_TIMEOUT: usize = 0x0C;
const CMD_REPLY_PTR: usize = 0x10;
const CMD_DATA_IN_PTR: usize = CMD_REPLY_PTR + PTR;
const CMD_DATA_OUT_PTR: usize = CMD_REPLY_PTR + 2 * PTR;
const CMD_SENSE_PTR: usize = CMD_REPLY_PTR + 3 * PTR;
const CMD_MAX_REPLY: usize = CMD_REPLY_PTR + 4 * PTR;
const CMD_DATA_IN_SIZE: usize = CMD_MAX_REPLY + 4;
const CMD_DATA_OUT_SIZE: usize = CMD_MAX_REPLY + 8;
const CMD_MAX_SENSE: usize = CMD_MAX_REPLY + 12;
const CMD_SGE_OFFSET: usize = CMD_MAX_REPLY + 16;
const CMD_MF: usize = CMD_MAX_REPLY + 20;
const CMD_STRUCT_LEN: usize = (CMD_MF + 1).next_multiple_of(PTR);

pub const MAX_REPLY_BYTES: usize = 128;
pub const MAX_SENSE_BYTES: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    Sas2,
    Sas3,
}

impl Generation {
    pub fn node(self) -> &'static str {
        match self {
            Generation::Sas2 => "/dev/mpt2ctl",
            Generation::Sas3 => "/dev/mpt3ctl",
        }
    }

    pub fn proc_name(self) -> &'static str {
        match self {
            Generation::Sas2 => "mpt2sas",
            Generation::Sas3 => "mpt3sas",
        }
    }
}

pub struct Command<'a> {
    pub frame: &'a [u8],
    pub sge_offset: u32,
    pub data_out: &'a [u8],
    pub data_in_len: usize,
    pub timeout: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub reply: Vec<u8>,
    pub data_in: Vec<u8>,
    pub sense: Vec<u8>,
}

impl Reply {
    pub fn ioc_status(&self) -> u16 {
        self.reply.u16_at(0x0E) & 0x7FFF
    }

    pub fn ioc_log_info(&self) -> u32 {
        self.reply.u32_at(0x10)
    }
}

pub trait Transport {
    fn ioc_number(&self) -> u32;
    fn generation(&self) -> Generation;
    fn raw(&self, nr: u8, encoded_len: usize, buf: &mut [u8]) -> Result<()>;
    fn command(&self, cmd: &Command) -> Result<Reply>;
}

pub struct LinuxTransport {
    dev: Device,
    ioc: u32,
    generation: Generation,
}

impl LinuxTransport {
    pub fn open(node: &Path, generation: Generation, ioc: u32) -> Result<Self> {
        let dev = Device::open(node).with_context(|| format!("opening {}", node.display()))?;
        Ok(Self {
            dev,
            ioc,
            generation,
        })
    }
}

impl Transport for LinuxTransport {
    fn ioc_number(&self) -> u32 {
        self.ioc
    }

    fn generation(&self) -> Generation {
        self.generation
    }

    fn raw(&self, nr: u8, encoded_len: usize, buf: &mut [u8]) -> Result<()> {
        if buf.len() < HEADER_LEN {
            bail!("ioctl buffer shorter than the header");
        }
        buf.put_u32(0, self.ioc);
        let request = iowr(MAGIC, nr, encoded_len);
        unsafe { self.dev.ioctl(request, buf.as_mut_ptr()) }
            .with_context(|| format!("mpt ioctl {nr} on ioc {}", self.ioc))
    }

    fn command(&self, cmd: &Command) -> Result<Reply> {
        let frame_len = (cmd.sge_offset as usize * 4).max(cmd.frame.len());
        let mut buf = vec![0u8; CMD_MF + frame_len + 32];
        let mut reply = vec![0u8; MAX_REPLY_BYTES];
        let mut sense = vec![0u8; MAX_SENSE_BYTES];
        let mut data_in = vec![0u8; cmd.data_in_len];
        let data_out = cmd.data_out.to_vec();
        buf.put_u32(0, self.ioc);
        buf.put_u32(CMD_TIMEOUT, cmd.timeout);
        put_ptr(&mut buf, CMD_REPLY_PTR, reply.as_mut_ptr() as usize);
        put_ptr(
            &mut buf,
            CMD_DATA_IN_PTR,
            if data_in.is_empty() {
                0
            } else {
                data_in.as_mut_ptr() as usize
            },
        );
        put_ptr(
            &mut buf,
            CMD_DATA_OUT_PTR,
            if data_out.is_empty() {
                0
            } else {
                data_out.as_ptr() as usize
            },
        );
        put_ptr(&mut buf, CMD_SENSE_PTR, sense.as_mut_ptr() as usize);
        buf.put_u32(CMD_MAX_REPLY, MAX_REPLY_BYTES as u32);
        buf.put_u32(CMD_DATA_IN_SIZE, data_in.len() as u32);
        buf.put_u32(CMD_DATA_OUT_SIZE, data_out.len() as u32);
        buf.put_u32(CMD_MAX_SENSE, MAX_SENSE_BYTES as u32);
        buf.put_u32(CMD_SGE_OFFSET, cmd.sge_offset);
        buf.put_bytes(CMD_MF, cmd.frame);
        let request = iowr(MAGIC, NR_COMMAND, CMD_STRUCT_LEN);
        unsafe { self.dev.ioctl(request, buf.as_mut_ptr()) }.with_context(|| {
            format!(
                "mpt command function 0x{:02x} on ioc {}",
                cmd.frame.u8_at(3),
                self.ioc
            )
        })?;
        Ok(Reply {
            reply,
            data_in,
            sense,
        })
    }
}

fn put_ptr(buf: &mut [u8], off: usize, v: usize) {
    if PTR == 8 {
        buf.put_u64(off, v as u64);
    } else {
        buf.put_u32(off, v as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_layout_matches_the_kernel() {
        if PTR == 8 {
            assert_eq!(CMD_MF, 0x44);
            assert_eq!(CMD_STRUCT_LEN, 72);
        } else {
            assert_eq!(CMD_MF, 0x34);
            assert_eq!(CMD_STRUCT_LEN, 56);
        }
    }
}
