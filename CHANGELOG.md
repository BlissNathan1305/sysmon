# Changelog

## 0.3.1 — 2026-10-17

- On a system with no fonts at all, sysmon now says which font package to install instead of crashing twice.
- The .deb depends on a basic font and the AUR packages on `ttf-font`; the .rpm recommends DejaVu Sans.
- Tested from the release packages on Ubuntu 22.04 (with and without Mesa), Debian 12, Fedora and Arch.

## 0.3.0 — 2026-10-17

- New **Settings** tab: theme, temperature unit, refresh rate, alerts and export in one place.
- **Desktop alerts** while sysmon is open: CPU too hot, memory or disk almost full, battery low. On by default with adjustable limits; temperature and memory must stay high for 15 seconds, and each alert repeats at most every 10 minutes. Also shown in a strip at the top of the window.
- **Export history** to CSV (Ctrl+E) in the Downloads folder.
- The window size is remembered.
- Machines without a usable GPU driver (virtual machines, no Mesa) now get a software-drawn window instead of a crash, and a missing display library gives clear install instructions.
- The `.deb` and `.rpm` now declare the display libraries the window loads at run time; tested on Ubuntu 22.04, Debian 12, Fedora and Arch.
- Card shadows removed: they drew as black boxes with the software renderer.
- Single files bind-mounted from a disk (containers) are no longer listed as file systems.
- Home-folder paths are shown as `~`.
- AppStream metadata for software centres, AUR recipes (`sysmon` and `sysmon-bin`), and ARM64 release builds.
- Tests with fake `/sys` trees for AMD and Intel graphics, AMD sensors and fans, ARM boards, several batteries and wireless devices.

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
