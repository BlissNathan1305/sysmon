//! sysmon: a live view of a Linux computer's CPU, memory, graphics, sensors,
//! battery, disks, network and processes.

mod alerts;
mod gpu;
mod graph;
mod history;
mod pci;
mod report;
mod stats;
mod style;
mod view;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::futures::channel::oneshot;
use iced::widget::text_input;
use iced::{
    clipboard, event, executor, keyboard, time, window, Application, Command, Element, Event, Settings, Size,
    Subscription, Theme,
};

use history::History;
use stats::{Collector, Snapshot, StaticInfo};

const HELP: &str = "\
sysmon — a live system monitor for Linux

Usage: sysmon [OPTIONS]

Options:
  -h, --help       Show this help and exit
  -V, --version    Show the version and exit

Keyboard shortcuts in the window:
  Ctrl+1 … Ctrl+9  Switch tabs
  Ctrl+F           Search processes
  Ctrl+P           Pause or resume updates
  Ctrl+E           Export the recent history to a CSV file
  Esc              Close the process details
  Ctrl+Q           Quit

Settings are kept in $XDG_CONFIG_HOME/sysmon/settings.conf
(usually ~/.config/sysmon/settings.conf).";

pub fn main() -> iced::Result {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("sysmon {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                eprintln!("sysmon: unknown option '{other}'\n\n{HELP}");
                std::process::exit(2);
            }
        }
    }
    let prefs = Prefs::load();
    install_panic_hook();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_window(prefs))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            retry_with_software_drawing();
            Err(e)
        }
        Err(panic) => {
            let message = PANIC_MESSAGE.lock().map(|m| m.clone()).unwrap_or_default();
            if message.contains("event loop") {
                eprintln!("\n{DISPLAY_HELP}");
                std::process::exit(1);
            }
            if message.contains("no default font") {
                eprintln!("\n{FONT_HELP}");
                std::process::exit(1);
            }
            retry_with_software_drawing();
            std::panic::resume_unwind(panic)
        }
    }
}

const DISPLAY_HELP: &str = "\
sysmon could not open a window. Make sure it is started from a graphical
desktop session. On minimal systems, install the display libraries:
  Debian/Ubuntu: sudo apt install libxkbcommon-x11-0 libxcursor1 libxrandr2 libxi6 libwayland-client0
  Fedora:        sudo dnf install libxkbcommon-x11 libXcursor libXrandr libXi libwayland-client
  Arch:          sudo pacman -S libxkbcommon-x11 libxcursor libxrandr libxi wayland";

const FONT_HELP: &str = "\
sysmon could not find any font to draw text with. Install one, for example:
  Debian/Ubuntu: sudo apt install fonts-dejavu-core
  Fedora:        sudo dnf install dejavu-sans-fonts
  openSUSE:      sudo zypper install dejavu-fonts
  Arch:          sudo pacman -S ttf-dejavu";

/// The last panic's text, so `main` can tell what went wrong.
static PANIC_MESSAGE: Mutex<String> = Mutex::new(String::new());

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut m) = PANIC_MESSAGE.lock() {
            *m = info.to_string();
        }
        default(info);
    }));
}

/// Some machines (virtual machines, systems without Mesa) have no usable GPU
/// driver, and the GPU renderer gives up. Start again with iced's software
/// renderer instead, unless that was already tried.
fn retry_with_software_drawing() {
    use std::os::unix::process::CommandExt;
    if std::env::var_os("ICED_BACKEND").is_some() {
        return;
    }
    eprintln!("sysmon: the graphics driver could not be used, so the window is drawn in software instead.");
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/proc/self/exe"));
    // exec only returns if it failed.
    let err = std::process::Command::new(exe).args(std::env::args_os().skip(1)).env("ICED_BACKEND", "tiny-skia").exec();
    eprintln!("sysmon: could not restart: {err}");
}

fn run_window(prefs: Prefs) -> iced::Result {
    // The size the window had last time, within sensible bounds.
    let (w, h) = prefs.window.unwrap_or((1180, 800));
    App::run(Settings {
        window: window::Settings {
            size: Size::new(w.clamp(MIN_W, 4000) as f32, h.clamp(MIN_H, 3000) as f32),
            min_size: Some(Size::new(MIN_W as f32, MIN_H as f32)),
            platform_specific: window::settings::PlatformSpecific { application_id: "sysmon".into() },
            ..Default::default()
        },
        ..Settings::with_flags(prefs)
    })
}

