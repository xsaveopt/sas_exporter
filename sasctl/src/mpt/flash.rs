use anyhow::{Result, bail};
use serde::Serialize;

use super::config::{self, IOC_0, MANUFACTURING_0};
use super::fw::{self, ImageFormat, ImageHeader};
use super::mpi::{self, FUNCTION_FW_DOWNLOAD, Request, TIMEOUT_FIRMWARE};
use super::pages::{Ioc0, Manufacturing0};
use super::transport::{Generation, Transport};
use crate::bytes::{Le, LeMut};
use crate::output::{Fields, Render};

pub const DOWNLOAD_TYPE_FW: u8 = 0x01;
pub const DOWNLOAD_TYPE_BIOS: u8 = 0x02;
pub const UPLOAD_TYPE_FW_BACKUP: u8 = 0x05;
pub const MSGFLAGS_LAST_SEGMENT: u8 = 0x01;
pub const SAS2_CHUNK: usize = 0x4000;
pub const VERIFY_PIECE: usize = 0x10000;

const VENDOR_LSI: u16 = 0x1000;
const EXT_HEADER_LEN: usize = 0x40;
const EXT_TYPE_NVDATA: u8 = 0x03;
const EXT_TYPE_SUPPORTED_DEVICES: u8 = 0x07;
const CHAIN_LIMIT: usize = 64;

const ROM_SIGNATURE: [u8; 2] = [0x55, 0xAA];
const ROM_BLANK: [u8; 2] = [0x55, 0xBB];
const PCIR_LEN: usize = 24;
const CODE_X86: u8 = 0;
const CODE_FCODE: u8 = 1;
const CODE_EFI: u8 = 3;
const CODE_EXTENSION: u8 = 0xFF;
const INDICATOR_LAST: u8 = 0x80;
const SAS2_FAMILY: u32 = 0x1_0000;

#[derive(Clone, Debug, Default)]
pub struct Controller {
    pub product_id: u16,
    pub device_id: u16,
    pub revision_id: u8,
    pub board_name: Option<String>,
}

pub fn controller(t: &dyn Transport) -> Result<Controller> {
    let facts = mpi::ioc_facts(t)?;
    let ioc0 = Ioc0::parse(&config::require_page(t, IOC_0, 0)?);
    let board_name =
        config::read_page(t, MANUFACTURING_0, 0)?.map(|p| Manufacturing0::parse(&p).board_name);
    Ok(Controller {
        product_id: facts.product_id,
        device_id: ioc0.device_id,
        revision_id: ioc0.revision_id,
        board_name,
    })
}

pub fn word_sum(data: &[u8]) -> u32 {
    data.as_chunks::<4>()
        .0
        .iter()
        .fold(0u32, |sum, w| sum.wrapping_add(u32::from_le_bytes(*w)))
}

fn ext_offset(image: &[u8], offset: u32) -> Result<usize> {
    let o = offset as usize;
    if image.len() < EXT_HEADER_LEN || o > image.len() - EXT_HEADER_LEN {
        bail!("image NextImageHeaderOffset 0x{offset:x} points outside the file");
    }
    Ok(o)
}

fn check_nvdata(image: &[u8], o: usize, size: u32, board: Option<&str>) -> Result<()> {
    if size as usize <= EXT_HEADER_LEN {
        bail!("image carries an NVDATA extended image with no data");
    }
    let Some(board) = board else {
        return Ok(());
    };
    let nvdata = o + EXT_HEADER_LEN;
    let cdh_size = image.u8_at(nvdata + 0x0C) as usize;
    let product = nvdata + cdh_size * 4 + 0x0C;
    if product + 16 > image.len() {
        bail!("image NVDATA product id lies outside the file");
    }
    let name = image.ascii_at(product, 16);
    if name != board {
        bail!("image NVDATA is for board {name:?}, this controller is {board:?}");
    }
    Ok(())
}

fn check_supported_devices(image: &[u8], o: usize, ctl: &Controller) -> Result<()> {
    let data = o + EXT_HEADER_LEN;
    let count = image.u8_at(data + 2) as usize;
    let matched = (0..count).any(|i| {
        let e = data + 8 + i * 16;
        let device = image.u16_at(e);
        let mask = image.u16_at(e + 4);
        let (low, high) = (image.u8_at(e + 8), image.u8_at(e + 9));
        ctl.device_id & !mask == device && low <= ctl.revision_id && ctl.revision_id <= high
    });
    if !matched {
        bail!(
            "image supported device list does not include device 0x{:04x} revision 0x{:02x}",
            ctl.device_id,
            ctl.revision_id
        );
    }
    Ok(())
}

