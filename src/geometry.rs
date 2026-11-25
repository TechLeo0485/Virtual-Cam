//! Rotation, crop and fit geometry shared by the worker, the compositor and the UI.

use crate::util::py_round;
use anyhow::{bail, Result};

/// 1 = actual size (the full camera frame fits the output; a crop keeps its size);
/// >1 shrinks the camera image, <1 enlarges it.
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 2.0;

/// Validate a rotation in degrees clockwise (0/90/180/270).
pub fn normalize_rotation(deg: i64) -> Result<u32> {
    let d = deg.rem_euclid(360);
    match d {
        0 | 90 | 180 | 270 => Ok(d as u32),
        _ => bail!("rotation must be 0, 90, 180 or 270"),
    }
}

/// (w, h) after rotating a frame by `rot` degrees (90/270 swap them).
pub fn rotated_size(w: usize, h: usize, rot: u32) -> (usize, usize) {
    if rot == 90 || rot == 270 {
        (h, w)
    } else {
        (w, h)
    }
}

/// Where a `cw x ch` camera image lands inside an `ow x oh` canvas: returns
/// (x, y, w, h). Wide cameras in a taller canvas are anchored to the bottom so the
/// torso runs off the bottom edge. With zoom < 1 the rectangle is larger than the
/// canvas, so x / y can be negative.
///
/// The scale is the one that makes a `ref_w x ref_h` frame FIT the canvas, divided by
/// `zoom`. Pass the full camera frame as the reference and a crop of it keeps exactly
/// the size it has in the full frame (cropping only drops pixels, it never enlarges
/// what is left); pass the image itself to fit that image.
#[allow(clippy::too_many_arguments)]
pub fn fit_rect(
    cw: usize,
    ch: usize,
    (ref_w, ref_h): (usize, usize),
    ow: usize,
    oh: usize,
    bottom: bool,
    zoom: f32,
) -> (i64, i64, i64, i64) {
    let (cwf, chf, owf, ohf) = (cw.max(1) as f64, ch.max(1) as f64, ow as f64, oh as f64);
    let (rwf, rhf) = (ref_w.max(1) as f64, ref_h.max(1) as f64);
    // zoom 1 = the reference frame fits; z>1 smaller, z<1 larger
    let s = (owf / rwf).min(ohf / rhf) / (zoom as f64).max(0.25);
    let w = py_round(cwf * s).max(1);
    let h = py_round(chf * s).max(1);
    let x = (ow as i64 - w).div_euclid(2);
    // middle-bottom points coincide
    let y = if bottom { oh as i64 - h } else { (oh as i64 - h).div_euclid(2) };
    (x, y, w, h)
}

/// Normalized crop rectangle (x0, y0, x1, y1, each 0..1 of the frame) -> pixel box
/// (x0, y0, x1, y1) inside a `w x h` frame, or `None` for "use the whole frame".
pub fn crop_pixels(crop: Option<[f32; 4]>, w: usize, h: usize, min_size: usize) -> Option<(usize, usize, usize, usize)> {
    let c = crop?;
    if c.iter().any(|t| !t.is_finite()) || w == 0 || h == 0 {
        return None;
    }
    let cl = |t: f32| (t as f64).clamp(0.0, 1.0);
    let (mut u0, mut u1) = (cl(c[0]), cl(c[2]));
    let (mut v0, mut v1) = (cl(c[1]), cl(c[3]));
    if u0 > u1 {
        std::mem::swap(&mut u0, &mut u1);
    }
    if v0 > v1 {
        std::mem::swap(&mut v0, &mut v1);
    }
    let (wi, hi) = (w as i64, h as i64);
    let mut x0 = py_round(u0 * w as f64);
    let mut x1 = py_round(u1 * w as f64);
    let mut y0 = py_round(v0 * h as f64);
    let mut y1 = py_round(v1 * h as f64);
    let min_w = (min_size as i64).min(wi);
    let min_h = (min_size as i64).min(hi);
    if x1 - x0 < min_w {
        // never smaller than min_size px
        x0 = x0.min(wi - min_w);
        x1 = x0 + min_w;
    }
    if y1 - y0 < min_h {
        y0 = y0.min(hi - min_h);
        y1 = y0 + min_h;
    }
    if (x0, y0, x1, y1) == (0, 0, wi, hi) {
        return None;
    }
    Some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_same_size() {
        assert_eq!(fit_rect(1280, 720, (1280, 720), 1280, 720, true, 1.0), (0, 0, 1280, 720));
    }

    #[test]
    fn fit_zoom_out_is_bottom_centred() {
        assert_eq!(fit_rect(1280, 720, (1280, 720), 1280, 720, true, 2.0), (320, 360, 640, 360));
    }

    #[test]
    fn fit_zoom_in_overflows() {
        assert_eq!(fit_rect(1280, 720, (1280, 720), 1280, 720, true, 0.5), (-640, -720, 2560, 1440));
    }

    #[test]
    fn crop_keeps_its_size_from_the_full_frame() {
        // a 400x300 crop of a 1280x720 camera, 1280x720 output: placed 1:1, bottom centre
        assert_eq!(fit_rect(400, 300, (1280, 720), 1280, 720, true, 1.0), (440, 420, 400, 300));
        // the same with a 640x360 capture: the full frame is scaled 2x, so is the crop
        assert_eq!(fit_rect(200, 150, (640, 360), 1280, 720, true, 1.0), (440, 420, 400, 300));
        // zoom 0.5 doubles it
        assert_eq!(fit_rect(400, 300, (1280, 720), 1280, 720, true, 0.5), (240, 120, 800, 600));
    }

    #[test]
    fn crop_full_frame_is_none() {
        assert_eq!(crop_pixels(Some([0.0, 0.0, 1.0, 1.0]), 640, 480, 16), None);
        assert_eq!(crop_pixels(None, 640, 480, 16), None);
        assert_eq!(crop_pixels(Some([0.25, 0.5, 0.75, 1.0]), 640, 480, 16), Some((160, 240, 480, 480)));
    }

    #[test]
    fn rotation() {
        assert_eq!(normalize_rotation(450).unwrap(), 90);
        assert!(normalize_rotation(45).is_err());
        assert_eq!(rotated_size(4, 3, 270), (3, 4));
    }
}
