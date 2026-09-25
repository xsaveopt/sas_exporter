use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use super::adapter::{self, IocInfo, Target};
use super::cli;
use super::config::{self, PageId};
use super::diag::{self, BufferType};
use super::flash;
use super::fw;
use super::inventory::{self, BootTarget, DriveAddress};
use super::ircfg;
use super::mpi::{self, IocFacts};
use super::pages::{self, Bios2, BootDevice, Enclosure0, RaidPhysDisk0, RaidVolume0, SasDevice0};
use super::raid;
use super::scsi::{self, Path};
use super::transport::{Command as Wire, Generation, Reply, Transport};
use crate::Ctx;
use crate::bytes::{Le, LeMut};
use crate::output::{Format, Render};
use crate::sysfs::ScsiHost;

#[derive(Clone, Debug)]
struct Sent {
    frame: Vec<u8>,
    sge_offset: u32,
    data_out: Vec<u8>,
    data_in_len: usize,
}

#[derive(Default)]
struct Mock {
    generation: Option<Generation>,
    pages: HashMap<(u8, u8, u8, u32), Vec<u8>>,
    scsi: HashMap<(u16, u8, u8), Vec<u8>>,
    replies: HashMap<u8, Vec<u8>>,
    raw_answers: HashMap<u8, Vec<u8>>,
    uploads: HashMap<u8, Vec<u8>>,
    sent: RefCell<Vec<Sent>>,
    raw_calls: RefCell<Vec<(u8, usize, Vec<u8>)>>,
}

impl Mock {
    fn sas3() -> Self {
        Self {
            generation: Some(Generation::Sas3),
            ..Default::default()
        }
    }

    fn page(&mut self, id: PageId, address: u32, body: Vec<u8>) {
        self.pages
            .insert((id.page_type, id.number, id.ext_type, address), body);
    }

    fn config_reply(&self, frame: &[u8], data_in_len: usize) -> Reply {
        let mut reply = vec![0u8; 128];
        reply.put_u8(0x03, mpi::FUNCTION_CONFIG);
        let action = frame.u8_at(0x00);
        let key = (
            frame.u8_at(0x17) & 0x0F,
            frame.u8_at(0x16),
            frame.u8_at(0x06),
            frame.u32_at(0x18),
        );
        let Some(page) = self.pages.get(&key) else {
            reply.put_u16(0x0E, mpi::IOCSTATUS_CONFIG_INVALID_PAGE);
            return Reply {
                reply,
                ..Default::default()
            };
        };
        reply.put_bytes(0x14, &page[0..4]);
        if key.0 == 0x0F {
            reply.put_u16(0x04, page.u16_at(4));
            reply.put_u8(0x06, page.u8_at(6));
        }
        let data_in = if action == config::ACTION_READ_CURRENT {
            let mut d = page.clone();
            d.resize(data_in_len, 0);
            d
        } else {
            vec![0u8; data_in_len]
        };
        Reply {
            reply,
            data_in,
            sense: Vec::new(),
        }
    }

    fn upload_reply(&self, frame: &[u8], data_in_len: usize) -> Reply {
        let mut reply = vec![0u8; 128];
        reply.put_u8(0x03, mpi::FUNCTION_FW_UPLOAD);
        let mut data_in = vec![0u8; data_in_len];
        if let Some(image) = self.uploads.get(&frame.u8_at(0x00)) {
            let offset = match self.generation() {
                Generation::Sas2 => frame.u32_at(0x1C),
                Generation::Sas3 => frame.u32_at(0x18),
            } as usize;
            reply.put_u32(0x14, image.len() as u32);
            let end = (offset + data_in_len).min(image.len());
            if offset < end {
                data_in[..end - offset].copy_from_slice(&image[offset..end]);
            }
        }
        Reply {
            reply,
            data_in,
            sense: Vec::new(),
        }
    }

    fn scsi_reply(&self, frame: &[u8], data_in_len: usize) -> Reply {
        let handle = frame.u16_at(0);
        let cdb = &frame[0x40..0x50];
        let sub = match cdb[0] {
            0x12 if cdb[1] & 1 == 1 => cdb[2],
            0x4D => cdb[2] & 0x3F,
            _ => 0,
        };
        match self.scsi.get(&(handle, cdb[0], sub)) {
            Some(d) => {
                let mut data = d.clone();
                data.resize(data_in_len, 0);
                Reply {
                    reply: vec![0u8; 128],
                    data_in: data,
                    sense: vec![0u8; 96],
                }
            }
            None => {
                let mut reply = vec![0u8; 128];
                reply.put_u8(0x02, 13);
                reply.put_u8(0x0C, 0x02);
                let mut sense = vec![0u8; 96];
                sense[0] = 0x70;
                sense[2] = 0x05;
                sense[12] = 0x24;
                Reply {
                    reply,
                    data_in: vec![0u8; data_in_len],
                    sense,
                }
            }
        }
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.borrow().clone()
    }

    fn writes(&self) -> usize {
        self.sent.borrow().len() + self.raw_calls.borrow().len()
    }
}

impl Transport for Mock {
    fn ioc_number(&self) -> u32 {
        0
    }

    fn generation(&self) -> Generation {
        self.generation.unwrap_or(Generation::Sas3)
    }

    fn raw(&self, nr: u8, encoded_len: usize, buf: &mut [u8]) -> Result<()> {
        self.raw_calls
            .borrow_mut()
            .push((nr, encoded_len, buf.to_vec()));
        if let Some(answer) = self.raw_answers.get(&nr) {
            let n = answer.len().min(buf.len());
            buf[..n].copy_from_slice(&answer[..n]);
        }
        Ok(())
    }

    fn command(&self, cmd: &Wire) -> Result<Reply> {
        self.sent.borrow_mut().push(Sent {
            frame: cmd.frame.to_vec(),
            sge_offset: cmd.sge_offset,
            data_out: cmd.data_out.to_vec(),
            data_in_len: cmd.data_in_len,
        });
        let function = cmd.frame.u8_at(3);
        Ok(match function {
            mpi::FUNCTION_CONFIG => self.config_reply(cmd.frame, cmd.data_in_len),
            mpi::FUNCTION_FW_UPLOAD if !self.uploads.is_empty() => {
                self.upload_reply(cmd.frame, cmd.data_in_len)
            }
            mpi::FUNCTION_SCSI_IO | mpi::FUNCTION_RAID_SCSI_IO_PASSTHROUGH => {
                self.scsi_reply(cmd.frame, cmd.data_in_len)
            }
            f => {
                let reply = self.replies.get(&f).cloned().unwrap_or_else(|| {
                    let mut r = vec![0u8; 128];
                    r.put_u8(0x03, f);
                    r
                });
                Reply {
                    reply,
                    data_in: vec![0u8; cmd.data_in_len],
                    sense: Vec::new(),
                }
            }
        })
    }
}

fn std_page(id: PageId, len: usize) -> Vec<u8> {
    let mut p = vec![0u8; len];
    p[0] = id.version;
    p[1] = (len / 4) as u8;
    p[2] = id.number;
    p[3] = id.page_type;
    p
}

fn ext_page(id: PageId, len: usize) -> Vec<u8> {
    let mut p = vec![0u8; len];
    p[0] = id.version;
    p[2] = id.number;
    p[3] = 0x0F;
    p.put_u16(4, (len / 4) as u16);
    p[6] = id.ext_type;
    p
}

fn sas_device(handle: u16, enclosure: u16, slot: u16, info: u32, sas: u64) -> Vec<u8> {
    let mut p = ext_page(config::SAS_DEVICE_0, 56);
    p.put_u16(0x08, slot);
    p.put_u16(0x0A, enclosure);
    p.put_u64(0x0C, sas);
    p.put_u16(0x14, 1);
    p.put_u8(0x16, 4);
    p.put_u16(0x18, handle);
    p.put_u32(0x1C, info);
    p.put_u64(0x24, sas + 2);
    p.put_bytes(0x30, b"C0.1");
    p
}

const SAS_DISK: u32 = pages::DEVICE_INFO_SSP_TARGET | 0x1;
const SATA_DISK: u32 = pages::DEVICE_INFO_SATA_DEVICE | pages::DEVICE_INFO_STP_TARGET | 0x1;

fn ctx(yes: bool) -> Ctx {
    Ctx {
        format: Format::Text,
        yes,
        sysfs: PathBuf::new(),
    }
}

fn host(host_no: u32, proc_name: &str, unique_id: Option<u32>) -> ScsiHost {
    ScsiHost {
        host_no,
        proc_name: proc_name.to_string(),
        unique_id,
        pci_address: None,
        path: PathBuf::from(format!("/nonexistent/host{host_no}")),
    }
}

fn target() -> Target {
    Target {
        index: 0,
        generation: Generation::Sas3,
        ioc_number: 0,
        host: host(3, "mpt3sas", Some(0)),
    }
}

fn run(mock: &Mock, yes: bool, args: &[&str]) -> Result<String> {
    use clap::Parser;
    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        args: cli::Args,
    }
    let mut argv = vec!["sasctl", "-c", "0"];
    argv.extend_from_slice(args);
    let h = Harness::try_parse_from(argv).map_err(|e| anyhow::anyhow!("{e}"))?;
    let out = cli::execute(&h.args.command, &ctx(yes), &target(), mock)?;
    Ok(out.text())
}

fn inquiry_data(vendor: &str, product: &str, rev: &str) -> Vec<u8> {
    let mut d = vec![b' '; 36];
    d[0] = 0;
    d[1..8].fill(0);
    d[8..8 + vendor.len()].copy_from_slice(vendor.as_bytes());
    d[16..16 + product.len()].copy_from_slice(product.as_bytes());
    d[32..32 + rev.len()].copy_from_slice(rev.as_bytes());
    d
}

fn add_sas_disk(mock: &mut Mock) {
    let dev = sas_device(0x0009, 2, 0, SAS_DISK, 0x5000_c500_85e7_bd3d);
    mock.page(config::SAS_DEVICE_0, 0xFFFF, dev.clone());
    mock.page(
        config::SAS_DEVICE_0,
        config::SAS_DEVICE_FORM_HANDLE | 9,
        dev,
    );
    mock.scsi.insert(
        (9, 0x12, 0),
        inquiry_data("SEAGATE", "ST4000NM0023", "0004"),
    );
    let mut serial = vec![0x00, 0x80, 0x00, 0x08];
    serial.extend_from_slice(b"Z1Z3ABCD");
    mock.scsi.insert((9, 0x12, 0x80), serial);
    let mut devid = vec![0x00, 0x83, 0x00, 0x0C, 0x01, 0x03, 0x00, 0x08];
    devid.extend_from_slice(&0x5000_c500_85e7_bd3f_u64.to_be_bytes());
    mock.scsi.insert((9, 0x12, 0x83), devid);
    let mut cap = vec![0u8; 32];
    cap[0..8].copy_from_slice(&7_814_037_167u64.to_be_bytes());
    cap[8..12].copy_from_slice(&512u32.to_be_bytes());
    mock.scsi.insert((9, 0x9E, 0), cap);
    mock.scsi
        .insert((9, 0x12, 0xB1), vec![0x00, 0xB1, 0x00, 0x3C, 0x1C, 0x20]);
    mock.scsi.insert(
        (9, 0x4D, 0x0D),
        vec![
            0x0D, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x03, 0x02, 0x00, 34, 0x00, 0x01, 0x03, 0x02, 0x00,
            60,
        ],
    );
}

