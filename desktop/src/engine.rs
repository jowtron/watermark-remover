//! Clean-room watermark-removal engine, ported 1:1 from the in-browser JS app.
//!
//! A fixed semi-transparent watermark is composited onto an image by
//!   `observed = original·(1 − α) + W·α`
//! with `α` the per-pixel opacity and `W` the (white) watermark colour. Both are
//! constant, so the composite is reversed exactly:
//!   `original = (observed − W·α) / (1 − α)`.
//! The watermark's position in a new image is found by correlating the α-shape
//! against a high-pass of the luminance (NCC). A profile can also be *learned*
//! blind from a batch of watermarked images via a low-percentile estimate.

use std::path::Path;

use crate::catalog::WmConfig;

#[inline]
fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// An α (opacity) map for one watermark tile size, with cached statistics that
/// the NCC detector needs (mirrors `setMap` in the web app).
#[derive(Clone)]
pub struct AlphaMap {
    pub size: usize,
    pub a: Vec<f32>, // size*size, each in 0..=1
    pub mean: f32,
    pub varsum: f32,
}

impl AlphaMap {
    pub fn new(size: usize, a: Vec<f32>) -> Self {
        let n = a.len() as f32;
        let s: f32 = a.iter().copied().sum();
        let ss: f32 = a.iter().map(|v| v * v).sum();
        AlphaMap {
            size,
            mean: s / n,
            varsum: ss - s * s / n,
            a,
        }
    }
    pub fn peak(&self) -> f32 {
        self.a.iter().copied().fold(0.0, f32::max)
    }
}

/// A decoded image plus its dimensions and source filename.
#[derive(Clone)]
pub struct LoadedImage {
    pub rgba: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub name: String,
}

/// Where the detector / learner believes the watermark sits.
pub struct Detection {
    pub ncc: f32,
    pub ox: i64,
    pub oy: i64,
    pub corner: &'static str,
    #[allow(dead_code)] // kept for parity with the web app's detection record
    pub size: usize,
}

/// Per-pixel luminance (Rec.601), matching the web app's `lumOf`.
pub fn lum_of(rgba: &[u8], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    let mut l = vec![0f32; n];
    for p in 0..n {
        let i = p * 4;
        l[p] = 0.299 * rgba[i] as f32 + 0.587 * rgba[i + 1] as f32 + 0.114 * rgba[i + 2] as f32;
    }
    l
}

/// Separable box blur with clamped edges (matches the web app's `boxBlur`).
pub fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let inv = 1.0 / (2 * r + 1) as f32;
    let cl = |x: i64, hi: usize| -> usize {
        if x < 0 {
            0
        } else if x as usize >= hi {
            hi - 1
        } else {
            x as usize
        }
    };
    let r = r as i64;
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let o = y * w;
        let mut acc = 0f32;
        for x in -r..=r {
            acc += src[o + cl(x, w)];
        }
        for x in 0..w {
            tmp[o + x] = acc * inv;
            acc += src[o + cl(x as i64 + r + 1, w)] - src[o + cl(x as i64 - r, w)];
        }
    }
    let mut out = vec![0f32; w * h];
    for x in 0..w {
        let mut acc = 0f32;
        for y in -r..=r {
            acc += tmp[cl(y, h) * w + x];
        }
        for y in 0..h {
            out[y * w + x] = acc * inv;
            acc += tmp[cl(y as i64 + r + 1, h) * w + x] - tmp[cl(y as i64 - r, h) * w + x];
        }
    }
    out
}

