use serde::Serialize;

use super::mpi::{FUNCTION_RAID_ACTION, Request, TIMEOUT_RAID};
use super::pages::RaidPhysDisk0;
use crate::bytes::{Le, LeMut};

pub const ACTION_INDICATOR_STRUCT: u8 = 0x01;
pub const ACTION_CREATE_VOLUME: u8 = 0x02;
pub const ACTION_DELETE_VOLUME: u8 = 0x03;
pub const ACTION_PHYSDISK_OFFLINE: u8 = 0x0A;
pub const ACTION_PHYSDISK_ONLINE: u8 = 0x0B;
pub const ACTION_ACTIVATE_VOLUME: u8 = 0x11;
pub const ACTION_CREATE_HOT_SPARE: u8 = 0x1D;
pub const ACTION_DELETE_HOT_SPARE: u8 = 0x1E;
pub const ACTION_START_RAID_FUNCTION: u8 = 0x21;

pub const ADATA_KEEP_LBA0: u32 = 0x0000_0000;
pub const ADATA_ZERO_LBA0: u32 = 0x0000_0001;

pub const VOL_TYPE_RAID0: u8 = 0x00;
pub const VOL_TYPE_RAID1E: u8 = 0x01;
pub const VOL_TYPE_RAID1: u8 = 0x02;
pub const VOL_TYPE_RAID10: u8 = 0x05;

pub const CREATION_DEFAULT_SETTINGS: u32 = 0x8000_0000;
pub const CREATION_BACKGROUND_INIT: u32 = 0x0000_0004;
pub const CREATION_MIGRATE_DATA: u32 = 0x0000_0001;

pub const PHYSDISK_MAP_PRIMARY: u8 = 0x01;
pub const PHYSDISK_MAP_SECONDARY: u8 = 0x02;

pub const CREATION_FIXED_LEN: usize = 0x2C;
pub const CREATION_PHYSDISK_LEN: usize = 4;
pub const VOLUME_NAME_MAX: usize = 15;

pub const RAID_FUNCTION_CONSISTENCY_CHECK: u8 = 0x02;
pub const RAID_FUNCTION_START_NEW: u8 = 0x00;

pub const STATUS_FLAG_ENABLED: u32 = 0x0000_0001;
pub const STATUS_FLAG_QUIESCED: u32 = 0x0000_0002;
pub const STATUS_FLAG_VOLUME_INACTIVE: u32 = 0x0000_0004;
pub const STATUS_FLAG_RESYNC_IN_PROGRESS: u32 = 0x0001_0000;
pub const STATUS_FLAG_BACKGROUND_INIT: u32 = 0x0002_0000;
pub const STATUS_FLAG_CAPACITY_EXPANSION: u32 = 0x0004_0000;
pub const STATUS_FLAG_CONSISTENCY_CHECK: u32 = 0x0008_0000;

pub const PD_STATE_NOT_CONFIGURED: u8 = 0x00;
pub const PD_STATE_NOT_COMPATIBLE: u8 = 0x01;
pub const PD_STATE_OFFLINE: u8 = 0x02;
pub const PD_STATE_ONLINE: u8 = 0x03;
pub const PD_STATE_HOT_SPARE: u8 = 0x04;
pub const PD_STATE_DEGRADED: u8 = 0x05;
pub const PD_STATE_REBUILDING: u8 = 0x06;
pub const PD_STATE_OPTIMAL: u8 = 0x07;
pub const PD_OFFLINE_MISSING: u8 = 0x01;
pub const PD_STATUS_OUT_OF_SYNC: u32 = 0x0000_0001;

fn raid_action(action: u8) -> Request {
    let mut r = Request::new(FUNCTION_RAID_ACTION, 5);
    r.frame.put_u8(0x00, action);
    r.timeout = TIMEOUT_RAID;
    r
}

pub fn volume_action_request(action: u8, volume_handle: u16) -> Request {
    let mut r = raid_action(action);
    r.frame.put_u16(0x04, volume_handle);
    r
}

pub fn physdisk_action_request(action: u8, phys_disk_num: u8) -> Request {
    let mut r = raid_action(action);
    r.frame.put_u8(0x06, phys_disk_num);
    r
}

fn full_raid_action(action: u8) -> Request {
    let mut r = Request::new(FUNCTION_RAID_ACTION, 8);
    r.frame.put_u8(0x00, action);
    r.timeout = TIMEOUT_RAID;
    r
}

pub fn delete_volume_request(volume_handle: u16, zero_lba0: bool) -> Request {
    let mut r = full_raid_action(ACTION_DELETE_VOLUME);
    r.frame.put_u16(0x04, volume_handle);
    r.frame.put_u32(
        0x10,
        if zero_lba0 {
            ADATA_ZERO_LBA0
        } else {
            ADATA_KEEP_LBA0
        },
    );
    r
}

pub fn hot_spare_action_word(dev_handle: u16, pool: u8) -> u32 {
    ((dev_handle as u32) << 16) | (1u32 << pool)
}

pub fn create_hot_spare_request(dev_handle: u16, pool: u8) -> Request {
    let mut r = full_raid_action(ACTION_CREATE_HOT_SPARE);
    r.frame
        .put_u32(0x10, hot_spare_action_word(dev_handle, pool));
    r
}

