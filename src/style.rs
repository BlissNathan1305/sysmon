//! Colours and custom widget styles.

use iced::theme::Palette;
use iced::widget::{button, container, progress_bar};
use iced::{color, Background, Border, Color, Shadow, Theme, Vector};

pub const CPU: Color = color!(0x5a, 0xa9, 0xff);
pub const MEM: Color = color!(0xb4, 0x8c, 0xff);
pub const SWAP: Color = color!(0xff, 0xb4, 0x54);
pub const GPU: Color = color!(0x5c, 0xd6, 0x8f);
pub const TEMP: Color = color!(0xff, 0x7a, 0x6b);
pub const RX: Color = color!(0x4c, 0xc3, 0x8a);
pub const TX: Color = color!(0xff, 0x9f, 0x43);
pub const READ: Color = color!(0x3c, 0xc7, 0xc7);
pub const WRITE: Color = color!(0xff, 0x7a, 0xb6);
pub const OK: Color = color!(0x4c, 0xc3, 0x8a);
pub const WARN: Color = color!(0xff, 0xb4, 0x54);
pub const HOT: Color = color!(0xff, 0x5f, 0x5f);

/// Line colours for per-core graphs.
pub const CORES: [Color; 8] = [
    color!(0x5a, 0xa9, 0xff),
    color!(0x4c, 0xc3, 0x8a),
    color!(0xff, 0xb4, 0x54),
    color!(0xff, 0x7a, 0xb6),
    color!(0xb4, 0x8c, 0xff),
    color!(0x3c, 0xc7, 0xc7),
    color!(0xff, 0x7a, 0x6b),
    color!(0xd4, 0xd4, 0x5a),
];

pub fn dark() -> Theme {
    Theme::custom(
        "Sysmon Dark".into(),
        Palette {
            background: color!(0x14, 0x16, 0x1b),
            text: color!(0xe4, 0xe7, 0xee),
            primary: color!(0x4a, 0x93, 0xe8),
            success: OK,
            danger: HOT,
        },
    )
}

pub fn light() -> Theme {
    Theme::custom(
        "Sysmon Light".into(),
        Palette {
            background: color!(0xf3, 0xf4, 0xf7),
            text: color!(0x1d, 0x21, 0x2a),
            primary: color!(0x2f, 0x7b, 0xd8),
            success: color!(0x22, 0x9a, 0x62),
            danger: color!(0xd9, 0x3f, 0x3f),
        },
    )
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1.0 }
}

fn is_dark(theme: &Theme) -> bool {
    let bg = theme.palette().background;
    bg.r + bg.g + bg.b < 1.5
}

/// Card background: a step lighter than the window in dark mode, white in light mode.
fn card_color(theme: &Theme) -> Color {
    let bg = theme.palette().background;
    if is_dark(theme) {
        mix(bg, Color::WHITE, 0.045)
    } else {
        Color::WHITE
    }
}

pub fn graph_bg(theme: &Theme) -> Color {
    let bg = theme.palette().background;
    if is_dark(theme) {
        mix(bg, Color::BLACK, 0.25)
    } else {
        mix(bg, Color::WHITE, 0.4)
    }
}

pub fn muted(theme: &Theme) -> Color {
    with_alpha(theme.palette().text, 0.6)
}

/// Green, amber or red for a 0–100 load figure.
pub fn level(pct: f32) -> Color {
    if pct >= 85.0 {
        HOT
    } else if pct >= 60.0 {
        WARN
    } else {
        OK
    }
}

/// Colour for a temperature, judged against its critical point when known.
pub fn temp_level(c: f32, crit: Option<f32>) -> Color {
    let crit = crit.unwrap_or(100.0);
    level(c / crit * 100.0)
}

struct Card;

impl container::StyleSheet for Card {
    type Style = Theme;

