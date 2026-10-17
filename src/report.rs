//! The "About this computer" report on the System tab, also copied to the
//! clipboard as plain text for help requests and forum posts.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::pci;
use crate::stats::{fmt_bytes, fmt_duration, read, read_num, read_trim, Snapshot, StaticInfo};

/// A titled group of label/value rows. Owned, so it can come from a remote agent.
pub type Section = (String, Vec<(String, String)>);

fn chassis(code: u32) -> &'static str {
    match code {
        3 | 4 | 5 | 6 | 7 | 15 | 16 | 24 => "Desktop",
        8 | 9 | 10 | 14 => "Laptop",
        11 => "Handheld",
        13 => "All-in-one",
        17 | 23 | 25 | 28 | 29 => "Server",
        30 => "Tablet",
        31 | 32 => "Convertible laptop",
        35 | 36 => "Mini PC",
        _ => "",
    }
}

/// A local date and time from a Unix timestamp.
fn local_time(unix: i64) -> String {
    let t = unix as libc::time_t;
    // SAFETY: localtime_r fills the zeroed struct from a valid time value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

fn dmi(field: &str) -> Option<String> {
    read_trim(format!("/sys/class/dmi/id/{field}"))
        // Placeholder text that some firmware leaves in.
        .filter(|v| {
            !matches!(
                v.to_lowercase().as_str(),
                "to be filled by o.e.m."
                    | "default string"
                    | "not applicable"
                    | "none"
                    | "system product name"
                    | "system manufacturer"
            )
        })
}

fn processor() -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let cpus: Vec<_> = fs::read_dir("/sys/devices/system/cpu")
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_prefix("cpu"))
                        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut cores = BTreeSet::new();
    let mut sockets = BTreeSet::new();
    for c in &cpus {
        let pkg = read_num::<i64>(c.join("topology/physical_package_id")).unwrap_or(0);
        let core = read_num::<i64>(c.join("topology/core_id")).unwrap_or(0);
        sockets.insert(pkg);
        cores.insert((pkg, core));
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(cpus.len());
    if !cores.is_empty() {
        rows.push(("Cores / threads".into(), format!("{} cores, {} threads", cores.len(), threads)));
    }
    if sockets.len() > 1 {
        rows.push(("Sockets".into(), sockets.len().to_string()));
    }
    if let Some(khz) = read_num::<f64>("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq") {
        rows.push(("Maximum clock".into(), format!("{:.2} GHz", khz / 1e6)));
    }
    let mut caches = Vec::new();
    for i in 0..8 {
        let base = format!("/sys/devices/system/cpu/cpu0/cache/index{i}");
        let (Some(level), Some(size)) = (read_trim(format!("{base}/level")), read_trim(format!("{base}/size"))) else {
            continue;
        };
        let kind = match read_trim(format!("{base}/type")).as_deref() {
            Some("Data") => "d",
            Some("Instruction") => "i",
            _ => "",
        };
        caches.push(format!("L{level}{kind} {size}"));
    }
    if !caches.is_empty() {
        rows.push(("Cache (per core / shared)".into(), caches.join(", ")));
    }
    if let Some(gov) = read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor") {
        rows.push(("Power governor".into(), gov));
    }
    let flags = read("/proc/cpuinfo").unwrap_or_default();
    let virt = if flags.contains(" vmx") {
        "Intel VT-x supported"
    } else if flags.contains(" svm") {
        "AMD-V supported"
    } else {
        ""
    };
    if !virt.is_empty() {
        rows.push(("Virtualisation".into(), virt.into()));
    }
    rows
}

fn disk_kind(name: &str, dev: &Path) -> &'static str {
    let link = fs::canonicalize(dev).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if link.contains("/usb") {
        "USB drive"
    } else if name.starts_with("nvme") {
        "NVMe SSD"
    } else if name.starts_with("mmcblk") {
        "SD card / eMMC"
    } else if name.starts_with("vd") || name.starts_with("xvd") {
        "Virtual disk"
    } else if read_num::<u8>(format!("/sys/block/{name}/queue/rotational")) == Some(1) {
        "Hard drive"
    } else if name.starts_with("sr") {
        "Optical drive"
    } else {
        "SSD"
    }
}

