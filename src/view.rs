//! Everything the window draws: the header, the tab bar and each tab.

use std::collections::VecDeque;
use std::time::Duration;

use iced::alignment::Horizontal;
use iced::widget::{
    button, canvas, checkbox, column, container, horizontal_space, mouse_area, pick_list, progress_bar, row,
    scrollable, text, text_input, Column, Row, Space,
};
use iced::{Alignment, Color, Element, Font, Length, Theme};

use crate::alerts::{self, Choice};
use crate::graph::{Graph, Series};
use crate::history::HISTORY;
use crate::remote::Failure;
use crate::stats::{self, fmt_bytes, fmt_duration, fmt_rate, fmt_temp, fmt_temp_exact, Snapshot};
use crate::{
    search_id, style, App, Link, Message, SortBy, Source, SourceChoice, Tab, TempUnit, ThemeChoice, INTERVALS,
};

const BOLD: Font = Font { weight: iced::font::Weight::Bold, ..Font::DEFAULT };
/// Most process rows drawn at once; more than this slows the window down.
const MAX_ROWS: usize = 400;
/// Above this many threads the per-core graph turns into spaghetti.
const MAX_CORE_LINES: usize = 16;

type Text<'a> = text::Text<'a, Theme, iced::Renderer>;

pub fn view(app: &App) -> Element<'_, Message> {
    let theme = iced::Application::theme(app);
    let tab = if app.tabs().contains(&app.prefs.tab) { app.prefs.tab } else { Tab::Overview };
    // A server with no reading yet: say so rather than showing zeros.
    let waiting = app.source != Source::Local && app.hist.times.is_empty() && tab != Tab::Settings;
    let body: Element<Message> = if waiting {
        waiting_panel(app, &theme)
    } else {
        match tab {
            Tab::Overview => overview(app, &theme),
            Tab::Cpu => cpu_tab(app, &theme),
            Tab::Memory => memory_tab(app, &theme),
            Tab::Gpu => gpu_tab(app, &theme),
            Tab::Sensors => sensors_tab(app, &theme),
            Tab::Storage => storage_tab(app, &theme),
            Tab::Processes => processes_tab(app, &theme),
            Tab::System => system_tab(app, &theme),
            Tab::Settings => settings_tab(app, &theme),
        }
    };
    let body: Element<Message> = if tab == Tab::Processes {
        container(body).padding([12, 16, 16, 16]).into()
    } else {
        scrollable(container(body).padding([12, 16, 16, 16])).height(Length::Fill).into()
    };
    let mut page = column![header(app, tab, &theme)];
    if let Some(strip) = connection_strip(app, &theme) {
        page = page.push(strip);
    }
    if let Some(b) = &app.banner {
        page = page.push(banner(b, &theme));
    }
    page.push(body).into()
}

// ---- small building blocks ----

fn muted<'a>(s: impl ToString, theme: &Theme) -> Text<'a> {
    text(s.to_string()).size(13).style(style::muted(theme))
}

fn big<'a>(s: impl ToString, c: Color) -> Text<'a> {
    text(s.to_string()).size(30).font(BOLD).style(c)
}

fn card<'a>(title: &str, theme: &Theme, content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(
        column![text(title.to_uppercase()).size(12).font(BOLD).style(style::muted(theme)), content.into()].spacing(10),
    )
    .padding(14)
    .width(Length::Fill)
    .style(style::card())
    .into()
}

/// A label on the left and its value on the right.
fn kv<'a>(label: impl ToString, value: impl ToString, theme: &Theme) -> Element<'a, Message> {
    row![
        muted(label, theme),
        horizontal_space(),
        text(value.to_string()).size(13).horizontal_alignment(Horizontal::Right)
    ]
    .spacing(12)
    .align_items(Alignment::Center)
    .into()
}

fn meter<'a>(pct: f32, c: Color) -> Element<'a, Message> {
    progress_bar(0.0..=100.0, pct.clamp(0.0, 100.0)).height(8).style(style::bar(c)).into()
}

fn chart(g: Graph<'_>, height: f32) -> Element<'_, Message> {
    canvas(g).width(Length::Fill).height(height).into()
}

/// A small unlabelled graph for the overview cards.
fn spark(series: Vec<Series<'_>>, max: Option<f32>, floor: f32, unit: fn(f32) -> String) -> Graph<'_> {
    Graph { series, max, floor, capacity: HISTORY, unit, labels: false }
}

fn big_graph(series: Vec<Series<'_>>, max: Option<f32>, floor: f32, unit: fn(f32) -> String) -> Graph<'_> {
    Graph { series, max, floor, capacity: HISTORY, unit, labels: true }
}

fn series<'a>(name: &str, data: &'a VecDeque<f32>, color: Color) -> Series<'a> {
    Series { name: name.to_string(), data, color }
}

/// Makes a whole card open another tab when clicked.
fn link(e: Element<'_, Message>, t: Tab) -> Element<'_, Message> {
    mouse_area(e).on_press(Message::Tab(t)).into()
}

pub(crate) fn pct_label(v: f32) -> String {
    format!("{v:.0}%")
}

fn rate_label(v: f32) -> String {
    fmt_rate(v as f64)
}

/// A coloured dot and name, for graph legends.
fn legend<'a>(items: &[(&str, Color)]) -> Element<'a, Message> {
    Row::with_children(items.iter().map(|(name, c)| {
        row![text("●").size(12).style(*c), text(name.to_string()).size(12)]
            .spacing(4)
            .align_items(Alignment::Center)
            .into()
    }))
    .spacing(14)
    .into()
}

fn or_dash(v: Option<String>) -> String {
    v.unwrap_or_else(|| "—".into())
}

pub(crate) fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub(crate) fn state_name(c: char) -> &'static str {
    match c {
        'R' => "Running",
        'S' => "Sleeping",
        'D' => "Disk wait",
        'I' => "Idle",
        'Z' => "Zombie",
        'T' | 't' => "Stopped",
        _ => "Other",
    }
}

pub(crate) fn avg_freq(s: &Snapshot) -> Option<f32> {
    let f: Vec<f32> = s.freqs_mhz.iter().copied().filter(|f| *f > 0.0).collect();
    (!f.is_empty()).then(|| f.iter().sum::<f32>() / f.len() as f32)
}

pub(crate) fn ghz(mhz: f32) -> String {
    if mhz >= 1000.0 {
        format!("{:.2} GHz", mhz / 1000.0)
    } else {
        format!("{mhz:.0} MHz")
    }
}

/// Groups hwmon chips for display, folding the many ACPI zones together.
pub(crate) fn sensor_group(chip: &str) -> String {
    if chip.starts_with("acpitz") {
        "ACPI thermal zones".into()
    } else {
        stats::friendly_chip(chip)
    }
}

/// What kind of connection an interface is, from its name.
pub(crate) fn net_kind(n: &stats::Net) -> &'static str {
    let name = n.name.as_str();
    if n.wireless {
        "Wi-Fi"
    } else if !n.virtual_ {
        if name.starts_with("ww") {
            "Mobile broadband"
        } else {
            "Ethernet"
        }
    } else if ["tun", "tap", "wg", "tailscale", "ppp", "proton", "nordlynx", "zt", "mullvad"]
        .iter()
        .any(|p| name.starts_with(p))
    {
        "VPN"
    } else if name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth") {
        "Docker"
    } else if name.starts_with("virbr")
        || name.starts_with("vnet")
        || name.starts_with("vbox")
        || name.starts_with("vmnet")
    {
        "Virtual machine network"
    } else if name.starts_with("br") {
        "Bridge"
    } else {
        "Virtual"
    }
}

