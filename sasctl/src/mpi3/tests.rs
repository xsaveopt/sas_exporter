use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::adapter::{self, AdpInfo, OsView, Target};
use super::cli;
use super::config::{self, PageId};
use super::event;
use super::inventory::{self, DriveAddress};
use super::mpi::{self, IocFacts};
use super::nvme;
use super::pages::{self, Device0, DeviceForm, Enclosure0, Protocol};
use super::scsi;
use super::transport::{
    self, Aligned, BUF_DATA_IN, BUF_DATA_OUT, BUF_ERR_RESPONSE, BUF_MPI_REPLY, BUF_MPI_REQUEST,
    Entry, MPI_REPLY_LEN, Reply, Request, SgIo, Transport,
};
use crate::Ctx;
use crate::bytes::{Le, LeMut};
use crate::output::Format;
use crate::sysfs::ScsiHost;

#[derive(Clone, Debug)]
struct Call {
    packet: Vec<u8>,
    dout: Vec<u8>,
    din_len: usize,
}

impl Call {
    fn is_driver(&self) -> bool {
        self.packet[0] == transport::CMD_DRIVER
    }

    fn opcode(&self) -> u8 {
        self.packet[9]
    }

    fn entries(&self) -> Vec<Entry> {
        parse_entries(&self.packet)
    }

    fn frame(&self) -> Vec<u8> {
        let e = self.entries();
        self.dout[..e[0].len as usize].to_vec()
    }

    fn function(&self) -> Option<u8> {
        (!self.is_driver()).then(|| self.frame()[3])
    }
}

fn parse_entries(packet: &[u8]) -> Vec<Entry> {
    let n = packet.u8_at(0x10) as usize;
    (0..n)
        .map(|i| Entry {
            buf_type: packet.u8_at(0x18 + i * 8),
            len: packet.u32_at(0x18 + i * 8 + 4),
        })
        .collect()
}

struct Answer {
    data_in: Vec<u8>,
    reply_type: u8,
    reply: Vec<u8>,
    sense: Vec<u8>,
}

impl Answer {
    fn ok(data_in: Vec<u8>) -> Self {
        Self {
            data_in,
            reply_type: transport::REPLY_TYPE_STATUS,
            reply: vec![0u8; 16],
            sense: Vec::new(),
        }
    }

    fn address(function: u8, status: u16) -> Self {
        let mut reply = vec![0u8; 128];
        reply.put_u8(0x03, function);
        reply.put_u16(0x0A, status);
        Self {
            data_in: Vec::new(),
            reply_type: transport::REPLY_TYPE_ADDRESS,
            reply,
            sense: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Mock {
    id: u8,
    adpinfo: Vec<u8>,
    map: Vec<(u16, u16, u32, u8)>,
    facts: Vec<u8>,
    manifest: Option<Vec<u8>>,
    pages: HashMap<(u8, u8, u32), Vec<u8>>,
    scsi: HashMap<(u16, u8, u8), Vec<u8>>,
    nvme: HashMap<(u16, u8), Vec<u8>>,
    seq: Vec<u8>,
    pel: Vec<Vec<u8>>,
    calls: RefCell<Vec<Call>>,
}

impl Mock {
    fn page(&mut self, id: PageId, address: u32, body: Vec<u8>) {
        self.pages.insert((id.page_type, id.number, address), body);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    fn mpt_calls(&self, function: u8) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.function() == Some(function))
            .collect()
    }

    fn driver_calls(&self, opcode: u8) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.is_driver() && c.opcode() == opcode)
            .collect()
    }

    fn driver(&self, packet: &[u8], dout: &[u8], din: &mut [u8]) -> Result<()> {
        assert_eq!(packet.len(), transport::PACKET_LEN);
        assert_eq!(packet[8], self.id);
        match packet[9] {
            adapter::OPCODE_ADPINFO => {
                assert!(dout.is_empty() && din.len() >= adapter::ADPINFO_LEN);
                din[..self.adpinfo.len()].copy_from_slice(&self.adpinfo);
            }
            adapter::OPCODE_ALLTGTDEVINFO => {
                assert!(dout.is_empty() && din.len() >= 4);
                din.put_u16(0, self.map.len() as u16);
                if din.len() > 8 {
                    let fits = (din.len() - 8) / 12;
                    for (i, (h, pid, tid, bus)) in self.map.iter().take(fits).enumerate() {
                        let o = 8 + i * 12;
                        din.put_u16(o, *h);
                        din.put_u16(o + 2, *pid);
                        din.put_u32(o + 4, *tid);
                        din.put_u8(o + 8, *bus);
                        din.put_bytes(o + 9, &[0xFF, 0xFF, 0xFF]);
                    }
                }
            }
            adapter::OPCODE_ADPRESET => {
                assert!(din.is_empty());
                assert_eq!(dout.len(), 4);
            }
            op => anyhow::bail!("unexpected driver opcode {op}"),
        }
        Ok(())
    }

    fn config(&self, f: &[u8]) -> Answer {
        let (number, page_type, action, address) =
            (f.u8_at(0x0D), f.u8_at(0x0E), f.u8_at(0x0F), f.u32_at(0x10));
        match action {
            config::ACTION_PAGE_HEADER => {
                assert_eq!((address, f.u16_at(0x14), f.u8_at(0x0C)), (0, 0, 0));
                match self
                    .pages
                    .iter()
                    .find(|((t, n, _), _)| *t == page_type && *n == number)
                {
                    Some((_, p)) => Answer::ok(p[..8].to_vec()),
                    None => {
                        Answer::address(mpi::FUNCTION_CONFIG, mpi::IOCSTATUS_CONFIG_INVALID_PAGE)
                    }
                }
            }
            config::ACTION_READ_CURRENT => match self.pages.get(&(page_type, number, address)) {
                Some(p) => {
                    assert_eq!(f.u16_at(0x14) as usize * 4, p.len());
                    Answer::ok(p.clone())
                }
                None => Answer::address(mpi::FUNCTION_CONFIG, mpi::IOCSTATUS_CONFIG_INVALID_PAGE),
            },
            other => panic!("unexpected config action {other}"),
        }
    }

    fn scsi_io(&self, f: &[u8]) -> Answer {
        let handle = f.u16_at(0x0A);
        let cdb = &f[0x20..0x30];
        let sub = match cdb[0] {
            0x12 if cdb[1] & 1 == 1 => cdb[2],
            0x4D => cdb[2] & 0x3F,
            _ => 0,
        };
        match self.scsi.get(&(handle, cdb[0], sub)) {
            Some(d) => Answer::ok(d.clone()),
            None => {
                let mut a = Answer::address(mpi::FUNCTION_SCSI_IO, 0);
                a.reply.put_u8(0x10, 0x02);
                a.reply.put_u8(0x11, 0x00);
                a.reply.put_u32(0x18, 18);
                let mut sense = vec![0u8; 18];
                sense[0] = 0x70;
                sense[2] = 0x05;
                sense[12] = 0x24;
                a.sense = sense;
                a
            }
        }
    }

    fn nvme(&self, f: &[u8]) -> Answer {
        match self.nvme.get(&(f.u16_at(0x0A), f.u8_at(0x20))) {
            Some(d) => Answer::ok(d.clone()),
            None => {
                let mut a = Answer::address(mpi::FUNCTION_NVME_ENCAPSULATED, 0);
                a.reply.put_u16(0x1E, 0x0002 << 1);
                a
            }
        }
    }

    fn pel(&self, f: &[u8], data_in_len: usize) -> Answer {
        match f.u8_at(0x0A) {
            event::ACTION_GET_SEQNUM => Answer::ok(self.seq.clone()),
            event::ACTION_GET_LOG => {
                let start = f.u32_at(0x0C);
                let room = (data_in_len - 8) / 128;
                let batch: Vec<&Vec<u8>> = self
                    .pel
                    .iter()
                    .filter(|e| e.u32_at(0x08) >= start)
                    .take(room)
                    .collect();
                if batch.is_empty() {
                    let mut a = Answer::address(mpi::FUNCTION_PERSISTENT_EVENT_LOG, 0);
                    a.reply.put_u16(0x14, event::STATUS_NOT_FOUND);
                    return a;
                }
                let mut d = vec![0u8; 8];
                d.put_u32(0, batch.len() as u32);
                for e in batch {
                    d.extend_from_slice(e);
                }
                Answer::ok(d)
            }
            other => panic!("unexpected PEL action {other}"),
        }
    }

