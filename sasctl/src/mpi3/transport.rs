use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::bytes::{Le, LeMut};
use crate::ioctl::Device;

pub const SG_IO: u64 = 0x2285;
pub const SG_IO_V4_LEN: usize = 0xA0;
pub const SG_GUARD: u32 = b'Q' as u32;
pub const BSG_PROTOCOL_SCSI: u32 = 0;
pub const BSG_SUB_PROTOCOL_SCSI_TRANSPORT: u32 = 2;
pub const BSG_RESPONSE_LEN: usize = 96;
pub const BLOCK_TIMEOUT_MS: u32 = 600_000;

pub const PACKET_LEN: usize = 0x20;
pub const CMD_DRIVER: u8 = 1;
pub const CMD_MPT: u8 = 2;
const PACKET_CMD: usize = 0x08;
const MPT_TIMEOUT: usize = PACKET_CMD + 0x02;
const MPT_NUM_ENTRIES: usize = PACKET_CMD + 0x08;
pub const MPT_ENTRIES: usize = PACKET_CMD + 0x10;
pub const ENTRY_LEN: usize = 8;

pub const BUF_DATA_IN: u8 = 3;
pub const BUF_DATA_OUT: u8 = 4;
pub const BUF_MPI_REPLY: u8 = 5;
pub const BUF_ERR_RESPONSE: u8 = 6;
pub const BUF_MPI_REQUEST: u8 = 0xFE;

pub const MAX_REQUEST_FRAME: usize = 128;
pub const REPLY_HEADER_LEN: usize = 4;
pub const MPI_REPLY_LEN: usize = REPLY_HEADER_LEN + 128;
pub const SENSE_LEN: usize = 256;
pub const REPLY_TYPE_STATUS: u8 = 1;
pub const REPLY_TYPE_ADDRESS: u8 = 2;

pub const TIMEOUT_DEFAULT: u16 = 60;
pub const TIMEOUT_DEVICE: u16 = 120;

pub const DMA_ALIGN: usize = 512;

pub fn node_path(mrioc_id: u8) -> PathBuf {
    PathBuf::from(format!("/dev/bsg/mpi3mrctl{mrioc_id}"))
}

pub trait Transport {
    fn mrioc_id(&self) -> u8;
    fn submit(&self, packet: &[u8], dout: &[u8], din: &mut [u8]) -> Result<()>;
}

pub fn driver_packet(mrioc_id: u8, opcode: u8) -> Vec<u8> {
    let mut p = vec![0u8; PACKET_LEN];
    p.put_u8(0x00, CMD_DRIVER);
    p.put_u8(PACKET_CMD, mrioc_id);
    p.put_u8(PACKET_CMD + 1, opcode);
    p
}

pub fn driver_read(t: &dyn Transport, opcode: u8, len: usize) -> Result<Vec<u8>> {
    let packet = driver_packet(t.mrioc_id(), opcode);
    let mut din = vec![0u8; len];
    t.submit(&packet, &[], &mut din)
        .with_context(|| format!("driver command {opcode} on controller {}", t.mrioc_id()))?;
    Ok(din)
}

