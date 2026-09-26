use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "sasctl",
    version,
    about = "Manage LSI and Broadcom SAS HBAs and MegaRAID controllers",
    after_help = "Run a command on its own to see what is there, then add an ID from the first column to look closer or act on it.\nReads cover every controller unless you pick one with -c.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[arg(
        short = 'c',
        long = "controller",
        global = true,
        value_name = "ID",
        help = "Only this controller, IDs come from sasctl controller"
    )]
    pub controller: Option<usize>,
    #[arg(long, global = true, help = "Print JSON")]
    pub json: bool,
    #[arg(
        long,
        short = 'y',
        global = true,
        help = "Skip the confirmation for changes"
    )]
    pub yes: bool,
    #[arg(long, global = true, default_value = "/sys", hide = true)]
    pub sysfs: PathBuf,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    #[command(
        visible_aliases = ["controllers", "ctrl"],
        about = "List controllers, or show one"
    )]
    Controller {
        #[arg(
            value_name = "ID",
            help = "Controller ID from the first column of sasctl controller"
        )]
        id: Option<usize>,
        #[command(subcommand)]
        action: Option<ControllerAction>,
    },
    #[command(
        visible_alias = "drives",
        about = "List drives, or show one by enclosure:slot"
    )]
    Drive {
        #[arg(
            value_name = "DRIVE",
            help = "Enclosure and slot like 32:4, or 0:32:4 to include the controller"
        )]
        id: Option<DriveId>,
        #[command(subcommand)]
        action: Option<DriveAction>,
    },
    #[command(visible_alias = "volumes", about = "List RAID volumes, or show one")]
    Volume {
        #[arg(
            value_name = "VOLUME",
            help = "Volume ID like 1, or 0:1 to include the controller"
        )]
        id: Option<VolumeId>,
        #[command(subcommand)]
        action: Option<VolumeAction>,
    },
    #[command(visible_alias = "enclosures", about = "List enclosures")]
    Enclosure,
    #[command(visible_alias = "phys", about = "List controller phys and link rates")]
    Phy {
        #[arg(
            value_name = "PHY",
            help = "Phy number like 3, or 0:3 to include the controller"
        )]
        id: Option<PhyId>,
        #[command(subcommand)]
        action: Option<PhyAction>,
    },
    #[command(
        visible_aliases = ["temp", "temps", "temperatures"],
        about = "Controller and drive temperatures"
    )]
    Temperature,
    #[command(
        visible_aliases = ["bbu", "cachevault"],
        about = "Battery or CacheVault state"
    )]
    Battery {
        #[command(subcommand)]
        action: Option<BatteryAction>,
    },
    #[command(about = "Patrol read state and schedule")]
    Patrol {
        #[command(subcommand)]
        action: Option<PatrolAction>,
    },
    #[command(about = "Controller alarm")]
    Alarm {
        #[command(subcommand)]
        action: Option<AlarmAction>,
    },
    #[command(visible_alias = "events", about = "Controller event log")]
    Event {
        #[command(flatten)]
        filter: EventFilter,
        #[command(subcommand)]
        action: Option<EventAction>,
    },
    #[command(about = "RAID configuration of the whole controller")]
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    #[command(about = "Foreign configurations found on attached drives")]
    Foreign {
        #[command(subcommand)]
        action: Option<ForeignAction>,
    },
    #[command(about = "BIOS boot devices")]
    Boot {
        #[command(subcommand)]
        action: Option<BootAction>,
    },
    #[command(about = "Controller persistent log")]
    Log {
        #[command(subcommand)]
        action: Option<LogAction>,
    },
    #[command(visible_alias = "fw", about = "Firmware and BIOS versions")]
    Firmware {
        #[command(subcommand)]
        action: Option<FirmwareAction>,
    },
    #[command(about = "Firmware diagnostic buffers")]
    Diag {
        #[command(subcommand)]
        action: DiagAction,
    },
}

