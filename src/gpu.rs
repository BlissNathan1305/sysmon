//! Graphics card readings.
//!
//! Each driver exposes different files, so this reads what it can:
//! - Intel (i915, xe): clock speed, and busy time estimated from the time the
//!   GPU spends in its RC6 sleep state (the exact figure needs root).
//! - AMD (amdgpu): busy %, video memory, temperature, power, fan and clock.
//! - NVIDIA (proprietary driver): everything, through `nvidia-smi`.
//! - Anything else: the name, and a temperature if the driver has a sensor.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::pci;

#[derive(Debug, Clone, Default)]
pub struct Gpu {
    pub name: String,
    pub driver: String,
    pub usage: Option<f32>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    pub temp: Option<f32>,
    pub power_w: Option<f32>,
    pub freq_mhz: Option<f32>,
    pub max_freq_mhz: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub fan_pct: Option<f32>,
    /// What is missing or estimated, in plain words.
    pub note: Option<String>,
}

struct Card {
    dir: PathBuf,
    dev: PathBuf,
    driver: String,
    name: String,
    /// Last RC6 (idle) counter reading in ms, for Intel busy time.
    rc6: Option<(u64, Instant)>,
}

pub struct Probe {
    cards: Vec<Card>,
    nvidia_smi: Option<PathBuf>,
    nvidia_failures: u32,
}

/// Framebuffer drivers that are not real GPUs.
const SKIP: [&str; 4] = ["simpledrm", "efifb", "vesafb", "simplefb"];

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_num<T: std::str::FromStr>(p: impl AsRef<Path>) -> Option<T> {
    read_trim(p)?.parse().ok()
}

fn first_num<T: std::str::FromStr>(paths: &[PathBuf]) -> Option<T> {
    paths.iter().find_map(read_num)
}

fn in_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join(program)).find(|p| p.is_file())
}

fn hwmon_dir(dev: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dev.join("hwmon")).ok()?.flatten().map(|e| e.path()).next()
}