/// Lays cards out in rows of equal width, padding the last row.
fn grid<'a>(items: Vec<Element<'a, Message>>, per_row: usize) -> Element<'a, Message> {
    let mut rows = Column::new().spacing(12);
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        let mut r = Row::new().spacing(12);
        for i in 0..per_row {
            r = match items.next() {
                Some(e) => r.push(e),
                None if i > 0 => r.push(Space::new(Length::Fill, Length::Shrink)),
                None => break,
            };
        }
        rows = rows.push(r);
    }
    rows.into()
}

// ---- header ----

fn header<'a>(app: &'a App, current: Tab, theme: &Theme) -> Element<'a, Message> {
    let title = row![
        text("System Monitor").size(18).font(BOLD),
        muted(format!("{}  ·  {}", app.info.hostname, app.info.os), theme),
    ]
    .spacing(14)
    .align_items(Alignment::Center);
    // "This computer", each saved server, and a shortcut to add one.
    let mut choices = vec![SourceChoice { source: Some(Source::Local), label: "This computer".into() }];
    choices.extend(
        app.prefs
            .servers
            .iter()
            .enumerate()
            .map(|(i, s)| SourceChoice { source: Some(Source::Remote(i)), label: format!("Server: {}", s.name) }),
    );
    choices.push(SourceChoice { source: None, label: "Add a server…".into() });
    let current_choice = choices.iter().find(|c| c.source == Some(app.source)).cloned();
    let controls = row![
        pick_list(choices, current_choice, Message::PickSource).text_size(13).padding([4, 8]).width(190),
        muted(if app.snap.uptime > 0 { format!("Up {}", fmt_duration(app.snap.uptime)) } else { String::new() }, theme),
        pick_list(&INTERVALS[..], Some(app.prefs.interval), Message::Interval).text_size(13).padding([4, 8]),
        pick_list(&[TempUnit::Celsius, TempUnit::Fahrenheit][..], Some(app.prefs.unit), Message::Unit)
            .text_size(13)
            .padding([4, 8]),
        pick_list(&ThemeChoice::ALL[..], Some(app.prefs.theme), Message::Theme).text_size(13).padding([4, 8]),
        button(text(if app.paused { "Resume" } else { "Pause" }).size(13))
            .padding([5, 12])
            .style(if app.paused { iced::theme::Button::Primary } else { iced::theme::Button::Secondary })
            .on_press(Message::TogglePause),
    ]
    .spacing(8)
    .align_items(Alignment::Center);
    let tabs = Row::with_children(app.tabs().into_iter().map(|t| {
        button(text(t.label()).size(14))
            .padding([6, 14])
            .style(style::tab(current == t))
            .on_press(Message::Tab(t))
            .into()
    }))
    .spacing(4);
    container(
        column![row![title, horizontal_space(), controls].spacing(12).align_items(Alignment::Center), tabs].spacing(10),
    )
    .padding([12, 16, 8, 16])
    .width(Length::Fill)
    .style(style::header())
    .into()
}

// ---- overview ----

fn overview<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let h = &app.hist;
    let mut cards: Vec<Element<Message>> = Vec::new();

    cards.push(link(
        card(
            "CPU",
            theme,
            column![
                row![
                    big(pct_label(s.cpu_total), style::CPU),
                    horizontal_space(),
                    muted(avg_freq(s).map(ghz).unwrap_or_default(), theme)
                ]
                .align_items(Alignment::Center),
                muted(truncate(&app.info.cpu_model, 40), theme),
                chart(spark(vec![series("CPU", &h.cpu, style::CPU)], Some(100.0), 1.0, pct_label), 60.0),
            ]
            .spacing(6),
        ),
        Tab::Cpu,
    ));
    cards.push(link(
        card(
            "Memory",
            theme,
            column![
                big(pct_label(s.mem.used_pct()), style::MEM),
                muted(format!("{} of {} in use", fmt_bytes(s.mem.used), fmt_bytes(s.mem.total)), theme),
                chart(spark(vec![series("RAM", &h.mem, style::MEM)], Some(100.0), 1.0, pct_label), 60.0),
            ]
            .spacing(6),
        ),
        Tab::Memory,
    ));
    if let Some(g) = s.gpus.first() {
        let detail = match (g.vram_used, g.vram_total) {
            (Some(u), Some(t)) => format!("{} of {} video memory", fmt_bytes(u), fmt_bytes(t)),
            _ => or_dash(g.freq_mhz.map(|f| format!("Clock {}", ghz(f)))),
        };
        let mut c = column![
            row![
                big(g.usage.map(pct_label).unwrap_or_else(|| "—".into()), style::GPU),
                horizontal_space(),
                muted(g.temp.map(fmt_temp).unwrap_or_default(), theme)
            ]
            .align_items(Alignment::Center),
            muted(truncate(&g.name, 40), theme),
        ]
        .spacing(6);
        if let Some(q) = h.gpu.first().filter(|q| !q.is_empty()) {
            c = c.push(chart(spark(vec![series("GPU", q, style::GPU)], Some(100.0), 1.0, pct_label), 44.0));
        }
        cards.push(link(
            card(if s.gpus.len() > 1 { "Graphics (GPU 1)" } else { "Graphics" }, theme, c.push(muted(detail, theme))),
            Tab::Gpu,
        ));
    }

    let temp = s.cpu_temp();
    cards.push(link(
        card(
            "CPU temperature",
            theme,
            column![
                big(
                    temp.map(fmt_temp).unwrap_or_else(|| "—".into()),
                    temp.map(|t| style::temp_level(t, None)).unwrap_or(style::TEMP)
                ),
                muted(
                    s.fans.first().map(|f| format!("Fan {} rpm", f.rpm)).unwrap_or_else(|| {
                        match s.temps.iter().map(|t| t.celsius).reduce(f32::max) {
                            Some(t) => format!("Hottest sensor {}", fmt_temp(t)),
                            None => "No temperature sensors found".into(),
                        }
                    }),
                    theme
                ),
                chart(spark(vec![series("CPU", &h.temp, style::TEMP)], Some(100.0), 1.0, fmt_temp), 60.0),
            ]
            .spacing(6),
        ),
        Tab::Sensors,
    ));

    if let Some(b) = s.battery() {
        let detail = match (b.status.as_str(), b.time_left) {
            ("Discharging", Some(t)) => format!(" · {} left", fmt_duration(t)),
            ("Charging", Some(t)) => format!(" · full in {}", fmt_duration(t)),
            _ => String::new(),
        };
        let colour = style::level(100.0 - b.percent);
        cards.push(link(
            card(
                if b.count > 1 { "Batteries" } else { "Battery" },
                theme,
                column![
                    row![
                        big(pct_label(b.percent), colour),
                        horizontal_space(),
                        muted(b.power_w.map(|w| format!("{w:.1} W")).unwrap_or_default(), theme)
                    ]
                    .align_items(Alignment::Center),
                    muted(format!("{}{detail}", b.status), theme),
                    meter(b.percent, colour),
                ]
                .spacing(6),
            ),
            Tab::Sensors,
        ));
    }

    let root = s.mounts.iter().find(|m| m.mount == "/").or(s.mounts.first());
    cards.push(link(
        card(
            "Disk",
            theme,
            column![
                row![
                    big(root.map(|m| pct_label(m.used_pct())).unwrap_or_else(|| "—".into()), style::READ),
                    horizontal_space(),
                    column![
                        text(format!("R {}", fmt_rate(s.disk_read()))).size(12).style(style::READ),
                        text(format!("W {}", fmt_rate(s.disk_write()))).size(12).style(style::WRITE)
                    ]
                    .align_items(Alignment::End)
                ]
                .align_items(Alignment::Center),
                muted(root.map(|m| format!("{} free on {}", fmt_bytes(m.avail), m.mount)).unwrap_or_default(), theme),
                chart(
                    spark(
                        vec![series("Read", &h.read, style::READ), series("Write", &h.write, style::WRITE)],
                        None,
                        64.0 * 1024.0,
                        rate_label
                    ),
                    44.0
                ),
            ]
            .spacing(6),
        ),
        Tab::Storage,
    ));
    cards.push(link(
        card(
            "Network",
            theme,
            column![
                row![
                    // Arrows in regular weight: the bold face has no arrow glyphs.
                    text("↓").size(20).style(style::RX),
                    text(fmt_rate(s.net_rx())).size(20).font(BOLD).style(style::RX),
                    horizontal_space(),
                    text("↑").size(20).style(style::TX),
                    text(fmt_rate(s.net_tx())).size(20).font(BOLD).style(style::TX),
                ]
                .spacing(4)
                .align_items(Alignment::Center),
                muted(
                    s.nets
                        .iter()
                        .find(|n| n.up && !n.virtual_)
                        .map(|n| format!(
                            "{} · {}",
                            n.name,
                            n.addrs.first().or(n.addrs6.first()).cloned().unwrap_or_else(|| "no address".into())
                        ))
                        .unwrap_or_else(|| "Not connected".into()),
                    theme
                ),
                chart(
                    spark(
                        vec![series("Down", &h.rx, style::RX), series("Up", &h.tx, style::TX)],
                        None,
                        16.0 * 1024.0,
                        rate_label
                    ),
                    52.0
                ),
            ]
            .spacing(6),
        ),
        Tab::Storage,
    ));

    let per_row = if cards.len() <= 6 { 3 } else { 4 };

    let mut top: Vec<&stats::Proc> = s.procs.iter().filter(|p| !p.kernel || app.prefs.show_kernel).collect();
    top.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
    let rows = Column::with_children(top.iter().take(6).enumerate().map(|(i, p)| {
        container(
            row![
                text(truncate(&p.name, 30)).size(13).width(Length::FillPortion(3)),
                muted(&p.user, theme).width(Length::FillPortion(2)),
                text(format!("{:.1}%", p.cpu)).size(13).width(70).horizontal_alignment(Horizontal::Right),
                text(fmt_bytes(p.mem)).size(13).width(90).horizontal_alignment(Horizontal::Right),
            ]
            .align_items(Alignment::Center),
        )
        .padding([4, 6])
        .style(style::row_bg(false, i % 2 == 1))
        .into()
    }));

    column![
        grid(cards, per_row),
        link(card("Busiest processes", theme, rows), Tab::Processes),
        muted("Click a card for details.  Ctrl+1–9 switch tabs, Ctrl+F searches processes, Ctrl+P pauses, Ctrl+E exports history.", theme),
    ]
    .spacing(12)
    .into()
}

