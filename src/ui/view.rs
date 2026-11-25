//! Geometry of the previews. The preview box can be any size (it follows the window);
//! the pictures are scaled to fit it by the GPU.

/// Where a `w x h` picture sits when scaled to fit, centred, in a `bw x bh` box:
/// (x, y, width, height) in box px.
pub fn fit_in_box(w: f32, h: f32, bw: f32, bh: f32) -> (f32, f32, f32, f32) {
    let s = (bw / w).min(bh / h);
    let (dw, dh) = (w * s, h * s);
    ((bw - dw) / 2.0, (bh - dh) / 2.0, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_is_centred_in_the_box() {
        assert_eq!(fit_in_box(1280.0, 720.0, 480.0, 270.0), (0.0, 0.0, 480.0, 270.0));
        // a wider box letterboxes left and right
        let (x, y, w, h) = fit_in_box(1280.0, 720.0, 2000.0, 720.0);
        assert_eq!((x, y, w, h), (360.0, 0.0, 1280.0, 720.0));
    }
}