#[test]
fn request_builders_put_fields_at_spec_offsets() {
    let r = config::config_request(
        config::ACTION_PAGE_HEADER,
        config::SAS_DEVICE_0,
        0xFFFF,
        None,
    );
    assert_eq!(r.sge_offset, 7);
    assert_eq!(r.frame.len(), 28);
    assert_eq!(r.frame[0x03], 0x04);
    assert_eq!(r.frame[0x06], 0x12);
    assert_eq!(&r.frame[0x14..0x18], &[0x09, 0x00, 0x00, 0x0F]);
    assert_eq!(r.frame.u32_at(0x18), 0xFFFF);

    let f = mpi::ioc_facts_request();
    assert_eq!((f.sge_offset, f.frame.len(), f.frame[3]), (3, 12, 0x03));

    let s = mpi::sep_locate_request(2, 5, true);
    assert_eq!(s.sge_offset, 8);
    assert_eq!(s.frame.len(), 32);
    assert_eq!(s.frame[0x03], 0x18);
    assert_eq!(s.frame[0x04], 0x00);
    assert_eq!(s.frame[0x05], 0x01);
    assert_eq!(s.frame.u32_at(0x0C), 0x0002_0000);
    assert_eq!(s.frame.u16_at(0x1C), 5);
    assert_eq!(s.frame.u16_at(0x1E), 2);
    assert_eq!(mpi::sep_locate_request(2, 5, false).frame.u32_at(0x0C), 0);

    let p = mpi::phy_reset_request(3, true);
    assert_eq!((p.sge_offset, p.frame.len()), (11, 44));
    assert_eq!((p.frame[0], p.frame[3], p.frame[0x0E]), (0x07, 0x1B, 3));
    assert_eq!(mpi::phy_reset_request(3, false).frame[0], 0x06);
}

#[test]
fn raid_action_builders_match_the_action_table() {
    let a = raid::volume_action_request(raid::ACTION_ACTIVATE_VOLUME, 0x143);
    assert_eq!((a.sge_offset, a.frame.len()), (5, 20));
    assert_eq!((a.frame[0], a.frame[3]), (0x11, 0x15));
    assert_eq!(a.frame.u16_at(0x04), 0x143);
    assert_eq!(a.frame.u32_at(0x10), 0);

    let c = raid::consistency_check_request(0x143);
    assert_eq!(c.frame[0], 0x21);
    assert_eq!(c.frame.u16_at(0x04), 0x143);
    assert_eq!((c.frame[0x10], c.frame[0x11]), (0x02, 0x00));

    let o = raid::physdisk_action_request(raid::ACTION_PHYSDISK_OFFLINE, 7);
    assert_eq!((o.frame[0], o.frame[0x06]), (0x0A, 7));
    assert_eq!(o.frame.u16_at(0x04), 0);
    let h = raid::physdisk_action_request(raid::ACTION_DELETE_HOT_SPARE, 2);
    assert_eq!((h.frame[0], h.frame[0x06]), (0x1E, 2));
}

#[test]
fn scsi_request_matches_the_minimal_recipe() {
    let r = scsi::scsi_read_request(0x0009, Path::Direct, &scsi::read_capacity16_cdb(), 32);
    assert_eq!((r.sge_offset, r.frame.len()), (24, 96));
    assert_eq!(r.frame.u16_at(0x00), 9);
    assert_eq!(r.frame[0x03], 0x00);
    assert_eq!(r.frame[0x14], 24);
    assert_eq!(r.frame.u32_at(0x1C), 32);
    assert_eq!(r.frame.u16_at(0x24), 16);
    assert_eq!(r.frame.u32_at(0x3C), 0x0200_0000);
    assert_eq!(&r.frame[0x40..0x42], &[0x9E, 0x10]);
    assert_eq!(r.frame.u32_at(0x0C), 0);
    assert_eq!(r.frame[0x12], 0);
    assert_eq!(r.data_in_len, 32);
    let m = scsi::scsi_read_request(0x0009, Path::RaidMember, &scsi::inquiry_cdb(), 96);
    assert_eq!(m.frame[0x03], 0x16);
    assert_eq!(scsi::log_sense_cdb(0x0D)[..3], [0x4D, 0x00, 0x4D]);
    assert_eq!(scsi::vpd_cdb(0x83)[..3], [0x12, 0x01, 0x83]);
}

#[test]
fn firmware_upload_uses_the_generation_specific_layout() {
    let s2 = fw::upload_request(Generation::Sas2, fw::UPLOAD_TYPE_FW_FLASH, 0, 4096);
    assert_eq!((s2.sge_offset, s2.frame.len()), (9, 36));
    assert_eq!((s2.frame[0], s2.frame[3]), (0x01, 0x12));
    assert_eq!(&s2.frame[0x14..0x18], &[0, 0, 12, 0]);
    assert_eq!(s2.frame.u32_at(0x1C), 0);
    assert_eq!(s2.frame.u32_at(0x20), 4096);
    assert_eq!(s2.data_in_len, 4096);

    let s3 = fw::upload_request(Generation::Sas3, fw::UPLOAD_TYPE_BIOS_FLASH, 0, 4096);
    assert_eq!((s3.sge_offset, s3.frame.len()), (8, 32));
    assert_eq!(s3.frame[0], 0x02);
    assert_eq!(s3.frame.u32_at(0x18), 0);
    assert_eq!(s3.frame.u32_at(0x1C), 4096);
}

#[test]
fn firmware_upload_probes_the_size_first() {
    let mut mock = Mock::sas3();
    let mut reply = vec![0u8; 128];
    reply.put_u8(3, 0x12);
    reply.put_u32(0x14, 1024);
    mock.replies.insert(0x12, reply);
    let data = fw::upload(&mock, fw::UPLOAD_TYPE_FW_FLASH).unwrap();
    assert_eq!(data.len(), 1024);
    let sent = mock.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].data_in_len, 0);
    assert_eq!(sent[1].data_in_len, 1024);
    assert_eq!(sent[1].frame.u32_at(0x1C), 1024);
}

#[test]
fn image_header_is_validated() {
    let mut img = vec![0u8; 0x100];
    img.put_u32(0x00, 0xEA00_0010);
    img.put_u32(0x04, 0x5AFA_A55A);
    img.put_u32(0x08, 0xA55A_FAA5);
    img.put_u32(0x0C, 0x5AA5_5AFA);
    img.put_u32(0x14, 0x1400_0700);
    img.put_u16(0x20, 0x1000);
    img.put_u16(0x22, 0x2213);
    img.put_bytes(0x68, b"P20");
    let h = fw::parse_image_header(&img).unwrap();
    assert_eq!(h.format, fw::ImageFormat::Mpi2);
    assert_eq!(h.firmware_version, "20.00.07.00");
    assert_eq!((h.vendor_id, h.product_id), (0x1000, 0x2213));
    assert_eq!(h.version_name, "P20");
    img.put_u32(0x08, 0);
    assert!(fw::parse_image_header(&img).is_err());
    assert!(fw::parse_image_header(&[0u8; 16]).is_err());
}

