//! Image primitives the pipeline needs (the parts of OpenCV that `vcam.py` used):
//! resizing, Gaussian blur, box / guided filter, binary morphology, connected
//! components, rotate / flip / crop. Pure Rust, parallelised over rows with rayon.

use rayon::prelude::*;

/// 8-bit BGR image, row-major, tightly packed (`data.len() == w * h * 3`).
#[derive(Clone, Debug)]
pub struct Bgr {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Bgr {
    pub fn new(w: usize, h: usize) -> Self {
        Bgr { w, h, data: vec![0u8; w * h * 3] }
    }

    pub fn filled(w: usize, h: usize, bgr: (u8, u8, u8)) -> Self {
        let mut data = vec![0u8; w * h * 3];
        for px in data.chunks_exact_mut(3) {
            px[0] = bgr.0;
            px[1] = bgr.1;
            px[2] = bgr.2;
        }
        Bgr { w, h, data }
    }
}

// --------------------------------------------------------------------------- //
// Border handling
// --------------------------------------------------------------------------- //

/// OpenCV BORDER_REFLECT_101: `gfedcb|abcdefgh|gfedcba`.
pub fn reflect101(mut i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let n = n as isize;
    loop {
        if i < 0 {
            i = -i;
        } else if i >= n {
            i = 2 * n - 2 - i;
        } else {
            return i as usize;
        }
    }
}

/// OpenCV BORDER_REFLECT: `fedcba|abcdefgh|hgfedcb`.
pub fn reflect(mut i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let n = n as isize;
    loop {
        if i < 0 {
            i = -i - 1;
        } else if i >= n {
            i = 2 * n - 1 - i;
        } else {
            return i as usize;
        }
    }
}

// --------------------------------------------------------------------------- //
// Resize
// --------------------------------------------------------------------------- //

/// `Linear` = cv2.INTER_LINEAR (two taps, no anti-aliasing). `Area` = cv2.INTER_AREA
/// when shrinking (box coverage); it falls back to `Linear` on an axis that grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Linear,
    Area,
}

/// Per-output-sample source indices and weights (`len` taps each).
struct Taps {
    len: usize,
    idx: Vec<u32>,
    w: Vec<f32>,
}

fn make_taps(src: usize, dst: usize, interp: Interp) -> Taps {
    let src = src.max(1);
    let dst = dst.max(1);
    if interp == Interp::Area && dst < src {
        // cv::computeResizeAreaTab
        let scale = src as f64 / dst as f64;
        let mut lists: Vec<Vec<(u32, f32)>> = Vec::with_capacity(dst);
        let mut max_len = 1;
        for d in 0..dst {
            let fsx1 = d as f64 * scale;
            let fsx2 = fsx1 + scale;
            let cell = scale.min(src as f64 - fsx1).max(1e-6);
            let mut sx1 = fsx1.ceil() as usize;
            let mut sx2 = fsx2.floor() as usize;
            sx2 = sx2.min(src);
            sx1 = sx1.min(sx2);
            let mut list: Vec<(u32, f32)> = Vec::new();
            if sx1 as f64 - fsx1 > 1e-3 && sx1 >= 1 {
                list.push(((sx1 - 1) as u32, ((sx1 as f64 - fsx1) / cell) as f32));
            }
            for sx in sx1..sx2 {
                list.push((sx as u32, (1.0 / cell) as f32));
            }
            if fsx2 - sx2 as f64 > 1e-3 && sx2 < src {
                let wgt = (fsx2 - sx2 as f64).min(1.0).min(cell) / cell;
                list.push((sx2 as u32, wgt as f32));
            }
            if list.is_empty() {
                list.push(((fsx1 as usize).min(src - 1) as u32, 1.0));
            }
            let sum: f32 = list.iter().map(|t| t.1).sum();
            if sum > 0.0 {
                for t in list.iter_mut() {
                    t.1 /= sum;
                }
            }
            max_len = max_len.max(list.len());
            lists.push(list);
        }
        let mut idx = Vec::with_capacity(dst * max_len);
        let mut w = Vec::with_capacity(dst * max_len);
        for list in &lists {
            let last = list[list.len() - 1].0;
            for k in 0..max_len {
                if k < list.len() {
                    idx.push(list[k].0);
                    w.push(list[k].1);
                } else {
                    idx.push(last);
                    w.push(0.0);
                }
            }
        }
        Taps { len: max_len, idx, w }
    } else {
        // cv2.INTER_LINEAR: pixel centres aligned, edges clamped.
        let scale = src as f64 / dst as f64;
        let mut idx = Vec::with_capacity(dst * 2);
        let mut w = Vec::with_capacity(dst * 2);
        for d in 0..dst {
            let f = (d as f64 + 0.5) * scale - 0.5;
            let mut x0 = f.floor();
            let mut t = f - x0;
            if x0 < 0.0 {
                x0 = 0.0;
                t = 0.0;
            }
            let mut i0 = x0 as usize;
            if i0 >= src - 1 {
                i0 = src - 1;
                t = 0.0;
            }
            let i1 = (i0 + 1).min(src - 1);
            idx.push(i0 as u32);
            w.push((1.0 - t) as f32);
            idx.push(i1 as u32);
            w.push(t as f32);
        }
        Taps { len: 2, idx, w }
    }
}

