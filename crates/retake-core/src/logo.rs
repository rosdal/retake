//! Retake's mark ("Frame"): a viewfinder's four corner brackets around a dot, on a dark rounded
//! square. Drawn here at any size, so the app icon, tray icons (the dot shows the status) and
//! the window header come from one definition. Pure: RGBA pixels and an `.ico` writer.

/// Brand colours.
pub const BG: [u8; 3] = [0x0E, 0x0F, 0x11];
pub const FG: [u8; 3] = [0xF4, 0xF4, 0xF2];
pub const ACCENT: [u8; 3] = [0xFF, 0x5A, 0x1F];
pub const IDLE: [u8; 3] = [0xA8, 0xAB, 0xB0];
pub const WARNING: [u8; 3] = [0xF5, 0xB8, 0x3D];
pub const ERROR: [u8; 3] = [0xE5, 0x48, 0x4D];

/// Coverage of the mark's parts at a point of the unit square: (background, brackets, dot).
fn shapes(x: f32, y: f32) -> (bool, bool, bool) {
    // Rounded square background.
    let r = 0.22;
    let (cx, cy) = (x.clamp(r, 1.0 - r), y.clamp(r, 1.0 - r));
    let bg = (x - cx).powi(2) + (y - cy).powi(2) <= r * r;
    // Corner brackets: an L in each corner of the inner square.
    let (lo, hi, arm, w) = (0.2, 0.8, 0.2, 0.09);
    let near = |v: f32, edge: f32| (v - edge).abs() <= w / 2.0;
    let within = |v: f32, edge: f32, dir: f32| {
        let d = (v - edge) * dir;
        (-w / 2.0..=arm).contains(&d)
    };
    let mut br = false;
    for (ex, dx) in [(lo, 1.0), (hi, -1.0)] {
        for (ey, dy) in [(lo, 1.0), (hi, -1.0)] {
            if (near(x, ex) && within(y, ey, dy)) || (near(y, ey) && within(x, ex, dx)) {
                br = true;
            }
        }
    }
    // The dot.
    let dot = (x - 0.5).powi(2) + (y - 0.5).powi(2) <= 0.135f32.powi(2);
    (bg, br, dot)
}

/// The mark as `size × size` RGBA rows, top to bottom, with `dot` as the dot's colour.
/// `background = false` leaves the square transparent.
pub fn rgba(size: u32, dot: [u8; 3], background: bool) -> Vec<u8> {
    const SS: u32 = 4;
    let n = size.max(1);
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for py in 0..n {
        for px in 0..n {
            let (mut acc, mut alpha) = ([0f32; 3], 0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32;
                    let (bg, br, d) = shapes(x, y);
                    let colour = if d {
                        Some(dot)
                    } else if br {
                        Some(FG)
                    } else if bg && background {
                        Some(BG)
                    } else {
                        None
                    };
                    if let Some(c) = colour {
                        for k in 0..3 {
                            acc[k] += f32::from(c[k]);
                        }
                        alpha += 1.0;
                    }
                }
            }
            let a = alpha / (SS * SS) as f32;
            let px_col = |k: usize| {
                if alpha > 0.0 {
                    (acc[k] / alpha) as u8
                } else {
                    0
                }
            };
            out.extend_from_slice(&[px_col(0), px_col(1), px_col(2), (a * 255.0).round() as u8]);
        }
    }
    out
}

/// An `.ico` file holding each `(size, rgba)` image as a 32-bit bitmap.
pub fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut dir = Vec::new();
    let mut data = Vec::new();
    let header = 6 + 16 * images.len();
    dir.extend_from_slice(&[0, 0, 1, 0]);
    dir.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (size, rgba) in images {
        let n = *size;
        let mut bmp = Vec::new();
        // BITMAPINFOHEADER: height counts the XOR and AND masks.
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(n as i32).to_le_bytes());
        bmp.extend_from_slice(&((2 * n) as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0u8; 24]);
        // Pixels bottom-up, BGRA.
        for y in (0..n).rev() {
            for x in 0..n {
                let i = ((y * n + x) * 4) as usize;
                bmp.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
            }
        }
        // AND mask: all clear (alpha decides), rows padded to 32 bits.
        let row = n.div_ceil(32) * 4;
        bmp.extend(std::iter::repeat_n(0u8, (row * n) as usize));
        let b = |v: u32| if v >= 256 { 0 } else { v as u8 };
        dir.extend_from_slice(&[b(n), b(n), 0, 0]);
        dir.extend_from_slice(&1u16.to_le_bytes());
        dir.extend_from_slice(&32u16.to_le_bytes());
        dir.extend_from_slice(&(bmp.len() as u32).to_le_bytes());
        dir.extend_from_slice(&((header + data.len()) as u32).to_le_bytes());
        data.extend(bmp);
    }
    dir.extend(data);
    dir
}

/// The app icon: every size Windows asks for.
pub fn app_ico() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = [16, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&s| (s, rgba(s, ACCENT, true)))
        .collect();
    ico(&images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_has_its_parts_where_expected() {
        let n = 64;
        let p = rgba(n, ACCENT, true);
        let at = |x: u32, y: u32| {
            let i = ((y * n + x) * 4) as usize;
            [p[i], p[i + 1], p[i + 2], p[i + 3]]
        };
        assert_eq!(at(32, 32), [ACCENT[0], ACCENT[1], ACCENT[2], 255], "dot");
        assert_eq!(&at(13, 18)[..3], &FG, "top-left bracket");
        assert_eq!(&at(32, 20)[..3], &BG, "background between brackets");
        assert_eq!(at(0, 0)[3], 0, "rounded corner is transparent");
        let clear = rgba(n, ACCENT, false);
        assert_eq!(clear[((20 * n + 32) * 4 + 3) as usize], 0);
    }

    #[test]
    fn ico_directory_points_at_each_image() {
        let f = ico(&[(16, rgba(16, IDLE, true)), (32, rgba(32, IDLE, true))]);
        assert_eq!(&f[..6], &[0, 0, 1, 0, 2, 0]);
        let size = u32::from_le_bytes(f[14..18].try_into().unwrap()) as usize;
        let off = u32::from_le_bytes(f[18..22].try_into().unwrap()) as usize;
        assert_eq!(off, 6 + 32);
        assert_eq!(size, 40 + 16 * 16 * 4 + 16 * 4);
        let off2 = u32::from_le_bytes(f[34..38].try_into().unwrap()) as usize;
        assert_eq!(off2, off + size);
        assert_eq!(f.len(), off2 + 40 + 32 * 32 * 4 + 32 * 4);
        assert_eq!(app_ico()[4], 9);
    }
}