#[test]
fn page_read_is_header_then_read() {
    let mut mock = Mock::sas3();
    let mut p = std_page(config::MANUFACTURING_0, 0x4C);
    p.put_bytes(0x04, b"SAS3008");
    p.put_bytes(0x1C, b"SAS9300-8i");
    mock.page(config::MANUFACTURING_0, 0, p);
    let page = config::read_page(&mock, config::MANUFACTURING_0, 0)
        .unwrap()
        .unwrap();
    let m = pages::Manufacturing0::parse(&page);
    assert_eq!(m.chip_name, "SAS3008");
    assert_eq!(m.board_name, "SAS9300-8i");
    let sent = mock.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].frame[0], config::ACTION_PAGE_HEADER);
    assert_eq!(sent[0].data_in_len, 0);
    assert_eq!(sent[1].frame[0], config::ACTION_READ_CURRENT);
    assert_eq!(sent[1].frame[0x15], 0x4C / 4);
    assert_eq!(sent[1].data_in_len, 0x4C);
    assert!(
        config::read_page(&mock, config::IO_UNIT_7, 0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn extended_page_read_copies_ext_length() {
    let mut mock = Mock::sas3();
    mock.page(
        config::SAS_DEVICE_0,
        0xFFFF,
        sas_device(9, 2, 0, SAS_DISK, 1),
    );
    config::read_page(&mock, config::SAS_DEVICE_0, 0xFFFF).unwrap();
    let sent = mock.sent();
    assert_eq!(sent[1].frame.u16_at(0x04), 14);
    assert_eq!(sent[1].frame[0x06], 0x12);
    assert_eq!(sent[1].frame[0x17], 0x0F);
    assert_eq!(sent[1].data_in_len, 56);
}

#[test]
fn get_next_walk_follows_handles_until_invalid_page() {
    let mut mock = Mock::sas3();
    mock.page(
        config::SAS_DEVICE_0,
        0xFFFF,
        sas_device(9, 1, 0, SAS_DISK, 1),
    );
    mock.page(config::SAS_DEVICE_0, 9, sas_device(10, 1, 1, SAS_DISK, 2));
    mock.page(config::SAS_DEVICE_0, 10, sas_device(12, 1, 2, SAS_DISK, 3));
    let pages = config::sas_devices(&mock).unwrap();
    let handles: Vec<u16> = pages.iter().map(|p| p.u16_at(0x18)).collect();
    assert_eq!(handles, vec![9, 10, 12]);
    let addresses: Vec<u32> = mock
        .sent()
        .iter()
        .filter(|s| s.frame[0] == config::ACTION_PAGE_HEADER)
        .map(|s| s.frame.u32_at(0x18))
        .collect();
    assert_eq!(addresses, vec![0xFFFF, 9, 10, 12]);
}

#[test]
fn ioc_facts_and_iocinfo_parse() {
    let mut r = vec![0u8; 68];
    r.put_u16(0x00, 0x0205);
    r.put_u16(0x04, 0x2300);
    r.put_u8(0x16, 1);
    r.put_u16(0x18, 9856);
    r.put_u16(0x1A, 0x2221);
    r.put_u32(0x1C, 0x0000_1808);
    r.put_u32(0x20, 0x1000_0100);
    r.put_u16(0x2A, 543);
    r.put_u8(0x37, 2);
    r.put_u16(0x38, 0x0200);
    let f = IocFacts::parse(&r);
    assert!(f.raid_support());
    assert_eq!(mpi::version_string(f.fw_version), "16.00.01.00");
    assert_eq!(
        (f.number_of_ports, f.request_credit, f.max_targets),
        (1, 9856, 543)
    );
    assert_eq!(f.mpi_version(), "205.2300");
    assert_eq!(
        mpi::capability_names(f.ioc_capabilities),
        vec!["integrated-raid", "tlr", "diag-trace-buffer"]
    );
    assert_eq!(mpi::bios_version_string(0x081D_0200), "8.29.02.00");

    let mut b = vec![0u8; 92];
    b.put_u32(0x0C, 6);
    b.put_u32(0x14, 0x97);
    b.put_u32(0x1C, 0x3020);
    b.put_u32(0x20, 0x1000);
    b.put_bytes(0x30, b"mpt3sas-54.100.00.00");
    b.put_u32(0x54, (3 << 8) | (1 << 5) | 2);
    b.put_u32(0x58, 1);
    let info = IocInfo::parse(&b);
    assert_eq!(info.pci_address(), "0001:03:02.1");
    assert_eq!(info.driver_version, "mpt3sas-54.100.00.00");
    assert_eq!(adapter::chip_name(info.pci_id), Some("SAS3008"));
    assert_eq!(adapter::chip_name(0x87), Some("SAS2308"));
    assert_eq!(adapter::chip_name(0x72), Some("SAS2008"));
    assert_eq!(adapter::adapter_type_name(info.adapter_type), "SAS3");
}

#[test]
fn enumeration_numbers_sas2_before_sas3_by_ioc_number() {
    let targets = adapter::order_targets(vec![
        host(5, "mpt3sas", Some(0)),
        host(2, "mpt2sas", Some(1)),
        host(9, "mpt3sas", None),
        host(3, "mpt2sas", Some(0)),
        host(7, "ahci", Some(0)),
        host(6, "mpt3sas", Some(4)),
    ]);
    let order: Vec<(usize, u32, u32)> = targets
        .iter()
        .map(|t| (t.index, t.host.host_no, t.ioc_number))
        .collect();
    assert_eq!(order, vec![(0, 3, 0), (1, 2, 1), (2, 5, 0), (3, 6, 4)]);
    assert_eq!(targets[1].generation, Generation::Sas2);
    assert_eq!(targets[2].generation, Generation::Sas3);
    assert_eq!(targets[2].generation.node(), "/dev/mpt3ctl");
    assert!(adapter::select(targets.clone(), 4).is_err());
    assert_eq!(adapter::select(targets, 2).unwrap().host.host_no, 5);
    assert!(adapter::select(Vec::new(), 0).is_err());
}

#[test]
fn list_uses_the_opener_for_every_adapter() {
    let targets = adapter::order_targets(vec![host(3, "mpt2sas", Some(0))]);
    let list = inventory::list_adapters(&targets, |_| {
        let mut m = Mock::sas3();
        let mut b = vec![0u8; 92];
        b.put_u32(0x14, 0x72);
        b.put_u32(0x28, 0x1400_0700);
        m.raw_answers.insert(super::transport::NR_IOCINFO, b);
        Ok(Box::new(m) as Box<dyn Transport>)
    })
    .unwrap();
    assert_eq!(list.adapters.len(), 1);
    assert_eq!(list.adapters[0].chip, "SAS2008");
    assert_eq!(list.adapters[0].generation, "SAS2");
    assert_eq!(list.adapters[0].firmware_version, "20.00.07.00");
    assert_eq!(list.adapters[0].vendor_id, 0x1000);
}

#[test]
fn sas_device_page_uses_corrected_connector_layout() {
    let p = sas_device(0x0B, 2, 5, SATA_DISK, 0x4433_2210_0500_0000);
    assert_eq!(p.len(), 56);
    let d = SasDevice0::parse(&p);
    assert_eq!((d.dev_handle, d.enclosure_handle, d.slot), (0x0B, 2, 5));
    assert_eq!(d.sas_address, 0x4433_2210_0500_0000);
    assert_eq!(d.connector_name, "C0.1");
    assert!(d.is_disk() && d.is_sata() && d.is_listed_device());
    assert_eq!(d.protocol(), "SATA");
    let sep = SasDevice0::parse(&sas_device(1, 2, 24, pages::DEVICE_INFO_SEP | SAS_DISK, 1));
    assert!(sep.is_sep() && !sep.is_disk());
    let mut hba = sas_device(1, 1, 0, 0x0000_0070 | 0x1, 1);
    hba.put_u16(0x14, 0);
    assert!(!SasDevice0::parse(&hba).is_listed_device());
}

#[test]
fn enclosure_and_phy_pages_parse() {
    let mut e = ext_page(config::SAS_ENCLOSURE_0, 40);
    e.put_u64(0x0C, 0x5003_0480_1b0e_2dbf);
    e.put_u16(0x14, 0x0021);
    e.put_u16(0x16, 2);
    e.put_u16(0x18, 29);
    e.put_u16(0x1E, 0x0A);
    let enc = Enclosure0::parse(&e);
    assert_eq!(inventory::wwn(enc.enclosure_logical_id), "500304801b0e2dbf");
    assert_eq!(
        (enc.enclosure_handle, enc.num_slots, enc.start_slot),
        (2, 29, 0)
    );
    assert_eq!(enc.management(), "IOC SES");
    assert!(enc.chassis_slot_valid());

    let mut io = ext_page(config::SAS_IO_UNIT_0, 0x10 + 2 * 20);
    io.put_u8(0x0C, 8);
    io.put_u8(0x10 + 3, 0x0B);
    io.put_u16(0x10 + 8, 0x0009);
    io.put_u8(0x10 + 20 + 2, 0x08);
    let phys = pages::parse_sas_io_unit_0(&io);
    assert_eq!(phys.len(), 2);
    assert_eq!(
        mpi::link_rate_name(phys[0].negotiated_link_rate),
        "12.0 Gb/s"
    );
    assert_eq!(phys[0].attached_dev_handle, 9);
    assert!(phys[1].disabled());

    let mut p1 = ext_page(config::SAS_PHY_1, 28);
    p1.put_u32(0x0C, 1);
    p1.put_u32(0x10, 2);
    p1.put_u32(0x14, 3);
    p1.put_u32(0x18, 4);
    let c = pages::SasPhy1::parse(&p1);
    assert_eq!(
        (
            c.invalid_dword_count,
            c.running_disparity_error_count,
            c.loss_dword_synch_count,
            c.phy_reset_problem_count
        ),
        (1, 2, 3, 4)
    );
}

#[test]
fn capacity_and_temperature_follow_the_mapping_table() {
    assert_eq!(scsi::size_mb(7_814_037_167, 512), 3_815_447);
    assert_eq!(scsi::size_mb(7_810_531_327, 512), 3_813_736);
    assert_eq!(format!("{:.2}", scsi::fahrenheit(34)), "93.20");
    assert_eq!(format!("{:.2}", scsi::fahrenheit(31)), "87.80");
    assert_eq!(
        format!("{:.2}", raid::percent(70_311_936, 68_250_624)),
        "2.93"
    );
    assert_eq!(raid::percent(0, 0), 0.0);
}

#[test]
fn scsi_parsers_decode_standard_pages() {
    let inq = scsi::parse_inquiry(&inquiry_data("SEAGATE", "ST4000NM0023", "0004"));
    assert_eq!(
        (
            inq.vendor.as_str(),
            inq.product.as_str(),
            inq.revision.as_str()
        ),
        ("SEAGATE", "ST4000NM0023", "0004")
    );
    let mut serial = vec![0x00, 0x80, 0x00, 0x08];
    serial.extend_from_slice(b"Z1Z3ABCD");
    assert_eq!(scsi::parse_serial_vpd(&serial).as_deref(), Some("Z1Z3ABCD"));
    let mut devid = vec![0x00, 0x83, 0x00, 0x18];
    devid.extend_from_slice(&[0x02, 0x01, 0x00, 0x08]);
    devid.extend_from_slice(b"SEAGATE ");
    devid.extend_from_slice(&[0x01, 0x03, 0x00, 0x08]);
    devid.extend_from_slice(&0x5000_c500_85e7_bd3f_u64.to_be_bytes());
    assert_eq!(
        scsi::parse_naa_vpd(&devid).as_deref(),
        Some("5000c50085e7bd3f")
    );
    assert_eq!(scsi::parse_naa_vpd(&[0, 0x83, 0, 0]), None);
    assert_eq!(
        scsi::parse_rotation_rate(&[0, 0xB1, 0, 0x3C, 0, 1]),
        Some(1)
    );
    let mut cap = vec![0u8; 32];
    cap[0..8].copy_from_slice(&7_814_037_167u64.to_be_bytes());
    cap[8..12].copy_from_slice(&512u32.to_be_bytes());
    assert_eq!(
        scsi::parse_read_capacity16(&cap),
        Some(scsi::Capacity {
            last_lba: 7_814_037_167,
            block_size: 512
        })
    );
    let log = [0x0D, 0, 0, 12, 0, 1, 3, 2, 0, 70, 0, 0, 3, 2, 0, 34];
    assert_eq!(scsi::parse_temperature_log(&log), Some(34));
    let unavailable = [0x0D, 0, 0, 6, 0, 0, 3, 2, 0, 0xFF];
    assert_eq!(scsi::parse_temperature_log(&unavailable), None);
    assert_eq!(
        scsi::sense_summary(&[0x70, 0, 0x05, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0x24, 0x00]),
        ", sense key 0x5 ASC 0x24 ASCQ 0x00"
    );
}

#[test]
fn drive_state_follows_the_state_table() {
    let pd = |state: u8, reason: u8, flags: u32| RaidPhysDisk0 {
        phys_disk_state: state,
        offline_reason: reason,
        phys_disk_status_flags: flags,
        ..Default::default()
    };
    assert_eq!(raid::drive_state(None, true), "Ready (RDY)");
    assert_eq!(raid::drive_state(None, false), "Standby (SBY)");
    assert_eq!(raid::drive_state(Some(&pd(7, 0, 0)), true), "Optimal (OPT)");
    assert_eq!(raid::drive_state(Some(&pd(3, 0, 0)), true), "Online (ONL)");
    assert_eq!(
        raid::drive_state(Some(&pd(4, 0, 0)), true),
        "Hot Spare (HSP)"
    );
    assert_eq!(
        raid::drive_state(Some(&pd(5, 0, 0)), true),
        "Degraded (DGD)"
    );
    assert_eq!(
        raid::drive_state(Some(&pd(6, 0, 0)), true),
        "Rebuilding (RBLD)"
    );
    assert_eq!(raid::drive_state(Some(&pd(2, 1, 0)), true), "Missing (MIS)");
    assert_eq!(raid::drive_state(Some(&pd(2, 3, 0)), true), "Failed (FLD)");
    assert_eq!(
        raid::drive_state(Some(&pd(1, 0, 0)), true),
        "Available (AVL)"
    );
    assert_eq!(
        raid::drive_state(Some(&pd(7, 0, 1)), true),
        "Out of Sync (OSY)"
    );
    assert_eq!(raid::volume_state_name(5), "Okay (OKY)");
    assert_eq!(raid::volume_type_name(2), "RAID1");
    assert_eq!(raid::current_operation(0x0008_0000), "Consistency Check");
    assert_eq!(raid::current_operation(0x0000_0001), "None");
}

#[test]
fn drive_show_combines_config_pages_and_passthrough() {
    let mut mock = Mock::sas3();
    add_sas_disk(&mut mock);
    let d = inventory::drive(&mock, "2:0".parse().unwrap()).unwrap();
    assert_eq!(d.state, "Ready (RDY)");
    assert_eq!(d.vendor.as_deref(), Some("SEAGATE"));
    assert_eq!(d.model.as_deref(), Some("ST4000NM0023"));
    assert_eq!(d.serial_number.as_deref(), Some("Z1Z3ABCD"));
    assert_eq!(d.guid.as_deref(), Some("5000c50085e7bd3f"));
    assert_eq!(d.size_mb, Some(3_815_447));
    assert_eq!(d.last_lba, Some(7_814_037_167));
    assert_eq!(d.drive_type.as_deref(), Some("SAS_HDD"));
    assert_eq!(d.sas_address, "5000c50085e7bd3d");
    assert_eq!(d.temperature.map(|t| t.celsius), Some(34));
    let mut text = String::new();
    d.render(&mut text);
    assert!(text.contains("34C (93.20F)"));
    assert!(inventory::drive(&mock, "2:7".parse().unwrap()).is_err());
    assert!(
        mock.sent()
            .iter()
            .filter(|s| s.frame[3] == 0x00)
            .all(|s| s.frame.u16_at(0) == 9)
    );
}

#[test]
fn hidden_raid_members_use_raid_passthrough_and_fall_back_to_ir_data() {
    let mut mock = Mock::sas3();
    let dev = sas_device(0x0A, 2, 1, SATA_DISK, 0x4433_2210_0500_0000);
    mock.page(config::SAS_DEVICE_0, 0xFFFF, dev);
    let mut pd = std_page(config::RAID_PHYS_DISK_0, 0x78);
    pd.put_u16(0x04, 0x0A);
    pd.put_u8(0x07, 3);
    pd.put_bytes(0x10, b"ATA     ");
    pd.put_bytes(0x18, b"Samsung SSD 860");
    pd.put_bytes(0x2C, b"S3Z9NB0K");
    pd.put_u8(0x50, raid::PD_STATE_OPTIMAL);
    pd.put_u8(0x53, 0x08 | 0x01);
    pd.put_u64(0x58, 976_773_167);
    pd.put_u16(0x70, 512);
    mock.page(
        config::RAID_PHYS_DISK_0,
        config::PHYSDISK_FORM_DEVHANDLE | 0x0A,
        pd,
    );
    let d = inventory::drive(&mock, "2:1".parse().unwrap()).unwrap();
    assert_eq!(d.state, "Optimal (OPT)");
    assert_eq!(d.phys_disk_num, Some(3));
    assert_eq!(d.vendor.as_deref(), Some("ATA"));
    assert_eq!(d.model.as_deref(), Some("Samsung SSD 860"));
    assert_eq!(d.drive_type.as_deref(), Some("SATA_SSD"));
    assert_eq!(d.size_mb, Some(476_940));
    assert_eq!(d.temperature, None);
    assert!(
        mock.sent()
            .iter()
            .filter(|s| s.frame[3] == 0x00 || s.frame[3] == 0x16)
            .all(|s| s.frame[3] == 0x16)
    );
}

fn add_volume(mock: &mut Mock) {
    let mut v0 = std_page(config::RAID_VOLUME_0, 0x28 + 8);
    v0.put_u16(0x04, 0x143);
    v0.put_u8(0x06, 0x05);
    v0.put_u8(0x07, 0x02);
    v0.put_u32(
        0x08,
        raid::STATUS_FLAG_ENABLED | raid::STATUS_FLAG_CONSISTENCY_CHECK,
    );
    v0.put_u64(0x10, 7_810_531_327);
    v0.put_u16(0x1C, 512);
    v0.put_u8(0x24, 2);
    v0.put_u8(0x28 + 2, 0);
    v0.put_u8(0x2C + 2, 1);
    mock.page(config::RAID_VOLUME_0, 0xFFFF, v0.clone());
    mock.page(
        config::RAID_VOLUME_0,
        config::RAID_VOLUME_FORM_HANDLE | 0x143,
        v0,
    );
    let mut v1 = std_page(config::RAID_VOLUME_1, 0x40);
    v1.put_u16(0x04, 0x143);
    v1.put_bytes(0x20, b"data");
    v1.put_u64(0x30, 0x0677_fd85_2dbb_1b9b);
    mock.page(
        config::RAID_VOLUME_1,
        config::RAID_VOLUME_FORM_HANDLE | 0x143,
        v1,
    );
    for (num, handle, slot) in [(0u8, 0x0Bu16, 0u16), (1, 0x0C, 1)] {
        let mut pd = std_page(config::RAID_PHYS_DISK_0, 0x78);
        pd.put_u16(0x04, handle);
        pd.put_u8(0x07, num);
        pd.put_u8(0x50, raid::PD_STATE_OPTIMAL);
        mock.page(
            config::RAID_PHYS_DISK_0,
            config::PHYSDISK_FORM_NUMBER | num as u32,
            pd.clone(),
        );
        mock.page(
            config::RAID_PHYS_DISK_0,
            config::PHYSDISK_FORM_DEVHANDLE | handle as u32,
            pd,
        );
        mock.page(
            config::SAS_DEVICE_0,
            config::SAS_DEVICE_FORM_HANDLE | handle as u32,
            sas_device(handle, 2, slot, SAS_DISK, 0x5000 + slot as u64),
        );
    }
    let mut b2 = std_page(config::BIOS_2, 0x70);
    b2.put_u8(0x54, pages::BOOT_FORM_SAS_WWID);
    b2.put_u64(0x58, 0x0677_fd85_2dbb_1b9b);
    mock.page(config::BIOS_2, 0, b2);
    let mut indicator = vec![0u8; 128];
    indicator.put_u8(3, 0x15);
    indicator.put_u64(0x14, 70_311_936);
    indicator.put_u64(0x1C, 68_250_624);
    mock.replies.insert(0x15, indicator);
}

#[test]
fn volume_show_resolves_members_boot_and_progress() {
    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    let v = inventory::volume(&mock, 0x143).unwrap();
    assert_eq!(v.id, 323);
    assert_eq!(v.raid_level, "RAID1");
    assert_eq!(v.state, "Okay (OKY)");
    assert_eq!(v.size_mb, 3_813_736);
    assert_eq!(v.wwid.as_deref(), Some("0677fd852dbb1b9b"));
    assert_eq!(v.name.as_deref(), Some("data"));
    assert_eq!(v.boot, Some("primary"));
    let members: Vec<_> = v
        .members
        .iter()
        .map(|m| m.address.clone().unwrap())
        .collect();
    assert_eq!(members, vec!["2:0", "2:1"]);
    assert_eq!(v.status.current_operation, "Consistency Check");
    let p = v.status.progress.clone().unwrap();
    assert_eq!(format!("{:.2}", p.percent_complete), "2.93");
    let indicator = mock
        .sent()
        .into_iter()
        .find(|s| s.frame[3] == 0x15)
        .unwrap();
    assert_eq!(indicator.frame[0], raid::ACTION_INDICATOR_STRUCT);
    assert_eq!(indicator.frame.u16_at(0x04), 0x143);
    assert_eq!(inventory::volumes(&mock).unwrap().volumes.len(), 1);
    assert!(inventory::volume(&mock, 7).is_err());
}

#[test]
fn bios_page_2_uses_corrected_offsets_and_boot_set_writes_nvram() {
    let mut p = std_page(config::BIOS_2, 0x70);
    p.put_u8(0x1C, pages::BOOT_FORM_ENCLOSURE_SLOT);
    p.put_u64(0x20, 0x5003_0480_1b0e_2dbf);
    p.put_u16(0x30, 4);
    p.put_u8(0x54, pages::BOOT_FORM_DEVICE_NAME);
    p.put_u64(0x58, 0xAB);
    let b = Bios2::parse(&p);
    assert_eq!(
        b.requested,
        BootDevice::EnclosureSlot {
            enclosure_logical_id: 0x5003_0480_1b0e_2dbf,
            slot: 4
        }
    );
    assert_eq!(b.requested_alternate, BootDevice::None);
    assert_eq!(b.current.identifier(), Some(0xAB));

    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    let mut e = ext_page(config::SAS_ENCLOSURE_0, 40);
    e.put_u64(0x0C, 0x5003_0480_1b0e_2dbf);
    e.put_u16(0x16, 2);
    mock.page(
        config::SAS_ENCLOSURE_0,
        config::ENCLOSURE_FORM_HANDLE | 2,
        e,
    );
    inventory::set_boot(&mock, true, BootTarget::Drive("2:4".parse().unwrap())).unwrap();
    let write = mock
        .sent()
        .into_iter()
        .find(|s| s.frame[3] == 0x04 && s.frame[0] == config::ACTION_WRITE_NVRAM)
        .unwrap();
    assert_eq!(write.frame[0x17], 0x02);
    assert_eq!(write.frame[0x16], 2);
    assert_eq!(write.data_out.len(), 0x70);
    assert_eq!(write.data_out[0x38], pages::BOOT_FORM_ENCLOSURE_SLOT);
    assert_eq!(write.data_out.u64_at(0x3C), 0x5003_0480_1b0e_2dbf);
    assert_eq!(write.data_out.u16_at(0x4C), 4);
    assert_eq!(write.data_out[0x54], pages::BOOT_FORM_SAS_WWID);

    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    inventory::set_boot(&mock, false, BootTarget::Volume(0x143)).unwrap();
    let write = mock
        .sent()
        .into_iter()
        .find(|s| s.frame[0] == config::ACTION_WRITE_NVRAM && s.frame[3] == 0x04)
        .unwrap();
    assert_eq!(write.data_out[0x1C], pages::BOOT_FORM_SAS_WWID);
    assert_eq!(write.data_out.u64_at(0x20), 0x0677_fd85_2dbb_1b9b);
}

#[test]
fn temperature_sensors_convert_units() {
    let mut mock = Mock::sas3();
    let mut p = std_page(config::IO_UNIT_7, 0x28);
    p.put_u16(0x10, 55);
    p.put_u8(0x12, 0x02);
    p.put_u16(0x14, 122);
    p.put_u8(0x16, 0x01);
    mock.page(config::IO_UNIT_7, 0, p);
    let t = inventory::temperature(&mock).unwrap();
    assert_eq!(t.sensors.len(), 2);
    assert_eq!((t.sensors[0].name, t.sensors[0].celsius), ("IOC", 55.0));
    assert_eq!((t.sensors[1].name, t.sensors[1].celsius), ("Board", 50.0));
    assert!(inventory::sensor("x", 10, 0).is_none());
}

#[test]
fn log_page_entries_skip_unused_slots() {
    let mut p = ext_page(config::LOG_0, 0x14 + 2 * 0x30);
    p.put_u16(0x10, 2);
    p.put_u64(0x14, 1234);
    p.put_u16(0x14 + 0x0C, 7);
    p.put_u16(0x14 + 0x0E, 0x0001);
    p.put_u8(0x14 + 0x14, 0xAB);
    let entries = pages::parse_log_0(&p);
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].time_stamp, entries[0].log_sequence), (1234, 7));
    assert!(entries[0].log_data.starts_with("ab00"));
    assert_eq!(entries[0].log_data.len(), pages::LOG_DATA_LENGTH * 2);
}

