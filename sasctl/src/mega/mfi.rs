use crate::bytes::LeMut;
use crate::mega::transport::FRAME_LEN;

pub const CMD_PD_SCSI_IO: u8 = 0x04;
pub const CMD_DCMD: u8 = 0x05;

pub const FLAG_DIR_NONE: u16 = 0x0000;
pub const FLAG_DIR_WRITE: u16 = 0x0008;
pub const FLAG_DIR_READ: u16 = 0x0010;

pub const HDR_CMD: usize = 0x00;
pub const HDR_SENSE_LEN: usize = 0x01;
pub const HDR_STATUS: usize = 0x02;
pub const HDR_TARGET: usize = 0x04;
pub const HDR_LUN: usize = 0x05;
pub const HDR_CDB_LEN: usize = 0x06;
pub const HDR_SGE_COUNT: usize = 0x07;
pub const HDR_FLAGS: usize = 0x10;
pub const HDR_TIMEOUT: usize = 0x12;
pub const HDR_XFER_LEN: usize = 0x14;
pub const DCMD_OPCODE: usize = 0x18;
pub const DCMD_MBOX: usize = 0x1c;
pub const PTHRU_CDB: usize = 0x20;

pub const DCMD_SGL_OFFSET: u32 = 0x28;
pub const PTHRU_SENSE_OFFSET: u32 = 0x18;
pub const PTHRU_SGL_OFFSET: u32 = 0x30;

pub const STATUS_OK: u8 = 0x00;
pub const STATUS_NOT_FOUND: u8 = 0x23;
pub const STATUS_SCSI_DONE_WITH_ERROR: u8 = 0x2d;
pub const STATUS_UNSET: u8 = 0xff;

pub mod op {
    pub const CTRL_GET_INFO: u32 = 0x0101_0000;
    pub const CTRL_GET_PROPS: u32 = 0x0102_0100;
    pub const CTRL_SET_PROPS: u32 = 0x0102_0200;
    pub const SPEAKER_GET: u32 = 0x0103_0100;
    pub const SPEAKER_ENABLE: u32 = 0x0103_0200;
    pub const SPEAKER_DISABLE: u32 = 0x0103_0300;
    pub const SPEAKER_SILENCE: u32 = 0x0103_0400;
    pub const EVENT_GET_INFO: u32 = 0x0104_0100;
    pub const EVENT_GET: u32 = 0x0104_0300;
    pub const CTRL_SHUTDOWN: u32 = 0x0105_0000;
    pub const PR_GET_STATUS: u32 = 0x0107_0100;
    pub const PR_GET_PROPERTIES: u32 = 0x0107_0200;
    pub const PR_SET_PROPERTIES: u32 = 0x0107_0300;
    pub const PR_START: u32 = 0x0107_0400;
    pub const PR_STOP: u32 = 0x0107_0500;
    pub const TIME_SECS_GET: u32 = 0x0108_0201;
    pub const FLASH_FW_OPEN: u32 = 0x010f_0100;
    pub const FLASH_FW_DOWNLOAD: u32 = 0x010f_0200;
    pub const FLASH_FW_FLASH: u32 = 0x010f_0300;
    pub const FLASH_FW_CLOSE: u32 = 0x010f_0400;
    pub const CTRL_CACHE_FLUSH: u32 = 0x0110_1000;
    pub const PD_GET_LIST: u32 = 0x0201_0000;
    pub const PD_GET_INFO: u32 = 0x0202_0000;
    pub const PD_STATE_SET: u32 = 0x0203_0100;
    pub const PD_REBUILD_START: u32 = 0x0204_0100;
    pub const PD_REBUILD_ABORT: u32 = 0x0204_0200;
    pub const PD_CLEAR_START: u32 = 0x0205_0100;
    pub const PD_CLEAR_ABORT: u32 = 0x0205_0200;
    pub const PD_LOCATE_START: u32 = 0x0207_0100;
    pub const PD_LOCATE_STOP: u32 = 0x0207_0200;
    pub const LD_GET_LIST: u32 = 0x0301_0000;
    pub const LD_GET_INFO: u32 = 0x0302_0000;
    pub const LD_GET_PROPERTIES: u32 = 0x0303_0000;
    pub const LD_SET_PROP: u32 = 0x0304_0000;
    pub const LD_DELETE: u32 = 0x0309_0000;
    pub const CFG_READ: u32 = 0x0401_0000;
    pub const CFG_ADD: u32 = 0x0402_0000;
    pub const CFG_CLEAR: u32 = 0x0403_0000;
    pub const CFG_MAKE_SPARE: u32 = 0x0404_0000;
    pub const CFG_REMOVE_SPARE: u32 = 0x0405_0000;
    pub const CFG_FOREIGN_SCAN: u32 = 0x0406_0100;
    pub const CFG_FOREIGN_DISPLAY: u32 = 0x0406_0200;
    pub const CFG_FOREIGN_PREVIEW: u32 = 0x0406_0300;
    pub const CFG_FOREIGN_IMPORT: u32 = 0x0406_0400;
    pub const CFG_FOREIGN_CLEAR: u32 = 0x0406_0500;
    pub const BBU_GET_STATUS: u32 = 0x0501_0000;
    pub const BBU_GET_CAPACITY_INFO: u32 = 0x0502_0000;
    pub const BBU_GET_DESIGN_INFO: u32 = 0x0503_0000;
    pub const BBU_START_LEARN: u32 = 0x0504_0000;
    pub const BBU_GET_PROP: u32 = 0x0505_0100;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    None,
    Read,
    Write,
}