    fn mpt(&self, packet: &[u8], dout: &[u8], din: &mut [u8]) -> Result<()> {
        let entries = parse_entries(packet);
        assert!(packet.len() >= 0x18 + 8 * entries.len());
        assert_eq!(packet[8], self.id);
        assert!(packet.u16_at(0x0A) >= 60);
        let out: usize = entries
            .iter()
            .filter(|e| matches!(e.buf_type, BUF_MPI_REQUEST | BUF_DATA_OUT))
            .map(|e| e.len as usize)
            .sum();
        assert_eq!(out, dout.len());
        assert_eq!(entries[0].buf_type, BUF_MPI_REQUEST);
        let frame = &dout[..entries[0].len as usize];
        let mut din_at = HashMap::new();
        let mut o = 0;
        for e in &entries {
            if matches!(e.buf_type, BUF_DATA_IN | BUF_MPI_REPLY | BUF_ERR_RESPONSE) {
                din_at.insert(e.buf_type, (o, e.len as usize));
                o += e.len as usize;
            }
        }
        assert!(din.len() >= o);
        let data_in_len = din_at.get(&BUF_DATA_IN).map_or(0, |x| x.1);
        let answer = match frame[3] {
            mpi::FUNCTION_IOC_FACTS => {
                assert_eq!(frame.len(), 0x10);
                Answer::ok(self.facts.clone())
            }
            mpi::FUNCTION_CONFIG => {
                assert_eq!(frame.len(), 0x20);
                self.config(frame)
            }
            mpi::FUNCTION_SCSI_IO => {
                assert_eq!(frame.len(), 0x40);
                assert!(din_at.contains_key(&BUF_ERR_RESPONSE));
                self.scsi_io(frame)
            }
            mpi::FUNCTION_NVME_ENCAPSULATED => {
                assert_eq!(frame.len(), 0x60);
                self.nvme(frame)
            }
            mpi::FUNCTION_CI_UPLOAD => match &self.manifest {
                Some(m) => Answer::ok(m.clone()),
                None => Answer::address(mpi::FUNCTION_CI_UPLOAD, 0x00B0),
            },
            mpi::FUNCTION_PERSISTENT_EVENT_LOG => self.pel(frame, data_in_len),
            mpi::FUNCTION_IO_UNIT_CONTROL => {
                assert_eq!(frame.len(), 0x40);
                assert_eq!(entries.len(), 2);
                Answer::ok(Vec::new())
            }
            f => Answer::address(f, 0x0001),
        };
        if let Some((at, len)) = din_at.get(&BUF_DATA_IN) {
            let n = answer.data_in.len().min(*len);
            din[*at..*at + n].copy_from_slice(&answer.data_in[..n]);
        }
        let (at, len) = din_at[&BUF_MPI_REPLY];
        din[at] = answer.reply_type;
        let n = answer.reply.len().min(len - 4);
        din[at + 4..at + 4 + n].copy_from_slice(&answer.reply[..n]);
        if let Some((at, len)) = din_at.get(&BUF_ERR_RESPONSE) {
            let n = answer.sense.len().min(*len);
            din[*at..*at + n].copy_from_slice(&answer.sense[..n]);
        }
        Ok(())
    }
}

impl Transport for Mock {
    fn mrioc_id(&self) -> u8 {
        self.id
    }

    fn submit(&self, packet: &[u8], dout: &[u8], din: &mut [u8]) -> Result<()> {
        self.calls.borrow_mut().push(Call {
            packet: packet.to_vec(),
            dout: dout.to_vec(),
            din_len: din.len(),
        });
        match packet[0] {
            transport::CMD_DRIVER => self.driver(packet, dout, din),
            transport::CMD_MPT => self.mpt(packet, dout, din),
            other => anyhow::bail!("bad cmd_type {other}"),
        }
    }
}

fn page(id: PageId, len: usize) -> Vec<u8> {
    let mut p = vec![0u8; len];
    p[2] = id.number;
    p.put_u16(4, (len / 4) as u16);
    p[6] = id.page_type;
    p
}

fn adpinfo(state: u8) -> Vec<u8> {
    let mut b = vec![0u8; 168];
    b.put_u32(0x00, 1);
    b.put_u32(0x08, 0x00A5);
    b.put_u32(0x0C, 0x01);
    b.put_u32(0x10, 0x4000);
    b.put_u32(0x14, 0x1000);
    b.put_u8(0x18, 3 | (1 << 5));
    b.put_u8(0x19, 0x41);
    b.put_u32(0x1C, 1);
    b.put_u32(0x20, 6);
    b.put_u8(0x24, state);
    b.put_bytes(0x30 + 0x04, b"Broadcom");
    b.put_bytes(0x30 + 0x2C, b"mpi3mr");
    b.put_bytes(0x30 + 0x40, b"8.17.0.3.50");
    b
}

fn facts() -> Vec<u8> {
    let mut b = vec![0u8; 0x70];
    b.put_u16(0x00, 0x70);
    b.put_u32(0x04, 0x0003_2700);
    b.put_bytes(0x08, &[0x2A, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x08]);
    b.put_u32(0x10, mpi::CAPABILITY_RAID_SUPPORTED | 0x0100);
    b.put_u16(0x18, 8192);
    b.put_u16(0x1A, 0x2B21);
    b.put_u16(0x1E, 32);
    b.put_u8(0x27, 0x10 | 0x08 | 0x04 | 0x02);
    b.put_u16(0x2E, 64);
    b.put_u16(0x36, 32);
    b.put_u16(0x3A, 240);
    b.put_u16(0x40, 240);
    b.put_u32(0x44, 0x0000_0002);
    b
}

fn manifest() -> Vec<u8> {
    let mut m = vec![0u8; 0xB0];
    m.put_u8(0x11, 0x50);
    m.put_u16(0x20, 0x1000);
    m.put_u16(0x22, 0x00A5);
    m.put_u16(0x24, 0x1000);
    m.put_u16(0x26, 0x4000);
    m.put_bytes(0x38, &[0x10, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x08]);
    m
}

const SSP_END: u16 = pages::SAS_DEVICE_INFO_SSP_TARGET | pages::SAS_DEVICE_TYPE_END_DEVICE;
const SATA_END: u16 = pages::SAS_DEVICE_INFO_STP_SATA_TARGET | pages::SAS_DEVICE_TYPE_END_DEVICE;

fn device0(handle: u16, enclosure: u16, slot: u16, pid: u16, form: u8) -> Vec<u8> {
    let mut p = page(config::DEVICE_0, 64);
    p.put_u16(0x08, handle);
    p.put_u16(0x0A, 1);
    p.put_u16(0x0C, slot);
    p.put_u16(0x0E, enclosure);
    p.put_u64(0x10, 0x5000_0000_0000_0000 | handle as u64);
    p.put_u16(0x18, pid);
    p.put_u8(0x27, form);
    p
}

fn sas_device(handle: u16, enclosure: u16, slot: u16, info: u16) -> Vec<u8> {
    let mut p = device0(
        handle,
        enclosure,
        slot,
        handle + 100,
        pages::DEVICE_FORM_SAS_SATA,
    );
    p.put_u64(0x28, 0x5000_c500_0000_0000 | handle as u64);
    p.put_u16(0x28 + 0x0A, info);
    p.put_u8(0x28 + 0x0C, 4);
    p.put_u8(0x28 + 0x13, 0xBB);
    p
}

fn nvme_device(handle: u16, enclosure: u16, slot: u16) -> Vec<u8> {
    let mut p = device0(
        handle,
        enclosure,
        slot,
        handle + 100,
        pages::DEVICE_FORM_PCIE,
    );
    p.put_u8(0x28 + 2, 4);
    p.put_u8(0x28 + 3, 0x05);
    p.put_u16(0x28 + 6, pages::PCIE_DEVICE_TYPE_NVME);
    p.put_u8(0x28 + 0x13, 12);
    p
}

fn vd_device(handle: u16, pid: u16, state: u8, level: u8) -> Vec<u8> {
    let mut p = device0(handle, 0, 0xFFFF, pid, pages::DEVICE_FORM_VD);
    p.put_u8(0x28, state);
    p.put_u8(0x29, level);
    p.put_u16(0x28 + 2, 0x0010 | 0x0001);
    p.put_u16(0x28 + 4, 0x0001);
    p.put_u16(0x28 + 6, 3);
    p.put_u16(0x28 + 8, 16);
    p.put_u16(0x28 + 0x0A, 64);
    p.put_u8(0x28 + 0x0C, 30);
    p
}

fn enclosure0(handle: u16, sep: u16, slots: u16) -> Vec<u8> {
    let mut p = page(config::ENCLOSURE_0, 32);
    p.put_u64(0x08, 0x500a_0980_0000_0000 | handle as u64);
    p.put_u16(0x10, 0x4000 | 0x0010 | 0x0002);
    p.put_u16(0x12, handle);
    p.put_u16(0x14, slots);
    p.put_u16(0x1A, sep);
    p
}

fn inquiry_data(vendor: &str, product: &str, rev: &str) -> Vec<u8> {
    let mut d = vec![b' '; 36];
    d[0..8].fill(0);
    d[8..8 + vendor.len()].copy_from_slice(vendor.as_bytes());
    d[16..16 + product.len()].copy_from_slice(product.as_bytes());
    d[32..32 + rev.len()].copy_from_slice(rev.as_bytes());
    d
}