#[test]
fn diag_and_event_buffers_follow_the_ioctl_layouts() {
    let r = diag::register_buffer(BufferType::Snapshot, 0x10000, 0x4252_434D, 2);
    assert_eq!(r.len(), 120);
    assert_eq!(r[0x0D], 1);
    assert_eq!(r.u32_at(0x10), 2);
    assert_eq!(r.u32_at(0x70), 0x10000);
    assert_eq!(r.u32_at(0x74), 0x4252_434D);
    let rb = diag::read_buffer_request(7, 64, 128);
    assert_eq!(rb.len(), 0x1C + 128);
    assert_eq!(
        (rb.u32_at(0x10), rb.u32_at(0x14), rb.u32_at(0x18)),
        (64, 128, 7)
    );
    let en = diag::event_enable_buffer([1, 2, 3, 4]);
    assert_eq!((en.len(), en.u32_at(0x0C), en.u32_at(0x18)), (28, 1, 4));

    let mut report = diag::event_report_buffer();
    assert_eq!(report.u32_at(0x08) as usize, report.len());
    let entry = |i: usize| 12 + i * 200;
    report.put_u32(entry(0), 0x1E);
    report.put_u32(entry(0) + 4, 201);
    report.put_u8(entry(0) + 8, 0x43);
    report.put_u32(entry(1), 0x27);
    report.put_u32(entry(1) + 4, 1);
    let events = diag::parse_events(&report);
    assert_eq!(events.len(), 2);
    assert_eq!((events[0].context, events[0].name), (1, "TEMP_THRESHOLD"));
    assert_eq!(
        (events[1].name, events[1].data.as_str()),
        ("IR_VOLUME", "43")
    );

    let mut mock = Mock::sas3();
    let mut query = vec![0u8; 124];
    query.put_u16(0x0E, 0x0003);
    query.put_u32(0x70, 8);
    query.put_u32(0x78, 0x4252_434D);
    mock.raw_answers
        .insert(super::transport::NR_DIAGQUERY, query);
    let q = diag::query(&mock, BufferType::Trace).unwrap();
    assert!(q.app_owned && q.buffer_valid && !q.fw_buffer_access);
    let data = diag::read_all(&mock, q.unique_id, q.total_buffer_size).unwrap();
    assert_eq!(data.len(), 8);
    let calls = mock.raw_calls.borrow();
    assert_eq!(
        (calls[1].0, calls[1].1),
        (super::transport::NR_DIAGREADBUFFER, 32)
    );
    assert_eq!(calls[1].2.u32_at(0x18), 0x4252_434D);
}

