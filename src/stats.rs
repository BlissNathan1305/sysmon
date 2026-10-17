//! Reads the machine's state straight from /proc and /sys.
//!
//! Counters such as CPU ticks or network bytes only mean something as a
//! difference between two readings, so `Collector` keeps the previous values
//! and turns them into rates on every `sample()`.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::gpu::{self, Gpu};

pub(crate) fn read(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok()
}

pub(crate) fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    read(p).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub(crate) fn read_num<T: FromStr>(p: impl AsRef<Path>) -> Option<T> {
    read_trim(p)?.parse().ok()
}

/// Show temperatures in Fahrenheit instead of Celsius.
pub static FAHRENHEIT: AtomicBool = AtomicBool::new(false);

pub fn fmt_temp(c: f32) -> String {
    if FAHRENHEIT.load(Ordering::Relaxed) {
        format!("{:.0} °F", c * 9.0 / 5.0 + 32.0)
    } else {
        format!("{c:.0} °C")
    }
}

pub fn fmt_temp_exact(c: f32) -> String {
    if FAHRENHEIT.load(Ordering::Relaxed) {
        format!("{:.1} °F", c * 9.0 / 5.0 + 32.0)
    } else {
        format!("{c:.1} °C")
    }
}

/// Things that do not change while the program runs.
#[derive(Debug, Clone, Default)]
pub struct StaticInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_threads: usize,
    pub machine: String,
}

/// The CPU's name. x86 has "model name"; ARM boards often only name the
/// board, so fall back through the other places it can be found.
fn cpu_model(cpuinfo: &str) -> Option<String> {
    let field = |key: &str| {
        cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key && !v.trim().is_empty()).then(|| v.split_whitespace().collect::<Vec<_>>().join(" "))
        })
    };
    field("model name")
        .or_else(|| field("Model"))
        .or_else(|| field("Hardware"))
        .or_else(|| field("cpu model"))
        .or_else(|| field("cpu"))
        .or_else(|| read_trim("/sys/firmware/devicetree/base/model").map(|m| m.trim_end_matches('\0').to_string()))
}

pub fn uname_machine() -> String {
    // SAFETY: utsname is a plain C struct that uname fills in.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return String::new();
    }
    unsafe { CStr::from_ptr(u.machine.as_ptr()) }.to_string_lossy().into_owned()
}

