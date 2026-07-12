# Running sas_exporter as a systemd service

sas_exporter is a single static binary. There's no installer — download it, drop
it somewhere on `PATH`, and (optionally) run it under systemd. This file is the
reference for the service setup; nothing here is required to just run the binary
by hand.

## 1. Install the binary

Download the latest release for your architecture from the
[releases page](https://github.com/xsaveopt/sas_exporter/releases/latest) and
copy it to `/usr/local/bin`:

```sh
ARCH=$(uname -m); case "$ARCH" in x86_64) ARCH=amd64 ;; aarch64) ARCH=arm64 ;; esac
curl -fL "https://github.com/xsaveopt/sas_exporter/releases/latest/download/sas_exporter_linux_${ARCH}" \
  -o /usr/local/bin/sas_exporter
sudo chmod +x /usr/local/bin/sas_exporter
```

## 2. Install the systemd unit

sas_exporter must run as root — the vendor tools (`sas2ircu`, `sas3ircu`,
`storcli`) require direct PCI access — so there's no dedicated service user.

Write `/etc/systemd/system/sas_exporter.service`:

```ini
[Unit]
Description=SAS HBA Prometheus Exporter
Documentation=https://github.com/xsaveopt/sas_exporter
After=network.target

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/sas_exporter
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

## 3. Enable and start

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now sas_exporter
```

## 4. Logs and status

```sh
sudo systemctl status sas_exporter
sudo journalctl -u sas_exporter -f
```