// ---- CPU ----

fn cpu_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let h = &app.hist;
    let usage = card(
        "CPU usage",
        theme,
        column![
            row![
                big(pct_label(s.cpu_total), style::CPU),
                horizontal_space(),
                column![
                    kv(
                        "Load average (1, 5, 15 min)",
                        format!("{:.2}  {:.2}  {:.2}", s.load[0], s.load[1], s.load[2]),
                        theme
                    ),
                    kv("Average clock", or_dash(avg_freq(s).map(ghz)), theme),
                    kv(
                        "Processes · threads · running",
                        format!("{} · {} · {}", group(s.tasks), group(s.threads), s.running),
                        theme
                    ),
                ]
                .spacing(4)
                .width(380)
            ]
            .align_items(Alignment::Center),
            chart(big_graph(vec![series("CPU", &h.cpu, style::CPU)], Some(100.0), 1.0, pct_label), 200.0),
            muted(format!("Last {HISTORY} samples. Hover the graph to read past values."), theme),
        ]
        .spacing(10),
    );

    let n = s.cpu_cores.len();
    let cols = match n {
        0..=8 => 1,
        9..=32 => 2,
        _ => 4,
    };
    let core = |i: usize, v: f32| -> Element<'a, Message> {
        let c = style::CORES[i % style::CORES.len()];
        row![
            text("●").size(12).style(if n <= MAX_CORE_LINES { c } else { style::level(v) }),
            text(format!("CPU {i}")).size(13).width(58),
            container(meter(v, style::level(v))).width(Length::Fill),
            text(pct_label(v)).size(13).width(44).horizontal_alignment(Horizontal::Right),
            muted(s.freqs_mhz.get(i).filter(|f| **f > 0.0).map(|f| ghz(*f)).unwrap_or_default(), theme)
                .width(72)
                .horizontal_alignment(Horizontal::Right),
        ]
        .spacing(8)
        .align_items(Alignment::Center)
        .width(Length::Fill)
        .into()
    };
    let cores: Vec<Element<Message>> = s.cpu_cores.iter().enumerate().map(|(i, v)| core(i, *v)).collect();
    let mut per_core = column![grid_tight(cores, cols)].spacing(12);
    if n <= MAX_CORE_LINES {
        let lines = h
            .cores
            .iter()
            .enumerate()
            .map(|(i, q)| series(&format!("CPU {i}"), q, style::CORES[i % style::CORES.len()]))
            .collect();
        per_core = per_core.push(chart(big_graph(lines, Some(100.0), 1.0, pct_label), 170.0));
    } else {
        per_core = per_core
            .push(muted(format!("Per-core history is hidden with {n} threads; the bars above update live."), theme));
    }

    let about = card(
        "Processor",
        theme,
        column![
            kv("Model", &app.info.cpu_model, theme),
            kv("Logical processors", app.info.cpu_threads, theme),
            kv("Architecture", &app.info.arch, theme),
            kv("Computer", if app.info.machine.is_empty() { "—" } else { &app.info.machine }, theme),
            kv("Kernel", &app.info.kernel, theme),
            kv("Uptime", fmt_duration(s.uptime), theme),
            muted("More detail (cores, caches, clock limits) is on the System tab.", theme),
        ]
        .spacing(6),
    );
    let layout: Element<Message> = if cols >= 2 {
        column![card("Per core", theme, per_core), about].spacing(12).into()
    } else {
        row![card("Per core", theme, per_core), about].spacing(12).into()
    };
    column![usage, layout].spacing(12).into()
}

/// Like `grid` but with tighter spacing, for rows of small meters.
fn grid_tight<'a>(items: Vec<Element<'a, Message>>, per_row: usize) -> Element<'a, Message> {
    let mut rows = Column::new().spacing(8);
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        let mut r = Row::new().spacing(24);
        for _ in 0..per_row {
            r = r.push(items.next().unwrap_or_else(|| Space::new(Length::Fill, Length::Shrink).into()));
        }
        rows = rows.push(r);
    }
    rows.into()
}

// ---- memory ----