#[test]
fn drive_address_parses_enclosure_and_slot() {
    let a: DriveAddress = "2:15".parse().unwrap();
    assert_eq!((a.enclosure, a.slot), (2, 15));
    assert_eq!(a.to_string(), "2:15");
    assert!("2".parse::<DriveAddress>().is_err());
    assert!("x:1".parse::<DriveAddress>().is_err());
}

const WRITES: &[&[&str]] = &[
    &["controller", "reset"],
    &["drive", "locate", "2:0", "--on"],
    &["drive", "locate", "2:0", "--off"],
    &["drive", "online", "2:0"],
    &["drive", "offline", "2:0"],
    &["volume", "activate", "323"],
    &["volume", "check", "323"],
    &["hotspare", "remove", "2:0"],
    &["phy", "reset", "1"],
    &["phy", "reset", "1", "--hard"],
    &["event", "enable"],
    &["boot", "set", "--drive", "2:0"],
    &["boot", "set", "--alternate", "--volume", "323"],
    &["diag", "register", "--type", "trace", "--size", "4096"],
    &["diag", "release", "--unique-id", "0x4252434d"],
    &["diag", "unregister", "--unique-id", "1"],
    &["volume", "create", "--level", "raid1", "2:0", "2:1"],
    &["volume", "delete", "323"],
    &["volume", "delete", "--all", "--zero-lba0"],
    &["hotspare", "add", "2:0"],
    &["hotspare", "add", "2:0", "--pool", "3"],
    &["log", "clear"],
    &["firmware", "flash", "target/test-fixtures/missing.bin"],
    &["bios", "flash", "target/test-fixtures/missing.rom"],
];

#[test]
fn every_state_change_is_refused_without_yes_before_sending_anything() {
    for args in WRITES {
        let mut mock = Mock::sas3();
        add_volume(&mut mock);
        add_sas_disk(&mut mock);
        let err = run(&mock, false, args).unwrap_err();
        assert!(err.to_string().contains("--yes"), "{args:?} gave {err}");
        assert_eq!(mock.writes(), 0, "{args:?} touched the controller");
    }
}

#[test]
fn read_commands_do_not_need_yes() {
    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    add_sas_disk(&mut mock);
    let out = run(&mock, false, &["drive", "list"]).unwrap();
    assert!(out.contains("2:0"));
    assert!(out.contains("SAS_HDD"));
    let out = run(&mock, false, &["volume", "status"]).unwrap();
    assert!(out.contains("Consistency Check"));
    assert!(out.contains("2.93%"));
}

#[test]
fn confirmed_locate_sends_exactly_one_sep_request() {
    let mock = Mock::sas3();
    let out = run(&mock, true, &["drive", "locate", "2:5", "--on"]).unwrap();
    assert!(out.contains("turned on"));
    let sent = mock.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].frame, mpi::sep_locate_request(2, 5, true).frame);
    assert_eq!(sent[0].sge_offset, 8);
}

#[test]
fn confirmed_hotspare_remove_checks_the_state() {
    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    mock.page(
        config::SAS_DEVICE_0,
        0xFFFF,
        sas_device(0x0B, 2, 0, SAS_DISK, 0x5000),
    );
    assert!(run(&mock, true, &["hotspare", "remove", "2:0"]).is_err());
    assert!(mock.sent().iter().all(|s| s.frame[3] != 0x15));
    let err = run(&mock, true, &["drive", "offline", "2:9"]).unwrap_err();
    assert!(err.to_string().contains("2:9"));
    run(&mock, true, &["drive", "offline", "2:0"]).unwrap();
    let action = mock
        .sent()
        .into_iter()
        .find(|s| s.frame[3] == 0x15)
        .unwrap();
    assert_eq!((action.frame[0], action.frame[0x06]), (0x0A, 0));
}

#[test]
fn failed_ioc_status_is_reported_with_log_info() {
    let mut mock = Mock::sas3();
    let mut reply = vec![0u8; 128];
    reply.put_u8(3, 0x1B);
    reply.put_u16(0x0E, 0x8007);
    reply.put_u32(0x10, 0x3112_0101);
    mock.replies.insert(0x1B, reply);
    let err = run(&mock, true, &["phy", "reset", "2"]).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("0x0007 (INVALID_FIELD)"), "{text}");
    assert!(text.contains("0x31120101"), "{text}");
}

#[test]
fn raid_volume_page_parses_members() {
    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    let p = config::read_page(&mock, config::RAID_VOLUME_0, 0xFFFF)
        .unwrap()
        .unwrap();
    let v = RaidVolume0::parse(&p);
    assert_eq!(v.members.len(), 2);
    assert_eq!(v.members[1].phys_disk_num, 1);
    assert_eq!(v.block_size, 512);
}

impl Mock {
    fn sas2() -> Self {
        Self {
            generation: Some(Generation::Sas2),
            ..Default::default()
        }
    }

    fn raid_actions(&self) -> Vec<Sent> {
        self.sent()
            .into_iter()
            .filter(|s| s.frame[3] == mpi::FUNCTION_RAID_ACTION)
            .collect()
    }

    fn downloads(&self) -> Vec<Sent> {
        self.sent()
            .into_iter()
            .filter(|s| s.frame[3] == mpi::FUNCTION_FW_DOWNLOAD)
            .collect()
    }
}

const MB: u64 = 2048;

fn ioc6_page() -> Vec<u8> {
    let mut p = std_page(config::IOC_6, 0x3C);
    p.put_u32(0x04, 0x1F);
    p.put_u8(0x08, 10);
    p.put_u8(0x09, 2);
    p.put_u8(0x0A, 10);
    p.put_u8(0x0B, 10);
    p.put_u8(0x0C, 2);
    p.put_u8(0x0D, 2);
    p.put_u8(0x0E, 3);
    p.put_u8(0x0F, 4);
    p.put_u8(0x14, 2);
    p.put_u8(0x15, 14);
    p.put_u8(0x16, 2);
    p.put_u32(0x1C, 0x0FF0);
    p.put_u32(0x20, 0x0080);
    p.put_u32(0x24, 0x0FF0);
    p
}

fn man4_page(flags: u32) -> Vec<u8> {
    let mut p = std_page(config::MANUFACTURING_4, 0x6C);
    p.put_u32(0x08, flags);
    p.put_u32(0x48, 0x0000_0002);
    p.put_u32(0x4C, 0x0000_0002);
    p.put_u32(0x50, 0x0001_0002);
    p.put_u32(0x54, 0x0000_0001);
    p.put_u8(0x65, 0x50);
    p.put_u16(0x66, 0x00A8);
    p.put_u8(0x69, 10);
    p.put_u8(0x6A, 14);
    p.put_u8(0x6B, 2);
    p
}

fn raid_config_page(counts: (u8, u8, u8), config_num: u8, elements: &[(u16, u16, u8)]) -> Vec<u8> {
    let mut p = ext_page(config::RAID_CONFIG_0, 0x30 + 8 * elements.len());
    p.put_u8(0x08, counts.2);
    p.put_u8(0x09, counts.1);
    p.put_u8(0x0A, counts.0);
    p.put_u8(0x0B, config_num);
    p.put_u8(0x2C, elements.len() as u8);
    for (i, (flags, vol, pd)) in elements.iter().enumerate() {
        let o = 0x30 + i * 8;
        p.put_u16(o, *flags);
        p.put_u16(o + 2, *vol);
        p.put_u8(o + 5, *pd);
    }
    p
}

fn bare_disks(mock: &mut Mock, disks: &[(u16, u32, u64, u8)]) {
    let mut prev = 0xFFFFu32;
    for (slot, (handle, info, mb, state)) in disks.iter().enumerate() {
        let dev = sas_device(*handle, 2, slot as u16, *info, 0x5000_0000 + slot as u64);
        mock.page(config::SAS_DEVICE_0, prev, dev.clone());
        mock.page(
            config::SAS_DEVICE_0,
            config::SAS_DEVICE_FORM_HANDLE | *handle as u32,
            dev,
        );
        prev = *handle as u32;
        let mut pd = std_page(config::RAID_PHYS_DISK_0, 0x78);
        pd.put_u16(0x04, *handle);
        pd.put_u8(0x50, *state);
        pd.put_u64(0x68, mb * MB - 1);
        pd.put_u16(0x70, 512);
        mock.page(
            config::RAID_PHYS_DISK_0,
            config::PHYSDISK_FORM_DEVHANDLE | *handle as u32,
            pd,
        );
    }
}