pub fn validate_firmware(image: &[u8], ctl: &Controller) -> Result<ImageHeader> {
    let header = fw::parse_image_header(image)?;
    if header.format == ImageFormat::Mpi26Component {
        bail!("MPI 2.6 component images are not supported, use a full firmware package");
    }
    if u32::try_from(image.len()).is_err() {
        bail!("image is too large");
    }
    let sum = word_sum(image);
    if sum != 0 {
        bail!("image checksum is invalid, the 32-bit word sum is 0x{sum:08x} instead of 0");
    }
    if header.vendor_id != VENDOR_LSI {
        bail!(
            "image vendor id is 0x{:04x}, expected 0x{VENDOR_LSI:04x}",
            header.vendor_id
        );
    }
    if header.product_id != ctl.product_id {
        bail!(
            "image product id is 0x{:04x}, this controller is 0x{:04x}",
            header.product_id,
            ctl.product_id
        );
    }
    let mut total = image.u32_at(0x2C) as u64;
    let mut next = image.u32_at(0x30);
    let mut steps = 0;
    while next != 0 {
        steps += 1;
        if steps > CHAIN_LIMIT {
            bail!("image extended header chain does not end");
        }
        let o = ext_offset(image, next)?;
        let kind = image.u8_at(o);
        let size = image.u32_at(o + 0x08);
        match kind {
            EXT_TYPE_NVDATA => check_nvdata(image, o, size, ctl.board_name.as_deref())?,
            EXT_TYPE_SUPPORTED_DEVICES => check_supported_devices(image, o, ctl)?,
            _ => {}
        }
        total += size as u64;
        next = image.u32_at(o + 0x0C);
    }
    if total != image.len() as u64 {
        bail!(
            "image is {} bytes but its headers describe {total} bytes",
            image.len()
        );
    }
    Ok(header)
}

pub fn download_request(
    generation: Generation,
    image_type: u8,
    total: u32,
    offset: u32,
    chunk: &[u8],
    last: bool,
) -> Request {
    let sge_offset = match generation {
        Generation::Sas2 => 9,
        Generation::Sas3 => 8,
    };
    let mut r = Request::new(FUNCTION_FW_DOWNLOAD, sge_offset);
    r.frame.put_u8(0x00, image_type);
    if last {
        r.frame.put_u8(0x07, MSGFLAGS_LAST_SEGMENT);
    }
    r.frame.put_u32(0x0C, total);
    match generation {
        Generation::Sas2 => {
            r.frame.put_u8(0x14 + 0x01, 0);
            r.frame.put_u8(0x14 + 0x02, 12);
            r.frame.put_u8(0x14 + 0x03, 0);
            r.frame.put_u32(0x14 + 0x08, offset);
            r.frame.put_u32(0x14 + 0x0C, chunk.len() as u32);
        }
        Generation::Sas3 => {
            r.frame.put_u32(0x18, offset);
            r.frame.put_u32(0x1C, chunk.len() as u32);
        }
    }
    r.data_out = chunk.to_vec();
    r.timeout = TIMEOUT_FIRMWARE;
    r
}

pub fn download_requests(generation: Generation, image_type: u8, image: &[u8]) -> Vec<Request> {
    let total = image.len() as u32;
    match generation {
        Generation::Sas2 => {
            let count = image.len().div_ceil(SAS2_CHUNK);
            image
                .chunks(SAS2_CHUNK)
                .enumerate()
                .map(|(i, chunk)| {
                    download_request(
                        generation,
                        image_type,
                        total,
                        (i * SAS2_CHUNK) as u32,
                        chunk,
                        i + 1 == count,
                    )
                })
                .collect()
        }
        Generation::Sas3 => vec![download_request(
            generation, image_type, total, 0, image, true,
        )],
    }
}

pub fn download(t: &dyn Transport, image_type: u8, image: &[u8]) -> Result<usize> {
    if image.is_empty() {
        bail!("image is empty");
    }
    let requests = download_requests(t.generation(), image_type, image);
    for (i, r) in requests.iter().enumerate() {
        r.send_checked(t, &format!("FW download chunk {}", i + 1))?;
    }
    Ok(requests.len())
}

