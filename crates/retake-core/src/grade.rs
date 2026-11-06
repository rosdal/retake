//! Automatic per-clip grading, as pure functions: measure a clip (luma percentiles and the
//! midtone colour cast, HUD areas masked), derive a correction (levels with a low-key guard,
//! capped white balance), add contrast and vibrance, apply the look LUT, and bake all of it
//! into one 3D LUT for ffmpeg's `lut3d`. The grade is constant per clip, so it cannot flicker.

/// A 3D LUT in `.cube` order: red changes fastest, then green, then blue.
#[derive(Debug, Clone, PartialEq)]
pub struct Cube {
    pub size: usize,
    pub data: Vec<[f32; 3]>,
}

impl Cube {
    pub fn identity(size: usize) -> Cube {
        Cube::from_fn(size, |rgb| rgb)
    }

    /// Builds a LUT by evaluating `f` at every grid point.
    pub fn from_fn(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> Cube {
        let step = 1.0 / (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(f([r as f32 * step, g as f32 * step, b as f32 * step]));
                }
            }
        }
        Cube { size, data }
    }

    /// Parses a `.cube` file: `LUT_3D_SIZE` 2–129, the default 0–1 domain, comments and
    /// `TITLE` ignored.
    pub fn parse(text: &str) -> Result<Cube, String> {
        let mut size = None;
        let mut data = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") {
                continue;
            }
            if let Some(v) = line.strip_prefix("LUT_3D_SIZE") {
                let s: usize = v
                    .trim()
                    .parse()
                    .map_err(|_| format!("line {}: bad LUT_3D_SIZE", n + 1))?;
                if !(2..=129).contains(&s) {
                    return Err(format!("LUT_3D_SIZE {s} is out of range"));
                }
                size = Some(s);
                continue;
            }
            if let Some(v) = line.strip_prefix("DOMAIN_MIN") {
                if v.split_whitespace().any(|x| x.parse::<f32>() != Ok(0.0)) {
                    return Err("only DOMAIN_MIN 0 0 0 is supported".into());
                }
                continue;
            }
            if let Some(v) = line.strip_prefix("DOMAIN_MAX") {
                if v.split_whitespace().any(|x| x.parse::<f32>() != Ok(1.0)) {
                    return Err("only DOMAIN_MAX 1 1 1 is supported".into());
                }
                continue;
            }
            if line.starts_with("LUT_1D_SIZE") {
                return Err("1D LUTs are not supported".into());
            }
            let v: Vec<f32> = line
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()
                .map_err(|_| format!("line {}: expected three numbers", n + 1))?;
            if v.len() != 3 {
                return Err(format!("line {}: expected three numbers", n + 1));
            }
            data.push([v[0], v[1], v[2]]);
        }
        let size = size.ok_or("no LUT_3D_SIZE")?;
        if data.len() != size * size * size {
            return Err(format!(
                "{} entries for a {size}-point cube (needs {})",
                data.len(),
                size * size * size
            ));
        }
        Ok(Cube { size, data })
    }

    /// Writes a `.cube` file that [`Cube::parse`] reads back exactly.
    pub fn write(&self, title: &str) -> String {
        let mut s = format!("TITLE \"{title}\"\nLUT_3D_SIZE {}\n", self.size);
        for [r, g, b] in &self.data {
            s.push_str(&format!("{r} {g} {b}\n"));
        }
        s
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[(b * self.size + g) * self.size + r]
    }

    /// Looks up `rgb` (0–1, clamped) with tetrahedral interpolation.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let max = (self.size - 1) as f32;
        let p = rgb.map(|c| c.clamp(0.0, 1.0) * max);
        let i = p.map(|c| (c.floor() as usize).min(self.size - 2));
        let f = [p[0] - i[0] as f32, p[1] - i[1] as f32, p[2] - i[2] as f32];
        let c = |dr, dg, db| self.at(i[0] + dr, i[1] + dg, i[2] + db);
        let (c000, c111) = (c(0, 0, 0), c(1, 1, 1));
        let (fr, fg, fb) = (f[0], f[1], f[2]);
        // The six tetrahedra of the unit cube, by the order of the fractions.
        let (w, a, b) = if fr >= fg {
            if fg >= fb {
                ([1.0 - fr, fr - fg, fg - fb, fb], c(1, 0, 0), c(1, 1, 0))
            } else if fr >= fb {
                ([1.0 - fr, fr - fb, fb - fg, fg], c(1, 0, 0), c(1, 0, 1))
            } else {
                ([1.0 - fb, fb - fr, fr - fg, fg], c(0, 0, 1), c(1, 0, 1))
            }
        } else if fb >= fg {
            ([1.0 - fb, fb - fg, fg - fr, fr], c(0, 0, 1), c(0, 1, 1))
        } else if fb >= fr {
            ([1.0 - fg, fg - fb, fb - fr, fr], c(0, 1, 0), c(0, 1, 1))
        } else {
            ([1.0 - fg, fg - fr, fr - fb, fb], c(0, 1, 0), c(1, 1, 0))
        };
        std::array::from_fn(|k| w[0] * c000[k] + w[1] * a[k] + w[2] * b[k] + w[3] * c111[k])
    }
}