/// Locate the watermark by correlating the α-shape against the high-pass
/// luminance over the four corners, then refine ±8px (mirrors `detect`).
pub fn detect(lum: &[f32], w: usize, h: usize, map: &AlphaMap) -> Option<Detection> {
    let t = map.size;
    let a = &map.a;
    let r = (t as f32 * 0.75).round() as usize;
    let b = box_blur(lum, w, h, r);
    let mut hp = vec![0f32; w * h];
    for i in 0..w * h {
        hp[i] = lum[i] - b[i];
    }
    let n = (t * t) as f32;
    let score = |ox: i64, oy: i64| -> f32 {
        if ox < 0 || oy < 0 || ox + t as i64 > w as i64 || oy + t as i64 > h as i64 {
            return -2.0;
        }
        let (mut sh, mut shh, mut sah) = (0f32, 0f32, 0f32);
        for y in 0..t {
            let row = (oy as usize + y) * w + ox as usize;
            let ar = y * t;
            for x in 0..t {
                let hv = hp[row + x];
                let av = a[ar + x];
                sh += hv;
                shh += hv * hv;
                sah += av * hv;
            }
        }
        let num = sah - map.mean * sh;
        let den = (map.varsum * (shh - sh * sh / n)).sqrt();
        if den > 1e-6 {
            num / den
        } else {
            -2.0
        }
    };
    let (w_i, h_i, t_i) = (w as i64, h as i64, t as i64);
    let corners: [(&'static str, i64, i64, i64, i64); 4] = [
        ("br", w_i - t_i, h_i - t_i, -1, -1),
        ("bl", 0, h_i - t_i, 1, -1),
        ("tr", w_i - t_i, 0, -1, 1),
        ("tl", 0, 0, 1, 1),
    ];
    let mg_max = (w.min(h) as i64 / 2 - t_i).min(240);
    let mut best = Detection {
        ncc: -2.0,
        ox: 0,
        oy: 0,
        corner: "br",
        size: t,
    };
    for (name, bx, by, sx, sy) in corners {
        let mut mg = 4i64;
        while mg <= mg_max {
            let ox = bx + sx * mg;
            let oy = by + sy * mg;
            let ncc = score(ox, oy);
            if ncc > best.ncc {
                best = Detection {
                    ncc,
                    ox,
                    oy,
                    corner: name,
                    size: t,
                };
            }
            mg += 2;
        }
    }
    if best.ncc <= -2.0 {
        return None;
    }
    for dy in -8i64..=8 {
        for dx in -8i64..=8 {
            let ox = best.ox + dx;
            let oy = best.oy + dy;
            let ncc = score(ox, oy);
            if ncc > best.ncc {
                best.ncc = ncc;
                best.ox = ox;
                best.oy = oy;
            }
        }
    }
    Some(best)
}

/// Per-pixel grayscale in 0..=1 (Rec.709), matching the reference engine's
/// `toGrayscale`. Used by the catalog detector's correlation scoring.
pub fn gray01_of(rgba: &[u8], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    let mut g = vec![0f32; n];
    for p in 0..n {
        let i = p * 4;
        g[p] = (0.2126 * rgba[i] as f32 + 0.7152 * rgba[i + 1] as f32 + 0.0722 * rgba[i + 2] as f32)
            / 255.0;
    }
    g
}

/// Sobel gradient magnitude of a `size×size` tile (borders left at 0), matching
/// the reference engine's `sobelMagnitude` applied to an extracted region.
fn sobel_tile(src: &[f32], size: usize) -> Vec<f32> {
    let mut out = vec![0f32; size * size];
    if size < 3 {
        return out;
    }
    for y in 1..size - 1 {
        for x in 1..size - 1 {
            let i = y * size + x;
            let gx = -src[i - size - 1] - 2.0 * src[i - 1] - src[i + size - 1]
                + src[i - size + 1]
                + 2.0 * src[i + 1]
                + src[i + size + 1];
            let gy = -src[i - size - 1] - 2.0 * src[i - size] - src[i - size + 1]
                + src[i + size - 1]
                + 2.0 * src[i + size]
                + src[i + size + 1];
            out[i] = (gx * gx + gy * gy).sqrt();
        }
    }
    out
}

/// Bilinear resample of a square α-map to a new size (mirrors the reference
/// engine's `interpolateAlphaMap`). Lets the single measured 96px profile seed
/// templates at any catalog size.
pub fn interpolate_alpha(src: &[f32], src_size: usize, target: usize) -> Vec<f32> {
    if target == src_size {
        return src.to_vec();
    }
    let mut out = vec![0f32; target * target];
    let scale = (src_size as f32 - 1.0) / (target.max(2) as f32 - 1.0);
    for y in 0..target {
        let sy = y as f32 * scale;
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(src_size - 1);
        let fy = sy - y0 as f32;
        for x in 0..target {
            let sx = x as f32 * scale;
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(src_size - 1);
            let fx = sx - x0 as f32;
            let p00 = src[y0 * src_size + x0];
            let p10 = src[y0 * src_size + x1];
            let p01 = src[y1 * src_size + x0];
            let p11 = src[y1 * src_size + x1];
            let top = p00 + (p10 - p00) * fx;
            let bot = p01 + (p11 - p01) * fx;
            out[y * target + x] = top + (bot - top) * fy;
        }
    }
    out
}

/// An α-map at an arbitrary tile size, interpolated from the 96px base when no
/// exact map exists. Used so removal applies at the catalog-detected size.
pub fn map_for_size(base96: &AlphaMap, size: usize) -> AlphaMap {
    if size == base96.size {
        return base96.clone();
    }
    AlphaMap::new(size, interpolate_alpha(&base96.a, base96.size, size))
}

/// Normalized cross-correlation of two equal-length signals (Pearson).
fn ncc(a: &[f32], a_mean: f32, b: &[f32], b_mean: f32) -> f32 {
    let mut num = 0f32;
    let (mut da, mut db) = (0f32, 0f32);
    for i in 0..a.len() {
        let x = a[i] - a_mean;
        let y = b[i] - b_mean;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    let den = (da * db).sqrt();
    if den > 1e-8 {
        num / den
    } else {
        0.0
    }
}

#[inline]
fn mean(v: &[f32]) -> f32 {
    v.iter().copied().sum::<f32>() / v.len() as f32
}

/// A template (interpolated α-map + its Sobel magnitude) cached per tile size.
struct Template {
    alpha: Vec<f32>,
    alpha_mean: f32,
    grad: Vec<f32>,
    grad_mean: f32,
}

/// Confidence that a watermark of `size` sits at `(ox, oy)`: a blend of raw-
/// grayscale correlation (the dominant, flat-region-robust signal) and gradient
/// correlation. Returns `None` if the tile is out of bounds.
fn score_region(gray: &[f32], w: usize, h: usize, ox: i64, oy: i64, tpl: &Template, size: usize) -> Option<f32> {
    if ox < 0 || oy < 0 || ox as usize + size > w || oy as usize + size > h {
        return None;
    }
    let (ox, oy) = (ox as usize, oy as usize);
    let mut reg = vec![0f32; size * size];
    for y in 0..size {
        let row = (oy + y) * w + ox;
        let dst = y * size;
        reg[dst..dst + size].copy_from_slice(&gray[row..row + size]);
    }
    let spatial = ncc(&reg, mean(&reg), &tpl.alpha, tpl.alpha_mean);
    let rgrad = sobel_tile(&reg, size);
    let gradient = ncc(&rgrad, mean(&rgrad), &tpl.grad, tpl.grad_mean);
    Some(0.6 * spatial.max(0.0) + 0.4 * gradient.max(0.0))
}

/// Cube-root size penalty: NCC favours tiny templates, so down-weight small
/// tiles to avoid a 48px window scoring high on part of a 96px mark
/// (mirrors `computeSizeAdjustedConfidence`).
#[inline]
fn size_adjust(conf: f32, size: usize) -> f32 {
    conf * (size as f32 / 96.0).cbrt().min(1.0)
}

/// Minimum size-adjusted confidence for a catalog anchor to count as a real
/// watermark. Below this we remove *nothing* rather than risk burning a hole in
/// clean pixels (the failure mode of an unconstrained corner search).
pub const DETECT_THRESHOLD: f32 = 0.10;

/// Locate the watermark by validating the **size catalog's** ordered anchors
/// (see `catalog::search_configs`) and taking the first that clears
/// `DETECT_THRESHOLD`, refined ±6px. This replaces the old free corner sweep,
/// which could lock onto a coincidental correlation peak on a flat region.
pub fn detect_catalog(
    rgba: &[u8],
    w: usize,
    h: usize,
    base96: &AlphaMap,
    configs: &[WmConfig],
) -> Option<Detection> {
    let gray = gray01_of(rgba, w, h);
    let mut cache: Vec<(usize, Template)> = Vec::new();
    let template = |cache: &mut Vec<(usize, Template)>, size: usize| -> usize {
        if let Some(idx) = cache.iter().position(|(s, _)| *s == size) {
            return idx;
        }
        let alpha = interpolate_alpha(&base96.a, base96.size, size);
        let grad = sobel_tile(&alpha, size);
        let (am, gm) = (mean(&alpha), mean(&grad));
        cache.push((
            size,
            Template {
                alpha,
                alpha_mean: am,
                grad,
                grad_mean: gm,
            },
        ));
        cache.len() - 1
    };

    const REFINE: i64 = 8;
    // Evaluate every catalog anchor and keep the one with the highest
    // size-adjusted confidence — rather than the first to clear the bar — so a
    // weak-but-plausible smaller anchor can't pre-empt the true full-size mark.
    let mut winner: Option<(f32, Detection)> = None; // (adjusted, detection)
    for cfg in configs {
        let Some((ox0, oy0)) = cfg.origin(w, h) else {
            continue;
        };
        let size = cfg.size;
        let ti = template(&mut cache, size);
        let mut best: Option<(f32, i64, i64)> = None; // (conf, ox, oy)
        for dy in -REFINE..=REFINE {
            for dx in -REFINE..=REFINE {
                if let Some(conf) = score_region(&gray, w, h, ox0 + dx, oy0 + dy, &cache[ti].1, size) {
                    if best.map_or(true, |b| conf > b.0) {
                        best = Some((conf, ox0 + dx, oy0 + dy));
                    }
                }
            }
        }
        if let Some((conf, ox, oy)) = best {
            let adjusted = size_adjust(conf, size);
            if winner.as_ref().map_or(true, |(a, _)| adjusted > *a) {
                winner = Some((
                    adjusted,
                    Detection {
                        ncc: conf,
                        ox,
                        oy,
                        corner: "br",
                        size,
                    },
                ));
            }
        }
    }
    winner.filter(|(a, _)| *a >= DETECT_THRESHOLD).map(|(_, d)| d)
}

/// Reverse the α-composite over the tile at (ox, oy). `color` is the watermark
/// colour (white for Gemini); `strength` is the opacity calibration (1.0 = 100%).
/// Mirrors `removeAt`.
pub fn remove_at(
    src: &[u8],
    w: usize,
    h: usize,
    map: &AlphaMap,
    ox: i64,
    oy: i64,
    color: [u8; 3],
    strength: f32,
) -> Vec<u8> {
    let t = map.size;
    let a = &map.a;
    let mut out = src.to_vec();
    for y in 0..t {
        let gy = oy + y as i64;
        if gy < 0 || gy >= h as i64 {
            continue;
        }
        for x in 0..t {
            let gx = ox + x as i64;
            if gx < 0 || gx >= w as i64 {
                continue;
            }
            let mut av = a[y * t + x] * strength;
            if av <= 0.002 {
                continue;
            }
            if av > 0.98 {
                av = 0.98;
            }
            let inv = 1.0 - av;
            let i = ((gy as usize) * w + gx as usize) * 4;
            for c in 0..3 {
                let s = src[i + c] as f32;
                out[i + c] = clampf((s - color[c] as f32 * av) / inv, 0.0, 255.0).round() as u8;
            }
        }
    }
    out
}

/// Blind-learn an α profile from a batch of watermarked images using a
/// per-pixel low percentile (no clean originals, no labels). Mirrors the
/// `learnBtn` handler in the web app.
pub fn learn(imgs: &[LoadedImage], corner: &str, size: usize) -> Option<AlphaMap> {
    if imgs.is_empty() {
        return None;
    }
    let t = size;
    let min_w = imgs.iter().map(|i| i.w).min()?;
    let min_h = imgs.iter().map(|i| i.h).min()?;
    let crop = (4 * t).min(min_w).min(min_h);
    if crop < t + 8 {
        return None;
    }
    let ax = |im: &LoadedImage| -> usize {
        if corner == "br" || corner == "tr" {
            im.w - crop
        } else {
            0
        }
    };
    let ay = |im: &LoadedImage| -> usize {
        if corner == "bl" || corner == "br" {
            im.h - crop
        } else {
            0
        }
    };
    let n = imgs.len();
    // stacks[(y*crop+x)*n + k] = luminance of image k at crop pixel (x,y)
    let mut stacks = vec![0f32; n * crop * crop];
    for (k, im) in imgs.iter().enumerate() {
        let l = lum_of(&im.rgba, im.w, im.h);
        let (ox, oy) = (ax(im), ay(im));
        for y in 0..crop {
            for x in 0..crop {
                stacks[(y * crop + x) * n + k] = l[(oy + y) * im.w + (ox + x)];
            }
        }
    }
    // low percentile per pixel ≈ 255*alpha (+ residual background)
    let p_idx = ((0.05 * (n as f32 - 1.0)).round() as i64).clamp(0, n as i64 - 1) as usize;
    let mut pmap = vec![0f32; crop * crop];
    let mut buf = vec![0f32; n];
    for p in 0..crop * crop {
        for k in 0..n {
            buf[k] = stacks[p * n + k];
        }
        buf.sort_by(|a, b| a.partial_cmp(b).unwrap());
        pmap[p] = buf[p_idx];
    }
    // localize the brightest TxT window via an integral image
    let cw = crop + 1;
    let mut ii = vec![0f64; cw * cw];
    for y in 0..crop {
        let mut row = 0f64;
        for x in 0..crop {
            row += pmap[y * crop + x] as f64;
            ii[(y + 1) * cw + (x + 1)] = ii[y * cw + (x + 1)] + row;
        }
    }
    let win = |x: usize, y: usize| -> f64 {
        ii[(y + t) * cw + (x + t)] - ii[y * cw + (x + t)] - ii[(y + t) * cw + x] + ii[y * cw + x]
    };
    let (mut best_s, mut bx, mut by) = (-1f64, 0usize, 0usize);
    for y in 0..=crop - t {
        for x in 0..=crop - t {
            let s = win(x, y);
            if s > best_s {
                best_s = s;
                bx = x;
                by = y;
            }
        }
    }
    // background level from a ring just outside the tile
    let r = 10i64;
    let (mut bs, mut bn) = (0f64, 0usize);
    for y in -r..(t as i64 + r) {
        for x in -r..(t as i64 + r) {
            if x >= 0 && x < t as i64 && y >= 0 && y < t as i64 {
                continue;
            }
            let gx = bx as i64 + x;
            let gy = by as i64 + y;
            if gx < 0 || gy < 0 || gx >= crop as i64 || gy >= crop as i64 {
                continue;
            }
            bs += pmap[gy as usize * crop + gx as usize] as f64;
            bn += 1;
        }
    }
    let pbg = (bs / bn as f64) as f32;
    let mut a = vec![0f32; t * t];
    for y in 0..t {
        for x in 0..t {
            let v = pmap[(by + y) * crop + (bx + x)];
            a[y * t + x] = clampf((v - pbg) / (255.0 - pbg), 0.0, 1.0);
        }
    }
    Some(AlphaMap::new(t, a))
}

/// Decode an image file to RGBA8.
pub fn load_image_from_path(path: &Path) -> Result<LoadedImage, String> {
    let dynimg = image::open(path).map_err(|e| e.to_string())?;
    let rgba = dynimg.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(LoadedImage {
        rgba: rgba.into_raw(),
        w,
        h,
        name,
    })
}

/// Encode RGBA8 to a PNG file.
pub fn save_png(path: &Path, rgba: &[u8], w: usize, h: usize) -> Result<(), String> {
    let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba.to_vec())
        .ok_or("buffer size does not match dimensions")?;
    img.save(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_map() -> AlphaMap {
        let bytes = include_bytes!("../assets/default96.bin");
        let a: Vec<f32> = bytes.iter().map(|&b| b as f32 / 255.0).collect();
        AlphaMap::new(96, a)
    }

    /// Composite the real Gemini α-map over a known gradient, then verify the
    /// detector finds the exact tile and the unblend recovers the background.
    #[test]
    fn roundtrip_recovers_background() {
        let map = default_map();
        let (w, h) = (400usize, 300usize);
        let mut bg = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                bg[i] = ((x * 255) / w) as u8;
                bg[i + 1] = ((y * 255) / h) as u8;
                bg[i + 2] = 128;
                bg[i + 3] = 255;
            }
        }
        let t = 96usize;
        let (ox, oy) = ((w - t - 64) as i64, (h - t - 64) as i64);
        let mut obs = bg.clone();
        for y in 0..t {
            for x in 0..t {
                let av = map.a[y * t + x];
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = bg[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        // Unblend math: removing at the *known* location must recover the
        // background near-exactly (this is the core correctness guarantee).
        let out = remove_at(&obs, w, h, &map, ox, oy, [255, 255, 255], 1.0);
        let mut maxerr = 0i32;
        for i in 0..w * h * 4 {
            maxerr = maxerr.max((out[i] as i32 - bg[i] as i32).abs());
        }
        assert!(maxerr <= 3, "max per-channel error after removal = {maxerr}");

        // Detection: on a smooth gradient there is little to lock onto, so allow
        // a few px of drift — it just needs the right corner and roughly the spot.
        let lum = lum_of(&obs, w, h);
        let det = detect(&lum, w, h, &map).expect("detect should find the watermark");
        assert_eq!(det.corner, "br");
        assert!((det.ox - ox).abs() <= 4, "detected ox {} vs {}", det.ox, ox);
        assert!((det.oy - oy).abs() <= 4, "detected oy {} vs {}", det.oy, oy);
    }

    /// Composite the real Gemini mark at the 96px/64px-margin anchor over a
    /// textured background; the catalog detector must find it at the bottom-right
    /// at size 96 — and must find *nothing* on the same image without a mark
    /// (the regression guard against burning a hole in clean pixels).
    #[test]
    fn catalog_detector_finds_mark_and_rejects_clean() {
        let map = default_map();
        let (w, h) = (1200usize, 1000usize); // ≥1024 tier → 96px candidates
        let mut clean = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                clean[i] = ((x * 31 + y * 17) % 200) as u8;
                clean[i + 1] = ((x * 13 + y * 29) % 180 + 20) as u8;
                clean[i + 2] = ((x * 7 + y * 11) % 160 + 40) as u8;
                clean[i + 3] = 255;
            }
        }
        let (t, m) = (96usize, 64i64);
        let (ox, oy) = (w as i64 - t as i64 - m, h as i64 - t as i64 - m);
        let mut obs = clean.clone();
        for y in 0..t {
            for x in 0..t {
                let av = map.a[y * t + x];
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = obs[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let configs = crate::catalog::search_configs(w, h);
        let det = detect_catalog(&obs, w, h, &map, &configs).expect("should locate the mark");
        assert_eq!(det.corner, "br");
        assert_eq!(det.size, 96);
        assert!((det.ox - ox).abs() <= 8 && (det.oy - oy).abs() <= 8, "at ({},{})", det.ox, det.oy);

        // No watermark present → must not "detect" one (no false-positive removal).
        assert!(
            detect_catalog(&clean, w, h, &map, &configs).is_none(),
            "detector fired on a clean image"
        );
    }

    #[test]
    fn learn_recovers_known_alpha() {
        let truth = default_map();
        let t = 96usize;
        let (w, h) = (200usize, 200usize);
        // Build 30 varied images, watermark fixed at the br corner (margin 32).
        let (ox, oy) = ((w - t - 32) as i64, (h - t - 32) as i64);
        let mut imgs = Vec::new();
        for k in 0..30usize {
            let mut rgba = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    // pseudo-random but deterministic content per image
                    let v = (((x * 7 + y * 13 + k * 53) % 200) + (k * 3) % 40) as u8;
                    rgba[i] = v;
                    rgba[i + 1] = v.wrapping_add(20);
                    rgba[i + 2] = v.wrapping_add(50);
                    rgba[i + 3] = 255;
                }
            }
            for y in 0..t {
                for x in 0..t {
                    let av = truth.a[y * t + x];
                    let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                    for c in 0..3 {
                        let o = rgba[i + c] as f32 * (1.0 - av) + 255.0 * av;
                        rgba[i + c] = o.round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            imgs.push(LoadedImage {
                rgba,
                w,
                h,
                name: format!("img{k}"),
            });
        }
        let learned = learn(&imgs, "br", t).expect("learn should produce a map");
        // Peaks should be close; learned is a lower bound (percentile) of truth.
        let dp = (learned.peak() - truth.peak()).abs();
        assert!(dp < 0.15, "peak alpha mismatch: {} vs {}", learned.peak(), truth.peak());
    }
}
