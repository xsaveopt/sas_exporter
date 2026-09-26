# sas_exporter

Prometheus exporter for LSI/Broadcom SAS controllers. Exposes controller info and drive temperatures for Fusion-MPT HBAs (SAS2 and SAS3, IT or IR mode), 9600-series and PERC12 controllers, and MegaRAID controllers.

The exporter carries sasctl, the controller CLI in [sasctl/](sasctl/), and runs it to talk to the mpt2sas, mpt3sas, mpi3mr and megaraid_sas kernel drivers directly.
sasctl is also released as its own binary for managing the same controllers by hand.

## Requirements

- Linux
- Root privileges, since the driver interfaces need them

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
| `--hwmon.path` | `/sys/class/hwmon` | Path to hwmon sysfs root |

Override flags by editing `/etc/systemd/system/sas_exporter.service`.

## Metrics

| Metric | Description |
|---|---|
| `sas_controller_info` | Controller metadata (type, firmware, BIOS, PCI address) |
| `sas_physical_device_info` | Per-drive metadata (state, protocol, drive type, model, serial) |
| `sas_physical_device_temperature_celsius` | Drive temperature |
| `sas_controller_temperature_celsius` | Controller temperature (labels: `controller`, `sensor`, `label`). Sources: sasctl (`sensor="roc"` and `sensor="ctrl"` on MegaRAID, `sensor="ioc"` and `sensor="board"` on Fusion-MPT, `sensor="sensor0"` and up on 9600-series and PERC12) or hwmon sysfs |
| `sas_exporter_tool_up` | 1 if the named sasctl family (`mpt`, `mpi3` or `mega`) ran successfully, 0 otherwise (labels: `tool`) |

## Build from source

The build needs Go and the Rust toolchain pinned in sasctl/rust-toolchain.toml, since make builds sasctl first and embeds it.

```sh
make build
# binary at ./bin/sas_exporter
```