pub fn verify_firmware(t: &dyn Transport, image: &[u8]) -> Result<()> {
    let mut offset = 0usize;
    loop {
        let reply = fw::upload_request(
            t.generation(),
            UPLOAD_TYPE_FW_BACKUP,
            offset as u32,
            VERIFY_PIECE as u32,
        )
        .send_checked(t, "FW upload for verification")?;
        let actual = reply.reply.u32_at(0x14) as usize;
        if actual != image.len() {
            bail!(
                "verification failed, the controller reports a {actual} byte image but {} bytes were written",
                image.len()
            );
        }
        let piece = VERIFY_PIECE.min(actual - offset);
        let got = reply.data_in.get(..piece).unwrap_or(&[]);
        if got != &image[offset..offset + piece] {
            bail!("verification failed, the flashed image differs at offset 0x{offset:x}");
        }
        offset += piece;
        if offset >= actual {
            return Ok(());
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Flashed {
    pub kind: &'static str,
    pub file: String,
    pub bytes: usize,
    pub requests: usize,
    pub verified: bool,
    pub header: Option<ImageHeader>,
    pub images: Vec<&'static str>,
}

impl Render for Flashed {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled(format!("{} flashed", self.kind));
        f.add("File", &self.file)
            .add("Bytes", self.bytes)
            .add("Requests", self.requests);
        if let Some(h) = &self.header {
            f.add("Firmware version", &h.firmware_version)
                .add("Version name", &h.version_name);
        }
        if !self.images.is_empty() {
            f.add("Images", self.images.join(", "));
        }
        f.add(
            "Verified",
            if self.verified { "yes" } else { "not checked" },
        );
        f.render(out);
    }
}

pub fn flash_firmware(t: &dyn Transport, file: &str, image: &[u8]) -> Result<Flashed> {
    let ctl = controller(t)?;
    let header = validate_firmware(image, &ctl)?;
    let requests = download(t, DOWNLOAD_TYPE_FW, image)?;
    verify_firmware(t, image)?;
    Ok(Flashed {
        kind: "Firmware",
        file: file.to_string(),
        bytes: image.len(),
        requests,
        verified: true,
        header: Some(header),
        images: Vec::new(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RomPiece {
    pub start: usize,
    pub end: usize,
    pub code_type: u8,
}

fn pcir_offset(image: &[u8]) -> Option<usize> {
    let p = image.u16_at(0x18) as usize;
    (p + PCIR_LEN < image.len() && image.get(p..p + 4) == Some(b"PCIR")).then_some(p)
}

pub fn split_rom(file: &[u8]) -> Result<Vec<RomPiece>> {
    let mut pieces = Vec::new();
    let mut o = 0;
    while o < file.len() {
        let rest = &file[o..];
        match rest.get(..2) {
            Some(s) if s == ROM_SIGNATURE => {}
            Some(s) if s == ROM_BLANK => bail!("image at offset 0x{o:x} is the blank placeholder"),
            _ => bail!("no 0x55 0xAA ROM signature at offset 0x{o:x}"),
        }
        let Some(p) = pcir_offset(rest) else {
            bail!("image at offset 0x{o:x} has no valid PCIR structure");
        };
        let len = rest.u16_at(p + 0x10) as usize * 512;
        if len == 0 || len > rest.len() {
            bail!(
                "image at offset 0x{o:x} declares {len} bytes but {} remain",
                rest.len()
            );
        }
        pieces.push(RomPiece {
            start: o,
            end: o + len,
            code_type: rest.u8_at(p + 0x14),
        });
        o += len;
        if rest.u8_at(p + 0x15) & INDICATOR_LAST != 0 && o < file.len() {
            bail!("file has data after the image marked last");
        }
    }
    if pieces.is_empty() {
        bail!("file is empty");
    }
    Ok(pieces)
}

fn has_what_string(data: &[u8]) -> bool {
    data.windows(4).any(|w| w == b"@(#)")
}

fn family(device_id: u16) -> u32 {
    match device_id & !1 {
        0x0064 | 0x0070 | 0x0072 | 0x0074 | 0x0076 => SAS2_FAMILY,
        _ => device_id as u32,
    }
}

fn fcode_checksum_ok(seg: &[u8]) -> bool {
    let len = seg.u32_at(0x38) as usize;
    let end = len.saturating_add(0x34);
    if end > seg.len() || end < 0x3C {
        return false;
    }
    let mut sum: u32 = seg[0x3C..end].iter().map(|b| *b as u32).sum();
    while sum > 0xFFFF {
        sum -= 0xFFFF;
    }
    sum == seg.u16_at(0x36) as u32
}

fn code_name(code_type: u8) -> &'static str {
    match code_type {
        CODE_X86 => "x86 BIOS",
        CODE_FCODE => "FCode",
        CODE_EFI => "EFI BIOS",
        _ => "extension",
    }
}

pub fn validate_rom_segment(seg: &[u8], main_len: usize, device_id: u16) -> Result<u8> {
    if seg.get(..2) != Some(&ROM_SIGNATURE[..]) {
        bail!("ROM signature is not 0x55 0xAA");
    }
    if !seg.len().is_multiple_of(512) {
        bail!("ROM image length {} is not a multiple of 512", seg.len());
    }
    let Some(p) = pcir_offset(seg) else {
        bail!("ROM image has no valid PCIR structure");
    };
    let code_type = seg.u8_at(p + 0x14);
    let name = code_name(code_type);
    if !matches!(code_type, CODE_X86 | CODE_FCODE | CODE_EFI) {
        bail!("ROM image code type {code_type} is not x86, FCode or EFI");
    }
    if (code_type == CODE_X86 || has_what_string(seg))
        && seg.iter().fold(0u8, |s, b| s.wrapping_add(*b)) != 0
    {
        bail!("{name} image byte checksum is invalid");
    }
    if code_type == CODE_FCODE && !fcode_checksum_ok(seg) {
        bail!("FCode image checksum is invalid");
    }
    let vendor = seg.u16_at(p + 4);
    if vendor != VENDOR_LSI {
        bail!("{name} image PCI vendor id is 0x{vendor:04x}, expected 0x{VENDOR_LSI:04x}");
    }
    let device = seg.u16_at(p + 6);
    if code_type != CODE_EFI && family(device) != family(device_id) {
        bail!(
            "{name} image is for PCI device 0x{device:04x}, not compatible with 0x{device_id:04x}"
        );
    }
    if seg.u16_at(p + 0x10) as usize * 512 != main_len {
        bail!("{name} image length field does not match the image");
    }
    Ok(code_type)
}

pub fn fixup_rom_image(image: &mut [u8], device_id: u16, last: bool) {
    let n = match pcir_offset(image) {
        Some(p) => {
            let code_type = image.u8_at(p + 0x14);
            if code_type != CODE_EXTENSION {
                image.put_u16(p + 6, device_id);
            }
            let indicator = image.u8_at(p + 0x15);
            image.put_u8(
                p + 0x15,
                if last {
                    indicator | INDICATOR_LAST
                } else {
                    indicator & !INDICATOR_LAST
                },
            );
            if code_type == CODE_FCODE && !has_what_string(image) {
                return;
            }
            (image.u16_at(p + 0x10) as usize * 512).min(image.len())
        }
        None => image.len(),
    };
    if n == 0 {
        return;
    }
    let sum = image[..n - 1].iter().fold(0u8, |s, b| s.wrapping_add(*b));
    image[n - 1] = sum.wrapping_neg();
}

struct Segment {
    code_type: u8,
    main: Vec<u8>,
    extensions: Vec<u8>,
}

pub fn build_bios_region(file: &[u8], device_id: u16) -> Result<(Vec<u8>, Vec<&'static str>)> {
    let pieces = split_rom(file)?;
    let mut segments: Vec<Segment> = Vec::new();
    let mut i = 0;
    while i < pieces.len() {
        let main = &pieces[i];
        if main.code_type == CODE_EXTENSION {
            bail!(
                "extension image at offset 0x{:x} does not follow an x86 or EFI image",
                main.start
            );
        }
        let mut j = i + 1;
        while j < pieces.len() && pieces[j].code_type == CODE_EXTENSION {
            j += 1;
        }
        let seg = &file[main.start..pieces[j - 1].end];
        let code_type = validate_rom_segment(seg, main.end - main.start, device_id)?;
        if segments.iter().any(|s| s.code_type == code_type) {
            bail!("file carries more than one {} image", code_name(code_type));
        }
        segments.push(Segment {
            code_type,
            main: file[main.start..main.end].to_vec(),
            extensions: file[main.end..pieces[j - 1].end].to_vec(),
        });
        i = j;
    }
    let take = |code: u8| segments.iter().find(|s| s.code_type == code);
    let (x86, fcode, efi) = (take(CODE_X86), take(CODE_FCODE), take(CODE_EFI));
    let mut region = Vec::new();
    let mut names = Vec::new();
    if let Some(s) = x86 {
        let mut m = s.main.clone();
        fixup_rom_image(&mut m, device_id, fcode.is_none() && efi.is_none());
        region.extend_from_slice(&m);
        names.push(code_name(CODE_X86));
    }
    if let Some(s) = fcode {
        let mut m = s.main.clone();
        fixup_rom_image(&mut m, device_id, efi.is_none());
        region.extend_from_slice(&m);
        region.extend_from_slice(&s.extensions);
        names.push(code_name(CODE_FCODE));
    }
    if let Some(s) = efi {
        let mut m = s.main.clone();
        fixup_rom_image(&mut m, device_id, true);
        region.extend_from_slice(&m);
        names.push(code_name(CODE_EFI));
    }
    if let Some(s) = x86 {
        region.extend_from_slice(&s.extensions);
    }
    if let Some(s) = efi {
        region.extend_from_slice(&s.extensions);
    }
    Ok((region, names))
}

pub fn flash_bios(t: &dyn Transport, file: &str, data: &[u8]) -> Result<Flashed> {
    let ioc0 = Ioc0::parse(&config::require_page(t, IOC_0, 0)?);
    let (region, images) = build_bios_region(data, ioc0.device_id)?;
    let requests = download(t, DOWNLOAD_TYPE_BIOS, &region)?;
    Ok(Flashed {
        kind: "BIOS",
        file: file.to_string(),
        bytes: region.len(),
        requests,
        verified: false,
        header: None,
        images,
    })
}