fn raid_mock(flags: u32) -> Mock {
    let mut mock = Mock::sas2();
    mock.page(config::IOC_6, 0, ioc6_page());
    mock.page(config::MANUFACTURING_4, 0, man4_page(flags));
    bare_disks(
        &mut mock,
        &[
            (0x20, SAS_DISK, 1_000_000, 0),
            (0x21, SAS_DISK, 1_200_000, 0),
            (0x22, SAS_DISK, 1_000_000, 0),
            (0x23, SAS_DISK, 1_000_000, 0),
            (0x24, SATA_DISK, 1_000_000, 0),
            (0x25, SAS_DISK, 1_000_000, raid::PD_STATE_ONLINE),
        ],
    );
    mock
}

#[test]
fn creation_struct_encodes_every_field() {
    let c = raid::VolumeCreation {
        volume_type: raid::VOL_TYPE_RAID1,
        flags: 0x8000_0005,
        settings: 0x0001_0002,
        resync_rate: 0x50,
        data_scrub_duration: 0x00A8,
        max_lba: 1_000_000 * MB - 1,
        stripe_blocks: 0,
        name: "data".into(),
        members: vec![0x20, 0x21],
    };
    let mut want = vec![
        0x02, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x80, 0x02, 0x00, 0x01, 0x00, 0x00, 0x50, 0xA8,
        0x00, 0xFF, 0xFF, 0x11, 0x7A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    want.extend_from_slice(b"data");
    want.extend_from_slice(&[0u8; 12]);
    want.extend_from_slice(&[0x00, 0x01, 0x20, 0x00, 0x00, 0x02, 0x21, 0x00]);
    assert_eq!(c.encode(), want);

    let r = raid::create_volume_request(&want);
    assert_eq!((r.sge_offset, r.frame.len()), (5, 20));
    assert_eq!((r.frame[0], r.frame[3]), (0x02, 0x15));
    assert_eq!(r.frame.u16_at(0x04), 0);
    assert_eq!(r.frame[0x06], 0);
    assert_eq!(r.frame.u32_at(0x10), 0);
    assert_eq!(r.data_out, want);

    let long = raid::VolumeCreation {
        volume_type: raid::VOL_TYPE_RAID10,
        name: "a-very-long-volume-name".into(),
        members: vec![1, 2, 3, 4],
        ..c
    };
    let b = long.encode();
    assert_eq!(b.len(), 0x2C + 16);
    assert_eq!(&b[0x1C..0x2B], b"a-very-long-vol");
    assert_eq!(b[0x2B], 0);
    let maps: Vec<u8> = (0..4).map(|i| b[0x2C + i * 4 + 1]).collect();
    assert_eq!(maps, vec![0, 1, 2, 3]);
}

#[test]
fn raid_action_words_for_delete_and_hot_spare() {
    let d = raid::delete_volume_request(0x143, false);
    assert_eq!((d.sge_offset, d.frame.len()), (8, 32));
    assert_eq!((d.frame[0], d.frame[3]), (0x03, 0x15));
    assert_eq!(d.frame.u16_at(0x04), 0x143);
    assert_eq!(d.frame.u32_at(0x10), 0);
    assert!(d.data_out.is_empty());
    let z = raid::delete_volume_request(0x143, true);
    assert_eq!(&z.frame[0x10..0x14], &[0x01, 0x00, 0x00, 0x00]);

    let h = raid::create_hot_spare_request(0x0009, 0);
    assert_eq!((h.sge_offset, h.frame.len()), (8, 32));
    assert_eq!((h.frame[0], h.frame[3]), (0x1D, 0x15));
    assert_eq!((h.frame.u16_at(0x04), h.frame[0x06]), (0, 0));
    assert_eq!(&h.frame[0x10..0x14], &[0x01, 0x00, 0x09, 0x00]);
    assert_eq!(raid::hot_spare_action_word(0x0009, 3), 0x0009_0008);
    assert_eq!(raid::hot_spare_action_word(0x1234, 7), 0x1234_0080);
}

#[test]
fn stripe_sizes_follow_the_ioc_page_6_map() {
    assert_eq!(ircfg::stripe_blocks(0x0FF0, None).unwrap(), 256);
    assert_eq!(ircfg::stripe_blocks(0x0FF0, Some(64)).unwrap(), 128);
    assert_eq!(ircfg::stripe_blocks(0x0FF0, Some(1024)).unwrap(), 2048);
    assert!(ircfg::stripe_blocks(0x0FF0, Some(2048)).is_err());
    assert!(ircfg::stripe_blocks(0x0FF0, Some(96)).is_err());
    assert_eq!(ircfg::stripe_blocks(0x0080, None).unwrap(), 128);
    assert_eq!(ircfg::stripe_blocks(0x0030, None).unwrap(), 32);
    assert_eq!(ircfg::stripe_blocks(0x3000, None).unwrap(), 4096);
    assert_eq!(ircfg::stripe_blocks(0, None).unwrap(), 0);
    assert!(ircfg::stripe_blocks(0, Some(64)).is_err());
}

#[test]
fn volume_create_sends_the_creation_struct_built_from_the_pages() {
    let mock = raid_mock(pages::MAN4_NO_MIX_SAS_SATA);
    let out = run(
        &mock,
        true,
        &[
            "volume", "create", "--level", "raid1", "2:0", "2:1", "--name", "data",
        ],
    )
    .unwrap();
    assert!(out.contains("1000000 MB"), "{out}");
    let actions = mock.raid_actions();
    assert_eq!(actions.len(), 1);
    let a = &actions[0];
    assert_eq!((a.sge_offset, a.frame.len()), (5, 20));
    assert_eq!((a.frame[0], a.frame.u32_at(0x10)), (0x02, 0));
    let mut want = vec![
        0x02, 0x02, 0x00, 0x00, 0x05, 0x00, 0x00, 0x80, 0x02, 0x00, 0x01, 0x00, 0x00, 0x50, 0xA8,
        0x00, 0xFF, 0xFF, 0x11, 0x7A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    want.extend_from_slice(b"data");
    want.extend_from_slice(&[0u8; 12]);
    want.extend_from_slice(&[0x00, 0x01, 0x20, 0x00, 0x00, 0x02, 0x21, 0x00]);
    assert_eq!(a.data_out, want);

    let mock = raid_mock(0);
    run(
        &mock,
        true,
        &[
            "volume", "create", "--level", "raid10", "2:0", "2:1", "2:2", "2:3", "--size", "1000",
            "--stripe", "256", "--pool", "2",
        ],
    )
    .unwrap();
    let d = &mock.raid_actions()[0].data_out;
    assert_eq!(d.len(), 0x2C + 16);
    assert_eq!((d[0], d[1]), (4, raid::VOL_TYPE_RAID10));
    assert_eq!(d.u32_at(0x04), 0x8000_0004);
    assert_eq!(d.u32_at(0x08), 0x0004_0001);
    assert_eq!(d[0x0A], 0x04);
    assert_eq!(d.u64_at(0x10), 1000 * MB - 1);
    assert_eq!(d.u32_at(0x18), 512);
    assert_eq!(&d[0x1C..0x2C], &[0u8; 16]);
    let members: Vec<(u8, u16)> = (0..4)
        .map(|i| (d[0x2C + i * 4 + 1], d.u16_at(0x2C + i * 4 + 2)))
        .collect();
    assert_eq!(members, vec![(0, 0x20), (1, 0x21), (2, 0x22), (3, 0x23)]);

    let mock = raid_mock(0);
    run(
        &mock,
        true,
        &["volume", "create", "--level", "raid0", "2:0", "2:1"],
    )
    .unwrap();
    let d = &mock.raid_actions()[0].data_out;
    assert_eq!(d.u32_at(0x04), 0x8000_0000);
    assert_eq!(d.u64_at(0x10), 2_000_000 * MB - 1);
    assert_eq!(d.u32_at(0x18), 256);
}

#[test]
fn volume_create_enforces_the_limits_before_sending() {
    let cases: &[(&[&str], &str)] = &[
        (&["--level", "raid1", "2:0", "2:1", "2:2"], "exactly 2"),
        (&["--level", "raid10", "2:0", "2:1", "2:2"], "needs 4 to 10"),
        (&["--level", "raid1e", "2:0", "2:1"], "needs 3 to 10"),
        (&["--level", "raid0", "2:0", "2:0"], "more than once"),
        (&["--level", "raid0", "2:0", "2:4"], "SAS and SATA"),
        (
            &["--level", "raid0", "2:0", "2:5"],
            "not an unconfigured disk",
        ),
        (&["--level", "raid0", "2:0", "2:9"], "2:9"),
        (
            &["--level", "raid1", "2:0", "2:1", "--size", "1000001"],
            "1 to 1000000",
        ),
        (
            &["--level", "raid1", "2:0", "2:1", "--stripe", "64"],
            "no stripe",
        ),
        (
            &["--level", "raid1e", "2:0", "2:1", "2:2", "--stripe", "128"],
            "not supported",
        ),
        (
            &[
                "--level",
                "raid0",
                "2:0",
                "2:1",
                "--name",
                "sixteen-chars-xx",
            ],
            "15",
        ),
        (&["--level", "raid0", "2:0", "2:1", "--pool", "8"], "pool"),
    ];
    for (args, want) in cases {
        let mock = raid_mock(pages::MAN4_NO_MIX_SAS_SATA);
        let mut argv = vec!["volume", "create"];
        argv.extend_from_slice(args);
        let err = run(&mock, true, &argv).unwrap_err().to_string();
        assert!(err.contains(want), "{args:?} gave {err}");
        assert!(
            mock.raid_actions().is_empty(),
            "{args:?} sent a RAID action"
        );
    }

    let mut mock = raid_mock(0);
    mock.page(
        config::RAID_CONFIG_0,
        config::RAID_CONFIG_FORM_ACTIVE,
        raid_config_page((2, 4, 0), 0, &[]),
    );
    let err = run(
        &mock,
        true,
        &["volume", "create", "--level", "raid0", "2:0", "2:1"],
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("at most 2 volumes"), "{err}");
    assert!(mock.raid_actions().is_empty());

    let mut mock = raid_mock(0);
    let mut ioc6 = ioc6_page();
    ioc6.put_u32(0x04, 0x0B);
    mock.page(config::IOC_6, 0, ioc6);
    let err = run(
        &mock,
        true,
        &[
            "volume", "create", "--level", "raid10", "2:0", "2:1", "2:2", "2:3",
        ],
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not support RAID10"), "{err}");
    assert!(mock.raid_actions().is_empty());
}

#[test]
fn volume_delete_sends_the_lba0_word() {
    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    run(&mock, true, &["volume", "delete", "323", "--zero-lba0"]).unwrap();
    let a = mock.raid_actions();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].sge_offset, a[0].frame.len()), (8, 32));
    assert_eq!((a[0].frame[0], a[0].frame.u16_at(0x04)), (0x03, 0x143));
    assert_eq!(a[0].frame.u32_at(0x10), 0x0000_0001);

    let mut mock = Mock::sas3();
    add_volume(&mut mock);
    run(&mock, true, &["volume", "delete", "323"]).unwrap();
    assert_eq!(mock.raid_actions()[0].frame.u32_at(0x10), 0);

    let err = run(&mock, true, &["volume", "delete", "7"]).unwrap_err();
    assert!(err.to_string().contains("no volume 7"));
    assert!(run(&mock, true, &["volume", "delete"]).is_err());
    assert!(run(&mock, true, &["volume", "delete", "7", "--all"]).is_err());
}