/// Resize the `sw x sh` region at (`sx0`, `sy0`) of an interleaved 8-bit image with `c`
/// channels into the `dw x dh` region at (`dx0`, `dy0`) of another one. Strides are in
/// bytes per row. Only the destination region is written.
#[allow(clippy::too_many_arguments)]
pub fn resize_u8_region(
    src: &[u8],
    src_stride: usize,
    sx0: usize,
    sy0: usize,
    sw: usize,
    sh: usize,
    c: usize,
    dst: &mut [u8],
    dst_stride: usize,
    dx0: usize,
    dy0: usize,
    dw: usize,
    dh: usize,
    interp: Interp,
) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let rows = &mut dst[dy0 * dst_stride..(dy0 + dh) * dst_stride];
    if sw == dw && sh == dh {
        rows.par_chunks_mut(dst_stride).enumerate().for_each(|(y, row)| {
            let s = &src[(sy0 + y) * src_stride + sx0 * c..][..sw * c];
            row[dx0 * c..(dx0 + dw) * c].copy_from_slice(s);
        });
        return;
    }
    let th = make_taps(sw, dw, interp);
    let tv = make_taps(sh, dh, interp);
    rows.par_chunks_mut(dst_stride).enumerate().for_each_init(
        || vec![0f32; dw * c],
        |acc, (y, row)| {
            for v in acc.iter_mut() {
                *v = 0.0;
            }
            for j in 0..tv.len {
                let wy = tv.w[y * tv.len + j];
                if wy == 0.0 {
                    continue;
                }
                let sy = sy0 + tv.idx[y * tv.len + j] as usize;
                let srow = &src[sy * src_stride + sx0 * c..][..sw * c];
                for x in 0..dw {
                    let base = x * th.len;
                    for k in 0..th.len {
                        let wx = th.w[base + k] * wy;
                        if wx == 0.0 {
                            continue;
                        }
                        let si = th.idx[base + k] as usize * c;
                        for ch in 0..c {
                            acc[x * c + ch] += wx * srow[si + ch] as f32;
                        }
                    }
                }
            }
            let out = &mut row[dx0 * c..(dx0 + dw) * c];
            for (o, a) in out.iter_mut().zip(acc.iter()) {
                *o = (*a + 0.5).clamp(0.0, 255.0) as u8;
            }
        },
    );
}

