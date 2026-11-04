use indicatif::{ProgressBar, ProgressStyle};
use console::Term;
use anyhow::Result;

/// Width of the gradient bar in cells, matching the existing `{bar:40}` template
/// so the total line length is unchanged.
const GRADIENT_BAR_WIDTH: usize = 40;

/// D65 reference white used by the CIE L*u*v* conversions (matches go-colorful).
const D65: (f64, f64, f64) = (0.95047, 1.0, 1.08883);

/// A gradient progress bar rendered as per-cell truecolor ANSI, blending between
/// two hex colors in CIE L*u*v* space (matching trzsz-go's `textProgressBar`,
/// which uses go-colorful's `BlendLuv`).
struct GradientBar {
    a: (f64, f64, f64),
    b: (f64, f64, f64),
}

pub struct TransferProgress {
    pb: ProgressBar,
    quiet: bool,
    gradient: Option<GradientBar>,
}

impl TransferProgress {
    pub fn new(total: u64, quiet: bool) -> Self {
        Self::new_with_color(total, quiet, None, None)
    }

    pub fn new_with_file(total: u64, quiet: bool, file_name: &str) -> Self {
        Self::new_with_color(total, quiet, Some(file_name.to_string()), None)
    }

    /// Builds a progress bar. When `color_pair` is `Some((hex_a, hex_b))` (two
    /// hex colors without leading `#`, e.g. `("B14FFF", "00FFA3")`) and both
    /// parse, the bar is rendered as a BlendLuv gradient with truecolor ANSI;
    /// otherwise the existing indicatif 2-color bar is used.
    pub fn new_with_color(
        total: u64,
        quiet: bool,
        file_name: Option<String>,
        color_pair: Option<(String, String)>,
    ) -> Self {
        let pb = ProgressBar::new(total);
        let mut gradient = None;

        if !quiet {
            if let Some((a, b)) = color_pair {
                if let (Some(a), Some(b)) = (hex_to_rgb(&a), hex_to_rgb(&b)) {
                    let template = match &file_name {
                        Some(name) => format!(
                            "{{spinner:.green}} {} [{{elapsed_precise}}] {{msg}} {{bytes}}/{{total_bytes}} ({{bytes_per_sec}}) ({{eta}})",
                            name
                        ),
                        None => "{spinner:.green} [{elapsed_precise}] {msg} {bytes}/{total_bytes} ({bytes_per_sec}) ({eta})".to_string(),
                    };
                    if let Ok(style) = ProgressStyle::default_bar().template(&template) {
                        pb.set_style(style);
                        gradient = Some(GradientBar { a, b });
                    }
                }
            }
        }

        if gradient.is_none() && !quiet {
            // Existing indicatif bar (2-color cyan/blue), unchanged default.
            if let Some(name) = &file_name {
                pb.set_style(ProgressStyle::default_bar()
                    .template(&format!("{{spinner:.green}} {} [{{elapsed_precise}}] [{{bar:40.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{bytes_per_sec}}) ({{eta}})", name))
                    .unwrap()
                    .progress_chars("=>-"));
            } else {
                pb.set_style(ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) ({eta})")
                    .unwrap()
                    .progress_chars("=>-"));
            }
        }

        if let Some(g) = &gradient {
            pb.set_message(render_gradient_bar(g, 0, total));
        }

        Self { pb, quiet, gradient }
    }

    pub fn update(&self, current: u64) {
        self.pb.set_position(current);
        if let Some(g) = &self.gradient {
            let total = self.pb.length().unwrap_or(0);
            self.pb.set_message(render_gradient_bar(g, current, total));
        }
    }

    pub fn finish(&self, message: &str) {
        if self.quiet {
            self.pb.finish_and_clear();
        } else {
            self.pb.finish_with_message(message.to_string());
        }
    }

    pub fn finish_with_error(&self, message: &str) {
        if self.quiet {
            self.pb.finish_and_clear();
        } else {
            self.pb.abandon_with_message(message.to_string());
        }
    }
}

/// Renders `[<gradient cells><empty>]` at the given position, matching
/// trzsz-go's `getProgressBar`: `full = round(width * pos / total)` cells are
/// colored with `BlendLuv(a, b, i/width)` and the rest are `0x2591` light
/// shade blocks.
fn render_gradient_bar(g: &GradientBar, pos: u64, total: u64) -> String {
    let total_size = GRADIENT_BAR_WIDTH;
    let full_size = if total == 0 {
        total_size
    } else {
        ((total_size as f64 * pos as f64 / total as f64).round() as usize).min(total_size)
    };
    let empty_size = total_size - full_size;

    let mut out = String::with_capacity(2 + full_size * 20 + empty_size);
    out.push('[');
    for i in 0..full_size {
        let t = i as f64 / total_size as f64;
        let (r, gr, b) = blend_luv(g.a, g.b, t);
        use std::fmt::Write;
        write!(out, "\x1b[38;2;{};{};{}m\u{2588}\x1b[0m", r, gr, b).unwrap();
    }
    for _ in 0..empty_size {
        out.push('\u{2591}');
    }
    out.push(']');
    out
}