fn memory_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let m = &app.snap.mem;
    let h = &app.hist;
    let history = card(
        "Memory and swap use",
        theme,
        column![
            legend(&[("Memory", style::MEM), ("Swap", style::SWAP)]),
            chart(
                big_graph(
                    vec![series("Memory", &h.mem, style::MEM), series("Swap", &h.swap, style::SWAP)],
                    Some(100.0),
                    1.0,
                    pct_label
                ),
                200.0
            ),
        ]
        .spacing(8),
    );

    // One bar split into in-use, reclaimable cache and free memory.
    let portion = |b: u64| ((b as f64 / m.total.max(1) as f64) * 1000.0).round().max(1.0) as u16;
    let seg = |b: u64, c: Color| -> Element<'a, Message> {
        container(Space::new(Length::Fill, 14))
            .width(Length::FillPortion(portion(b)))
            .style(move |_: &Theme| container::Appearance { background: Some(c.into()), ..Default::default() })
            .into()
    };
    // In use + available = total, and available = free + reclaimable cache.
    let reclaimable = m.available.saturating_sub(m.free);
    let free_colour = style::with_alpha(theme.palette().text, 0.12);
    let memory = card(
        "Memory",
        theme,
        column![
            row![big(pct_label(m.used_pct()), style::MEM), horizontal_space(), muted(format!("{} total", fmt_bytes(m.total)), theme)]
                .align_items(Alignment::Center),
            row![seg(m.used, style::MEM), seg(reclaimable, style::with_alpha(style::MEM, 0.4)), seg(m.free, free_colour)],
            legend(&[("In use", style::MEM), ("Reclaimable cache", style::with_alpha(style::MEM, 0.4)), ("Free", style::with_alpha(theme.palette().text, 0.3))]),
            kv("In use", fmt_bytes(m.used), theme),
            kv("Available (free + reclaimable cache)", fmt_bytes(m.available), theme),
            kv("Cache", fmt_bytes(m.cached), theme),
            kv("Buffers", fmt_bytes(m.buffers), theme),
            kv("Shared (tmpfs, graphics)", fmt_bytes(m.shared), theme),
            kv("Completely free", fmt_bytes(m.free), theme),
            muted("Linux uses spare memory as cache and gives it back when programs need it, so low free memory is normal.", theme),
        ]
        .spacing(8),
    );
    let swap = card(
        "Swap",
        theme,
        if m.swap_total == 0 {
            column![muted("No swap space is set up.", theme)]
        } else {
            column![
                big(pct_label(m.swap_pct()), style::SWAP),
                meter(m.swap_pct(), style::SWAP),
                kv("Used", fmt_bytes(m.swap_used), theme),
                kv("Total", fmt_bytes(m.swap_total), theme),
                muted("Swap is disk space (or compressed memory) used when RAM runs short. Heavy use slows the computer down.", theme),
            ]
            .spacing(8)
        },
    );
    column![history, row![memory, swap].spacing(12)].spacing(12).into()
}

// ---- graphics ----

fn gpu_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let h = &app.hist;
    Column::with_children(s.gpus.iter().enumerate().map(|(i, g)| {
        let mut lines = Vec::new();
        if let Some(q) = h.gpu.get(i).filter(|q| !q.is_empty()) {
            lines.push(series("Busy", q, style::GPU));
        }
        if let Some(q) = h.gpu_mem.get(i).filter(|q| !q.is_empty()) {
            lines.push(series("Video memory", q, style::MEM));
        }
        let mut left = column![row![
            big(g.usage.map(pct_label).unwrap_or_else(|| "—".into()), style::GPU),
            column![text(&g.name).size(15).font(BOLD), muted(format!("Driver: {}", g.driver), theme)].spacing(2),
        ]
        .spacing(16)
        .align_items(Alignment::Center),]
        .spacing(10);
        if !lines.is_empty() {
            let names: Vec<(&str, Color)> =
                lines.iter().map(|l| (if l.color == style::GPU { "Busy" } else { "Video memory" }, l.color)).collect();
            left = left.push(legend(&names)).push(chart(big_graph(lines, Some(100.0), 1.0, pct_label), 170.0));
        }
        let mut right = column![].spacing(8).width(Length::FillPortion(2));
        if let (Some(u), Some(t)) = (g.vram_used, g.vram_total) {
            let p = stats::pct(u, t);
            right = right
                .push(kv("Video memory", format!("{} of {}", fmt_bytes(u), fmt_bytes(t)), theme))
                .push(meter(p, style::MEM));
        }
        let clock = match (g.freq_mhz, g.max_freq_mhz) {
            (Some(f), Some(m)) => Some(format!("{} (max {})", ghz(f), ghz(m))),
            (Some(f), None) => Some(ghz(f)),
            (None, Some(m)) => Some(format!("max {}", ghz(m))),
            _ => None,
        };
        right = right
            .push(kv("Busy", or_dash(g.usage.map(|u| format!("{u:.0}%"))), theme))
            .push(kv("Clock", or_dash(clock), theme))
            .push(kv("Temperature", or_dash(g.temp.map(fmt_temp_exact)), theme))
            .push(kv("Power draw", or_dash(g.power_w.map(|w| format!("{w:.1} W"))), theme));
        if g.fan_rpm.is_some() || g.fan_pct.is_some() {
            let fan = match (g.fan_rpm, g.fan_pct) {
                (Some(r), Some(p)) => format!("{r} rpm ({p:.0}%)"),
                (Some(r), None) => format!("{r} rpm"),
                (None, Some(p)) => format!("{p:.0}%"),
                _ => unreachable!(),
            };
            right = right.push(kv("Fan", fan, theme));
        }
        if let Some(note) = &g.note {
            right = right.push(muted(note, theme));
        }
        card(
            &if s.gpus.len() > 1 { format!("GPU {}", i + 1) } else { "Graphics card".into() },
            theme,
            row![left.width(Length::FillPortion(3)), right].spacing(24),
        )
    }))
    .spacing(12)
    .into()
}

// ---- sensors and power ----

