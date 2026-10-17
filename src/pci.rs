//! Turns PCI vendor and device numbers into names, using the pci.ids file
//! that most distributions ship (hwdata / pciutils).

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const PATHS: [&str; 4] =
    ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/usr/share/pci.ids", "/usr/local/share/pci.ids"];

#[derive(Default)]
struct Db {
    vendors: HashMap<u16, String>,
    devices: HashMap<(u16, u16), String>,
}

fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = Db::default();
        let Some(text) = PATHS.iter().find_map(|p| std::fs::read(p).ok()) else { return db };
        let text = String::from_utf8_lossy(&text);
        let mut vendor = None;
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            // Device classes come after all vendors; nothing more to read.
            if line.starts_with("C ") {
                break;
            }
            if line.starts_with("\t\t") {
                continue;
            }
            let (id, name) = match line.trim_start().split_once("  ") {
                Some((id, name)) => (id, name.trim()),
                None => continue,
            };
            let Ok(id) = u16::from_str_radix(id, 16) else { continue };
            if line.starts_with('\t') {
                if let Some(v) = vendor {
                    db.devices.insert((v, id), name.to_string());
                }
            } else {
                vendor = Some(id);
                db.vendors.insert(id, name.to_string());
            }
        }
        db
    })
}

fn read_id(p: &Path) -> Option<u16> {
    let s = std::fs::read_to_string(p).ok()?;
    u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Vendor and device ids of a sysfs device directory (…/device).
pub fn ids(device_dir: &Path) -> Option<(u16, u16)> {
    Some((read_id(&device_dir.join("vendor"))?, read_id(&device_dir.join("device"))?))
}

/// A short vendor name for the common makers, otherwise the pci.ids name.
pub fn vendor_name(vendor: u16) -> String {
    match vendor {
        0x8086 => "Intel".into(),
        0x1002 | 0x1022 => "AMD".into(),
        0x10de => "NVIDIA".into(),
        0x14e4 => "Broadcom".into(),
        0x10ec => "Realtek".into(),
        0x168c | 0x17cb => "Qualcomm".into(),
        0x1af4 => "Virtio".into(),
        0x15ad => "VMware".into(),
        0x1234 => "QEMU".into(),
        _ => db().vendors.get(&vendor).cloned().unwrap_or_else(|| format!("{vendor:04x}")),
    }
}

/// The marketing name when pci.ids gives one in brackets
/// ("Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]" → "Radeon RX 6800/6800 XT / 6900 XT").
pub fn device_name(vendor: u16, device: u16) -> Option<String> {
    let name = db().devices.get(&(vendor, device))?;
    match (name.find('['), name.rfind(']')) {
        (Some(a), Some(b)) if b > a + 1 => Some(name[a + 1..b].to_string()),
        _ => Some(name.clone()),
    }
}

/// "Intel HD Graphics 620", falling back to the raw ids.
pub fn full_name(device_dir: &Path) -> Option<String> {
    let (v, d) = ids(device_dir)?;
    let vendor = vendor_name(v);
    Some(match device_name(v, d) {
        Some(n) if n.starts_with(&vendor) => n,
        Some(n) => format!("{vendor} {n}"),
        None => format!("{vendor} device {d:04x}"),
    })
}
