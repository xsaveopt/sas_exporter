use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::bytes::{Le, LeMut};
use crate::ioctl::{Device, iowr};

pub const FRAME_LEN: usize = 128;
pub const MAX_SGE: usize = 16;
pub const NODE_NAME: &str = "megaraid_sas_ioctl";
pub const DEFAULT_NODE: &str = "/dev/megaraid_sas_ioctl_node";

#[cfg(target_pointer_width = "64")]
const IOVEC: usize = 16;
#[cfg(target_pointer_width = "32")]
const IOVEC: usize = 8;

const PKT_HOST: usize = 0;
const PKT_SGL_OFF: usize = 4;
const PKT_SGE_COUNT: usize = 8;
const PKT_SENSE_OFF: usize = 12;
const PKT_SENSE_LEN: usize = 16;
const PKT_FRAME: usize = 20;
const PKT_SGL: usize = PKT_FRAME + FRAME_LEN;
const PKT_LEN: usize = PKT_SGL + MAX_SGE * IOVEC;

pub const STATUS_OFFSET: usize = 2;

pub trait Transport {
    fn host_no(&self) -> u16;
    fn firmware(
        &self,
        frame: &mut [u8; FRAME_LEN],
        sgl_off: u32,
        bufs: &mut [&mut [u8]],
        sense: Option<(u32, &mut [u8])>,
    ) -> Result<()>;
    fn reset_host(&self, sysfs: &Path) -> Result<()>;
}

pub struct LinuxTransport {
    dev: Device,
    host: u16,
}

impl LinuxTransport {
    pub fn open(node: &Path, host: u16) -> Result<Self> {
        let dev = Device::open(node).with_context(|| format!("opening {}", node.display()))?;
        Ok(Self { dev, host })
    }
}

impl Transport for LinuxTransport {
    fn reset_host(&self, sysfs: &Path) -> Result<()> {
        crate::mega::reset::sg_reset_host(sysfs, u32::from(self.host))?;
        Ok(())
    }

    fn host_no(&self) -> u16 {
        self.host
    }

    fn firmware(
        &self,
        frame: &mut [u8; FRAME_LEN],
        sgl_off: u32,
        bufs: &mut [&mut [u8]],
        sense: Option<(u32, &mut [u8])>,
    ) -> Result<()> {
        if bufs.len() > MAX_SGE {
            bail!("at most {MAX_SGE} buffers per frame");
        }
        let mut pkt = vec![0u8; PKT_LEN];
        pkt.put_u16(PKT_HOST, self.host);
        pkt.put_u32(PKT_SGL_OFF, sgl_off);
        pkt.put_u32(PKT_SGE_COUNT, bufs.len() as u32);
        let mut frame_copy = *frame;
        if let Some((off, buf)) = &sense {
            if *off as usize > 56 {
                bail!("sense offset {off} beyond the frame");
            }
            pkt.put_u32(PKT_SENSE_OFF, *off);
            pkt.put_u32(PKT_SENSE_LEN, buf.len() as u32);
            frame_copy.put_u64(*off as usize, buf.as_ptr() as u64);
        }
        pkt.put_bytes(PKT_FRAME, &frame_copy);
        for (i, b) in bufs.iter_mut().enumerate() {
            let at = PKT_SGL + i * IOVEC;
            if IOVEC == 16 {
                pkt.put_u64(at, b.as_mut_ptr() as u64);
                pkt.put_u64(at + 8, b.len() as u64);
            } else {
                pkt.put_u32(at, b.as_mut_ptr() as u32);
                pkt.put_u32(at + 4, b.len() as u32);
            }
        }
        let request = iowr(b'M', 1, PKT_LEN);
        unsafe { self.dev.ioctl(request, pkt.as_mut_ptr()) }
            .with_context(|| format!("megaraid firmware ioctl on host {}", self.host))?;
        frame[STATUS_OFFSET] = pkt.u8_at(PKT_FRAME + STATUS_OFFSET);
        Ok(())
    }
}