/// Resize a whole interleaved 8-bit image with `c` channels.
pub fn resize_u8(src: &[u8], sw: usize, sh: usize, c: usize, dw: usize, dh: usize, interp: Interp) -> Vec<u8> {
    let mut dst = vec![0u8; dw * dh * c];
    resize_u8_region(src, sw * c, 0, 0, sw, sh, c, &mut dst, dw * c, 0, 0, dw, dh, interp);
    dst
}

pub fn resize_bgr(src: &Bgr, dw: usize, dh: usize, interp: Interp) -> Bgr {
    Bgr { w: dw, h: dh, data: resize_u8(&src.data, src.w, src.h, 3, dw, dh, interp) }
}

/// Resize a single-channel float image.
pub fn resize_f32(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize, interp: Interp) -> Vec<f32> {
    if sw == dw && sh == dh {
        return src.to_vec();
    }
    let mut dst = vec![0f32; dw * dh];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return dst;
    }
    let th = make_taps(sw, dw, interp);
    let tv = make_taps(sh, dh, interp);
    dst.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        for v in row.iter_mut() {
            *v = 0.0;
        }
        for j in 0..tv.len {
            let wy = tv.w[y * tv.len + j];
            if wy == 0.0 {
                continue;
            }
            let sy = tv.idx[y * tv.len + j] as usize;
            let srow = &src[sy * sw..(sy + 1) * sw];
            for x in 0..dw {
                let base = x * th.len;
                let mut acc = 0f32;
                for k in 0..th.len {
                    acc += th.w[base + k] * srow[th.idx[base + k] as usize];
                }
                row[x] += wy * acc;
            }
        }
    });
    dst
}

/// Nearest-neighbour resize of a single-channel 8-bit image (cv2.INTER_NEAREST).
pub fn resize_nearest_u8(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut dst = vec![0u8; dw * dh];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return dst;
    }
    let xs: Vec<usize> = (0..dw).map(|x| (x * sw / dw).min(sw - 1)).collect();
    dst.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let sy = (y * sh / dh).min(sh - 1);
        let srow = &src[sy * sw..(sy + 1) * sw];
        for (o, &sx) in row.iter_mut().zip(xs.iter()) {
            *o = srow[sx];
        }
    });
    dst
}

// --------------------------------------------------------------------------- //
// Gaussian blur
// --------------------------------------------------------------------------- //

/// cv::getGaussianKernel (including OpenCV's fixed small kernels when `sigma <= 0`).
pub fn gaussian_kernel(ksize: usize, sigma: f64) -> Vec<f32> {
    let n = ksize.max(1);
    if sigma <= 0.0 {
        match n {
            1 => return vec![1.0],
            3 => return vec![0.25, 0.5, 0.25],
            5 => return vec![0.0625, 0.25, 0.375, 0.25, 0.0625],
            7 => return vec![0.03125, 0.109375, 0.21875, 0.28125, 0.21875, 0.109375, 0.03125],
            _ => {}
        }
    }
    let sigma_x = if sigma > 0.0 { sigma } else { ((n as f64 - 1.0) * 0.5 - 1.0) * 0.3 + 0.8 };
    let scale2x = -0.5 / (sigma_x * sigma_x);
    let mut k: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - (n as f64 - 1.0) * 0.5;
            (scale2x * x * x).exp()
        })
        .collect();
    let sum: f64 = k.iter().sum();
    for v in k.iter_mut() {
        *v /= sum;
    }
    k.into_iter().map(|v| v as f32).collect()
}

/// Kernel size OpenCV derives from sigma for 8-bit images when `ksize == (0, 0)`.
pub fn ksize_for_sigma(sigma: f64) -> usize {
    (((sigma * 6.0 + 1.0).round() as usize).max(1)) | 1
}

