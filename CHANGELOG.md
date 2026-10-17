# Changelog

## 0.2.0 — 2026-10-17

First release.

- Tabs for Overview, CPU, Memory, Graphics, Sensors & Power, Disks & Network, Processes and System.
- Graphics monitoring for AMD (amdgpu), NVIDIA (through nvidia-smi) and Intel (busy time estimated from idle residency), with names from pci.ids.
- Temperature sensors from hwmon, falling back to thermal zones on boards without hwmon; fans; several batteries combined; wireless device batteries.
- Disks including ZFS and bcachefs pools; network interfaces with IPv4/IPv6 addresses and Wi-Fi signal; optional VPN, Docker and VM interfaces.
- Process list with search, sorting, "only my processes", optional kernel threads and a details pane (view only).
- System tab with a hardware report that can be copied as text, without the computer name or MAC addresses.
- Light, dark or system theme; °C or °F; 0.5–5 s refresh; pause; keyboard shortcuts; `--help` and `--version`.
- `install.sh` for per-user or system-wide installs and uninstalls; release builds as .tar.gz, .deb and .rpm.