fn temperature_log(celsius: u8) -> Vec<u8> {
    vec![
        0x0D, 0x00, 0x00, 0x0C, 0x00, 0x00, 0x03, 0x02, 0x00, celsius, 0x00, 0x01, 0x03, 0x02,
        0x00, 60,
    ]
}

fn identify(serial: &str, model: &str, fw: &str) -> Vec<u8> {
    let mut d = vec![b' '; 4096];
    d[0..4].fill(0);
    d[4..4 + serial.len()].copy_from_slice(serial.as_bytes());
    d[24..24 + model.len()].copy_from_slice(model.as_bytes());
    d[64..64 + fw.len()].copy_from_slice(fw.as_bytes());
    d
}

fn smart_log(kelvin: u16, warning: u8) -> Vec<u8> {
    let mut d = vec![0u8; 512];
    d[0] = warning;
    d.put_u16(1, kelvin);
    d[3] = 100;
    d[4] = 10;
    d[5] = 3;
    d.put_u64(112, 42);
    d.put_u64(128, 12000);
    d.put_u64(144, 5);
    d.put_u64(160, 0);
    d.put_u64(176, 7);
    d
}

fn pel_entry(seq: u32, code: u16, class: u8, locale: u16) -> Vec<u8> {
    let mut e = vec![0u8; 128];
    e.put_u64(0x00, 0x1_0000 + seq as u64);
    e.put_u32(0x08, seq);
    e.put_u16(0x0C, code);
    e.put_u16(0x10, locale);
    e.put_u8(0x12, class);
    e.put_u32(0x20, 0xDEAD_BEEF);
    e
}

fn base_mock() -> Mock {
    Mock {
        id: 2,
        adpinfo: adpinfo(1),
        facts: facts(),
        manifest: Some(manifest()),
        ..Default::default()
    }
}

fn rich_mock() -> Mock {
    let mut m = base_mock();
    m.map = vec![
        (0x0001, 1, u32::MAX, 0xFF),
        (0x0009, 109, 0, 0),
        (0x000A, 110, 1, 0),
        (0x000B, 111, 2, 1),
        (0x000C, 112, u32::MAX, 0xFF),
        (0x0020, 0, 0, 1),
        (0x0021, 1, u32::MAX, 0xFF),
    ];
    let mut expander = sas_device(0x0001, 0, 0, pages::SAS_DEVICE_TYPE_EXPANDER);
    expander.put_u16(0x0E, 0);
    m.page(config::DEVICE_0, 0x2000_0001, expander);
    m.page(
        config::DEVICE_0,
        0x2000_0009,
        sas_device(0x0009, 2, 0, SSP_END),
    );
    m.page(
        config::DEVICE_0,
        0x2000_000A,
        sas_device(0x000A, 2, 1, SATA_END),
    );
    m.page(config::DEVICE_0, 0x2000_000B, nvme_device(0x000B, 3, 0));
    m.page(
        config::DEVICE_0,
        0x2000_000C,
        sas_device(0x000C, 2, 24, SSP_END),
    );
    m.page(config::DEVICE_0, 0x2000_0020, vd_device(0x0020, 0, 3, 5));
    m.page(config::DEVICE_0, 0x2000_0021, vd_device(0x0021, 1, 0, 1));
    m.page(config::ENCLOSURE_0, 0x1000_0002, enclosure0(2, 0x000C, 24));
    m.page(config::ENCLOSURE_0, 0x1000_0003, {
        let mut e = enclosure0(3, 0, 8);
        e.put_u16(0x10, 0x8000 | 0x0020);
        e.put_u8(0x1C, 5);
        e
    });
    m.scsi.insert(
        (9, 0x12, 0),
        inquiry_data("SEAGATE", "ST4000NM0023", "0004"),
    );
    let mut serial = vec![0x00, 0x80, 0x00, 0x08];
    serial.extend_from_slice(b"Z1Z3ABCD");
    m.scsi.insert((9, 0x12, 0x80), serial);
    let mut devid = vec![0x00, 0x83, 0x00, 0x0C, 0x01, 0x03, 0x00, 0x08];
    devid.extend_from_slice(&0x5000_c500_85e7_bd3f_u64.to_be_bytes());
    m.scsi.insert((9, 0x12, 0x83), devid);
    let mut cap = vec![0u8; 32];
    cap[0..8].copy_from_slice(&7_814_037_167u64.to_be_bytes());
    cap[8..12].copy_from_slice(&512u32.to_be_bytes());
    m.scsi.insert((9, 0x9E, 0), cap.clone());
    m.scsi
        .insert((9, 0x12, 0xB1), vec![0x00, 0xB1, 0x00, 0x3C, 0x1C, 0x20]);
    m.scsi.insert((9, 0x4D, 0x0D), temperature_log(34));
    m.scsi.insert(
        (9, 0x4D, 0x2F),
        vec![
            0x2F, 0x00, 0x00, 0x08, 0x00, 0x00, 0x03, 0x04, 0x00, 0x00, 33, 0x00,
        ],
    );
    m.scsi.insert(
        (0x0A, 0x12, 0),
        inquiry_data("ATA", "Samsung SSD 870", "SVT0"),
    );
    m.scsi
        .insert((0x0A, 0x12, 0xB1), vec![0x00, 0xB1, 0x00, 0x3C, 0x00, 0x01]);
    m.scsi.insert(
        (0x0C, 0x12, 0),
        inquiry_data("BROADCOM", "VirtualSES", "03"),
    );
    m.scsi.insert((0x20, 0x9E, 0), cap);
    m.nvme.insert(
        (0x0B, nvme::OPCODE_IDENTIFY),
        identify("S5XYZ", "SAMSUNG MZWLR3T8HBLS", "MPK7525Q"),
    );
    m.nvme
        .insert((0x0B, nvme::OPCODE_GET_LOG_PAGE), smart_log(310, 0));
    m
}

fn ctx(yes: bool, sysfs: PathBuf) -> Ctx {
    Ctx {
        format: Format::Text,
        yes,
        interactive: false,
        sysfs,
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
        id: 2,
        index: 2,
        host: host(7, "mpi3mr", Some(2)),
    }
}

fn run_in(mock: &Mock, yes: bool, sysfs: &Path, args: &[&str]) -> Result<String> {
    let command = crate::cli::try_parse(args)?;
    let out = cli::execute(&command, &ctx(yes, sysfs.to_path_buf()), &target(), mock)?;
    Ok(out.text())
}

fn run(mock: &Mock, yes: bool, args: &[&str]) -> Result<String> {
    run_in(mock, yes, Path::new("/nonexistent"), args)
}