impl StaticInfo {
    pub fn load() -> Self {
        let os = read("/etc/os-release")
            .or_else(|| read("/usr/lib/os-release"))
            .and_then(|s| {
                s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
            })
            .unwrap_or_else(|| "Linux".into());
        let vendor = read_trim("/sys/class/dmi/id/sys_vendor").unwrap_or_default();
        let product = read_trim("/sys/class/dmi/id/product_name").unwrap_or_default();
        let machine = if product.is_empty() {
            read_trim("/sys/firmware/devicetree/base/model")
                .map(|m| m.trim_end_matches('\0').to_string())
                .unwrap_or_default()
        } else if product.starts_with(&vendor) {
            product
        } else {
            format!("{vendor} {product}").trim().to_string()
        };
        StaticInfo {
            hostname: read_trim("/proc/sys/kernel/hostname").unwrap_or_else(|| "localhost".into()),
            os,
            kernel: read_trim("/proc/sys/kernel/osrelease").unwrap_or_default(),
            arch: uname_machine(),
            cpu_model: read("/proc/cpuinfo").and_then(|c| cpu_model(&c)).unwrap_or_else(|| "Unknown processor".into()),
            cpu_threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            machine,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub used: u64,
    pub cached: u64,
    pub buffers: u64,
    pub free: u64,
    pub shared: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

impl Memory {
    pub fn used_pct(&self) -> f32 {
        pct(self.used, self.total)
    }
    pub fn swap_pct(&self) -> f32 {
        pct(self.swap_used, self.swap_total)
    }
}

#[derive(Debug, Clone)]
pub struct Temp {
    pub chip: String,
    pub label: String,
    pub celsius: f32,
    pub high: Option<f32>,
    pub crit: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct Fan {
    pub label: String,
    pub rpm: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Battery {
    pub name: String,
    pub model: String,
    pub percent: f32,
    pub status: String,
    pub power_w: Option<f32>,
    /// Seconds until empty (discharging) or full (charging).
    pub time_left: Option<u64>,
    pub health: Option<f32>,
    pub cycles: Option<u32>,
    pub voltage: Option<f32>,
    pub technology: String,
    /// Charge now and when full, in the battery's own units (µAh or µWh),
    /// and the rate in the same units per hour. Used to combine batteries.
    pub now: f64,
    pub full: f64,
    pub rate: Option<f64>,
}

/// A battery in a wireless mouse, keyboard, headset or similar.
#[derive(Debug, Clone)]
pub struct DeviceBattery {
    pub name: String,
    pub percent: Option<f32>,
    pub level: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub device: String,
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub used: u64,
    pub avail: u64,
}

impl Mount {
    pub fn used_pct(&self) -> f32 {
        pct(self.used, self.used + self.avail)
    }
}

#[derive(Debug, Clone)]
pub struct DiskIo {
    pub name: String,
    pub read_bps: f64,
    pub write_bps: f64,
    pub read_total: u64,
    pub write_total: u64,
}

#[derive(Debug, Clone)]
pub struct Net {
    pub name: String,
    pub up: bool,
    pub wireless: bool,
    /// VPN tunnels, bridges, Docker and VM networks: no physical card.
    pub virtual_: bool,
    pub addrs: Vec<String>,
    pub addrs6: Vec<String>,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    /// Signal strength in dBm, for Wi-Fi.
    pub signal_dbm: Option<i32>,
    pub link_quality: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub user: String,
    pub uid: u32,
    pub state: char,
    /// Share of the whole CPU (all cores together = 100 %).
    pub cpu: f32,
    pub mem: u64,
    pub virt: u64,
    pub threads: u32,
    pub nice: i32,
    /// Seconds after boot that the process started.
    pub started: u64,
    pub kernel: bool,
    pub cmd: String,
}

/// One full reading of the machine.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub cpu_total: f32,
    pub cpu_cores: Vec<f32>,
    pub freqs_mhz: Vec<f32>,
    pub load: [f32; 3],
    pub uptime: u64,
    pub tasks: usize,
    pub threads: usize,
    pub running: usize,
    pub mem: Memory,
    pub temps: Vec<Temp>,
    pub fans: Vec<Fan>,
    pub batteries: Vec<Battery>,
    pub device_batteries: Vec<DeviceBattery>,
    pub ac_online: Option<bool>,
    pub mounts: Vec<Mount>,
    pub disk_io: Vec<DiskIo>,
    pub nets: Vec<Net>,
    pub gpus: Vec<Gpu>,
    pub procs: Vec<Proc>,
}

/// Sensor chips that measure the CPU itself, across Intel, AMD and ARM.
const CPU_CHIPS: [&str; 7] =
    ["coretemp", "k10temp", "zenpower", "cpu_thermal", "soc_thermal", "x86_pkg_temp", "cpu-thermal"];

/// What the overview shows for power: one battery, or several added up.
pub struct BatterySummary {
    pub percent: f32,
    pub status: String,
    pub power_w: Option<f32>,
    pub time_left: Option<u64>,
    pub count: usize,
}

impl Snapshot {
    /// The temperature that best stands for "the CPU".
    pub fn cpu_temp(&self) -> Option<f32> {
        let core: Vec<&Temp> = self.temps.iter().filter(|t| CPU_CHIPS.contains(&t.chip.as_str())).collect();
        core.iter()
            .find(|t| t.label.starts_with("Package") || t.label == "Tctl" || t.label == "Tdie")
            .map(|t| t.celsius)
            .or_else(|| core.iter().map(|t| t.celsius).reduce(f32::max))
            .or_else(|| self.temps.iter().map(|t| t.celsius).reduce(f32::max))
    }

    pub fn battery(&self) -> Option<BatterySummary> {
        let first = self.batteries.first()?;
        if self.batteries.len() == 1 {
            return Some(BatterySummary {
                percent: first.percent,
                status: first.status.clone(),
                power_w: first.power_w,
                time_left: first.time_left,
                count: 1,
            });
        }
        let now: f64 = self.batteries.iter().map(|b| b.now).sum();
        let full: f64 = self.batteries.iter().map(|b| b.full).sum();
        let rate: f64 = self.batteries.iter().filter_map(|b| b.rate).sum();
        let status = if self.batteries.iter().any(|b| b.status == "Discharging") {
            "Discharging"
        } else if self.batteries.iter().any(|b| b.status == "Charging") {
            "Charging"
        } else {
            first.status.as_str()
        };
        let time_left = (rate > 0.0)
            .then(|| match status {
                "Discharging" => Some(now / rate),
                "Charging" => Some((full - now).max(0.0) / rate),
                _ => None,
            })
            .flatten()
            .map(|h| (h * 3600.0) as u64);
        let power: f32 = self.batteries.iter().filter_map(|b| b.power_w).sum();
        Some(BatterySummary {
            percent: if full > 0.0 { (now / full * 100.0) as f32 } else { first.percent },
            status: status.to_string(),
            power_w: (power > 0.0).then_some(power),
            time_left,
            count: self.batteries.len(),
        })
    }

    pub fn disk_read(&self) -> f64 {
        self.disk_io.iter().map(|d| d.read_bps).sum()
    }
    pub fn disk_write(&self) -> f64 {
        self.disk_io.iter().map(|d| d.write_bps).sum()
    }
    /// Traffic through physical cards only; VPN traffic would count twice.
    pub fn net_rx(&self) -> f64 {
        self.nets.iter().filter(|n| !n.virtual_).map(|n| n.rx_bps).sum()
    }
    pub fn net_tx(&self) -> f64 {
        self.nets.iter().filter(|n| !n.virtual_).map(|n| n.tx_bps).sum()
    }
}

pub fn pct(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 100.0) as f32
    }
}

#[derive(Clone, Copy, Default)]
struct CpuTicks {
    total: u64,
    idle: u64,
}

pub struct Collector {
    last: Option<Instant>,
    cpu: Vec<CpuTicks>,
    disks: HashMap<String, (u64, u64)>,
    nets: HashMap<String, (u64, u64)>,
    procs: HashMap<u32, u64>,
    users: HashMap<u32, String>,
    gpus: gpu::Probe,
    clk_tck: f64,
    page_size: u64,
    threads: usize,
}

impl Collector {
    pub fn new(threads: usize) -> Self {
        // SAFETY: sysconf only reads system constants.
        let (clk, page) = unsafe { (libc::sysconf(libc::_SC_CLK_TCK), libc::sysconf(libc::_SC_PAGESIZE)) };
        Collector {
            last: None,
            cpu: Vec::new(),
            disks: HashMap::new(),
            nets: HashMap::new(),
            procs: HashMap::new(),
            users: HashMap::new(),
            gpus: gpu::Probe::new(),
            clk_tck: if clk > 0 { clk as f64 } else { 100.0 },
            page_size: if page > 0 { page as u64 } else { 4096 },
            threads: threads.max(1),
        }
    }

    pub fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let dt = self.last.map(|l| now.duration_since(l).as_secs_f64()).filter(|d| *d > 0.0);
        self.last = Some(now);

        let mut s = Snapshot::default();
        self.read_cpu(&mut s);
        s.freqs_mhz = (0..s.cpu_cores.len())
            .map(|i| {
                read_num::<f32>(format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"))
                    .map(|khz| khz / 1000.0)
                    .unwrap_or(0.0)
            })
            .collect();
        if let Some(l) = read("/proc/loadavg") {
            let v: Vec<f32> = l.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                s.load = [v[0], v[1], v[2]];
            }
        }
        s.uptime = read("/proc/uptime")
            .and_then(|u| u.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()))
            .map(|x| x as u64)
            .unwrap_or(0);
        s.mem = read_memory();
        let (temps, fans) = read_hwmon(Path::new("/sys/class/hwmon"));
        s.temps = if temps.is_empty() { read_thermal_zones(Path::new("/sys/class/thermal")) } else { temps };
        s.fans = fans;
        let (bats, devices, ac) = read_power(Path::new("/sys/class/power_supply"));
        s.batteries = bats;
        s.device_batteries = devices;
        s.ac_online = ac;
        s.mounts = read_mounts();
        s.disk_io = self.read_disk_io(dt);
        s.nets = self.read_nets(dt);
        s.gpus = self.gpus.sample();
        self.read_procs(&mut s, dt);
        s
    }