    fn appearance(&self, theme: &Theme) -> container::Appearance {
        container::Appearance {
            background: Some(Background::Color(card_color(theme))),
            text_color: Some(theme.palette().text),
            border: Border { color: with_alpha(theme.palette().text, 0.07), width: 1.0, radius: 10.0.into() },
            shadow: Shadow {
                color: with_alpha(Color::BLACK, if is_dark(theme) { 0.25 } else { 0.06 }),
                offset: Vector::new(0.0, 2.0),
                blur_radius: 8.0,
            },
        }
    }
}

pub fn card() -> iced::theme::Container {
    iced::theme::Container::Custom(Box::new(Card))
}

struct Header;

impl container::StyleSheet for Header {
    type Style = Theme;

    fn appearance(&self, theme: &Theme) -> container::Appearance {
        container::Appearance {
            background: Some(Background::Color(card_color(theme))),
            border: Border { color: with_alpha(theme.palette().text, 0.07), width: 1.0, radius: 0.0.into() },
            ..Default::default()
        }
    }
}

pub fn header() -> iced::theme::Container {
    iced::theme::Container::Custom(Box::new(Header))
}

/// A table row: striped, and tinted when selected.
struct RowBg {
    selected: bool,
    stripe: bool,
}

impl container::StyleSheet for RowBg {
    type Style = Theme;

    fn appearance(&self, theme: &Theme) -> container::Appearance {
        let background = if self.selected {
            Some(Background::Color(with_alpha(theme.extended_palette().primary.base.color, 0.22)))
        } else if self.stripe {
            Some(Background::Color(with_alpha(theme.palette().text, 0.035)))
        } else {
            None
        };
        container::Appearance { background, border: Border::with_radius(4.0), ..Default::default() }
    }
}

pub fn row_bg(selected: bool, stripe: bool) -> iced::theme::Container {
    iced::theme::Container::Custom(Box::new(RowBg { selected, stripe }))
}

struct Tab(bool);

impl button::StyleSheet for Tab {
    type Style = Theme;

    fn active(&self, theme: &Theme) -> button::Appearance {
        let p = theme.extended_palette();
        button::Appearance {
            background: self.0.then(|| Background::Color(with_alpha(p.primary.base.color, 0.18))),
            text_color: if self.0 { p.primary.strong.color } else { muted(theme) },
            border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 8.0.into() },
            ..Default::default()
        }
    }

    fn hovered(&self, theme: &Theme) -> button::Appearance {
        let p = theme.extended_palette();
        button::Appearance {
            background: Some(Background::Color(with_alpha(p.primary.base.color, if self.0 { 0.24 } else { 0.08 }))),
            text_color: if self.0 { p.primary.strong.color } else { theme.palette().text },
            ..self.active(theme)
        }
    }
}

pub fn tab(active: bool) -> iced::theme::Button {
    iced::theme::Button::Custom(Box::new(Tab(active)))
}

/// A flat button for table headers and small toggles.
struct Flat(bool);

impl button::StyleSheet for Flat {
    type Style = Theme;

    fn active(&self, theme: &Theme) -> button::Appearance {
        button::Appearance {
            background: None,
            text_color: if self.0 { theme.palette().text } else { muted(theme) },
            border: Border::with_radius(4.0),
            ..Default::default()
        }
    }

    fn hovered(&self, theme: &Theme) -> button::Appearance {
        button::Appearance {
            background: Some(Background::Color(with_alpha(theme.palette().text, 0.06))),
            text_color: theme.palette().text,
            ..self.active(theme)
        }
    }
}

pub fn flat(active: bool) -> iced::theme::Button {
    iced::theme::Button::Custom(Box::new(Flat(active)))
}

struct Bar(Color);

impl progress_bar::StyleSheet for Bar {
    type Style = Theme;

    fn appearance(&self, theme: &Theme) -> progress_bar::Appearance {
        progress_bar::Appearance {
            background: Background::Color(with_alpha(theme.palette().text, 0.09)),
            bar: Background::Color(self.0),
            border_radius: 4.0.into(),
        }
    }
}

pub fn bar(c: Color) -> iced::theme::ProgressBar {
    iced::theme::ProgressBar::Custom(Box::new(Bar(c)))
}
