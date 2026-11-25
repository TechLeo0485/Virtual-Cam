//! Background helpers: load / cover-resize / blur a picture, solid colours.

use crate::imgproc::{self, Bgr, Interp};
use anyhow::{anyhow, bail, Result};
use image::ImageDecoder;
use std::path::Path;

/// Decode an image file into BGR, honouring its EXIF orientation.
pub fn decode_image(path: &Path) -> Result<Bgr> {
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().ok();
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let rgb = img.into_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    if w == 0 || h == 0 {
        bail!("empty image");
    }
    let mut data = rgb.into_raw();
    for px in data.chunks_exact_mut(3) {
        px.swap(0, 2);
    }
    Ok(Bgr { w, h, data })
}

/// Resize `img` to fill (width, height), cropping the overflow (no distortion).
pub fn resize_cover(img: &Bgr, width: usize, height: usize) -> Bgr {
    let scale = (width as f64 / img.w as f64).max(height as f64 / img.h as f64);
    let nw = (crate::util::py_round(img.w as f64 * scale).max(1) as usize).max(width);
    let nh = (crate::util::py_round(img.h as f64 * scale).max(1) as usize).max(height);
    let resized = imgproc::resize_bgr(img, nw, nh, Interp::Area);
    let x0 = (nw - width) / 2;
    let y0 = (nh - height) / 2;
    imgproc::crop(&resized, x0, y0, x0 + width, y0 + height)
}

pub fn load_background_image(path: &Path, width: usize, height: usize) -> Result<Bgr> {
    let img = decode_image(path)
        .map_err(|e| anyhow!("Could not read background image: {} ({})", path.display(), e))?;
    Ok(resize_cover(&img, width, height))
}

/// Soft, out-of-focus version of a chosen background picture (the Meet-style
/// "blurred office photo" look, applied to your own image). Built once per
/// background choice rather than per frame, so unlike the live blur mode there's no
/// speed budget: downscale->blur->upscale gives a smoother, more photographic result
/// than one big-sigma blur on the full-res image.
///
/// `strength` scales the blur sigma; 1.0 reproduces the original fixed amount.
pub fn blur_background_image(img: &Bgr, strength: f32) -> Bgr {
    let (w, h) = (img.w, img.h);
    let (sw, sh) = ((w / 8).max(1), (h / 8).max(1));
    let mut small = imgproc::resize_bgr(img, sw, sh, Interp::Area);
    let sigma = 6.0 * (strength as f64).max(0.1);
    imgproc::gaussian_blur_u8(&mut small.data, sw, sh, 3, imgproc::ksize_for_sigma(sigma), sigma);
    imgproc::resize_bgr(&small, w, h, Interp::Linear)
}

pub fn solid_color_background(color_bgr: (u8, u8, u8), width: usize, height: usize) -> Bgr {
    Bgr::filled(width, height, color_bgr)
}

/// Parse 'R,G,B' (0-255) into a BGR tuple.
pub fn parse_color(text: &str) -> Result<(u8, u8, u8), String> {
    let parts: Vec<&str> = text.split(',').map(|s| s.trim()).collect();
    if parts.len() != 3 {
        return Err("--color must be 'R,G,B', e.g. 0,177,64".to_string());
    }
    let mut v = [0u8; 3];
    for (i, p) in parts.iter().enumerate() {
        let n: i64 = p.parse().map_err(|_| "--color must be 'R,G,B', e.g. 0,177,64".to_string())?;
        if !(0..=255).contains(&n) {
            return Err("color channels must be 0-255".to_string());
        }
        v[i] = n as u8;
    }
    Ok((v[2], v[1], v[0])) // stored as BGR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_parsing() {
        assert_eq!(parse_color("0,177,64"), Ok((64, 177, 0)));
        assert!(parse_color("1,2").is_err());
        assert!(parse_color("1,2,300").is_err());
    }

    #[test]
    fn cover_has_exact_size() {
        let img = Bgr::filled(100, 50, (1, 2, 3));
        let out = resize_cover(&img, 64, 64);
        assert_eq!((out.w, out.h), (64, 64));
        assert_eq!(&out.data[..3], &[1, 2, 3]);
    }
}