    fn read_cpu(&mut self, s: &mut Snapshot) {
        let Some(stat) = read("/proc/stat") else { return };
        let ticks: Vec<CpuTicks> = stat
            .lines()
            .take_while(|l| l.starts_with("cpu"))
            .map(|l| {
                let v: Vec<u64> = l.split_whitespace().skip(1).take(8).filter_map(|x| x.parse().ok()).collect();
                let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
                CpuTicks { total: v.iter().sum(), idle }
            })
            .collect();
        let usage: Vec<f32> = ticks
            .iter()
            .enumerate()
            .map(|(i, t)| match self.cpu.get(i) {
                Some(p) if t.total > p.total => {
                    let dt = (t.total - p.total) as f64;
                    let di = t.idle.saturating_sub(p.idle) as f64;
                    ((1.0 - di / dt) * 100.0).clamp(0.0, 100.0) as f32
                }
                _ => 0.0,
            })
            .collect();
        self.cpu = ticks;
        if let Some((first, rest)) = usage.split_first() {
            s.cpu_total = *first;
            s.cpu_cores = rest.to_vec();
        }
    }

    fn read_disk_io(&mut self, dt: Option<f64>) -> Vec<DiskIo> {
        let Some(stats) = read("/proc/diskstats") else { return Vec::new() };
        let mut out = Vec::new();
        let mut seen = HashMap::new();
        for line in stats.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let name = f[2];
            // Whole physical disks only: partitions and device-mapper volumes
            // would count the same bytes twice.
            if !Path::new(&format!("/sys/block/{name}/device")).exists() {
                continue;
            }
            let rd = f[5].parse::<u64>().unwrap_or(0) * 512;
            let wr = f[9].parse::<u64>().unwrap_or(0) * 512;
            let (read_bps, write_bps) = match (dt, self.disks.get(name)) {
                (Some(dt), Some(&(pr, pw))) => (rd.saturating_sub(pr) as f64 / dt, wr.saturating_sub(pw) as f64 / dt),
                _ => (0.0, 0.0),
            };
            seen.insert(name.to_string(), (rd, wr));
            out.push(DiskIo { name: name.to_string(), read_bps, write_bps, read_total: rd, write_total: wr });
        }
        self.disks = seen;
        out
    }

    fn read_nets(&mut self, dt: Option<f64>) -> Vec<Net> {
        let Some(dev) = read("/proc/net/dev") else { return Vec::new() };
        let (addrs, addrs6) = if_addrs();
        let wireless = read_wireless();
        let mut out = Vec::new();
        let mut seen = HashMap::new();
        for line in dev.lines().skip(2) {
            let Some((name, rest)) = line.split_once(':') else { continue };
            let name = name.trim();
            if name == "lo" {
                continue;
            }
            let v: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() < 9 {
                continue;
            }
            let (rx, tx) = (v[0], v[8]);
            let (rx_bps, tx_bps) = match (dt, self.nets.get(name)) {
                (Some(dt), Some(&(pr, pt))) => (rx.saturating_sub(pr) as f64 / dt, tx.saturating_sub(pt) as f64 / dt),
                _ => (0.0, 0.0),
            };
            seen.insert(name.to_string(), (rx, tx));
            let w = wireless.get(name);
            let state = read_trim(format!("/sys/class/net/{name}/operstate")).unwrap_or_default();
            out.push(Net {
                name: name.to_string(),
                // Tunnels report "unknown" while working.
                up: state == "up" || (state == "unknown" && addrs.contains_key(name)),
                wireless: Path::new(&format!("/sys/class/net/{name}/wireless")).exists(),
                virtual_: !Path::new(&format!("/sys/class/net/{name}/device")).exists(),
                addrs: addrs.get(name).cloned().unwrap_or_default(),
                addrs6: addrs6.get(name).cloned().unwrap_or_default(),
                rx_bps,
                tx_bps,
                rx_total: rx,
                tx_total: tx,
                signal_dbm: w.map(|w| w.1),
                link_quality: w.map(|w| w.0),
            });
        }
        // Physical cards first, then connected before disconnected.
        out.sort_by_key(|n| (n.virtual_, !n.up, n.name.clone()));
        self.nets = seen;
        out
    }

    fn user_name(&mut self, uid: u32) -> String {
        self.users.entry(uid).or_insert_with(|| lookup_user(uid).unwrap_or_else(|| uid.to_string())).clone()
    }

    fn read_procs(&mut self, s: &mut Snapshot, dt: Option<f64>) {
        let Ok(dir) = fs::read_dir("/proc") else { return };
        let mut seen = HashMap::new();
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            let base = entry.path();
            // A process can exit between listing and reading; just skip it.
            let Some(stat) = read(base.join("stat")) else { continue };
            let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else { continue };
            let name = stat[open + 1..close].to_string();
            let f: Vec<&str> = stat[close + 1..].split_whitespace().collect();
            if f.len() < 22 {
                continue;
            }
            let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
            let state = f[0].chars().next().unwrap_or('?');
            let ppid = n(1) as u32;
            let ticks = n(11) + n(12);
            let threads = n(17) as u32;
            let cpu = match (dt, self.procs.get(&pid)) {
                (Some(dt), Some(&prev)) => {
                    (ticks.saturating_sub(prev) as f64 / self.clk_tck / dt / self.threads as f64 * 100.0) as f32
                }
                _ => 0.0,
            };
            seen.insert(pid, ticks);
            let uid = fs::metadata(&base).map(|m| m.uid()).unwrap_or(0);
            let user = self.user_name(uid);
            let kernel = pid == 2 || ppid == 2;
            let cmd = read(base.join("cmdline"))
                .map(|c| c.trim_end_matches('\0').replace('\0', " "))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| format!("[{name}]"));
            if state == 'R' {
                s.running += 1;
            }
            s.threads += threads as usize;
            s.procs.push(Proc {
                pid,
                ppid,
                name,
                user,
                uid,
                state,
                cpu: cpu.clamp(0.0, 100.0),
                mem: n(21) * self.page_size,
                virt: n(20),
                threads,
                nice: f[16].parse().unwrap_or(0),
                started: (n(19) as f64 / self.clk_tck) as u64,
                kernel,
                cmd,
            });
        }
        s.tasks = s.procs.len();
        self.procs = seen;
    }
}