#[derive(Subcommand, Clone, Debug)]
pub enum ControllerAction {
    #[command(about = "Reset the controller")]
    Reset {
        #[arg(long, help = "Save a snapdump before the reset")]
        snapdump: bool,
    },
    #[command(about = "Show controller settings")]
    Settings,
    #[command(about = "Change a controller setting")]
    Set { setting: String, value: String },
    #[command(about = "Show the controller clock")]
    Time,
    #[command(about = "Shut the controller down")]
    Shutdown {
        #[arg(long, help = "Spin drives down as well")]
        spindown: bool,
    },
    #[command(about = "Flush the controller cache")]
    Flush {
        #[arg(long, help = "Flush drive caches as well")]
        disks: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Operation {
    Start,
    Stop,
}

#[derive(Subcommand, Clone, Debug)]
pub enum DriveAction {
    #[command(about = "Turn the locate LED on, or off")]
    Locate {
        #[arg(value_enum, default_value = "on")]
        state: Switch,
    },
    #[command(about = "SMART data and error counters")]
    Smart,
    #[command(about = "Temperature log of the drive")]
    Temperature,
    #[command(about = "Bring a RAID member online")]
    Online,
    #[command(about = "Take a RAID member offline")]
    Offline,
    #[command(about = "Mark the drive unconfigured good")]
    Good,
    #[command(about = "Expose the drive to the host as JBOD")]
    Jbod,
    #[command(about = "Make the drive a hot spare")]
    Spare {
        #[arg(
            long,
            value_name = "VOLUME",
            help = "Dedicate the spare to this volume"
        )]
        volume: Option<u8>,
        #[arg(long, help = "Hot spare pool 0 to 7")]
        pool: Option<u8>,
        #[arg(long, help = "Copy data back once the failed drive is replaced")]
        revertible: bool,
        #[arg(long, help = "Prefer volumes in the same enclosure")]
        affinity: bool,
    },
    #[command(about = "Remove the hot spare role")]
    Unspare,
    #[command(about = "Rebuild progress, or start and stop a rebuild")]
    Rebuild {
        #[arg(value_enum)]
        operation: Option<Operation>,
    },
    #[command(about = "Erase progress, or start and stop overwriting the drive")]
    Erase {
        #[arg(value_enum)]
        operation: Option<Operation>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum VolumeInit {
    None,
    Fast,
    Full,
}

#[derive(Subcommand, Clone, Debug)]
pub enum VolumeAction {
    #[command(about = "Create a volume from unconfigured drives")]
    Create {
        #[arg(help = "RAID level, like 1 or raid10")]
        level: String,
        #[arg(value_name = "DRIVE", required = true, num_args = 1..)]
        drives: Vec<DriveId>,
        #[arg(long, help = "Volume name, up to 15 characters")]
        name: Option<String>,
        #[arg(long, help = "Stripe size, like 64k")]
        stripe: Option<String>,
        #[arg(
            long,
            value_name = "MB",
            help = "Volume size, the largest possible when left out"
        )]
        size: Option<u64>,
        #[arg(long, help = "Hot spare pool 0 to 7")]
        pool: Option<u8>,
        #[arg(
            long,
            value_name = "N",
            help = "Drives per span for RAID 10, 50 and 60"
        )]
        span: Option<usize>,
        #[arg(long, value_enum, help = "Initialize the volume after creating it")]
        init: Option<VolumeInit>,
    },
    #[command(about = "Delete the volume and its data")]
    Delete {
        #[arg(long = "zero-lba0", help = "Zero block 0 of every member")]
        zero_lba0: bool,
    },
    #[command(about = "Activate an inactive volume")]
    Activate,
    #[command(about = "Start a consistency check")]
    Check,
    #[command(about = "Running initialization, check and rebuild progress")]
    Progress,
    #[command(about = "Show volume settings")]
    Settings,
    #[command(about = "Change a volume setting")]
    Set { setting: String, value: String },
}

#[derive(Subcommand, Clone, Debug)]
pub enum PhyAction {
    #[command(about = "Phy error counters")]
    Errors,
    #[command(about = "Reset the phy")]
    Reset {
        #[arg(long, help = "Hard reset instead of link reset")]
        hard: bool,
    },
}

#[derive(Subcommand, Clone, Debug)]
pub enum BatteryAction {
    #[command(about = "Start a learn cycle")]
    Learn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PatrolMode {
    Auto,
    Manual,
    Off,
}

#[derive(Subcommand, Clone, Debug)]
pub enum PatrolAction {
    #[command(about = "Start patrol read")]
    Start,
    #[command(about = "Stop patrol read")]
    Stop,
    #[command(about = "Change patrol read mode and schedule")]
    Set {
        #[arg(value_enum)]
        mode: PatrolMode,
        #[arg(long, help = "Seconds between runs, or continuous, in auto mode")]
        interval: Option<String>,
        #[arg(
            long,
            value_name = "SECONDS",
            help = "Seconds from now until the next run, in auto mode"
        )]
        start_in: Option<u32>,
    },
}

#[derive(Subcommand, Clone, Debug)]
pub enum AlarmAction {
    #[command(about = "Enable the alarm")]
    On,
    #[command(about = "Disable the alarm")]
    Off,
    #[command(about = "Silence a sounding alarm")]
    Silence,
}

