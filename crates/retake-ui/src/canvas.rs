//! Drawn widgets: the clip timeline (kill markers, playhead, trim handles) and the montage
//! timeline (song waveform, beats, the drop, clip blocks to drag and resize). They draw no
//! text: iced 0.13's CPU renderer gives canvas text an infinite clip, which breaks its damage
//! tracking, so labels are ordinary widgets around the canvases.

use iced::mouse::{self, Cursor};
use iced::widget::canvas::{self, Event, Frame, Geometry, Path};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme};

use crate::style;

// ---- Clip timeline ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimelineMsg {
    Seek(f64),
    TrimIn(f64),
    TrimOut(f64),
}

pub struct Timeline {
    pub duration: f64,
    pub position: f64,
    pub kills: Vec<f64>,
    pub trim: (f64, f64),
}

#[derive(Default)]
pub struct TimelineState {
    dragging: Option<Handle>,
}

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    In,
    Out,
    Head,
}

const PAD: f32 = 12.0;

/// A filled rectangle snapped to whole pixels: iced 0.13's CPU renderer drops some
/// rectangles with fractional sizes.
fn rect(f: &mut Frame, at: Point, size: Size, colour: impl Into<canvas::Fill>) {
    let (x0, y0) = (at.x.round(), at.y.round());
    let (x1, y1) = ((at.x + size.width).round(), (at.y + size.height).round());
    f.fill_rectangle(
        Point::new(x0, y0),
        Size::new((x1 - x0).max(1.0), (y1 - y0).max(1.0)),
        colour,
    );
}

impl Timeline {
    fn x_of(&self, t: f64, w: f32) -> f32 {
        PAD + (t / self.duration.max(0.001)) as f32 * (w - 2.0 * PAD)
    }

    fn t_of(&self, x: f32, w: f32) -> f64 {
        (f64::from((x - PAD) / (w - 2.0 * PAD))).clamp(0.0, 1.0) * self.duration
    }
}

impl canvas::Program<TimelineMsg> for Timeline {
    type State = TimelineState;

