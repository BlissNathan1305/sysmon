//! Desktop notifications for things that need attention: a very hot CPU,
//! memory or a disk nearly full, or a low battery. They only run while
//! sysmon is open.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use crate::gpu;
use crate::stats::{fmt_bytes, fmt_duration, fmt_temp, Snapshot};

/// How long the same alert stays quiet after it has been shown.
const COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// How long a temperature or memory reading must stay high before it
/// counts, so a short spike does not raise an alarm.
const HOLD: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Temp,
    Memory,
    Disk,
    Battery,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Temp, Kind::Memory, Kind::Disk, Kind::Battery];

    pub fn key(self) -> &'static str {
        match self {
            Kind::Temp => "temp",
            Kind::Memory => "memory",
            Kind::Disk => "disk",
            Kind::Battery => "battery",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Temp => "CPU temperature reaches",
            Kind::Memory => "Memory use reaches",
            Kind::Disk => "A disk is fuller than",
            Kind::Battery => "Battery drops to",
        }
    }

    pub fn choices(self) -> &'static [u32] {
        match self {
            Kind::Temp => &[75, 80, 85, 90, 95],
            Kind::Memory => &[80, 85, 90, 95],
            Kind::Disk => &[80, 85, 90, 95, 98],
            Kind::Battery => &[5, 10, 15, 20, 25],
        }
    }

    pub fn default_value(self) -> u32 {
        match self {
            Kind::Temp => 90,
            Kind::Memory => 95,
            Kind::Disk => 95,
            Kind::Battery => 10,
        }
    }
}

/// One threshold choice, shown in °C/°F or as a percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub kind: Kind,
    pub value: u32,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.kind {
            Kind::Temp => f.write_str(&fmt_temp(self.value as f32)),
            _ => write!(f, "{}%", self.value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub on: bool,
    pub value: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub limits: HashMap<Kind, Limit>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { limits: Kind::ALL.into_iter().map(|k| (k, Limit { on: true, value: k.default_value() })).collect() }
    }
}

impl Settings {
    pub fn get(&self, k: Kind) -> Limit {
        self.limits.get(&k).copied().unwrap_or(Limit { on: true, value: k.default_value() })
    }

    pub fn set(&mut self, k: Kind, limit: Limit) {
        self.limits.insert(k, limit);
    }