const MIN_W: u32 = 900;
const MIN_H: u32 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Cpu,
    Memory,
    Gpu,
    Sensors,
    Storage,
    Processes,
    System,
    Settings,
}

impl Tab {
    const ALL: [Tab; 9] = [
        Tab::Overview,
        Tab::Cpu,
        Tab::Memory,
        Tab::Gpu,
        Tab::Sensors,
        Tab::Storage,
        Tab::Processes,
        Tab::System,
        Tab::Settings,
    ];

    fn key(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Cpu => "cpu",
            Tab::Memory => "memory",
            Tab::Gpu => "gpu",
            Tab::Sensors => "sensors",
            Tab::Storage => "storage",
            Tab::Processes => "processes",
            Tab::System => "system",
            Tab::Settings => "settings",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Cpu => "CPU",
            Tab::Memory => "Memory",
            Tab::Gpu => "Graphics",
            Tab::Sensors => "Sensors & Power",
            Tab::Storage => "Disks & Network",
            Tab::Processes => "Processes",
            Tab::System => "System",
            Tab::Settings => "Settings",
        }
    }

    fn from_key(k: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|t| t.key() == k)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interval(u64);

const INTERVALS: [Interval; 4] = [Interval(500), Interval(1000), Interval(2000), Interval(5000)];

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0 < 1000 {
            write!(f, "Every {:.1} s", self.0 as f32 / 1000.0)
        } else {
            write!(f, "Every {} s", self.0 / 1000)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light];
}

impl fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            ThemeChoice::System => "System theme",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TempUnit {
    Celsius,
    Fahrenheit,
}

impl fmt::Display for TempUnit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(if *self == TempUnit::Celsius { "°C" } else { "°F" })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Pid,
    Name,
    User,
    Cpu,
    Mem,
    Threads,
}

/// Settings kept between runs in ~/.config/sysmon/settings.conf.
#[derive(Debug, Clone)]
struct Prefs {
    theme: ThemeChoice,
    interval: Interval,
    tab: Tab,
    unit: TempUnit,
    show_kernel: bool,
    show_virtual: bool,
    /// Last window size in logical pixels.
    window: Option<(u32, u32)>,
    alerts: alerts::Settings,
}