/// A rectangle as fractions of the frame (x, y, width, height).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Siege's HUD on a 16:9 frame: round timer and team bar, kill feed, operator card, ammo and
/// gadget panel. Skipped when measuring so white UI text doesn't skew the numbers.
pub const SIEGE_HUD: [Rect; 4] = [
    Rect {
        x: 0.35,
        y: 0.0,
        w: 0.30,
        h: 0.10,
    },
    Rect {
        x: 0.72,
        y: 0.05,
        w: 0.28,
        h: 0.22,
    },
    Rect {
        x: 0.0,
        y: 0.80,
        w: 0.25,
        h: 0.20,
    },
    Rect {
        x: 0.78,
        y: 0.82,
        w: 0.22,
        h: 0.18,
    },
];

/// BT.709 luma of linear-ish display RGB (0–1).
pub fn luma([r, g, b]: [f32; 3]) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

const BINS: usize = 1024;

/// Accumulates frames for [`Stats`]: a luma histogram and the mean colour of midtones.
#[derive(Debug, Clone)]
pub struct Histogram {
    luma: Vec<u64>,
    mid_sum: [f64; 3],
    mid_n: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram {
            luma: vec![0; BINS],
            mid_sum: [0.0; 3],
            mid_n: 0,
        }
    }
}

/// What a clip looks like before grading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    /// Luma at the 0.5th, 50th and 99.5th percentile.
    pub black: f32,
    pub median: f32,
    pub white: f32,
    /// Mean RGB of pixels with luma 0.2–0.8 (ignores muzzle flashes, sky and deep shadows).
    pub mid_mean: [f32; 3],
}

impl Histogram {
    /// Adds one packed RGB24 frame, skipping pixels inside `mask`.
    pub fn add_rgb24(&mut self, pixels: &[u8], width: usize, height: usize, mask: &[Rect]) {
        let boxes: Vec<[usize; 4]> = mask
            .iter()
            .map(|r| {
                [
                    (r.x * width as f32) as usize,
                    (r.y * height as f32) as usize,
                    ((r.x + r.w) * width as f32).ceil() as usize,
                    ((r.y + r.h) * height as f32).ceil() as usize,
                ]
            })
            .collect();
        for y in 0..height {
            for x in 0..width {
                if boxes
                    .iter()
                    .any(|b| x >= b[0] && x < b[2] && y >= b[1] && y < b[3])
                {
                    continue;
                }
                let i = (y * width + x) * 3;
                let Some(p) = pixels.get(i..i + 3) else {
                    return;
                };
                let rgb = [p[0], p[1], p[2]].map(|v| f32::from(v) / 255.0);
                let l = luma(rgb);
                self.luma[((l * (BINS - 1) as f32).round() as usize).min(BINS - 1)] += 1;
                if (0.2..=0.8).contains(&l) {
                    for (sum, c) in self.mid_sum.iter_mut().zip(rgb) {
                        *sum += f64::from(c);
                    }
                    self.mid_n += 1;
                }
            }
        }
    }

    fn percentile(&self, p: f64, total: u64) -> f32 {
        let want = (p * total as f64).ceil() as u64;
        let mut seen = 0;
        for (i, &n) in self.luma.iter().enumerate() {
            seen += n;
            if seen >= want.max(1) {
                return i as f32 / (BINS - 1) as f32;
            }
        }
        1.0
    }