/// Looks the account up through NSS, so LDAP and other network accounts
/// get their names too, not only those in /etc/passwd.
fn lookup_user(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: getpwuid_r writes into `pwd` and `buf`, both owned here and
    // large enough; `result` is checked before use.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
        if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
            return None;
        }
        Some(CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned())
    }
}

fn read_memory() -> Memory {
    let mut kv = HashMap::new();
    if let Some(m) = read("/proc/meminfo") {
        for line in m.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let n = v.split_whitespace().next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
                kv.insert(k.to_string(), n * 1024);
            }
        }
    }
    let g = |k: &str| kv.get(k).copied().unwrap_or(0);
    let total = g("MemTotal");
    // Very old kernels have no MemAvailable; estimate it the classic way.
    let available = kv.get("MemAvailable").copied().unwrap_or(g("MemFree") + g("Buffers") + g("Cached"));
    Memory {
        total,
        available,
        used: total.saturating_sub(available),
        cached: g("Cached") + g("SReclaimable"),
        buffers: g("Buffers"),
        free: g("MemFree"),
        shared: g("Shmem"),
        swap_total: g("SwapTotal"),
        swap_used: g("SwapTotal").saturating_sub(g("SwapFree")),
    }
}

/// Temperatures and fans from every hwmon chip under `root` (/sys/class/hwmon).
fn read_hwmon(root: &Path) -> (Vec<Temp>, Vec<Fan>) {
    let mut temps = Vec::new();
    let mut fans = Vec::new();
    let Ok(dir) = fs::read_dir(root) else { return (temps, fans) };
    let mut chips: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    chips.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.trim_start_matches("hwmon").parse::<u32>().ok())
            .unwrap_or(0)
    });
    for chip in chips {
        let name = read_trim(chip.join("name")).unwrap_or_else(|| "sensor".into());
        // GPU sensors are shown with the GPU.
        if matches!(name.as_str(), "amdgpu" | "radeon" | "nouveau") {
            continue;
        }
        let Ok(files) = fs::read_dir(&chip) else { continue };
        let mut inputs: Vec<String> = files
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with("_input") && (n.starts_with("temp") || n.starts_with("fan")))
            .collect();
        inputs.sort_by_key(|n| {
            (
                n.starts_with("fan"),
                n.trim_start_matches(|c: char| c.is_alphabetic())
                    .trim_end_matches("_input")
                    .parse::<u32>()
                    .unwrap_or(0),
            )
        });
        for input in inputs {
            let prefix = input.trim_end_matches("_input");
            let label = read_trim(chip.join(format!("{prefix}_label")));
            if prefix.starts_with("temp") {
                // Unused sensors read 0 and some report junk limits (65262 °C).
                let Some(milli) = read_num::<f32>(chip.join(&input)).filter(|m| *m > 500.0 && *m < 150_000.0) else {
                    continue;
                };
                let limit = |s: &str| {
                    read_num::<f32>(chip.join(format!("{prefix}_{s}")))
                        .map(|v| v / 1000.0)
                        .filter(|v| *v > 0.0 && *v < 200.0)
                };
                temps.push(Temp {
                    chip: name.clone(),
                    label: label.unwrap_or_else(|| friendly_chip(&name)),
                    celsius: milli / 1000.0,
                    high: limit("max"),
                    crit: limit("crit"),
                });
            } else if let Some(rpm) = read_num::<u32>(chip.join(&input)) {
                fans.push(Fan { label: label.unwrap_or_else(|| format!("{} {prefix}", friendly_chip(&name))), rpm });
            }
        }
    }
    (temps, fans)
}

