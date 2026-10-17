//! A scrolling line chart for value histories.

use std::collections::VecDeque;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry, Path, Program, Stroke};
use iced::{mouse, Color, Point, Rectangle, Size, Theme};

use crate::style;

pub struct Series<'a> {
    pub name: String,
    pub data: &'a VecDeque<f32>,
    pub color: Color,
}

pub struct Graph<'a> {
    pub series: Vec<Series<'a>>,
    /// Fixed top of the scale (100 for percentages), or `None` to follow the data.
    pub max: Option<f32>,
    /// Smallest top used when following the data, so idle lines stay flat.
    pub floor: f32,
    pub capacity: usize,
    pub unit: fn(f32) -> String,
    /// Draw the scale and the hover readout (off for small sparklines).
    pub labels: bool,
}

impl<'a> Graph<'a> {
    fn top(&self) -> f32 {
        self.max.unwrap_or_else(|| {
            let peak = self.series.iter().flat_map(|s| s.data.iter().copied()).fold(0.0, f32::max);
            (peak * 1.15).max(self.floor)
        })
    }
}

impl<'a, Message> Program<Message> for Graph<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let p = theme.extended_palette();
        let text = p.background.base.text;
        let grid = style::with_alpha(text, 0.08);

        frame.fill(&Path::rectangle(Point::ORIGIN, bounds.size()), style::graph_bg(theme));
        for i in 1..4 {
            let y = h * i as f32 / 4.0;
            frame.stroke(
                &Path::line(Point::new(0.0, y), Point::new(w, y)),
                Stroke::default().with_color(grid).with_width(1.0),
            );
        }

        let top = self.top();
        let step = w / (self.capacity.max(2) - 1) as f32;
        let point = |len: usize, i: usize, v: f32| {
            let x = w - (len - 1 - i) as f32 * step;
            let y = h - (v / top).clamp(0.0, 1.0) * (h - 2.0) - 1.0;
            Point::new(x, y)
        };

        for (n, s) in self.series.iter().enumerate() {
            let len = s.data.len();
            if len < 2 {
                continue;
            }
            let line = Path::new(|b| {
                for (i, v) in s.data.iter().enumerate() {
                    let pt = point(len, i, *v);
                    if i == 0 {
                        b.move_to(pt)
                    } else {
                        b.line_to(pt)
                    }
                }
            });
            // Only the first series gets a filled area, so overlapping
            // series stay readable.
            if n == 0 {
                let area = Path::new(|b| {
                    let first = point(len, 0, s.data[0]);
                    b.move_to(Point::new(first.x, h));
                    for (i, v) in s.data.iter().enumerate() {
                        b.line_to(point(len, i, *v));
                    }
                    b.line_to(Point::new(w, h));
                    b.close();
                });
                frame.fill(&area, style::with_alpha(s.color, 0.16));
            }
            frame.stroke(&line, Stroke::default().with_color(s.color).with_width(if self.labels { 1.8 } else { 1.4 }));
        }

        if self.labels {
            frame.fill_text(canvas::Text {
                content: (self.unit)(top),
                position: Point::new(w - 6.0, 4.0),
                color: style::with_alpha(text, 0.55),
                size: 11.0.into(),
                horizontal_alignment: Horizontal::Right,
                ..canvas::Text::default()
            });

            if let Some(pos) = cursor.position_in(bounds) {
                let back = ((w - pos.x) / step).round() as usize;
                frame.stroke(
                    &Path::line(Point::new(pos.x, 0.0), Point::new(pos.x, h)),
                    Stroke::default().with_color(style::with_alpha(text, 0.35)).with_width(1.0),
                );
                let lines: Vec<(String, Color)> = self
                    .series
                    .iter()
                    .filter_map(|s| {
                        let v = s.data.len().checked_sub(1 + back).and_then(|i| s.data.get(i))?;
                        Some((format!("{} {}", s.name, (self.unit)(*v)), s.color))
                    })
                    .collect();
                if !lines.is_empty() {
                    let ago = format!("{back}s ago");
                    let box_w =
                        lines.iter().map(|(l, _)| l.len()).chain([ago.len()]).max().unwrap_or(0) as f32 * 6.6 + 16.0;
                    let box_h = (lines.len() + 1) as f32 * 15.0 + 8.0;
                    let x = if pos.x + 10.0 + box_w < w { pos.x + 10.0 } else { (pos.x - 10.0 - box_w).max(0.0) };
                    frame.fill(
                        &Path::rectangle(Point::new(x, 6.0), Size::new(box_w, box_h)),
                        style::with_alpha(p.background.base.color, 0.92),
                    );
                    let mut y = 10.0;
                    for (l, c) in std::iter::once((ago, style::with_alpha(text, 0.6))).chain(lines) {
                        frame.fill_text(canvas::Text {
                            content: l,
                            position: Point::new(x + 8.0, y),
                            color: c,
                            size: 12.0.into(),
                            vertical_alignment: Vertical::Top,
                            ..canvas::Text::default()
                        });
                        y += 15.0;
                    }
                }
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, _state: &(), bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if self.labels && cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}
