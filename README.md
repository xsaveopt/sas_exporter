# sas_exporter

Prometheus exporter for LSI/Broadcom SAS controllers. Exposes controller info and drive temperatures via `sas2ircu` / `sas3ircu` (HBA/IT-mode) and `storcli` (MegaRAID).

## Requirements

- Linux
- One or more of the vendor tools below in `$PATH` (or specify paths via flags)
- Root privileges (tools require direct PCI access)

## Vendor tools

These are not bundled — download the one(s) matching your controller and place them in `$PATH`:

| Tool | Use for | Download |
|---|---|---|
| `sas3ircu` | HBA/IT-mode, 12Gb/s (LSI SAS 3008, etc.) | [IBM — sas3ircu v17.00.00.00 (Linux)](https://www.ibm.com/support/pages/sas3ircu-command-line-utility-storage-management-v17000000-linux-ibm-system-x) |
| `sas2ircu` | HBA/IT-mode, 6Gb/s (LSI SAS 2008, etc.) | [IBM — sas2ircu v18.00.00.00 (Linux)](https://www.ibm.com/support/pages/command-line-utility-storage-management-v18000000-linux-ibm-systems) |
| `storcli` | MegaRAID RAID controllers | [Broadcom — StorCLI](https://docs.broadcom.com/docs/1232743397) |

## Install

Download the binary for your architecture and run it. For a systemd service, see [docs/systemd.md](docs/systemd.md).

```sh
ARCH=$(uname -m); case "$ARCH" in x86_64) ARCH=amd64 ;; aarch64) ARCH=arm64 ;; esac
curl -fL "https://github.com/xsaveopt/sas_exporter/releases/latest/download/sas_exporter_linux_${ARCH}" \
  -o ./sas_exporter && chmod +x ./sas_exporter
```

Metrics are exposed on `:9856/metrics`.

## Flags

| Flag | Default | Description |
|---|---|---|
| `--web.listen-address` | `:9856` | Address to expose metrics on |
| `--web.telemetry-path` | `/metrics` | Path to expose metrics on |
| `--sas3ircu` | `sas3ircu` | Path to sas3ircu binary |
| `--sas2ircu` | `sas2ircu` | Path to sas2ircu binary |
| `--storcli` | `storcli` | Path to storcli binary |
| `--hwmon.path` | `/sys/class/hwmon` | Path to hwmon sysfs root |

Override flags by editing `/etc/systemd/system/sas_exporter.service`.

## Metrics

| Metric | Description |
|---|---|
| `sas_controller_info` | Controller metadata (type, firmware, BIOS, PCI address) |
| `sas_physical_device_info` | Per-drive metadata (state, protocol, drive type, model, serial) |
| `sas_physical_device_temperature_celsius` | Drive temperature |
| `sas_controller_temperature_celsius` | Controller temperature (labels: `controller`, `sensor`, `label`). Sources: storcli (`sensor="roc"`, `sensor="ctrl"`) or hwmon sysfs |
| `sas_exporter_tool_up` | 1 if the named tool ran successfully, 0 otherwise (labels: `tool`) |

## Build from source

```sh
make build
# binary at ./bin/sas_exporter
```