pub fn create_volume_request(creation: &[u8]) -> Request {
    let mut r = raid_action(ACTION_CREATE_VOLUME);
    r.data_out = creation.to_vec();
    r
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeCreation {
    pub volume_type: u8,
    pub flags: u32,
    pub settings: u32,
    pub resync_rate: u8,
    pub data_scrub_duration: u16,
    pub max_lba: u64,
    pub stripe_blocks: u32,
    pub name: String,
    pub members: Vec<u16>,
}

impl VolumeCreation {
    pub fn encode(&self) -> Vec<u8> {
        let n = self.members.len();
        let mut b = vec![0u8; CREATION_FIXED_LEN + CREATION_PHYSDISK_LEN * n];
        b.put_u8(0x00, n as u8);
        b.put_u8(0x01, self.volume_type);
        b.put_u32(0x04, self.flags);
        b.put_u32(0x08, self.settings);
        b.put_u8(0x0D, self.resync_rate);
        b.put_u16(0x0E, self.data_scrub_duration);
        b.put_u64(0x10, self.max_lba);
        b.put_u32(0x18, self.stripe_blocks);
        let name = self.name.as_bytes();
        b.put_bytes(0x1C, &name[..name.len().min(VOLUME_NAME_MAX)]);
        for (i, handle) in self.members.iter().enumerate() {
            let o = CREATION_FIXED_LEN + i * CREATION_PHYSDISK_LEN;
            let map = match (self.volume_type, i) {
                (VOL_TYPE_RAID1, 0) => PHYSDISK_MAP_PRIMARY,
                (VOL_TYPE_RAID1, 1) => PHYSDISK_MAP_SECONDARY,
                _ => i as u8,
            };
            b.put_u8(o, 0);
            b.put_u8(o + 1, map);
            b.put_u16(o + 2, *handle);
        }
        b
    }
}

pub fn consistency_check_request(volume_handle: u16) -> Request {
    let mut r = volume_action_request(ACTION_START_RAID_FUNCTION, volume_handle);
    r.frame.put_u8(0x10, RAID_FUNCTION_CONSISTENCY_CHECK);
    r.frame.put_u8(0x11, RAID_FUNCTION_START_NEW);
    r
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Progress {
    pub total_blocks: u64,
    pub blocks_remaining: u64,
    pub flags: u32,
    pub elapsed_seconds: Option<u32>,
    pub percent_complete: f64,
}

pub fn parse_indicator(reply: &[u8]) -> Progress {
    let total = reply.u64_at(0x14);
    let remaining = reply.u64_at(0x1C);
    let flags = reply.u32_at(0x24);
    let elapsed = reply.u32_at(0x28);
    Progress {
        total_blocks: total,
        blocks_remaining: remaining,
        flags,
        elapsed_seconds: (flags & 0x8000_0000 != 0).then_some(elapsed),
        percent_complete: percent(total, remaining),
    }
}

pub fn percent(total: u64, remaining: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let done = total.saturating_sub(remaining) as f64;
    (done * 100.0 / total as f64 * 100.0).floor() / 100.0
}

pub fn volume_state_name(state: u8) -> &'static str {
    match state {
        0x00 => "Missing (MIS)",
        0x01 => "Failed (FLD)",
        0x02 => "Initializing (INIT)",
        0x03 => "Online (ONL)",
        0x04 => "Degraded (DGD)",
        0x05 => "Okay (OKY)",
        _ => "Unknown",
    }
}

pub fn volume_type_name(volume_type: u8) -> &'static str {
    match volume_type {
        0x00 => "RAID0",
        0x01 => "RAID1E",
        0x02 => "RAID1",
        0x05 => "RAID10",
        _ => "Unknown",
    }
}

pub fn current_operation(flags: u32) -> &'static str {
    if flags & STATUS_FLAG_RESYNC_IN_PROGRESS != 0 {
        "Synchronize"
    } else if flags & STATUS_FLAG_CONSISTENCY_CHECK != 0 {
        "Consistency Check"
    } else if flags & STATUS_FLAG_CAPACITY_EXPANSION != 0 {
        "OCE"
    } else if flags & STATUS_FLAG_BACKGROUND_INIT != 0 {
        "Background Init"
    } else {
        "None"
    }
}

pub fn drive_state(physdisk: Option<&RaidPhysDisk0>, is_disk: bool) -> &'static str {
    let Some(pd) = physdisk else {
        return if is_disk {
            "Ready (RDY)"
        } else {
            "Standby (SBY)"
        };
    };
    let in_sync_state = matches!(pd.phys_disk_state, PD_STATE_ONLINE | PD_STATE_OPTIMAL);
    if in_sync_state && pd.phys_disk_status_flags & PD_STATUS_OUT_OF_SYNC != 0 {
        return "Out of Sync (OSY)";
    }
    match pd.phys_disk_state {
        PD_STATE_NOT_CONFIGURED => "Ready (RDY)",
        PD_STATE_NOT_COMPATIBLE => "Available (AVL)",
        PD_STATE_OFFLINE if pd.offline_reason == PD_OFFLINE_MISSING => "Missing (MIS)",
        PD_STATE_OFFLINE => "Failed (FLD)",
        PD_STATE_ONLINE => "Online (ONL)",
        PD_STATE_HOT_SPARE => "Hot Spare (HSP)",
        PD_STATE_DEGRADED => "Degraded (DGD)",
        PD_STATE_REBUILDING => "Rebuilding (RBLD)",
        PD_STATE_OPTIMAL => "Optimal (OPT)",
        _ => "Unknown",
    }
}

pub fn is_hidden_member(pd: &RaidPhysDisk0) -> bool {
    !matches!(
        pd.phys_disk_state,
        PD_STATE_NOT_CONFIGURED | PD_STATE_NOT_COMPATIBLE
    )
}
