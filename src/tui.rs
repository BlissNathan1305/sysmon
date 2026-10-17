//! Terminal mode (`sysmon --tui`): the same readings as the window, drawn
//! as text, for SSH sessions and machines without a desktop.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, LineGauge, Paragraph, Row, Sparkline, Table, TableState, Tabs, Wrap,
};
use ratatui::{DefaultTerminal, Frame};

use crate::history::History;
use crate::report::{self, Section};
use crate::stats::{self, fmt_bytes, fmt_duration, fmt_rate, fmt_temp, Collector, Snapshot, StaticInfo};
use crate::view::{avg_freq, ghz, group, net_kind, sensor_group, state_name, truncate};
use crate::Prefs;

const TABS: [&str; 5] = ["Overview", "Processes", "Disks & Network", "Sensors & Power", "System"];
const INTERVALS: [u64; 4] = [500, 1000, 2000, 5000];

const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const HOT: Color = Color::Red;
const CPU: Color = Color::Cyan;
const MEM: Color = Color::Magenta;
const RX: Color = Color::Green;
const TX: Color = Color::Yellow;
const DIM: Color = Color::DarkGray;

fn level(pct: f32) -> Color {
    if pct >= 85.0 {
        HOT
    } else if pct >= 60.0 {
        WARN
    } else {
        OK
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Cpu,
    Mem,
    Pid,
    Name,
    User,
    Threads,
}

impl Sort {
    const ORDER: [Sort; 6] = [Sort::Cpu, Sort::Mem, Sort::Pid, Sort::Name, Sort::User, Sort::Threads];

    fn label(self) -> &'static str {
        match self {
            Sort::Cpu => "CPU",
            Sort::Mem => "memory",
            Sort::Pid => "PID",
            Sort::Name => "name",
            Sort::User => "user",
            Sort::Threads => "threads",
        }
    }
}

struct Tui {
    info: StaticInfo,
    collector: Collector,
    snap: Snapshot,
    hist: History,
    report: Vec<Section>,
    report_at: Option<Instant>,
    tab: usize,
    interval: u64,
    paused: bool,
    sort: Sort,
    desc: bool,
    query: String,
    searching: bool,
    show_kernel: bool,
    mine_only: bool,
    uid: u32,
    table: TableState,
    details: bool,
    help: bool,
    scroll: u16,
}

/// Runs terminal mode until the user quits. Returns the process exit code.
pub fn run(prefs: &Prefs, interval: Option<u64>) -> i32 {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!("sysmon --tui needs a terminal. To print readings for other programs, use: sysmon --agent");
        return 2;
    }
    let info = StaticInfo::load();
    let mut collector = Collector::new(info.cpu_threads);
    let snap = collector.sample();
    let mut app = Tui {
        info,
        collector,
        snap,
        hist: History::default(),
        report: Vec::new(),
        report_at: None,
        tab: 0,
        interval: interval.unwrap_or(prefs.interval.0).clamp(250, 60_000),
        paused: false,
        sort: Sort::Cpu,
        desc: true,
        query: String::new(),
        searching: false,
        show_kernel: prefs.show_kernel,
        mine_only: false,
        // SAFETY: getuid cannot fail.
        uid: unsafe { libc::getuid() },
        table: TableState::default().with_selected(Some(0)),
        details: false,
        help: false,
        scroll: 0,
    };
    // ratatui::init switches to the alternate screen and restores the
    // terminal on panic.
    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sysmon: {e}");
            1
        }
    }
}

