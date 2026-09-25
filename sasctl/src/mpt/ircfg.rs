use anyhow::{Result, anyhow, bail};
use clap::ValueEnum;
use serde::Serialize;

use super::config::{
    self, IOC_6, LOG_0, MANUFACTURING_4, RAID_CONFIG_0, RAID_CONFIG_FORM_ACTIVE,
    RAID_CONFIG_FORM_CONFIGNUM, RAID_VOLUME_0, RAID_VOLUME_FORM_HANDLE,
};
use super::inventory::{self, DriveAddress};
use super::pages::{
    Ioc6, MAN4_MIX_SSD_AND_NON_SSD, MAN4_MIX_SSD_SAS_SATA, MAN4_NO_MIX_SAS_SATA, Manufacturing4,
    RAIDCONFIG_ELEMENT_HOT_SPARE, RAIDCONFIG_ELEMENT_VOLUME, RaidConfig0,
};
use super::raid::{self, VolumeCreation};
use super::transport::Transport;
use crate::bytes::{Le, LeMut};
use crate::output::{Fields, Render};

const CAP_RAID0: u32 = 0x0000_0002;
const CAP_RAID1E: u32 = 0x0000_0004;
const CAP_RAID1: u32 = 0x0000_0008;
const CAP_RAID10: u32 = 0x0000_0010;

const MAX_VOLUME_MB: u64 = 1 << 53;
const DEFAULT_STRIPE_KB: u32 = 128;
const MAX_POOL: u8 = 7;
const SETTINGS_POOL_MASK: u32 = 0x00FF_0000;
const LOG_NUM_ENTRIES: usize = 0x10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Raid0,
    Raid1,
    Raid1e,
    Raid10,
}

impl Level {
    fn volume_type(self) -> u8 {
        match self {
            Level::Raid0 => raid::VOL_TYPE_RAID0,
            Level::Raid1 => raid::VOL_TYPE_RAID1,
            Level::Raid1e => raid::VOL_TYPE_RAID1E,
            Level::Raid10 => raid::VOL_TYPE_RAID10,
        }
    }

    pub fn name(self) -> &'static str {
        raid::volume_type_name(self.volume_type())
    }

    fn capability(self) -> u32 {
        match self {
            Level::Raid0 => CAP_RAID0,
            Level::Raid1 => CAP_RAID1,
            Level::Raid1e => CAP_RAID1E,
            Level::Raid10 => CAP_RAID10,
        }
    }

    fn drive_range(self, ioc6: &Ioc6) -> (u8, u8) {
        match self {
            Level::Raid0 => (ioc6.min_drives_raid0, ioc6.max_drives_raid0),
            Level::Raid1 => (ioc6.min_drives_raid1, ioc6.max_drives_raid1),
            Level::Raid1e => (ioc6.min_drives_raid1e, ioc6.max_drives_raid1e),
            Level::Raid10 => (ioc6.min_drives_raid10, ioc6.max_drives_raid10),
        }
    }

    fn settings(self, man4: &Manufacturing4) -> u32 {
        match self {
            Level::Raid0 => man4.raid0_volume_settings,
            Level::Raid1 => man4.raid1_volume_settings,
            Level::Raid1e => man4.raid1e_volume_settings,
            Level::Raid10 => man4.raid10_volume_settings,
        }
    }

    fn stripe_map(self, ioc6: &Ioc6) -> Option<u32> {
        match self {
            Level::Raid0 => Some(ioc6.stripe_map_raid0),
            Level::Raid1 => None,
            Level::Raid1e => Some(ioc6.stripe_map_raid1e),
            Level::Raid10 => Some(ioc6.stripe_map_raid10),
        }
    }
}

pub struct CreateSpec {
    pub level: Level,
    pub members: Vec<DriveAddress>,
    pub size_mb: Option<u64>,
    pub name: Option<String>,
    pub stripe_kb: Option<u32>,
    pub pool: u8,
}

#[derive(Clone, Debug, Serialize)]
pub struct VolumePlan {
    pub raid_level: &'static str,
    pub members: Vec<String>,
    pub member_handles: Vec<u16>,
    pub size_mb: u64,
    pub max_size_mb: u64,
    pub stripe_kb: u32,
    pub name: String,
    pub hot_spare_pool: u8,
    #[serde(skip)]
    pub creation: VolumeCreation,
}