/// Parses a hex color (`#rrggbb` or `#rgb`, leading `#` optional) into RGB in
/// [0, 1]. Returns `None` for anything else (matching go-colorful's `Hex`).
fn hex_to_rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let hex = hex.trim_start_matches('#');
    let vals: [u8; 3] = match hex.len() {
        3 => {
            let mut out = [0u8; 3];
            for (i, c) in hex.chars().enumerate() {
                if i >= 3 {
                    break;
                }
                out[i] = c.to_digit(16)? as u8;
            }
            out
        }
        6 => {
            let parse = |s: &str| -> Option<u8> { u8::from_str_radix(s, 16).ok() };
            [parse(&hex[0..2])?, parse(&hex[2..4])?, parse(&hex[4..6])?]
        }
        _ => return None,
    };
    let factor = if hex.len() == 3 { 1.0 / 15.0 } else { 1.0 / 255.0 };
    Some((
        vals[0] as f64 * factor,
        vals[1] as f64 * factor,
        vals[2] as f64 * factor,
    ))
}

/// Blends two RGB colors at `t` (0 → a, 1 → b) in CIE L*u*v* space,
/// reproducing go-colorful's `Color.BlendLuv` exactly (sRGB → linear RGB →
/// XYZ → Luv, interpolate, back).
fn blend_luv(a: (f64, f64, f64), b: (f64, f64, f64), t: f64) -> (u8, u8, u8) {
    fn to_luv(rgb: (f64, f64, f64)) -> (f64, f64, f64) {
        let (r, g, b) = (linearize(rgb.0), linearize(rgb.1), linearize(rgb.2));
        let (x, y, z) = linear_rgb_to_xyz(r, g, b);
        xyz_to_luv(x, y, z)
    }
    let (l1, u1, v1) = to_luv(a);
    let (l2, u2, v2) = to_luv(b);
    let (l, u, v) = (
        l1 + t * (l2 - l1),
        u1 + t * (u2 - u1),
        v1 + t * (v2 - v1),
    );
    let (x, y, z) = luv_to_xyz(l, u, v);
    let (r, g, bb) = xyz_to_linear_rgb(x, y, z);
    let to_u8 = |v: f64| {
        // Match go-colorful's Hex(): round via +0.5, then truncate; clamp to
        // [0, 255] for tiny out-of-gamut overshoot.
        ((delinearize(v) * 255.0 + 0.5).floor().clamp(0.0, 255.0)) as u8
    };
    (to_u8(r), to_u8(g), to_u8(bb))
}

fn linearize(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn delinearize(v: f64) -> f64 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_rgb_to_xyz(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    (
        0.41239079926595948 * r + 0.35758433938387796 * g + 0.18048078840183429 * b,
        0.21263900587151036 * r + 0.71516867876775593 * g + 0.072192315360733715 * b,
        0.019330818715591851 * r + 0.11919477979462599 * g + 0.95053215224966058 * b,
    )
}

fn xyz_to_linear_rgb(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    (
        3.2409699419045214 * x - 1.5373831775700935 * y - 0.49861076029300328 * z,
        -0.96924363628087983 * x + 1.8759675015077207 * y + 0.041555057407175613 * z,
        0.055630079696993609 * x - 0.20397695888897657 * y + 1.0569715142428786 * z,
    )
}

fn xyz_to_uv(x: f64, y: f64, z: f64) -> (f64, f64) {
    let denom = x + 15.0 * y + 3.0 * z;
    if denom == 0.0 {
        (0.0, 0.0)
    } else {
        (4.0 * x / denom, 9.0 * y / denom)
    }
}

fn xyz_to_luv(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let yw = y / D65.1;
    let l = if yw <= (6.0_f64 / 29.0_f64).powi(3) {
        yw * (29.0_f64 / 3.0_f64).powi(3) / 100.0
    } else {
        1.16 * yw.cbrt() - 0.16
    };
    let (ubis, vbis) = xyz_to_uv(x, y, z);
    let (un, vn) = xyz_to_uv(D65.0, D65.1, D65.2);
    (l, 13.0 * l * (ubis - un), 13.0 * l * (vbis - vn))
}

fn luv_to_xyz(l: f64, u: f64, v: f64) -> (f64, f64, f64) {
    let y = if l <= 0.08 {
        D65.1 * l * 100.0 * (3.0_f64 / 29.0_f64).powi(3)
    } else {
        D65.1 * ((l + 0.16) / 1.16).powi(3)
    };
    let (un, vn) = xyz_to_uv(D65.0, D65.1, D65.2);
    if l != 0.0 {
        let ubis = u / (13.0 * l) + un;
        let vbis = v / (13.0 * l) + vn;
        let x = y * 9.0 * ubis / (4.0 * vbis);
        let z = y * (12.0 - 3.0 * ubis - 20.0 * vbis) / (4.0 * vbis);
        (x, y, z)
    } else {
        (0.0, 0.0, 0.0)
    }
}

pub fn print_transfer_result(file_name: &str, success: bool, size: u64) -> Result<()> {
    let term = Term::stdout();

    if success {
        let msg = format!("{}: {} bytes transferred successfully", file_name, size);
        term.write_line(&msg)?;
    } else {
        let msg = format!("{}: transfer failed", file_name);
        term.write_line(&msg)?;
    }

    Ok(())
}