fn storage() -> Vec<(String, String)> {
    let Ok(dir) = fs::read_dir("/sys/block") else { return Vec::new() };
    let mut names: Vec<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let dev = Path::new("/sys/block").join(&name).join("device");
            if !dev.exists() {
                return None;
            }
            let sectors = read_num::<u64>(format!("/sys/block/{name}/size")).unwrap_or(0);
            if sectors == 0 {
                return None;
            }
            let model = [read_trim(dev.join("vendor")), read_trim(dev.join("model"))]
                .into_iter()
                .flatten()
                .filter(|s| s != "ATA")
                .collect::<Vec<_>>()
                .join(" ");
            let model = if model.is_empty() { read_trim(dev.join("name")).unwrap_or_default() } else { model };
            Some((
                name.clone(),
                format!(
                    "{} · {} · {}",
                    if model.is_empty() { "Unknown model" } else { &model },
                    fmt_bytes(sectors * 512),
                    disk_kind(&name, &dev)
                ),
            ))
        })
        .collect()
}

fn network() -> Vec<(String, String)> {
    let Ok(dir) = fs::read_dir("/sys/class/net") else { return Vec::new() };
    let mut names: Vec<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let base = Path::new("/sys/class/net").join(&name);
            let dev = base.join("device");
            if !dev.exists() {
                return None;
            }
            let kind = if base.join("wireless").exists() { "Wi-Fi" } else { "Ethernet" };
            let model = pci::full_name(&dev)
                .or_else(|| {
                    fs::read_link(dev.join("driver"))
                        .ok()
                        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                })
                .unwrap_or_default();
            let mut parts = vec![kind.to_string()];
            if !model.is_empty() {
                parts.push(model);
            }
            if let Some(speed) = read_num::<i64>(base.join("speed")).filter(|s| *s > 0) {
                parts.push(if speed >= 1000 {
                    format!("{} Gb/s link", speed / 1000)
                } else {
                    format!("{speed} Mb/s link")
                });
            }
            Some((name, parts.join(" · ")))
        })
        .collect()
}