fn fixture(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn sg_io_v4_header_matches_the_uapi_layout() {
    let h = transport::sg_io_v4(&SgIo {
        request: 0x1111,
        request_len: 0x40,
        response: 0x2222,
        max_response_len: 96,
        dout: 0x3333,
        dout_len: 0x50,
        din: 0x4444,
        din_len: 0x84,
        timeout_ms: 5000,
    });
    assert_eq!(h.len(), 0xA0);
    assert_eq!(h.u32_at(0x00), 0x51);
    assert_eq!(h.u32_at(0x04), 0);
    assert_eq!(h.u32_at(0x08), 2);
    assert_eq!(h.u32_at(0x0C), 0x40);
    assert_eq!(h.u64_at(0x10), 0x1111);
    assert_eq!(h.u32_at(0x2C), 96);
    assert_eq!(h.u64_at(0x30), 0x2222);
    assert_eq!(h.u32_at(0x38), 0);
    assert_eq!(h.u32_at(0x3C), 0x50);
    assert_eq!(h.u32_at(0x40), 0);
    assert_eq!(h.u32_at(0x44), 0x84);
    assert_eq!(h.u64_at(0x48), 0x3333);
    assert_eq!(h.u64_at(0x50), 0x4444);
    assert_eq!(h.u32_at(0x58), 5000);
    assert_eq!(h.u32_at(0x5C), 0);
    assert_eq!(transport::SG_IO, 0x2285);
    assert_eq!(
        transport::node_path(3),
        PathBuf::from("/dev/bsg/mpi3mrctl3")
    );
}

#[test]
fn aligned_buffers_start_on_a_512_byte_boundary() {
    for len in [0usize, 1, 4, 168, 4096] {
        let mut a = Aligned::zeroed(len);
        assert_eq!(a.as_slice().len(), len);
        if len == 0 {
            assert_eq!(a.addr(), 0);
        } else {
            assert_eq!(a.addr() % 512, 0);
        }
    }
    let a = Aligned::from_slice(&[1, 2, 3]);
    assert_eq!(a.as_slice(), &[1, 2, 3]);
}

#[test]
fn driver_packet_carries_type_id_and_opcode() {
    let p = transport::driver_packet(7, adapter::OPCODE_ALLTGTDEVINFO);
    assert_eq!(p.len(), 32);
    assert_eq!(p[0], 1);
    assert_eq!(p[8], 7);
    assert_eq!(p[9], 4);
    assert!(p[1..8].iter().chain(&p[10..]).all(|b| *b == 0));
}

#[test]
fn mpt_packet_lists_entries_after_the_header() {
    let entries = [
        Entry {
            buf_type: BUF_MPI_REQUEST,
            len: 0x20,
        },
        Entry {
            buf_type: BUF_DATA_IN,
            len: 8,
        },
        Entry {
            buf_type: BUF_MPI_REPLY,
            len: 132,
        },
    ];
    let p = transport::mpt_packet(4, 90, &entries);
    assert_eq!(p.len(), 0x18 + 3 * 8);
    assert_eq!((p[0], p[8], p.u16_at(0x0A), p[0x10]), (2, 4, 90, 3));
    assert_eq!((p[0x18], p.u32_at(0x1C)), (0xFE, 0x20));
    assert_eq!((p[0x20], p.u32_at(0x24)), (3, 8));
    assert_eq!((p[0x28], p.u32_at(0x2C)), (5, 132));
    assert_eq!(parse_entries(&p), entries);
    let one = transport::mpt_packet(0, 60, &entries[..1]);
    assert_eq!(one.len(), 32);
}

#[test]
fn ioc_facts_goes_out_as_request_data_in_and_reply() {
    let mock = base_mock();
    let f = mpi::ioc_facts(&mock).unwrap();
    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(
        c.entries(),
        vec![
            Entry {
                buf_type: BUF_MPI_REQUEST,
                len: 16
            },
            Entry {
                buf_type: BUF_DATA_IN,
                len: 112
            },
            Entry {
                buf_type: BUF_MPI_REPLY,
                len: 132
            },
        ]
    );
    assert_eq!(c.dout.len(), 16);
    assert_eq!(c.dout[3], 0x01);
    assert!(c.dout.iter().enumerate().all(|(i, b)| i == 3 || *b == 0));
    assert_eq!(c.din_len, 112 + MPI_REPLY_LEN);
    assert_eq!(f.fw_version.to_string(), "8.0.1.0.00000-00042");
    assert_eq!(f.personality(), "RAID");
    assert!(f.raid_supported());
    assert_eq!(f.reply_frame_size, 32);
    assert_eq!((f.max_vds, f.max_raid_pds, f.max_nvme), (240, 240, 32));
    assert_eq!(f.capabilities(), vec!["complete-reset", "raid"]);
    assert_eq!(f.protocols(), vec!["SAS", "SATA", "NVMe", "SCSI initiator"]);
}

#[test]
fn reply_status_is_read_from_the_form_the_driver_returns() {
    let mut frame = vec![0u8; 128];
    frame.put_u16(0x00, 0x8022);
    frame.put_u32(0x04, 0x3000_1234);
    let status = Reply {
        reply_type: transport::REPLY_TYPE_STATUS,
        frame: frame.clone(),
        ..Default::default()
    };
    assert_eq!(status.ioc_status(), 0x0022);
    assert_eq!(status.ioc_log_info(), 0x3000_1234);
    let mut a = vec![0u8; 128];
    a.put_u16(0x0A, 0x8045);
    a.put_u32(0x0C, 0x31);
    let address = Reply {
        reply_type: transport::REPLY_TYPE_ADDRESS,
        frame: a,
        ..Default::default()
    };
    assert_eq!(address.ioc_status(), 0x0045);
    assert_eq!(address.ioc_log_info(), 0x31);
    let err = mpi::ensure_success(&address, "thing").unwrap_err();
    assert!(err.to_string().contains("SCSI_DATA_UNDERRUN"));
}

#[test]
fn request_frames_outside_the_driver_limits_are_refused() {
    let mock = base_mock();
    assert!(Request::new(0x10, 130).send(&mock).is_err());
    assert!(Request::new(0x10, 6).send(&mock).is_err());
    assert!(mock.calls().is_empty());
}

#[test]
fn config_read_is_a_header_step_then_a_read_step() {
    let mut mock = base_mock();
    let mut p = page(config::IOC_0, 28);
    p[0] = 0x03;
    p.put_u16(0x0C, 0x1000);
    mock.page(config::IOC_0, 0, p.clone());
    let got = config::read_page(&mock, config::IOC_0, 0).unwrap().unwrap();
    assert_eq!(got, p);
    let calls = mock.mpt_calls(mpi::FUNCTION_CONFIG);
    assert_eq!(calls.len(), 2);
    let h = calls[0].frame();
    assert_eq!(h.len(), 0x20);
    assert_eq!((h[0x0C], h[0x0D], h[0x0E], h[0x0F]), (0, 0, 0x02, 0));
    assert_eq!((h.u32_at(0x10), h.u16_at(0x14)), (0, 0));
    assert_eq!(calls[0].entries()[1].len, 8);
    let r = calls[1].frame();
    assert_eq!((r[0x0C], r[0x0D], r[0x0E], r[0x0F]), (0x03, 0, 0x02, 2));
    assert_eq!(r.u16_at(0x14), 7);
    assert_eq!(calls[1].entries()[1].len, 28);
    assert!(
        config::read_page(&mock, config::SAS_PHY_1, 0)
            .unwrap()
            .is_none()
    );
    let err = config::require_page(&mock, config::SAS_PHY_1, 3).unwrap_err();
    assert!(err.to_string().contains("SAS PHY page 1"));
}

#[test]
fn scsi_io_request_bytes_follow_the_passthrough_recipe() {
    let r = scsi::scsi_read_request(0x0009, &scsi::read_capacity16_cdb(), 32);
    assert_eq!(r.frame.len(), 0x40);
    assert_eq!(r.frame[3], 0x20);
    assert_eq!(r.frame.u16_at(0x0A), 9);
    assert_eq!(r.frame.u32_at(0x0C), 0x0008_0000);
    assert_eq!(r.frame.u32_at(0x10), 0);
    assert_eq!(r.frame.u32_at(0x14), 32);
    assert_eq!(&r.frame[0x18..0x20], &[0u8; 8]);
    assert_eq!(&r.frame[0x20..0x22], &[0x9E, 0x10]);
    assert_eq!(r.frame.u32_at(0x2A).swap_bytes(), 32);
    let types: Vec<u8> = r.entries().iter().map(|e| e.buf_type).collect();
    assert_eq!(
        types,
        vec![
            BUF_MPI_REQUEST,
            BUF_DATA_IN,
            BUF_MPI_REPLY,
            BUF_ERR_RESPONSE
        ]
    );
    assert_eq!(r.entries()[3].len, 256);
    assert!(r.timeout >= 60);
}

#[test]
fn scsi_io_reports_sense_from_an_address_reply() {
    let mock = rich_mock();
    let d = scsi::scsi_read(&mock, 9, &scsi::inquiry_cdb(), 36).unwrap();
    assert_eq!(scsi::parse_inquiry(&d).vendor, "SEAGATE");
    let err = scsi::scsi_read(&mock, 9, &scsi::log_sense_cdb(0x18), 252).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("SCSI status 0x02"), "{msg}");
    assert!(msg.contains("sense key 0x5 ASC 0x24"), "{msg}");
}

#[test]
fn nvme_encapsulated_request_carries_the_admin_command() {
    let r = nvme::smart_log_request(0x000B);
    assert_eq!(r.frame.len(), 0x60);
    assert_eq!(r.frame[3], 0x24);
    assert_eq!(r.frame.u16_at(0x0A), 0x0B);
    assert_eq!(r.frame.u16_at(0x0C), 64);
    assert_eq!(r.frame.u16_at(0x0E), 1);
    assert_eq!(r.frame.u32_at(0x10), 512);
    assert_eq!(r.frame[0x20], 0x02);
    assert_eq!(r.frame.u32_at(0x24), 0xFFFF_FFFF);
    assert_eq!(r.frame.u32_at(0x20 + 40), 0x007F_0002);
    assert_eq!(r.data_in_len, 512);
    let i = nvme::identify_controller_request(0x0B);
    assert_eq!((i.frame[0x20], i.frame.u32_at(0x24)), (0x06, 0));
    assert_eq!(i.frame.u32_at(0x48), 1);
    assert_eq!(i.data_in_len, 4096);
    let mock = rich_mock();
    let err = nvme::admin(&mock, &nvme::smart_log_request(0x0C)).unwrap_err();
    assert!(err.to_string().contains("NVMe status 0x0002"));
}

#[test]
fn io_unit_control_manifest_and_pel_requests() {
    let p = mpi::phy_reset_request(5, false);
    assert_eq!(p.frame.len(), 0x40);
    assert_eq!((p.frame[3], p.frame[0x0B]), (0x08, 0x21));
    assert_eq!((p.frame[0x38], p.frame[0x39]), (1, 5));
    assert_eq!(mpi::phy_reset_request(5, true).frame[0x38], 2);
    assert_eq!(p.data_in_len, 0);

    let m = mpi::manifest_request();
    assert_eq!(m.frame.len(), 0x20);
    assert_eq!(m.frame[3], 0x07);
    assert_eq!(m.frame[7], 0);
    assert_eq!(m.frame.u32_at(0x0C), 0x464E_414D);
    assert_eq!(m.frame.u32_at(0x14), 0x100);
    assert_eq!(m.frame.u32_at(0x18), 176);
    assert_eq!(m.data_in_len, 176);

    let s = event::seqnum_request();
    assert_eq!((s.frame.len(), s.frame[3], s.frame[0x0A]), (0x20, 0x09, 1));
    assert_eq!(s.data_in_len, 24);
    let g = event::get_log_request(77, 0x03FF, 2, 4);
    assert_eq!(g.frame[0x0A], 3);
    assert_eq!(g.frame.u32_at(0x0C), 77);
    assert_eq!(g.frame.u16_at(0x10), 0x03FF);
    assert_eq!(g.frame[0x12], 2);
    assert_eq!(g.data_in_len, 8 + 4 * 128);
}

