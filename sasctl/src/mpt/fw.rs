use anyhow::{Result, bail};
use serde::Serialize;

use super::mpi::{FUNCTION_FW_UPLOAD, Request, TIMEOUT_FIRMWARE, version_string};
use super::transport::{Generation, Transport};
use crate::bytes::{Le, LeMut};

pub const UPLOAD_TYPE_FW_FLASH: u8 = 0x01;
pub const UPLOAD_TYPE_BIOS_FLASH: u8 = 0x02;

const MPI2_SIGNATURE_MASK: u32 = 0xFF00_0000;
const MPI2_SIGNATURE: u32 = 0xEA00_0000;
const MPI26_SIGNATURE: u32 = 0xEB00_0000;
const MPI2_SIGNATURE0: u32 = 0x5AFA_A55A;
const MPI26_SIGNATURE0_BASE: u32 = 0x5AEA_A500;
const MPI2_SIGNATURE1: u32 = 0xA55A_FAA5;
const MPI26_SIGNATURE1: u32 = 0xA55A_EAA5;
const MPI2_SIGNATURE2: u32 = 0x5AA5_5AFA;
const MPI26_SIGNATURE2: u32 = 0x5AA5_5AEA;
const MPI26_COMPONENT_SIGNATURE0: u32 = 0xEB00_0042;
pub const IMAGE_HEADER_SIZE: usize = 0x100;

pub fn upload_request(generation: Generation, image_type: u8, offset: u32, size: u32) -> Request {
    match generation {
        Generation::Sas2 => {
            let mut r = Request::new(FUNCTION_FW_UPLOAD, 9);
            r.frame.put_u8(0x00, image_type);
            r.frame.put_u8(0x14 + 0x01, 0);
            r.frame.put_u8(0x14 + 0x02, 12);
            r.frame.put_u8(0x14 + 0x03, 0);
            r.frame.put_u32(0x14 + 0x08, offset);
            r.frame.put_u32(0x14 + 0x0C, size);
            r.data_in_len = size as usize;
            r.timeout = TIMEOUT_FIRMWARE;
            r
        }
        Generation::Sas3 => {
            let mut r = Request::new(FUNCTION_FW_UPLOAD, 8);
            r.frame.put_u8(0x00, image_type);
            r.frame.put_u32(0x18, offset);
            r.frame.put_u32(0x1C, size);
            r.data_in_len = size as usize;
            r.timeout = TIMEOUT_FIRMWARE;
            r
        }
    }
}

pub fn upload(t: &dyn Transport, image_type: u8) -> Result<Vec<u8>> {
    let probe = upload_request(t.generation(), image_type, 0, 0).send_checked(t, "FW upload")?;
    let size = probe.reply.u32_at(0x14);
    if size == 0 {
        bail!("the controller reported an empty image for image type 0x{image_type:02x}");
    }
    let reply = upload_request(t.generation(), image_type, 0, size).send_checked(t, "FW upload")?;
    let actual = reply.reply.u32_at(0x14) as usize;
    let mut data = reply.data_in;
    if actual > 0 {
        data.truncate(actual);
    }
    Ok(data)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Mpi2,
    Mpi26,
    Mpi26Component,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImageHeader {
    pub format: ImageFormat,
    pub firmware_version: String,
    pub nvdata_version: String,
    pub package_version: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub image_size: u32,
    pub version_name: String,
    pub package_name: String,
}

pub fn parse_image_header(d: &[u8]) -> Result<ImageHeader> {
    if d.len() < IMAGE_HEADER_SIZE {
        bail!(
            "image is {} bytes, shorter than the {IMAGE_HEADER_SIZE} byte header",
            d.len()
        );
    }
    if d.u32_at(0x00) == MPI26_COMPONENT_SIGNATURE0 {
        return Ok(ImageHeader {
            format: ImageFormat::Mpi26Component,
            firmware_version: version_string(d.u32_at(0x54)),
            nvdata_version: version_string(d.u32_at(0x58)),
            package_version: String::new(),
            vendor_id: 0,
            product_id: 0,
            image_size: d.u32_at(0x08),
            version_name: String::new(),
            package_name: String::new(),
        });
    }
    let signature = d.u32_at(0x00) & MPI2_SIGNATURE_MASK;
    let (s0, s1, s2) = (d.u32_at(0x04), d.u32_at(0x08), d.u32_at(0x0C));
    let format = if signature == MPI2_SIGNATURE
        && s0 == MPI2_SIGNATURE0
        && s1 == MPI2_SIGNATURE1
        && s2 == MPI2_SIGNATURE2
    {
        ImageFormat::Mpi2
    } else if signature == MPI26_SIGNATURE
        && s0 & 0xFFFF_FF00 == MPI26_SIGNATURE0_BASE
        && s1 == MPI26_SIGNATURE1
        && s2 == MPI26_SIGNATURE2
    {
        ImageFormat::Mpi26
    } else {
        bail!("image does not carry an MPI firmware header signature");
    };
    Ok(ImageHeader {
        format,
        firmware_version: version_string(d.u32_at(0x14)),
        nvdata_version: version_string(d.u32_at(0x18)),
        package_version: version_string(d.u32_at(0x1C)),
        vendor_id: d.u16_at(0x20),
        product_id: d.u16_at(0x22),
        image_size: d.u32_at(0x2C),
        version_name: d.ascii_at(0x68, 32),
        package_name: d.ascii_at(0xB0, 32),
    })
}