/// For machines whose sensors only appear as thermal zones (many ARM boards).
fn read_thermal_zones(root: &Path) -> Vec<Temp> {
    let Ok(dir) = fs::read_dir(root) else { return Vec::new() };
    let mut zones: Vec<_> =
        dir.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().contains("thermal_zone")).collect();
    zones.sort();
    zones
        .into_iter()
        .filter_map(|z| {
            let kind = read_trim(z.join("type")).unwrap_or_else(|| "zone".into());
            let milli = read_num::<f32>(z.join("temp")).filter(|m| *m > 500.0)?;
            Some(Temp {
                chip: kind.clone(),
                label: friendly_chip(&kind),
                celsius: milli / 1000.0,
                high: None,
                crit: None,
            })
        })
        .collect()
}

/// A readable name for a hwmon chip.
pub fn friendly_chip(name: &str) -> String {
    let lower = name.to_lowercase();
    if CPU_CHIPS.contains(&lower.as_str()) {
        "CPU".into()
    } else if lower.starts_with("nvme") {
        "NVMe SSD".into()
    } else if lower == "drivetemp" {
        "Hard drive / SSD".into()
    } else if lower.starts_with("iwlwifi")
        || lower.starts_with("ath")
        || lower.starts_with("mt7")
        || lower.starts_with("rtw")
    {
        "Wi-Fi card".into()
    } else if lower.starts_with("pch") {
        "Chipset (PCH)".into()
    } else if let Some(n) = lower.strip_prefix("acpitz") {
        let n = n.trim_start_matches('_');
        if n.is_empty() {
            "ACPI thermal zone".into()
        } else {
            format!("ACPI thermal zone {n}")
        }
    } else if lower.starts_with("bat") {
        "Battery".into()
    } else if lower.starts_with("nct")
        || lower.starts_with("it87")
        || lower.starts_with("f71")
        || lower.starts_with("asus")
    {
        "Motherboard".into()
    } else if lower.contains("dimm") || lower.starts_with("spd5118") || lower.starts_with("jc42") {
        "Memory modules".into()
    } else {
        name.to_string()
    }
}