/// In-place separable Gaussian blur of an interleaved 8-bit image with `c` channels
/// (BORDER_REFLECT_101, like cv2.GaussianBlur). `sigma <= 0` derives it from `ksize`.
pub fn gaussian_blur_u8(data: &mut [u8], w: usize, h: usize, c: usize, ksize: usize, sigma: f64) {
    if ksize <= 1 || w == 0 || h == 0 {
        return;
    }
    let kern = gaussian_kernel(ksize, sigma);
    let r = ksize / 2;
    let row_len = w * c;
    let mut tmp = vec![0f32; row_len * h];

    // Horizontal pass: data -> tmp.
    tmp.par_chunks_mut(row_len).zip(data.par_chunks(row_len)).for_each_init(
        || vec![0u8; (w + 2 * r) * c],
        |pad, (trow, srow)| {
            for x in 0..(w + 2 * r) {
                let sx = reflect101(x as isize - r as isize, w);
                pad[x * c..x * c + c].copy_from_slice(&srow[sx * c..sx * c + c]);
            }
            for v in trow.iter_mut() {
                *v = 0.0;
            }
            for (k, &kv) in kern.iter().enumerate() {
                let seg = &pad[k * c..k * c + row_len];
                for (t, &p) in trow.iter_mut().zip(seg.iter()) {
                    *t += kv * p as f32;
                }
            }
        },
    );

    // Vertical pass: tmp -> data.
    let tmp_ref = &tmp;
    let kern_ref = &kern;
    data.par_chunks_mut(row_len).enumerate().for_each_init(
        || vec![0f32; row_len],
        |acc, (y, orow)| {
            for v in acc.iter_mut() {
                *v = 0.0;
            }
            for (k, &kv) in kern_ref.iter().enumerate() {
                let sy = reflect101(y as isize + k as isize - r as isize, h);
                let trow = &tmp_ref[sy * row_len..(sy + 1) * row_len];
                for (a, &t) in acc.iter_mut().zip(trow.iter()) {
                    *a += kv * t;
                }
            }
            for (o, a) in orow.iter_mut().zip(acc.iter()) {
                *o = (*a + 0.5).clamp(0.0, 255.0) as u8;
            }
        },
    );
}

// --------------------------------------------------------------------------- //
// Box filter / guided filter (float, single channel)
// --------------------------------------------------------------------------- //

/// Normalised box filter with a `(2r+1) x (2r+1)` window and BORDER_REFLECT.
pub fn box_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    let win = 2 * r + 1;
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).zip(src.par_chunks(w)).for_each_init(
        || vec![0f32; w + 2 * r],
        |pad, (trow, srow)| {
            for x in 0..(w + 2 * r) {
                pad[x] = srow[reflect(x as isize - r as isize, w)];
            }
            let mut s: f32 = pad[..win].iter().sum();
            trow[0] = s;
            for x in 1..w {
                s += pad[x + win - 1] - pad[x - 1];
                trow[x] = s;
            }
        },
    );
    let norm = 1.0 / (win * win) as f32;
    let mut out = vec![0f32; w * h];
    let mut col = vec![0f32; w];
    for k in 0..win {
        let sy = reflect(k as isize - r as isize, h);
        let trow = &tmp[sy * w..(sy + 1) * w];
        for (cv, &t) in col.iter_mut().zip(trow.iter()) {
            *cv += t;
        }
    }
    for y in 0..h {
        let orow = &mut out[y * w..(y + 1) * w];
        for (o, &cv) in orow.iter_mut().zip(col.iter()) {
            *o = cv * norm;
        }
        if y + 1 < h {
            let add = reflect(y as isize + r as isize + 1, h);
            let sub = reflect(y as isize - r as isize, h);
            let arow = &tmp[add * w..(add + 1) * w];
            let srow = &tmp[sub * w..(sub + 1) * w];
            for ((cv, &a), &s) in col.iter_mut().zip(arow.iter()).zip(srow.iter()) {
                *cv += a - s;
            }
        }
    }
    out
}