#[derive(Args, Clone, Debug, Default)]
pub struct EventFilter {
    #[arg(long, value_name = "N", help = "Only the newest N events")]
    pub count: Option<usize>,
    #[arg(
        long,
        help = "Start at boot, shutdown, clear, oldest, newest or a sequence number"
    )]
    pub since: Option<String>,
    #[arg(
        long,
        help = "Lowest class, one of debug, progress, info, warning, critical, fatal, dead"
    )]
    pub class: Option<String>,
    #[arg(
        long,
        help = "Locales, one of ld, pd, enclosure, bbu, sas, controller, config, cluster, all"
    )]
    pub locale: Option<String>,
}

#[derive(Subcommand, Clone, Debug)]
pub enum EventAction {
    #[command(about = "Turn event logging on for every event type")]
    Enable,
    #[command(about = "Event log sequence numbers")]
    Info,
}

#[derive(Subcommand, Clone, Debug)]
pub enum ConfigAction {
    #[command(about = "Delete every volume and hot spare")]
    Clear {
        #[arg(long = "zero-lba0", help = "Zero block 0 of every member")]
        zero_lba0: bool,
    },
    #[command(about = "Save the raw configuration to a file")]
    Save { file: PathBuf },
}

#[derive(Subcommand, Clone, Debug)]
pub enum ForeignAction {
    #[command(about = "Show what an import would produce")]
    Preview { index: Option<u8> },
    #[command(about = "Import every foreign configuration, or one by index")]
    Import { index: Option<u8> },
    #[command(about = "Discard every foreign configuration")]
    Clear,
}

#[derive(Subcommand, Clone, Debug)]
pub enum BootAction {
    #[command(about = "Set the boot device to a drive or a volume")]
    Set {
        #[arg(
            long,
            value_name = "DRIVE",
            conflicts_with = "volume",
            required_unless_present = "volume"
        )]
        drive: Option<DriveId>,
        #[arg(long, value_name = "VOLUME")]
        volume: Option<VolumeId>,
        #[arg(long, help = "Set the alternate boot device")]
        alternate: bool,
    },
}

#[derive(Subcommand, Clone, Debug)]
pub enum LogAction {
    #[command(about = "Save the raw log page to a file")]
    Save { file: PathBuf },
    #[command(about = "Erase every log entry")]
    Clear,
}

#[derive(Subcommand, Clone, Debug)]
pub enum FirmwareAction {
    #[command(about = "Flash a firmware image")]
    Flash {
        file: PathBuf,
        #[arg(long, help = "The file is an option ROM for the BIOS region")]
        bios: bool,
    },
    #[command(about = "Save the flashed image to a file")]
    Save {
        file: PathBuf,
        #[arg(long, help = "Save the BIOS image")]
        bios: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BufferKind {
    Trace,
    Snapshot,
    Extended,
}

#[derive(Subcommand, Clone, Debug)]
pub enum DiagAction {
    #[command(about = "Register and post a diagnostic buffer")]
    Register {
        #[arg(value_enum)]
        kind: BufferKind,
        #[arg(long, help = "Buffer size in bytes, a multiple of 4")]
        size: u32,
        #[arg(long, value_parser = parse_u32, help = "Unique id, the driver's trace id when left out")]
        unique_id: Option<u32>,
        #[arg(long, value_parser = parse_u32, default_value = "0")]
        diagnostic_flags: u32,
    },
    #[command(about = "Show a diagnostic buffer")]
    Query {
        #[arg(value_enum)]
        kind: BufferKind,
    },
    #[command(about = "Save a diagnostic buffer to a file")]
    Read {
        #[arg(value_enum)]
        kind: BufferKind,
        file: PathBuf,
    },
    #[command(about = "Release a diagnostic buffer so the firmware stops writing")]
    Release {
        #[arg(value_parser = parse_u32)]
        unique_id: u32,
    },
    #[command(about = "Free a released diagnostic buffer")]
    Unregister {
        #[arg(value_parser = parse_u32)]
        unique_id: u32,
    },
}

fn parse_u32(s: &str) -> Result<u32, String> {
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|e| format!("invalid number {s:?}: {e}"))
}

fn split_controller(s: &str, max_parts: usize) -> Result<(Option<usize>, &str), String> {
    let parts = s.split(':').count();
    if parts < max_parts {
        return Ok((None, s));
    }
    if parts > max_parts {
        return Err(format!("{s:?} has too many parts"));
    }
    let (c, rest) = s.split_once(':').unwrap_or((s, ""));
    let controller = c
        .parse()
        .map_err(|_| format!("invalid controller {c:?} in {s:?}"))?;
    Ok((Some(controller), rest))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveId {
    pub controller: Option<usize>,
    pub address: String,
}

impl FromStr for DriveId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (controller, address) = split_controller(s, 3)?;
        if address.is_empty() {
            return Err(format!("{s:?} is missing the slot"));
        }
        Ok(Self {
            controller,
            address: address.to_string(),
        })
    }
}

