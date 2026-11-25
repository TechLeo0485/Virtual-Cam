//! Alpha-blends the person over a background using a precomputed alpha matte.

use crate::background::resize_cover;
use crate::geometry::{fit_rect, ZOOM_MAX, ZOOM_MIN};
use crate::imgproc::{self, Bgr, Interp};
use crate::util::py_round;
use rayon::prelude::*;

/// Highest value of the matte threshold slider.
pub const THRESHOLD_MAX: f32 = 0.9;

/// u8 matte remap for threshold `t`, or `None` for 0 (off).
fn threshold_lut(t: f32) -> Option<[u8; 256]> {
    if t <= 0.0 {
        return None;
    }
    let lo = (t - 0.1).max(0.0);
    let hi = (t + 0.1).min(1.0);
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let a = i as f32 / 255.0;
        let m = ((a - lo) / (hi - lo)).clamp(0.0, 1.0);
        *v = (m * 255.0 + 0.5) as u8;
    }
    Some(lut)
}

/// Hot path uses integer math (`out = bg + (fg - bg) * alpha / 255`) and reused scratch
/// buffers so nothing large is reallocated per frame.
///
/// Blur mode blurs a quarter-size copy and upscales it; blur is low-frequency so the
/// result is visually identical to a full-size blur at a fraction of the cost.
///
/// The matte is blended as RVM made it: RVM already returns a soft, anti-aliased edge, so
/// the extra Gaussian feather the MediaPipe masks needed only blurred it a second time
/// (and cost 3.6 ms of CPU per 720p frame). An optional threshold (`set_threshold`)
/// pushes uncertain, half-transparent matte values to solid person or background.
///
/// The source frame may be smaller than `cam_size`: the worker shrinks it to the size it
/// is shown at before matting, and `render` maps the visible region accordingly.
pub struct Compositor {
    /// OUTPUT canvas size.
    pub w: usize,
    pub h: usize,
    background: Option<Bgr>, // None in blur mode
    blur_bg: bool,
    /// When the camera frame differs from the output size it is scaled to FIT (never
    /// clipped) at `rect` = (x, y, w, h); the rest shows the background.
    rect: Option<(usize, usize, usize, usize)>,
    zoom: f32,
    cam_size: (usize, usize),
    /// The frame whose FIT sets the scale (see `geometry::fit_rect`): the full camera
    /// frame, so a crop keeps its size instead of being blown up to fill the output.
    scale_ref: (usize, usize),
    src: (usize, usize, usize, usize),
    interp: Interp,
    fgc: Vec<u8>,      // canvas-sized foreground
    alc: Vec<u8>,      // canvas-sized alpha (0 outside)
    alpha_u8: Vec<u8>, // scratch
    crop_alpha: Vec<u8>,
    out: Bgr,
    bw: usize,
    bh: usize,
    blur_sigma: f64,
    /// 0 = off; else the matte value that becomes the person / background boundary
    threshold: f32,
    /// matte remap for `threshold` (None = identity)
    lut: Option<[u8; 256]>,
}

impl Compositor {
    /// `background` is `None` (with `blur_bg = true`) for the live-blur mode.
    pub fn new(
        width: usize,
        height: usize,
        background: Option<Bgr>,
        blur_bg: bool,
        blur_strength: f32,
        cam_size: (usize, usize),
    ) -> Self {
        let mut c = Compositor {
            w: width,
            h: height,
            background,
            blur_bg,
            rect: None,
            zoom: 1.0,
            cam_size,
            scale_ref: cam_size,
            src: (0, 0, cam_size.0, cam_size.1),
            interp: Interp::Linear,
            fgc: vec![0u8; width * height * 3],
            alc: vec![0u8; width * height],
            alpha_u8: vec![0u8; width * height],
            crop_alpha: Vec::new(),
            out: Bgr::new(width, height),
            // Downscaled blur working size (>= 160x90).
            bw: (width / 4).max(160),
            bh: (height / 4).max(90),
            // Blur intensity (blur mode only); 1.0 reproduces the original fixed sigma.
            blur_sigma: 4.0 * (blur_strength as f64).max(0.1),
            threshold: 0.0,
            lut: None,
        };
        c.set_zoom(1.0);
        c
    }