pub fn driver_write(t: &dyn Transport, opcode: u8, data: &[u8]) -> Result<()> {
    let packet = driver_packet(t.mrioc_id(), opcode);
    t.submit(&packet, data, &mut [])
        .with_context(|| format!("driver command {opcode} on controller {}", t.mrioc_id()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub buf_type: u8,
    pub len: u32,
}

pub fn mpt_packet(mrioc_id: u8, timeout: u16, entries: &[Entry]) -> Vec<u8> {
    let len = (MPT_ENTRIES + ENTRY_LEN * entries.len()).max(PACKET_LEN);
    let mut p = vec![0u8; len];
    p.put_u8(0x00, CMD_MPT);
    p.put_u8(PACKET_CMD, mrioc_id);
    p.put_u16(MPT_TIMEOUT, timeout);
    p.put_u8(MPT_NUM_ENTRIES, entries.len() as u8);
    for (i, e) in entries.iter().enumerate() {
        let o = MPT_ENTRIES + i * ENTRY_LEN;
        p.put_u8(o, e.buf_type);
        p.put_u32(o + 4, e.len);
    }
    p
}

#[derive(Clone, Debug)]
pub struct Request {
    pub frame: Vec<u8>,
    pub data_out: Vec<u8>,
    pub data_in_len: usize,
    pub sense: bool,
    pub timeout: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub reply_type: u8,
    pub frame: Vec<u8>,
    pub data_in: Vec<u8>,
    pub sense: Vec<u8>,
}

impl Reply {
    pub fn is_address(&self) -> bool {
        self.reply_type == REPLY_TYPE_ADDRESS
    }

    pub fn ioc_status(&self) -> u16 {
        let off = if self.is_address() { 0x0A } else { 0x00 };
        self.frame.u16_at(off) & 0x7FFF
    }

    pub fn ioc_log_info(&self) -> u32 {
        let off = if self.is_address() { 0x0C } else { 0x04 };
        self.frame.u32_at(off)
    }
}

impl Request {
    pub fn new(function: u8, len: usize) -> Self {
        let mut frame = vec![0u8; len];
        frame.put_u8(0x03, function);
        Self {
            frame,
            data_out: Vec::new(),
            data_in_len: 0,
            sense: false,
            timeout: TIMEOUT_DEFAULT,
        }
    }

    pub fn function(&self) -> u8 {
        self.frame.u8_at(0x03)
    }

    pub fn entries(&self) -> Vec<Entry> {
        let mut e = vec![Entry {
            buf_type: BUF_MPI_REQUEST,
            len: self.frame.len() as u32,
        }];
        if !self.data_out.is_empty() {
            e.push(Entry {
                buf_type: BUF_DATA_OUT,
                len: self.data_out.len() as u32,
            });
        }
        if self.data_in_len > 0 {
            e.push(Entry {
                buf_type: BUF_DATA_IN,
                len: self.data_in_len as u32,
            });
        }
        e.push(Entry {
            buf_type: BUF_MPI_REPLY,
            len: MPI_REPLY_LEN as u32,
        });
        if self.sense {
            e.push(Entry {
                buf_type: BUF_ERR_RESPONSE,
                len: SENSE_LEN as u32,
            });
        }
        e
    }

    pub fn packet(&self, mrioc_id: u8) -> Vec<u8> {
        mpt_packet(mrioc_id, self.timeout, &self.entries())
    }

    pub fn dout(&self) -> Vec<u8> {
        let mut d = self.frame.clone();
        d.extend_from_slice(&self.data_out);
        d
    }

    pub fn din_len(&self) -> usize {
        self.data_in_len + MPI_REPLY_LEN + if self.sense { SENSE_LEN } else { 0 }
    }

    pub fn send(&self, t: &dyn Transport) -> Result<Reply> {
        let len = self.frame.len();
        if !(4..=MAX_REQUEST_FRAME).contains(&len) || !len.is_multiple_of(4) {
            bail!("MPI request frame of {len} bytes is not 4 to 128 bytes in whole dwords");
        }
        let mut din = vec![0u8; self.din_len()];
        t.submit(&self.packet(t.mrioc_id()), &self.dout(), &mut din)
            .with_context(|| {
                format!(
                    "MPI function 0x{:02x} on controller {}",
                    self.function(),
                    t.mrioc_id()
                )
            })?;
        let reply_at = self.data_in_len;
        let sense_at = reply_at + MPI_REPLY_LEN;
        let reply_type = din.u8_at(reply_at);
        let frame = din[reply_at + REPLY_HEADER_LEN..sense_at].to_vec();
        let sense = din[sense_at..].to_vec();
        din.truncate(reply_at);
        if reply_type != REPLY_TYPE_STATUS && reply_type != REPLY_TYPE_ADDRESS {
            bail!(
                "MPI function 0x{:02x} returned no reply (reply type {reply_type})",
                self.function()
            );
        }
        Ok(Reply {
            reply_type,
            frame,
            data_in: din,
            sense,
        })
    }

    pub fn send_checked(&self, t: &dyn Transport, what: &str) -> Result<Reply> {
        let reply = self.send(t)?;
        super::mpi::ensure_success(&reply, what)?;
        Ok(reply)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SgIo {
    pub request: u64,
    pub request_len: u32,
    pub response: u64,
    pub max_response_len: u32,
    pub dout: u64,
    pub dout_len: u32,
    pub din: u64,
    pub din_len: u32,
    pub timeout_ms: u32,
}

pub fn sg_io_v4(s: &SgIo) -> Vec<u8> {
    let mut h = vec![0u8; SG_IO_V4_LEN];
    h.put_u32(0x00, SG_GUARD);
    h.put_u32(0x04, BSG_PROTOCOL_SCSI);
    h.put_u32(0x08, BSG_SUB_PROTOCOL_SCSI_TRANSPORT);
    h.put_u32(0x0C, s.request_len);
    h.put_u64(0x10, s.request);
    h.put_u32(0x2C, s.max_response_len);
    h.put_u64(0x30, s.response);
    h.put_u32(0x3C, s.dout_len);
    h.put_u32(0x44, s.din_len);
    h.put_u64(0x48, s.dout);
    h.put_u64(0x50, s.din);
    h.put_u32(0x58, s.timeout_ms);
    h
}

pub struct Aligned {
    storage: Vec<u8>,
    offset: usize,
    len: usize,
}

impl Aligned {
    pub fn zeroed(len: usize) -> Self {
        let storage = vec![0u8; len + DMA_ALIGN];
        let addr = storage.as_ptr() as usize;
        let offset = (DMA_ALIGN - addr % DMA_ALIGN) % DMA_ALIGN;
        Self {
            storage,
            offset,
            len,
        }
    }

    pub fn from_slice(data: &[u8]) -> Self {
        let mut a = Self::zeroed(data.len());
        a.as_mut_slice().copy_from_slice(data);
        a
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.storage[self.offset..self.offset + self.len]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.storage[self.offset..self.offset + self.len]
    }

    pub fn addr(&mut self) -> u64 {
        if self.len == 0 {
            0
        } else {
            self.as_mut_slice().as_mut_ptr() as u64
        }
    }
}

pub struct LinuxTransport {
    dev: Device,
    mrioc_id: u8,
}

impl LinuxTransport {
    pub fn open(node: &Path, mrioc_id: u8) -> Result<Self> {
        let dev = Device::open(node).with_context(|| format!("opening {}", node.display()))?;
        Ok(Self { dev, mrioc_id })
    }
}

impl Transport for LinuxTransport {
    fn mrioc_id(&self) -> u8 {
        self.mrioc_id
    }

    fn submit(&self, packet: &[u8], dout: &[u8], din: &mut [u8]) -> Result<()> {
        let mut request = Aligned::from_slice(packet);
        let mut out = Aligned::from_slice(dout);
        let mut input = Aligned::zeroed(din.len());
        let mut response = Aligned::zeroed(BSG_RESPONSE_LEN);
        let mut header = sg_io_v4(&SgIo {
            request: request.addr(),
            request_len: packet.len() as u32,
            response: response.addr(),
            max_response_len: BSG_RESPONSE_LEN as u32,
            dout: out.addr(),
            dout_len: dout.len() as u32,
            din: input.addr(),
            din_len: din.len() as u32,
            timeout_ms: BLOCK_TIMEOUT_MS,
        });
        unsafe { self.dev.ioctl(SG_IO, header.as_mut_ptr()) }
            .with_context(|| format!("SG_IO on mpi3mrctl{}", self.mrioc_id))?;
        din.copy_from_slice(input.as_slice());
        Ok(())
    }
}