impl Render for VolumePlan {
    fn render(&self, out: &mut String) {
        let mut f = Fields::titled("Volume created");
        f.add("RAID level", self.raid_level)
            .add("Members", self.members.join(" "))
            .add("Size", format!("{} MB", self.size_mb))
            .add("Stripe size", format!("{} KB", self.stripe_kb))
            .add("Name", &self.name)
            .add("Hot spare pool", self.hot_spare_pool);
        f.render(out);
        out.push_str("Run `volume list` to see the new volume id\n");
    }
}

pub fn check_pool(pool: u8) -> Result<()> {
    if pool > MAX_POOL {
        bail!("hot spare pool must be 0 to {MAX_POOL}, got {pool}");
    }
    Ok(())
}

pub fn active_config(t: &dyn Transport) -> Result<Option<RaidConfig0>> {
    Ok(
        config::read_page(t, RAID_CONFIG_0, RAID_CONFIG_FORM_ACTIVE)?
            .map(|p| RaidConfig0::parse(&p)),
    )
}

fn counts(t: &dyn Transport) -> Result<(usize, usize, usize)> {
    Ok(active_config(t)?.map_or((0, 0, 0), |c| {
        (
            c.num_volumes as usize,
            c.num_phys_disks as usize,
            c.num_hot_spares as usize,
        )
    }))
}

pub fn stripe_blocks(map: u32, requested_kb: Option<u32>) -> Result<u32> {
    if map == 0 {
        if requested_kb.is_some() {
            bail!("the controller reports no supported stripe sizes for this level");
        }
        return Ok(0);
    }
    let min_kb = (1u32 << map.trailing_zeros()) / 2;
    let max_kb = (1u32 << (31 - map.leading_zeros())) / 2;
    let supported =
        |kb: u32| kb.is_power_of_two() && kb.checked_mul(2).is_some_and(|blocks| map & blocks != 0);
    let kb = match requested_kb {
        Some(kb) if supported(kb) => kb,
        Some(kb) => {
            let sizes: Vec<String> = (0..32)
                .filter(|bit| map & (1 << bit) != 0)
                .map(|bit| format!("{}", (1u64 << bit) / 2))
                .collect();
            bail!(
                "stripe size {kb} KB is not supported, the controller allows {} KB",
                sizes.join(", ")
            );
        }
        None if map.is_power_of_two() => min_kb,
        None => DEFAULT_STRIPE_KB.max(min_kb).min(max_kb),
    };
    Ok(kb * 2)
}

struct Candidate {
    handle: u16,
    sata: bool,
    ssd: bool,
    size_mb: u64,
}

fn check_mixing(flags: u32, members: &[Candidate]) -> Result<()> {
    let Some(first) = members.first() else {
        return Ok(());
    };
    for m in &members[1..] {
        if flags & MAN4_NO_MIX_SAS_SATA != 0 && m.sata != first.sata {
            bail!("this controller does not allow mixing SAS and SATA drives in a volume");
        }
        if flags & MAN4_MIX_SSD_AND_NON_SSD == 0 && m.ssd != first.ssd {
            bail!("this controller does not allow mixing SSD and HDD drives in a volume");
        }
        if flags & MAN4_MIX_SSD_SAS_SATA == 0 && m.ssd && first.ssd && m.sata != first.sata {
            bail!("this controller does not allow mixing SAS and SATA SSDs in a volume");
        }
    }
    Ok(())
}