impl Tui {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        // A short first wait so the rates fill in quickly.
        let mut next = Instant::now() + Duration::from_millis(400);
        loop {
            if Instant::now() >= next {
                if !self.paused {
                    self.snap = self.collector.sample();
                    self.hist.record(&self.snap);
                    if self.tab == 4 && self.report_at.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
                        self.report = report::gather(&self.info, &self.snap);
                        self.report_at = Some(Instant::now());
                    }
                }
                next = Instant::now() + Duration::from_millis(self.interval);
            }
            terminal.draw(|f| self.draw(f))?;
            let wait = next.saturating_duration_since(Instant::now());
            if event::poll(wait)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && !self.on_key(key) {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Handles a key; returns false to quit.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return false;
        }
        if self.searching {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(c) => self.query.push(c),
                _ => {}
            }
            self.table.select(Some(0));
            return true;
        }
        if self.help {
            self.help = false;
            return true;
        }
        if self.details && matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            self.details = false;
            return true;
        }
        match key.code {
            KeyCode::Char('q') => return false,
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                } else {
                    return false;
                }
            }
            KeyCode::Char('?') | KeyCode::Char('h') => self.help = true,
            KeyCode::Char(c @ '1'..='5') => self.set_tab(c as usize - '1' as usize),
            KeyCode::Tab | KeyCode::Right => self.set_tab((self.tab + 1) % TABS.len()),
            KeyCode::BackTab | KeyCode::Left => self.set_tab((self.tab + TABS.len() - 1) % TABS.len()),
            KeyCode::Char('p') | KeyCode::Char(' ') => self.paused = !self.paused,
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.interval = INTERVALS.iter().copied().find(|i| *i > self.interval).unwrap_or(self.interval)
            }
            KeyCode::Char('-') => {
                self.interval = INTERVALS.iter().rev().copied().find(|i| *i < self.interval).unwrap_or(self.interval)
            }
            KeyCode::Char('/') => {
                self.set_tab(1);
                self.searching = true;
            }
            KeyCode::Char('s') => {
                let i = Sort::ORDER.iter().position(|s| *s == self.sort).unwrap_or(0);
                self.sort = Sort::ORDER[(i + 1) % Sort::ORDER.len()];
                self.desc = matches!(self.sort, Sort::Cpu | Sort::Mem | Sort::Threads);
            }
            KeyCode::Char('r') => self.desc = !self.desc,
            KeyCode::Char('k') => self.show_kernel = !self.show_kernel,
            KeyCode::Char('o') => self.mine_only = !self.mine_only,
            KeyCode::Enter if self.tab == 1 => self.details = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(15),
            KeyCode::PageUp => self.move_by(-15),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(usize::MAX),
            _ => {}
        }
        true
    }

    fn set_tab(&mut self, t: usize) {
        self.tab = t;
        self.scroll = 0;
        self.details = false;
        if t == 4 {
            self.report = report::gather(&self.info, &self.snap);
            self.report_at = Some(Instant::now());
        }
    }

    fn move_by(&mut self, d: i64) {
        if self.tab == 1 {
            let cur = self.table.selected().unwrap_or(0) as i64;
            self.move_to((cur + d).max(0) as usize);
        } else {
            self.scroll = (self.scroll as i64 + d).clamp(0, 500) as u16;
        }
    }

    fn move_to(&mut self, i: usize) {
        let n = self.procs().len();
        self.table.select(Some(i.min(n.saturating_sub(1))));
    }

    fn procs(&self) -> Vec<&stats::Proc> {
        let q = self.query.to_lowercase();
        let mut v: Vec<&stats::Proc> = self
            .snap
            .procs
            .iter()
            .filter(|p| self.show_kernel || !p.kernel)
            .filter(|p| !self.mine_only || p.uid == self.uid)
            .filter(|p| {
                q.is_empty()
                    || p.name.to_lowercase().contains(&q)
                    || p.cmd.to_lowercase().contains(&q)
                    || p.user.to_lowercase().contains(&q)
                    || p.pid.to_string() == q
            })
            .collect();
        v.sort_by(|a, b| {
            let o = match self.sort {
                Sort::Cpu => a.cpu.total_cmp(&b.cpu).then(a.mem.cmp(&b.mem)),
                Sort::Mem => a.mem.cmp(&b.mem),
                Sort::Pid => a.pid.cmp(&b.pid),
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Sort::User => a.user.cmp(&b.user),
                Sort::Threads => a.threads.cmp(&b.threads),
            };
            if self.desc {
                o.reverse()
            } else {
                o
            }
        });
        v
    }

    // ---- drawing ----

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < 60 || area.height < 16 {
            f.render_widget(
                Paragraph::new("The terminal is too small for sysmon.\nMake it at least 60×16, or press q to quit.")
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let [top, tabs, body, foot] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)])
                .areas(area);
        self.draw_header(f, top);
        let titles: Vec<Line> = TABS.iter().enumerate().map(|(i, t)| Line::from(format!("{} {t}", i + 1))).collect();
        f.render_widget(
            Tabs::new(titles)
                .select(self.tab)
                .highlight_style(Style::new().fg(Color::Black).bg(CPU).bold())
                .divider(" "),
            tabs,
        );
        match self.tab {
            0 => self.draw_overview(f, body),
            1 => self.draw_processes(f, body),
            2 => self.draw_storage(f, body),
            3 => self.draw_sensors(f, body),
            _ => self.draw_system(f, body),
        }
        self.draw_footer(f, foot);
        if self.help {
            draw_help(f, area);
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let s = &self.snap;
        let mut spans = vec![
            Span::styled(" sysmon ", Style::new().fg(Color::Black).bg(CPU).bold()),
            Span::raw(format!(
                " {} · {} · up {} · load {:.2} {:.2} {:.2}",
                self.info.hostname,
                self.info.os,
                fmt_duration(s.uptime),
                s.load[0],
                s.load[1],
                s.load[2]
            )),
        ];
        let right = if self.paused {
            Span::styled(" PAUSED ", Style::new().fg(Color::Black).bg(WARN).bold())
        } else {
            Span::styled(format!(" every {:.1} s ", self.interval as f32 / 1000.0), Style::new().fg(DIM))
        };
        let used: usize = spans.iter().map(|s| s.width()).sum::<usize>() + right.width();
        spans.push(Span::raw(" ".repeat((area.width as usize).saturating_sub(used))));
        spans.push(right);
        f.render_widget(Line::from(spans), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let keys = if self.searching {
            format!(" Search: {}▏  (Enter to finish, Esc to stop)", self.query)
        } else {
            match self.tab {
                1 => format!(
                    " ↑↓ select  Enter details  / search{}  s sort ({})  r reverse  k kernel threads {}  o only mine {}  ? help  q quit",
                    if self.query.is_empty() { String::new() } else { format!(" \"{}\"", self.query) },
                    self.sort.label(),
                    if self.show_kernel { "on" } else { "off" },
                    if self.mine_only { "on" } else { "off" },
                ),
                _ => " 1-5 or Tab switch tabs  p pause  +/- refresh rate  / search processes  ? help  q quit".into(),
            }
        };
        f.render_widget(Paragraph::new(keys).style(Style::new().fg(Color::Black).bg(Color::Gray)), area);
    }

    fn draw_overview(&self, f: &mut Frame, area: Rect) {
        let s = &self.snap;
        let cores = s.cpu_cores.len().max(1);
        // Cores in up to four columns; enough rows for all of them.
        let cols = if cores <= 8 {
            1
        } else if cores <= 24 {
            2
        } else {
            4
        };
        let core_rows = cores.div_ceil(cols) as u16;
        let [cpu_area, mid, low, procs] = Layout::vertical([
            Constraint::Length((core_rows + 2).clamp(6, 14)),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Min(4),
        ])
        .areas(area);

        // CPU: total with history on the left, cores on the right.
        let [cpu_l, cpu_r] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(cpu_area);
        let title = format!(" CPU {:.0}% · {} ", s.cpu_total, avg_freq(s).map(ghz).unwrap_or_default());
        let block = boxed(&title, level(s.cpu_total));
        let inner = block.inner(cpu_l);
        f.render_widget(block, cpu_l);
        let [model, spark] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        f.render_widget(Paragraph::new(truncate(&self.info.cpu_model, model.width as usize)).fg(DIM), model);
        f.render_widget(
            Sparkline::default().data(tail(&self.hist.cpu, spark.width)).max(100).style(Style::new().fg(CPU)),
            spark,
        );

        let block = boxed(" Cores ", Color::Reset);
        let inner = block.inner(cpu_r);
        f.render_widget(block, cpu_r);
        let col_areas = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).spacing(1).split(inner);
        for (i, v) in s.cpu_cores.iter().enumerate() {
            let (c, r) = (i / core_rows as usize, i % core_rows as usize);
            let Some(col) = col_areas.get(c) else { continue };
            if r as u16 >= col.height {
                continue;
            }
            let line = Rect { y: col.y + r as u16, height: 1, ..*col };
            f.render_widget(gauge(&format!("{i:>2}"), *v / 100.0, &format!("{v:>3.0}%"), level(*v)), line);
        }

        // Memory, swap, graphics, temperature and battery.
        let [mem_a, misc_a] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(mid);
        let m = &s.mem;
        let block = boxed(format!(" Memory {} of {} ", fmt_bytes(m.used), fmt_bytes(m.total)), level(m.used_pct()));
        let inner = block.inner(mem_a);
        f.render_widget(block, mem_a);
        let rows = Layout::vertical([Constraint::Length(1); 5]).split(inner);
        f.render_widget(gauge("RAM ", m.used_pct() / 100.0, &format!("{:>3.0}%", m.used_pct()), MEM), rows[0]);
        if m.swap_total > 0 {
            f.render_widget(gauge("Swap", m.swap_pct() / 100.0, &format!("{:>3.0}%", m.swap_pct()), WARN), rows[1]);
        } else {
            f.render_widget(Paragraph::new("Swap  none").fg(DIM), rows[1]);
        }
        f.render_widget(
            Paragraph::new(format!("Available {}  ·  cache {}", fmt_bytes(m.available), fmt_bytes(m.cached))).fg(DIM),
            rows[2],
        );
        let spark_area = Rect { height: inner.height.saturating_sub(3), ..rows[3] };
        f.render_widget(
            Sparkline::default().data(tail(&self.hist.mem, spark_area.width)).max(100).style(Style::new().fg(MEM)),
            spark_area,
        );

        let mut lines: Vec<Line> = Vec::new();
        match s.cpu_temp() {
            Some(t) => lines.push(kv_line("Temperature", fmt_temp(t), stats_color_temp(t))),
            None => lines.push(kv_line("Temperature", "no sensors".into(), DIM)),
        }
        for g in s.gpus.iter().take(2) {
            let mut parts = vec![g.usage.map(|u| format!("{u:.0}% busy")).unwrap_or_else(|| "usage n/a".into())];
            if let (Some(u), Some(t)) = (g.vram_used, g.vram_total) {
                parts.push(format!("{} / {}", fmt_bytes(u), fmt_bytes(t)));
            }
            if let Some(t) = g.temp {
                parts.push(fmt_temp(t));
            }
            lines.push(kv_line(&truncate(&g.name, 26), parts.join(" · "), Color::Reset));
        }
        if let Some(b) = s.battery() {
            let left = match (b.status.as_str(), b.time_left) {
                ("Discharging", Some(t)) => format!(" · {} left", fmt_duration(t)),
                ("Charging", Some(t)) => format!(" · full in {}", fmt_duration(t)),
                _ => String::new(),
            };
            lines.push(kv_line("Battery", format!("{:.0}% {}{left}", b.percent, b.status), level(100.0 - b.percent)));
        }
        lines.push(kv_line(
            "Tasks",
            format!("{} processes · {} threads · {} running", group(s.tasks), group(s.threads), s.running),
            Color::Reset,
        ));
        f.render_widget(Paragraph::new(lines).block(boxed(" System ", Color::Reset)), misc_a);

        // Disks and network.
        let [disk_a, net_a] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(low);
        let block =
            boxed(format!(" Disks · R {} · W {} ", fmt_rate(s.disk_read()), fmt_rate(s.disk_write())), Color::Reset);
        let inner = block.inner(disk_a);
        f.render_widget(block, disk_a);
        for (i, mnt) in s.mounts.iter().take(inner.height as usize).enumerate() {
            let p = mnt.used_pct();
            let line = Rect { y: inner.y + i as u16, height: 1, ..inner };
            f.render_widget(
                gauge(
                    &truncate(&mnt.mount, 10),
                    p / 100.0,
                    &format!("{p:>3.0}% {} free", fmt_bytes(mnt.avail)),
                    level(p),
                ),
                line,
            );
        }
        let block = boxed(format!(" Network · ↓ {} · ↑ {} ", fmt_rate(s.net_rx()), fmt_rate(s.net_tx())), Color::Reset);
        let inner = block.inner(net_a);
        f.render_widget(block, net_a);
        let [down, up] = Layout::vertical([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).areas(inner);
        f.render_widget(Sparkline::default().data(tail(&self.hist.rx, down.width)).style(Style::new().fg(RX)), down);
        f.render_widget(Sparkline::default().data(tail(&self.hist.tx, up.width)).style(Style::new().fg(TX)), up);

        // The busiest processes.
        let mut top: Vec<&stats::Proc> = s.procs.iter().filter(|p| self.show_kernel || !p.kernel).collect();
        top.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
        let rows: Vec<Row> = top
            .iter()
            .take(procs.height.saturating_sub(3) as usize)
            .map(|p| {
                Row::new(vec![
                    Cell::from(p.pid.to_string()).fg(DIM),
                    Cell::from(truncate(&p.user, 12)).fg(DIM),
                    Cell::from(format!("{:>5.1}", p.cpu)).fg(if p.cpu >= 25.0 {
                        level(p.cpu * 2.0)
                    } else {
                        Color::Reset
                    }),
                    Cell::from(format!("{:>9}", fmt_bytes(p.mem))),
                    Cell::from(p.name.clone()).bold(),
                    Cell::from(p.cmd.clone()).fg(DIM),
                ])
            })
            .collect();
        f.render_widget(
            Table::new(rows, proc_widths())
                .header(Row::new(vec!["PID", "USER", " CPU%", "   MEMORY", "NAME", "COMMAND"]).bold())
                .block(boxed(" Busiest processes ", Color::Reset)),
            procs,
        );
    }

    fn draw_processes(&mut self, f: &mut Frame, area: Rect) {
        // Owned copies, so the selection can change while drawing.
        let list: Vec<stats::Proc> = self.procs().into_iter().cloned().collect();
        let count = list.len();
        let rows: Vec<Row> = list
            .iter()
            .map(|p| {
                Row::new(vec![
                    Cell::from(p.pid.to_string()).fg(DIM),
                    Cell::from(truncate(&p.user, 12)).fg(DIM),
                    Cell::from(format!("{:>5.1}", p.cpu)).fg(if p.cpu >= 25.0 {
                        level(p.cpu * 2.0)
                    } else {
                        Color::Reset
                    }),
                    Cell::from(format!("{:>9}", fmt_bytes(p.mem))),
                    Cell::from(format!("{:>4}", p.threads)),
                    Cell::from(p.state.to_string()).fg(DIM),
                    Cell::from(p.name.clone()).bold(),
                    Cell::from(p.cmd.clone()).fg(DIM),
                ])
            })
            .collect();
        let arrow = if self.desc { "▼" } else { "▲" };
        let head = |name: &str, s: Sort| if self.sort == s { format!("{name}{arrow}") } else { name.to_string() };
        let header = Row::new(vec![
            head("PID", Sort::Pid),
            head("USER", Sort::User),
            head(" CPU%", Sort::Cpu),
            head("   MEMORY", Sort::Mem),
            head("THR", Sort::Threads),
            "S".into(),
            head("NAME", Sort::Name),
            "COMMAND".into(),
        ])
        .bold();
        let title = format!(
            " Processes · {} shown of {} · sorted by {} · CPU is a share of the whole processor ",
            group(count),
            group(self.snap.tasks),
            self.sort.label()
        );
        let widths = [
            Constraint::Length(7),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(4),
            Constraint::Length(1),
            Constraint::Length(18),
            Constraint::Min(10),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(boxed(&title, Color::Reset))
            .row_highlight_style(Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        if self.table.selected().unwrap_or(0) >= count {
            self.table.select(Some(count.saturating_sub(1)));
        }
        f.render_stateful_widget(table, area, &mut self.table);

        if self.details {
            let Some(p) = list.get(self.table.selected().unwrap_or(0)) else { return };
            let parent = self
                .snap
                .procs
                .iter()
                .find(|q| q.pid == p.ppid)
                .map(|q| format!("{} ({})", q.name, q.pid))
                .unwrap_or_else(|| p.ppid.to_string());
            let lines = vec![
                kv_line("User", p.user.clone(), Color::Reset),
                kv_line("State", state_name(p.state).into(), Color::Reset),
                kv_line("Parent", parent, Color::Reset),
                kv_line("Running for", fmt_duration(self.snap.uptime.saturating_sub(p.started)), Color::Reset),
                kv_line("CPU", format!("{:.1}%", p.cpu), Color::Reset),
                kv_line(
                    "Memory",
                    format!("{} resident · {} virtual", fmt_bytes(p.mem), fmt_bytes(p.virt)),
                    Color::Reset,
                ),
                kv_line("Threads", p.threads.to_string(), Color::Reset),
                kv_line("Priority (nice)", p.nice.to_string(), Color::Reset),
                Line::raw(""),
                Line::styled("Command line", Style::new().fg(DIM)),
                Line::raw(p.cmd.clone()),
                Line::raw(""),
                Line::styled("Enter or Esc to close. sysmon is view-only.", Style::new().fg(DIM)),
            ];
            let popup = centered(area, 80, 17);
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(boxed(format!(" {} · PID {} ", p.name, p.pid), CPU)),
                popup,
            );
        }
    }

    fn draw_storage(&self, f: &mut Frame, area: Rect) {
        let s = &self.snap;
        let [top, bottom] = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
        let [fs_a, io_a] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(top);
        let block = boxed(" File systems ", Color::Reset);
        let inner = block.inner(fs_a);
        f.render_widget(block, fs_a);
        let mut y = inner.y;
        for m in &s.mounts {
            if y + 1 >= inner.y + inner.height {
                break;
            }
            let p = m.used_pct();
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(m.mount.clone(), Style::new().bold()),
                    Span::styled(format!("  {} · {}", truncate(&m.device, 36), m.fs), Style::new().fg(DIM)),
                ])),
                Rect { y, height: 1, ..inner },
            );
            f.render_widget(
                gauge(
                    "",
                    p / 100.0,
                    &format!(
                        "{p:>3.0}% · {} used of {} · {} free",
                        fmt_bytes(m.used),
                        fmt_bytes(m.total),
                        fmt_bytes(m.avail)
                    ),
                    level(p),
                ),
                Rect { y: y + 1, height: 1, ..inner },
            );
            y += 3;
        }
        let mut io: Vec<Line> = vec![
            kv_line("Read now", fmt_rate(s.disk_read()), Color::Cyan),
            kv_line("Write now", fmt_rate(s.disk_write()), Color::LightMagenta),
            Line::raw(""),
        ];
        for d in &s.disk_io {
            io.push(kv_line(
                &d.name,
                format!("{} read · {} written since boot", fmt_bytes(d.read_total), fmt_bytes(d.write_total)),
                DIM,
            ));
        }
        f.render_widget(Paragraph::new(io).block(boxed(" Disk activity ", Color::Reset)), io_a);

        let mut lines: Vec<Line> = Vec::new();
        for n in s.nets.iter().filter(|n| !n.virtual_ || n.up) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<12}", n.name), Style::new().bold()),
                Span::styled(format!("{:<24}", net_kind(n)), Style::new().fg(DIM)),
                if n.up {
                    Span::styled("connected   ", Style::new().fg(OK))
                } else {
                    Span::styled("disconnected", Style::new().fg(DIM))
                },
                Span::raw(format!("  ↓ {:>11}  ↑ {:>11}", fmt_rate(n.rx_bps), fmt_rate(n.tx_bps))),
                Span::styled(
                    format!("   {}", n.addrs.first().or(n.addrs6.first()).cloned().unwrap_or_default()),
                    Style::new().fg(DIM),
                ),
                Span::styled(n.signal_dbm.map(|d| format!("  {d} dBm")).unwrap_or_default(), Style::new().fg(DIM)),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::styled("No network interfaces found.", Style::new().fg(DIM)));
        }
        f.render_widget(
            Paragraph::new(lines).block(boxed(
                format!(" Network · ↓ {} · ↑ {} ", fmt_rate(s.net_rx()), fmt_rate(s.net_tx())),
                Color::Reset,
            )),
            bottom,
        );
    }

    fn draw_sensors(&self, f: &mut Frame, area: Rect) {
        let s = &self.snap;
        let [left, right] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
        let mut lines: Vec<Line> = Vec::new();
        let mut last_group = String::new();
        for t in &s.temps {
            let g = sensor_group(&t.chip);
            if g != last_group {
                lines.push(Line::styled(g.clone(), Style::new().bold()));
                last_group = g;
            }
            let limit = t.crit.or(t.high).map(|c| format!("  (critical {})", fmt_temp(c))).unwrap_or_default();
            lines.push(Line::from(vec![
                Span::raw(format!("  {:<22}", truncate(&t.label, 22))),
                Span::styled(
                    format!("{:>8}", stats::fmt_temp_exact(t.celsius)),
                    Style::new().fg(stats_color_temp_limit(t.celsius, t.crit.or(t.high))),
                ),
                Span::styled(limit, Style::new().fg(DIM)),
            ]));
        }
        if lines.is_empty() {
            lines
                .push(Line::styled("No temperature sensors found (virtual machines have none).", Style::new().fg(DIM)));
        }
        for fan in &s.fans {
            lines.push(kv_line(&fan.label, format!("{} rpm", fan.rpm), Color::Reset));
        }
        for g in &s.gpus {
            lines.push(Line::raw(""));
            lines.push(Line::styled(g.name.clone(), Style::new().bold()));
            if let Some(u) = g.usage {
                lines.push(kv_line("  Busy", format!("{u:.0}%"), level(u)));
            }
            if let Some(t) = g.temp {
                lines.push(kv_line("  Temperature", fmt_temp(t), stats_color_temp(t)));
            }
            if let Some(w) = g.power_w {
                lines.push(kv_line("  Power", format!("{w:.1} W"), Color::Reset));
            }
        }
        f.render_widget(
            Paragraph::new(lines)
                .scroll((self.scroll, 0))
                .block(boxed(" Temperatures, fans and graphics ", Color::Reset)),
            left,
        );

        let mut lines: Vec<Line> = Vec::new();
        for b in &s.batteries {
            lines.push(Line::styled(format!("{} {}", b.name, b.model), Style::new().bold()));
            lines.push(kv_line("  Charge", format!("{:.0}% · {}", b.percent, b.status), level(100.0 - b.percent)));
            lines.push(kv_line(
                "  Power",
                b.power_w
                    .map(|w| format!("{w:.1} W{}", if b.estimated { " (estimated)" } else { "" }))
                    .unwrap_or_else(|| "—".into()),
                Color::Reset,
            ));
            lines.push(kv_line(
                "  Time left",
                b.time_left
                    .map(|t| format!("{}{}", fmt_duration(t), if b.estimated { " (estimated)" } else { "" }))
                    .unwrap_or_else(|| "—".into()),
                Color::Reset,
            ));
            lines.push(kv_line(
                "  Health",
                b.health.map(|h| format!("{h:.0}%")).unwrap_or_else(|| "—".into()),
                Color::Reset,
            ));
            lines.push(kv_line(
                "  Cycles",
                b.cycles.map(|c| c.to_string()).unwrap_or_else(|| "—".into()),
                Color::Reset,
            ));
        }
        if s.batteries.is_empty() {
            lines.push(Line::styled("No battery (mains powered).", Style::new().fg(DIM)));
        }
        for d in &s.device_batteries {
            lines.push(kv_line(
                &d.name,
                d.percent.map(|p| format!("{p:.0}%")).unwrap_or_else(|| d.level.clone()),
                Color::Reset,
            ));
        }
        f.render_widget(Paragraph::new(lines).block(boxed(" Power ", Color::Reset)), right);
    }

    fn draw_system(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        for (title, rows) in &self.report {
            lines.push(Line::styled(title.clone(), Style::new().bold().fg(CPU)));
            for (k, v) in rows {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {k:<26}"), Style::new().fg(DIM)),
                    Span::raw(v.clone()),
                ]));
            }
            lines.push(Line::raw(""));
        }
        f.render_widget(
            Paragraph::new(lines)
                .scroll((self.scroll, 0))
                .wrap(Wrap { trim: false })
                .block(boxed(" About this machine (↑↓ to scroll) ", Color::Reset)),
            area,
        );
    }
}

