use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::mega;
use crate::mpi3;
use crate::mpt;
use crate::output::{Render, Table};
use crate::sysfs::parse_pci_address;

#[derive(Clone, Debug)]
pub enum Backend {
    Mpt(mpt::adapter::Target),
    Mpi3(mpi3::adapter::Target),
    Mega(mega::cli::ControllerRef),
}

#[derive(Clone, Debug)]
pub struct Controller {
    pub id: usize,
    pub driver: String,
    pub host_no: u32,
    pub pci_address: Option<String>,
    pub backend: Backend,
}

impl Controller {
    pub fn label(&self) -> String {
        match &self.pci_address {
            Some(pci) => format!("Controller {}, {} at {pci}", self.id, self.driver),
            None => format!("Controller {}, {}", self.id, self.driver),
        }
    }
}

pub fn order(mut found: Vec<Controller>) -> Vec<Controller> {
    found.sort_by_key(|c| {
        let pci = c.pci_address.as_deref().and_then(parse_pci_address);
        (pci.is_none(), pci.unwrap_or_default(), c.host_no)
    });
    for (id, c) in found.iter_mut().enumerate() {
        c.id = id;
        match &mut c.backend {
            Backend::Mpt(t) => t.index = id,
            Backend::Mpi3(t) => t.id = id,
            Backend::Mega(r) => r.index = id,
        }
    }
    found
}

