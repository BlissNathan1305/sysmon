//! The last few minutes of readings, for the graphs and the CSV export.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::stats::{self, Snapshot};

/// Points of history kept for the graphs (one per sample).
pub const HISTORY: usize = 120;

#[derive(Default)]
pub struct History {
    /// Unix time of each sample.
    pub times: VecDeque<u64>,
    pub cpu: VecDeque<f32>,
    pub cores: Vec<VecDeque<f32>>,
    pub mem: VecDeque<f32>,
    pub swap: VecDeque<f32>,
    pub temp: VecDeque<f32>,
    pub rx: VecDeque<f32>,
    pub tx: VecDeque<f32>,
    pub read: VecDeque<f32>,
    pub write: VecDeque<f32>,
    pub gpu: Vec<VecDeque<f32>>,
    pub gpu_mem: Vec<VecDeque<f32>>,
}

fn push<T>(q: &mut VecDeque<T>, v: T) {
    if q.len() == HISTORY {
        q.pop_front();
    }
    q.push_back(v);
}

impl History {
    pub fn record(&mut self, s: &Snapshot) {
        push(&mut self.times, SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        push(&mut self.cpu, s.cpu_total);
        self.cores.resize_with(s.cpu_cores.len(), VecDeque::new);
        for (q, v) in self.cores.iter_mut().zip(&s.cpu_cores) {
            push(q, *v);
        }
        push(&mut self.mem, s.mem.used_pct());
        push(&mut self.swap, s.mem.swap_pct());
        if let Some(t) = s.cpu_temp() {
            push(&mut self.temp, t);
        }
        push(&mut self.rx, s.net_rx() as f32);
        push(&mut self.tx, s.net_tx() as f32);
        push(&mut self.read, s.disk_read() as f32);
        push(&mut self.write, s.disk_write() as f32);
        self.gpu.resize_with(s.gpus.len(), VecDeque::new);
        self.gpu_mem.resize_with(s.gpus.len(), VecDeque::new);
        for (i, g) in s.gpus.iter().enumerate() {
            if let Some(u) = g.usage {
                push(&mut self.gpu[i], u);
            }
            if let (Some(used), Some(total)) = (g.vram_used, g.vram_total) {
                push(&mut self.gpu_mem[i], stats::pct(used, total));
            }
        }
    }

    /// The history as CSV, one row per sample, oldest first. Readings that
    /// started later (a sensor that appeared) are blank in earlier rows.
    pub fn to_csv(&self) -> String {
        let mut columns: Vec<(String, &VecDeque<f32>)> = vec![
            ("cpu_percent".into(), &self.cpu),
            ("memory_percent".into(), &self.mem),
            ("swap_percent".into(), &self.swap),
            ("cpu_temp_celsius".into(), &self.temp),
            ("net_down_bytes_per_s".into(), &self.rx),
            ("net_up_bytes_per_s".into(), &self.tx),
            ("disk_read_bytes_per_s".into(), &self.read),
            ("disk_write_bytes_per_s".into(), &self.write),
        ];
        for (i, q) in self.gpu.iter().enumerate() {
            columns.push((format!("gpu{}_busy_percent", i + 1), q));
        }
        for (i, q) in self.gpu_mem.iter().enumerate().filter(|(_, q)| !q.is_empty()) {
            columns.push((format!("gpu{}_memory_percent", i + 1), q));
        }
        for (i, q) in self.cores.iter().enumerate() {
            columns.push((format!("cpu{i}_percent"), q));
        }

        let rows = self.times.len();
        let mut out = String::from("time");
        for (name, _) in &columns {
            out.push(',');
            out.push_str(name);
        }
        out.push('\n');
        for (r, t) in self.times.iter().enumerate() {
            out.push_str(&local_time(*t));
            for (_, q) in &columns {
                out.push(',');
                // Align every column on the newest sample.
                if let Some(i) = (r + q.len()).checked_sub(rows) {
                    if let Some(v) = q.get(i) {
                        out.push_str(&format!("{v:.1}"));
                    }
                }
            }
            out.push('\n');
        }
        out
    }
}

/// "2026-10-17 14:05:09" in local time.
pub fn local_time(unix: u64) -> String {
    let t = unix as libc::time_t;
    // SAFETY: localtime_r fills the zeroed struct from a valid time value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return unix.to_string();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The user's Downloads folder (from user-dirs.dirs), else their home.
pub fn download_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let from_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| {
        text.lines().find_map(|l| {
            let v = l.strip_prefix("XDG_DOWNLOAD_DIR=")?.trim().trim_matches('"');
            Some(PathBuf::from(v.replace("$HOME", &home.to_string_lossy())))
        })
    });
    [from_dirs, Some(home.join("Downloads"))].into_iter().flatten().find(|p| p.is_dir()).unwrap_or(home)
}

/// Writes the history to sysmon-history-<date>-<time>.csv in `dir`.
pub fn save_csv(history: &History, dir: &Path) -> std::io::Result<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let stamp: String = local_time(now).chars().filter(|c| c.is_ascii_digit() || *c == ' ').collect();
    let path = dir.join(format!("sysmon-history-{}.csv", stamp.replace(' ', "-")));
    let mut f = std::fs::File::create(&path)?;
    f.write_all(history.to_csv().as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_aligns_late_columns_on_the_newest_row() {
        let mut h = History::default();
        h.times.extend([100, 101, 102]);
        h.cpu.extend([10.0, 20.0, 30.0]);
        h.temp.extend([55.0]);
        let csv = h.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("time,cpu_percent,memory_percent,swap_percent,cpu_temp_celsius,"));
        let row = |i: usize| lines[i].split(',').collect::<Vec<_>>();
        assert_eq!(row(1)[1], "10.0");
        assert_eq!(row(1)[4], "", "temperature had no reading yet");
        assert_eq!(row(3)[1], "30.0");
        assert_eq!(row(3)[4], "55.0");
    }

    #[test]
    fn history_is_capped() {
        let mut h = History::default();
        let s = Snapshot::default();
        for _ in 0..HISTORY + 10 {
            h.record(&s);
        }
        assert_eq!(h.cpu.len(), HISTORY);
        assert_eq!(h.times.len(), HISTORY);
    }

    #[test]
    fn csv_file_is_written() {
        let dir = std::env::temp_dir().join(format!("sysmon-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = History::default();
        h.record(&Snapshot::default());
        let path = save_csv(&h, &dir).unwrap();
        assert!(path.file_name().unwrap().to_string_lossy().starts_with("sysmon-history-"));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