    fn update(
        &self,
        state: &mut TimelineState,
        event: Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<TimelineMsg>) {
        let w = bounds.width;
        let pos = cursor.position_in(bounds);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = pos else {
                    return (canvas::event::Status::Ignored, None);
                };
                let near = |t: f64| (self.x_of(t, w) - p.x).abs() < 10.0;
                state.dragging = Some(if near(self.trim.0) && p.y > bounds.height * 0.5 {
                    Handle::In
                } else if near(self.trim.1) && p.y > bounds.height * 0.5 {
                    Handle::Out
                } else {
                    Handle::Head
                });
                let t = self.t_of(p.x, w);
                let msg = match state.dragging {
                    Some(Handle::Head) => Some(TimelineMsg::Seek(t)),
                    _ => None,
                };
                (canvas::event::Status::Captured, msg)
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (Some(h), Some(p)) = (state.dragging, cursor.position_from(bounds.position()))
                else {
                    return (canvas::event::Status::Ignored, None);
                };
                let t = self.t_of(p.x, w);
                let msg = match h {
                    Handle::In => TimelineMsg::TrimIn(t.min(self.trim.1 - 0.5)),
                    Handle::Out => TimelineMsg::TrimOut(t.max(self.trim.0 + 0.5)),
                    Handle::Head => TimelineMsg::Seek(t),
                };
                (canvas::event::Status::Captured, Some(msg))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.dragging = None;
                (canvas::event::Status::Ignored, None)
            }
            _ => (canvas::event::Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        _state: &TimelineState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let mid = h * 0.62;
        // Track, the kept part, kill markers, playhead.
        rect(
            &mut f,
            Point::new(PAD, mid - 3.0),
            Size::new(w - 2.0 * PAD, 6.0),
            style::RAISED,
        );
        let (a, b) = (self.x_of(self.trim.0, w), self.x_of(self.trim.1, w));
        rect(
            &mut f,
            Point::new(a, mid - 3.0),
            Size::new(b - a, 6.0),
            style::MUTED,
        );
        for &k in &self.kills {
            let x = self.x_of(k, w);
            f.fill(&Path::circle(Point::new(x, mid - 16.0), 5.0), style::ACCENT);
            rect(
                &mut f,
                Point::new(x - 1.0, mid - 12.0),
                Size::new(2.0, 12.0),
                style::ACCENT,
            );
        }
        for x in [a, b] {
            rect(
                &mut f,
                Point::new(x - 3.0, mid - 12.0),
                Size::new(6.0, 24.0),
                style::TEXT,
            );
        }
        let px = self.x_of(self.position, w);
        rect(
            &mut f,
            Point::new(px - 1.0, 4.0),
            Size::new(2.0, h - 8.0),
            style::TEXT,
        );
        vec![f.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &TimelineState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

// ---- Montage timeline ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MontageMsg {
    Select(usize),
    /// Move the clip at this project index to another project index.
    Move {
        from: usize,
        to: usize,
    },
    /// Change a clip's length by this many seconds of output (drag of its right edge).
    Resize {
        index: usize,
        by: f64,
    },
}

/// One clip's block: output span and project index.
#[derive(Debug, Clone)]
pub struct Block {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub kills: Vec<f64>,
    pub role: String,
}

pub struct MontageView {
    pub length: f64,
    /// Song peaks over the montage window.
    pub peaks: Vec<f32>,
    /// Beat times (output seconds) and which are bar starts.
    pub beats: Vec<(f64, bool)>,
    pub drop: Option<f64>,
    pub blocks: Vec<Block>,
    pub selected: Option<usize>,
    pub playhead: Option<f64>,
}

#[derive(Default)]
pub struct MontageState {
    drag: Option<Drag>,
}

#[derive(Clone, Copy)]
enum Drag {
    Move {
        index: usize,
        grab_x: f32,
        now_x: f32,
    },
    Resize {
        index: usize,
        grab_x: f32,
        now_x: f32,
    },
}

impl MontageView {
    fn x_of(&self, t: f64, w: f32) -> f32 {
        PAD + (t / self.length.max(0.1)) as f32 * (w - 2.0 * PAD)
    }

    fn secs_per_px(&self, w: f32) -> f64 {
        self.length / f64::from(w - 2.0 * PAD)
    }

    fn block_at(&self, x: f32, w: f32) -> Option<&Block> {
        self.blocks
            .iter()
            .find(|b| x >= self.x_of(b.start, w) && x <= self.x_of(b.end, w))
    }
}

impl canvas::Program<MontageMsg> for MontageView {
    type State = MontageState;

    fn update(
        &self,
        state: &mut MontageState,
        event: Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (canvas::event::Status, Option<MontageMsg>) {
        let w = bounds.width;
        let track_top = bounds.height * 0.52;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_in(bounds) else {
                    return (canvas::event::Status::Ignored, None);
                };
                if p.y < track_top {
                    return (canvas::event::Status::Ignored, None);
                }
                let Some(b) = self.block_at(p.x, w) else {
                    return (canvas::event::Status::Ignored, None);
                };
                let edge = (self.x_of(b.end, w) - p.x).abs() < 8.0;
                state.drag = Some(if edge {
                    Drag::Resize {
                        index: b.index,
                        grab_x: p.x,
                        now_x: p.x,
                    }
                } else {
                    Drag::Move {
                        index: b.index,
                        grab_x: p.x,
                        now_x: p.x,
                    }
                });
                (
                    canvas::event::Status::Captured,
                    Some(MontageMsg::Select(b.index)),
                )
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(p) = cursor.position_from(bounds.position()) else {
                    return (canvas::event::Status::Ignored, None);
                };
                match &mut state.drag {
                    Some(Drag::Move { now_x, .. } | Drag::Resize { now_x, .. }) => {
                        *now_x = p.x;
                        (canvas::event::Status::Captured, None)
                    }
                    None => (canvas::event::Status::Ignored, None),
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let msg = match state.drag.take() {
                    Some(Drag::Move {
                        index,
                        grab_x,
                        now_x,
                    }) if (now_x - grab_x).abs() > 6.0 => {
                        // Drop onto the block under the cursor: take its place.
                        self.block_at(now_x, w)
                            .filter(|b| b.index != index)
                            .map(|b| MontageMsg::Move {
                                from: index,
                                to: b.index,
                            })
                    }
                    Some(Drag::Resize {
                        index,
                        grab_x,
                        now_x,
                    }) if (now_x - grab_x).abs() > 3.0 => Some(MontageMsg::Resize {
                        index,
                        by: f64::from(now_x - grab_x) * self.secs_per_px(w),
                    }),
                    _ => None,
                };
                (canvas::event::Status::Captured, msg)
            }
            _ => (canvas::event::Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        state: &MontageState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let wave_mid = h * 0.26;
        let wave_h = h * 0.22;
        // Waveform.
        if !self.peaks.is_empty() {
            let n = self.peaks.len();
            let bw = (w - 2.0 * PAD) / n as f32;
            for (i, p) in self.peaks.iter().enumerate() {
                let x = PAD + i as f32 * bw;
                let ph = (p * wave_h).max(1.0);
                rect(
                    &mut f,
                    Point::new(x, wave_mid - ph),
                    Size::new(bw.max(1.0) * 0.8, ph * 2.0),
                    Color {
                        a: 0.55,
                        ..style::MUTED
                    },
                );
            }
        }
        // Beats (bars brighter), the drop.
        for &(t, bar) in &self.beats {
            let x = self.x_of(t, w);
            let (top, c) = if bar {
                (
                    4.0,
                    Color {
                        a: 0.7,
                        ..style::TEXT
                    },
                )
            } else {
                (
                    wave_mid - wave_h * 0.4,
                    Color {
                        a: 0.3,
                        ..style::TEXT
                    },
                )
            };
            rect(&mut f, Point::new(x, top), Size::new(1.0, h * 0.5 - top), c);
        }
        if let Some(d) = self.drop {
            let x = self.x_of(d, w);
            rect(
                &mut f,
                Point::new(x - 1.0, 0.0),
                Size::new(3.0, h),
                style::ACCENT,
            );
        }
        // Clip blocks.
        let top = h * 0.55;
        let bh = h * 0.4;
        for b in &self.blocks {
            let (mut x0, mut x1) = (self.x_of(b.start, w), self.x_of(b.end, w));
            match state.drag {
                Some(Drag::Move {
                    index,
                    grab_x,
                    now_x,
                }) if index == b.index => {
                    x0 += now_x - grab_x;
                    x1 += now_x - grab_x;
                }
                Some(Drag::Resize {
                    index,
                    grab_x,
                    now_x,
                }) if index == b.index => {
                    x1 = (x1 + now_x - grab_x).max(x0 + 8.0);
                }
                _ => {}
            }
            let sel = self.selected == Some(b.index);
            // Plain rectangles: filled paths other than circles don't draw with iced 0.13's
            // CPU renderer here.
            let (bx, bw) = (x0 + 1.0, (x1 - x0 - 2.0).max(2.0));
            rect(
                &mut f,
                Point::new(bx, top),
                Size::new(bw, bh),
                if sel {
                    style::ACCENT_DIM
                } else {
                    style::RAISED
                },
            );
            let edge = if sel { style::ACCENT } else { style::LINE };
            let ew = if sel { 2.0 } else { 1.0 };
            for (p, s) in [
                (Point::new(bx, top), Size::new(bw, ew)),
                (Point::new(bx, top + bh - ew), Size::new(bw, ew)),
                (Point::new(bx, top), Size::new(ew, bh)),
                (Point::new(bx + bw - ew, top), Size::new(ew, bh)),
            ] {
                rect(&mut f, p, s, edge);
            }
            for &k in &b.kills {
                let x = self.x_of(k, w);
                f.fill(
                    &Path::circle(Point::new(x, top + bh - 10.0), 3.5),
                    style::ACCENT,
                );
            }
            // Resize grip.
            rect(
                &mut f,
                Point::new(x1 - 4.0, top + bh * 0.3),
                Size::new(2.0, bh * 0.4),
                style::MUTED,
            );
        }
        if let Some(t) = self.playhead {
            let x = self.x_of(t, w);
            rect(
                &mut f,
                Point::new(x - 1.0, 0.0),
                Size::new(2.0, h),
                style::TEXT,
            );
        }
        vec![f.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &MontageState,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        match state.drag {
            Some(Drag::Move { .. }) => return mouse::Interaction::Grabbing,
            Some(Drag::Resize { .. }) => return mouse::Interaction::ResizingHorizontally,
            None => {}
        }
        let Some(p) = cursor.position_in(bounds) else {
            return mouse::Interaction::default();
        };
        if p.y < bounds.height * 0.52 {
            return mouse::Interaction::default();
        }
        match self.block_at(p.x, bounds.width) {
            Some(b) if (self.x_of(b.end, bounds.width) - p.x).abs() < 8.0 => {
                mouse::Interaction::ResizingHorizontally
            }
            Some(_) => mouse::Interaction::Grab,
            None => mouse::Interaction::default(),
        }
    }
}