    /// `None` if no pixel was added.
    pub fn stats(&self) -> Option<Stats> {
        let total: u64 = self.luma.iter().sum();
        if total == 0 {
            return None;
        }
        let mid_mean = if self.mid_n > 0 {
            self.mid_sum.map(|s| (s / self.mid_n as f64) as f32)
        } else {
            [0.5; 3]
        };
        Some(Stats {
            black: self.percentile(0.005, total),
            median: self.percentile(0.5, total),
            white: self.percentile(0.995, total),
            mid_mean,
        })
    }
}

/// The grade settings (`[grade]` in the config).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub auto_exposure: bool,
    pub exposure_amount: f32,
    pub auto_white_bal: bool,
    pub wb_amount: f32,
    pub contrast: f32,
    pub vibrance: f32,
    pub look_strength: f32,
}

/// Target levels after correction.
pub const TARGET_BLACK: f32 = 0.03;
pub const TARGET_WHITE: f32 = 0.95;
pub const TARGET_MEDIAN: f32 = 0.42;
/// Below this median a clip is low-key (night maps, dark basements): it stays dark.
pub const LOW_KEY: f32 = 0.18;
/// White balance gains stay within ±6 %.
pub const MAX_WB: f32 = 0.06;

/// The per-clip exposure and colour correction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Correction {
    /// Input black and white points, mapped to the targets.
    pub black: f32,
    pub white: f32,
    /// Applied after the levels; 1 = none.
    pub gamma: f32,
    /// How much of the levels and gamma to apply, 0–1.
    pub amount: f32,
    pub gains: [f32; 3],
}

impl Correction {
    pub const NONE: Correction = Correction {
        black: 0.0,
        white: 1.0,
        gamma: 1.0,
        amount: 0.0,
        gains: [1.0; 3],
    };

    fn levels(&self, x: f32) -> f32 {
        let span = (self.white - self.black).max(1e-3);
        let l = TARGET_BLACK + (x - self.black) / span * (TARGET_WHITE - TARGET_BLACK);
        l.clamp(0.0, 1.0).powf(self.gamma)
    }

    /// Applies the correction to one colour.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|k| {
            let x = rgb[k];
            let leveled = x + (self.levels(x) - x) * self.amount;
            (leveled * self.gains[k]).clamp(0.0, 1.0)
        })
    }
}

/// Derives the correction from a clip's stats.
pub fn correction(s: &Stats, p: &Params) -> Correction {
    let mut c = Correction::NONE;
    if p.auto_exposure && p.exposure_amount > 0.0 {
        c.black = s.black;
        c.white = s.white.max(s.black + 0.05);
        c.amount = p.exposure_amount.clamp(0.0, 1.0);
        // Where the median lands after the levels alone, and where it should go.
        let span = c.white - c.black;
        let m = (TARGET_BLACK + (s.median - c.black) / span * (TARGET_WHITE - TARGET_BLACK))
            .clamp(0.01, 0.99);
        let target = if s.median < LOW_KEY {
            // Low-key guard: lift a dark clip only a little, never to mid-grey.
            (s.median + 0.05).min(TARGET_MEDIAN)
        } else {
            TARGET_MEDIAN
        };
        c.gamma = (target.ln() / m.ln()).clamp(0.85, 1.15);
    }
    if p.auto_white_bal && p.wb_amount > 0.0 {
        let mean = s.mid_mean;
        let grey = (mean[0] + mean[1] + mean[2]) / 3.0;
        c.gains = mean.map(|m| {
            let full = if m > 1e-3 { grey / m } else { 1.0 };
            (1.0 + (full - 1.0) * p.wb_amount.clamp(0.0, 1.0)).clamp(1.0 - MAX_WB, 1.0 + MAX_WB)
        });
    }
    c
}

/// A gentle S-curve that keeps 0, 1 and the pivot fixed; `contrast` 1.0 is no change.
pub fn contrast_curve(x: f32, contrast: f32) -> f32 {
    let p = TARGET_MEDIAN;
    let x = x.clamp(0.0, 1.0);
    if x <= p {
        p * (x / p).powf(contrast)
    } else {
        1.0 - (1.0 - p) * ((1.0 - x) / (1.0 - p)).powf(contrast)
    }
}