impl Prefs {
    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("sysmon").join("settings.conf"))
    }

    fn load() -> Self {
        let mut p = Prefs {
            theme: ThemeChoice::System,
            interval: Interval(1000),
            tab: Tab::Overview,
            unit: TempUnit::Celsius,
            show_kernel: false,
            show_virtual: false,
            window: None,
            alerts: alerts::Settings::default(),
        };
        if let Some(text) = Self::path().and_then(|f| std::fs::read_to_string(f).ok()) {
            for line in text.lines() {
                match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
                    Some(("theme", v)) => {
                        p.theme = match v {
                            "dark" => ThemeChoice::Dark,
                            "light" => ThemeChoice::Light,
                            _ => ThemeChoice::System,
                        }
                    }
                    Some(("interval_ms", v)) => {
                        if let Some(i) = v.parse().ok().and_then(|ms| INTERVALS.into_iter().find(|i| i.0 == ms)) {
                            p.interval = i;
                        }
                    }
                    Some(("tab", v)) => p.tab = Tab::from_key(v).unwrap_or(p.tab),
                    Some(("unit", v)) => p.unit = if v == "f" { TempUnit::Fahrenheit } else { TempUnit::Celsius },
                    Some(("show_kernel_threads", v)) => p.show_kernel = v == "true",
                    Some(("show_virtual_interfaces", v)) => p.show_virtual = v == "true",
                    Some(("window", v)) => {
                        p.window = v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    }
                    Some((k, v)) if k.starts_with("alert_") => {
                        // alert_temp=on:90 or alert_temp=off:90
                        let name = &k["alert_".len()..];
                        if let (Some(kind), Some((state, value))) =
                            (alerts::Kind::ALL.into_iter().find(|a| a.key() == name), v.split_once(':'))
                        {
                            if let Ok(value) = value.parse::<u32>() {
                                p.alerts.set(kind, alerts::Limit { on: state == "on", value });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        stats::FAHRENHEIT.store(p.unit == TempUnit::Fahrenheit, Ordering::Relaxed);
        p
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let theme = match self.theme {
            ThemeChoice::System => "system",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        };
        let mut body = format!(
            "theme={theme}\ninterval_ms={}\ntab={}\nunit={}\nshow_kernel_threads={}\nshow_virtual_interfaces={}\n",
            self.interval.0,
            self.tab.key(),
            if self.unit == TempUnit::Fahrenheit { "f" } else { "c" },
            self.show_kernel,
            self.show_virtual,
        );
        if let Some((w, h)) = self.window {
            body.push_str(&format!("window={w}x{h}\n"));
        }
        for kind in alerts::Kind::ALL {
            let l = self.alerts.get(kind);
            body.push_str(&format!("alert_{}={}:{}\n", kind.key(), if l.on { "on" } else { "off" }, l.value));
        }
        let _ = std::fs::write(path, body);
    }
}

/// Whether the desktop is set to a dark style. Checks GNOME (and the many
/// desktops that use its settings), KDE, Xfce and the GTK_THEME variable.
fn system_prefers_dark() -> Option<bool> {
    if let Ok(t) = std::env::var("GTK_THEME") {
        return Some(t.to_lowercase().contains("dark"));
    }
    let run = |program: &str, args: &[&str]| -> Option<String> {
        let path =
            std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join(program)).find(|p| p.is_file())?;
        gpu::run_with_timeout(&path, args, Duration::from_millis(800)).map(|s| s.to_lowercase())
    };
    if let Some(scheme) = run("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]) {
        if scheme.contains("dark") {
            return Some(true);
        }
        if scheme.contains("light") {
            return Some(false);
        }
        // "default": older desktops (Cinnamon, MATE, Budgie) name a dark theme instead.
        if let Some(theme) = run("gsettings", &["get", "org.gnome.desktop.interface", "gtk-theme"]) {
            return Some(theme.contains("dark"));
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    if let Ok(kde) = std::fs::read_to_string(config.join("kdeglobals")) {
        if let Some(line) = kde.lines().find(|l| l.starts_with("ColorScheme=")) {
            return Some(line.to_lowercase().contains("dark"));
        }
    }
    run("xfconf-query", &["-c", "xsettings", "-p", "/Net/ThemeName"]).map(|t| t.contains("dark"))
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Sampled(Option<Arc<Snapshot>>),
    Tab(Tab),
    TabIndex(usize),
    Interval(Interval),
    Theme(ThemeChoice),
    SystemDark(Option<bool>),
    Unit(TempUnit),
    TogglePause,
    Query(String),
    Sort(SortBy),
    MineOnly(bool),
    ShowKernel(bool),
    ShowVirtual(bool),
    Select(u32),
    CloseDetails,
    FocusSearch,
    CopyReport,
    Resized(u32, u32),
    AlertOn(alerts::Kind, bool),
    AlertValue(alerts::Choice),
    TestAlert,
    Notified(Option<bool>),
    ExportCsv,
    OpenFolder(PathBuf),
    DismissBanner,
    Quit,
}

/// A message across the top of the window: an alert, or where a file went.
struct Banner {
    text: String,
    at: Instant,
    urgent: bool,
    folder: Option<PathBuf>,
}

struct App {
    info: StaticInfo,
    collector: Arc<Mutex<Collector>>,
    snap: Arc<Snapshot>,
    hist: History,
    prefs: Prefs,
    system_dark: bool,
    theme_checked: Instant,
    paused: bool,
    busy: bool,
    query: String,
    sort: SortBy,
    desc: bool,
    mine_only: bool,
    selected: Option<u32>,
    my_uid: u32,
    width: u32,
    copied: Option<Instant>,
    /// The System tab's report and when it was gathered.
    report: Option<(Instant, Vec<report::Section>)>,
    watcher: alerts::Watcher,
    banner: Option<Banner>,
    /// When the window was last resized, until the new size is saved.
    resized_at: Option<Instant>,
}

fn search_id() -> text_input::Id {
    text_input::Id::new("search")
}

/// Runs blocking work on its own thread and hands the result back to the
/// window as a message, so a slow /proc or nvidia-smi never stalls drawing.
fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Option<T> + Send + 'static,
    done: fn(Option<T>) -> Message,
) -> Command<Message> {
    Command::perform(
        async move {
            let (tx, rx) = oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(work());
            });
            rx.await.ok().flatten()
        },
        done,
    )
}

fn sample(c: Arc<Mutex<Collector>>, delay: Duration) -> Command<Message> {
    off_thread(
        move || {
            std::thread::sleep(delay);
            c.lock().map(|mut c| Arc::new(c.sample())).ok()
        },
        Message::Sampled,
    )
}

impl App {
    /// Tabs worth showing on this machine (no Graphics tab without a GPU).
    fn tabs(&self) -> Vec<Tab> {
        Tab::ALL.into_iter().filter(|t| *t != Tab::Gpu || !self.snap.gpus.is_empty()).collect()
    }

    /// Re-reads the hardware report when the System tab is open and the
    /// copy is missing or older than ten seconds (it is mostly static).
    fn refresh_report(&mut self) {
        if self.prefs.tab == Tab::System && self.report.as_ref().is_none_or(|r| r.0.elapsed() > Duration::from_secs(10))
        {
            self.report = Some((Instant::now(), report::gather(&self.info, &self.snap)));
        }
    }

    fn show_banner(&mut self, text: String, urgent: bool, folder: Option<PathBuf>) {
        self.banner = Some(Banner { text, at: Instant::now(), urgent, folder });
    }

    fn dark(&self) -> bool {
        match self.prefs.theme {
            ThemeChoice::System => self.system_dark,
            ThemeChoice::Dark => true,
            ThemeChoice::Light => false,
        }
    }
}

impl Application for App {
    type Executor = executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = Prefs;

    fn new(prefs: Prefs) -> (Self, Command<Message>) {
        let info = StaticInfo::load();
        let mut collector = Collector::new(info.cpu_threads);
        // The first reading only sets the baseline that rates are measured from.
        let snap = Arc::new(collector.sample());
        let app = App {
            info,
            collector: Arc::new(Mutex::new(collector)),
            snap,
            hist: History::default(),
            prefs,
            system_dark: true,
            theme_checked: Instant::now(),
            paused: false,
            busy: true,
            query: String::new(),
            sort: SortBy::Cpu,
            desc: true,
            mine_only: false,
            selected: None,
            // SAFETY: getuid cannot fail.
            my_uid: unsafe { libc::getuid() },
            width: 1180,
            copied: None,
            report: None,
            watcher: alerts::Watcher::default(),
            banner: None,
            resized_at: None,
        };
        // A second reading straight away so the numbers fill in quickly.
        let first = sample(app.collector.clone(), Duration::from_millis(400));
        (app, Command::batch([first, off_thread(system_prefers_dark, Message::SystemDark)]))
    }

    fn title(&self) -> String {
        format!("System Monitor — {}", self.info.hostname)
    }

    fn theme(&self) -> Theme {
        if self.dark() {
            style::dark()
        } else {
            style::light()
        }
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Tick => {
                let mut cmds = Vec::new();
                if !self.paused && !self.busy {
                    self.busy = true;
                    cmds.push(sample(self.collector.clone(), Duration::ZERO));
                }
                // Save the window size once a resize has settled.
                if self.resized_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                    self.resized_at = None;
                    self.prefs.save();
                }
                if self.banner.as_ref().is_some_and(|b| b.at.elapsed() > Duration::from_secs(30)) {
                    self.banner = None;
                }
                // Follow the desktop if the user flips light/dark mode.
                if self.prefs.theme == ThemeChoice::System && self.theme_checked.elapsed() > Duration::from_secs(5) {
                    self.theme_checked = Instant::now();
                    cmds.push(off_thread(system_prefers_dark, Message::SystemDark));
                }
                return Command::batch(cmds);
            }
            Message::Sampled(snap) => {
                self.busy = false;
                if let Some(s) = snap {
                    if !self.paused {
                        self.hist.record(&s);
                        self.snap = s;
                        self.refresh_report();
                    }
                    let fired = self.watcher.check(&self.prefs.alerts, &self.snap, Instant::now());
                    if let Some(first) = fired.first() {
                        self.show_banner(format!("{}: {}", first.title, first.body), first.urgent, None);
                    }
                    let sends: Vec<_> = fired
                        .into_iter()
                        .map(|a| off_thread(move || Some(alerts::notify(&a)), Message::Notified))
                        .collect();
                    return Command::batch(sends);
                }
            }
            Message::Tab(t) => {
                self.prefs.tab = t;
                self.prefs.save();
                self.refresh_report();
            }
            Message::TabIndex(i) => {
                if let Some(t) = self.tabs().get(i) {
                    self.prefs.tab = *t;
                    self.prefs.save();
                    self.refresh_report();
                }
            }
            Message::Interval(i) => {
                self.prefs.interval = i;
                self.prefs.save();
            }
            Message::Theme(t) => {
                self.prefs.theme = t;
                self.prefs.save();
                if t == ThemeChoice::System {
                    return off_thread(system_prefers_dark, Message::SystemDark);
                }
            }
            Message::SystemDark(d) => self.system_dark = d.unwrap_or(true),
            Message::Unit(u) => {
                self.prefs.unit = u;
                stats::FAHRENHEIT.store(u == TempUnit::Fahrenheit, Ordering::Relaxed);
                self.prefs.save();
            }
            Message::TogglePause => self.paused = !self.paused,
            Message::Query(q) => self.query = q,
            Message::Sort(by) => {
                if self.sort == by {
                    self.desc = !self.desc;
                } else {
                    self.sort = by;
                    // Numbers read best largest-first, names A to Z.
                    self.desc = matches!(by, SortBy::Cpu | SortBy::Mem | SortBy::Threads);
                }
            }
            Message::MineOnly(v) => self.mine_only = v,
            Message::ShowKernel(v) => {
                self.prefs.show_kernel = v;
                self.prefs.save();
            }
            Message::ShowVirtual(v) => {
                self.prefs.show_virtual = v;
                self.prefs.save();
            }
            Message::Select(pid) => {
                self.selected = if self.selected == Some(pid) { None } else { Some(pid) };
            }
            Message::CloseDetails => self.selected = None,
            Message::FocusSearch => {
                self.prefs.tab = Tab::Processes;
                return text_input::focus(search_id());
            }
            Message::CopyReport => {
                self.copied = Some(Instant::now());
                return clipboard::write(report::to_text(&report::gather(&self.info, &self.snap)));
            }
            Message::Resized(w, h) => {
                self.width = w;
                if self.prefs.window != Some((w, h)) {
                    self.prefs.window = Some((w, h));
                    self.resized_at = Some(Instant::now());
                }
            }
            Message::AlertOn(kind, on) => {
                let l = self.prefs.alerts.get(kind);
                self.prefs.alerts.set(kind, alerts::Limit { on, ..l });
                self.prefs.save();
            }
            Message::AlertValue(c) => {
                let l = self.prefs.alerts.get(c.kind);
                self.prefs.alerts.set(c.kind, alerts::Limit { value: c.value, ..l });
                self.prefs.save();
            }
            Message::TestAlert => {
                let a = alerts::Alert {
                    title: "sysmon test alert".into(),
                    body: "Alerts are working. This is how a warning will look.".into(),
                    urgent: false,
                };
                return off_thread(move || Some(alerts::notify(&a)), Message::Notified);
            }
            Message::Notified(ok) => {
                if ok == Some(false) {
                    self.show_banner(
                        "Desktop notifications are not available (install libnotify / notify-send). Alerts will still appear here."
                            .into(),
                        false,
                        None,
                    );
                }
            }
            Message::ExportCsv => {
                let dir = history::download_dir();
                match history::save_csv(&self.hist, &dir) {
                    Ok(path) => self.show_banner(format!("History saved to {}", view::tilde(&path)), false, Some(dir)),
                    Err(e) => self.show_banner(format!("Could not save the history: {e}"), true, None),
                }
            }
            Message::OpenFolder(dir) => {
                if let Some(p) = gpu::in_path("xdg-open") {
                    let _ = std::process::Command::new(p)
                        .arg(dir)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
            }
            Message::DismissBanner => self.banner = None,
            Message::Quit => {
                if self.resized_at.is_some() {
                    self.prefs.save();
                }
                return window::close(window::Id::MAIN);
            }
        }
        Command::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(Duration::from_millis(self.prefs.interval.0)).map(|_| Message::Tick),
            keyboard::on_key_press(on_key),
            event::listen_with(|e, _| match e {
                Event::Window(_, window::Event::Resized { width, height }) => Some(Message::Resized(width, height)),
                _ => None,
            }),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        view::view(self)
    }
}

fn on_key(key: keyboard::Key, m: keyboard::Modifiers) -> Option<Message> {
    if let keyboard::Key::Named(keyboard::key::Named::Escape) = key {
        return Some(Message::CloseDetails);
    }
    let keyboard::Key::Character(c) = key else { return None };
    if !m.command() {
        return None;
    }
    match c.as_str() {
        d @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
            Some(Message::TabIndex(d.parse::<usize>().ok()? - 1))
        }
        "e" => Some(Message::ExportCsv),
        "f" => Some(Message::FocusSearch),
        "p" => Some(Message::TogglePause),
        "q" => Some(Message::Quit),
        _ => None,
    }
}