#[test]
fn volume_delete_all_removes_volumes_then_leftover_spares() {
    let mut mock = Mock::sas3();
    mock.page(
        config::RAID_CONFIG_0,
        config::RAID_CONFIG_FORM_ACTIVE,
        raid_config_page(
            (2, 5, 1),
            3,
            &[
                (0x0000, 0x143, 0),
                (0x0001, 0x143, 0),
                (0x0000, 0x144, 0),
                (0x0002, 0, 4),
            ],
        ),
    );
    mock.page(
        config::RAID_CONFIG_0,
        config::RAID_CONFIG_FORM_CONFIGNUM | 3,
        raid_config_page((0, 2, 2), 3, &[(0x0002, 0, 4), (0x0012, 0, 6)]),
    );
    let out = run(&mock, true, &["volume", "delete", "--all", "--zero-lba0"]).unwrap();
    assert!(out.contains("volume 323 deleted"), "{out}");
    let actions: Vec<(u8, u16, u8, u32)> = mock
        .raid_actions()
        .iter()
        .map(|s| {
            (
                s.frame[0],
                s.frame.u16_at(0x04),
                s.frame[0x06],
                s.frame.u32_at(0x10),
            )
        })
        .collect();
    assert_eq!(
        actions,
        vec![
            (0x03, 0x143, 0, 1),
            (0x03, 0x144, 0, 1),
            (0x1E, 0, 4, 0),
            (0x1E, 0, 6, 0),
        ]
    );

    let mut mock = Mock::sas3();
    mock.page(
        config::RAID_CONFIG_0,
        config::RAID_CONFIG_FORM_ACTIVE,
        raid_config_page((1, 2, 1), 1, &[(0x0000, 0x143, 0), (0x0002, 0, 4)]),
    );
    mock.page(
        config::RAID_CONFIG_0,
        config::RAID_CONFIG_FORM_CONFIGNUM | 1,
        raid_config_page((1, 2, 1), 1, &[(0x0000, 0x145, 0), (0x0002, 0, 4)]),
    );
    let err = run(&mock, true, &["volume", "delete", "--all"]).unwrap_err();
    assert!(err.to_string().contains("still configured"));
    assert!(mock.raid_actions().iter().all(|s| s.frame[0] != 0x1E));

    let mock = Mock::sas3();
    let err = run(&mock, true, &["volume", "delete", "--all"]).unwrap_err();
    assert!(err.to_string().contains("no active"));
    assert!(mock.raid_actions().is_empty());
}

#[test]
fn hotspare_add_sends_handle_and_pool_bitmap() {
    let mock = raid_mock(0);
    run(&mock, true, &["hotspare", "add", "2:2"]).unwrap();
    let a = mock.raid_actions();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].sge_offset, a[0].frame.len()), (8, 32));
    assert_eq!(a[0].frame[0], 0x1D);
    assert_eq!(&a[0].frame[0x10..0x14], &[0x01, 0x00, 0x22, 0x00]);

    let mock = raid_mock(0);
    run(&mock, true, &["hotspare", "add", "2:3", "--pool", "5"]).unwrap();
    assert_eq!(mock.raid_actions()[0].frame.u32_at(0x10), 0x0023_0020);

    let mock = raid_mock(0);
    assert!(run(&mock, true, &["hotspare", "add", "2:5"]).is_err());
    assert!(run(&mock, true, &["hotspare", "add", "2:0", "--pool", "8"]).is_err());

    let mut mock = raid_mock(0);
    mock.page(
        config::RAID_CONFIG_0,
        config::RAID_CONFIG_FORM_ACTIVE,
        raid_config_page((1, 4, 2), 0, &[]),
    );
    let err = run(&mock, true, &["hotspare", "add", "2:2"]).unwrap_err();
    assert!(err.to_string().contains("hot spares"), "{err}");
    assert!(mock.raid_actions().is_empty());
}

fn log_page(attrs: u8) -> Vec<u8> {
    let mut p = ext_page(config::LOG_0, 0x14 + 2 * 0x30);
    p[3] = 0x0F | attrs;
    p.put_u16(0x10, 2);
    p.put_u64(0x14, 1234);
    p.put_u16(0x14 + 0x0E, 0x0001);
    p.put_u64(0x14 + 0x30, 5678);
    p.put_u16(0x14 + 0x30 + 0x0E, 0x0002);
    p
}

#[test]
fn log_clear_writes_the_page_back_with_zero_entries() {
    for (attrs, action) in [
        (0x20, config::ACTION_WRITE_NVRAM),
        (0x30, config::ACTION_WRITE_NVRAM),
        (0x10, config::ACTION_WRITE_CURRENT),
        (0x00, config::ACTION_WRITE_CURRENT),
    ] {
        let mut mock = Mock::sas3();
        let page = log_page(attrs);
        mock.page(config::LOG_0, 0, page.clone());
        let out = run(&mock, true, &["log", "clear"]).unwrap();
        assert!(out.contains("2 log entries cleared"), "{out}");
        let writes: Vec<Sent> = mock
            .sent()
            .into_iter()
            .filter(|s| s.frame[0] != config::ACTION_PAGE_HEADER && !s.data_out.is_empty())
            .collect();
        assert_eq!(writes.len(), 1);
        let w = &writes[0];
        assert_eq!(w.frame[0], action, "attrs 0x{attrs:02x}");
        assert_eq!(w.sge_offset, 7);
        assert_eq!(w.frame[0x17], 0x0F | attrs);
        assert_eq!(w.frame[0x06], 0x14);
        assert_eq!(w.frame.u16_at(0x04), page.len() as u16 / 4);
        assert_eq!(w.frame.u32_at(0x18), 0);
        let mut want = page.clone();
        want.put_u16(0x10, 0);
        assert_eq!(w.data_out, want);
    }
}

fn fix_checksum(image: &mut [u8]) {
    image.put_u32(0x34, 0);
    let sum = flash::word_sum(image);
    image.put_u32(0x34, sum.wrapping_neg());
}

fn fw_image(product: u16, body: usize, board: &str, device: u16) -> Vec<u8> {
    let mut img = vec![0u8; body];
    for (i, b) in img.iter_mut().enumerate().skip(0x100) {
        *b = (i * 7 % 251) as u8;
    }
    img.put_u32(0x00, 0xEA00_0010);
    img.put_u32(0x04, 0x5AFA_A55A);
    img.put_u32(0x08, 0xA55A_FAA5);
    img.put_u32(0x0C, 0x5AA5_5AFA);
    img.put_u32(0x14, 0x1400_0700);
    img.put_u16(0x20, 0x1000);
    img.put_u16(0x22, product);
    img.put_u32(0x2C, body as u32);
    img.put_bytes(0x68, b"P20");

    let mut nvdata = vec![0u8; 0x40 + 64];
    nvdata.put_u8(0x00, 0x03);
    let len = nvdata.len() as u32;
    nvdata.put_u32(0x08, len);
    nvdata.put_u8(0x40 + 0x0C, 8);
    nvdata.put_bytes(0x40 + 32 + 0x0C, board.as_bytes());

    let mut devices = vec![0u8; 0x40 + 24];
    devices.put_u8(0x00, 0x07);
    let len = devices.len() as u32;
    devices.put_u32(0x08, len);
    devices.put_u8(0x40 + 2, 1);
    devices.put_u16(0x40 + 8, device);
    devices.put_u16(0x40 + 8 + 2, 0x1000);
    devices.put_u8(0x40 + 8 + 8, 0x00);
    devices.put_u8(0x40 + 8 + 9, 0x05);

    img.put_u32(0x30, body as u32);
    let next = (body + nvdata.len()) as u32;
    nvdata.put_u32(0x0C, next);
    img.extend_from_slice(&nvdata);
    img.extend_from_slice(&devices);
    fix_checksum(&mut img);
    img
}

fn flash_controller(mock: &mut Mock) {
    let mut facts = vec![0u8; 128];
    facts.put_u8(0x03, mpi::FUNCTION_IOC_FACTS);
    facts.put_u16(0x1A, 0x2213);
    mock.replies.insert(mpi::FUNCTION_IOC_FACTS, facts);
    let mut ioc0 = std_page(config::IOC_0, 0x1C);
    ioc0.put_u16(0x0C, 0x1000);
    ioc0.put_u16(0x0E, 0x0072);
    ioc0.put_u8(0x10, 0x03);
    mock.page(config::IOC_0, 0, ioc0);
    let mut m0 = std_page(config::MANUFACTURING_0, 0x4C);
    m0.put_bytes(0x1C, b"SAS9211-8i");
    mock.page(config::MANUFACTURING_0, 0, m0);
}

fn good_image() -> Vec<u8> {
    fw_image(0x2213, 0x9000, "SAS9211-8i", 0x0072)
}

#[test]
fn firmware_validation_runs_all_six_checks() {
    let ctl = flash::Controller {
        product_id: 0x2213,
        device_id: 0x0072,
        revision_id: 3,
        board_name: Some("SAS9211-8i".into()),
    };
    let img = good_image();
    assert_eq!(flash::word_sum(&img), 0);
    let h = flash::validate_firmware(&img, &ctl).unwrap();
    assert_eq!(h.product_id, 0x2213);

    let mut bad = img.clone();
    bad[0x200] ^= 1;
    let e = flash::validate_firmware(&bad, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("checksum"), "{e}");

    let mut vendor = img.clone();
    vendor.put_u16(0x20, 0x1001);
    fix_checksum(&mut vendor);
    let e = flash::validate_firmware(&vendor, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("vendor id is 0x1001"), "{e}");

    let product = fw_image(0x2214, 0x9000, "SAS9211-8i", 0x0072);
    let e = flash::validate_firmware(&product, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("product id is 0x2214"), "{e}");

    let mut oversize = img.clone();
    oversize.extend_from_slice(&[0u8; 4096]);
    let e = flash::validate_firmware(&oversize, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("headers describe"), "{e}");

    let mut chain = img.clone();
    let len = chain.len() as u32;
    chain.put_u32(0x30, len);
    fix_checksum(&mut chain);
    let e = flash::validate_firmware(&chain, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("outside the file"), "{e}");

    let board = fw_image(0x2213, 0x9000, "SAS9207-8i", 0x0072);
    let e = flash::validate_firmware(&board, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("SAS9207-8i"), "{e}");

    let device = fw_image(0x2213, 0x9000, "SAS9211-8i", 0x0087);
    let e = flash::validate_firmware(&device, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("supported device list"), "{e}");

    let old_rev = flash::Controller {
        revision_id: 6,
        ..ctl.clone()
    };
    assert!(flash::validate_firmware(&img, &old_rev).is_err());

    let mut empty_nvdata = img.clone();
    empty_nvdata.put_u32(0x9000 + 0x08, 0x40);
    empty_nvdata.put_u32(0x2C, 0x9000 + 64);
    fix_checksum(&mut empty_nvdata);
    let e = flash::validate_firmware(&empty_nvdata, &ctl)
        .unwrap_err()
        .to_string();
    assert!(e.contains("NVDATA"), "{e}");
}