#[test]
fn adpinfo_bitfields_and_driver_block_parse() {
    let i = AdpInfo::parse(&adpinfo(1));
    assert_eq!(i.pci_address(), "0001:41:03.1");
    assert_eq!((i.pci_dev_id, i.pci_subsys_dev_id), (0xA5, 0x4000));
    assert_eq!(i.app_intfc_ver, 6);
    assert!(i.operational());
    assert_eq!(i.driver_name, "mpi3mr");
    assert_eq!(i.driver_version, "8.17.0.3.50");
    assert_eq!(adapter::adp_state_name(3), "reset in progress");
    assert_eq!(adapter::chip_name(0xA5), Some("SAS4116"));
    assert_eq!(adapter::chip_name(0xB5), Some("SAS5116 MPI MGMT"));
    assert_eq!(adapter::chip_of(0x1234, Some("")), "unknown (0x1234)");
}

#[test]
fn target_list_is_sized_first_then_read_whole() {
    let mut mock = base_mock();
    mock.map = vec![(9, 109, 0, 0), (0x20, 0, 3, 1), (1, 1, u32::MAX, 0xFF)];
    let list = adapter::all_target_info(&mock).unwrap();
    let calls = mock.driver_calls(adapter::OPCODE_ALLTGTDEVINFO);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].din_len, 8);
    assert_eq!(calls[1].din_len, 8 + 3 * 12);
    assert!(calls.iter().all(|c| c.dout.is_empty()));
    assert_eq!(list.len(), 3);
    assert_eq!(list[1].handle, 0x20);
    assert!(list[1].exposed());
    assert!(!list[2].exposed());
    let empty = base_mock();
    assert!(adapter::all_target_info(&empty).unwrap().is_empty());
    assert_eq!(empty.calls().len(), 1);
}

#[test]
fn enumeration_uses_proc_name_and_unique_id() {
    let targets = adapter::order_targets(vec![
        host(9, "mpi3mr", Some(1)),
        host(3, "mpt3sas", Some(0)),
        host(4, "mpi3mr", Some(0)),
        host(5, "mpi3mr", None),
        host(6, "mpi3mr", Some(300)),
    ]);
    let order: Vec<(u8, u32)> = targets.iter().map(|t| (t.index, t.host.host_no)).collect();
    assert_eq!(order, vec![(0, 4), (1, 9)]);

    let root = fixture("test-mpi3-enum");
    for (n, driver, uid) in [
        (4, "mpi3mr", "0"),
        (5, "megaraid_sas", "0"),
        (8, "mpi3mr", "1"),
    ] {
        let h = root.join(format!("class/scsi_host/host{n}"));
        fs::create_dir_all(&h).unwrap();
        fs::write(h.join("proc_name"), format!("{driver}\n")).unwrap();
        fs::write(h.join("unique_id"), uid).unwrap();
    }
    let found = adapter::enumerate(&root);
    fs::remove_dir_all(&root).unwrap();
    let ids: Vec<(u8, u32)> = found.iter().map(|t| (t.index, t.host.host_no)).collect();
    assert_eq!(ids, vec![(0, 4), (1, 8)]);
}

#[test]
fn adapter_row_reads_adpinfo_and_facts() {
    let targets =
        adapter::order_targets(vec![host(4, "mpi3mr", Some(0)), host(8, "mpi3mr", Some(1))]);
    let rows: Vec<_> = targets
        .iter()
        .map(|t| {
            let mut m = base_mock();
            m.id = t.index;
            if t.index == 1 {
                m.adpinfo = adpinfo(2);
            }
            inventory::adapter_row(t, &m).unwrap()
        })
        .collect();
    let a = &rows[0];
    assert_eq!((a.index, a.host), (0, 4));
    assert_eq!(a.chip, "SAS4116");
    assert_eq!(a.pci_address, "0001:41:03.1");
    assert_eq!(a.firmware_version.as_deref(), Some("8.0.1.0.00000-00042"));
    assert_eq!(a.personality, Some("RAID"));
    let b = &rows[1];
    assert_eq!(b.state, "fault");
    assert!(b.firmware_version.is_none());
}

#[test]
fn page_parsers_follow_the_offset_tables() {
    let mut m0 = page(config::MANUFACTURING_0, 0x1C0);
    m0.put_bytes(0x08, b"B0");
    m0.put_bytes(0x10, b"SAS4116");
    m0.put_bytes(0x30, b"HBA 9600-16i  ");
    m0.put_bytes(0x50, b"03-50111-00001");
    m0.put_bytes(0x70, b"SP12345678");
    m0.put_u8(0xA0, 7);
    m0.put_u8(0xA1, 3);
    m0.put_u16(0xA2, 2023);
    m0.put_bytes(0xC0, b"Broadcom HBA 9600-16i Tri-Mode Storage Adapter");
    let m = pages::Manufacturing0::parse(&m0);
    assert_eq!(
        (m.chip_name.as_str(), m.chip_revision.as_str()),
        ("SAS4116", "B0")
    );
    assert_eq!(m.board_name, "HBA 9600-16i");
    assert_eq!(m.board_tracer_number, "SP12345678");
    assert_eq!(m.board_mfg_date.as_deref(), Some("2023-03-07"));
    assert!(m.product_name.starts_with("Broadcom HBA 9600-16i"));

    let mut io4 = page(config::IO_UNIT_4, 0x10 + 3 * 16);
    io4.put_u8(0x0C, 3);
    io4.put_u16(0x10, 55);
    io4.put_u8(0x14, 0x01);
    io4.put_u16(0x18, 0xFFFF);
    io4.put_u16(0x20, 41);
    io4.put_u8(0x24, 0x01 | (1 << 5));
    io4.put_u16(0x28, 2);
    io4.put_u8(0x2A, 1);
    let s = pages::parse_io_unit_4(&io4);
    assert_eq!(s.len(), 3);
    assert!(s[0].valid && s[0].internal && s[0].location == "internal" && s[0].raw == 55);
    assert!(s[1].valid && !s[1].internal && s[1].location == "inlet");
    assert_eq!((s[1].istwi_index, s[1].channel), (2, 1));
    assert!(!s[2].valid);
    let mut truncated = io4.clone();
    truncated.put_u8(0x0C, 9);
    assert_eq!(pages::parse_io_unit_4(&truncated).len(), 3);

    let mut io19 = page(config::IO_UNIT_19, 0x10 + 2 * 8);
    io19.put_u16(0x08, 2);
    io19.put_u16(0x10, 38);
    io19.put_u16(0x12, 9);
    io19.put_u16(0x14, 109);
    io19.put_u16(0x18, 0x8000);
    io19.put_u16(0x1A, 10);
    let d = pages::parse_io_unit_19(&io19);
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].raw, d[0].dev_handle, d[0].persistent_id),
        (38, 9, 109)
    );

    let mut sio = page(config::SAS_IO_UNIT_0, 0x10 + 2 * 20);
    sio.put_u8(0x0C, 2);
    sio.put_u8(0x10, 0);
    sio.put_u8(0x13, 0xBB);
    sio.put_u16(0x18, 0x0009);
    sio.put_u8(0x10 + 20, 1);
    sio.put_u8(0x10 + 20 + 2, 0x08);
    let phys = pages::parse_sas_io_unit_0(&sio);
    assert_eq!(phys.len(), 2);
    assert_eq!(
        (phys[0].attached_dev_handle, phys[0].negotiated_link_rate),
        (9, 0xBB)
    );
    assert!(phys[1].disabled() && phys[1].io_unit_port == 1);

    let mut phy1 = page(config::SAS_PHY_1, 28);
    phy1.put_u32(0x0C, 1);
    phy1.put_u32(0x10, 2);
    phy1.put_u32(0x14, 3);
    phy1.put_u32(0x18, 4);
    let c = pages::SasPhy1::parse(&phy1);
    assert_eq!(
        (
            c.invalid_dword_count,
            c.running_disparity_error_count,
            c.loss_dword_synch_count,
            c.phy_reset_problem_count
        ),
        (1, 2, 3, 4)
    );

    let mut phy0 = page(config::SAS_PHY_0, 36);
    phy0.put_u8(0x15, 0xC8);
    let p0 = pages::SasPhy0::parse(&phy0);
    assert_eq!(mpi::sas_link_rate_name(p0.hw_link_rate >> 4), "22.5 Gb/s");
    assert_eq!(mpi::sas_link_rate_name(p0.hw_link_rate), "1.5 Gb/s");

    let e = Enclosure0::parse(&enclosure0(2, 0x0C, 24));
    assert_eq!(
        (e.enclosure_handle, e.num_slots, e.sep()),
        (2, 24, Some(0x0C))
    );
    assert_eq!(
        (e.enclosure_type(), e.management()),
        ("SAS", "SES enclosure")
    );
    let mut virt = enclosure0(4, 0x0D, 8);
    virt.put_u16(0x10, 0x0001);
    let v = Enclosure0::parse(&virt);
    assert_eq!(
        (v.sep(), v.enclosure_type(), v.management()),
        (None, "virtual", "IOC SES")
    );

    let mut ioc0 = page(config::IOC_0, 28);
    ioc0.put_u16(0x0C, 0x1000);
    ioc0.put_u16(0x0E, 0xA5);
    ioc0.put_u16(0x18, 0x1028);
    let i = pages::Ioc0::parse(&ioc0);
    assert_eq!(
        (i.vendor_id, i.device_id, i.subsystem_vendor_id),
        (0x1000, 0xA5, 0x1028)
    );

    let mut io0 = page(config::IO_UNIT_0, 24);
    io0.put_u32(0x10, 0x0A00_0001);
    io0.put_u32(0x14, 0x0A00_0002);
    let u = pages::IoUnit0::parse(&io0);
    assert_eq!(
        (u.nvdata_version_default, u.nvdata_version_persistent),
        (0x0A00_0001, 0x0A00_0002)
    );
}