/// Batteries, device batteries and charger state under `root` (/sys/class/power_supply).
fn read_power(root: &Path) -> (Vec<Battery>, Vec<DeviceBattery>, Option<bool>) {
    let mut batteries = Vec::new();
    let mut devices = Vec::new();
    let mut ac = None;
    let Ok(dir) = fs::read_dir(root) else { return (batteries, devices, ac) };
    let mut entries: Vec<_> = dir.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let p = entry.path();
        let kind = read_trim(p.join("type")).unwrap_or_default();
        let name = entry.file_name().to_string_lossy().into_owned();
        // USB-C chargers on laptops without a "Mains" supply show up as ucsi-*.
        if kind == "Mains" || (kind == "USB" && name.starts_with("ucsi")) {
            if let Some(on) = read_num::<u8>(p.join("online")) {
                ac = Some(ac.unwrap_or(false) || on == 1);
            }
            continue;
        }
        if kind != "Battery" || read_num::<u8>(p.join("present")).unwrap_or(1) != 1 {
            continue;
        }
        let model = read_trim(p.join("model_name")).unwrap_or_default();
        let status = read_trim(p.join("status")).unwrap_or_else(|| "Unknown".into());
        // Batteries of mice, keyboards and headsets say scope=Device.
        if read_trim(p.join("scope")).as_deref() == Some("Device") {
            devices.push(DeviceBattery {
                name: if model.is_empty() { name } else { model },
                percent: read_num::<f32>(p.join("capacity")),
                level: read_trim(p.join("capacity_level")).unwrap_or_default(),
                status,
            });
            continue;
        }
        let n = |f: &str| read_num::<f64>(p.join(f));
        let voltage = n("voltage_now").map(|v| v / 1e6);
        let current = n("current_now").map(|v| v.abs() / 1e6).filter(|v| *v > 0.0);
        let power = n("power_now").map(|v| v.abs() / 1e6).filter(|v| *v > 0.0);
        // Batteries report either charge (µAh, with current) or energy
        // (µWh, with power). Use whichever pair exists.
        let (now, full, design, rate) = match (n("energy_now"), n("energy_full")) {
            (Some(now), Some(full)) => (now, full, n("energy_full_design"), power.map(|w| w * 1e6)),
            _ => (
                n("charge_now").unwrap_or(0.0),
                n("charge_full").unwrap_or(0.0),
                n("charge_full_design"),
                current.map(|a| a * 1e6),
            ),
        };
        let time_left = rate.filter(|r| *r > 0.0).and_then(|r| {
            let hours = match status.as_str() {
                "Discharging" => now / r,
                "Charging" => (full - now).max(0.0) / r,
                _ => return None,
            };
            // Some firmware reports nonsense rates; ignore days-long estimates.
            (hours < 48.0).then_some((hours * 3600.0) as u64)
        });
        let percent = n("capacity").map(|c| c as f32).unwrap_or_else(|| pct(now as u64, full as u64));
        batteries.push(Battery {
            name,
            model,
            percent,
            status,
            power_w: power.or_else(|| Some(voltage? * current?)).map(|w| w as f32),
            time_left,
            health: design.filter(|d| *d > 0.0).map(|d| (full / d * 100.0).min(100.0) as f32),
            cycles: read_num::<u32>(p.join("cycle_count")).filter(|c| *c > 0),
            voltage: voltage.map(|v| v as f32),
            technology: read_trim(p.join("technology")).unwrap_or_default(),
            now,
            full,
            rate,
        });
    }
    (batteries, devices, ac)
}

/// File systems on block devices that are worth showing, plus pool-based
/// ones (ZFS, bcachefs) whose source is not a /dev path.
fn read_mounts() -> Vec<Mount> {
    let Some(m) = read("/proc/mounts") else { return Vec::new() };
    let mut out: Vec<Mount> = Vec::new();
    for line in m.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let pooled = matches!(f[2], "zfs" | "bcachefs");
        let block = f[0].starts_with("/dev/") && !f[0].starts_with("/dev/loop") && f[2] != "squashfs";
        let mount = unescape_mount(f[1]);
        if !(block || pooled) || mount.starts_with("/snap/") || mount.starts_with("/var/lib/docker") {
            continue;
        }
        // Single files bind-mounted from a disk (containers do this) are not file systems.
        if !Path::new(&mount).is_dir() {
            continue;
        }
        // The same disk mounted twice (btrfs subvolumes, bind mounts) once only.
        if out.iter().any(|o| o.device == f[0]) {
            continue;
        }
        let Some((total, used, avail)) = statvfs(&mount) else { continue };
        if total == 0 {
            continue;
        }
        out.push(Mount { device: f[0].to_string(), mount, fs: f[2].to_string(), total, used, avail });
    }
    out
}

/// /proc/mounts writes spaces and tabs as octal escapes (\040).
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn statvfs(path: &str) -> Option<(u64, u64, u64)> {
    let c = CString::new(path).ok()?;
    // SAFETY: `s` is a plain C struct that statvfs fills in.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let f = s.f_frsize as u64;
    let total = s.f_blocks as u64 * f;
    let free = s.f_bfree as u64 * f;
    let avail = s.f_bavail as u64 * f;
    Some((total, total.saturating_sub(free), avail))
}