/// Guided filter (He et al.) of `p` with the grey guide `guide` (both in [0, 1]) — the
/// equivalent of `cv2.ximgproc.guidedFilter(guide, p, radius, eps)`.
pub fn guided_filter(guide: &[f32], p: &[f32], w: usize, h: usize, radius: usize, eps: f32) -> Vec<f32> {
    let n = w * h;
    let ii: Vec<f32> = guide.par_iter().map(|&g| g * g).collect();
    let ip: Vec<f32> = guide.par_iter().zip(p.par_iter()).map(|(&g, &q)| g * q).collect();
    let mean_i = box_filter(guide, w, h, radius);
    let mean_p = box_filter(p, w, h, radius);
    let corr_i = box_filter(&ii, w, h, radius);
    let corr_ip = box_filter(&ip, w, h, radius);
    let mut a = vec![0f32; n];
    let mut b = vec![0f32; n];
    for i in 0..n {
        let var_i = corr_i[i] - mean_i[i] * mean_i[i];
        let cov_ip = corr_ip[i] - mean_i[i] * mean_p[i];
        let av = cov_ip / (var_i + eps);
        a[i] = av;
        b[i] = mean_p[i] - av * mean_i[i];
    }
    let mean_a = box_filter(&a, w, h, radius);
    let mean_b = box_filter(&b, w, h, radius);
    let mut q = vec![0f32; n];
    for i in 0..n {
        q[i] = mean_a[i] * guide[i] + mean_b[i];
    }
    q
}

// --------------------------------------------------------------------------- //
// Connected components
// --------------------------------------------------------------------------- //

fn uf_find(parent: &mut [u32], mut a: u32) -> u32 {
    while parent[a as usize] != a {
        let p = parent[a as usize];
        parent[a as usize] = parent[p as usize];
        a = parent[a as usize];
    }
    a
}

/// Mask (0/1) of the largest 8-connected blob of non-zero pixels in `bin`, or `None`
/// when there is no foreground at all.
pub fn largest_component(bin: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    // Runs of set pixels per row: (x0, x1 exclusive, label).
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    let mut row_off: Vec<usize> = Vec::with_capacity(h + 1);
    let mut parent: Vec<u32> = Vec::new();
    let mut area: Vec<u64> = Vec::new();
    for y in 0..h {
        row_off.push(runs.len());
        let row = &bin[y * w..(y + 1) * w];
        let (prev_start, prev_end) = if y > 0 { (row_off[y - 1], row_off[y]) } else { (0, 0) };
        let mut x = 0;
        while x < w {
            if row[x] == 0 {
                x += 1;
                continue;
            }
            let x0 = x;
            while x < w && row[x] != 0 {
                x += 1;
            }
            let x1 = x;
            let label = parent.len() as u32;
            parent.push(label);
            area.push((x1 - x0) as u64);
            for pi in prev_start..prev_end {
                let (px0, px1, plabel) = runs[pi];
                // 8-connectivity: the runs touch, also diagonally.
                if (px0 as usize) <= x1 && (px1 as usize) >= x0 {
                    let ra = uf_find(&mut parent, plabel);
                    let rb = uf_find(&mut parent, label);
                    if ra != rb {
                        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                        parent[hi as usize] = lo;
                        area[lo as usize] += area[hi as usize];
                    }
                }
            }
            runs.push((x0 as u32, x1 as u32, label));
        }
    }
    row_off.push(runs.len());
    if runs.is_empty() {
        return None;
    }
    let mut best = 0u32;
    let mut best_area = 0u64;
    for l in 0..parent.len() as u32 {
        if parent[l as usize] == l && area[l as usize] > best_area {
            best_area = area[l as usize];
            best = l;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for ri in row_off[y]..row_off[y + 1] {
            let (x0, x1, label) = runs[ri];
            if uf_find(&mut parent, label) == best {
                for v in &mut out[y * w + x0 as usize..y * w + x1 as usize] {
                    *v = 1;
                }
            }
        }
    }
    Some(out)
}

// --------------------------------------------------------------------------- //
// Binary morphology with OpenCV's elliptical structuring element
// --------------------------------------------------------------------------- //

/// Half-width of each row of `cv2.getStructuringElement(MORPH_ELLIPSE, (k, k))`.
pub fn ellipse_half_widths(k: usize) -> Vec<i32> {
    let k = k.max(1);
    let r = (k / 2) as f64;
    let c = (k / 2) as f64;
    let inv_r2 = if r > 0.0 { 1.0 / (r * r) } else { 0.0 };
    (0..k)
        .map(|i| {
            let dy = i as f64 - r;
            (c * ((r * r - dy * dy) * inv_r2).max(0.0).sqrt()).round_ties_even() as i32
        })
        .collect()
}

/// Dilate a 0/1 image with the elliptical element described by `half_widths`.
pub fn dilate(bin: &[u8], w: usize, h: usize, half_widths: &[i32]) -> Vec<u8> {
    let r = (half_widths.len() / 2) as isize;
    // Horizontal distance from each pixel to the nearest set pixel in its row.
    let mut hd = vec![u16::MAX; w * h];
    hd.par_chunks_mut(w).zip(bin.par_chunks(w)).for_each(|(drow, srow)| {
        let mut d = u16::MAX;
        for x in 0..w {
            d = if srow[x] != 0 { 0 } else { d.saturating_add(1) };
            drow[x] = d;
        }
        let mut d = u16::MAX;
        for x in (0..w).rev() {
            d = if srow[x] != 0 { 0 } else { d.saturating_add(1) };
            if d < drow[x] {
                drow[x] = d;
            }
        }
    });
    let hd_ref = &hd;
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, orow)| {
        for (i, &hw) in half_widths.iter().enumerate() {
            let sy = y as isize + i as isize - r;
            if sy < 0 || sy >= h as isize || hw < 0 {
                continue;
            }
            let drow = &hd_ref[sy as usize * w..(sy as usize + 1) * w];
            let lim = hw as u16;
            for (o, &d) in orow.iter_mut().zip(drow.iter()) {
                if d <= lim {
                    *o = 1;
                }
            }
        }
    });
    out
}