fn sensors_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let mut groups: Vec<(String, Vec<&stats::Temp>)> = Vec::new();
    for t in &s.temps {
        let g = sensor_group(&t.chip);
        match groups.iter_mut().find(|(n, _)| *n == g) {
            Some((_, v)) => v.push(t),
            None => groups.push((g, vec![t])),
        }
    }
    let temp_rows = Column::with_children(groups.into_iter().map(|(name, temps)| {
        column![
            text(name).size(13).font(BOLD),
            Column::with_children(temps.into_iter().map(|t| {
                let limit = t.crit.or(t.high).unwrap_or(100.0);
                let c = style::temp_level(t.celsius, t.crit.or(t.high));
                let limits = match (t.high, t.crit) {
                    (Some(h), Some(c)) if (h - c).abs() > 0.5 => {
                        format!("high {}, critical {}", fmt_temp(h), fmt_temp(c))
                    }
                    (_, Some(c)) => format!("critical {}", fmt_temp(c)),
                    (Some(h), None) => format!("high {}", fmt_temp(h)),
                    _ => String::new(),
                };
                row![
                    muted(&t.label, theme).width(170),
                    container(meter(t.celsius / limit * 100.0, c)).width(Length::Fill),
                    text(fmt_temp_exact(t.celsius)).size(13).width(70).horizontal_alignment(Horizontal::Right).style(c),
                    muted(limits, theme).width(170).horizontal_alignment(Horizontal::Right),
                ]
                .spacing(10)
                .align_items(Alignment::Center)
                .into()
            }))
            .spacing(6)
        ]
        .spacing(6)
        .into()
    }))
    .spacing(14);
    let temps = card(
        "Temperatures",
        theme,
        if s.temps.is_empty() {
            column![muted("No temperature sensors were found. Virtual machines and some desktops do not expose any; on desktops, installing lm-sensors and running sensors-detect can help.", theme)]
        } else {
            column![temp_rows]
        },
    );
    let history = card(
        "CPU temperature history",
        theme,
        chart(big_graph(vec![series("CPU", &app.hist.temp, style::TEMP)], Some(100.0), 1.0, fmt_temp), 150.0),
    );
    let fans = card(
        "Fans",
        theme,
        if s.fans.is_empty() {
            column![muted("This computer's firmware does not report fan speed to Linux, so it cannot be shown.", theme)]
        } else {
            Column::with_children(s.fans.iter().map(|f| kv(&f.label, format!("{} rpm", f.rpm), theme))).spacing(6)
        },
    );

    let mut power: Vec<Element<Message>> = s
        .batteries
        .iter()
        .map(|b| {
            let colour = style::level(100.0 - b.percent);
            let time = match (b.status.as_str(), b.time_left) {
                ("Discharging", Some(t)) => ("Time until empty", fmt_duration(t)),
                ("Charging", Some(t)) => ("Time until full", fmt_duration(t)),
                _ => ("Time remaining", "—".into()),
            };
            // The firmware gave no rate, so it was worked out from the charge level.
            let est = if b.estimated { " (estimated)" } else { "" };
            let title = if b.model.is_empty() {
                format!("Battery ({})", b.name)
            } else {
                format!("Battery ({}, {})", b.name, b.model)
            };
            card(
                &title,
                theme,
                column![
                    row![big(pct_label(b.percent), colour), horizontal_space(), text(&b.status).size(15).font(BOLD)]
                        .align_items(Alignment::Center),
                    meter(b.percent, colour),
                    kv(
                        "Charger",
                        match s.ac_online {
                            Some(true) => "Plugged in",
                            Some(false) => "Unplugged",
                            None => "—",
                        },
                        theme
                    ),
                    kv(
                        if b.status == "Charging" { "Charging rate" } else { "Power draw" },
                        or_dash(b.power_w.map(|w| format!("{w:.1} W{est}"))),
                        theme
                    ),
                    kv(time.0, if time.1 == "—" { time.1 } else { format!("{}{est}", time.1) }, theme),
                    kv("Health (capacity left vs. new)", or_dash(b.health.map(|h| format!("{h:.0}%"))), theme),
                    kv("Charge cycles", or_dash(b.cycles.map(|c| c.to_string())), theme),
                    kv("Voltage", or_dash(b.voltage.map(|v| format!("{v:.2} V"))), theme),
                    kv("Chemistry", if b.technology.is_empty() { "—" } else { &b.technology }, theme),
                ]
                .spacing(8),
            )
        })
        .collect();
    if s.batteries.is_empty() {
        power.push(card("Power", theme, muted("No battery found. The computer is running on mains power.", theme)));
    }
    if !s.device_batteries.is_empty() {
        power.push(card(
            "Wireless devices",
            theme,
            Column::with_children(s.device_batteries.iter().map(|d| {
                let value = match d.percent {
                    Some(p) => format!("{p:.0}%"),
                    None if !d.level.is_empty() => d.level.clone(),
                    None => "—".into(),
                };
                let status = if d.status == "Unknown" || d.status.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", d.status)
                };
                column![
                    kv(&d.name, format!("{value}{status}"), theme),
                    meter(d.percent.unwrap_or(0.0), style::level(100.0 - d.percent.unwrap_or(100.0)))
                ]
                .spacing(4)
                .into()
            }))
            .spacing(10),
        ));
    }
    power.push(fans);
    column![row![temps, Column::with_children(power).spacing(12).width(Length::Fill)].spacing(12), history]
        .spacing(12)
        .into()
}

// ---- disks and network ----

fn storage_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let h = &app.hist;
    let disks = card(
        "Disk activity",
        theme,
        column![
            row![
                legend(&[("Read", style::READ), ("Write", style::WRITE)]),
                horizontal_space(),
                text(format!("Read {}", fmt_rate(s.disk_read()))).size(13).style(style::READ),
                text(format!("Write {}", fmt_rate(s.disk_write()))).size(13).style(style::WRITE),
            ]
            .spacing(14)
            .align_items(Alignment::Center),
            chart(
                big_graph(
                    vec![series("Read", &h.read, style::READ), series("Write", &h.write, style::WRITE)],
                    None,
                    1024.0 * 1024.0,
                    rate_label
                ),
                150.0
            ),
            Column::with_children(s.disk_io.iter().map(|d| {
                kv(
                    &d.name,
                    format!("{} read · {} written since boot", fmt_bytes(d.read_total), fmt_bytes(d.write_total)),
                    theme,
                )
            }))
            .spacing(4),
        ]
        .spacing(10),
    );

    let mounts = card(
        "File systems",
        theme,
        if s.mounts.is_empty() {
            Column::new().push(muted("No disk file systems found.", theme))
        } else {
            Column::with_children(s.mounts.iter().map(|m| {
                let p = m.used_pct();
                column![
                    row![
                        text(&m.mount).size(14).font(BOLD),
                        muted(format!("{} · {}", truncate(&m.device, 40), m.fs), theme),
                        horizontal_space(),
                        text(pct_label(p)).size(13).style(style::level(p))
                    ]
                    .spacing(10)
                    .align_items(Alignment::Center),
                    meter(p, style::level(p)),
                    muted(
                        format!("{} used of {} · {} free", fmt_bytes(m.used), fmt_bytes(m.total), fmt_bytes(m.avail)),
                        theme
                    ),
                ]
                .spacing(6)
                .into()
            }))
            .spacing(14)
        },
    );

    let network = card(
        "Network activity",
        theme,
        column![
            row![
                legend(&[("Download", style::RX), ("Upload", style::TX)]),
                horizontal_space(),
                text(format!("↓ {}", fmt_rate(s.net_rx()))).size(13).style(style::RX),
                text(format!("↑ {}", fmt_rate(s.net_tx()))).size(13).style(style::TX),
            ]
            .spacing(14)
            .align_items(Alignment::Center),
            chart(
                big_graph(
                    vec![series("Down", &h.rx, style::RX), series("Up", &h.tx, style::TX)],
                    None,
                    64.0 * 1024.0,
                    rate_label
                ),
                150.0
            ),
            muted("Totals count network cards only, so VPN traffic is not counted twice.", theme),
        ]
        .spacing(10),
    );

    let shown: Vec<&stats::Net> = s.nets.iter().filter(|n| app.prefs.show_virtual || !n.virtual_).collect();
    let hidden = s.nets.len() - shown.len();
    let mut list = Column::new().spacing(16);
    if shown.is_empty() {
        list = list.push(muted("No network cards found.", theme));
    }
    for n in shown {
        let state = if n.up { ("Connected", style::OK) } else { ("Disconnected", style::muted(theme)) };
        let mut c = column![
            row![
                text(&n.name).size(14).font(BOLD),
                muted(net_kind(n), theme),
                horizontal_space(),
                text(state.0).size(13).style(state.1)
            ]
            .spacing(10)
            .align_items(Alignment::Center),
            kv("IPv4 address", if n.addrs.is_empty() { "—".into() } else { n.addrs.join(", ") }, theme),
        ]
        .spacing(5);
        if !n.addrs6.is_empty() {
            c = c.push(kv("IPv6 address", n.addrs6.iter().take(2).cloned().collect::<Vec<_>>().join(", "), theme));
        }
        if let (Some(dbm), Some(q)) = (n.signal_dbm, n.link_quality) {
            c = c.push(kv("Signal", format!("{dbm} dBm · {q:.0}% link quality"), theme));
        }
        list = list.push(
            c.push(kv("Now", format!("↓ {}   ↑ {}", fmt_rate(n.rx_bps), fmt_rate(n.tx_bps)), theme)).push(kv(
                "Since boot",
                format!("↓ {}   ↑ {}", fmt_bytes(n.rx_total), fmt_bytes(n.tx_total)),
                theme,
            )),
        );
    }
    let label = if hidden > 0 && !app.prefs.show_virtual {
        format!("Show virtual interfaces ({hidden} hidden: VPN, Docker, bridges)")
    } else {
        "Show virtual interfaces (VPN, Docker, bridges)".into()
    };
    let ifaces = card(
        "Network interfaces",
        theme,
        column![checkbox(label, app.prefs.show_virtual).on_toggle(Message::ShowVirtual).text_size(13).size(16), list]
            .spacing(14),
    );

    column![row![disks, network].spacing(12), row![mounts, ifaces].spacing(12)].spacing(12).into()
}