impl fmt::Display for DriveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.controller {
            Some(c) => write!(f, "{c}:{}", self.address),
            None => f.write_str(&self.address),
        }
    }
}

impl DriveId {
    pub fn parse<T>(&self) -> anyhow::Result<T>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        self.address
            .parse()
            .map_err(|e| anyhow::anyhow!("drive {}: {e}", self.address))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeId {
    pub controller: Option<usize>,
    pub id: u16,
}

impl FromStr for VolumeId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (controller, id) = split_controller(s, 2)?;
        let id = id.parse().map_err(|_| format!("invalid volume {s:?}"))?;
        Ok(Self { controller, id })
    }
}

impl fmt::Display for VolumeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.controller {
            Some(c) => write!(f, "{c}:{}", self.id),
            None => write!(f, "{}", self.id),
        }
    }
}

impl VolumeId {
    pub fn narrow(&self) -> anyhow::Result<u8> {
        u8::try_from(self.id).map_err(|_| anyhow::anyhow!("volume {} is out of range", self.id))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhyId {
    pub controller: Option<usize>,
    pub phy: u8,
}

impl FromStr for PhyId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (controller, phy) = split_controller(s, 2)?;
        let phy = phy.parse().map_err(|_| format!("invalid phy {s:?}"))?;
        Ok(Self { controller, phy })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Find,
    One,
}

impl Command {
    pub fn scope(&self) -> Scope {
        use Command as C;
        match self {
            C::Controller { action: None, .. } => Scope::Find,
            C::Drive {
                id: None,
                action: None,
            } => Scope::All,
            C::Drive {
                action:
                    None
                    | Some(DriveAction::Smart)
                    | Some(DriveAction::Temperature)
                    | Some(DriveAction::Rebuild { operation: None })
                    | Some(DriveAction::Erase { operation: None }),
                ..
            } => Scope::Find,
            C::Volume {
                id: None,
                action: None,
            } => Scope::All,
            C::Volume {
                action: None | Some(VolumeAction::Progress) | Some(VolumeAction::Settings),
                ..
            } => Scope::Find,
            C::Enclosure | C::Temperature => Scope::All,
            C::Phy {
                action: None | Some(PhyAction::Errors),
                ..
            } => Scope::All,
            C::Battery { action: None }
            | C::Patrol { action: None }
            | C::Alarm { action: None }
            | C::Config { action: None }
            | C::Boot { action: None }
            | C::Log { action: None }
            | C::Firmware { action: None } => Scope::All,
            C::Event {
                action: None | Some(EventAction::Info),
                ..
            } => Scope::All,
            C::Foreign {
                action: None | Some(ForeignAction::Preview { .. }),
            } => Scope::All,
            C::Diag {
                action: DiagAction::Query { .. },
            } => Scope::All,
            _ => Scope::One,
        }
    }

    pub fn controller_hint(&self) -> Result<Option<usize>, String> {
        let mut found: Vec<usize> = Vec::new();
        match self {
            Command::Controller { id, .. } => found.extend(*id),
            Command::Drive { id, .. } => found.extend(id.as_ref().and_then(|d| d.controller)),
            Command::Volume { id, action } => {
                found.extend(id.and_then(|v| v.controller));
                if let Some(VolumeAction::Create { drives, .. }) = action {
                    found.extend(drives.iter().filter_map(|d| d.controller));
                }
            }
            Command::Phy { id, .. } => found.extend(id.and_then(|p| p.controller)),
            Command::Boot {
                action: Some(BootAction::Set { drive, volume, .. }),
            } => {
                found.extend(drive.as_ref().and_then(|d| d.controller));
                found.extend(volume.and_then(|v| v.controller));
            }
            _ => {}
        }
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            _ => Err("the IDs point at different controllers".into()),
        }
    }