pub fn discover(sysfs: &Path) -> Vec<Controller> {
    let mut found = Vec::new();
    for t in mpt::adapter::enumerate(sysfs) {
        found.push(Controller {
            id: 0,
            driver: t.host.proc_name.clone(),
            host_no: t.host.host_no,
            pci_address: t.host.pci_address.clone(),
            backend: Backend::Mpt(t),
        });
    }
    for t in mpi3::adapter::enumerate(sysfs) {
        found.push(Controller {
            id: 0,
            driver: t.host.proc_name.clone(),
            host_no: t.host.host_no,
            pci_address: t.host.pci_address.clone(),
            backend: Backend::Mpi3(t),
        });
    }
    for r in mega::cli::controllers(sysfs) {
        found.push(Controller {
            id: 0,
            driver: mega::cli::DRIVER.to_string(),
            host_no: r.host_no,
            pci_address: r.pci_address.clone(),
            backend: Backend::Mega(r),
        });
    }
    order(found)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ControllerRow {
    pub controller: usize,
    pub driver: String,
    pub host: u32,
    pub pci_address: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    pub firmware_version: Option<String>,
    pub bios_version: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ControllerList {
    pub controllers: Vec<ControllerRow>,
}

impl Serialize for ControllerList {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.controllers.serialize(s)
    }
}

impl Render for ControllerList {
    fn render(&self, out: &mut String) {
        if self.controllers.is_empty() {
            out.push_str("No supported controllers found\n");
            return;
        }
        let mut t = Table::new(["ID", "Driver", "Model", "PCI address", "Firmware", "State"]);
        for c in &self.controllers {
            let state = match (&c.error, &c.state) {
                (Some(e), _) => format!("error: {e}"),
                (None, Some(s)) => s.clone(),
                (None, None) => "ok".to_string(),
            };
            t.row([
                c.controller.to_string(),
                c.driver.clone(),
                c.model.clone().unwrap_or_else(|| "-".into()),
                c.pci_address.clone().unwrap_or_else(|| "-".into()),
                c.firmware_version.clone().unwrap_or_else(|| "-".into()),
                state,
            ]);
        }
        t.render(out);
    }
}

fn row(c: &Controller) -> ControllerRow {
    let mut r = ControllerRow {
        controller: c.id,
        driver: c.driver.clone(),
        host: c.host_no,
        pci_address: c.pci_address.clone(),
        ..Default::default()
    };
    let filled: Result<()> = (|| {
        match &c.backend {
            Backend::Mpt(t) => {
                let transport = mpt::adapter::open(t)?;
                let a = mpt::inventory::adapter_row(t, transport.as_ref())?;
                r.pci_address = Some(a.pci_address);
                r.model = Some(a.chip);
                r.firmware_version = Some(a.firmware_version);
                r.bios_version = Some(a.bios_version);
            }
            Backend::Mpi3(t) => {
                let transport = mpi3::adapter::open(t)?;
                let a = mpi3::inventory::adapter_row(t, transport.as_ref())?;
                r.pci_address = Some(a.pci_address);
                r.model = Some(a.chip);
                r.firmware_version = a.firmware_version;
                r.state = Some(a.state.to_string());
            }
            Backend::Mega(m) => {
                let transport = mega::open_host(m.host_no)?;
                let i = mega::ctrl::get_info(transport.as_ref())?;
                r.firmware_version = Some(i.firmware_version());
                r.model = Some(i.product_name);
                r.serial_number = Some(i.serial_number);
            }
        }
        Ok(())
    })();
    if let Err(e) = filled {
        r.error = Some(format!("{e:#}"));
    }
    r
}

pub fn list(ctrls: &[Controller]) -> ControllerList {
    ControllerList {
        controllers: ctrls.iter().map(row).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::ScsiHost;
    use std::path::PathBuf;

    fn host(host_no: u32, proc_name: &str, pci: Option<&str>) -> ScsiHost {
        ScsiHost {
            host_no,
            proc_name: proc_name.into(),
            unique_id: Some(0),
            pci_address: pci.map(str::to_string),
            path: PathBuf::from(format!("/nonexistent/host{host_no}")),
        }
    }

    fn mega(host_no: u32, pci: &str) -> Controller {
        Controller {
            id: 99,
            driver: mega::cli::DRIVER.into(),
            host_no,
            pci_address: Some(pci.into()),
            backend: Backend::Mega(mega::cli::ControllerRef {
                index: 99,
                host_no,
                pci_address: Some(pci.into()),
            }),
        }
    }

    #[test]
    fn every_driver_shares_one_numbering_in_pci_order() {
        let h = host(3, "mpt3sas", Some("0000:81:00.0"));
        let mpt = Controller {
            id: 99,
            driver: h.proc_name.clone(),
            host_no: 3,
            pci_address: h.pci_address.clone(),
            backend: Backend::Mpt(mpt::adapter::order_targets(vec![h]).remove(0)),
        };
        let h = host(5, "mpi3mr", None);
        let mpi3 = Controller {
            id: 99,
            driver: h.proc_name.clone(),
            host_no: 5,
            pci_address: None,
            backend: Backend::Mpi3(mpi3::adapter::order_targets(vec![h]).remove(0)),
        };
        let ordered = order(vec![mpi3, mpt, mega(1, "0000:03:00.0")]);
        let seen: Vec<(usize, &str)> = ordered.iter().map(|c| (c.id, c.driver.as_str())).collect();
        assert_eq!(
            seen,
            vec![(0, "megaraid_sas"), (1, "mpt3sas"), (2, "mpi3mr")]
        );
        let Backend::Mpt(t) = &ordered[1].backend else {
            panic!("not mpt");
        };
        assert_eq!(t.index, 1);
        let Backend::Mpi3(t) = &ordered[2].backend else {
            panic!("not mpi3");
        };
        assert_eq!((t.id, t.index), (2, 0));
    }

    #[test]
    fn list_keeps_going_when_a_controller_cannot_be_opened() {
        let l = list(&[mega(70000, "0000:03:00.0")]);
        assert_eq!(l.controllers.len(), 1);
        assert!(l.controllers[0].error.is_some());
        let mut out = String::new();
        l.render(&mut out);
        assert!(out.contains("error:"), "{out}");
    }

    fn filled_row() -> ControllerRow {
        ControllerRow {
            controller: 0,
            driver: "mpt3sas".into(),
            host: 2,
            pci_address: Some("0000:01:00.0".into()),
            model: Some("SAS3008".into()),
            serial_number: Some("SERIAL".into()),
            firmware_version: Some("16.00.12.00".into()),
            bios_version: Some("8.37.00.00".into()),
            state: Some("ready".into()),
            error: Some("failed".into()),
        }
    }

    #[test]
    fn controller_list_json_matches_the_go_fixtures() {
        use crate::output::Emit;
        use crate::tests::{assert_matches_fixture, go_fixture};
        let full = ControllerList {
            controllers: vec![filled_row()],
        }
        .json()
        .unwrap();
        assert!(full.is_array(), "{full}");
        for name in [
            "sasctl_controllers.json",
            "sasctl_controllers_mpt_down.json",
        ] {
            assert_matches_fixture(&full, &go_fixture(name), name);
        }
        let failed = list(&[mega(70000, "0000:03:00.0")]).json().unwrap();
        assert!(failed[0]["error"].is_string(), "{failed}");
        assert!(failed[0]["firmware_version"].is_null(), "{failed}");
        assert_matches_fixture(
            &failed,
            &go_fixture("sasctl_controllers_mpt_down.json"),
            "sasctl_controllers_mpt_down.json",
        );
    }

    #[test]
    fn list_text_prefers_the_error_then_the_state() {
        let mut ok = filled_row();
        ok.error = None;
        let mut plain = ok.clone();
        plain.state = None;
        plain.controller = 1;
        let mut broken = filled_row();
        broken.controller = 2;
        let l = ControllerList {
            controllers: vec![ok, plain, broken],
        };
        let mut out = String::new();
        l.render(&mut out);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[2].ends_with("ready"), "{out}");
        assert!(lines[3].ends_with("ok"), "{out}");
        assert!(lines[4].ends_with("error: failed"), "{out}");
        let mut empty = String::new();
        ControllerList::default().render(&mut empty);
        assert_eq!(empty, "No supported controllers found\n");
    }

    #[test]
    fn label_names_the_pci_address_when_known() {
        let c = mega(1, "0000:03:00.0");
        assert_eq!(c.label(), "Controller 99, megaraid_sas at 0000:03:00.0");
        let mut c = c;
        c.pci_address = None;
        assert_eq!(c.label(), "Controller 99, megaraid_sas");
    }

    #[test]
    fn controllers_without_a_pci_address_sort_last_by_host() {
        let mut a = mega(9, "0000:03:00.0");
        a.pci_address = None;
        let mut b = mega(2, "0000:03:00.0");
        b.pci_address = None;
        let c = mega(5, "0000:81:00.0");
        let ordered = order(vec![a, b, c]);
        let hosts: Vec<(usize, u32)> = ordered.iter().map(|c| (c.id, c.host_no)).collect();
        assert_eq!(hosts, vec![(0, 5), (1, 2), (2, 9)]);
    }
}