#[test]
fn sas2_firmware_flash_sends_16k_chunks_then_verifies() {
    let img = good_image();
    assert_eq!(img.len(), 0x9000 + 128 + 88);
    let mut mock = Mock::sas2();
    flash_controller(&mut mock);
    mock.uploads
        .insert(flash::UPLOAD_TYPE_FW_BACKUP, img.clone());
    let out = flash::flash_firmware(&mock, "fw.bin", &img).unwrap();
    assert_eq!(out.requests, 3);
    assert!(out.verified);
    let chunks = mock.downloads();
    assert_eq!(chunks.len(), 3);
    let sizes = [0x4000usize, 0x4000, img.len() - 0x8000];
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!((c.sge_offset, c.frame.len()), (9, 36));
        assert_eq!((c.frame[0x00], c.frame[0x03]), (0x01, 0x09));
        assert_eq!(c.frame[0x07], if i == 2 { 0x01 } else { 0x00 });
        assert_eq!(c.frame.u32_at(0x0C), img.len() as u32);
        assert_eq!(c.frame.u32_at(0x10), 0);
        assert_eq!(&c.frame[0x14..0x18], &[0x00, 0x00, 12, 0x00]);
        assert_eq!(c.frame.u32_at(0x18), 0);
        assert_eq!(c.frame.u32_at(0x1C), (i * 0x4000) as u32);
        assert_eq!(c.frame.u32_at(0x20), sizes[i] as u32);
        assert_eq!(c.data_out, img[i * 0x4000..i * 0x4000 + sizes[i]]);
    }
    let uploads: Vec<Sent> = mock
        .sent()
        .into_iter()
        .filter(|s| s.frame[3] == mpi::FUNCTION_FW_UPLOAD)
        .collect();
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].frame[0], 0x05);
    assert_eq!(uploads[0].frame.u32_at(0x1C), 0);
    assert_eq!(uploads[0].frame.u32_at(0x20), 0x10000);
    let last_download = mock
        .sent()
        .iter()
        .rposition(|s| s.frame[3] == mpi::FUNCTION_FW_DOWNLOAD)
        .unwrap();
    let first_upload = mock
        .sent()
        .iter()
        .position(|s| s.frame[3] == mpi::FUNCTION_FW_UPLOAD)
        .unwrap();
    assert!(first_upload > last_download);
}

#[test]
fn verification_reads_back_in_64k_pieces_and_compares() {
    let big = fw_image(0x2213, 0x18000, "SAS9211-8i", 0x0072);
    let mut mock = Mock::sas2();
    mock.uploads
        .insert(flash::UPLOAD_TYPE_FW_BACKUP, big.clone());
    flash::verify_firmware(&mock, &big).unwrap();
    let offsets: Vec<u32> = mock.sent().iter().map(|s| s.frame.u32_at(0x1C)).collect();
    assert_eq!(offsets, vec![0, 0x10000]);

    let mut other = big.clone();
    other[0x12345] ^= 0xFF;
    let mut mock = Mock::sas2();
    mock.uploads.insert(flash::UPLOAD_TYPE_FW_BACKUP, other);
    let e = flash::verify_firmware(&mock, &big).unwrap_err().to_string();
    assert!(e.contains("0x10000"), "{e}");

    let mut mock = Mock::sas2();
    mock.uploads
        .insert(flash::UPLOAD_TYPE_FW_BACKUP, big[..0x10000].to_vec());
    assert!(flash::verify_firmware(&mock, &big).is_err());
}

#[test]
fn sas3_firmware_flash_is_one_request_with_the_mpi25_layout() {
    let img = good_image();
    let mut mock = Mock::sas3();
    flash_controller(&mut mock);
    mock.uploads
        .insert(flash::UPLOAD_TYPE_FW_BACKUP, img.clone());
    flash::flash_firmware(&mock, "fw.bin", &img).unwrap();
    let d = mock.downloads();
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].sge_offset, d[0].frame.len()), (8, 32));
    assert_eq!((d[0].frame[0x00], d[0].frame[0x07]), (0x01, 0x01));
    assert_eq!(d[0].frame.u32_at(0x0C), img.len() as u32);
    assert_eq!(d[0].frame.u32_at(0x14), 0);
    assert_eq!(d[0].frame.u32_at(0x18), 0);
    assert_eq!(d[0].frame.u32_at(0x1C), img.len() as u32);
    assert_eq!(d[0].data_out, img);
}

#[test]
fn rejected_firmware_sends_no_download() {
    let mut bad = good_image();
    bad[0x300] ^= 0x10;
    let wrong_product = fw_image(0x2214, 0x9000, "SAS9211-8i", 0x0072);
    let mut wrong_vendor = good_image();
    wrong_vendor.put_u16(0x20, 0x1028);
    fix_checksum(&mut wrong_vendor);
    let mut oversize = good_image();
    oversize.extend_from_slice(&[0u8; 0x4000]);
    for img in [bad, wrong_product, wrong_vendor, oversize] {
        let mut mock = Mock::sas2();
        flash_controller(&mut mock);
        assert!(flash::flash_firmware(&mock, "fw.bin", &img).is_err());
        assert!(mock.downloads().is_empty());
    }
}

#[test]
fn firmware_flash_reads_the_file_after_confirmation() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sas2-firmware.bin");
    let img = good_image();
    std::fs::write(&path, &img).unwrap();
    let file = path.to_str().unwrap();
    let mut mock = Mock::sas2();
    flash_controller(&mut mock);
    mock.uploads.insert(flash::UPLOAD_TYPE_FW_BACKUP, img);
    let out = run(&mock, true, &["firmware", "flash", file]).unwrap();
    assert!(out.contains("Verified"), "{out}");
    assert!(out.contains("20.00.07.00"), "{out}");
    assert_eq!(mock.downloads().len(), 3);
    let missing = dir.join("does-not-exist.bin");
    let mock = Mock::sas2();
    assert!(
        run(
            &mock,
            true,
            &["firmware", "flash", missing.to_str().unwrap()]
        )
        .is_err()
    );
    assert_eq!(mock.writes(), 0);
}

fn rom(code_type: u8, device: u16, blocks: u16, last: bool) -> Vec<u8> {
    let mut v = vec![0u8; blocks as usize * 512];
    for (i, b) in v.iter_mut().enumerate().skip(0x60) {
        *b = (i % 13) as u8;
    }
    v[0] = 0x55;
    v[1] = 0xAA;
    v.put_u16(0x18, 0x40);
    v.put_bytes(0x40, b"PCIR");
    v.put_u16(0x44, 0x1000);
    v.put_u16(0x46, device);
    v.put_u16(0x4A, 24);
    v.put_u16(0x50, blocks);
    v.put_u8(0x54, code_type);
    v.put_u8(0x55, if last { 0x80 } else { 0 });
    let n = v.len();
    v[n - 1] = 0;
    let sum = v.iter().fold(0u8, |s, b| s.wrapping_add(*b));
    v[n - 1] = sum.wrapping_neg();
    v
}

fn byte_sum(d: &[u8]) -> u8 {
    d.iter().fold(0u8, |s, b| s.wrapping_add(*b))
}

#[test]
fn bios_region_is_validated_ordered_and_fixed_up() {
    let efi = rom(3, 0x1234, 2, false);
    let x86 = rom(0, 0x0072, 3, true);
    let mut file = efi.clone();
    file.extend_from_slice(&x86);
    let (region, names) = flash::build_bios_region(&file, 0x0074).unwrap();
    assert_eq!(names, vec!["x86 BIOS", "EFI BIOS"]);
    assert_eq!(region.len(), 5 * 512);
    let (a, b) = region.split_at(3 * 512);
    assert_eq!(a.u16_at(0x46), 0x0074);
    assert_eq!(a[0x55] & 0x80, 0);
    assert_eq!(byte_sum(a), 0);
    assert_eq!(b.u16_at(0x46), 0x0074);
    assert_eq!(b[0x55] & 0x80, 0x80);
    assert_eq!(byte_sum(b), 0);

    let e = flash::build_bios_region(&x86, 0x0087)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not compatible"), "{e}");
    assert!(flash::build_bios_region(&efi, 0x0097).is_ok());
    let (same, _) = flash::build_bios_region(&x86, 0x0072).unwrap();
    assert_eq!(same, x86);

    let mut bad_sig = x86.clone();
    bad_sig[1] = 0xAB;
    assert!(flash::build_bios_region(&bad_sig, 0x0072).is_err());
    let mut blank = x86.clone();
    blank[1] = 0xBB;
    let e = flash::build_bios_region(&blank, 0x0072)
        .unwrap_err()
        .to_string();
    assert!(e.contains("blank"), "{e}");
    let mut bad_sum = x86.clone();
    bad_sum[0x100] ^= 1;
    let e = flash::build_bios_region(&bad_sum, 0x0072)
        .unwrap_err()
        .to_string();
    assert!(e.contains("checksum"), "{e}");
    let mut vendor = rom(0, 0x0072, 3, true);
    vendor.put_u16(0x44, 0x9005);
    let n = vendor.len();
    vendor[n - 1] = 0;
    let s = byte_sum(&vendor);
    vendor[n - 1] = s.wrapping_neg();
    let e = flash::build_bios_region(&vendor, 0x0072)
        .unwrap_err()
        .to_string();
    assert!(e.contains("vendor"), "{e}");
    let mut short = x86.clone();
    short.truncate(1024);
    assert!(flash::build_bios_region(&short, 0x0072).is_err());
    let mut twice = x86.clone();
    twice[0x55] = 0;
    let n = twice.len();
    twice[n - 1] = 0;
    let s = byte_sum(&twice);
    twice[n - 1] = s.wrapping_neg();
    let doubled = [twice.clone(), twice].concat();
    let e = flash::build_bios_region(&doubled, 0x0072)
        .unwrap_err()
        .to_string();
    assert!(e.contains("more than one"), "{e}");
}

#[test]
fn bios_flash_sends_the_fixed_up_region_as_type_2() {
    let x86 = rom(0, 0x0072, 64, true);
    let mut mock = Mock::sas2();
    flash_controller(&mut mock);
    let out = flash::flash_bios(&mock, "x.rom", &x86).unwrap();
    assert_eq!(out.requests, 2);
    let d = mock.downloads();
    assert_eq!(d.len(), 2);
    assert_eq!(d[0].frame[0], 0x02);
    assert_eq!((d[0].frame[0x07], d[1].frame[0x07]), (0x00, 0x01));
    assert_eq!(d[1].frame.u32_at(0x1C), 0x4000);
    assert_eq!(d[1].frame.u32_at(0x20), 0x4000);
    assert_eq!([d[0].data_out.clone(), d[1].data_out.clone()].concat(), x86);

    let mut mock = Mock::sas3();
    flash_controller(&mut mock);
    flash::flash_bios(&mock, "x.rom", &x86).unwrap();
    let d = mock.downloads();
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].frame[0], d[0].frame[0x07]), (0x02, 0x01));
    assert_eq!(d[0].frame.u32_at(0x1C), x86.len() as u32);

    let mut mock = Mock::sas2();
    flash_controller(&mut mock);
    let mut bad = x86.clone();
    bad[0x300] ^= 1;
    assert!(flash::flash_bios(&mock, "x.rom", &bad).is_err());
    assert!(mock.downloads().is_empty());
}