type AddrMap = HashMap<String, Vec<String>>;

/// IPv4 and global IPv6 addresses of every interface.
fn if_addrs() -> (AddrMap, AddrMap) {
    let mut v4: AddrMap = HashMap::new();
    let mut v6: AddrMap = HashMap::new();
    // SAFETY: getifaddrs hands back a linked list that stays valid until
    // freeifaddrs; every pointer is checked before it is read.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return (v4, v6);
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() {
                let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                match (*ifa.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                        v4.entry(name).or_default().push(ip.to_string());
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                        // Link-local (fe80::) addresses are not useful to show.
                        if ip.segments()[0] & 0xffc0 != 0xfe80 && !ip.is_loopback() {
                            v6.entry(name).or_default().push(ip.to_string());
                        }
                    }
                    _ => {}
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    (v4, v6)
}

/// Link quality (out of 70) as a percentage, and signal level in dBm.
fn read_wireless() -> HashMap<String, (f32, i32)> {
    let mut out = HashMap::new();
    if let Some(w) = read("/proc/net/wireless") {
        for line in w.lines().skip(2) {
            let Some((name, rest)) = line.split_once(':') else { continue };
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() < 3 {
                continue;
            }
            let q = f[1].trim_end_matches('.').parse::<f32>().unwrap_or(0.0);
            let lvl = f[2].trim_end_matches('.').parse::<f32>().unwrap_or(0.0) as i32;
            out.insert(name.trim().to_string(), ((q / 70.0 * 100.0).min(100.0), lvl));
        }
    }
    out
}

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn fmt_rate(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps.max(0.0) as u64))
}

