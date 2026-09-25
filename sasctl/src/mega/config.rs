use anyhow::{Result, bail};
use serde::Serialize;

use crate::bytes::{Le, LeMut};
use crate::mega::dcmd::{dcmd_none, dcmd_read, dcmd_variable, dcmd_write};
use crate::mega::ld::{
    CACHE_ALLOW_WRITE_CACHE, CACHE_WRITE_BACK, DDF_RAID0, DDF_RAID1, DDF_RAID5, DDF_RAID6,
    LD_CONFIG_LEN, LdConfig, MAX_SPAN_DEPTH,
};
use crate::mega::mfi::{Mbox, op};
use crate::mega::pd::{PdInfo, STATE_ONLINE, STATE_UNCONFIGURED_GOOD, state_name};
use crate::mega::transport::Transport;

pub const HEADER_LEN: usize = 32;
pub const ARRAY_LEN: usize = 288;
pub const SPARE_LEN: usize = 40;
pub const MAX_ROW_SIZE: usize = 32;
pub const MAX_ARRAYS_PER_SPARE: usize = 16;
pub const SPARE_DEDICATED: u8 = 1 << 0;
pub const SPARE_REVERTIBLE: u8 = 1 << 1;
pub const SPARE_ENCL_AFFINITY: u8 = 1 << 2;
pub const MISSING_DEVICE: u16 = 0xffff;
pub const FOREIGN_SCAN_LEN: usize = 196;
pub const FOREIGN_ALL: u8 = 0xff;
pub const MAX_FOREIGN_CONFIGS: u32 = 8;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ArrayMember {
    pub device_id: u16,
    pub seq_num: u16,
    pub missing: bool,
    pub fw_state: u16,
    pub state: String,
    pub encl_index: u8,
    pub slot: u8,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Array {
    pub size_blocks: u64,
    pub num_drives: u8,
    pub array_ref: u16,
    pub members: Vec<ArrayMember>,
}

impl Array {
    pub fn parse(b: &[u8]) -> Self {
        let num_drives = b.u8_at(8);
        let members = (0..usize::from(num_drives).min(MAX_ROW_SIZE))
            .map(|i| {
                let at = 32 + i * 8;
                let device_id = b.u16_at(at);
                let fw_state = b.u16_at(at + 4);
                ArrayMember {
                    device_id,
                    seq_num: b.u16_at(at + 2),
                    missing: device_id == MISSING_DEVICE,
                    fw_state,
                    state: state_name(fw_state, false),
                    encl_index: b.u8_at(at + 6),
                    slot: b.u8_at(at + 7),
                }
            })
            .collect();
        Self {
            size_blocks: b.u64_at(0),
            num_drives,
            array_ref: b.u16_at(10),
            members,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Spare {
    pub device_id: u16,
    pub seq_num: u16,
    pub dedicated: bool,
    pub revertible: bool,
    pub enclosure_affinity: bool,
    pub array_refs: Vec<u16>,
}

impl Spare {
    pub fn parse(b: &[u8]) -> Self {
        let kind = b.u8_at(4);
        let count = usize::from(b.u8_at(7)).min(MAX_ARRAYS_PER_SPARE);
        Self {
            device_id: b.u16_at(0),
            seq_num: b.u16_at(2),
            dedicated: kind & SPARE_DEDICATED != 0,
            revertible: kind & SPARE_REVERTIBLE != 0,
            enclosure_affinity: kind & SPARE_ENCL_AFFINITY != 0,
            array_refs: (0..count).map(|i| b.u16_at(8 + i * 2)).collect(),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ConfigData {
    pub size: u32,
    pub arrays: Vec<Array>,
    pub volumes: Vec<LdConfig>,
    pub spares: Vec<Spare>,
}

fn records(b: &[u8], start: usize, count: usize, size: usize) -> Vec<&[u8]> {
    if size == 0 {
        return Vec::new();
    }
    (0..count)
        .map_while(|i| b.get(start + i * size..start + (i + 1) * size))
        .collect()
}

impl ConfigData {
    pub fn parse(b: &[u8]) -> Self {
        let array_count = usize::from(b.u16_at(4));
        let array_size = usize::from(b.u16_at(6));
        let ld_count = usize::from(b.u16_at(8));
        let ld_size = usize::from(b.u16_at(10));
        let spare_count = usize::from(b.u16_at(12));
        let spare_size = usize::from(b.u16_at(14));
        let ld_start = HEADER_LEN + array_count * array_size;
        let spare_start = ld_start + ld_count * ld_size;
        Self {
            size: b.u32_at(0),
            arrays: records(b, HEADER_LEN, array_count, array_size)
                .into_iter()
                .map(Array::parse)
                .collect(),
            volumes: records(b, ld_start, ld_count, ld_size)
                .into_iter()
                .map(LdConfig::parse)
                .collect(),
            spares: records(b, spare_start, spare_count, spare_size)
                .into_iter()
                .map(Spare::parse)
                .collect(),
        }
    }

    pub fn array(&self, array_ref: u16) -> Option<&Array> {
        self.arrays.iter().find(|a| a.array_ref == array_ref)
    }

    pub fn volume(&self, target_id: u8) -> Option<&LdConfig> {
        self.volumes
            .iter()
            .find(|v| v.properties.target_id == target_id)
    }

    pub fn array_index_of(&self, device_id: u16) -> Option<usize> {
        self.arrays
            .iter()
            .position(|a| a.members.iter().any(|m| m.device_id == device_id))
    }

    pub fn volume_drives(&self, target_id: u8) -> Vec<&ArrayMember> {
        let Some(ld) = self.volume(target_id) else {
            return Vec::new();
        };
        ld.spans
            .iter()
            .filter_map(|s| self.array(s.array_ref))
            .flat_map(|a| a.members.iter())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaidLevel {
    Raid0,
    Raid1,
    Raid5,
    Raid6,
    Raid10,
    Raid50,
    Raid60,
}

pub const SRL_SPANNED: u8 = 3;
pub const INIT_NONE: u8 = 0;
pub const INIT_QUICK: u8 = 1;
pub const INIT_FULL: u8 = 2;

impl RaidLevel {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().trim_start_matches("raid") {
            "0" => RaidLevel::Raid0,
            "1" => RaidLevel::Raid1,
            "5" => RaidLevel::Raid5,
            "6" => RaidLevel::Raid6,
            "10" => RaidLevel::Raid10,
            "50" => RaidLevel::Raid50,
            "60" => RaidLevel::Raid60,
            "00" | "1e" => bail!(
                "RAID00 and RAID1E volumes are not supported, their level encoding is not documented"
            ),
            _ => bail!("unknown RAID level {s}, use 0, 1, 5, 6, 10, 50 or 60"),
        })
    }

    pub fn base(self) -> Self {
        match self {
            RaidLevel::Raid10 => RaidLevel::Raid1,
            RaidLevel::Raid50 => RaidLevel::Raid5,
            RaidLevel::Raid60 => RaidLevel::Raid6,
            other => other,
        }
    }

    pub fn spanned(self) -> bool {
        self.base() != self
    }

    fn encoding(self) -> (u8, u8, u8) {
        let secondary = if self.spanned() { SRL_SPANNED } else { 0 };
        match self.base() {
            RaidLevel::Raid1 => (DDF_RAID1, 0, secondary),
            RaidLevel::Raid5 => (DDF_RAID5, 3, secondary),
            RaidLevel::Raid6 => (DDF_RAID6, 3, secondary),
            _ => (DDF_RAID0, 0, secondary),
        }
    }

    fn check_drive_count(self, n: usize) -> Result<()> {
        if n == 0 || n > MAX_ROW_SIZE {
            bail!("an array takes between 1 and {MAX_ROW_SIZE} drives");
        }
        match self.base() {
            RaidLevel::Raid1 if !n.is_multiple_of(2) => {
                bail!("RAID1 needs an even number of drives per array")
            }
            RaidLevel::Raid5 if n < 3 => bail!("RAID5 needs at least 3 drives per array"),
            RaidLevel::Raid6 if n < 4 => bail!("RAID6 needs at least 4 drives per array"),
            _ => Ok(()),
        }
    }

    pub fn ctrl_capability(self) -> &'static str {
        match self.base() {
            RaidLevel::Raid1 => "RAID1",
            RaidLevel::Raid5 => "RAID5",
            RaidLevel::Raid6 => "RAID6",
            _ => "RAID0",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RaidLevel::Raid10 => "RAID10",
            RaidLevel::Raid50 => "RAID50",
            RaidLevel::Raid60 => "RAID60",
            other => other.ctrl_capability(),
        }
    }
}

pub fn parse_stripe(s: &str) -> Result<u32> {
    let lower = s.to_ascii_lowercase();
    let (num, mult) = if let Some(n) = lower
        .strip_suffix("kib")
        .or(lower.strip_suffix("kb"))
        .or(lower.strip_suffix('k'))
    {
        (n, 1024)
    } else if let Some(n) = lower
        .strip_suffix("mib")
        .or(lower.strip_suffix("mb"))
        .or(lower.strip_suffix('m'))
    {
        (n, 1024 * 1024)
    } else {
        (lower.as_str(), 1)
    };
    let bytes = num.trim().parse::<u32>()?.checked_mul(mult).unwrap_or(0);
    if bytes < 512 || !bytes.is_power_of_two() {
        bail!("stripe size {s} must be a power of two of at least 512 bytes");
    }
    Ok(bytes)
}

pub fn stripe_code(bytes: u32) -> u8 {
    (bytes.trailing_zeros() - 9) as u8
}

pub struct VolumeRequest<'a> {
    pub level: RaidLevel,
    pub drives: &'a [PdInfo],
    pub drives_per_array: Option<usize>,
    pub stripe_bytes: u32,
    pub name: Option<&'a str>,
    pub init_state: u8,
}

fn next_free<T: Copy + Into<u32> + PartialEq>(used: &[T], limit: u32) -> Option<u32> {
    (0..limit).find(|c| !used.iter().any(|u| (*u).into() == *c))
}

fn array_layout(req: &VolumeRequest<'_>) -> Result<usize> {
    let n = req.drives.len();
    let per = match (req.level.spanned(), req.drives_per_array) {
        (false, None) => n,
        (false, Some(p)) if p == n => n,
        (false, Some(_)) => bail!(
            "{} takes a single array, drop --pd-per-array or use a spanned level",
            req.level.name()
        ),
        (true, None) => bail!("{} needs --pd-per-array", req.level.name()),
        (true, Some(p)) => p,
    };
    req.level.check_drive_count(per)?;
    if !n.is_multiple_of(per) {
        bail!("{n} drives do not split evenly into arrays of {per}");
    }
    let depth = n / per;
    if req.level.spanned() && !(2..=MAX_SPAN_DEPTH).contains(&depth) {
        bail!(
            "{} needs between 2 and {MAX_SPAN_DEPTH} arrays, {n} drives in arrays of {per} make {depth}",
            req.level.name()
        );
    }
    Ok(per)
}

pub fn new_target_id(data: &[u8]) -> u8 {
    data.u8_at(HEADER_LEN + usize::from(data.u16_at(4)) * ARRAY_LEN)
}

pub fn build_volume(current: &ConfigData, req: &VolumeRequest<'_>) -> Result<Vec<u8>> {
    let per = array_layout(req)?;
    if req.init_state > INIT_FULL {
        bail!("init state {} is not 0, 1 or 2", req.init_state);
    }
    for d in req.drives {
        if d.fw_state != STATE_UNCONFIGURED_GOOD {
            bail!(
                "drive {} is {} and not unconfigured good",
                d.address(),
                d.state
            );
        }
        if d.is_foreign {
            bail!("drive {} carries a foreign configuration", d.address());
        }
    }
    let mut seen: Vec<u16> = req.drives.iter().map(|d| d.device_id).collect();
    seen.sort_unstable();
    seen.dedup();
    if seen.len() != req.drives.len() {
        bail!("a drive is listed twice");
    }
    if let Some(name) = req.name
        && (name.len() > 15 || !name.is_ascii())
    {
        bail!("name must be at most 15 ASCII characters");
    }
    let mut used_arrays: Vec<u16> = current.arrays.iter().map(|a| a.array_ref).collect();
    let used_targets: Vec<u8> = current
        .volumes
        .iter()
        .map(|v| v.properties.target_id)
        .collect();
    let Some(target_id) = next_free(&used_targets, 0xff) else {
        bail!("no free target id");
    };
    let groups: Vec<&[PdInfo]> = req.drives.chunks(per).collect();
    let mut arrays = Vec::with_capacity(groups.len());
    for group in &groups {
        let Some(array_ref) = next_free(&used_arrays, 0xffff) else {
            bail!("no free array reference");
        };
        used_arrays.push(array_ref as u16);
        let size = group.iter().map(|d| d.coerced_blocks).min().unwrap_or(0);
        if size == 0 {
            bail!("drive sizes are unknown");
        }
        arrays.push((array_ref as u16, size));
    }

    let depth = groups.len();
    let total = HEADER_LEN + depth * ARRAY_LEN + LD_CONFIG_LEN;
    let mut b = vec![0u8; total];
    b.put_u32(0, total as u32);
    b.put_u16(4, depth as u16);
    b.put_u16(6, ARRAY_LEN as u16);
    b.put_u16(8, 1);
    b.put_u16(10, LD_CONFIG_LEN as u16);
    b.put_u16(12, 0);
    b.put_u16(14, SPARE_LEN as u16);

    for (i, (group, (array_ref, size))) in groups.iter().zip(&arrays).enumerate() {
        let a = HEADER_LEN + i * ARRAY_LEN;
        b.put_u64(a, *size);
        b.put_u8(a + 8, group.len() as u8);
        b.put_u16(a + 10, *array_ref);
        for (j, d) in group.iter().enumerate() {
            let row = a + 32 + j * 8;
            b.put_u16(row, d.device_id);
            b.put_u16(row + 2, d.seq_num);
            b.put_u16(row + 4, STATE_ONLINE);
        }
    }

    let l = HEADER_LEN + depth * ARRAY_LEN;
    let (primary, qualifier, secondary) = req.level.encoding();
    let cache = CACHE_ALLOW_WRITE_CACHE | CACHE_WRITE_BACK;
    b.put_u8(l, target_id as u8);
    b.put_u16(l + 2, 0);
    if let Some(name) = req.name {
        b.put_bytes(l + 4, name.as_bytes());
    }
    b.put_u8(l + 20, cache);
    b.put_u8(l + 21, 0);
    b.put_u8(l + 22, 0);
    b.put_u8(l + 23, cache);
    b.put_u8(l + 24, 0);
    b.put_u8(l + 32, primary);
    b.put_u8(l + 33, qualifier);
    b.put_u8(l + 34, secondary);
    b.put_u8(l + 35, stripe_code(req.stripe_bytes));
    b.put_u8(l + 36, per as u8);
    b.put_u8(l + 37, depth as u8);
    b.put_u8(l + 38, 3);
    b.put_u8(l + 39, req.init_state);
    b.put_u8(l + 40, 0);
    for (i, (array_ref, size)) in arrays.iter().enumerate() {
        let s = l + 64 + i * 24;
        b.put_u64(s, 0);
        b.put_u64(s + 8, *size);
        b.put_u16(s + 16, *array_ref);
    }
    Ok(b)
}

pub fn build_spare(
    current: &ConfigData,
    drive: &PdInfo,
    dedicated_to: Option<u8>,
    extra_flags: u8,
) -> Result<Vec<u8>> {
    if extra_flags & !(SPARE_REVERTIBLE | SPARE_ENCL_AFFINITY) != 0 {
        bail!("unknown spare flags {extra_flags:#04x}");
    }
    if drive.fw_state != STATE_UNCONFIGURED_GOOD {
        bail!(
            "drive {} is {} and not unconfigured good",
            drive.address(),
            drive.state
        );
    }
    let arrays: Vec<&Array> = match dedicated_to {
        None => current.arrays.iter().collect(),
        Some(target) => {
            let Some(ld) = current.volume(target) else {
                bail!("volume {target} does not exist");
            };
            ld.spans
                .iter()
                .map(|s| {
                    current.array(s.array_ref).ok_or_else(|| {
                        anyhow::anyhow!("volume {target} points at missing array {}", s.array_ref)
                    })
                })
                .collect::<Result<_>>()?
        }
    };
    for a in &arrays {
        if a.size_blocks > drive.coerced_blocks {
            bail!(
                "drive {} is too small for array {}",
                drive.address(),
                a.array_ref
            );
        }
    }
    let listed: &[&Array] = if dedicated_to.is_some() { &arrays } else { &[] };
    let mut b = vec![0u8; SPARE_LEN + 2 * listed.len()];
    b.put_u16(0, drive.device_id);
    b.put_u16(2, drive.seq_num);
    let kind = if dedicated_to.is_some() {
        SPARE_DEDICATED
    } else {
        0
    };
    b.put_u8(4, kind | extra_flags);
    b.put_u8(7, listed.len() as u8);
    for (i, a) in listed.iter().enumerate() {
        b.put_u16(8 + i * 2, a.array_ref);
    }
    Ok(b)
}

pub fn read_raw(t: &dyn Transport) -> Result<Vec<u8>> {
    dcmd_variable(t, op::CFG_READ, &Mbox::new())
}

pub fn read(t: &dyn Transport) -> Result<ConfigData> {
    Ok(ConfigData::parse(&read_raw(t)?))
}

pub fn add(t: &dyn Transport, data: &[u8]) -> Result<()> {
    dcmd_write(t, op::CFG_ADD, &Mbox::new(), data)
}

pub fn clear(t: &dyn Transport) -> Result<()> {
    dcmd_none(t, op::CFG_CLEAR, &Mbox::new())
}

pub fn make_spare(t: &dyn Transport, data: &[u8]) -> Result<()> {
    dcmd_write(t, op::CFG_MAKE_SPARE, &Mbox::new(), data)
}

pub fn remove_spare(t: &dyn Transport, device_id: u16, seq_num: u16) -> Result<()> {
    dcmd_none(t, op::CFG_REMOVE_SPARE, &Mbox::pd_ref(device_id, seq_num))
}

pub fn foreign_count(t: &dyn Transport) -> Result<u32> {
    let buf = dcmd_read(t, op::CFG_FOREIGN_SCAN, &Mbox::new(), FOREIGN_SCAN_LEN)?;
    Ok(buf.u32_at(0).min(MAX_FOREIGN_CONFIGS))
}

pub fn foreign_display(t: &dyn Transport, index: u8) -> Result<ConfigData> {
    let raw = dcmd_variable(t, op::CFG_FOREIGN_DISPLAY, &Mbox::new().byte(0, index))?;
    Ok(ConfigData::parse(&raw))
}

pub fn foreign_preview(t: &dyn Transport, index: u8) -> Result<ConfigData> {
    let raw = dcmd_variable(t, op::CFG_FOREIGN_PREVIEW, &Mbox::new().byte(0, index))?;
    Ok(ConfigData::parse(&raw))
}

pub fn foreign_import_all(t: &dyn Transport) -> Result<()> {
    dcmd_none(t, op::CFG_FOREIGN_IMPORT, &Mbox::new().byte(0, FOREIGN_ALL))
}

pub fn foreign_import(t: &dyn Transport, index: u8) -> Result<()> {
    let count = foreign_count(t)?;
    if u32::from(index) >= count {
        bail!("foreign configuration {index} does not exist, the scan found {count}");
    }
    dcmd_none(t, op::CFG_FOREIGN_IMPORT, &Mbox::new().byte(0, index))
}

pub fn foreign_clear(t: &dyn Transport) -> Result<()> {
    dcmd_none(t, op::CFG_FOREIGN_CLEAR, &Mbox::new())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::mega::ld::tests::ld_config_bytes;
    use crate::mega::mock::Mock;
    use crate::mega::pd::STATE_ONLINE;
    use crate::mega::pd::tests::pd_info_bytes;

    pub fn array_bytes(array_ref: u16, size: u64, members: &[(u16, u16, u8)]) -> Vec<u8> {
        let mut b = vec![0u8; ARRAY_LEN];
        b.put_u64(0, size);
        b[8] = members.len() as u8;
        b.put_u16(10, array_ref);
        for (i, (dev, state, slot)) in members.iter().enumerate() {
            let at = 32 + i * 8;
            b.put_u16(at, *dev);
            b.put_u16(at + 2, 1);
            b.put_u16(at + 4, *state);
            b[at + 7] = *slot;
        }
        b
    }

    pub fn config_bytes(arrays: &[Vec<u8>], lds: &[Vec<u8>], spares: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0u8; HEADER_LEN];
        b.put_u16(4, arrays.len() as u16);
        b.put_u16(6, ARRAY_LEN as u16);
        b.put_u16(8, lds.len() as u16);
        b.put_u16(10, LD_CONFIG_LEN as u16);
        b.put_u16(12, spares.len() as u16);
        b.put_u16(14, SPARE_LEN as u16);
        for r in arrays.iter().chain(lds).chain(spares) {
            b.extend_from_slice(r);
        }
        let n = b.len() as u32;
        b.put_u32(0, n);
        b
    }

    fn spare_bytes(dev: u16, kind: u8, arrays: &[u16]) -> Vec<u8> {
        let mut b = vec![0u8; SPARE_LEN];
        b.put_u16(0, dev);
        b[4] = kind;
        b[7] = arrays.len() as u8;
        for (i, a) in arrays.iter().enumerate() {
            b.put_u16(8 + i * 2, *a);
        }
        b
    }

    fn sample() -> Vec<u8> {
        config_bytes(
            &[
                array_bytes(0, 1000, &[(8, STATE_ONLINE, 0), (9, STATE_ONLINE, 1)]),
                array_bytes(2, 2000, &[(0xffff, 0, 0), (11, STATE_ONLINE, 3)]),
            ],
            &[
                ld_config_bytes(0, DDF_RAID1, 0, 2, &[(1000, 0)]),
                ld_config_bytes(1, DDF_RAID1, 0, 2, &[(2000, 2)]),
            ],
            &[spare_bytes(12, SPARE_DEDICATED | SPARE_REVERTIBLE, &[2])],
        )
    }

    #[test]
    fn parses_config_using_the_reported_record_sizes() {
        let cfg = ConfigData::parse(&sample());
        assert_eq!(cfg.arrays.len(), 2);
        assert_eq!(cfg.arrays[1].array_ref, 2);
        assert!(cfg.arrays[1].members[0].missing);
        assert_eq!(cfg.arrays[0].members[1].slot, 1);
        assert_eq!(cfg.volumes.len(), 2);
        assert_eq!(cfg.volumes[1].spans[0].array_ref, 2);
        assert_eq!(cfg.spares.len(), 1);
        assert!(cfg.spares[0].dedicated && cfg.spares[0].revertible);
        assert_eq!(cfg.spares[0].array_refs, vec![2]);
        assert_eq!(cfg.array_index_of(11), Some(1));
        let drives: Vec<u16> = cfg.volume_drives(0).iter().map(|m| m.device_id).collect();
        assert_eq!(drives, vec![8, 9]);
    }

    #[test]
    fn parses_nonstandard_record_sizes() {
        let mut b = vec![0u8; HEADER_LEN];
        b.put_u16(4, 1);
        b.put_u16(6, (ARRAY_LEN + 16) as u16);
        b.extend_from_slice(&array_bytes(5, 10, &[(1, STATE_ONLINE, 0)]));
        b.extend_from_slice(&[0u8; 16]);
        let cfg = ConfigData::parse(&b);
        assert_eq!(cfg.arrays[0].array_ref, 5);
    }

    fn ugood(dev: u16, slot: u8, blocks: u64) -> PdInfo {
        let mut raw = pd_info_bytes(dev, dev + 100, STATE_UNCONFIGURED_GOOD, 252, slot, 30);
        raw.put_u16(188, 0);
        raw.put_u64(248, blocks);
        PdInfo::parse(&raw)
    }

    #[test]
    fn builds_a_raid5_volume_like_mfiutil() {
        let current = ConfigData::parse(&sample());
        let drives = [ugood(20, 4, 5000), ugood(21, 5, 4000), ugood(22, 6, 6000)];
        let req = VolumeRequest {
            level: RaidLevel::Raid5,
            drives: &drives,
            drives_per_array: None,
            stripe_bytes: 256 * 1024,
            name: Some("scratch"),
            init_state: INIT_NONE,
        };
        let b = build_volume(&current, &req).unwrap();
        assert_eq!(b.len(), 32 + 288 + 256);
        assert_eq!(b.u32_at(0), 576);
        assert_eq!(
            (
                b.u16_at(4),
                b.u16_at(6),
                b.u16_at(8),
                b.u16_at(10),
                b.u16_at(12),
                b.u16_at(14)
            ),
            (1, 288, 1, 256, 0, 40)
        );
        assert_eq!(b.u64_at(32), 4000);
        assert_eq!(b[40], 3);
        assert_eq!(b.u16_at(42), 1);
        assert_eq!(b.u16_at(64), 20);
        assert_eq!(b.u16_at(66), 120);
        assert_eq!(b.u16_at(68), STATE_ONLINE);
        assert_eq!(b.u16_at(72), 21);
        let l = 32 + 288;
        assert_eq!(b[l], 2);
        assert_eq!(b.ascii_at(l + 4, 16), "scratch");
        assert_eq!((b[l + 20], b[l + 23]), (0x21, 0x21));
        assert_eq!((b[l + 32], b[l + 33], b[l + 34]), (0x05, 3, 0));
        assert_eq!(b[l + 35], 9);
        assert_eq!((b[l + 36], b[l + 37], b[l + 38]), (3, 1, 3));
        assert_eq!(b.u64_at(l + 64), 0);
        assert_eq!(b.u64_at(l + 72), 4000);
        assert_eq!(b.u16_at(l + 80), 1);
        let parsed = ConfigData::parse(&b);
        assert_eq!(parsed.volumes[0].params.raid, "RAID5");
    }

    #[test]
    fn volume_validation_rejects_bad_requests() {
        let current = ConfigData::parse(&config_bytes(&[], &[], &[]));
        let three = [ugood(1, 0, 10), ugood(2, 1, 10), ugood(3, 2, 10)];
        let req = |level, drives| VolumeRequest {
            level,
            drives,
            drives_per_array: None,
            stripe_bytes: 65536,
            name: None,
            init_state: INIT_NONE,
        };
        assert!(build_volume(&current, &req(RaidLevel::Raid1, &three)).is_err());
        assert!(build_volume(&current, &req(RaidLevel::Raid6, &three)).is_err());
        let mut busy = three.clone();
        busy[0].fw_state = STATE_ONLINE;
        assert!(build_volume(&current, &req(RaidLevel::Raid0, &busy)).is_err());
        let dup = [ugood(1, 0, 10), ugood(1, 0, 10)];
        assert!(build_volume(&current, &req(RaidLevel::Raid1, &dup)).is_err());
        let b = build_volume(&current, &req(RaidLevel::Raid0, &three)).unwrap();
        assert_eq!(b[32 + 288], 0);
        assert_eq!(new_target_id(&b), 0);
        assert_eq!(b.u16_at(32 + 10), 0);
        let mut split = req(RaidLevel::Raid0, &three);
        split.drives_per_array = Some(1);
        assert!(build_volume(&current, &split).is_err());
        let mut bad_init = req(RaidLevel::Raid0, &three);
        bad_init.init_state = 3;
        assert!(build_volume(&current, &bad_init).is_err());
    }

    fn drives(n: u16) -> Vec<PdInfo> {
        (0..n)
            .map(|i| ugood(40 + i, i as u8, 9000 - u64::from(i) * 10))
            .collect()
    }

    fn spanned(level: RaidLevel, drives: &[PdInfo], per: Option<usize>) -> VolumeRequest<'_> {
        VolumeRequest {
            level,
            drives,
            drives_per_array: per,
            stripe_bytes: 64 * 1024,
            name: Some("span"),
            init_state: INIT_FULL,
        }
    }

    #[test]
    fn builds_a_raid10_volume_with_one_array_per_span() {
        let current = ConfigData::parse(&sample());
        let d = drives(4);
        let b = build_volume(&current, &spanned(RaidLevel::Raid10, &d, Some(2))).unwrap();
        let l = 32 + 2 * 288;
        assert_eq!(b.len(), l + 256);
        assert_eq!(b.u32_at(0), (l + 256) as u32);
        assert_eq!((b.u16_at(4), b.u16_at(6), b.u16_at(8)), (2, 288, 1));
        assert_eq!(b.u64_at(32), 8990);
        assert_eq!(b[32 + 8], 2);
        assert_eq!(b.u16_at(32 + 10), 1);
        assert_eq!((b.u16_at(64), b.u16_at(72)), (40, 41));
        let a1 = 32 + 288;
        assert_eq!(b.u64_at(a1), 8970);
        assert_eq!(b[a1 + 8], 2);
        assert_eq!(b.u16_at(a1 + 10), 3);
        assert_eq!((b.u16_at(a1 + 32), b.u16_at(a1 + 40)), (42, 43));
        assert_eq!(b.u16_at(a1 + 36), STATE_ONLINE);
        assert_eq!(b[l], 2);
        assert_eq!(new_target_id(&b), 2);
        assert_eq!((b[l + 32], b[l + 33], b[l + 34]), (0x01, 0, 3));
        assert_eq!(b[l + 35], 7);
        assert_eq!((b[l + 36], b[l + 37], b[l + 38], b[l + 39]), (2, 2, 3, 2));
        assert_eq!(
            (b.u64_at(l + 64), b.u64_at(l + 72), b.u16_at(l + 80)),
            (0, 8990, 1)
        );
        assert_eq!(
            (b.u64_at(l + 88), b.u64_at(l + 96), b.u16_at(l + 104)),
            (0, 8970, 3)
        );
        assert!(b[l + 112..].iter().all(|v| *v == 0));
        let parsed = ConfigData::parse(&b);
        assert_eq!(parsed.volumes[0].params.raid, "RAID10");
        assert_eq!(parsed.volumes[0].spans.len(), 2);
        assert_eq!(parsed.volume_drives(2).len(), 4);
    }

    #[test]
    fn builds_raid50_and_raid60_with_secondary_level_three() {
        let current = ConfigData::parse(&config_bytes(&[], &[], &[]));
        let six = drives(6);
        let b = build_volume(&current, &spanned(RaidLevel::Raid50, &six, Some(3))).unwrap();
        let l = 32 + 2 * 288;
        assert_eq!((b[l + 32], b[l + 33], b[l + 34]), (0x05, 3, 3));
        assert_eq!((b[l + 36], b[l + 37]), (3, 2));
        assert_eq!((b.u16_at(32 + 10), b.u16_at(32 + 288 + 10)), (0, 1));
        let twelve = drives(12);
        let b = build_volume(&current, &spanned(RaidLevel::Raid60, &twelve, Some(4))).unwrap();
        let l = 32 + 3 * 288;
        assert_eq!(b.len(), l + 256);
        assert_eq!((b[l + 32], b[l + 33], b[l + 34]), (0x06, 3, 3));
        assert_eq!((b[l + 36], b[l + 37]), (4, 3));
        assert_eq!(b.u16_at(l + 64 + 2 * 24 + 16), 2);
        assert_eq!(ConfigData::parse(&b).volumes[0].params.raid, "RAID60");
        let b = build_volume(&current, &spanned(RaidLevel::Raid6, &drives(4), None)).unwrap();
        assert_eq!(b[32 + 288 + 32], 0x06);
    }

    #[test]
    fn spanned_layouts_are_validated() {
        let current = ConfigData::parse(&config_bytes(&[], &[], &[]));
        let four = drives(4);
        let err = |level, d: &[PdInfo], per| {
            build_volume(&current, &spanned(level, d, per))
                .unwrap_err()
                .to_string()
        };
        assert!(err(RaidLevel::Raid10, &four, None).contains("--pd-per-array"));
        assert!(err(RaidLevel::Raid10, &four, Some(4)).contains("between 2 and 8"));
        assert!(err(RaidLevel::Raid10, &drives(6), Some(3)).contains("even"));
        assert!(err(RaidLevel::Raid10, &drives(5), Some(2)).contains("evenly"));
        assert!(err(RaidLevel::Raid50, &four, Some(2)).contains("at least 3"));
        assert!(err(RaidLevel::Raid60, &drives(6), Some(3)).contains("at least 4"));
        assert!(err(RaidLevel::Raid10, &drives(18), Some(2)).contains("between 2 and 8"));
        assert!(build_volume(&current, &spanned(RaidLevel::Raid10, &drives(16), Some(2))).is_ok());
    }

    #[test]
    fn raid_levels_and_stripes_parse() {
        assert_eq!(RaidLevel::parse("raid5").unwrap(), RaidLevel::Raid5);
        assert_eq!(RaidLevel::parse("RAID10").unwrap(), RaidLevel::Raid10);
        assert_eq!(RaidLevel::parse("50").unwrap(), RaidLevel::Raid50);
        assert_eq!(RaidLevel::parse("60").unwrap().ctrl_capability(), "RAID6");
        assert!(RaidLevel::parse("00").is_err());
        assert!(RaidLevel::parse("1e").is_err());
        assert_eq!(parse_stripe("64k").unwrap(), 65536);
        assert_eq!(parse_stripe("1m").unwrap(), 1 << 20);
        assert!(parse_stripe("100k").is_err());
        assert_eq!(stripe_code(65536), 7);
    }

    #[test]
    fn builds_global_and_dedicated_spares() {
        let current = ConfigData::parse(&sample());
        let drive = ugood(30, 7, 5000);
        let global = build_spare(&current, &drive, None, 0).unwrap();
        assert_eq!(global.len(), 40);
        assert_eq!(
            (global.u16_at(0), global.u16_at(2), global[4], global[7]),
            (30, 130, 0, 0)
        );
        let dedicated = build_spare(&current, &drive, Some(1), 0).unwrap();
        assert_eq!(dedicated.len(), 42);
        assert_eq!(
            (dedicated[4], dedicated[7], dedicated.u16_at(8)),
            (SPARE_DEDICATED, 1, 2)
        );
        let small = ugood(31, 8, 10);
        assert!(build_spare(&current, &small, None, 0).is_err());
        assert!(build_spare(&current, &drive, Some(9), 0).is_err());
    }

    #[test]
    fn spare_flags_land_in_spare_type() {
        let current = ConfigData::parse(&sample());
        let drive = ugood(30, 7, 5000);
        let b = build_spare(&current, &drive, Some(1), SPARE_REVERTIBLE).unwrap();
        assert_eq!(b[4], 0x03);
        assert_eq!((b[7], b.u16_at(8)), (1, 2));
        let b = build_spare(&current, &drive, None, SPARE_ENCL_AFFINITY).unwrap();
        assert_eq!((b[4], b[7], b.len()), (0x04, 0, 40));
        let b = build_spare(
            &current,
            &drive,
            Some(1),
            SPARE_REVERTIBLE | SPARE_ENCL_AFFINITY,
        )
        .unwrap();
        assert_eq!(b[4], 0x07);
        assert_eq!(Spare::parse(&b).array_refs, vec![2]);
        assert!(build_spare(&current, &drive, None, SPARE_DEDICATED).is_err());
        assert!(build_spare(&current, &drive, None, 0x08).is_err());
    }

    #[test]
    fn foreign_import_by_index_puts_the_index_in_b0() {
        let mut scan = vec![0u8; FOREIGN_SCAN_LEN];
        scan.put_u32(0, 3);
        let mock = Mock::new()
            .reply(op::CFG_FOREIGN_SCAN, scan)
            .reply(op::CFG_FOREIGN_IMPORT, vec![]);
        foreign_import(&mock, 2).unwrap();
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        let import = &calls[1];
        assert_eq!(import.opcode(), 0x0406_0400);
        assert_eq!(import.mbox(), &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(import.bufs.is_empty());
        assert_eq!(import.frame[7], 0);
        assert_eq!(import.frame.u16_at(0x10), 0);
        assert!(foreign_import(&mock, 3).is_err());
        assert_eq!(mock.calls().len(), 3);
    }

    #[test]
    fn foreign_and_spare_commands_use_documented_mailboxes() {
        let mut scan = vec![0u8; FOREIGN_SCAN_LEN];
        scan.put_u32(0, 2);
        let mock = Mock::new()
            .reply(op::CFG_FOREIGN_SCAN, scan)
            .reply(op::CFG_FOREIGN_IMPORT, vec![])
            .reply(op::CFG_FOREIGN_CLEAR, vec![])
            .reply(op::CFG_REMOVE_SPARE, vec![])
            .reply_mbox(op::CFG_FOREIGN_PREVIEW, &[1], config_bytes(&[], &[], &[]));
        assert_eq!(foreign_count(&mock).unwrap(), 2);
        foreign_import_all(&mock).unwrap();
        foreign_clear(&mock).unwrap();
        remove_spare(&mock, 0x0102, 0x0304).unwrap();
        foreign_preview(&mock, 1).unwrap();
        let calls = mock.calls();
        assert_eq!(calls[1].mbox()[0], 0xff);
        assert_eq!(calls[3].mbox()[..4], [0x02, 0x01, 0x04, 0x03]);
        assert_eq!(calls[4].opcode(), 0x0406_0300);
        assert_eq!(calls[4].mbox()[0], 1);
    }
}
