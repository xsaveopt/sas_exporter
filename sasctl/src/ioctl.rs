use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

pub const fn iowr(ty: u8, nr: u8, size: usize) -> u64 {
    (3u64 << 30) | ((size as u64 & 0x3FFF) << 16) | ((ty as u64) << 8) | nr as u64
}

pub struct Device {
    file: File,
}

impl Device {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    pub unsafe fn ioctl(&self, request: u64, arg: *mut u8) -> io::Result<()> {
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, arg) };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_numbers_match_the_kernel_headers() {
        assert_eq!(iowr(b'L', 17, 92), 0xC05C_4C11);
        assert_eq!(iowr(b'L', 20, 72), 0xC048_4C14);
        assert_eq!(iowr(b'L', 20, 56), 0xC038_4C14);
        assert_eq!(iowr(b'M', 1, 404), 0xC194_4D01);
        assert_eq!(iowr(b'M', 1, 276), 0xC114_4D01);
    }
}