pub fn plan_volume(t: &dyn Transport, spec: &CreateSpec) -> Result<VolumePlan> {
    let name = spec.name.clone().unwrap_or_default();
    if !name.is_ascii() || name.len() > raid::VOLUME_NAME_MAX {
        bail!(
            "volume name must be ASCII and at most {} characters",
            raid::VOLUME_NAME_MAX
        );
    }
    check_pool(spec.pool)?;
    for (i, m) in spec.members.iter().enumerate() {
        if spec.members[..i].contains(m) {
            bail!("{m} is listed more than once");
        }
    }
    let n = spec.members.len();
    let level = spec.level;

    let ioc6 = Ioc6::parse(&config::require_page(t, IOC_6, 0)?);
    let man4 = Manufacturing4::parse(&config::require_page(t, MANUFACTURING_4, 0)?);
    if ioc6.capabilities_flags & level.capability() == 0 {
        bail!("this controller does not support {}", level.name());
    }
    let (nv, nd, _) = counts(t)?;
    let max_volumes = ioc6.max_volumes.min(man4.max_volumes) as usize;
    if nv >= max_volumes {
        bail!("the controller already has {nv} of at most {max_volumes} volumes");
    }
    let max_disks = ioc6.max_phys_disks.min(man4.max_phys_disks) as usize;
    if nd + n > max_disks {
        bail!(
            "{nd} physical disks are already configured and the controller allows at most {max_disks}"
        );
    }
    if n > man4.max_phys_disks_per_vol as usize {
        bail!(
            "a volume can have at most {} physical disks",
            man4.max_phys_disks_per_vol
        );
    }
    let (min, max) = level.drive_range(&ioc6);
    if level == Level::Raid1 && n != 2 {
        bail!("RAID1 needs exactly 2 drives, got {n}");
    }
    if n < min as usize || n > max as usize {
        bail!("{} needs {min} to {max} drives, got {n}", level.name());
    }
    if level == Level::Raid10 && !n.is_multiple_of(2) {
        bail!("RAID10 needs an even number of drives, got {n}");
    }

    let mut members = Vec::with_capacity(n);
    for address in &spec.members {
        let dev = inventory::find_device(t, *address)?;
        if !dev.is_disk() {
            bail!("{address} is not a disk");
        }
        let pd = inventory::physdisk_by_handle(t, dev.dev_handle)?
            .ok_or_else(|| anyhow!("{address} is not known to the Integrated RAID firmware"))?;
        if pd.phys_disk_state != raid::PD_STATE_NOT_CONFIGURED {
            bail!(
                "{address} is not an unconfigured disk, its state is {}",
                raid::drive_state(Some(&pd), true)
            );
        }
        if pd.block_size != 0 && pd.block_size != 512 {
            bail!(
                "{address} has {} byte blocks, and volume sizing is only known for 512 byte blocks",
                pd.block_size
            );
        }
        members.push(Candidate {
            handle: dev.dev_handle,
            sata: dev.is_sata(),
            ssd: pd.is_ssd(),
            size_mb: pd.coerced_max_lba.saturating_add(1) / 2048,
        });
    }
    check_mixing(man4.flags, &members)?;

    let smallest = members.iter().map(|m| m.size_mb).min().unwrap_or(0);
    let mut max_size_mb = smallest * n as u64;
    if level != Level::Raid0 {
        max_size_mb /= 2;
    }
    max_size_mb = max_size_mb.min(MAX_VOLUME_MB);
    if max_size_mb == 0 {
        bail!("the selected drives leave no usable capacity");
    }
    let size_mb = spec.size_mb.unwrap_or(max_size_mb);
    if size_mb == 0 || size_mb > max_size_mb {
        bail!("volume size must be 1 to {max_size_mb} MB, got {size_mb}");
    }

    let stripe = match level.stripe_map(&ioc6) {
        Some(map) => stripe_blocks(map, spec.stripe_kb)?,
        None if spec.stripe_kb.is_some() => bail!("RAID1 has no stripe size"),
        None => 0,
    };

    let settings = (level.settings(&man4) & !SETTINGS_POOL_MASK) | ((1u32 << spec.pool) << 16);
    let mut flags = raid::CREATION_DEFAULT_SETTINGS;
    if level == Level::Raid1 {
        flags |= raid::CREATION_MIGRATE_DATA;
    }
    if level != Level::Raid0 {
        flags |= raid::CREATION_BACKGROUND_INIT;
    }
    let handles: Vec<u16> = members.iter().map(|m| m.handle).collect();
    let creation = VolumeCreation {
        volume_type: level.volume_type(),
        flags,
        settings,
        resync_rate: man4.resync_rate,
        data_scrub_duration: man4.data_scrub_duration,
        max_lba: size_mb * 2048 - 1,
        stripe_blocks: stripe,
        name: name.clone(),
        members: handles.clone(),
    };
    Ok(VolumePlan {
        raid_level: level.name(),
        members: spec.members.iter().map(ToString::to_string).collect(),
        member_handles: handles,
        size_mb,
        max_size_mb,
        stripe_kb: stripe / 2,
        name,
        hot_spare_pool: spec.pool,
        creation,
    })
}