#[test]
fn device_page_zero_decodes_every_form() {
    let sas = Device0::parse(&sas_device(9, 2, 5, SSP_END));
    assert_eq!((sas.dev_handle, sas.enclosure_handle, sas.slot), (9, 2, 5));
    assert_eq!(sas.persistent_id, 109);
    assert_eq!(sas.protocol(), Some(Protocol::Sas));
    assert_eq!(sas.link_rate(), Some("12.0 Gb/s"));
    let s = sas.sas().unwrap();
    assert_eq!((s.sas_address, s.phy_num), (0x5000_c500_0000_0009, 4));

    let short = Device0::parse(&sas_device(9, 2, 5, SATA_END)[..0x3B]);
    assert_eq!(short.protocol(), Some(Protocol::Sata));
    assert_eq!(short.link_rate(), None);

    let exp = Device0::parse(&sas_device(
        1,
        0,
        0,
        pages::SAS_DEVICE_TYPE_EXPANDER | 0x0100,
    ));
    assert_eq!(exp.protocol(), None);
    assert!(exp.is_expander());

    let nv = Device0::parse(&nvme_device(0x0B, 3, 0));
    assert_eq!(nv.protocol(), Some(Protocol::Nvme));
    assert_eq!(nv.link_rate(), Some("16.0 GT/s"));
    match &nv.form {
        DeviceForm::Pcie(p) => assert_eq!((p.negotiated_port_width, p.page_size), (4, 12)),
        other => panic!("{other:?}"),
    }

    let mut hidden = sas_device(9, 2, 5, SSP_END);
    hidden.put_u16(0x1C, pages::DEVICE_FLAGS_HIDDEN);
    hidden.put_u8(0x1B, 0x05);
    let h = Device0::parse(&hidden);
    assert!(h.hidden());
    assert_eq!(inventory::drive_state(&h), "device missing delay");

    let vd = Device0::parse(&vd_device(0x20, 0, 2, 10));
    let v = vd.vd().unwrap();
    assert_eq!((v.vd_state, v.raid_level, v.vd_abort_to), (2, 10, 30));
    assert_eq!(pages::vd_state_name(v.vd_state), "Degraded");
    assert_eq!(pages::vd_media(v.device_info), vec!["HDD", "SAS"]);
    assert_eq!(pages::vd_os_exposure(v.flags), "SSD");
    assert_eq!(vd.protocol(), None);
    let other = Device0::parse(&device0(5, 0, 0, 0, 7));
    assert_eq!(other.form, DeviceForm::Other { code: 7 });
}

#[test]
fn manifest_nvme_and_scsi_payloads_parse() {
    let m = mpi::parse_manifest(&manifest()).unwrap();
    assert_eq!(m.package_version.to_string(), "8.0.3.0.00000-00016");
    assert_eq!(mpi::release_level_name(m.release_level), "GCA");
    let mut other = manifest();
    other[0] = 1;
    assert!(mpi::parse_manifest(&other).is_none());
    assert!(mpi::parse_manifest(&manifest()[..0x40]).is_none());

    let id = nvme::parse_identify_controller(&identify("S5XYZ", "MODEL X", "FW1"));
    assert_eq!(
        (id.serial.as_str(), id.model.as_str(), id.firmware.as_str()),
        ("S5XYZ", "MODEL X", "FW1")
    );
    let s = nvme::parse_smart_log(&smart_log(310, 0x05));
    assert_eq!(s.celsius(), Some(37));
    assert_eq!((s.available_spare, s.percentage_used), (100, 3));
    assert_eq!(
        (s.power_cycles, s.power_on_hours, s.error_log_entries),
        (42, 12000, 7)
    );
    assert_eq!(
        nvme::critical_warnings(s.critical_warning),
        vec!["spare below threshold", "reliability degraded"]
    );
    let mut huge = smart_log(0, 0);
    huge.put_u64(168, 1);
    assert_eq!(nvme::parse_smart_log(&huge).media_errors, u64::MAX);
    assert_eq!(nvme::parse_smart_log(&huge).celsius(), None);

    assert_eq!(scsi::parse_temperature_log(&temperature_log(41)), Some(41));
    assert_eq!(scsi::parse_temperature_log(&temperature_log(0xFF)), None);
    let ie = scsi::parse_ie_log(&[0x2F, 0, 0, 7, 0, 0, 3, 3, 0x5D, 0x10, 45]).unwrap();
    assert!(ie.failure_predicted());
    assert_eq!(ie.most_recent_temperature, Some(45));
    assert!(scsi::parse_ie_log(&temperature_log(3)).is_none());
}

#[test]
fn controller_show_combines_adpinfo_facts_pages_and_manifest() {
    let mut mock = rich_mock();
    let mut m0 = page(config::MANUFACTURING_0, 0x1C0);
    m0.put_bytes(0x10, b"SAS4116");
    m0.put_bytes(0x30, b"PERC H965i");
    mock.page(config::MANUFACTURING_0, 0, m0);
    let mut ioc0 = page(config::IOC_0, 28);
    ioc0.put_u16(0x0C, 0x1000);
    mock.page(config::IOC_0, 0, ioc0);
    let mut io4 = page(config::IO_UNIT_4, 0x20);
    io4.put_u8(0x0C, 1);
    io4.put_u16(0x10, 52);
    io4.put_u8(0x14, 0x01);
    io4.put_u16(0x18, 0xFFFF);
    mock.page(config::IO_UNIT_4, 0, io4);
    let out = run(&mock, false, &["controller"]).unwrap();
    assert!(out.contains("Controller 2"), "{out}");
    assert!(out.contains("PERC H965i"));
    assert!(out.contains("8.0.1.0.00000-00042"));
    assert!(out.contains("8.0.3.0.00000-00016"));
    assert!(out.contains("1000:00a5 rev 01, subsystem 1000:4000"));
    assert!(out.contains("RAID"));
    assert!(out.contains("4 SAS/SATA, 1 PCIe, 2 VD"), "{out}");
    assert!(out.contains("Temperature sensors"));
    assert!(out.contains("52 C"));
    let upload = mock.mpt_calls(mpi::FUNCTION_CI_UPLOAD);
    assert_eq!(upload.len(), 1);
    assert_eq!(upload[0].frame(), mpi::manifest_request().frame);

    let mut faulted = rich_mock();
    faulted.adpinfo = adpinfo(4);
    let err = run(&faulted, false, &["controller"]).unwrap_err();
    assert!(err.to_string().contains("unrecoverable"));
}

