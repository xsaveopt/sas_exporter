use std::cell::{Cell, RefCell};
use std::path::Path;

use anyhow::Result;

use crate::bytes::Le;
use crate::mega::mfi::{
    CMD_DCMD, CMD_PD_SCSI_IO, DCMD_MBOX, DCMD_OPCODE, HDR_STATUS, HDR_TARGET, PTHRU_CDB,
};
use crate::mega::transport::{FRAME_LEN, Transport};

const INVALID_DCMD: u8 = 0x02;

#[derive(Clone, Debug)]
pub struct Call {
    pub frame: [u8; FRAME_LEN],
    pub sgl_off: u32,
    pub bufs: Vec<Vec<u8>>,
    pub sense_off: Option<u32>,
}

impl Call {
    pub fn opcode(&self) -> u32 {
        self.frame.u32_at(DCMD_OPCODE)
    }

    pub fn mbox(&self) -> &[u8] {
        &self.frame[DCMD_MBOX..DCMD_MBOX + 12]
    }
}

enum Key {
    Dcmd { opcode: u32, mbox_prefix: Vec<u8> },
    Scsi { target: u8, cdb_prefix: Vec<u8> },
}

struct Rule {
    key: Key,
    status: u8,
    data: Vec<u8>,
    sense: Vec<u8>,
}

pub struct Mock {
    rules: Vec<Rule>,
    calls: RefCell<Vec<Call>>,
    resets: Cell<u32>,
}

impl Mock {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            calls: RefCell::new(Vec::new()),
            resets: Cell::new(0),
        }
    }

    fn push(mut self, key: Key, status: u8, data: Vec<u8>, sense: Vec<u8>) -> Self {
        self.rules.push(Rule {
            key,
            status,
            data,
            sense,
        });
        self
    }

    pub fn reply(self, opcode: u32, data: Vec<u8>) -> Self {
        self.push(
            Key::Dcmd {
                opcode,
                mbox_prefix: Vec::new(),
            },
            0,
            data,
            Vec::new(),
        )
    }

    pub fn reply_mbox(self, opcode: u32, mbox_prefix: &[u8], data: Vec<u8>) -> Self {
        self.push(
            Key::Dcmd {
                opcode,
                mbox_prefix: mbox_prefix.to_vec(),
            },
            0,
            data,
            Vec::new(),
        )
    }

    pub fn status(self, opcode: u32, status: u8) -> Self {
        self.push(
            Key::Dcmd {
                opcode,
                mbox_prefix: Vec::new(),
            },
            status,
            Vec::new(),
            Vec::new(),
        )
    }

    pub fn scsi(self, target: u8, cdb_prefix: &[u8], data: Vec<u8>) -> Self {
        self.push(
            Key::Scsi {
                target,
                cdb_prefix: cdb_prefix.to_vec(),
            },
            0,
            data,
            Vec::new(),
        )
    }

    pub fn scsi_error(self, target: u8, cdb_prefix: &[u8], status: u8, sense: Vec<u8>) -> Self {
        self.push(
            Key::Scsi {
                target,
                cdb_prefix: cdb_prefix.to_vec(),
            },
            status,
            Vec::new(),
            sense,
        )
    }

    pub fn resets(&self) -> u32 {
        self.resets.get()
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    pub fn opcodes(&self) -> Vec<u32> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.frame[0] == CMD_DCMD)
            .map(Call::opcode)
            .collect()
    }

    fn find(&self, frame: &[u8; FRAME_LEN]) -> Option<&Rule> {
        self.rules.iter().find(|r| match &r.key {
            Key::Dcmd {
                opcode,
                mbox_prefix,
            } => {
                frame[0] == CMD_DCMD
                    && frame.u32_at(DCMD_OPCODE) == *opcode
                    && frame[DCMD_MBOX..].starts_with(mbox_prefix)
            }
            Key::Scsi { target, cdb_prefix } => {
                frame[0] == CMD_PD_SCSI_IO
                    && frame[HDR_TARGET] == *target
                    && frame[PTHRU_CDB..].starts_with(cdb_prefix)
            }
        })
    }
}

impl Transport for Mock {
    fn reset_host(&self, _sysfs: &Path) -> Result<()> {
        self.resets.set(self.resets.get() + 1);
        Ok(())
    }

    fn host_no(&self) -> u16 {
        0
    }

    fn firmware(
        &self,
        frame: &mut [u8; FRAME_LEN],
        sgl_off: u32,
        bufs: &mut [&mut [u8]],
        sense: Option<(u32, &mut [u8])>,
    ) -> Result<()> {
        self.calls.borrow_mut().push(Call {
            frame: *frame,
            sgl_off,
            bufs: bufs.iter().map(|b| b.to_vec()).collect(),
            sense_off: sense.as_ref().map(|(off, _)| *off),
        });
        let Some(rule) = self.find(frame) else {
            frame[HDR_STATUS] = INVALID_DCMD;
            return Ok(());
        };
        if let Some(buf) = bufs.first_mut() {
            let n = buf.len().min(rule.data.len());
            buf[..n].copy_from_slice(&rule.data[..n]);
        }
        if let Some((_, sense_buf)) = sense {
            let n = sense_buf.len().min(rule.sense.len());
            sense_buf[..n].copy_from_slice(&rule.sense[..n]);
        }
        frame[HDR_STATUS] = rule.status;
        Ok(())
    }
}
