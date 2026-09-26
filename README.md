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

| Flag                   | Default            | Description                                                                                                                 |
| ---------------------- | ------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| `--web.listen-address` | `:9856`            | Address to expose metrics on                                                                                                |
| `--web.telemetry-path` | `/metrics`         | Path to expose metrics on                                                                                                   |
| `--hwmon.path`         | `/sys/class/hwmon` | Path to hwmon sysfs root                                                                                                    |
| `--drive.errors`       | `false`            | Export media, other, predictive failure and bad block counts for each MegaRAID drive                                        |
| `--drive.smart`        | `false`            | Export SMART health for each MegaRAID and 9600-series drive, plus ATA attributes on MegaRAID and NVMe health on 9600-series |
| `--drive.locate`       | `false`            | Export whether the locate LED is on for each MegaRAID drive                                                                 |
| `--drive.progress`     | `false`            | Export rebuild, patrol, clear and erase progress for each MegaRAID drive                                                    |

Each of the drive flags runs sasctl once per drive on every scrape, and SMART reads can wake drives that have spun down.

Override flags by editing `/etc/systemd/system/sas_exporter.service`.

## Metrics

| Metric                                                                                                                                                                      | Description                                                                                                                                                                                                                                               |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sas_controller_info`                                                                                                                                                       | Controller metadata (type, firmware, BIOS, PCI address)                                                                                                                                                                                                   |
| `sas_physical_device_info`                                                                                                                                                  | Per-drive metadata (state, protocol, drive type, model, serial)                                                                                                                                                                                           |
| `sas_physical_device_temperature_celsius`                                                                                                                                   | Drive temperature                                                                                                                                                                                                                                         |
| `sas_controller_temperature_celsius`                                                                                                                                        | Controller temperature (labels: `controller`, `sensor`, `label`). Sources: sasctl (`sensor="roc"` and `sensor="ctrl"` on MegaRAID, `sensor="ioc"` and `sensor="board"` on Fusion-MPT, `sensor="sensor0"` and up on 9600-series and PERC12) or hwmon sysfs |
| `sas_controller_ioc_exceptions`                                                                                                                                             | Raw IOC exceptions bitmask on Fusion-MPT and 9600-series                                                                                                                                                                                                  |
| `sas_controller_devices`                                                                                                                                                    | Devices a 9600-series controller sees (labels: `type`)                                                                                                                                                                                                    |
| `sas_controller_volumes_degraded`, `sas_controller_volumes_offline`                                                                                                         | Volume health counts on MegaRAID                                                                                                                                                                                                                          |
| `sas_controller_drives_failed`, `sas_controller_drives_predictive_failure`                                                                                                  | Drive health counts on MegaRAID                                                                                                                                                                                                                           |
| `sas_controller_memory_errors_total`                                                                                                                                        | Controller memory errors on MegaRAID (labels: `type`)                                                                                                                                                                                                     |
| `sas_controller_battery_present`, `sas_controller_alarm_present`                                                                                                            | Whether a MegaRAID controller has a battery and an alarm                                                                                                                                                                                                  |
| `sas_phy_enabled`, `sas_phy_link_rate_gbps`, `sas_phy_max_link_rate_gbps`                                                                                                   | Controller phy state and link rate on Fusion-MPT and 9600-series (labels: `phy`)                                                                                                                                                                          |
| `sas_phy_errors_total`                                                                                                                                                      | Controller phy link error counters (labels: `phy`, `type`)                                                                                                                                                                                                |
| `sas_volume_info`                                                                                                                                                           | RAID volume metadata (labels: `volume`, `name`, `raid_level`, `state`)                                                                                                                                                                                    |
| `sas_volume_operation_progress_percent`                                                                                                                                     | Progress of a running volume operation (labels: `volume`, `operation`)                                                                                                                                                                                    |
| `sas_battery_*`                                                                                                                                                             | Charge, capacity, cycles, voltage, current, temperature and health of a MegaRAID battery                                                                                                                                                                  |
| `sas_patrol_info`, `sas_patrol_iterations_total`, `sas_patrol_drives_done`, `sas_patrol_next_run_seconds`                                                                   | MegaRAID patrol read state and schedule                                                                                                                                                                                                                   |
| `sas_physical_device_media_errors_total`, `sas_physical_device_other_errors_total`, `sas_physical_device_predictive_failures_total`, `sas_physical_device_bbm_errors_total` | Drive error counts, with `--drive.errors`                                                                                                                                                                                                                 |
| `sas_physical_device_smart_healthy`, `sas_physical_device_ata_smart_attribute_*`, `sas_physical_device_nvme_*`                                                              | Drive SMART data, with `--drive.smart`                                                                                                                                                                                                                    |
| `sas_physical_device_locate_active`                                                                                                                                         | Drive locate LED, with `--drive.locate`                                                                                                                                                                                                                   |
| `sas_physical_device_operation_progress_percent`                                                                                                                            | Progress of a running drive operation (labels: `operation`), with `--drive.progress`                                                                                                                                                                      |
| `sas_exporter_tool_up`                                                                                                                                                      | 1 if the named sasctl family (`mpt`, `mpi3` or `mega`) ran successfully, 0 otherwise (labels: `tool`)                                                                                                                                                     |

## Build from source

The build needs Go and the Rust toolchain pinned in sasctl/rust-toolchain.toml, since make builds sasctl first and embeds it.

```sh
make build
# binary at ./bin/sas_exporter
```