fn boxed(title: impl Into<String>, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Span::styled(title.into(), Style::new().fg(color).bold()))
}

fn gauge<'a>(label: &str, ratio: f32, value: &str, color: Color) -> LineGauge<'a> {
    LineGauge::default()
        .ratio(ratio.clamp(0.0, 1.0) as f64)
        .label(Line::from(vec![
            Span::raw(format!("{label} ")),
            Span::styled(value.to_string(), Style::new().fg(color)),
        ]))
        .filled_style(Style::new().fg(color))
        .unfilled_style(Style::new().fg(DIM))
}

fn kv_line<'a>(label: &str, value: String, color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<16} "), Style::new().fg(DIM)),
        Span::styled(value, Style::new().fg(color)),
    ])
}

fn stats_color_temp(c: f32) -> Color {
    stats_color_temp_limit(c, None)
}

fn stats_color_temp_limit(c: f32, crit: Option<f32>) -> Color {
    level(c / crit.unwrap_or(100.0) * 100.0)
}

/// The newest `width` points of a history, scaled to integers for a sparkline.
fn tail(q: &VecDeque<f32>, width: u16) -> Vec<u64> {
    let skip = q.len().saturating_sub(width as usize);
    q.iter().skip(skip).map(|v| v.max(0.0).round() as u64).collect()
}

