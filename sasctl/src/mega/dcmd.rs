use std::fmt;

use anyhow::{Result, bail};

use crate::bytes::Le;
use crate::mega::mfi::{
    DCMD_SGL_OFFSET, Dir, HDR_STATUS, Mbox, STATUS_OK, STATUS_UNSET, dcmd_frame, status_name,
};
use crate::mega::transport::Transport;

pub const VARIABLE_FIRST_READ: usize = 1024;
pub const VARIABLE_MAX: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MfiError {
    pub opcode: u32,
    pub status: u8,
}

impl fmt::Display for MfiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status == STATUS_UNSET {
            return write!(
                f,
                "firmware command {:#010x} did not complete (status left at 0xff)",
                self.opcode
            );
        }
        match status_name(self.status) {
            Some(name) => write!(
                f,
                "firmware command {:#010x} failed with {} ({:#04x})",
                self.opcode, name, self.status
            ),
            None => write!(
                f,
                "firmware command {:#010x} failed with unknown status {:#04x}",
                self.opcode, self.status
            ),
        }
    }
}

impl std::error::Error for MfiError {}

pub fn check(opcode: u32, status: u8) -> Result<(), MfiError> {
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(MfiError { opcode, status })
    }
}

pub fn status_of(err: &anyhow::Error) -> Option<u8> {
    err.downcast_ref::<MfiError>().map(|e| e.status)
}

fn exec(t: &dyn Transport, opcode: u32, mbox: &Mbox, dir: Dir, buf: &mut [u8]) -> Result<()> {
    let mut frame = dcmd_frame(opcode, mbox, dir, buf.len() as u32);
    if buf.is_empty() {
        t.firmware(&mut frame, DCMD_SGL_OFFSET, &mut [], None)?;
    } else {
        let mut bufs: [&mut [u8]; 1] = [buf];
        t.firmware(&mut frame, DCMD_SGL_OFFSET, &mut bufs, None)?;
    }
    check(opcode, frame[HDR_STATUS])?;
    Ok(())
}

pub fn dcmd(t: &dyn Transport, opcode: u32, mbox: &Mbox, dir: Dir, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    exec(t, opcode, mbox, dir, &mut buf)?;
    Ok(buf)
}

pub fn dcmd_read(t: &dyn Transport, opcode: u32, mbox: &Mbox, len: usize) -> Result<Vec<u8>> {
    dcmd(t, opcode, mbox, Dir::Read, len)
}

pub fn dcmd_write(t: &dyn Transport, opcode: u32, mbox: &Mbox, data: &[u8]) -> Result<()> {
    let mut buf = data.to_vec();
    exec(t, opcode, mbox, Dir::Write, &mut buf)
}

pub fn dcmd_none(t: &dyn Transport, opcode: u32, mbox: &Mbox) -> Result<()> {
    exec(t, opcode, mbox, Dir::None, &mut [])
}