// ---- processes ----

fn processes_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let q = app.query.to_lowercase();
    let mut procs: Vec<&stats::Proc> = s
        .procs
        .iter()
        .filter(|p| app.prefs.show_kernel || !p.kernel)
        .filter(|p| !app.mine_only || p.uid == app.my_uid)
        .filter(|p| {
            q.is_empty()
                || p.name.to_lowercase().contains(&q)
                || p.cmd.to_lowercase().contains(&q)
                || p.user.to_lowercase().contains(&q)
                || p.pid.to_string() == q
        })
        .collect();
    procs.sort_by(|a, b| {
        let o = match app.sort {
            SortBy::Pid => a.pid.cmp(&b.pid),
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::User => a.user.cmp(&b.user),
            SortBy::Cpu => a.cpu.total_cmp(&b.cpu).then(a.mem.cmp(&b.mem)),
            SortBy::Mem => a.mem.cmp(&b.mem),
            SortBy::Threads => a.threads.cmp(&b.threads),
        };
        if app.desc {
            o.reverse()
        } else {
            o
        }
    });
    let shown = procs.len().min(MAX_ROWS);

    let toolbar = row![
        text_input("Search by name, command, user or PID", &app.query)
            .id(search_id())
            .on_input(Message::Query)
            .padding([6, 10])
            .size(14)
            .width(340),
        checkbox("Only my processes", app.mine_only).on_toggle(Message::MineOnly).text_size(14).size(16),
        checkbox("Show kernel threads", app.prefs.show_kernel).on_toggle(Message::ShowKernel).text_size(14).size(16),
        horizontal_space(),
        muted(
            format!(
                "{} processes · {} threads · {} running{}",
                group(s.tasks),
                group(s.threads),
                s.running,
                if procs.len() != s.tasks { format!(" · {} shown", group(procs.len())) } else { String::new() }
            ),
            theme
        ),
    ]
    .spacing(16)
    .align_items(Alignment::Center);

    // Column widths: fixed for numbers, the rest shared by name and command.
    let fixed = 70 + 140 + 80 + 80 + 100 + 70 + 40;
    let flexible = (app.width as i32 - 32 - 28 - fixed).max(200) as f32;
    let name_chars = (flexible * 3.0 / 8.0 / 7.4) as usize;
    let cmd_chars = (flexible * 5.0 / 8.0 / 7.0) as usize;

    let head = |label: &str, by: SortBy, width: Length, right: bool| -> Element<'a, Message> {
        let arrow = if app.sort == by {
            if app.desc {
                "▼"
            } else {
                "▲"
            }
        } else {
            ""
        };
        let mut r = row![].spacing(2).width(Length::Fill).align_items(Alignment::Center);
        if right {
            r = r.push(horizontal_space());
        }
        button(r.push(text(label.to_string()).size(13).font(BOLD)).push(text(arrow).size(12)))
            .padding([4, 6])
            .width(width)
            .style(style::flat(app.sort == by))
            .on_press(Message::Sort(by))
            .into()
    };
    let plain_head = |label: &str, width: Length| -> Element<'a, Message> {
        container(text(label.to_string()).size(13).font(BOLD).style(style::muted(theme)))
            .padding([4, 6])
            .width(width)
            .into()
    };
    let header = row![
        head("PID", SortBy::Pid, Length::Fixed(70.0), true),
        head("Name", SortBy::Name, Length::FillPortion(3), false),
        head("User", SortBy::User, Length::Fixed(140.0), false),
        plain_head("State", Length::Fixed(80.0)),
        head("CPU", SortBy::Cpu, Length::Fixed(80.0), true),
        head("Memory", SortBy::Mem, Length::Fixed(100.0), true),
        head("Threads", SortBy::Threads, Length::Fixed(70.0), true),
        plain_head("Command", Length::FillPortion(5)),
    ]
    .padding([0, 14, 0, 0])
    .align_items(Alignment::Center);

    let cell = |t: Text<'static>, w: Length| -> Element<'static, Message> {
        container(t.size(13)).width(w).padding([0, 6]).height(24).clip(true).center_y().into()
    };
    let right = |s: String| text(s).horizontal_alignment(Horizontal::Right).width(Length::Fill);
    let rows = Column::with_children(procs.iter().take(MAX_ROWS).enumerate().map(|(i, p)| {
        let cpu_colour = if p.cpu >= 25.0 { style::level(p.cpu * 2.0) } else { theme.palette().text };
        let line = container(
            row![
                cell(right(p.pid.to_string()).style(style::muted(theme)), Length::Fixed(70.0)),
                cell(text(truncate(&p.name, name_chars)), Length::FillPortion(3)),
                cell(text(truncate(&p.user, 18)).style(style::muted(theme)), Length::Fixed(140.0)),
                cell(text(state_name(p.state)).style(style::muted(theme)), Length::Fixed(80.0)),
                cell(right(format!("{:.1}%", p.cpu)).style(cpu_colour), Length::Fixed(80.0)),
                cell(right(fmt_bytes(p.mem)), Length::Fixed(100.0)),
                cell(right(p.threads.to_string()), Length::Fixed(70.0)),
                cell(text(truncate(&p.cmd, cmd_chars)).style(style::muted(theme)), Length::FillPortion(5)),
            ]
            .align_items(Alignment::Center),
        )
        .style(style::row_bg(app.selected == Some(p.pid), i % 2 == 1));
        mouse_area(line).on_press(Message::Select(p.pid)).into()
    }));
    let mut list = column![rows];
    if procs.len() > shown {
        list = list.push(
            container(muted(
                format!("Showing the first {shown} of {}. Search to narrow the list.", group(procs.len())),
                theme,
            ))
            .padding(8),
        );
    }
    if procs.is_empty() {
        list = list.push(container(muted("No processes match.", theme)).padding(12));
    }

    let mut body = column![
        toolbar,
        muted("CPU is a share of the whole processor (all cores = 100%). Memory is resident memory (RSS). Click a row for details. View only.", theme),
        header,
        scrollable(list.padding([0, 14, 0, 0])).height(Length::Fill),
    ]
    .spacing(10);
    if let Some(pid) = app.selected {
        body = body.push(details(app, pid, theme));
    }
    container(body).padding(14).height(Length::Fill).style(style::card()).into()
}