impl Dir {
    pub fn flags(self) -> u16 {
        match self {
            Dir::None => FLAG_DIR_NONE,
            Dir::Read => FLAG_DIR_READ,
            Dir::Write => FLAG_DIR_WRITE,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mbox(pub [u8; 12]);

impl Mbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn byte(mut self, index: usize, value: u8) -> Self {
        self.0[index] = value;
        self
    }

    pub fn short(mut self, index: usize, value: u16) -> Self {
        self.0.put_u16(index * 2, value);
        self
    }

    pub fn word(mut self, index: usize, value: u32) -> Self {
        self.0.put_u32(index * 4, value);
        self
    }

    pub fn pd_ref(device_id: u16, seq_num: u16) -> Self {
        Self::new().short(0, device_id).short(1, seq_num)
    }
}

pub fn dcmd_frame(opcode: u32, mbox: &Mbox, dir: Dir, len: u32) -> [u8; FRAME_LEN] {
    let mut frame = [0u8; FRAME_LEN];
    let dir = if len == 0 { Dir::None } else { dir };
    frame[HDR_CMD] = CMD_DCMD;
    frame[HDR_STATUS] = STATUS_UNSET;
    frame[HDR_SGE_COUNT] = u8::from(len > 0);
    frame.put_u16(HDR_FLAGS, dir.flags());
    frame.put_u16(HDR_TIMEOUT, 0);
    frame.put_u32(HDR_XFER_LEN, len);
    frame.put_u32(DCMD_OPCODE, opcode);
    frame.put_bytes(DCMD_MBOX, &mbox.0);
    frame
}

pub fn pthru_frame(target: u8, cdb: &[u8], dir: Dir, len: u32, sense_len: u8) -> [u8; FRAME_LEN] {
    let mut frame = [0u8; FRAME_LEN];
    let dir = if len == 0 { Dir::None } else { dir };
    frame[HDR_CMD] = CMD_PD_SCSI_IO;
    frame[HDR_SENSE_LEN] = sense_len;
    frame[HDR_STATUS] = STATUS_UNSET;
    frame[HDR_TARGET] = target;
    frame[HDR_LUN] = 0;
    frame[HDR_CDB_LEN] = cdb.len().min(16) as u8;
    frame[HDR_SGE_COUNT] = u8::from(len > 0);
    frame.put_u16(HDR_FLAGS, dir.flags());
    frame.put_u16(HDR_TIMEOUT, 0);
    frame.put_u32(HDR_XFER_LEN, len);
    frame.put_bytes(PTHRU_CDB, &cdb[..cdb.len().min(16)]);
    frame
}

pub fn status_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "MFI_STAT_OK",
        0x01 => "MFI_STAT_INVALID_CMD",
        0x02 => "MFI_STAT_INVALID_DCMD",
        0x03 => "MFI_STAT_INVALID_PARAMETER",
        0x04 => "MFI_STAT_INVALID_SEQUENCE_NUMBER",
        0x05 => "MFI_STAT_ABORT_NOT_POSSIBLE",
        0x06 => "MFI_STAT_APP_HOST_CODE_NOT_FOUND",
        0x07 => "MFI_STAT_APP_IN_USE",
        0x08 => "MFI_STAT_APP_NOT_INITIALIZED",
        0x09 => "MFI_STAT_ARRAY_INDEX_INVALID",
        0x0a => "MFI_STAT_ARRAY_ROW_NOT_EMPTY",
        0x0b => "MFI_STAT_CONFIG_RESOURCE_CONFLICT",
        0x0c => "MFI_STAT_DEVICE_NOT_FOUND",
        0x0d => "MFI_STAT_DRIVE_TOO_SMALL",
        0x0e => "MFI_STAT_FLASH_ALLOC_FAIL",
        0x0f => "MFI_STAT_FLASH_BUSY",
        0x10 => "MFI_STAT_FLASH_ERROR",
        0x11 => "MFI_STAT_FLASH_IMAGE_BAD",
        0x12 => "MFI_STAT_FLASH_IMAGE_INCOMPLETE",
        0x13 => "MFI_STAT_FLASH_NOT_OPEN",
        0x14 => "MFI_STAT_FLASH_NOT_STARTED",
        0x15 => "MFI_STAT_FLUSH_FAILED",
        0x16 => "MFI_STAT_HOST_CODE_NOT_FOUND",
        0x17 => "MFI_STAT_LD_CC_IN_PROGRESS",
        0x18 => "MFI_STAT_LD_INIT_IN_PROGRESS",
        0x19 => "MFI_STAT_LD_LBA_OUT_OF_RANGE",
        0x1a => "MFI_STAT_LD_MAX_CONFIGURED",
        0x1b => "MFI_STAT_LD_NOT_OPTIMAL",
        0x1c => "MFI_STAT_LD_RBLD_IN_PROGRESS",
        0x1d => "MFI_STAT_LD_RECON_IN_PROGRESS",
        0x1e => "MFI_STAT_LD_WRONG_RAID_LEVEL",
        0x1f => "MFI_STAT_MAX_SPARES_EXCEEDED",
        0x20 => "MFI_STAT_MEMORY_NOT_AVAILABLE",
        0x21 => "MFI_STAT_MFC_HW_ERROR",
        0x22 => "MFI_STAT_NO_HW_PRESENT",
        0x23 => "MFI_STAT_NOT_FOUND",
        0x24 => "MFI_STAT_NOT_IN_ENCL",
        0x25 => "MFI_STAT_PD_CLEAR_IN_PROGRESS",
        0x26 => "MFI_STAT_PD_TYPE_WRONG",
        0x27 => "MFI_STAT_PR_DISABLED",
        0x28 => "MFI_STAT_ROW_INDEX_INVALID",
        0x29 => "MFI_STAT_SAS_CONFIG_INVALID_ACTION",
        0x2a => "MFI_STAT_SAS_CONFIG_INVALID_DATA",
        0x2b => "MFI_STAT_SAS_CONFIG_INVALID_PAGE",
        0x2c => "MFI_STAT_SAS_CONFIG_INVALID_TYPE",
        0x2d => "MFI_STAT_SCSI_DONE_WITH_ERROR",
        0x2e => "MFI_STAT_SCSI_IO_FAILED",
        0x2f => "MFI_STAT_SCSI_RESERVATION_CONFLICT",
        0x30 => "MFI_STAT_SHUTDOWN_FAILED",
        0x31 => "MFI_STAT_TIME_NOT_SET",
        0x32 => "MFI_STAT_WRONG_STATE",
        0x33 => "MFI_STAT_LD_OFFLINE",
        0x34 => "MFI_STAT_PEER_NOTIFICATION_REJECTED",
        0x35 => "MFI_STAT_PEER_NOTIFICATION_FAILED",
        0x36 => "MFI_STAT_RESERVATION_IN_PROGRESS",
        0x37 => "MFI_STAT_I2C_ERRORS_DETECTED",
        0x38 => "MFI_STAT_PCI_ERRORS_DETECTED",
        0x39 => "MFI_STAT_DIAG_FAILED",
        0x3a => "MFI_STAT_BOOT_MSG_PENDING",
        0x3b => "MFI_STAT_FOREIGN_CONFIG_INCOMPLETE",
        0x67 => "MFI_STAT_CONFIG_SEQ_MISMATCH",
        0xff => "MFI_STAT_INVALID_STATUS",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Le;

    #[test]
    fn dcmd_read_frame_matches_the_spec_layout() {
        let mbox = Mbox::new().byte(0, 1);
        let f = dcmd_frame(op::CTRL_GET_INFO, &mbox, Dir::Read, 2384);
        assert_eq!(f[0x00], 0x05);
        assert_eq!(f[0x02], 0xff);
        assert_eq!(f[0x07], 1);
        assert_eq!(f.u16_at(0x10), 0x0010);
        assert_eq!(f.u16_at(0x12), 0);
        assert_eq!(f.u32_at(0x14), 2384);
        assert_eq!(f.u32_at(0x18), 0x0101_0000);
        assert_eq!(&f[0x1c..0x28], &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(f[0x28..].iter().all(|b| *b == 0));
    }

    #[test]
    fn dcmd_without_data_has_no_direction_and_no_sge() {
        let mbox = Mbox::pd_ref(0x0123, 0x0456).short(2, 0x40);
        let f = dcmd_frame(op::PD_STATE_SET, &mbox, Dir::Write, 0);
        assert_eq!(f[0x07], 0);
        assert_eq!(f.u16_at(0x10), 0);
        assert_eq!(f.u32_at(0x14), 0);
        assert_eq!(f.u32_at(0x18), 0x0203_0100);
        assert_eq!(&f[0x1c..0x22], &[0x23, 0x01, 0x56, 0x04, 0x40, 0x00]);
    }

    #[test]
    fn dcmd_write_frame_sets_dir_write() {
        let f = dcmd_frame(op::CTRL_SET_PROPS, &Mbox::new(), Dir::Write, 64);
        assert_eq!(f.u16_at(0x10), 0x0008);
        assert_eq!(f.u32_at(0x14), 64);
    }

    #[test]
    fn mbox_words_are_little_endian() {
        let m = Mbox::new().word(0, 0x1122_3344).word(1, 0xff00_ffff);
        assert_eq!(&m.0[..8], &[0x44, 0x33, 0x22, 0x11, 0xff, 0xff, 0x00, 0xff]);
    }

    #[test]
    fn pthru_frame_matches_the_spec_layout() {
        let cdb = [0x4d, 0, 0x4d, 0, 0, 0, 0, 0x01, 0x00, 0];
        let f = pthru_frame(9, &cdb, Dir::Read, 256, 96);
        assert_eq!(f[0x00], 0x04);
        assert_eq!(f[0x01], 96);
        assert_eq!(f[0x02], 0xff);
        assert_eq!(f[0x04], 9);
        assert_eq!(f[0x05], 0);
        assert_eq!(f[0x06], 10);
        assert_eq!(f[0x07], 1);
        assert_eq!(f.u16_at(0x10), 0x0010);
        assert_eq!(f.u32_at(0x14), 256);
        assert_eq!(f.u64_at(0x18), 0);
        assert_eq!(&f[0x20..0x2a], &cdb);
        assert!(f[0x2a..0x30].iter().all(|b| *b == 0));
    }

    #[test]
    fn status_names_cover_linux_and_freebsd_codes() {
        assert_eq!(status_name(0x0c), Some("MFI_STAT_DEVICE_NOT_FOUND"));
        assert_eq!(
            status_name(0x3b),
            Some("MFI_STAT_FOREIGN_CONFIG_INCOMPLETE")
        );
        assert_eq!(status_name(0x67), Some("MFI_STAT_CONFIG_SEQ_MISMATCH"));
        assert_eq!(status_name(0x40), None);
    }
}