#[test]
fn drive_list_keeps_end_devices_and_nvme_and_drops_the_sep() {
    let mock = rich_mock();
    let root = fixture("test-mpi3-os");
    fs::create_dir_all(root.join("class/scsi_device/7:0:0:0/device/block/sdb")).unwrap();
    let out = run_in(&mock, false, &root, &["drive"]).unwrap();
    fs::remove_dir_all(&root).unwrap();
    assert!(out.contains("2:0"), "{out}");
    assert!(out.contains("SAS_HDD"));
    assert!(out.contains("2:1"));
    assert!(out.contains("SATA_SSD"));
    assert!(out.contains("3:0"));
    assert!(out.contains("NVMe_SSD"));
    assert!(out.contains("/dev/sdb"));
    assert!(!out.contains("2:24"), "{out}");
    assert!(!out.contains("0x0001"));
    assert!(out.contains("34C"));
    assert!(out.contains("37C"));
    assert!(out.contains("ST4000NM0023"));
    assert!(out.contains("SAMSUNG MZWLR3T8HBLS"));
    let scsi_handles: Vec<u16> = mock
        .mpt_calls(mpi::FUNCTION_SCSI_IO)
        .iter()
        .map(|c| c.frame().u16_at(0x0A))
        .collect();
    assert!(!scsi_handles.contains(&0x0B));
    assert!(
        mock.mpt_calls(mpi::FUNCTION_SCSI_IO)
            .iter()
            .filter(|c| c.frame().u16_at(0x0A) == 0x0A)
            .all(|c| c.frame()[0x20] != 0x4D)
    );
    let device_reads: Vec<u32> = mock
        .mpt_calls(mpi::FUNCTION_CONFIG)
        .iter()
        .map(|c| c.frame())
        .filter(|f| f[0x0E] == 0x12 && f[0x0F] == 2)
        .map(|f| f.u32_at(0x10))
        .collect();
    assert_eq!(
        device_reads,
        vec![
            0x2000_0001,
            0x2000_0009,
            0x2000_000A,
            0x2000_000B,
            0x2000_000C,
            0x2000_0020,
            0x2000_0021
        ]
    );
}

#[test]
fn drive_show_fills_identity_capacity_and_placement() {
    let mock = rich_mock();
    let d = inventory::drive(
        &mock,
        &OsView {
            sysfs: Path::new("/nonexistent"),
            host: 7,
        },
        "2:0".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(d.protocol, "SAS");
    assert_eq!(d.serial_number.as_deref(), Some("Z1Z3ABCD"));
    assert_eq!(d.guid.as_deref(), Some("5000c50085e7bd3f"));
    assert_eq!(d.size_mb, Some(3_815_447));
    assert_eq!(d.block_size, Some(512));
    assert_eq!(d.rotation_rate, Some(7200));
    assert_eq!(d.link_rate, Some("12.0 Gb/s"));
    assert_eq!(d.linux_channel_target.as_deref(), Some("0:0"));
    assert_eq!(d.enclosure_logical_id.as_deref(), Some("500a098000000002"));
    assert_eq!(d.state, "healthy");
    assert!(d.exposed);
    let out = run(&mock, false, &["drive", "3:0"]).unwrap();
    assert!(out.contains("S5XYZ"), "{out}");
    assert!(out.contains("MPK7525Q"));
    assert!(out.contains("16.0 GT/s"));
    assert!(out.contains("37C (98.60F)"));
    let err = run(&mock, false, &["drive", "2:24"]).unwrap_err();
    assert!(err.to_string().contains("2:24"));
}

#[test]
fn drive_smart_uses_log_sense_for_sas_and_the_health_log_for_nvme() {
    let mock = rich_mock();
    let out = run(&mock, false, &["drive", "2:0", "smart"]).unwrap();
    assert!(out.contains("Healthy") && out.contains("Yes"), "{out}");
    assert!(out.contains("ASC 0x00 ASCQ 0x00"));
    assert!(out.contains("34C"));
    let out = run(&mock, false, &["drive", "3:0", "smart"]).unwrap();
    assert!(
        out.contains("Percentage used") && out.contains("3%"),
        "{out}"
    );
    assert!(out.contains("Warnings") && out.contains("none"));
    let smart = mock.mpt_calls(mpi::FUNCTION_NVME_ENCAPSULATED);
    assert!(
        smart
            .iter()
            .any(|c| c.frame() == nvme::smart_log_request(0x0B).frame)
    );
    let err = run(&mock, false, &["drive", "2:1", "smart"]).unwrap_err();
    assert!(err.to_string().contains("not documented"));
}

#[test]
fn volumes_come_from_the_vd_form_and_read_capacity() {
    let mock = rich_mock();
    let out = run(&mock, false, &["volume"]).unwrap();
    assert!(out.contains("RAID5"), "{out}");
    assert!(out.contains("Optimal"));
    assert!(out.contains("RAID1"));
    assert!(out.contains("Offline"));
    assert!(out.contains("3815447"));
    let v = inventory::volume(
        &mock,
        &OsView {
            sysfs: Path::new("/nonexistent"),
            host: 7,
        },
        0,
    )
    .unwrap();
    assert_eq!(v.handle, 0x20);
    assert_eq!(v.media, vec!["HDD", "SAS"]);
    assert_eq!(v.os_exposure_hint, "SSD");
    assert_eq!(v.abort_timeout_seconds, Some(30));
    assert_eq!(v.reset_timeout_seconds, None);
    assert_eq!(v.linux_channel_target.as_deref(), Some("1:0"));
    let out = run(&mock, false, &["volume", "1"]).unwrap();
    assert!(out.contains("Virtual disk 1"));
    assert!(out.contains("16 MiB low, 64 MiB high"));
    assert!(run(&mock, false, &["volume", "9"]).is_err());
}

#[test]
fn enclosure_list_reads_each_handle_and_the_sep_inquiry() {
    let mock = rich_mock();
    let out = run(&mock, false, &["enclosure"]).unwrap();
    assert!(out.contains("500a098000000002"), "{out}");
    assert!(out.contains("VirtualSES"));
    assert!(out.contains("PCIe"));
    let reads: Vec<u32> = mock
        .mpt_calls(mpi::FUNCTION_CONFIG)
        .iter()
        .map(|c| c.frame())
        .filter(|f| f[0x0E] == 0x11 && f[0x0F] == 2)
        .map(|f| f.u32_at(0x10))
        .collect();
    assert_eq!(reads, vec![0x1000_0002, 0x1000_0003]);
}

fn phy_mock() -> Mock {
    let mut mock = rich_mock();
    let mut sio = page(config::SAS_IO_UNIT_0, 0x10 + 2 * 20);
    sio.put_u8(0x0C, 2);
    sio.put_u8(0x13, 0x0B);
    sio.put_u16(0x18, 0x0009);
    sio.put_u8(0x10 + 20 + 2, 0x08);
    mock.page(config::SAS_IO_UNIT_0, 0, sio);
    let mut phy0 = page(config::SAS_PHY_0, 36);
    phy0.put_u8(0x15, 0xC8);
    mock.page(config::SAS_PHY_0, 0, phy0);
    let mut phy1 = page(config::SAS_PHY_1, 28);
    phy1.put_u32(0x0C, 17);
    phy1.put_u32(0x18, 2);
    mock.page(config::SAS_PHY_1, 0, phy1);
    mock
}

#[test]
fn phy_list_and_errors_come_from_sas_io_unit_and_phy_pages() {
    let mock = phy_mock();
    let out = run(&mock, false, &["phy"]).unwrap();
    assert!(out.contains("12.0 Gb/s"), "{out}");
    assert!(out.contains("22.5 Gb/s"));
    assert!(out.contains("5000c50000000009"));
    assert!(out.contains("SAS end device"));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[3].starts_with("1") && lines[3].contains("No"));
    let out = run(&mock, false, &["phy", "errors"]).unwrap();
    assert!(out.contains("17"), "{out}");
    let phy1: Vec<u32> = mock
        .mpt_calls(mpi::FUNCTION_CONFIG)
        .iter()
        .map(|c| c.frame())
        .filter(|f| f[0x0E] == 0x23 && f[0x0D] == 1 && f[0x0F] == 2)
        .map(|f| f.u32_at(0x10))
        .collect();
    assert_eq!(phy1, vec![0, 1]);
}

#[test]
fn temperature_show_prints_sensor_readings_in_celsius() {
    let mut mock = rich_mock();
    let mut io4 = page(config::IO_UNIT_4, 0x30);
    io4.put_u8(0x0C, 2);
    io4.put_u16(0x10, 61);
    io4.put_u8(0x14, 0x01);
    io4.put_u16(0x18, 0xFFFF);
    io4.put_u16(0x20, 44);
    io4.put_u8(0x24, 0x01 | (2 << 5));
    mock.page(config::IO_UNIT_4, 0, io4);
    let mut io19 = page(config::IO_UNIT_19, 0x18);
    io19.put_u16(0x08, 1);
    io19.put_u16(0x10, 39);
    io19.put_u16(0x12, 0x0009);
    io19.put_u16(0x14, 109);
    mock.page(config::IO_UNIT_19, 0, io19);
    let out = run(&mock, false, &["temperature"]).unwrap();
    assert!(out.contains("unit is not documented"), "{out}");
    assert!(out.contains("61"));
    assert!(out.contains("outlet"));
    assert!(out.contains("2:0"));
    assert!(out.contains("39"));
    assert!(!out.contains('C') || !out.contains("39C"));
    let t = inventory::temperature(&mock).unwrap();
    let json = serde_json::to_string(&t).unwrap();
    assert!(json.contains("\"raw\":61"));
    assert!(json.contains("\"celsius\":61"));
    let bare = rich_mock();
    assert!(run(&bare, false, &["temperature"]).is_err());
}