/// Erode a 0/1 image (pixels outside the image count as set, as in OpenCV).
pub fn erode(bin: &[u8], w: usize, h: usize, half_widths: &[i32]) -> Vec<u8> {
    let inv: Vec<u8> = bin.iter().map(|&v| if v != 0 { 0 } else { 1 }).collect();
    let mut d = dilate(&inv, w, h, half_widths);
    for v in d.iter_mut() {
        *v = if *v != 0 { 0 } else { 1 };
    }
    d
}

// --------------------------------------------------------------------------- //
// Geometry on BGR frames
// --------------------------------------------------------------------------- //

/// BGR -> 8-bit grey with OpenCV's fixed-point weights.
pub fn bgr_to_gray(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).zip(src.par_chunks(w * 3)).for_each(|(orow, srow)| {
        for (o, px) in orow.iter_mut().zip(srow.chunks_exact(3)) {
            let v = px[0] as u32 * 1868 + px[1] as u32 * 9617 + px[2] as u32 * 4899 + 8192;
            *o = (v >> 14) as u8;
        }
    });
    out
}

/// Horizontal flip (selfie view).
pub fn flip_horizontal(src: &Bgr) -> Bgr {
    let w = src.w;
    let mut out = Bgr::new(src.w, src.h);
    out.data.par_chunks_mut(w * 3).zip(src.data.par_chunks(w * 3)).for_each(|(orow, srow)| {
        for x in 0..w {
            let s = (w - 1 - x) * 3;
            orow[x * 3..x * 3 + 3].copy_from_slice(&srow[s..s + 3]);
        }
    });
    out
}