/// Raises saturation in inverse proportion to how saturated a colour already is.
pub fn vibrance(rgb: [f32; 3], amount: f32) -> [f32; 3] {
    if amount == 0.0 {
        return rgb;
    }
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let sat = if max > 1e-4 { (max - min) / max } else { 0.0 };
    let factor = 1.0 + amount * (1.0 - sat);
    let l = luma(rgb);
    rgb.map(|c| (l + (c - l) * factor).clamp(0.0, 1.0))
}

/// Bakes correction → contrast → vibrance → look into one `size`-point LUT. The look is blended
/// with what it was applied to by `look_strength`.
pub fn compose(c: &Correction, p: &Params, look: Option<&Cube>, size: usize) -> Cube {
    Cube::from_fn(size, |rgb| {
        let x = c.apply(rgb);
        let x = x.map(|v| contrast_curve(v, p.contrast));
        let x = vibrance(x, p.vibrance);
        match look {
            Some(l) if p.look_strength > 0.0 => {
                let y = l.sample(x);
                let s = p.look_strength.clamp(0.0, 1.0);
                std::array::from_fn(|k| x[k] + (y[k] - x[k]) * s)
            }
            _ => x,
        }
    })
}

/// Retake's built-in Siege look: a little more contrast, a vibrance-style colour boost,
/// slightly cooler shadows and warmer highlights, and a soft shoulder so highlights don't clip.
/// Because every clip arrives normalised, it can be applied at full strength.
pub fn builtin_look(size: usize) -> Cube {
    Cube::from_fn(size, |rgb| {
        let x = rgb.map(|v| contrast_curve(v, 1.06));
        let x = vibrance(x, 0.12);
        let l = luma(x);
        // Split toning: weights fall off towards the midtones.
        let shadow = (1.0 - l / 0.45).clamp(0.0, 1.0) * 0.025;
        let high = ((l - 0.55) / 0.45).clamp(0.0, 1.0) * 0.02;
        let toned = [
            x[0] - shadow + high,
            x[1] + high * 0.4,
            x[2] + shadow - high,
        ];
        // Soft shoulder above 0.9.
        toned.map(|v| {
            let v = v.max(0.0);
            if v <= 0.9 {
                v
            } else {
                0.9 + 0.1 * (1.0 - (-(v - 0.9) / 0.1).exp())
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: Params = Params {
        auto_exposure: true,
        exposure_amount: 0.0,
        auto_white_bal: true,
        wb_amount: 0.0,
        contrast: 1.0,
        vibrance: 0.0,
        look_strength: 0.0,
    };

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= eps)
    }

    #[test]
    fn cube_round_trips_exactly() {
        let c = builtin_look(17);
        let back = Cube::parse(&c.write("test")).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn cube_parse_rejects_wrong_counts_and_domains() {
        assert!(Cube::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
        assert!(Cube::parse("LUT_3D_SIZE 2\nDOMAIN_MAX 2 2 2\n").is_err());
        assert!(Cube::parse("0 0 0").is_err());
        let ok = Cube::identity(2).write("x");
        assert!(Cube::parse(&format!("# comment\n{ok}")).is_ok());
    }

    #[test]
    fn tetrahedral_sample_of_identity_is_identity() {
        let id = Cube::identity(5);
        for rgb in [
            [0.1, 0.7, 0.33],
            [0.9, 0.2, 0.5],
            [0.0, 1.0, 0.25],
            [0.6, 0.6, 0.6],
        ] {
            assert!(close(id.sample(rgb), rgb, 1e-5), "{rgb:?}");
        }
        // Grid points are exact on any LUT.
        let look = builtin_look(5);
        assert!(close(look.sample([0.25, 0.5, 1.0]), look.at(1, 2, 4), 1e-6));
    }

    #[test]
    fn correction_is_identity_at_amount_zero() {
        let s = Stats {
            black: 0.1,
            median: 0.3,
            white: 0.8,
            mid_mean: [0.5, 0.4, 0.3],
        };
        let c = correction(&s, &OFF);
        let lut = compose(&c, &OFF, None, 9);
        let id = Cube::identity(9);
        for (a, b) in lut.data.iter().zip(&id.data) {
            assert!(close(*a, *b, 1e-6));
        }
    }

    fn on() -> Params {
        Params {
            exposure_amount: 1.0,
            wb_amount: 1.0,
            ..OFF
        }
    }

    #[test]
    fn low_key_clip_stays_dark() {
        let s = Stats {
            black: 0.01,
            median: 0.08,
            white: 0.7,
            mid_mean: [0.3; 3],
        };
        let c = correction(&s, &on());
        let m = c.apply([0.08; 3])[0];
        assert!(m < 0.2, "night median went to {m}");
        // A normal clip's median goes towards 0.42.
        let s = Stats { median: 0.3, ..s };
        let c = correction(&s, &on());
        assert!(c.apply([0.3; 3])[0] > 0.33);
    }

    #[test]
    fn wb_gains_never_exceed_six_percent() {
        let s = Stats {
            black: 0.0,
            median: 0.4,
            white: 1.0,
            mid_mean: [0.7, 0.3, 0.2],
        };
        let c = correction(&s, &on());
        for g in c.gains {
            assert!(
                (1.0 - MAX_WB - 1e-6..=1.0 + MAX_WB + 1e-6).contains(&g),
                "{g}"
            );
        }
        // Red is too strong, so its gain is the lowest.
        assert!(c.gains[0] < c.gains[1] && c.gains[0] < c.gains[2]);
    }

    #[test]
    fn contrast_keeps_ends_and_pivot() {
        for c in [1.0, 1.12, 1.3] {
            assert!((contrast_curve(0.0, c)).abs() < 1e-6);
            assert!((contrast_curve(1.0, c) - 1.0).abs() < 1e-6);
            assert!((contrast_curve(TARGET_MEDIAN, c) - TARGET_MEDIAN).abs() < 1e-6);
        }
        assert!(contrast_curve(0.2, 1.2) < 0.2);
        assert!(contrast_curve(0.7, 1.2) > 0.7);
    }

    #[test]
    fn vibrance_favours_muted_colours() {
        let muted = [0.5, 0.45, 0.4];
        let vivid = [0.9, 0.1, 0.1];
        let spread = |c: [f32; 3]| c[0] - c[2];
        let gain_muted = spread(vibrance(muted, 0.5)) / spread(muted);
        let gain_vivid = spread(vibrance(vivid, 0.5)) / spread(vivid);
        assert!(gain_muted > gain_vivid);
        assert_eq!(vibrance(muted, 0.0), muted);
    }

    #[test]
    fn histogram_percentiles_and_mask() {
        let (w, h) = (100, 10);
        let mut px = vec![0u8; w * h * 3];
        // Left half black, right half mid-grey; the mask hides the right 10 %.
        for y in 0..h {
            for x in 50..w {
                let i = (y * w + x) * 3;
                px[i..i + 3].copy_from_slice(&[128, 128, 128]);
            }
            for x in 90..w {
                let i = (y * w + x) * 3;
                px[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let mut hist = Histogram::default();
        hist.add_rgb24(
            &px,
            w,
            h,
            &[Rect {
                x: 0.9,
                y: 0.0,
                w: 0.1,
                h: 1.0,
            }],
        );
        let s = hist.stats().unwrap();
        assert_eq!(s.black, 0.0);
        assert!(
            (s.white - 0.5).abs() < 0.01,
            "white pixels were masked: {}",
            s.white
        );
        assert!((s.mid_mean[0] - 128.0 / 255.0).abs() < 1e-3);
        assert!(Histogram::default().stats().is_none());
    }

    #[test]
    fn builtin_look_keeps_highlights_unclipped_and_shadows_cool() {
        let look = builtin_look(33);
        let top = look.sample([1.0, 1.0, 1.0]);
        assert!(top.iter().all(|&v| v < 1.0), "{top:?}");
        let shadow = look.sample([0.1, 0.1, 0.1]);
        assert!(shadow[2] > shadow[0], "{shadow:?}");
        let high = look.sample([0.8, 0.8, 0.8]);
        assert!(high[0] > high[2], "{high:?}");
    }
}