pub fn char_major(proc_devices: &str) -> Option<u32> {
    let mut in_char = false;
    for line in proc_devices.lines() {
        let line = line.trim();
        if line.starts_with("Character devices") {
            in_char = true;
            continue;
        }
        if line.starts_with("Block devices") {
            in_char = false;
            continue;
        }
        if !in_char {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(major), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name == NODE_NAME {
            return major.parse().ok();
        }
    }
    None
}

pub fn ensure_node(path: &Path) -> Result<PathBuf> {
    let devices = fs::read_to_string("/proc/devices").context("reading /proc/devices")?;
    let Some(major) = char_major(&devices) else {
        bail!("megaraid_sas driver is not loaded ({NODE_NAME} missing from /proc/devices)");
    };
    make_node(path, major)?;
    Ok(path.to_path_buf())
}

#[cfg(target_os = "linux")]
fn make_node(path: &Path, major: u32) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;

    let want = libc::makedev(major, 0);
    if let Ok(meta) = fs::metadata(path) {
        if meta.file_type().is_char_device() && meta.rdev() == want {
            return Ok(());
        }
        fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    let c = CString::new(path.as_os_str().as_bytes())?;
    let rc = unsafe { libc::mknod(c.as_ptr(), libc::S_IFCHR | 0o600, want) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("creating {}", path.display()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn make_node(_path: &Path, _major: u32) -> Result<()> {
    bail!("megaraid management is only supported on Linux")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_is_the_kernel_size() {
        if IOVEC == 16 {
            assert_eq!(PKT_LEN, 404);
        } else {
            assert_eq!(PKT_LEN, 276);
        }
    }

    #[test]
    fn finds_major_in_character_block() {
        let text = "Character devices:\n  1 mem\n244 megaraid_sas_ioctl\n\nBlock devices:\n  8 megaraid_sas_ioctl\n";
        assert_eq!(char_major(text), Some(244));
        assert_eq!(char_major("Character devices:\n  1 mem\n"), None);
    }

    fn null_transport(host: u16) -> LinuxTransport {
        LinuxTransport {
            dev: Device::open(Path::new("/dev/null")).unwrap(),
            host,
        }
    }

    #[test]
    fn firmware_refuses_too_many_buffers_before_the_ioctl() {
        let t = null_transport(7);
        let mut frame = [0u8; FRAME_LEN];
        let mut storage = vec![vec![0u8; 4]; MAX_SGE + 1];
        let mut bufs: Vec<&mut [u8]> = storage.iter_mut().map(|b| b.as_mut_slice()).collect();
        let err = t.firmware(&mut frame, 0x28, &mut bufs, None).unwrap_err();
        assert_eq!(err.to_string(), "at most 16 buffers per frame");
        assert_eq!(frame, [0u8; FRAME_LEN]);
    }

    #[test]
    fn firmware_refuses_a_sense_pointer_outside_the_frame() {
        let t = null_transport(7);
        let mut frame = [0u8; FRAME_LEN];
        let mut sense = [0u8; 32];
        let err = t
            .firmware(&mut frame, 0x28, &mut [], Some((57, &mut sense[..])))
            .unwrap_err();
        assert_eq!(err.to_string(), "sense offset 57 beyond the frame");
    }

    #[test]
    fn firmware_reports_the_host_when_the_ioctl_fails() {
        let t = null_transport(7);
        assert_eq!(t.host_no(), 7);
        let mut frame = [0u8; FRAME_LEN];
        frame[STATUS_OFFSET] = 0xff;
        let mut data = [0u8; 8];
        let mut bufs: [&mut [u8]; 1] = [&mut data];
        let mut sense = [0u8; 32];
        let err = t
            .firmware(&mut frame, 0x28, &mut bufs, Some((56, &mut sense[..])))
            .unwrap_err();
        assert!(
            format!("{err:#}").starts_with("megaraid firmware ioctl on host 7"),
            "{err:#}"
        );
        assert_eq!(frame[STATUS_OFFSET], 0xff);
        assert_eq!(&frame[56..64], &[0u8; 8]);
    }

    #[test]
    fn reset_host_goes_through_the_scsi_generic_node() {
        let t = null_transport(4);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-mega-transport-reset");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("class/scsi_generic")).unwrap();
        let err = t.reset_host(&root).unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(err.to_string().contains("host 4"), "{err}");
    }

    #[test]
    fn char_major_ignores_malformed_and_other_lines() {
        let text =
            "Character devices:\n\n  x\nabc megaraid_sas_ioctl\n 10 megaraid_sas_ioctl_other\n";
        assert_eq!(char_major(text), None);
        let text =
            "Block devices:\n  9 megaraid_sas_ioctl\nCharacter devices:\n 12 megaraid_sas_ioctl\n";
        assert_eq!(char_major(text), Some(12));
    }
}