pub fn create_volume(t: &dyn Transport, spec: &CreateSpec) -> Result<VolumePlan> {
    let plan = plan_volume(t, spec)?;
    raid::create_volume_request(&plan.creation.encode()).send_checked(t, "CREATE_VOLUME")?;
    Ok(plan)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Deleted {
    pub volumes: Vec<u16>,
    pub hot_spares: Vec<u8>,
    pub zero_lba0: bool,
}

impl Render for Deleted {
    fn render(&self, out: &mut String) {
        if self.volumes.is_empty() && self.hot_spares.is_empty() {
            out.push_str("Nothing to delete\n");
            return;
        }
        for v in &self.volumes {
            out.push_str(&format!("volume {v} deleted\n"));
        }
        for s in &self.hot_spares {
            out.push_str(&format!("hot spare (phys disk {s}) deleted\n"));
        }
    }
}

pub fn delete_volume(t: &dyn Transport, id: u16, zero_lba0: bool) -> Result<Deleted> {
    if config::read_page(t, RAID_VOLUME_0, RAID_VOLUME_FORM_HANDLE | id as u32)?.is_none() {
        bail!("no volume {id}, see `volume list`");
    }
    raid::delete_volume_request(id, zero_lba0).send_checked(t, "DELETE_VOLUME")?;
    Ok(Deleted {
        volumes: vec![id],
        hot_spares: Vec::new(),
        zero_lba0,
    })
}

pub fn delete_all(t: &dyn Transport, zero_lba0: bool) -> Result<Deleted> {
    let Some(active) = active_config(t)? else {
        bail!("there is no active Integrated RAID configuration");
    };
    let mut done = Deleted {
        zero_lba0,
        ..Default::default()
    };
    for e in active.elements_of(RAIDCONFIG_ELEMENT_VOLUME) {
        raid::delete_volume_request(e.vol_dev_handle, zero_lba0)
            .send_checked(t, "DELETE_VOLUME")?;
        done.volumes.push(e.vol_dev_handle);
    }
    let address = RAID_CONFIG_FORM_CONFIGNUM | active.config_num as u32;
    let Some(after) = config::read_page(t, RAID_CONFIG_0, address)?.map(|p| RaidConfig0::parse(&p))
    else {
        return Ok(done);
    };
    if after.num_volumes > 0 {
        bail!(
            "{} volumes are still configured after deleting, hot spares were left in place",
            after.num_volumes
        );
    }
    for e in after.elements_of(RAIDCONFIG_ELEMENT_HOT_SPARE) {
        raid::physdisk_action_request(raid::ACTION_DELETE_HOT_SPARE, e.phys_disk_num)
            .send_checked(t, "DELETE_HOT_SPARE")?;
        done.hot_spares.push(e.phys_disk_num);
    }
    Ok(done)
}

pub fn add_hot_spare(t: &dyn Transport, address: DriveAddress, pool: u8) -> Result<u16> {
    check_pool(pool)?;
    let ioc6 = Ioc6::parse(&config::require_page(t, IOC_6, 0)?);
    let (_, nd, ns) = counts(t)?;
    if nd >= ioc6.max_phys_disks as usize {
        bail!(
            "the controller already has {nd} of at most {} physical disks",
            ioc6.max_phys_disks
        );
    }
    if ns >= ioc6.max_global_hot_spares as usize {
        bail!(
            "the controller already has {ns} of at most {} hot spares",
            ioc6.max_global_hot_spares
        );
    }
    let dev = inventory::find_device(t, address)?;
    if !dev.is_disk() {
        bail!("{address} is not a disk");
    }
    if let Some(pd) = inventory::physdisk_by_handle(t, dev.dev_handle)?
        && pd.phys_disk_state != raid::PD_STATE_NOT_CONFIGURED
    {
        bail!(
            "{address} is not an unconfigured disk, its state is {}",
            raid::drive_state(Some(&pd), true)
        );
    }
    raid::create_hot_spare_request(dev.dev_handle, pool).send_checked(t, "CREATE_HOT_SPARE")?;
    Ok(dev.dev_handle)
}

pub fn clear_log(t: &dyn Transport) -> Result<u16> {
    let mut page = config::require_page(t, LOG_0, 0)?;
    let entries = page.u16_at(LOG_NUM_ENTRIES);
    if page.len() < LOG_NUM_ENTRIES + 2 {
        bail!("{} is too short to clear", LOG_0.name);
    }
    page.put_u16(LOG_NUM_ENTRIES, 0);
    config::write_page_by_attribute(t, LOG_0, 0, &page)?;
    Ok(entries)
}
