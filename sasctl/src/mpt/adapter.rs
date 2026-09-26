use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::transport::{Generation, LinuxTransport, NR_BTDHMAPPING, NR_IOCINFO, Transport};
use crate::bytes::{Le, LeMut};
use crate::sysfs::{self, ScsiHost};

pub const IOCINFO_LEN: usize = 92;

#[derive(Clone, Debug)]
pub struct Target {
    pub index: usize,
    pub generation: Generation,
    pub ioc_number: u32,
    pub host: ScsiHost,
}

pub fn generation_of(proc_name: &str) -> Option<Generation> {
    [Generation::Sas2, Generation::Sas3]
        .into_iter()
        .find(|g| g.proc_name() == proc_name)
}

pub fn generation_name(generation: Generation) -> &'static str {
    match generation {
        Generation::Sas2 => "SAS2",
        Generation::Sas3 => "SAS3",
    }
}

pub fn order_targets(hosts: Vec<ScsiHost>) -> Vec<Target> {
    let mut found: Vec<(Generation, u32, ScsiHost)> = hosts
        .into_iter()
        .filter_map(|h| {
            let generation = generation_of(&h.proc_name)?;
            let ioc = h.unique_id?;
            Some((generation, ioc, h))
        })
        .collect();
    found.sort_by_key(|(g, ioc, h)| (matches!(g, Generation::Sas3), *ioc, h.host_no));
    found
        .into_iter()
        .enumerate()
        .map(|(index, (generation, ioc_number, host))| Target {
            index,
            generation,
            ioc_number,
            host,
        })
        .collect()
}

pub fn enumerate(sysfs_root: &Path) -> Vec<Target> {
    order_targets(sysfs::scsi_hosts(sysfs_root, &["mpt2sas", "mpt3sas"]))
}

pub fn open(target: &Target) -> Result<Box<dyn Transport>> {
    let node = Path::new(target.generation.node());
    let t = LinuxTransport::open(node, target.generation, target.ioc_number)?;
    Ok(Box::new(t))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IocInfo {
    pub adapter_type: u32,
    pub port_number: u32,
    pub pci_id: u32,
    pub hw_rev: u32,
    pub subsystem_device: u32,
    pub subsystem_vendor: u32,
    pub firmware_version: u32,
    pub bios_version: u32,
    pub driver_version: String,
    pub pci_segment: u32,
    pub pci_bus: u32,
    pub pci_device: u32,
    pub pci_function: u32,
}

impl IocInfo {
    pub fn parse(b: &[u8]) -> Self {
        let word = b.u32_at(0x54);
        Self {
            adapter_type: b.u32_at(0x0C),
            port_number: b.u32_at(0x10),
            pci_id: b.u32_at(0x14),
            hw_rev: b.u32_at(0x18),
            subsystem_device: b.u32_at(0x1C),
            subsystem_vendor: b.u32_at(0x20),
            firmware_version: b.u32_at(0x28),
            bios_version: b.u32_at(0x2C),
            driver_version: b.ascii_at(0x30, 32),
            pci_device: word & 0x1F,
            pci_function: (word >> 5) & 0x07,
            pci_bus: word >> 8,
            pci_segment: b.u32_at(0x58),
        }
    }

    pub fn pci_address(&self) -> String {
        format!(
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.pci_segment, self.pci_bus, self.pci_device, self.pci_function
        )
    }
}

pub fn ioc_info(t: &dyn Transport) -> Result<IocInfo> {
    let mut b = vec![0u8; IOCINFO_LEN];
    t.raw(NR_IOCINFO, IOCINFO_LEN, &mut b)?;
    Ok(IocInfo::parse(&b))
}

pub fn adapter_type_name(adapter_type: u32) -> &'static str {
    match adapter_type {
        0x04 => "SAS2",
        0x05 => "SAS2 SSS6200",
        0x06 => "SAS3",
        0x07 => "SAS3.5",
        _ => "unknown",
    }
}

pub fn chip_name(pci_id: u32) -> Option<&'static str> {
    Some(match pci_id {
        0x0070 => "SAS2004",
        0x0072 => "SAS2008",
        0x0074 | 0x0076 | 0x0077 => "SAS2108",
        0x0064 | 0x0065 => "SAS2116",
        0x0080..=0x0085 => "SAS2208",
        0x0086 | 0x0087 | 0x006E => "SAS2308",
        0x007E => "SSS6200",
        0x02B0 | 0x02B1 => "Switch MPI endpoint",
        0x0096 => "SAS3004",
        0x0097 => "SAS3008",
        0x0090 | 0x0091 | 0x0094 | 0x0095 => "SAS3108",
        0x00C9 => "SAS3216",
        0x00C4 => "SAS3224",
        0x00C5..=0x00C8 => "SAS3316",
        0x00C0..=0x00C3 => "SAS3324",
        0x00AA | 0x00AB => "SAS3516",
        0x00AC => "SAS3416",
        0x00AD | 0x00AE => "SAS3508",
        0x00AF => "SAS3408",
        0x00D1 => "SAS3616",
        0x00E0..=0x00E3 => "SAS3916",
        0x00E4..=0x00E7 => "SAS3816",
        _ => return None,
    })
}

pub const BTDH_LEN: usize = 24;

pub fn btdh_request(handle: u16) -> Vec<u8> {
    let mut b = vec![0u8; BTDH_LEN];
    b.put_u32(0x0C, u32::MAX);
    b.put_u32(0x10, u32::MAX);
    b.put_u16(0x14, handle);
    b
}

pub fn scsi_target_of(t: &dyn Transport, handle: u16) -> Option<(u32, u32)> {
    let mut b = btdh_request(handle);
    t.raw(NR_BTDHMAPPING, BTDH_LEN, &mut b).ok()?;
    let (id, bus) = (b.u32_at(0x0C), b.u32_at(0x10));
    (id != u32::MAX && bus != u32::MAX).then_some((bus, id))
}