pub fn dcmd_variable(t: &dyn Transport, opcode: u32, mbox: &Mbox) -> Result<Vec<u8>> {
    let first = dcmd_read(t, opcode, mbox, VARIABLE_FIRST_READ)?;
    let size = first.u32_at(0) as usize;
    if size <= first.len() {
        return Ok(first);
    }
    if size > VARIABLE_MAX {
        bail!("firmware command {opcode:#010x} reported an implausible size of {size} bytes");
    }
    dcmd_read(t, opcode, mbox, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mega::mfi::op;
    use crate::mega::mock::Mock;

    #[test]
    fn maps_status_codes_to_named_errors() {
        let mock = Mock::new().status(op::PD_GET_INFO, 0x0c);
        let err = dcmd_read(&mock, op::PD_GET_INFO, &Mbox::new().short(0, 7), 512).unwrap_err();
        assert_eq!(status_of(&err), Some(0x0c));
        assert_eq!(
            err.to_string(),
            "firmware command 0x02020000 failed with MFI_STAT_DEVICE_NOT_FOUND (0x0c)"
        );
    }

    #[test]
    fn unknown_and_untouched_status_are_reported() {
        let e = MfiError {
            opcode: 0x0101_0000,
            status: 0x50,
        };
        assert!(e.to_string().contains("unknown status 0x50"));
        let e = MfiError {
            opcode: 0x0101_0000,
            status: 0xff,
        };
        assert!(e.to_string().contains("did not complete"));
    }

    #[test]
    fn presets_status_and_passes_one_buffer_at_the_dcmd_sgl() {
        let mock = Mock::new().reply(op::CTRL_GET_PROPS, vec![7u8; 64]);
        let data = dcmd_read(&mock, op::CTRL_GET_PROPS, &Mbox::new(), 64).unwrap();
        assert_eq!(data, vec![7u8; 64]);
        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].frame[2], 0xff);
        assert_eq!(calls[0].sgl_off, 0x28);
        assert_eq!(calls[0].bufs.len(), 1);
        assert_eq!(calls[0].bufs[0].len(), 64);
    }

    #[test]
    fn no_data_commands_send_no_buffers() {
        let mock = Mock::new().reply(op::PR_START, vec![]);
        dcmd_none(&mock, op::PR_START, &Mbox::new()).unwrap();
        let calls = mock.calls();
        assert!(calls[0].bufs.is_empty());
        assert_eq!(calls[0].frame[7], 0);
    }

    #[test]
    fn variable_reads_grow_to_the_reported_size() {
        let mut big = vec![0u8; 3000];
        big[..4].copy_from_slice(&3000u32.to_le_bytes());
        big[2999] = 0xaa;
        let mock = Mock::new().reply(op::CFG_READ, big);
        let data = dcmd_variable(&mock, op::CFG_READ, &Mbox::new()).unwrap();
        assert_eq!(data.len(), 3000);
        assert_eq!(data[2999], 0xaa);
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].bufs[0].len(), 1024);
        assert_eq!(calls[1].bufs[0].len(), 3000);
    }

    fn sized(size: usize) -> Vec<u8> {
        let mut b = vec![0u8; VARIABLE_FIRST_READ];
        b[..4].copy_from_slice(&(size as u32).to_le_bytes());
        b
    }

    #[test]
    fn variable_reads_refuse_an_implausible_size_after_one_read() {
        let mock = Mock::new().reply(op::CFG_READ, sized(VARIABLE_MAX + 1));
        let err = dcmd_variable(&mock, op::CFG_READ, &Mbox::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "firmware command 0x04010000 reported an implausible size of {} bytes",
                VARIABLE_MAX + 1
            )
        );
        assert_eq!(mock.calls().len(), 1);
        let mock = Mock::new().reply(op::CFG_READ, sized(u32::MAX as usize));
        assert!(dcmd_variable(&mock, op::CFG_READ, &Mbox::new()).is_err());
        assert_eq!(mock.calls().len(), 1);
    }

    #[test]
    fn variable_reads_accept_the_largest_size() {
        let mock = Mock::new().reply(op::CFG_READ, sized(VARIABLE_MAX));
        let data = dcmd_variable(&mock, op::CFG_READ, &Mbox::new()).unwrap();
        assert_eq!(data.len(), VARIABLE_MAX);
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].bufs[0].len(), VARIABLE_MAX);
    }

    #[test]
    fn variable_reads_stop_at_the_first_read_when_it_fits() {
        for size in [0, 16, VARIABLE_FIRST_READ] {
            let mock = Mock::new().reply(op::CFG_READ, sized(size));
            let data = dcmd_variable(&mock, op::CFG_READ, &Mbox::new()).unwrap();
            assert_eq!(data.len(), VARIABLE_FIRST_READ, "size {size}");
            assert_eq!(mock.calls().len(), 1, "size {size}");
        }
    }

    #[test]
    fn variable_reads_pass_a_failed_first_read_through() {
        let mock = Mock::new().status(op::CFG_READ, 0x0c);
        let err = dcmd_variable(&mock, op::CFG_READ, &Mbox::new()).unwrap_err();
        assert_eq!(status_of(&err), Some(0x0c));
        assert_eq!(mock.calls().len(), 1);
    }
}
