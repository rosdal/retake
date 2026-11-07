//! Retake's look: colours, type and the widget styles built from them.

use iced::border::Radius;
use iced::font::{Family, Weight};
use iced::widget::{button, container, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

pub const BG: Color = rgb(0x0E, 0x0F, 0x11);
pub const SURFACE: Color = rgb(0x17, 0x19, 0x1C);
pub const RAISED: Color = rgb(0x21, 0x24, 0x28);
pub const LINE: Color = rgb(0x2C, 0x30, 0x35);
pub const TEXT: Color = rgb(0xF4, 0xF4, 0xF2);
pub const MUTED: Color = rgb(0xA8, 0xAB, 0xB0);
pub const ACCENT: Color = rgb(0xFF, 0x5A, 0x1F);
pub const ACCENT_DIM: Color = rgb(0x5C, 0x2A, 0x17);
pub const WARNING: Color = rgb(0xF5, 0xB8, 0x3D);
pub const ERROR: Color = rgb(0xE5, 0x48, 0x4D);
pub const OK: Color = rgb(0x3D, 0xD6, 0x8C);

pub const INTER: Font = Font::with_name("Inter");
pub const INTER_SEMI: Font = Font {
    family: Family::Name("Inter"),
    weight: Weight::Semibold,
    ..Font::DEFAULT
};
pub const GROTESK_BOLD: Font = Font {
    family: Family::Name("Space Grotesk"),
    weight: Weight::Bold,
    ..Font::DEFAULT
};

/// Every click target is at least this tall.
pub const TARGET: f32 = 44.0;

pub fn theme() -> Theme {
    Theme::custom(
        "Retake".into(),
        iced::theme::Palette {
            background: BG,
            text: TEXT,
            primary: ACCENT,
            success: OK,
            danger: ERROR,
        },
    )
}

/// The root container's style for one frame: the background colour at an alpha that
/// alternates between two invisible values on every call (once per view), so each frame's
/// root quad differs from the last and the CPU renderer repaints everything.
pub fn repaint() -> impl Fn(&Theme) -> container::Style {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FLIP: AtomicBool = AtomicBool::new(false);
    let odd = FLIP.fetch_xor(true, Ordering::Relaxed);
    move |_| container::Style {
        background: Some(Background::Color(BG)),
        border: Border {
            color: Color {
                a: if odd { 0.01 } else { 0.02 },
                ..BG
            },
            width: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A five-pointed star as RGBA pixels (filled = starred), anti-aliased.
pub fn star_rgba(size: u32, filled: bool) -> Vec<u8> {
    let n = size.max(1) as f32;
    let (cx, cy, ro) = (n / 2.0, n / 2.0 + n * 0.04, n / 2.0 - 1.0);
    let ri = ro * 0.45;
    let pts: Vec<(f32, f32)> = (0..10)
        .map(|k| {
            let r = if k % 2 == 0 { ro } else { ri };
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    let inside = |x: f32, y: f32| {
        let mut c = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let ((xi, yi), (xj, yj)) = (pts[i], pts[j]);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    };
    // Outline: inside the star but not inside a slightly smaller one.
    let inner: Vec<(f32, f32)> = pts
        .iter()
        .map(|(x, y)| (cx + (x - cx) * 0.72, cy + (y - cy) * 0.72))
        .collect();
    let inside_inner = |x: f32, y: f32| {
        let mut c = false;
        let mut j = inner.len() - 1;
        for i in 0..inner.len() {
            let ((xi, yi), (xj, yj)) = (inner[i], inner[j]);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    };
    let [r, g, b] = [ACCENT.r, ACCENT.g, ACCENT.b].map(|v| (v * 255.0) as u8);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    const SS: u32 = 4;
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = px as f32 + (sx as f32 + 0.5) / SS as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SS as f32;
                    if inside(x, y) && (filled || !inside_inner(x, y)) {
                        hits += 1;
                    }
                }
            }
            out.extend_from_slice(&[r, g, b, (hits * 255 / (SS * SS)) as u8]);
        }
    }
    out
}

fn radius(r: f32) -> Radius {
    r.into()
}

pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        border: Border {
            radius: radius(12.0),
            width: 1.0,
            color: LINE,
        },
        text_color: Some(TEXT),
        shadow: Shadow::default(),
    }
}

pub fn sidebar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

pub fn page(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

pub fn banner(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(Color { a: 0.14, ..colour })),
        border: Border {
            radius: radius(10.0),
            width: 1.0,
            color: Color { a: 0.5, ..colour },
        },
        text_color: Some(TEXT),
        shadow: Shadow::default(),
    }
}