fn swap_devices() -> String {
    read("/proc/swaps")
        .map(|s| {
            s.lines()
                .skip(1)
                .filter_map(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    let size = f.get(2)?.parse::<u64>().ok()? * 1024;
                    let kind =
                        if f[0].contains("zram") { "zram (compressed RAM)" } else { f.get(1).copied().unwrap_or("") };
                    Some(format!("{} {} ({})", f[0], kind, fmt_bytes(size)))
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

pub fn gather(info: &StaticInfo, s: &Snapshot) -> Vec<Section> {
    let mut sections = Vec::new();

    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().replace(':', " / ");
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut system = vec![
        ("Operating system".to_string(), info.os.clone()),
        ("Kernel".into(), format!("Linux {}", info.kernel)),
        ("Architecture".into(), info.arch.clone()),
    ];
    if !desktop.is_empty() || !session.is_empty() {
        let session = match session.as_str() {
            "wayland" => "Wayland",
            "x11" => "X11",
            "tty" => "console",
            other => other,
        };
        system.push(("Desktop".into(), format!("{desktop} ({session})").replace(" ()", "")));
    }
    system.push(("Computer name".into(), info.hostname.clone()));
    system.push(("Uptime".into(), fmt_duration(s.uptime)));
    system.push(("Started".into(), local_time(now as i64 - s.uptime as i64)));
    sections.push(("System", system));

    let mut computer = Vec::new();
    if !info.machine.is_empty() {
        computer.push(("Model".to_string(), info.machine.clone()));
    }
    if let Some(v) = dmi("product_version").filter(|v| v.len() > 1) {
        computer.push(("Version".into(), v));
    }
    if let Some(kind) = read_num::<u32>("/sys/class/dmi/id/chassis_type").map(chassis).filter(|k| !k.is_empty()) {
        computer.push(("Type".into(), kind.into()));
    }
    let board = [dmi("board_vendor"), dmi("board_name")].into_iter().flatten().collect::<Vec<_>>().join(" ");
    if !board.is_empty() {
        computer.push(("Motherboard".into(), board));
    }
    let bios = [dmi("bios_vendor"), dmi("bios_version")].into_iter().flatten().collect::<Vec<_>>().join(" ");
    if !bios.is_empty() {
        let date = dmi("bios_date").map(|d| format!(" ({d})")).unwrap_or_default();
        computer.push(("Firmware (BIOS/UEFI)".into(), format!("{bios}{date}")));
    }
    computer.push((
        "Boot mode".into(),
        if Path::new("/sys/firmware/efi").exists() { "UEFI".into() } else { "Legacy BIOS".into() },
    ));
    sections.push(("Computer", computer));

    let mut cpu = vec![("Model".to_string(), info.cpu_model.clone())];
    cpu.extend(processor());
    sections.push(("Processor", cpu));

    let mut mem = vec![("Installed (usable)".to_string(), fmt_bytes(s.mem.total))];
    mem.push((
        "Swap".into(),
        if s.mem.swap_total == 0 {
            "None".into()
        } else {
            format!("{} — {}", fmt_bytes(s.mem.swap_total), swap_devices())
        },
    ));
    sections.push(("Memory", mem));

    if !s.gpus.is_empty() {
        let rows = s
            .gpus
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let vram = g.vram_total.map(|v| format!(" · {} video memory", fmt_bytes(v))).unwrap_or_default();
                (format!("GPU {}", i + 1), format!("{} · driver {}{vram}", g.name, g.driver))
            })
            .collect();
        sections.push(("Graphics", rows));
    }

    let disks = storage();
    if !disks.is_empty() {
        sections.push(("Storage", disks));
    }
    let nets = network();
    if !nets.is_empty() {
        sections.push(("Network", nets));
    }

    if !s.batteries.is_empty() {
        let rows = s
            .batteries
            .iter()
            .map(|b| {
                let mut parts = Vec::new();
                if !b.model.is_empty() {
                    parts.push(b.model.clone());
                }
                if !b.technology.is_empty() {
                    parts.push(b.technology.clone());
                }
                if let Some(h) = b.health {
                    parts.push(format!("{h:.0}% of original capacity"));
                }
                if let Some(c) = b.cycles {
                    parts.push(format!("{c} cycles"));
                }
                (b.name.clone(), parts.join(" · "))
            })
            .collect();
        sections.push(("Battery", rows));
    }
    sections.into_iter().map(|(title, rows)| (title.to_string(), rows)).collect()
}

pub fn to_text(sections: &[Section]) -> String {
    let mut out = String::from("System report (sysmon)\n");
    for (title, rows) in sections {
        out.push_str(&format!("\n{title}\n"));
        // Left out so the text can be posted publicly without naming the owner.
        for (k, v) in rows.iter().filter(|(k, _)| k != "Computer name") {
            out.push_str(&format!("  {k}: {v}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_has_the_basics() {
        let info = StaticInfo::load();
        let s = Snapshot { uptime: 3600, ..Default::default() };
        let text = to_text(&gather(&info, &s));
        assert!(text.contains("Operating system: "));
        assert!(text.contains("Processor\n"));
        assert!(text.contains("Uptime: 1h 00m"));
        assert!(!text.contains("Computer name"));
    }

    #[test]
    fn chassis_codes() {
        assert_eq!(chassis(10), "Laptop");
        assert_eq!(chassis(3), "Desktop");
        assert_eq!(chassis(99), "");
    }
}
