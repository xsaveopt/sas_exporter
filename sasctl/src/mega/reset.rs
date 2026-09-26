use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::ioctl::Device;
use crate::mega::ctrl::CtrlProps;

pub const SG_SCSI_RESET: u64 = 0x2284;
pub const SG_SCSI_RESET_HOST: i32 = 3;

pub fn check_allowed(props: &CtrlProps) -> Result<()> {
    if props.on_off.disable_online_ctrl_reset {
        bail!(
            "online controller reset is disabled on this controller (ocr off), the driver would take the adapter offline instead of resetting it, so the reset is refused"
        );
    }
    Ok(())
}

fn sg_index(name: &str) -> Option<u32> {
    name.strip_prefix("sg")?.parse().ok()
}

fn host_of(device: &Path) -> Option<u32> {
    let resolved = fs::canonicalize(device).ok()?;
    let name = resolved.file_name()?.to_str()?;
    let mut parts = name.split(':');
    let host = parts.next()?.parse().ok()?;
    (parts.count() == 3).then_some(host)
}

pub fn find_sg_node(sysfs: &Path, host_no: u32) -> Result<String> {
    let dir = sysfs.join("class/scsi_generic");
    let entries = fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
    let mut found: Vec<(u32, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let index = sg_index(&name)?;
            (host_of(&e.path().join("device"))? == host_no).then_some((index, name))
        })
        .collect();
    found.sort();
    match found.into_iter().next() {
        Some((_, name)) => Ok(name),
        None => bail!(
            "no SCSI generic device belongs to host {host_no}, load the sg module or expose a volume first"
        ),
    }
}

pub fn sg_reset_host(sysfs: &Path, host_no: u32) -> Result<PathBuf> {
    let node = Path::new("/dev").join(find_sg_node(sysfs, host_no)?);
    let dev = Device::open(&node).with_context(|| format!("opening {}", node.display()))?;
    let mut kind = SG_SCSI_RESET_HOST;
    unsafe { dev.ioctl(SG_SCSI_RESET, (&raw mut kind).cast::<u8>()) }
        .with_context(|| format!("SG_SCSI_RESET_HOST on {}", node.display()))?;
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mega::ctrl::CtrlProps;

    fn fake(name: &str, nodes: &[(&str, &str)]) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(name);
        let _ = fs::remove_dir_all(&root);
        for (sg, hctl) in nodes {
            let host = hctl.split(':').next().unwrap();
            let target = hctl.rsplit_once(':').unwrap().0;
            let dev = root.join(format!(
                "devices/pci0000:00/0000:03:00.0/host{host}/target{target}/{hctl}"
            ));
            fs::create_dir_all(&dev).unwrap();
            let class = root.join("class/scsi_generic").join(sg);
            fs::create_dir_all(&class).unwrap();
            std::os::unix::fs::symlink(&dev, class.join("device")).unwrap();
        }
        root
    }

    #[test]
    fn picks_the_lowest_sg_node_on_the_host() {
        let root = fake(
            "test-mega-reset-sg",
            &[("sg3", "2:2:1:0"), ("sg10", "2:2:0:0"), ("sg0", "0:0:0:0")],
        );
        assert_eq!(find_sg_node(&root, 2).unwrap(), "sg3");
        assert_eq!(find_sg_node(&root, 0).unwrap(), "sg0");
        assert!(find_sg_node(&root, 5).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_when_online_controller_reset_is_disabled() {
        let mut raw = vec![0u8; 64];
        assert!(check_allowed(&CtrlProps::parse(&raw)).is_ok());
        raw[33] = 0x04;
        let e = check_allowed(&CtrlProps::parse(&raw)).unwrap_err();
        assert!(e.to_string().contains("refused"));
    }

    #[test]
    fn find_sg_node_skips_foreign_names_and_non_device_links() {
        let root = fake("test-mega-reset-skip", &[("sg7", "4:0:0:0")]);
        let host_dir = root.join("devices/pci0000:00/0000:03:00.0/host4");
        for name in ["bsg1", "sgx"] {
            let class = root.join("class/scsi_generic").join(name);
            fs::create_dir_all(&class).unwrap();
            std::os::unix::fs::symlink(
                root.join("devices/pci0000:00/0000:03:00.0/host4/target4:0:0/4:0:0:0"),
                class.join("device"),
            )
            .unwrap();
        }
        let class = root.join("class/scsi_generic/sg2");
        fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&host_dir, class.join("device")).unwrap();
        let class = root.join("class/scsi_generic/sg1");
        fs::create_dir_all(&class).unwrap();
        let found = find_sg_node(&root, 4);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.unwrap(), "sg7");
    }

    #[test]
    fn find_sg_node_needs_the_scsi_generic_class() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-mega-reset-none");
        let _ = fs::remove_dir_all(&root);
        let err = find_sg_node(&root, 0).unwrap_err();
        assert!(err.to_string().contains("class/scsi_generic"), "{err}");
    }

    #[test]
    fn sg_reset_host_names_the_host_without_a_node() {
        let root = fake("test-mega-reset-nohost", &[("sg0", "0:0:0:0")]);
        let err = sg_reset_host(&root, 6).unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            err.to_string()
                .contains("no SCSI generic device belongs to host 6"),
            "{err}"
        );
    }

    #[test]
    fn sg_reset_host_opens_the_dev_node_it_found() {
        let root = fake("test-mega-reset-open", &[("sg987", "6:2:0:0")]);
        let err = sg_reset_host(&root, 6).unwrap_err();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            format!("{err:#}").starts_with("opening /dev/sg987"),
            "{err:#}"
        );
    }
}