impl Probe {
    pub fn new() -> Self {
        let mut cards = Vec::new();
        if let Ok(dir) = std::fs::read_dir("/sys/class/drm") {
            let mut paths: Vec<PathBuf> = dir
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_prefix("card"))
                        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                })
                .collect();
            paths.sort();
            for dir in paths {
                let dev = dir.join("device");
                let driver = std::fs::read_link(dev.join("driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                if SKIP.contains(&driver.as_str()) {
                    continue;
                }
                let name = pci::full_name(&dev).unwrap_or_else(|| {
                    if driver.is_empty() {
                        "Graphics".into()
                    } else {
                        format!("{driver} graphics")
                    }
                });
                cards.push(Card { dir, dev, driver, name, rc6: None });
            }
        }
        Probe { cards, nvidia_smi: in_path("nvidia-smi"), nvidia_failures: 0 }
    }

    pub fn sample(&mut self) -> Vec<Gpu> {
        let use_smi = self.nvidia_smi.is_some() && self.nvidia_failures < 3;
        let mut out = Vec::new();
        for card in &mut self.cards {
            match card.driver.as_str() {
                "i915" | "xe" => out.push(intel(card)),
                "amdgpu" | "radeon" => out.push(amd(card)),
                // nvidia-smi covers these below.
                "nvidia" if use_smi => {}
                "nvidia" => out.push(Gpu {
                    name: card.name.clone(),
                    driver: card.driver.clone(),
                    note: Some("Install the NVIDIA utilities (nvidia-smi) to see usage and temperature.".into()),
                    ..Default::default()
                }),
                _ => out.push(generic(card)),
            }
        }
        if use_smi {
            match self.nvidia_smi.as_deref().and_then(nvidia) {
                Some(gpus) => {
                    self.nvidia_failures = 0;
                    out.extend(gpus);
                }
                None => {
                    // Stop asking after repeated failures so a broken driver
                    // does not slow every reading down.
                    self.nvidia_failures += 1;
                    for card in self.cards.iter().filter(|c| c.driver == "nvidia") {
                        out.push(Gpu {
                            name: card.name.clone(),
                            driver: card.driver.clone(),
                            note: Some("nvidia-smi did not answer, so usage is unavailable.".into()),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        out
    }
}

fn intel(card: &mut Card) -> Gpu {
    let d = &card.dir;
    let xe = card.dev.join("tile0/gt0");
    let freq = first_num::<f32>(&[
        d.join("gt_act_freq_mhz"),
        d.join("gt/gt0/rps_act_freq_mhz"),
        xe.join("freq0/act_freq"),
        d.join("gt_cur_freq_mhz"),
        xe.join("freq0/cur_freq"),
    ]);
    let max = first_num::<f32>(&[d.join("gt_RP0_freq_mhz"), d.join("gt_max_freq_mhz"), xe.join("freq0/rp0_freq")]);
    let rc6 = first_num::<u64>(&[
        d.join("gt/gt0/rc6_residency_ms"),
        d.join("power/rc6_residency_ms"),
        xe.join("gtidle/idle_residency_ms"),
    ]);
    let now = Instant::now();
    let usage = match (rc6, card.rc6) {
        (Some(idle), Some((prev, at))) => {
            let wall = now.duration_since(at).as_secs_f64() * 1000.0;
            (wall > 0.0).then(|| ((1.0 - idle.saturating_sub(prev) as f64 / wall) * 100.0).clamp(0.0, 100.0) as f32)
        }
        _ => None,
    };
    card.rc6 = rc6.map(|v| (v, now));
    let hw = hwmon_dir(&card.dev);
    Gpu {
        name: card.name.clone(),
        driver: card.driver.clone(),
        usage: usage.or(rc6.map(|_| 0.0)),
        temp: hw.as_ref().and_then(|h| read_num::<f32>(h.join("temp1_input"))).map(|m| m / 1000.0),
        power_w: hw.as_ref().and_then(|h| read_num::<f32>(h.join("power1_input"))).map(|u| u / 1e6),
        freq_mhz: freq,
        max_freq_mhz: max,
        note: Some(if rc6.is_some() {
            "Busy % is estimated from the time the GPU spends idle. Integrated graphics share system memory.".into()
        } else {
            "This driver does not report busy time without root (try intel_gpu_top).".into()
        }),
        ..Default::default()
    }
}

fn amd(card: &Card) -> Gpu {
    let dev = &card.dev;
    let hw = hwmon_dir(dev);
    let h = |f: &str| hw.as_ref().and_then(|h| read_num::<f64>(h.join(f)));
    // pp_dpm_sclk lists the clock steps, with the current one marked "*".
    let steps: Vec<(f32, bool)> = read_trim(dev.join("pp_dpm_sclk"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mhz = l.split_whitespace().nth(1)?.to_lowercase();
            Some((mhz.trim_end_matches("mhz").parse().ok()?, l.ends_with('*')))
        })
        .collect();
    let fan_pct = match (h("pwm1"), h("pwm1_max")) {
        (Some(p), Some(m)) if m > 0.0 => Some((p / m * 100.0) as f32),
        (Some(p), None) => Some((p / 255.0 * 100.0) as f32),
        _ => None,
    };
    Gpu {
        name: card.name.clone(),
        driver: card.driver.clone(),
        usage: read_num(dev.join("gpu_busy_percent")),
        vram_used: read_num(dev.join("mem_info_vram_used")),
        vram_total: read_num(dev.join("mem_info_vram_total")),
        temp: h("temp1_input").map(|m| (m / 1000.0) as f32),
        power_w: h("power1_average").or_else(|| h("power1_input")).map(|u| (u / 1e6) as f32),
        freq_mhz: h("freq1_input").map(|hz| (hz / 1e6) as f32).or_else(|| steps.iter().find(|s| s.1).map(|s| s.0)),
        max_freq_mhz: steps.last().map(|s| s.0),
        fan_rpm: h("fan1_input").map(|r| r as u32).filter(|r| *r > 0),
        fan_pct,
        note: (card.driver == "radeon").then(|| "The older radeon driver reports little; amdgpu shows more.".into()),
    }
}

fn generic(card: &Card) -> Gpu {
    let temp = hwmon_dir(&card.dev).and_then(|h| read_num::<f32>(h.join("temp1_input"))).map(|m| m / 1000.0);
    Gpu {
        name: card.name.clone(),
        driver: card.driver.clone(),
        temp,
        note: Some(format!(
            "The {} driver does not report usage.",
            if card.driver.is_empty() { "graphics" } else { &card.driver }
        )),
        ..Default::default()
    }
}

pub(crate) fn run_with_timeout(program: &Path, args: &[&str], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

fn nvidia(smi: &Path) -> Option<Vec<Gpu>> {
    let out = run_with_timeout(
        smi,
        &[
            "--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,fan.speed,clocks.gr,clocks.max.gr",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(2),
    )?;
    Some(out.lines().filter(|l| !l.trim().is_empty()).map(parse_nvidia_line).collect())
}

/// One CSV line of nvidia-smi output; "[N/A]" fields become `None`.
fn parse_nvidia_line(line: &str) -> Gpu {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
    let name = f.first().copied().unwrap_or("NVIDIA GPU");
    Gpu {
        name: if name.starts_with("NVIDIA") { name.to_string() } else { format!("NVIDIA {name}") },
        driver: "nvidia".into(),
        usage: num(1).map(|v| v as f32),
        vram_used: num(2).map(|m| (m * 1024.0 * 1024.0) as u64),
        vram_total: num(3).map(|m| (m * 1024.0 * 1024.0) as u64),
        temp: num(4).map(|v| v as f32),
        power_w: num(5).map(|v| v as f32),
        fan_pct: num(6).map(|v| v as f32),
        freq_mhz: num(7).map(|v| v as f32),
        max_freq_mhz: num(8).map(|v| v as f32),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_csv_is_parsed() {
        let g = parse_nvidia_line("NVIDIA GeForce RTX 3060, 37, 1024, 12288, 54, 41.20, 30, 1710, 2100");
        assert_eq!(g.name, "NVIDIA GeForce RTX 3060");
        assert_eq!(g.usage, Some(37.0));
        assert_eq!(g.vram_total, Some(12288 * 1024 * 1024));
        assert_eq!(g.fan_pct, Some(30.0));
        assert_eq!(g.max_freq_mhz, Some(2100.0));
    }

    #[test]
    fn nvidia_missing_fields_are_none() {
        let g = parse_nvidia_line("Quadro P400, 3, 200, 2000, 40, [N/A], [Not Supported], 1200, 1500");
        assert_eq!(g.name, "NVIDIA Quadro P400");
        assert_eq!(g.power_w, None);
        assert_eq!(g.fan_pct, None);
        assert_eq!(g.freq_mhz, Some(1200.0));
    }
}
