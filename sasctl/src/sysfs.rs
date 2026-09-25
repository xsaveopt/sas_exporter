use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScsiHost {
    pub host_no: u32,
    pub proc_name: String,
    pub unique_id: Option<u32>,
    pub pci_address: Option<String>,
    pub path: PathBuf,
}

impl ScsiHost {
    pub fn attr(&self, name: &str) -> Option<String> {
        read_trimmed(&self.path.join(name))
    }
}

pub fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn scsi_hosts(sysfs: &Path, drivers: &[&str]) -> Vec<ScsiHost> {
    let dir = sysfs.join("class/scsi_host");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut hosts: Vec<ScsiHost> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let host_no = name.strip_prefix("host")?.parse().ok()?;
            let path = e.path();
            let proc_name = read_trimmed(&path.join("proc_name"))?;
            if !drivers.contains(&proc_name.as_str()) {
                return None;
            }
            let unique_id = read_trimmed(&path.join("unique_id")).and_then(|s| s.parse().ok());
            let pci_address = pci_address_of(&path.join("device"));
            Some(ScsiHost {
                host_no,
                proc_name,
                unique_id,
                pci_address,
                path,
            })
        })
        .collect();
    hosts.sort_by_key(|h| h.host_no);
    hosts
}

fn pci_address_of(device: &Path) -> Option<String> {
    let resolved = fs::canonicalize(device).ok()?;
    resolved
        .ancestors()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .find(|n| is_pci_address(n))
}

pub fn is_pci_address(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[4] == b':'
        && b[7] == b':'
        && b[10] == b'.'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10) || c.is_ascii_hexdigit())
}

pub fn parse_pci_address(s: &str) -> Option<(u32, u8, u8, u8)> {
    if !is_pci_address(s) {
        return None;
    }
    Some((
        u32::from_str_radix(&s[0..4], 16).ok()?,
        u8::from_str_radix(&s[5..7], 16).ok()?,
        u8::from_str_radix(&s[8..10], 16).ok()?,
        u8::from_str_radix(&s[11..12], 16).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_hosts_by_driver() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-sysfs");
        let _ = fs::remove_dir_all(&root);
        let pci = root.join("devices/pci0000:00/0000:00:01.0/0000:02:00.0/host3");
        fs::create_dir_all(&pci).unwrap();
        for (n, driver, uid) in [(3, "mpt3sas", "0"), (4, "ahci", "1")] {
            let h = root.join(format!("class/scsi_host/host{n}"));
            fs::create_dir_all(&h).unwrap();
            fs::write(h.join("proc_name"), format!("{driver}\n")).unwrap();
            fs::write(h.join("unique_id"), uid).unwrap();
            if n == 3 {
                std::os::unix::fs::symlink(&pci, h.join("device")).unwrap();
            }
        }
        let hosts = scsi_hosts(&root, &["mpt2sas", "mpt3sas"]);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host_no, 3);
        assert_eq!(hosts[0].unique_id, Some(0));
        assert_eq!(hosts[0].pci_address.as_deref(), Some("0000:02:00.0"));
    }

    #[test]
    fn parses_pci_addresses() {
        assert_eq!(parse_pci_address("0000:81:00.1"), Some((0, 0x81, 0, 1)));
        assert_eq!(parse_pci_address("host3"), None);
    }
}