    pub fn missing_object(&self) -> Option<(&'static str, Command)> {
        match self {
            Command::Drive {
                id: None,
                action: Some(_),
            } => Some((
                "drive",
                Command::Drive {
                    id: None,
                    action: None,
                },
            )),
            Command::Volume {
                id: None,
                action: Some(a),
            } if !matches!(a, VolumeAction::Create { .. }) => Some((
                "volume",
                Command::Volume {
                    id: None,
                    action: None,
                },
            )),
            Command::Phy {
                id: None,
                action: Some(PhyAction::Reset { .. }),
            } => Some((
                "phy",
                Command::Phy {
                    id: None,
                    action: None,
                },
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
pub fn try_parse(argv: &[&str]) -> anyhow::Result<Command> {
    let mut full = vec!["sasctl"];
    full.extend_from_slice(argv);
    Cli::try_parse_from(full)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .command
        .ok_or_else(|| anyhow::anyhow!("no command"))
}

#[cfg(test)]
pub fn parse(argv: &[&str]) -> Command {
    let mut full = vec!["sasctl"];
    full.extend_from_slice(argv);
    Cli::try_parse_from(full)
        .unwrap_or_else(|e| panic!("{argv:?}: {e}"))
        .command
        .expect("a command")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        let mut full = vec!["sasctl"];
        full.extend_from_slice(argv);
        Cli::try_parse_from(full)
    }

    #[test]
    fn nouns_work_without_a_verb() {
        for argv in [
            &["temperature"][..],
            &["temp"],
            &["drive"],
            &["drives"],
            &["volume"],
            &["controller"],
            &["patrol"],
            &["event"],
            &["firmware"],
        ] {
            assert!(try_parse(argv).is_ok(), "{argv:?}");
        }
        assert!(try_parse(&[]).unwrap().command.is_none());
    }

    #[test]
    fn global_flags_go_anywhere() {
        for argv in [
            &["-c", "1", "temperature"][..],
            &["temperature", "-c", "1"],
            &["drive", "2:5", "locate", "-c", "1", "--yes"],
            &["--json", "drive", "-c", "1"],
        ] {
            assert_eq!(try_parse(argv).unwrap().controller, Some(1), "{argv:?}");
        }
    }

    #[test]
    fn object_ids_come_before_the_action() {
        let Command::Drive { id, action } = parse(&["drive", "0:32:4", "locate", "off"]) else {
            panic!("not a drive command");
        };
        assert_eq!(
            id,
            Some(DriveId {
                controller: Some(0),
                address: "32:4".into()
            })
        );
        assert!(matches!(
            action,
            Some(DriveAction::Locate { state: Switch::Off })
        ));
        let Command::Drive { id, action } = parse(&["drive", "locate"]) else {
            panic!("not a drive command");
        };
        assert!(id.is_none());
        assert!(matches!(
            action,
            Some(DriveAction::Locate { state: Switch::On })
        ));
        let Command::Controller { id, action } = parse(&["controller", "1", "reset"]) else {
            panic!("not a controller command");
        };
        assert_eq!(id, Some(1));
        assert!(action.is_some());
    }

    #[test]
    fn ids_parse_with_and_without_a_controller() {
        let d: DriveId = "32:4".parse().unwrap();
        assert_eq!(d.controller, None);
        let d: DriveId = "1::4".parse().unwrap();
        assert_eq!((d.controller, d.address.as_str()), (Some(1), ":4"));
        let d: DriveId = "7".parse().unwrap();
        assert_eq!(d.address, "7");
        assert!("1:2:3:4".parse::<DriveId>().is_err());
        let v: VolumeId = "2:9".parse().unwrap();
        assert_eq!((v.controller, v.id), (Some(2), 9));
        let v: VolumeId = "9".parse().unwrap();
        assert_eq!(v.controller, None);
    }

    #[test]
    fn scopes_follow_reads_and_changes() {
        assert_eq!(parse(&["drive"]).scope(), Scope::All);
        assert_eq!(parse(&["drive", "2:1"]).scope(), Scope::Find);
        assert_eq!(parse(&["drive", "2:1", "smart"]).scope(), Scope::Find);
        assert_eq!(parse(&["drive", "2:1", "rebuild"]).scope(), Scope::Find);
        assert_eq!(
            parse(&["drive", "2:1", "rebuild", "start"]).scope(),
            Scope::One
        );
        assert_eq!(parse(&["drive", "2:1", "locate"]).scope(), Scope::One);
        assert_eq!(parse(&["temperature"]).scope(), Scope::All);
        assert_eq!(parse(&["patrol", "start"]).scope(), Scope::One);
        assert_eq!(parse(&["controller"]).scope(), Scope::Find);
    }

    #[test]
    fn controller_hints_must_agree() {
        assert_eq!(parse(&["drive", "1:2:3"]).controller_hint(), Ok(Some(1)));
        assert_eq!(parse(&["controller", "3"]).controller_hint(), Ok(Some(3)));
        assert!(
            parse(&["volume", "create", "1", "0:2:0", "1:2:1"])
                .controller_hint()
                .is_err()
        );
    }
}