pub fn fmt_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m {:02}s", secs % 60)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn bytes_are_readable() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
        assert_eq!(fmt_bytes(250 * 1024 * 1024), "250 MiB");
    }

    #[test]
    fn mount_escapes_are_decoded() {
        assert_eq!(unescape_mount("/media/My\\040Disk"), "/media/My Disk");
        assert_eq!(unescape_mount("/plain"), "/plain");
        assert_eq!(unescape_mount("/ends\\04"), "/ends\\04");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(59), "0m 59s");
        assert_eq!(fmt_duration(3 * 3600 + 5 * 60), "3h 05m");
        assert_eq!(fmt_duration(2 * 86_400 + 3600), "2d 1h 0m");
    }

    #[test]
    fn cpu_names_on_x86_and_arm() {
        let x86 = "processor\t: 0\nmodel name\t: AMD Ryzen 7  5800X 8-Core Processor\n";
        assert_eq!(cpu_model(x86).as_deref(), Some("AMD Ryzen 7 5800X 8-Core Processor"));
        let pi =
            "processor\t: 0\nBogoMIPS\t: 108.00\n\nHardware\t: BCM2835\nModel\t\t: Raspberry Pi 4 Model B Rev 1.4\n";
        assert_eq!(cpu_model(pi).as_deref(), Some("Raspberry Pi 4 Model B Rev 1.4"));
    }

    #[test]
    fn several_batteries_add_up() {
        let bat = |now: f64, full: f64, status: &str| Battery {
            percent: (now / full * 100.0) as f32,
            status: status.into(),
            now,
            full,
            rate: Some(5_000_000.0),
            power_w: Some(5.0),
            ..Default::default()
        };
        let s = Snapshot {
            batteries: vec![bat(20e6, 40e6, "Discharging"), bat(10e6, 20e6, "Unknown")],
            ..Default::default()
        };
        let b = s.battery().unwrap();
        assert_eq!(b.count, 2);
        assert!((b.percent - 50.0).abs() < 0.01);
        assert_eq!(b.status, "Discharging");
        // 30 units left at 10 units per hour.
        assert_eq!(b.time_left, Some(3 * 3600));
    }

    #[test]
    fn fahrenheit_switch() {
        assert_eq!(fmt_temp(100.0), "100 °C");
        FAHRENHEIT.store(true, Ordering::Relaxed);
        assert_eq!(fmt_temp(100.0), "212 °F");
        FAHRENHEIT.store(false, Ordering::Relaxed);
    }

    #[test]
    fn current_user_has_a_name() {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        assert!(lookup_user(uid).is_some_and(|n| !n.is_empty()));
    }

    /// A throwaway directory tree standing in for part of /sys.
    pub(crate) struct FakeSys(pub std::path::PathBuf);

    impl FakeSys {
        pub(crate) fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("sysmon-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            FakeSys(p)
        }

        pub(crate) fn file(&self, rel: &str, body: &str) -> &Self {
            let f = self.0.join(rel);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
            self
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn amd_desktop_sensors() {
        let sys = FakeSys::new("hwmon");
        sys.file("hwmon0/name", "k10temp\n")
            .file("hwmon0/temp1_input", "61250\n")
            .file("hwmon0/temp1_label", "Tctl\n")
            .file("hwmon0/temp3_input", "55000\n")
            .file("hwmon0/temp3_label", "Tccd1\n")
            .file("hwmon1/name", "nct6798\n")
            .file("hwmon1/fan2_input", "1180\n")
            .file("hwmon1/fan2_label", "CPU fan\n")
            .file("hwmon1/temp7_input", "0\n")
            .file("hwmon2/name", "amdgpu\n")
            .file("hwmon2/temp1_input", "48000\n")
            .file("hwmon3/name", "nvme\n")
            .file("hwmon3/temp1_input", "39850\n")
            .file("hwmon3/temp1_crit", "84850\n")
            .file("hwmon3/temp2_max", "65261850\n");
        let (temps, fans) = read_hwmon(&sys.0);
        let labels: Vec<&str> = temps.iter().map(|t| t.label.as_str()).collect();
        // The unused sensor (0 °C) is dropped and the GPU is left to the GPU tab.
        assert_eq!(labels, ["Tctl", "Tccd1", "NVMe SSD"]);
        assert_eq!(temps[2].crit, Some(84.85));
        assert_eq!(fans.len(), 1);
        assert_eq!((fans[0].label.as_str(), fans[0].rpm), ("CPU fan", 1180));
        let snap = Snapshot { temps, ..Default::default() };
        assert_eq!(snap.cpu_temp(), Some(61.25));
    }

    #[test]
    fn arm_board_thermal_zones() {
        let sys = FakeSys::new("thermal");
        sys.file("thermal_zone0/type", "cpu-thermal\n").file("thermal_zone0/temp", "47236\n");
        let temps = read_thermal_zones(&sys.0);
        assert_eq!(temps.len(), 1);
        assert_eq!(temps[0].label, "CPU");
        assert_eq!(Snapshot { temps, ..Default::default() }.cpu_temp(), Some(47.236));
    }

    #[test]
    fn thinkpad_with_two_batteries_and_a_mouse() {
        let sys = FakeSys::new("power");
        sys.file("AC/type", "Mains\n")
            .file("AC/online", "0\n")
            // Energy-based battery (µWh / µW), discharging at 10 W.
            .file("BAT0/type", "Battery\n")
            .file("BAT0/status", "Discharging\n")
            .file("BAT0/capacity", "50\n")
            .file("BAT0/energy_now", "20000000\n")
            .file("BAT0/energy_full", "40000000\n")
            .file("BAT0/energy_full_design", "50000000\n")
            .file("BAT0/power_now", "10000000\n")
            .file("BAT0/model_name", "01AV430\n")
            // Charge-based battery (µAh / µA), idle.
            .file("BAT1/type", "Battery\n")
            .file("BAT1/status", "Unknown\n")
            .file("BAT1/capacity", "100\n")
            .file("BAT1/charge_now", "2000000\n")
            .file("BAT1/charge_full", "2000000\n")
            .file("BAT1/current_now", "0\n")
            .file("hidpp_battery_0/type", "Battery\n")
            .file("hidpp_battery_0/scope", "Device\n")
            .file("hidpp_battery_0/model_name", "MX Master 3\n")
            .file("hidpp_battery_0/capacity_level", "Normal\n")
            .file("hidpp_battery_0/status", "Discharging\n");
        let (bats, devices, ac) = read_power(&sys.0);
        assert_eq!(ac, Some(false));
        assert_eq!(bats.len(), 2);
        assert_eq!(bats[0].time_left, Some(2 * 3600), "20 Wh left at 10 W");
        assert_eq!(bats[0].health, Some(80.0));
        assert_eq!(bats[0].power_w, Some(10.0));
        assert_eq!(bats[1].time_left, None);
        assert_eq!(devices.len(), 1);
        assert_eq!((devices[0].name.as_str(), devices[0].level.as_str()), ("MX Master 3", "Normal"));
        let summary = Snapshot { batteries: bats, ..Default::default() }.battery().unwrap();
        assert_eq!(summary.count, 2);
        assert_eq!(summary.status, "Discharging");
    }

    #[test]
    fn desktop_without_battery() {
        let sys = FakeSys::new("power-desktop");
        sys.file("ucsi-source-psy-USBC000:001/type", "USB\n").file("ucsi-source-psy-USBC000:001/online", "1\n");
        let (bats, devices, ac) = read_power(&sys.0);
        assert!(bats.is_empty() && devices.is_empty());
        assert_eq!(ac, Some(true));
    }

    #[test]
    fn two_samples_give_sane_numbers() {
        let mut c = Collector::new(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
        c.sample();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = c.sample();
        assert!((0.0..=100.0).contains(&s.cpu_total));
        assert!(s.mem.total > 0 && s.mem.used <= s.mem.total);
        assert!(!s.procs.is_empty());
        assert!(s.procs.iter().all(|p| (0.0..=100.0).contains(&p.cpu)));
    }
}