    fn active(&self, k: Kind) -> Option<f32> {
        let l = self.get(k);
        l.on.then_some(l.value as f32)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub title: String,
    pub body: String,
    pub urgent: bool,
}

#[derive(Default)]
pub struct Watcher {
    since: HashMap<String, Instant>,
    sent: HashMap<String, Instant>,
}

impl Watcher {
    /// The alerts to show now for this reading.
    pub fn check(&mut self, settings: &Settings, s: &Snapshot, now: Instant) -> Vec<Alert> {
        let mut found: Vec<(String, bool, Alert)> = Vec::new();
        if let (Some(limit), Some(t)) = (settings.active(Kind::Temp), s.cpu_temp()) {
            if t >= limit {
                found.push((
                    "temp".into(),
                    true,
                    Alert {
                        title: "CPU is very hot".into(),
                        body: format!(
                            "The processor is at {}. Close heavy programs and make sure the air vents are not blocked.",
                            fmt_temp(t)
                        ),
                        urgent: true,
                    },
                ));
            }
        }
        if let Some(limit) = settings.active(Kind::Memory) {
            let used = s.mem.used_pct();
            if s.mem.total > 0 && used >= limit {
                found.push((
                    "memory".into(),
                    true,
                    Alert {
                        title: "Memory almost full".into(),
                        body: format!(
                            "{used:.0}% of memory is in use ({} available). Programs may slow down; close ones you are not using.",
                            fmt_bytes(s.mem.available)
                        ),
                        urgent: false,
                    },
                ));
            }
        }
        if let Some(limit) = settings.active(Kind::Disk) {
            for m in &s.mounts {
                let used = m.used_pct();
                if used >= limit {
                    found.push((
                        format!("disk:{}", m.mount),
                        false,
                        Alert {
                            title: "Disk almost full".into(),
                            body: format!("{} is {used:.0}% full; only {} is left.", m.mount, fmt_bytes(m.avail)),
                            urgent: used >= 99.0,
                        },
                    ));
                }
            }
        }
        if let (Some(limit), Some(b)) = (settings.active(Kind::Battery), s.battery()) {
            if b.status == "Discharging" && b.percent <= limit {
                let left = b.time_left.map(|t| format!(" (about {})", fmt_duration(t))).unwrap_or_default();
                found.push((
                    "battery".into(),
                    false,
                    Alert {
                        title: "Battery low".into(),
                        body: format!("{:.0}% left{left}. Plug in the charger.", b.percent),
                        urgent: true,
                    },
                ));
            }
        }

        let keys: HashSet<&String> = found.iter().map(|f| &f.0).collect();
        self.since.retain(|k, _| keys.contains(k));
        let mut out = Vec::new();
        for (key, hold, alert) in found {
            let since = *self.since.entry(key.clone()).or_insert(now);
            if hold && now.duration_since(since) < HOLD {
                continue;
            }
            if self.sent.get(&key).is_some_and(|t| now.duration_since(*t) < COOLDOWN) {
                continue;
            }
            self.sent.insert(key, now);
            out.push(alert);
        }
        out
    }
}

/// Text quoted as a GVariant string for `gdbus call`.
fn gvariant_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Shows a desktop notification. Blocks briefly, so call it off the window
/// thread. Returns false when no notification tool worked.
pub fn notify(a: &Alert) -> bool {
    let limit = Duration::from_secs(3);
    if let Some(p) = gpu::in_path("notify-send") {
        let urgency = if a.urgent { "--urgency=critical" } else { "--urgency=normal" };
        if gpu::run_with_timeout(&p, &["--app-name=sysmon", "--icon=sysmon", urgency, &a.title, &a.body], limit)
            .is_some()
        {
            return true;
        }
    }
    // Without libnotify, talk to the notification service directly.
    let Some(p) = gpu::in_path("gdbus") else { return false };
    let urgency = format!("{{'urgency': <byte {}>}}", if a.urgent { 2 } else { 1 });
    gpu::run_with_timeout(
        &p,
        &[
            "call",
            "--session",
            "--dest=org.freedesktop.Notifications",
            "--object-path=/org/freedesktop/Notifications",
            "--method=org.freedesktop.Notifications.Notify",
            "'sysmon'",
            "0",
            "'sysmon'",
            &gvariant_str(&a.title),
            &gvariant_str(&a.body),
            "@as []",
            &urgency,
            "10000",
        ],
        limit,
    )
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{Battery, Memory, Mount, Temp};

    fn hot(celsius: f32) -> Snapshot {
        Snapshot {
            temps: vec![Temp {
                chip: "coretemp".into(),
                label: "Package id 0".into(),
                celsius,
                high: None,
                crit: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn temperature_must_stay_high_before_alerting() {
        let mut w = Watcher::default();
        let s = Settings::default();
        let t0 = Instant::now();
        assert!(w.check(&s, &hot(95.0), t0).is_empty(), "a first reading is only a spike");
        assert!(w.check(&s, &hot(95.0), t0 + Duration::from_secs(5)).is_empty());
        let fired = w.check(&s, &hot(95.0), t0 + Duration::from_secs(16));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].title, "CPU is very hot");
        // Quiet during the cooldown, even while still hot.
        assert!(w.check(&s, &hot(96.0), t0 + Duration::from_secs(60)).is_empty());
        assert_eq!(w.check(&s, &hot(96.0), t0 + Duration::from_secs(16) + COOLDOWN).len(), 1);
    }

    #[test]
    fn a_dip_restarts_the_wait() {
        let mut w = Watcher::default();
        let s = Settings::default();
        let t0 = Instant::now();
        w.check(&s, &hot(95.0), t0);
        w.check(&s, &hot(60.0), t0 + Duration::from_secs(10));
        assert!(w.check(&s, &hot(95.0), t0 + Duration::from_secs(20)).is_empty());
    }

    #[test]
    fn disk_and_battery_alert_at_once_and_can_be_switched_off() {
        let snap = Snapshot {
            mounts: vec![Mount {
                device: "/dev/sda1".into(),
                mount: "/".into(),
                fs: "ext4".into(),
                total: 100,
                used: 97,
                avail: 3,
            }],
            batteries: vec![Battery { percent: 8.0, status: "Discharging".into(), ..Default::default() }],
            mem: Memory { total: 100, used: 50, ..Default::default() },
            ..Default::default()
        };
        let mut s = Settings::default();
        let titles: Vec<String> =
            Watcher::default().check(&s, &snap, Instant::now()).into_iter().map(|a| a.title).collect();
        assert_eq!(titles, ["Disk almost full", "Battery low"]);

        s.set(Kind::Disk, Limit { on: false, value: 95 });
        s.set(Kind::Battery, Limit { on: true, value: 5 });
        assert!(Watcher::default().check(&s, &snap, Instant::now()).is_empty());
    }

    #[test]
    fn charging_battery_is_not_low() {
        let snap = Snapshot {
            batteries: vec![Battery { percent: 3.0, status: "Charging".into(), ..Default::default() }],
            ..Default::default()
        };
        assert!(Watcher::default().check(&Settings::default(), &snap, Instant::now()).is_empty());
    }

    #[test]
    fn gvariant_quoting() {
        assert_eq!(gvariant_str("it's"), "'it\\'s'");
    }
}