fn proc_widths() -> [Constraint; 6] {
    [
        Constraint::Length(7),
        Constraint::Length(12),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Min(10),
    ]
}

fn centered(area: Rect, width_pct: u16, height: u16) -> Rect {
    let w = area.width * width_pct / 100;
    let h = height.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("1 … 5, Tab, ← →", "switch tabs"),
        ("p or Space", "pause or resume"),
        ("+ / -", "refresh faster or slower"),
        ("/", "search processes (name, command, user or PID)"),
        ("↑ ↓ PgUp PgDn Home End", "move through the list or scroll"),
        ("Enter", "details of the selected process"),
        ("s / r", "change the sort column / reverse it"),
        ("k", "show or hide kernel threads"),
        ("o", "show only my processes"),
        ("q, Esc or Ctrl+C", "quit"),
    ];
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| Line::from(vec![Span::styled(format!("  {k:<24}"), Style::new().bold()), Span::raw(*v)]))
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled("  sysmon is view-only. Press any key to close this help.", Style::new().fg(DIM)));
    let popup = centered(area, 70, lines.len() as u16 + 2);
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(boxed(" Keys ", CPU)), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app() -> Tui {
        let info = StaticInfo::load();
        let mut collector = Collector::new(info.cpu_threads);
        collector.sample();
        std::thread::sleep(Duration::from_millis(200));
        let snap = collector.sample();
        let mut hist = History::default();
        hist.record(&snap);
        Tui {
            report: report::gather(&info, &snap),
            info,
            collector,
            snap,
            hist,
            report_at: None,
            tab: 0,
            interval: 1000,
            paused: false,
            sort: Sort::Cpu,
            desc: true,
            query: String::new(),
            searching: false,
            show_kernel: false,
            mine_only: false,
            uid: 0,
            table: TableState::default().with_selected(Some(0)),
            details: false,
            help: false,
            scroll: 0,
        }
    }

    fn screen(t: &mut Tui, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect()
    }

    #[test]
    fn every_tab_draws_at_common_sizes() {
        let mut t = app();
        for (tab, name) in TABS.iter().enumerate() {
            t.set_tab(tab);
            for (w, h) in [(80, 24), (120, 40), (200, 60)] {
                let s = screen(&mut t, w, h);
                assert!(s.contains("sysmon"), "tab {tab} at {w}x{h}");
                assert!(s.contains(name), "tab {tab} title at {w}x{h}");
            }
        }
    }

    #[test]
    fn small_terminal_gets_a_message() {
        let mut t = app();
        assert!(screen(&mut t, 40, 10).contains("too small"));
    }

    #[test]
    fn keys_switch_tabs_search_and_quit() {
        let mut t = app();
        let key = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
        assert!(t.on_key(key(KeyCode::Char('3'))));
        assert_eq!(t.tab, 2);
        assert!(t.on_key(key(KeyCode::Char('/'))));
        assert_eq!(t.tab, 1);
        for c in "sysmon".chars() {
            t.on_key(key(KeyCode::Char(c)));
        }
        t.on_key(key(KeyCode::Enter));
        assert_eq!(t.query, "sysmon");
        assert!(t.procs().iter().all(|p| p.name.contains("sysmon") || p.cmd.contains("sysmon")));
        assert!(t.on_key(key(KeyCode::Char('+'))));
        assert_eq!(t.interval, 2000);
        assert!(t.on_key(key(KeyCode::Char('?'))) && t.help);
        assert!(t.on_key(key(KeyCode::Char('x'))) && !t.help, "any key closes help");
        assert!(!t.on_key(key(KeyCode::Char('q'))));
    }

    #[test]
    fn process_details_popup() {
        let mut t = app();
        t.set_tab(1);
        t.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.details);
        assert!(screen(&mut t, 120, 40).contains("Command line"));
    }
}
