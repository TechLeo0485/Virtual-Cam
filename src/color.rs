//! Dell-monitor style colour management.
//!
//! Everyday colour dimensions, all on a -100 ... +100 scale (0 = neutral):
//!   brightness  darker <-> lighter       (1 unit = 1 level of 0-255)
//!   contrast    flatter <-> punchier     (factor 1 + value/100 around mid-grey)
//!   warmth      cooler (blue) <-> warmer (orange); brightness-neutral
//!   tint        greener (-) <-> magenta (+); brightness-neutral
//!   saturation  paler <-> more vivid     (factor 1 + value/100 away from grey)

use crate::imgproc::Bgr;
use rayon::prelude::*;

pub const COLOR_KEYS: [&str; 5] = ["brightness", "contrast", "warmth", "tint", "saturation"];
pub const COLOR_LABELS: [&str; 5] = ["Brightness", "Contrast", "Warmth", "Tint", "Saturation"];

const WARMTH_K: f64 = 0.30; // log colour-gain at warmth +/-100 (red x1.35, blue x0.74, before luma fix)
const TINT_K: f64 = 0.10; // log colour-gain at tint   +/-100
const LUMA_BGR: [f64; 3] = [0.114, 0.587, 0.299];

pub struct Preset {
    pub name: &'static str,
    /// brightness, contrast, warmth, tint, saturation
    pub values: [i32; 5],
    /// "Custom Color": the values come from the sliders instead.
    pub custom: bool,
}

/// Factory presets. The values were converted from the original Dell presets (per-channel
/// R/G/B gains + contrast + brightness offset), so each preset looks as it did before.
pub const DELL_PRESETS: [Preset; 8] = [
    Preset { name: "Standard", values: [0, 0, 0, 0, 0], custom: false },
    // Low blue light: warmer, slightly lower contrast, darker.
    Preset { name: "ComfortView", values: [-13, -6, 50, -23, 0], custom: false },
    // Red / yellow bias.
    Preset { name: "Warm", values: [1, 1, 34, -8, 0], custom: false },
    // Blue tint bias.
    Preset { name: "Cool", values: [-2, -2, -33, -2, 0], custom: false },
    // High impact: boosted contrast, brighter, slightly cool highlights.
    Preset { name: "Movie / Cinema", values: [25, 16, -5, 11, 0], custom: false },
    // Lift shadows, punchy: high contrast and a higher brightness lift.
    Preset { name: "Game", values: [44, 26, 0, 0, 0], custom: false },
    // Balanced: moderate contrast and brightness boost.
    Preset { name: "Multimedia", values: [18, 10, 0, 0, 0], custom: false },
    // Manual sliders (see apply_dell_color).
    Preset { name: "Custom Color", values: [0, 0, 0, 0, 0], custom: true },
];

pub const DEFAULT_COLOR_PRESET: &str = "Standard";
pub const CUSTOM_COLOR_PRESET: &str = "Custom Color";

pub fn preset(name: &str) -> Option<&'static Preset> {
    DELL_PRESETS.iter().find(|p| p.name == name)
}

/// 3x3 matrix (BGR in -> BGR out): warmth / tint gains (brightness-neutral), then saturation.
fn color_matrix(warmth: i32, tint: i32, saturation: i32) -> [[f32; 3]; 3] {
    let a = WARMTH_K * warmth as f64 / 100.0;
    let b = TINT_K * tint as f64 / 100.0;
    // log gains for (R, G, B): warm = red up / blue down, tint+ = red+blue up / green down
    let g_r = (a + b).exp();
    let g_g = (-2.0 * b).exp();
    let g_b = (-a + b).exp();
    let norm = 0.299 * g_r + 0.587 * g_g + 0.114 * g_b; // keep the luma unchanged
    let gains = [g_b / norm, g_g / norm, g_r / norm]; // B, G, R order
    let s = (1.0 + saturation as f64 / 100.0).max(0.0);
    let mut m = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            // blend each pixel with its grey, then apply the per-channel gain
            let sat = if i == j { s } else { 0.0 } + (1.0 - s) * LUMA_BGR[j];
            m[i][j] = (sat * gains[j]) as f32;
        }
    }
    m
}

/// Colour-manage a BGR frame in place like a monitor preset.
///
/// 1. Mode check: a factory preset supplies its own five values; "Custom Color" uses
///    `custom` (each clamped to -100 ... +100).
/// 2. Brightness + contrast in ONE operation (skipped when both are 0): contrast pivots
///    on mid-grey, brightness adds levels.
/// 3. Warmth / tint / saturation in ONE 3x3 colour transform (skipped when all are 0).
/// 4. An unknown preset name leaves the frame unmodified.
pub fn apply_dell_color(frame: &mut Bgr, preset_name: &str, custom: [i32; 5]) {
    let spec = match preset(preset_name) {
        Some(s) => s,
        None => return, // 4. bypass fallback
    };
    let v = if spec.custom {
        [
            custom[0].clamp(-100, 100),
            custom[1].clamp(-100, 100),
            custom[2].clamp(-100, 100),
            custom[3].clamp(-100, 100),
            custom[4].clamp(-100, 100),
        ]
    } else {
        spec.values
    };
    let (br, co, wa, ti, sa) = (v[0], v[1], v[2], v[3], v[4]);
    let tone = br != 0 || co != 0;
    let colour = wa != 0 || ti != 0 || sa != 0;
    if !tone && !colour {
        return;
    }
    let mut lut = [0u8; 256];
    let c = 1.0 + co as f32 / 100.0;
    let offset = 128.0 * (1.0 - c) + br as f32;
    for (i, l) in lut.iter_mut().enumerate() {
        *l = if tone { (i as f32 * c + offset).round().clamp(0.0, 255.0) as u8 } else { i as u8 };
    }
    let m = color_matrix(wa, ti, sa);
    let row_len = frame.w.max(1) * 3;
    frame.data.par_chunks_mut(row_len).for_each(|row| {
        for px in row.chunks_exact_mut(3) {
            let b = lut[px[0] as usize];
            let g = lut[px[1] as usize];
            let r = lut[px[2] as usize];
            if colour {
                let (bf, gf, rf) = (b as f32, g as f32, r as f32);
                px[0] = (m[0][0] * bf + m[0][1] * gf + m[0][2] * rf).round().clamp(0.0, 255.0) as u8;
                px[1] = (m[1][0] * bf + m[1][1] * gf + m[1][2] * rf).round().clamp(0.0, 255.0) as u8;
                px[2] = (m[2][0] * bf + m[2][1] * gf + m[2][2] * rf).round().clamp(0.0, 255.0) as u8;
            } else {
                px[0] = b;
                px[1] = g;
                px[2] = r;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_is_identity() {
        let mut f = Bgr::filled(4, 2, (10, 120, 250));
        apply_dell_color(&mut f, "Standard", [50, 50, 50, 50, 50]);
        assert_eq!(&f.data[..3], &[10, 120, 250]);
    }

    #[test]
    fn warmth_keeps_grey_luma() {
        let mut f = Bgr::filled(2, 2, (128, 128, 128));
        apply_dell_color(&mut f, "Custom Color", [0, 0, 100, 0, 0]);
        let (b, g, r) = (f.data[0] as f64, f.data[1] as f64, f.data[2] as f64);
        assert!(r > b);
        let luma = 0.114 * b + 0.587 * g + 0.299 * r;
        assert!((luma - 128.0).abs() < 2.0);
    }
}
