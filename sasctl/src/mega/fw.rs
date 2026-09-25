use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::mega::ctrl::{Component, CtrlInfo};
use crate::mega::dcmd::{dcmd_none, dcmd_read, dcmd_write};
use crate::mega::mfi::{Mbox, op};
use crate::mega::transport::Transport;

pub const CHUNK: usize = 64 * 1024;
pub const IMAGE_ALIGN: usize = 1024;
pub const IMAGE_MAX: usize = 0x7fff_ffff;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInfo {
    pub product_name: String,
    pub package_version: String,
    pub firmware_version: String,
    pub components: Vec<Component>,
    pub pending_components: Vec<Component>,
}

impl FirmwareInfo {
    pub fn from_info(info: &CtrlInfo) -> Self {
        Self {
            product_name: info.product_name.clone(),
            package_version: info.package_version.clone(),
            firmware_version: info.firmware_version(),
            components: info.image_components.clone(),
            pending_components: info.pending_image_components.clone(),
        }
    }
}

pub fn validate(image: &[u8]) -> Result<()> {
    if image.is_empty() {
        bail!("firmware image is empty");
    }
    if !image.len().is_multiple_of(IMAGE_ALIGN) {
        bail!(
            "firmware image is {} bytes, which is not a multiple of {IMAGE_ALIGN}",
            image.len()
        );
    }
    if image.len() > IMAGE_MAX {
        bail!(
            "firmware image is {} bytes, larger than the controller accepts",
            image.len()
        );
    }
    Ok(())
}

pub fn flash(t: &dyn Transport, image: &[u8]) -> Result<()> {
    validate(image)?;
    dcmd_none(
        t,
        op::FLASH_FW_OPEN,
        &Mbox::new().word(0, image.len() as u32),
    )
    .context("allocating controller memory for the image")?;
    let result = download_and_flash(t, image);
    if result.is_err() {
        let _ = dcmd_none(t, op::FLASH_FW_CLOSE, &Mbox::new());
    }
    result
}

fn download_and_flash(t: &dyn Transport, image: &[u8]) -> Result<()> {
    for (i, chunk) in image.chunks(CHUNK).enumerate() {
        let offset = (i * CHUNK) as u32;
        dcmd_write(
            t,
            op::FLASH_FW_DOWNLOAD,
            &Mbox::new().word(0, offset),
            chunk,
        )
        .with_context(|| format!("downloading the image at offset {offset}"))?;
    }
    dcmd_read(t, op::FLASH_FW_FLASH, &Mbox::new(), 4).context("flashing the image")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Le;
    use crate::mega::mock::Mock;

    #[test]
    fn validates_image_size() {
        assert!(validate(&[]).is_err());
        assert!(validate(&vec![0u8; 1000]).is_err());
        assert!(validate(&vec![0u8; 2048]).is_ok());
    }

    #[test]
    fn flashes_in_64k_chunks_without_close_on_success() {
        let image: Vec<u8> = (0..CHUNK * 2 + 3072).map(|i| (i % 251) as u8).collect();
        let mock = Mock::new()
            .reply(op::FLASH_FW_OPEN, vec![])
            .reply(op::FLASH_FW_DOWNLOAD, vec![])
            .reply(op::FLASH_FW_FLASH, vec![]);
        flash(&mock, &image).unwrap();
        let calls = mock.calls();
        assert_eq!(
            mock.opcodes(),
            vec![
                op::FLASH_FW_OPEN,
                op::FLASH_FW_DOWNLOAD,
                op::FLASH_FW_DOWNLOAD,
                op::FLASH_FW_DOWNLOAD,
                op::FLASH_FW_FLASH
            ]
        );
        assert_eq!(calls[0].mbox().u32_at(0), image.len() as u32);
        assert!(calls[0].bufs.is_empty());
        assert_eq!(calls[2].mbox().u32_at(0), CHUNK as u32);
        assert_eq!(calls[3].mbox().u32_at(0), (2 * CHUNK) as u32);
        assert_eq!(calls[3].bufs[0], image[2 * CHUNK..]);
        assert_eq!(calls[1].frame.u16_at(0x10), 0x0008);
        assert_eq!(calls[4].bufs[0].len(), 4);
        assert_eq!(calls[4].frame.u16_at(0x10), 0x0010);
    }

    #[test]
    fn a_failed_download_closes_the_flash_session() {
        let mock = Mock::new()
            .reply(op::FLASH_FW_OPEN, vec![])
            .status(op::FLASH_FW_DOWNLOAD, 0x11)
            .reply(op::FLASH_FW_CLOSE, vec![]);
        let e = flash(&mock, &vec![0u8; 4096]).unwrap_err();
        assert!(format!("{e:#}").contains("MFI_STAT_FLASH_IMAGE_BAD"));
        assert_eq!(
            mock.opcodes(),
            vec![op::FLASH_FW_OPEN, op::FLASH_FW_DOWNLOAD, op::FLASH_FW_CLOSE]
        );
    }

    #[test]
    fn an_invalid_image_sends_nothing() {
        let mock = Mock::new();
        assert!(flash(&mock, &vec![0u8; 1500]).is_err());
        assert!(mock.calls().is_empty());
    }
}