fn details<'a>(app: &'a App, pid: u32, theme: &Theme) -> Element<'a, Message> {
    let s = &app.snap;
    let close = button(text("Close").size(13))
        .padding([4, 12])
        .style(iced::theme::Button::Secondary)
        .on_press(Message::CloseDetails);
    let Some(p) = s.procs.iter().find(|p| p.pid == pid) else {
        return container(
            row![muted(format!("Process {pid} has exited."), theme), horizontal_space(), close]
                .align_items(Alignment::Center),
        )
        .padding(12)
        .style(style::row_bg(true, false))
        .into();
    };
    let parent = s
        .procs
        .iter()
        .find(|q| q.pid == p.ppid)
        .map(|q| format!("{} ({})", q.name, q.pid))
        .unwrap_or_else(|| p.ppid.to_string());
    let running_for = s.uptime.saturating_sub(p.started);
    let priority = match p.nice {
        0 => "Normal (0)".to_string(),
        n if n < 0 => format!("Higher ({n})"),
        n => format!("Lower ({n})"),
    };
    let left = column![
        kv("User", &p.user, theme),
        kv("State", state_name(p.state), theme),
        kv("Parent process", parent, theme),
        kv("Running for", fmt_duration(running_for), theme),
    ]
    .spacing(6)
    .width(Length::Fill);
    let mid = column![
        kv("CPU", format!("{:.1}%", p.cpu), theme),
        kv("Memory (resident)", fmt_bytes(p.mem), theme),
        kv("Virtual memory", fmt_bytes(p.virt), theme),
        kv("Threads · priority", format!("{} · {priority}", p.threads), theme),
    ]
    .spacing(6)
    .width(Length::Fill);
    container(
        column![
            row![text(&p.name).size(15).font(BOLD), muted(format!("PID {}", p.pid), theme), horizontal_space(), close]
                .spacing(10)
                .align_items(Alignment::Center),
            row![left, mid].spacing(32),
            column![muted("Command line", theme), text(truncate(&p.cmd, 600)).size(13)].spacing(2),
        ]
        .spacing(10),
    )
    .padding(12)
    .style(style::row_bg(true, false))
    .into()
}

// ---- system report ----

fn system_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let copied = app.copied.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
    let top = container(
        row![
            column![
                text(if app.source == Source::Local { "About this computer" } else { "About this server" })
                    .size(16)
                    .font(BOLD),
                muted(
                    match &app.agent_version {
                        Some(v) if app.source != Source::Local => format!(
                            "Read through SSH from sysmon {v} on the server. Copy this report into a help request; the machine's name and network hardware addresses are left out."
                        ),
                        _ => "Copy this report into a help forum or support request. Your computer name and network hardware addresses are left out.".into(),
                    },
                    theme
                ),
            ]
            .spacing(4)
            .width(Length::Fill),
            button(text(if copied { "Copied" } else { "Copy report" }).size(14))
                .padding([7, 16])
                .style(iced::theme::Button::Primary)
                .on_press(Message::CopyReport),
        ]
        .spacing(16)
        .align_items(Alignment::Center),
    )
    .padding(14)
    .width(Length::Fill)
    .style(style::card());

    let sections = app.report.as_ref().map(|r| r.1.as_slice()).unwrap_or_default();
    let mut left = Column::new().spacing(12).width(Length::Fill);
    let mut right = Column::new().spacing(12).width(Length::Fill);
    let (mut lh, mut rh) = (0, 0);
    // Fill whichever column is shorter so the two stay balanced.
    for (title, rows) in sections {
        let c = card(title, theme, Column::with_children(rows.iter().map(|(k, v)| kv(k, v, theme))).spacing(6));
        if lh <= rh {
            left = left.push(c);
            lh += rows.len() + 2;
        } else {
            right = right.push(c);
            rh += rows.len() + 2;
        }
    }
    column![top, row![left, right].spacing(12)].spacing(12).into()
}

// ---- banner ----

fn banner<'a>(b: &crate::Banner, theme: &Theme) -> Element<'a, Message> {
    let mut r = row![text(&b.text).size(14).width(Length::Fill)].spacing(10).align_items(Alignment::Center);
    if let Some(dir) = &b.folder {
        r = r.push(
            button(text("Open folder").size(13))
                .padding([4, 12])
                .style(iced::theme::Button::Secondary)
                .on_press(Message::OpenFolder(dir.clone())),
        );
    }
    r = r.push(
        button(text("Dismiss").size(13)).padding([4, 12]).style(style::flat(true)).on_press(Message::DismissBanner),
    );
    container(r).padding([8, 16]).width(Length::Fill).style(style::banner(b.urgent, theme)).into()
}

// ---- settings ----

fn settings_tab<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let p = &app.prefs;
    let setting = |label: &str, hint: &str, control: Element<'a, Message>| -> Element<'a, Message> {
        row![column![text(label.to_string()).size(14), muted(hint, theme)].spacing(2).width(Length::Fill), control]
            .spacing(16)
            .align_items(Alignment::Center)
            .into()
    };
    let appearance = card(
        "Appearance and updates",
        theme,
        column![
            setting(
                "Theme",
                "System theme follows your desktop's light or dark mode.",
                pick_list(&ThemeChoice::ALL[..], Some(p.theme), Message::Theme).text_size(14).width(170).into()
            ),
            setting(
                "Temperature unit",
                "Used everywhere temperatures appear.",
                pick_list(&[TempUnit::Celsius, TempUnit::Fahrenheit][..], Some(p.unit), Message::Unit)
                    .text_size(14)
                    .width(170)
                    .into()
            ),
            setting(
                "Refresh",
                "How often readings are taken. Slower uses a little less power.",
                pick_list(&INTERVALS[..], Some(p.interval), Message::Interval).text_size(14).width(170).into()
            ),
        ]
        .spacing(14),
    );

    let rows = Column::with_children(alerts::Kind::ALL.into_iter().map(|kind| {
        let limit = p.alerts.get(kind);
        let choices: Vec<Choice> = kind.choices().iter().map(|v| Choice { kind, value: *v }).collect();
        row![
            checkbox(kind.label(), limit.on)
                .on_toggle(move |on| Message::AlertOn(kind, on))
                .text_size(14)
                .size(16)
                .width(Length::Fill),
            pick_list(choices, Some(Choice { kind, value: limit.value }), Message::AlertValue).text_size(14).width(120),
        ]
        .spacing(16)
        .align_items(Alignment::Center)
        .into()
    }))
    .spacing(10);
    let alerts_card = card(
        "Alerts",
        theme,
        column![
            muted(
                "While sysmon is open, it shows a desktop notification when one of these happens. Temperature and memory must stay high for 15 seconds first, and each alert repeats at most every 10 minutes.",
                theme
            ),
            rows,
            row![
                button(text("Send a test alert").size(13)).padding([5, 12]).style(iced::theme::Button::Secondary).on_press(Message::TestAlert),
            ],
        ]
        .spacing(12),
    );

    let export = card(
        "History",
        theme,
        column![
            muted(
                format!(
                    "Save the last {} readings (CPU, memory, temperature, network, disk, graphics and each core) as a CSV file you can open in a spreadsheet. It goes to {}.",
                    app.hist.times.len().max(1),
                    tilde(&crate::history::download_dir())
                ),
                theme
            ),
            row![button(text("Export history to CSV").size(13)).padding([5, 12]).style(iced::theme::Button::Primary).on_press(Message::ExportCsv)],
        ]
        .spacing(12),
    );

    let config = crate::Prefs::path().map(|p| tilde(&p)).unwrap_or_else(|| "—".into());
    let about = card(
        "About",
        theme,
        column![
            kv("Version", env!("CARGO_PKG_VERSION"), theme),
            kv("Licence", "Apache-2.0", theme),
            kv("Source", env!("CARGO_PKG_REPOSITORY"), theme),
            kv("Settings file", config, theme),
            muted("sysmon reads everything locally from /proc and /sys and needs no root access. It sends nothing over the network; servers are read through your own SSH connection.", theme),
        ]
        .spacing(8),
    );
    row![
        column![appearance, alerts_card].spacing(12).width(Length::Fill),
        column![servers_card(app, theme), export, about].spacing(12).width(Length::Fill)
    ]
    .spacing(12)
    .into()
}