pub fn badge(colour: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(Color { a: 0.85, ..colour })),
        border: Border {
            radius: radius(6.0),
            ..Default::default()
        },
        text_color: Some(TEXT),
        shadow: Shadow::default(),
    }
}

/// A card that shows it is selected (keyboard focus or the open clip).
pub fn card(selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(SURFACE)),
        border: Border {
            radius: radius(12.0),
            width: if selected { 2.0 } else { 1.0 },
            color: if selected { ACCENT } else { LINE },
        },
        text_color: Some(TEXT),
        shadow: Shadow::default(),
    }
}

fn base_button(bg: Color, fg: Color, status: button::Status, border: Color) -> button::Style {
    let bg = match status {
        button::Status::Hovered => lighten(bg, 0.08),
        button::Status::Pressed => lighten(bg, -0.05),
        button::Status::Disabled => Color { a: 0.4, ..bg },
        button::Status::Active => bg,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: if status == button::Status::Disabled {
            Color { a: 0.5, ..fg }
        } else {
            fg
        },
        border: Border {
            radius: radius(10.0),
            width: 1.0,
            color: border,
        },
        shadow: Shadow {
            color: Color::TRANSPARENT,
            offset: Vector::ZERO,
            blur_radius: 0.0,
        },
    }
}

fn lighten(c: Color, by: f32) -> Color {
    Color {
        r: (c.r + by).clamp(0.0, 1.0),
        g: (c.g + by).clamp(0.0, 1.0),
        b: (c.b + by).clamp(0.0, 1.0),
        a: c.a,
    }
}

/// The one orange button per screen.
pub fn primary(_: &Theme, s: button::Status) -> button::Style {
    base_button(ACCENT, rgb(0x14, 0x0A, 0x05), s, ACCENT)
}

pub fn secondary(_: &Theme, s: button::Status) -> button::Style {
    base_button(RAISED, TEXT, s, LINE)
}

pub fn danger(_: &Theme, s: button::Status) -> button::Style {
    base_button(RAISED, ERROR, s, LINE)
}

/// Sidebar entries: the current screen is marked in the accent colour.
pub fn nav(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, s| {
        let mut st = base_button(
            if active { ACCENT_DIM } else { SURFACE },
            if active { TEXT } else { MUTED },
            s,
            if active { ACCENT } else { SURFACE },
        );
        if !active && s == button::Status::Hovered {
            st.background = Some(Background::Color(RAISED));
            st.text_color = TEXT;
        }
        st
    }
}

/// A transparent button (cards, list rows).
pub fn ghost(_: &Theme, s: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(if s == button::Status::Hovered {
            RAISED
        } else {
            Color::TRANSPARENT
        })),
        text_color: TEXT,
        border: Border {
            radius: radius(12.0),
            ..Default::default()
        },
        shadow: Shadow::default(),
    }
}

pub fn input(_: &Theme, s: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(RAISED),
        border: Border {
            radius: radius(10.0),
            width: 1.0,
            color: if matches!(s, text_input::Status::Focused) {
                ACCENT
            } else {
                LINE
            },
        },
        icon: MUTED,
        placeholder: MUTED,
        value: TEXT,
        selection: ACCENT_DIM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color) -> f32 {
        let f = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Text contrast at least 4.5:1 on every surface it sits on.
    #[test]
    fn text_contrast_meets_wcag_aa() {
        for bg in [BG, SURFACE, RAISED, ACCENT_DIM] {
            assert!(contrast(TEXT, bg) >= 4.5, "text on {bg:?}");
            assert!(contrast(MUTED, bg) >= 4.5, "muted on {bg:?}");
        }
        assert!(
            contrast(rgb(0x14, 0x0A, 0x05), ACCENT) >= 4.5,
            "label on the accent"
        );
        assert!(contrast(ACCENT, BG) >= 4.5, "accent text on the background");
    }
}