#[test]
fn event_list_reads_sequence_numbers_then_pages_through_the_log() {
    let mut mock = base_mock();
    let mut seq = vec![0u8; 24];
    seq.put_u32(0x00, 140);
    seq.put_u32(0x04, 100);
    seq.put_u32(0x10, 120);
    mock.seq = seq;
    mock.pel = (100..=140)
        .map(|s| pel_entry(s, 0x0100 + s as u16, (s % 4) as u8, 0x0002))
        .collect();
    let log = event::read_log(&mock, None).unwrap();
    assert_eq!(log.events.len(), 41);
    assert_eq!(log.events[0].sequence, 100);
    assert_eq!(log.events[40].sequence, 140);
    assert_eq!(log.events[3].class_name, "warning");
    assert_eq!(log.events[3].locale_names, vec!["PD"]);
    assert_eq!(log.events[0].info, "efbeadde");
    let starts: Vec<u32> = mock
        .mpt_calls(mpi::FUNCTION_PERSISTENT_EVENT_LOG)
        .iter()
        .map(|c| c.frame())
        .filter(|f| f[0x0A] == 3)
        .map(|f| f.u32_at(0x0C))
        .collect();
    assert_eq!(starts, vec![100, 132]);
    let out = run(&mock, false, &["event", "--count", "3"]).unwrap();
    assert!(out.contains("oldest 100, newest 140, boot 120"), "{out}");
    assert!(out.contains("138") && out.contains("140") && !out.contains("137"));
    assert!(out.contains("0x018c"));

    let mut empty = base_mock();
    let mut seq = vec![0u8; 24];
    seq.put_u32(0x00, 5);
    seq.put_u32(0x04, 1);
    empty.seq = seq;
    let out = run(&empty, false, &["event"]).unwrap();
    assert!(out.contains("No events logged"));
}

#[test]
fn firmware_show_reports_running_package_and_nvdata_versions() {
    let mut mock = base_mock();
    let mut io0 = page(config::IO_UNIT_0, 24);
    io0.put_u32(0x10, 0x0A00_0001);
    io0.put_u32(0x14, 0x0A00_0002);
    mock.page(config::IO_UNIT_0, 0, io0);
    let out = run(&mock, false, &["firmware"]).unwrap();
    assert!(out.contains("8.0.1.0.00000-00042"), "{out}");
    assert!(out.contains("8.0.3.0.00000-00016"));
    assert!(out.contains("GCA"));
    assert!(out.contains("0a000002"));
    assert!(out.contains("1000:00a5 subsystem 1000:4000"));
    let mut plain = base_mock();
    plain.manifest = None;
    let f = inventory::firmware_info(&plain).unwrap();
    assert!(f.package_version.is_none() && f.nvdata_version_default.is_none());
}

const WRITES: &[&[&str]] = &[
    &["controller", "reset"],
    &["controller", "reset", "--snapdump"],
    &["phy", "0", "reset"],
    &["phy", "1", "reset", "--hard"],
];

#[test]
fn every_state_change_is_refused_without_yes_before_sending_anything() {
    for args in WRITES {
        let mock = phy_mock();
        let err = run(&mock, false, args).unwrap_err();
        assert!(err.to_string().contains("--yes"), "{args:?} gave {err}");
        assert!(mock.calls().is_empty(), "{args:?} touched the controller");
    }
}

#[test]
fn confirmed_resets_send_exactly_the_documented_requests() {
    let mock = phy_mock();
    let out = run(&mock, true, &["controller", "reset"]).unwrap();
    assert!(out.contains("soft reset completed"));
    let calls = mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].packet, transport::driver_packet(2, 2));
    assert_eq!(calls[0].dout, vec![1, 0, 0, 0]);
    assert_eq!(calls[0].din_len, 0);

    let mock = phy_mock();
    run(&mock, true, &["controller", "reset", "--snapdump"]).unwrap();
    assert_eq!(mock.calls()[0].dout, vec![2, 0, 0, 0]);

    let mock = phy_mock();
    let out = run(&mock, true, &["phy", "1", "reset", "--hard"]).unwrap();
    assert!(out.contains("phy 1 hard reset"));
    let ctl = mock.mpt_calls(mpi::FUNCTION_IO_UNIT_CONTROL);
    assert_eq!(ctl.len(), 1);
    assert_eq!(ctl[0].frame(), mpi::phy_reset_request(1, true).frame);
    assert_eq!(ctl[0].entries()[1].buf_type, BUF_MPI_REPLY);

    let mock = phy_mock();
    let err = run(&mock, true, &["phy", "9", "reset"]).unwrap_err();
    assert!(err.to_string().contains("phy 9 does not exist"));
    assert!(mock.mpt_calls(mpi::FUNCTION_IO_UNIT_CONTROL).is_empty());
}

#[test]
fn every_read_renders_json() {
    let mock = phy_mock();
    let os = OsView {
        sysfs: Path::new("/nonexistent"),
        host: 7,
    };
    let a: DriveAddress = "2:0".parse().unwrap();
    let values = [
        serde_json::to_value(inventory::drives(&mock, &os).unwrap()).unwrap(),
        serde_json::to_value(inventory::drive(&mock, &os, a).unwrap()).unwrap(),
        serde_json::to_value(inventory::drive_smart(&mock, a).unwrap()).unwrap(),
        serde_json::to_value(inventory::volumes(&mock, &os).unwrap()).unwrap(),
        serde_json::to_value(inventory::enclosure_list(&mock).unwrap()).unwrap(),
        serde_json::to_value(inventory::phy_list(&mock).unwrap()).unwrap(),
        serde_json::to_value(inventory::phy_errors(&mock).unwrap()).unwrap(),
        serde_json::to_value(inventory::firmware_info(&mock).unwrap()).unwrap(),
        serde_json::to_value(inventory::controller_info(&target(), &mock).unwrap()).unwrap(),
    ];
    assert_eq!(values[0]["drives"][0]["address"], "2:0");
    assert_eq!(values[1]["temperature"]["celsius"], 34);
    assert_eq!(values[2]["informational_exceptions"]["asc"], 0);
    assert_eq!(values[3]["volumes"][0]["raid_level"], "RAID5");
    assert_eq!(values[8]["personality"], "RAID");
    let facts: IocFacts = mpi::ioc_facts(&mock).unwrap();
    assert_eq!(serde_json::to_value(facts).unwrap()["max_vds"], 240);
}

#[test]
fn drive_address_parses_enclosure_and_slot() {
    let a: DriveAddress = " 2 : 14 ".parse().unwrap();
    assert_eq!((a.enclosure, a.slot), (2, 14));
    assert_eq!(a.to_string(), "2:14");
    assert!("2".parse::<DriveAddress>().is_err());
    assert!("x:1".parse::<DriveAddress>().is_err());
}

fn execute_json(mock: &Mock, args: &[&str]) -> serde_json::Value {
    let command = crate::cli::try_parse(args).unwrap();
    crate::tests::enveloped(
        2,
        "mpi3mr",
        cli::execute(
            &command,
            &ctx(false, PathBuf::from("/nonexistent")),
            &target(),
            mock,
        ),
    )
}

#[test]
fn drive_json_matches_the_go_fixture() {
    use crate::tests::{assert_matches_fixture, go_fixture};
    let mock = rich_mock();
    let real = execute_json(&mock, &["drive"]);
    assert!(real[0]["error"].is_null(), "{real}");
    assert_matches_fixture(
        &real,
        &go_fixture("sasctl_2_drives.json"),
        "sasctl_2_drives.json",
    );
    let drives = real[0]["drives"].as_array().unwrap();
    let nvme = drives
        .iter()
        .find(|d| d["protocol"] == "NVMe")
        .expect("an NVMe drive");
    assert_eq!(nvme["drive_type"], "NVMe_SSD");
    assert_eq!(nvme["temperature"]["celsius"], 37);
    let sas = drives.iter().find(|d| d["address"] == "2:0").unwrap();
    assert_eq!(sas["temperature"]["celsius"], 34);
}

#[test]
fn temperature_json_matches_the_go_fixture() {
    use crate::tests::{assert_matches_fixture, go_fixture};
    let mut mock = rich_mock();
    let mut io4 = page(config::IO_UNIT_4, 0x30);
    io4.put_u8(0x0C, 2);
    io4.put_u16(0x10, 52);
    io4.put_u8(0x14, 0x01);
    io4.put_u16(0x18, 0xFFFF);
    io4.put_u16(0x20, 44);
    io4.put_u8(0x24, 2 << 5);
    mock.page(config::IO_UNIT_4, 0, io4);
    let real = execute_json(&mock, &["temperature"]);
    assert!(real[0]["error"].is_null(), "{real}");
    assert_matches_fixture(
        &real,
        &go_fixture("sasctl_2_temperature.json"),
        "sasctl_2_temperature.json",
    );
    let sensors = real[0]["sensors"].as_array().unwrap();
    assert_eq!(sensors[0]["celsius"], 52);
    assert_eq!(sensors[0]["internal"], true);
    assert!(sensors[1]["celsius"].is_null(), "{real}");
}