/// A path with the home folder written as ~, as people expect to read it.
pub fn tilde(p: &std::path::Path) -> String {
    let shown = p.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => format!("~{}", &shown[home.len()..]),
        _ => shown,
    }
}

// ---- remote servers ----

/// Shown under the tabs while a server is connecting, installing or failed.
fn connection_strip<'a>(app: &'a App, theme: &Theme) -> Option<Element<'a, Message>> {
    let server = app.server()?;
    let back = button(text("Back to this computer").size(13))
        .padding([4, 12])
        .style(iced::theme::Button::Secondary)
        .on_press(Message::PickSource(SourceChoice { source: Some(Source::Local), label: String::new() }));
    let action = |label: &str, msg: Message| {
        button(text(label.to_string()).size(13)).padding([4, 12]).style(iced::theme::Button::Primary).on_press(msg)
    };
    let (message, urgent, buttons): (String, bool, Vec<Element<'a, Message>>) = match &app.link {
        Link::Live => return None,
        Link::Connecting => (format!("Connecting to {} ({})…", server.name, server.target), false, vec![back.into()]),
        Link::Installing => {
            (format!("Installing sysmon on {}… This downloads about 5 MB on the server.", server.name), false, vec![])
        }
        Link::Failed(Failure::NotInstalled) => (
            format!(
                "sysmon is not installed on {}. It can be installed in your home folder there (no root needed).",
                server.name
            ),
            true,
            vec![action("Install on server", Message::InstallAgent).into(), back.into()],
        ),
        Link::Failed(Failure::Outdated(v)) => (
            format!(
                "{} has sysmon {v}, which cannot talk to this version ({}).",
                server.name,
                env!("CARGO_PKG_VERSION")
            ),
            true,
            vec![action("Update sysmon on server", Message::InstallAgent).into(), back.into()],
        ),
        Link::Failed(Failure::Other(e)) => {
            (format!("{}: {e}", server.name), true, vec![action("Try again", Message::Retry).into(), back.into()])
        }
    };
    let mut r = row![text(message).size(14).width(Length::Fill)].spacing(10).align_items(Alignment::Center);
    for b in buttons {
        r = r.push(b);
    }
    Some(container(r).padding([8, 16]).width(Length::Fill).style(style::banner(urgent, theme)).into())
}

fn servers_card<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let f = &app.form;
    let list: Element<'a, Message> = if app.prefs.servers.is_empty() {
        muted("No servers yet.", theme).into()
    } else {
        Column::with_children(app.prefs.servers.iter().enumerate().map(|(i, s)| {
            let detail = [
                Some(s.target.clone()),
                s.port.map(|p| format!("port {p}")),
                s.identity.as_ref().map(|k| format!("key {k}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            let watching = app.source == Source::Remote(i);
            row![
                column![text(&s.name).size(14).font(BOLD), muted(detail, theme)].spacing(2).width(Length::Fill),
                button(text(if watching { "Watching" } else { "Connect" }).size(13))
                    .padding([4, 12])
                    .style(if watching { iced::theme::Button::Secondary } else { iced::theme::Button::Primary })
                    .on_press_maybe((!watching).then_some(Message::ConnectServer(i))),
                button(text("Remove").size(13))
                    .padding([4, 12])
                    .style(style::flat(true))
                    .on_press(Message::RemoveServer(i)),
            ]
            .spacing(8)
            .align_items(Alignment::Center)
            .into()
        }))
        .spacing(10)
        .into()
    };
    let field = |label: &str, placeholder: &str, value: &str, msg: fn(String) -> Message| -> Element<'a, Message> {
        column![
            text(label.to_string()).size(13),
            text_input(placeholder, value).on_input(msg).on_submit(Message::AddServer).padding([5, 8]).size(14)
        ]
        .spacing(4)
        .into()
    };
    let mut form = column![
        row![
            container(field("Address", "user@203.0.113.10 or an ~/.ssh/config name", &f.target, Message::FormTarget))
                .width(Length::FillPortion(3)),
            container(field("Name (optional)", "web server", &f.name, Message::FormName)).width(Length::FillPortion(2)),
        ]
        .spacing(10),
        row![
            container(field("Key file (optional)", "~/.ssh/my-aws-key.pem", &f.key, Message::FormKey))
                .width(Length::FillPortion(3)),
            container(field("Port (optional)", "22", &f.port, Message::FormPort)).width(Length::FillPortion(2)),
        ]
        .spacing(10),
    ]
    .spacing(10);
    if let Some(e) = &f.error {
        form = form.push(text(e).size(13).style(style::HOT));
    }
    form = form.push(row![button(text("Add and connect").size(13))
        .padding([5, 12])
        .style(iced::theme::Button::Primary)
        .on_press(Message::AddServer)]);
    card(
        "Servers",
        theme,
        column![
            muted(
                "Watch a cloud or home server (AWS, Azure, DigitalOcean, a Raspberry Pi…) over SSH. sysmon uses this computer's ssh with your keys and ~/.ssh/config, so the server needs no open ports. Password logins are not supported. If sysmon is missing on the server, it can install itself there in your home folder.",
                theme
            ),
            list,
            form,
        ]
        .spacing(14),
    )
}

fn waiting_panel<'a>(app: &'a App, theme: &Theme) -> Element<'a, Message> {
    let name = app.server().map(|s| s.name.as_str()).unwrap_or("the server");
    let (title, detail) = match &app.link {
        Link::Connecting | Link::Live => (
            format!("Waiting for the first reading from {name}…"),
            "Connecting over SSH and starting sysmon there. This usually takes a few seconds.".to_string(),
        ),
        Link::Installing => (
            format!("Installing sysmon on {name}…"),
            "It goes into ~/.local/bin on the server; no root access is used.".into(),
        ),
        Link::Failed(Failure::NotInstalled) => (
            format!("sysmon is not installed on {name}"),
            "Use \u{201c}Install on server\u{201d} above, or install it there yourself from the releases page.".into(),
        ),
        Link::Failed(Failure::Outdated(_)) => {
            (format!("sysmon on {name} needs updating"), "Use \u{201c}Update sysmon on server\u{201d} above.".into())
        }
        Link::Failed(Failure::Other(_)) => (
            format!("No readings from {name}"),
            "The message above says what went wrong. You can check the address and key in Settings, then try again."
                .into(),
        ),
    };
    container(column![text(title).size(18).font(BOLD), muted(detail, theme)].spacing(8).align_items(Alignment::Center))
        .width(Length::Fill)
        .padding(60)
        .center_x()
        .style(style::card())
        .into()
}