/// Rotate clockwise by 0 / 90 / 180 / 270 degrees.
pub fn rotate(src: &Bgr, deg: u32) -> Bgr {
    let (w, h) = (src.w, src.h);
    match deg % 360 {
        90 => {
            let mut out = Bgr::new(h, w);
            out.data.par_chunks_mut(h * 3).enumerate().for_each(|(y, orow)| {
                // out(x, y) = src(y, h - 1 - x)
                for x in 0..h {
                    let s = ((h - 1 - x) * w + y) * 3;
                    orow[x * 3..x * 3 + 3].copy_from_slice(&src.data[s..s + 3]);
                }
            });
            out
        }
        180 => {
            let mut out = Bgr::new(w, h);
            out.data.par_chunks_mut(w * 3).enumerate().for_each(|(y, orow)| {
                let srow = &src.data[(h - 1 - y) * w * 3..(h - y) * w * 3];
                for x in 0..w {
                    let s = (w - 1 - x) * 3;
                    orow[x * 3..x * 3 + 3].copy_from_slice(&srow[s..s + 3]);
                }
            });
            out
        }
        270 => {
            let mut out = Bgr::new(h, w);
            out.data.par_chunks_mut(h * 3).enumerate().for_each(|(y, orow)| {
                // out(x, y) = src(w - 1 - y, x)
                for x in 0..h {
                    let s = (x * w + (w - 1 - y)) * 3;
                    orow[x * 3..x * 3 + 3].copy_from_slice(&src.data[s..s + 3]);
                }
            });
            out
        }
        _ => src.clone(),
    }
}

/// Copy the pixel box `(x0, y0, x1, y1)` (exclusive end) out of `src`.
pub fn crop(src: &Bgr, x0: usize, y0: usize, x1: usize, y1: usize) -> Bgr {
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Bgr::new(cw, ch);
    for y in 0..ch {
        let s = ((y0 + y) * src.w + x0) * 3;
        out.data[y * cw * 3..(y + 1) * cw * 3].copy_from_slice(&src.data[s..s + cw * 3]);
    }
    out
}

/// BGR -> RGB bytes (for display).
pub fn bgr_to_rgb_bytes(src: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for (o, s) in out.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
        o[0] = s[2];
        o[1] = s[1];
        o[2] = s[0];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipse_matches_opencv() {
        assert_eq!(ellipse_half_widths(3), vec![0, 1, 0]);
        assert_eq!(ellipse_half_widths(5), vec![0, 2, 2, 2, 0]);
        assert_eq!(ellipse_half_widths(11), vec![0, 3, 4, 5, 5, 5, 5, 5, 4, 3, 0]);
    }

    #[test]
    fn largest_component_picks_big_blob() {
        // 6x4: a 2-pixel blob on the left, a 5-pixel blob on the right.
        let bin = vec![
            1, 0, 0, 1, 1, 0, //
            1, 0, 0, 0, 1, 1, //
            0, 0, 0, 0, 0, 1, //
            0, 0, 0, 0, 0, 0,
        ];
        let m = largest_component(&bin, 6, 4).unwrap();
        assert_eq!(m.iter().map(|&v| v as u32).sum::<u32>(), 5);
        assert_eq!(m[0], 0);
        assert!(largest_component(&vec![0u8; 24], 6, 4).is_none());
    }

    #[test]
    fn resize_identity_and_constant() {
        let src = vec![100u8; 8 * 6 * 3];
        let out = resize_u8(&src, 8, 6, 3, 4, 3, Interp::Area);
        assert!(out.iter().all(|&v| v == 100));
        let out = resize_u8(&src, 8, 6, 3, 16, 12, Interp::Linear);
        assert!(out.iter().all(|&v| v == 100));
    }

    #[test]
    fn rotate_round_trip() {
        let mut img = Bgr::new(3, 2);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = i as u8;
        }
        let back = rotate(&rotate(&img, 90), 270);
        assert_eq!(back.data, img.data);
        assert_eq!(rotate(&img, 90).w, 2);
    }
}