    /// Matte threshold `t` (0 = off). Matte values below `t - 0.1` become background,
    /// above `t + 0.1` person, with a linear ramp between so edges stay anti-aliased.
    pub fn set_threshold(&mut self, t: f32) {
        let t = t.clamp(0.0, THRESHOLD_MAX);
        if (t - self.threshold).abs() < 1e-6 {
            return;
        }
        self.threshold = t;
        self.lut = threshold_lut(t);
    }

    /// (x, y, w, h) of the camera frame inside the output, or `None` for an exact fit.
    pub fn rect(&self) -> Option<(usize, usize, usize, usize)> {
        self.rect
    }

    /// Scale the camera image as if it were part of a `full` frame that fits the canvas
    /// (the uncropped camera frame). Without this the camera image itself is fitted.
    pub fn set_scale_reference(&mut self, full: (usize, usize)) {
        self.scale_ref = full;
        let zoom = self.zoom;
        self.set_zoom(zoom);
    }

    /// zoom 1 = the reference frame fits the canvas (a crop keeps its size within it);
    /// zoom z scales that by 1/z, centred horizontally and bottom-aligned (the
    /// middle-bottom point of the camera image coincides with the middle-bottom point
    /// of the output frame).
    pub fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        let (cw, ch) = self.cam_size;
        let (x, y, w, h) = fit_rect(cw, ch, self.scale_ref, self.w, self.h, true, zoom);
        self.zoom = zoom;
        let (cw_i, ch_i) = (self.w as i64, self.h as i64);
        if (x, y, w, h) == (0, 0, cw_i, ch_i) && (cw, ch) == (self.w, self.h) {
            self.rect = None; // exact fit: untouched fast path
            return;
        }
        // Zoom in (<1) makes the scaled frame larger than the canvas. Keep only the part
        // that is visible: `dst` in canvas coordinates, and the matching crop `src` of the
        // camera frame, so only the visible region is ever resized.
        let (dx0, dy0) = (x.max(0), y.max(0));
        let (dx1, dy1) = ((x + w).min(cw_i), (y + h).min(ch_i));
        let fx = cw as f64 / w as f64;
        let fy = ch as f64 / h as f64;
        let sx0 = py_round((dx0 - x) as f64 * fx).clamp(0, cw as i64 - 1);
        let sy0 = py_round((dy0 - y) as f64 * fy).clamp(0, ch as i64 - 1);
        let sx1 = py_round((dx1 - x) as f64 * fx).max(sx0 + 1).min(cw as i64);
        let sy1 = py_round((dy1 - y) as f64 * fy).max(sy0 + 1).min(ch as i64);
        self.src = (sx0 as usize, sy0 as usize, sx1 as usize, sy1 as usize);
        let rw = (dx1 - dx0).max(1) as usize;
        let rh = (dy1 - dy0).max(1) as usize;
        // clear what the previous size left behind
        for v in self.fgc.iter_mut() {
            *v = 0;
        }
        for v in self.alc.iter_mut() {
            *v = 0;
        }
        // Shrinking: area averaging avoids aliasing; enlarging: bilinear.
        let src_area = (sx1 - sx0) * (sy1 - sy0);
        self.interp = if (rw * rh) as i64 > 0 && ((rw * rh) as i64) < src_area { Interp::Area } else { Interp::Linear };
        self.rect = Some((dx0 as usize, dy0 as usize, rw, rh));
    }

    /// `self.src` (in `cam_size` pixels) mapped onto a `fw x fh` source frame.
    fn source_region(&self, fw: usize, fh: usize) -> (usize, usize, usize, usize) {
        let (cw, ch) = self.cam_size;
        if (fw, fh) == (cw, ch) || cw == 0 || ch == 0 || fw == 0 || fh == 0 {
            return self.src;
        }
        let (a, b, c, d) = self.src;
        let fx = fw as f64 / cw as f64;
        let fy = fh as f64 / ch as f64;
        let x0 = (py_round(a as f64 * fx).max(0) as usize).min(fw - 1);
        let y0 = (py_round(b as f64 * fy).max(0) as usize).min(fh - 1);
        let x1 = (py_round(c as f64 * fx).max(0) as usize).clamp(x0 + 1, fw);
        let y1 = (py_round(d as f64 * fy).max(0) as usize).clamp(y0 + 1, fh);
        (x0, y0, x1, y1)
    }

    fn blurred_background(&self, frame: &Bgr) -> Bgr {
        let mut small = resize_cover(frame, self.bw, self.bh);
        imgproc::gaussian_blur_u8(
            &mut small.data,
            self.bw,
            self.bh,
            3,
            imgproc::ksize_for_sigma(self.blur_sigma),
            self.blur_sigma,
        );
        imgproc::resize_bgr(&small, self.w, self.h, Interp::Linear)
    }

    /// `alpha` is a `frame.w x frame.h` matte in [0, 1]; 1 == person, 0 == background.
    /// `zoom_target` is applied here, on the render thread.
    /// `threshold` is applied the same way (see `set_threshold`).
    pub fn render(&mut self, frame: &Bgr, alpha: &[f32], zoom_target: f32, threshold: f32) -> &Bgr {
        if (zoom_target.clamp(ZOOM_MIN, ZOOM_MAX) - self.zoom).abs() > 1e-6 {
            self.set_zoom(zoom_target);
        }
        self.set_threshold(threshold);
        let blurred;
        let bg: &Bgr = if self.blur_bg || self.background.is_none() {
            blurred = self.blurred_background(frame);
            &blurred
        } else {
            self.background.as_ref().unwrap()
        };
        let w = self.w;

        let use_canvas = self.rect.is_some();
        if let Some((x, y, rw, rh)) = self.rect {
            // Scale only the matted foreground + alpha into the camera rectangle; the
            // background stays sharp at output resolution, so there is no seam.
            let (sx0, sy0, sx1, sy1) = self.source_region(frame.w, frame.h);
            let (sw, sh) = (sx1 - sx0, sy1 - sy0);
            imgproc::resize_u8_region(
                &frame.data, frame.w * 3, sx0, sy0, sw, sh, 3,
                &mut self.fgc, w * 3, x, y, rw, rh, self.interp,
            );
            self.crop_alpha.resize(sw * sh, 0);
            let fw = frame.w;
            self.crop_alpha.par_chunks_mut(sw).enumerate().for_each(|(cy, row)| {
                let arow = &alpha[(sy0 + cy) * fw + sx0..(sy0 + cy) * fw + sx1];
                for (o, &a) in row.iter_mut().zip(arow.iter()) {
                    *o = (a * 255.0).clamp(0.0, 255.0) as u8;
                }
            });
            imgproc::resize_u8_region(
                &self.crop_alpha, sw, 0, 0, sw, sh, 1,
                &mut self.alc, w, x, y, rw, rh, self.interp,
            );
            self.alpha_u8.copy_from_slice(&self.alc);
        } else {
            self.alpha_u8.par_chunks_mut(w).zip(alpha.par_chunks(w)).for_each(|(orow, arow)| {
                for (o, &a) in orow.iter_mut().zip(arow.iter()) {
                    *o = (a * 255.0).clamp(0.0, 255.0) as u8;
                }
            });
        }
        if let Some(lut) = self.lut.as_ref() {
            self.alpha_u8.par_chunks_mut(w).for_each(|row| {
                for a in row.iter_mut() {
                    *a = lut[*a as usize];
                }
            });
        }

        // out = bg + (fg - bg) * alpha / 255, rounded to nearest.
        let fg: &[u8] = if use_canvas { &self.fgc } else { &frame.data };
        let alpha_u8 = &self.alpha_u8;
        self.out
            .data
            .par_chunks_mut(w * 3)
            .zip(fg.par_chunks(w * 3))
            .zip(bg.data.par_chunks(w * 3))
            .zip(alpha_u8.par_chunks(w))
            .for_each(|(((orow, frow), brow), arow)| {
                for x in 0..w {
                    let a = arow[x] as i32;
                    for ch in 0..3 {
                        let i = x * 3 + ch;
                        let b = brow[i] as i32;
                        let d = (frow[i] as i32 - b) * a;
                        let q = if d >= 0 { (d + 127) / 255 } else { (d - 127) / 255 };
                        orow[i] = (b + q).clamp(0, 255) as u8;
                    }
                }
            });
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_matte_shows_frame_and_clear_matte_shows_background() {
        let frame = Bgr::filled(64, 48, (10, 20, 30));
        let bg = Bgr::filled(64, 48, (200, 100, 50));
        let mut c = Compositor::new(64, 48, Some(bg), false, 1.0, (64, 48));
        assert!(c.rect().is_none());
        let ones = vec![1.0f32; 64 * 48];
        assert_eq!(&c.render(&frame, &ones, 1.0, 0.0).data[..3], &[10, 20, 30]);
        let zeros = vec![0.0f32; 64 * 48];
        assert_eq!(&c.render(&frame, &zeros, 1.0, 0.0).data[..3], &[200, 100, 50]);
    }

    #[test]
    fn zoom_out_places_camera_bottom_centre() {
        let frame = Bgr::filled(64, 48, (255, 255, 255));
        let bg = Bgr::filled(64, 48, (0, 0, 0));
        let mut c = Compositor::new(64, 48, Some(bg), false, 1.0, (64, 48));
        let ones = vec![1.0f32; 64 * 48];
        let out = c.render(&frame, &ones, 2.0, 0.0).clone();
        assert_eq!(c.rect(), Some((16, 24, 32, 24)));
        assert_eq!(out.data[0], 0); // top-left: background
        let mid = (40 * 64 + 32) * 3; // inside the camera rectangle
        assert_eq!(out.data[mid], 255);
    }

    #[test]
    fn threshold_makes_uncertain_matte_solid() {
        let frame = Bgr::filled(64, 48, (10, 20, 30));
        let bg = Bgr::filled(64, 48, (200, 100, 50));
        let mut c = Compositor::new(64, 48, Some(bg), false, 1.0, (64, 48));
        let half = vec![0.5f32; 64 * 48];
        // off: a 50/50 mix (0.5 -> 127 of 255)
        assert_eq!(c.render(&frame, &half, 1.0, 0.0).data[0], 105);
        // threshold 0.35: 0.5 is above 0.45 -> solid person
        assert_eq!(&c.render(&frame, &half, 1.0, 0.35).data[..3], &[10, 20, 30]);
        // threshold 0.7: 0.5 is below 0.6 -> background
        assert_eq!(&c.render(&frame, &half, 1.0, 0.7).data[..3], &[200, 100, 50]);
    }

    #[test]
    fn smaller_source_maps_onto_the_same_rectangle() {
        // a 64x48 camera shown at zoom 2 is a 32x24 rectangle; a 32x24 source fills it 1:1
        let bg = Bgr::filled(64, 48, (0, 0, 0));
        let mut c = Compositor::new(64, 48, Some(bg), false, 1.0, (64, 48));
        let small = Bgr::filled(32, 24, (255, 255, 255));
        let ones = vec![1.0f32; 32 * 24];
        let out = c.render(&small, &ones, 2.0, 0.0).clone();
        assert_eq!(c.rect(), Some((16, 24, 32, 24)));
        assert_eq!(out.data[(40 * 64 + 32) * 3], 255);
        assert_eq!(out.data[(23 * 64 + 32) * 3], 0);
    }
}
