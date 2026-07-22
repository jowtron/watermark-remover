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

/// Central-difference gradient magnitude at every pixel of a size×size tile
/// (edge-clamped). A cheaper, more localized operator than the 3×3 Sobel used
/// for template matching; used by the occlusion-robust TV scoring.
fn grad_tile(src: &[f32], size: usize) -> Vec<f32> {
    let mut out = vec![0f32; size * size];
    for y in 0..size {
        let ym = y.saturating_sub(1) * size;
        let yp = (y + 1).min(size - 1) * size;
        let yc = y * size;
        for x in 0..size {
            let xm = x.saturating_sub(1);
            let xp = (x + 1).min(size - 1);
            let gx = src[yc + xp] - src[yc + xm];
            let gy = src[yp + x] - src[ym + x];
            out[yc + x] = (gx * gx + gy * gy).sqrt();
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
    /// Indices of the α-edge band (|∇α| > 0.02) — where un-blending visibly
    /// changes local structure. The TV fallback score is confined to it.
    edge_band: Vec<usize>,
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

/// Occlusion-robust "removal improves smoothness" score. Un-blending at the
/// right spot dissolves the α-edge structure into the background (art edges
/// crossing the tile contribute the same total variation before and after),
/// while un-blending a clean or wrong tile *introduces* a star-shaped artifact.
/// Returns the relative TV drop over the α-edge band, maximized over a strength
/// grid; > 0 only where a mark plausibly sits. Each gradient is weighted by
/// (1−α·s) to cancel the 1/(1−α·s) noise amplification of the un-blend, which
/// otherwise biases the score toward low strengths.
fn dtv_score(gray: &[f32], w: usize, h: usize, ox: i64, oy: i64, tpl: &Template, size: usize) -> Option<f32> {
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
    let band_grad = |tile: &[f32]| -> Vec<f32> {
        tpl.edge_band
            .iter()
            .map(|&i| {
                let (y, x) = (i / size, i % size);
                let xm = x.saturating_sub(1);
                let xp = (x + 1).min(size - 1);
                let ym = y.saturating_sub(1);
                let yp = (y + 1).min(size - 1);
                let gx = tile[y * size + xp] - tile[y * size + xm];
                let gy = tile[yp * size + x] - tile[ym * size + x];
                (gx * gx + gy * gy).sqrt()
            })
            .collect()
    };
    let tv_o: f32 = band_grad(&reg).iter().sum();
    let mut tv_min = f32::INFINITY;
    let mut cor = vec![0f32; size * size];
    let mut s = 0.30f32;
    while s <= 1.20 + 1e-4 {
        for i in 0..size * size {
            let av = (tpl.alpha[i] * s).min(0.98);
            cor[i] = (reg[i] - av) / (1.0 - av);
        }
        let g = band_grad(&cor);
        let tv: f32 = tpl
            .edge_band
            .iter()
            .zip(g.iter())
            .map(|(&i, gv)| gv * (1.0 - (tpl.alpha[i] * s).min(0.98)))
            .sum();
        tv_min = tv_min.min(tv);
        s += 0.10;
    }
    Some((tv_o - tv_min) / tv_o.max(1e-6))
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
        let cd = grad_tile(&alpha, size);
        let edge_band = (0..size * size).filter(|&i| cd[i] > 0.02).collect();
        cache.push((
            size,
            Template {
                alpha,
                alpha_mean: am,
                grad,
                grad_mean: gm,
                edge_band,
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
    // Occlusion fallback: when no anchor validates convincingly by correlation
    // (art crossing the tile wrecks full-tile NCC, and the mark is invisible
    // where background ≈ mark colour), re-rank the anchors by the TV-drop
    // score, which only looks at the α-edge band and is indifferent to
    // background structure. The score is sharply alignment-sensitive, so it
    // does its own ±REFINE position search from the raw catalog origin.
    const STRONG: f32 = 0.35;
    const DTV_ACCEPT: f32 = 0.07;
    if winner.as_ref().map_or(true, |(a, _)| *a < STRONG) {
        let mut best_tv: Option<(f32, Detection)> = None;
        for cfg in configs {
            let Some((ox0, oy0)) = cfg.origin(w, h) else {
                continue;
            };
            let size = cfg.size;
            let ti = template(&mut cache, size);
            for dy in -REFINE..=REFINE {
                for dx in -REFINE..=REFINE {
                    if let Some(d) = dtv_score(&gray, w, h, ox0 + dx, oy0 + dy, &cache[ti].1, size) {
                        if best_tv.as_ref().map_or(true, |(b, _)| d > *b) {
                            best_tv = Some((
                                d,
                                Detection {
                                    ncc: d,
                                    ox: ox0 + dx,
                                    oy: oy0 + dy,
                                    corner: "br",
                                    size,
                                },
                            ));
                        }
                    }
                }
            }
        }
        if let Some((d, det)) = best_tv {
            if d >= DTV_ACCEPT {
                return Some(det);
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

/// Auto-estimate the opacity calibration (α-scale) for the mark at `(ox, oy)`
/// by paired-pixel regression: each strong-α pixel is paired with the nearest
/// un-marked pixel along 8 directions as its local background estimate `b`,
/// giving a direct per-pixel solve `s = (o − b) / (α (c − b))` from the blend
/// model `o = b(1−αs) + c·αs`; the median over all pairs is the answer.
/// Pairs whose background is too close to the mark colour are skipped — there
/// the blend is insensitive to `s` (white mark on white background), which is
/// also what makes this estimator robust to hard art edges crossing the tile
/// (the failure mode of the residual-correlation method, kept as fallback).
/// Mirrors `calibrateStrength` in the web app.
pub fn calibrate_strength(
    src: &[u8],
    w: usize,
    h: usize,
    map: &AlphaMap,
    ox: i64,
    oy: i64,
    color: [u8; 3],
) -> f32 {
    calibrate_fit(src, w, h, map, ox, oy, color).0
}

/// 3×3 box blur of a square α-map (edge-clamped) — the "softened" template.
fn box3_alpha(a: &[f32], t: usize) -> Vec<f32> {
    let mut out = vec![0f32; t * t];
    for y in 0..t {
        for x in 0..t {
            let mut s = 0f32;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let yy = (y as i64 + dy).clamp(0, t as i64 - 1) as usize;
                    let xx = (x as i64 + dx).clamp(0, t as i64 - 1) as usize;
                    s += a[yy * t + xx];
                }
            }
            out[y * t + x] = s / 9.0;
        }
    }
    out
}

/// The profile blended toward its 3×3 box blur by `softness` ∈ [0,1].
/// Some Gemini cohorts stamp a slightly softer-edged star than the measured
/// template (resampled rendering); removal with the raw template then leaves a
/// bright rim (edge under-removed) and a dark inner ring. `calibrate_fit`
/// measures the blend per image.
pub fn soften_map(map: &AlphaMap, softness: f32) -> AlphaMap {
    if softness <= 0.0 {
        return map.clone();
    }
    let b = box3_alpha(&map.a, map.size);
    let a = map
        .a
        .iter()
        .zip(b.iter())
        .map(|(&v, &bv)| v * (1.0 - softness) + bv * softness)
        .collect();
    AlphaMap::new(map.size, a)
}

/// Jointly fit `(strength, edge softness)` for the mark at `(ox, oy)` by
/// paired-pixel regression: each marked pixel is paired with the nearest
/// un-marked pixel along 8 directions as its local background estimate `b`,
/// giving a direct per-pixel solve `s = (o − b) / (α (c − b))` from the blend
/// model `o = b(1−αs) + c·αs`. For each candidate softness the strength is the
/// median solve over the star core (α ≥ 0.30), and the winning softness is the
/// one whose model leaves the smallest median absolute residual across *all*
/// pairs (which is edge-dominated). Pairs whose background is too close to the
/// mark colour are skipped — there the blend is insensitive to `s`, which is
/// also what makes this robust to hard art edges crossing the tile (the
/// failure mode of the residual-correlation method, kept as fallback).
/// Mirrors `calibrateFit` in the web app.
pub fn calibrate_fit(
    src: &[u8],
    w: usize,
    h: usize,
    map: &AlphaMap,
    ox: i64,
    oy: i64,
    color: [u8; 3],
) -> (f32, f32) {
    let t = map.size;
    let lum_w = 0.299 * color[0] as f32 + 0.587 * color[1] as f32 + 0.114 * color[2] as f32;
    // Observed tile luminance (clamped sampling at image edges).
    let mut o = vec![0f32; t * t];
    for y in 0..t {
        let gy = (oy + y as i64).clamp(0, h as i64 - 1) as usize;
        for x in 0..t {
            let gx = (ox + x as i64).clamp(0, w as i64 - 1) as usize;
            let i = (gy * w + gx) * 4;
            o[y * t + x] =
                0.299 * src[i] as f32 + 0.587 * src[i + 1] as f32 + 0.114 * src[i + 2] as f32;
        }
    }
    const DIRS: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];
    let box3 = box3_alpha(&map.a, t);
    let mut best: Option<(f32, f32, f32)> = None; // (residual, strength, softness)
    for step in 0..=4 {
        let soft = step as f32 * 0.25;
        let a: Vec<f32> = map
            .a
            .iter()
            .zip(box3.iter())
            .map(|(&v, &bv)| v * (1.0 - soft) + bv * soft)
            .collect();
        // (α, observed, background) triples for every usable pair
        let mut pairs: Vec<(f32, f32, f32)> = Vec::new();
        for y in 0..t {
            for x in 0..t {
                let av = a[y * t + x];
                if av < 0.05 {
                    continue;
                }
                for (dx, dy) in DIRS {
                    let (mut cx, mut cy) = (x as i32, y as i32);
                    for _ in 0..40 {
                        cx += dx;
                        cy += dy;
                        if cx < 0 || cy < 0 || cx >= t as i32 || cy >= t as i32 {
                            break;
                        }
                        if a[cy as usize * t + cx as usize] < 0.02 {
                            let b = o[cy as usize * t + cx as usize];
                            if (lum_w - b).abs() >= 40.0 {
                                pairs.push((av, o[y * t + x], b));
                            }
                            break;
                        }
                    }
                }
            }
        }
        let median = |v: &mut Vec<f32>| -> f32 {
            v.sort_by(|p, q| p.partial_cmp(q).unwrap());
            let n = v.len();
            if n % 2 == 1 {
                v[n / 2]
            } else {
                (v[n / 2 - 1] + v[n / 2]) / 2.0
            }
        };
        let mut core: Vec<f32> = pairs
            .iter()
            .filter(|(av, _, _)| *av >= 0.30)
            .map(|(av, ov, b)| (ov - b) / (av * (lum_w - b)))
            .collect();
        if core.len() < 20 {
            continue;
        }
        let s = median(&mut core).clamp(0.30, 1.20);
        let mut res: Vec<f32> = pairs
            .iter()
            .map(|(av, ov, b)| (ov - b - av * s * (lum_w - b)).abs())
            .collect();
        let r = median(&mut res);
        if best.map_or(true, |(br, _, _)| r < br) {
            best = Some((r, s, soft));
        }
    }
    match best {
        Some((_, s, soft)) => (s, soft),
        None => (calibrate_strength_residual(src, w, h, map, ox, oy, color), 0.0),
    }
}

/// The pre-2026-07-22 auto-opacity: the strength at which the un-blended tile
/// stops correlating with the watermark shape (monotone in `s`; scan
/// `[0.30, 1.20]`, linear-interp the zero crossing). Accurate on unoccluded
/// marks but misled by hard high-contrast edges inside the tile; kept only for
/// tiles where paired-pixel regression finds too few usable pairs.
fn calibrate_strength_residual(
    src: &[u8],
    w: usize,
    h: usize,
    map: &AlphaMap,
    ox: i64,
    oy: i64,
    color: [u8; 3],
) -> f32 {
    let t = map.size;
    let a = &map.a;
    let a_mean = map.mean;
    let lum_w = 0.299 * color[0] as f32 + 0.587 * color[1] as f32 + 0.114 * color[2] as f32;
    // Observed tile luminance (clamped sampling at image edges).
    let mut o = vec![0f32; t * t];
    for y in 0..t {
        let gy = (oy + y as i64).clamp(0, h as i64 - 1) as usize;
        for x in 0..t {
            let gx = (ox + x as i64).clamp(0, w as i64 - 1) as usize;
            let i = (gy * w + gx) * 4;
            o[y * t + x] =
                0.299 * src[i] as f32 + 0.587 * src[i + 1] as f32 + 0.114 * src[i + 2] as f32;
        }
    }
    let r = ((t as f32 * 0.10).round() as usize).max(4);
    let a_ss: f32 = a.iter().map(|v| (v - a_mean) * (v - a_mean)).sum();
    // NCC of the high-passed corrected tile against the α-shape at scale `s`.
    let corr_at = |s: f32| -> f32 {
        let mut cor = vec![0f32; t * t];
        for i in 0..t * t {
            let av = (a[i] * s).min(0.98);
            cor[i] = (o[i] - lum_w * av) / (1.0 - av);
        }
        let blur = box_blur(&cor, t, t, r);
        let mut hp = vec![0f32; t * t];
        for i in 0..t * t {
            hp[i] = cor[i] - blur[i];
        }
        let hp_mean = mean(&hp);
        let (mut num, mut dh) = (0f32, 0f32);
        for i in 0..t * t {
            let x = hp[i] - hp_mean;
            num += x * (a[i] - a_mean);
            dh += x * x;
        }
        let den = (dh * a_ss).sqrt();
        if den > 1e-8 {
            num / den
        } else {
            0.0
        }
    };
    let (lo, hi, step) = (0.30f32, 1.20f32, 0.05f32);
    let mut prev_s = lo;
    let mut prev_c = corr_at(lo);
    if prev_c <= 0.0 {
        return lo; // mark already fainter than 0.30·profile
    }
    let mut s = lo + step;
    while s <= hi + 1e-4 {
        let c = corr_at(s);
        if c <= 0.0 {
            let f = prev_c / (prev_c - c);
            return (prev_s + f * (s - prev_s)).clamp(lo, hi);
        }
        prev_s = s;
        prev_c = c;
        s += step;
    }
    hi // still correlated at 1.20 → mark stronger than profile; cap
}

/// Optional cosmetic pass over the removed tile: dissolve the faint residual
/// **outline** that survives when the profile's edge doesn't perfectly match the
/// mark, by locally averaging RGB within the α edge-band (weight ∝ |∇α|). A
/// toggle — it very slightly softens real image lines crossing the mark. Mirrors
/// `featherEdges` in the web app.
pub fn feather_edges(out: &mut [u8], w: usize, h: usize, map: &AlphaMap, ox: i64, oy: i64) {
    let t = map.size;
    let a = &map.a;
    // |∇α|, widened by a 3×3 max and normalized → per-pixel feather weight.
    let mut g = vec![0f32; t * t];
    let mut gmax = 1e-6f32;
    for y in 0..t {
        for x in 0..t {
            let xm = x.saturating_sub(1);
            let xp = (x + 1).min(t - 1);
            let ym = y.saturating_sub(1);
            let yp = (y + 1).min(t - 1);
            let gx = a[y * t + xp] - a[y * t + xm];
            let gy = a[yp * t + x] - a[ym * t + x];
            let m = (gx * gx + gy * gy).sqrt();
            g[y * t + x] = m;
            if m > gmax {
                gmax = m;
            }
        }
    }
    let mut wt = vec![0f32; t * t];
    for y in 0..t {
        for x in 0..t {
            let mut mx = 0f32;
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let yy = (y as i64 + dy).clamp(0, t as i64 - 1) as usize;
                    let xx = (x as i64 + dx).clamp(0, t as i64 - 1) as usize;
                    mx = mx.max(g[yy * t + xx]);
                }
            }
            wt[y * t + x] = (mx / gmax).min(1.0);
        }
    }
    let sample = |gx: i64, gy: i64, c: usize| -> f32 {
        let sx = gx.clamp(0, w as i64 - 1) as usize;
        let sy = gy.clamp(0, h as i64 - 1) as usize;
        out[(sy * w + sx) * 4 + c] as f32
    };
    // Compute all new values from the *current* buffer, then write back, so the
    // 3×3 average never reads already-feathered neighbours.
    let mut newv = vec![0u8; t * t * 3];
    for y in 0..t {
        let gyc = oy + y as i64;
        for x in 0..t {
            let gxc = ox + x as i64;
            if gyc < 0 || gyc >= h as i64 || gxc < 0 || gxc >= w as i64 {
                continue;
            }
            let ww = wt[y * t + x];
            for c in 0..3 {
                let orig = sample(gxc, gyc, c);
                if ww <= 0.02 {
                    newv[(y * t + x) * 3 + c] = orig.round() as u8;
                    continue;
                }
                let mut acc = 0f32;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        acc += sample(gxc + dx, gyc + dy, c);
                    }
                }
                let blur = acc / 9.0;
                newv[(y * t + x) * 3 + c] =
                    clampf(orig * (1.0 - ww) + blur * ww, 0.0, 255.0).round() as u8;
            }
        }
    }
    for y in 0..t {
        let gyc = oy + y as i64;
        for x in 0..t {
            let gxc = ox + x as i64;
            if gyc < 0 || gyc >= h as i64 || gxc < 0 || gxc >= w as i64 {
                continue;
            }
            let i = ((gyc as usize) * w + gxc as usize) * 4;
            for c in 0..3 {
                out[i + c] = newv[(y * t + x) * 3 + c];
            }
        }
    }
}

/// Reconstruct a smooth background under the mark when the surrounding area is
/// flat (a gradient or solid fill), by fitting a quadratic surface to the
/// un-marked pixels and blending it over the mark footprint. On smooth
/// backgrounds a single α-scale can't be exact, so the plain un-blend leaves a
/// faint ghost outline; rebuilding the surface removes it entirely. A flatness
/// gate makes this a **no-op on textured art** (e.g. waves), so it never
/// invents detail. Mirrors `reconstructFlat` in the web app. Runs after
/// `remove_at` (and any feather), mutating `out` in place.
pub fn reconstruct_flat(out: &mut [u8], w: usize, h: usize, map: &AlphaMap, ox: i64, oy: i64) {
    let t = map.size;
    let a = &map.a;
    let pad = 22usize;
    let (ww, wh) = (t + 2 * pad, t + 2 * pad);
    let (wx0, wy0) = (ox - pad as i64, oy - pad as i64);
    // Snapshot the padded window RGB (clamped at image edges) so the fit reads a
    // stable copy while we later mutate `out`.
    let mut win = vec![0f32; ww * wh * 3];
    for ly in 0..wh {
        for lx in 0..ww {
            let gx = (wx0 + lx as i64).clamp(0, w as i64 - 1) as usize;
            let gy = (wy0 + ly as i64).clamp(0, h as i64 - 1) as usize;
            let si = (gy * w + gx) * 4;
            for c in 0..3 {
                win[(ly * ww + lx) * 3 + c] = out[si + c] as f32;
            }
        }
    }
    let lum_at = |i: usize| -> f32 { 0.299 * win[i * 3] + 0.587 * win[i * 3 + 1] + 0.114 * win[i * 3 + 2] };
    let mut lum = vec![0f32; ww * wh];
    for i in 0..ww * wh {
        lum[i] = lum_at(i);
    }
    let blur = box_blur(&lum, ww, wh, 7);
    // Flatness = 1 when the ring around the tile is smooth, → 0 when textured.
    let (mut s, mut ss, mut lsum, mut n) = (0f64, 0f64, 0f64, 0usize);
    for ly in 0..wh {
        for lx in 0..ww {
            if lx >= pad && lx < pad + t && ly >= pad && ly < pad + t {
                continue; // skip the tile interior (holds the mark residual)
            }
            let hp = (lum[ly * ww + lx] - blur[ly * ww + lx]) as f64;
            s += hp;
            ss += hp * hp;
            lsum += lum[ly * ww + lx] as f64;
            n += 1;
        }
    }
    let mean = s / n as f64;
    let flat = (ss / n as f64 - mean * mean).max(0.0).sqrt() as f32;
    let flatness = (1.0 - (flat - 2.5) / 7.0).clamp(0.0, 1.0);
    // Absolute σ misreads dark textured art as flat (σ is small only because
    // the pixels are dark) — also require low *contrast-relative* texture, or
    // the surface fit wipes real texture under the mark.
    let rel = flat / (lsum / n as f64 + 12.0) as f32;
    let flatness = flatness * ((0.12 - rel) / 0.09).clamp(0.0, 1.0);
    if flatness <= 0.05 {
        return; // textured background — leave the un-blend untouched
    }
    // Footprint weight: the star mask, soft-dilated and feathered.
    let mut fp = vec![0f32; ww * wh];
    for ly in 0..wh {
        for lx in 0..ww {
            if lx >= pad && lx < pad + t && ly >= pad && ly < pad + t
                && a[(ly - pad) * t + (lx - pad)] > 0.03
            {
                fp[ly * ww + lx] = 1.0;
            }
        }
    }
    let fp = box_blur(&fp, ww, wh, 2);
    let fp: Vec<f32> = fp.iter().map(|v| (v * 1.6).min(1.0)).collect();
    // Quadratic surface fit on the known background (fp < 0.10) via normal
    // equations. Basis [1, x, y, x², y², xy] with coords scaled to keep the
    // system well-conditioned.
    let basis = |lx: usize, ly: usize| -> [f64; 6] {
        let x = (lx as f64 - ww as f64 / 2.0) / 100.0;
        let y = (ly as f64 - wh as f64 / 2.0) / 100.0;
        [1.0, x, y, x * x, y * y, x * y]
    };
    let mut ata = [[0f64; 6]; 6];
    let mut atb = [[0f64; 3]; 6];
    for ly in 0..wh {
        for lx in 0..ww {
            if fp[ly * ww + lx] >= 0.10 {
                continue;
            }
            let b = basis(lx, ly);
            let px = (ly * ww + lx) * 3;
            for i in 0..6 {
                for j in 0..6 {
                    ata[i][j] += b[i] * b[j];
                }
                for c in 0..3 {
                    atb[i][c] += b[i] * win[px + c] as f64;
                }
            }
        }
    }
    // Gaussian elimination with partial pivoting, solving all 3 channels at once.
    let (mut m, mut rhs) = (ata, atb);
    for col in 0..6 {
        let mut piv = col;
        for r in col + 1..6 {
            if m[r][col].abs() > m[piv][col].abs() {
                piv = r;
            }
        }
        if m[piv][col].abs() < 1e-9 {
            return; // singular (degenerate window) → skip reconstruction
        }
        m.swap(col, piv);
        rhs.swap(col, piv);
        let d = m[col][col];
        for j in 0..6 {
            m[col][j] /= d;
        }
        for c in 0..3 {
            rhs[col][c] /= d;
        }
        for r in 0..6 {
            if r == col {
                continue;
            }
            let f = m[r][col];
            if f == 0.0 {
                continue;
            }
            for j in 0..6 {
                m[r][j] -= f * m[col][j];
            }
            for c in 0..3 {
                rhs[r][c] -= f * rhs[col][c];
            }
        }
    }
    // Evaluate the fitted surface and blend it in over the footprint.
    for ly in 0..wh {
        let gy = wy0 + ly as i64;
        if gy < 0 || gy >= h as i64 {
            continue;
        }
        for lx in 0..ww {
            let wgt = fp[ly * ww + lx] * flatness;
            if wgt <= 0.002 {
                continue;
            }
            let gx = wx0 + lx as i64;
            if gx < 0 || gx >= w as i64 {
                continue;
            }
            let b = basis(lx, ly);
            let i = ((gy as usize) * w + gx as usize) * 4;
            for c in 0..3 {
                let mut surf = 0f64;
                for k in 0..6 {
                    surf += b[k] * rhs[k][c];
                }
                let base = out[i + c] as f32;
                out[i + c] =
                    clampf(base * (1.0 - wgt) + surf as f32 * wgt, 0.0, 255.0).round() as u8;
            }
        }
    }
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

    /// Composite the mark at a known opacity *scale* (0.6) over textured
    /// content, then check the auto-calibrator recovers roughly that scale —
    /// the guard that removal won't over-subtract when the mark is fainter than
    /// the stored profile.
    #[test]
    fn calibrate_recovers_known_scale() {
        let map = default_map();
        let (w, h) = (600usize, 500usize);
        let t = 96usize;
        let (ox, oy) = ((w - t - 64) as i64, (h - t - 64) as i64);
        let true_scale = 0.6f32;
        let mut obs = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                // low-frequency, watermark-uncorrelated background
                let v = (120.0 + 60.0 * ((x as f32 / 40.0).sin() + (y as f32 / 55.0).cos())) as u8;
                obs[i] = v;
                obs[i + 1] = v.wrapping_add(15);
                obs[i + 2] = v.wrapping_sub(10);
                obs[i + 3] = 255;
            }
        }
        for y in 0..t {
            for x in 0..t {
                let av = (map.a[y * t + x] * true_scale).min(0.98);
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = obs[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let s = calibrate_strength(&obs, w, h, &map, ox, oy, [255, 255, 255]);
        assert!(
            (s - true_scale).abs() <= 0.15,
            "calibrated scale {s} vs true {true_scale}"
        );
    }

    /// A hard high-contrast edge crossing the tile must not derail auto-opacity
    /// (the residual-correlation method's failure mode on occluded marks).
    #[test]
    fn calibrate_robust_to_hard_edge() {
        let map = default_map();
        let (w, h) = (500usize, 400usize);
        let t = 96usize;
        let (ox, oy) = ((w - t - 64) as i64, (h - t - 64) as i64);
        let true_scale = 0.6f32;
        let mut obs = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                // hard vertical edge through the middle of the tile:
                // near-black art on the left, bright background on the right
                let v = if x < ox as usize + 40 { 8u8 } else { 210u8 };
                obs[i] = v;
                obs[i + 1] = v;
                obs[i + 2] = v;
                obs[i + 3] = 255;
            }
        }
        for y in 0..t {
            for x in 0..t {
                let av = (map.a[y * t + x] * true_scale).min(0.98);
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = obs[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let s = calibrate_strength(&obs, w, h, &map, ox, oy, [255, 255, 255]);
        assert!(
            (s - true_scale).abs() <= 0.12,
            "calibrated scale {s} vs true {true_scale} despite hard edge"
        );
    }

    /// A mark stamped with a softer edge than the template (a resampled ✦, as
    /// some Gemini cohorts produce) must be detected by `calibrate_fit` as
    /// softness > 0, with the strength still recovered — a uniform-scale fit
    /// leaves a bright rim / dark inner ring there.
    #[test]
    fn calibrate_fits_soft_edge() {
        let map = default_map();
        let (w, h) = (500usize, 400usize);
        let t = 96usize;
        let (ox, oy) = ((w - t - 64) as i64, (h - t - 64) as i64);
        let true_scale = 0.6f32;
        let soft_a = box3_alpha(&map.a, t); // fully softened template
        let mut obs = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = (60.0 + 40.0 * ((x as f32 / 37.0).sin() + (y as f32 / 53.0).cos())) as u8;
                obs[i] = v;
                obs[i + 1] = v;
                obs[i + 2] = v;
                obs[i + 3] = 255;
            }
        }
        for y in 0..t {
            for x in 0..t {
                let av = (soft_a[y * t + x] * true_scale).min(0.98);
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = obs[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let (s, soft) = calibrate_fit(&obs, w, h, &map, ox, oy, [255, 255, 255]);
        assert!(soft >= 0.5, "fitted softness {soft} for a fully soft mark");
        assert!(
            (s - true_scale).abs() <= 0.12,
            "calibrated scale {s} vs true {true_scale}"
        );
        // and an unsoftened mark must fit softness ≈ 0
        let mut obs2 = obs.clone();
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = (60.0 + 40.0 * ((x as f32 / 37.0).sin() + (y as f32 / 53.0).cos())) as u8;
                obs2[i] = v;
                obs2[i + 1] = v;
                obs2[i + 2] = v;
            }
        }
        for y in 0..t {
            for x in 0..t {
                let av = (map.a[y * t + x] * true_scale).min(0.98);
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = obs2[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs2[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let (_, soft2) = calibrate_fit(&obs2, w, h, &map, ox, oy, [255, 255, 255]);
        assert!(soft2 <= 0.25, "fitted softness {soft2} for a sharp mark");
    }

    /// The flatness gate must also be contrast-relative: dark textured art has
    /// a small *absolute* high-pass σ (only because the pixels are dark) and
    /// used to pass as "flat", letting the surface fit wipe real texture.
    #[test]
    fn reconstruct_flat_skips_dark_texture() {
        let map = default_map();
        let (w, h) = (500usize, 400usize);
        let t = 96usize;
        let (ox, oy) = ((w - t - 64) as i64, (h - t - 64) as i64);
        let mut obs = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                // dark background with fine ±6 texture (absolute σ ≈ 4)
                let v = (10 + ((x * 7 + y * 13) % 13) as i32 - 6).clamp(0, 255) as u8;
                obs[i] = v;
                obs[i + 1] = v;
                obs[i + 2] = v;
                obs[i + 3] = 255;
            }
        }
        let before = obs.clone();
        let mut out = obs;
        reconstruct_flat(&mut out, w, h, &map, ox, oy);
        assert_eq!(
            out, before,
            "reconstruct_flat must be a no-op on dark textured background"
        );
    }

    /// A mark that is invisible over most of its footprint (white-on-white)
    /// and crossed by black art defeats full-tile NCC; the TV-drop fallback
    /// must still locate it at the 192px-margin catalog anchor.
    #[test]
    fn dtv_fallback_finds_occluded_mark() {
        let map = default_map();
        let (w, h) = (1400usize, 1200usize);
        let t = 96usize;
        // NEW_MARGIN_96 anchor for a large non-official size
        let (ox, oy) = ((w - t - 192) as i64, (h - t - 192) as i64);
        let mut obs = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                // bright page with mild deterministic grain + a black band
                // crossing the right half of the tile
                let grain = (((x * 7 + y * 13) % 5) as i32 - 2) as i32;
                let v = if (1150..1190).contains(&x) {
                    6i32
                } else {
                    (248 + grain).clamp(0, 255)
                };
                let v = v as u8;
                obs[i] = v;
                obs[i + 1] = v;
                obs[i + 2] = v;
                obs[i + 3] = 255;
            }
        }
        for y in 0..t {
            for x in 0..t {
                let av = (map.a[y * t + x] * 0.6).min(0.98);
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    let v = obs[i + c] as f32 * (1.0 - av) + 255.0 * av;
                    obs[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let configs = crate::catalog::search_configs(w, h);
        let det = detect_catalog(&obs, w, h, &map, &configs)
            .expect("occluded mark must be found via the TV fallback");
        assert_eq!(det.size, 96);
        assert!(
            (det.ox - ox).abs() <= 2 && (det.oy - oy).abs() <= 2,
            "found ({},{}) vs true ({ox},{oy})",
            det.ox,
            det.oy
        );
    }

    /// `reconstruct_flat` must rebuild a smooth gradient under the mark (removing
    /// the ghost) but leave a **textured** background untouched (its flatness
    /// gate is the guard against inventing detail over real art).
    #[test]
    fn reconstruct_flat_rebuilds_smooth_and_skips_textured() {
        let map = default_map();
        let (w, h) = (400usize, 300usize);
        let t = 96usize;
        let (ox, oy) = (200i64, 130i64);
        // --- smooth linear gradient: mark composited then removed at a wrong
        //     (too-strong) scale to leave a residual ghost; reconstruction should
        //     pull the footprint back onto the gradient.
        let mut smooth = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = (40.0 + 0.25 * x as f32 + 0.15 * y as f32).min(255.0) as u8;
                smooth[i] = v;
                smooth[i + 1] = v;
                smooth[i + 2] = v;
                smooth[i + 3] = 255;
            }
        }
        let clean = smooth.clone();
        // composite the mark, then remove 20% too strong → residual ghost
        let mut obs = smooth.clone();
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
        let mut out = remove_at(&obs, w, h, &map, ox, oy, [255, 255, 255], 1.2);
        let ghost_before = footprint_err(&out, &clean, w, &map, ox, oy);
        reconstruct_flat(&mut out, w, h, &map, ox, oy);
        let ghost_after = footprint_err(&out, &clean, w, &map, ox, oy);
        assert!(
            ghost_after < ghost_before * 0.5,
            "reconstruction should roughly halve the residual: {ghost_before:.1} → {ghost_after:.1}"
        );

        // --- high-frequency texture: reconstruction must be a no-op.
        let mut tex = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = (((x * 37 + y * 53) % 2) * 200 + 30) as u8; // checker-ish
                tex[i] = v;
                tex[i + 1] = v;
                tex[i + 2] = v;
                tex[i + 3] = 255;
            }
        }
        let before = tex.clone();
        reconstruct_flat(&mut tex, w, h, &map, ox, oy);
        assert_eq!(before, tex, "reconstruction fired on textured background");
    }

    /// Mean per-channel error inside the mark footprint vs a clean reference.
    fn footprint_err(out: &[u8], clean: &[u8], w: usize, map: &AlphaMap, ox: i64, oy: i64) -> f32 {
        let t = map.size;
        let (mut sum, mut n) = (0f32, 0usize);
        for y in 0..t {
            for x in 0..t {
                if map.a[y * t + x] <= 0.05 {
                    continue;
                }
                let i = ((oy as usize + y) * w + (ox as usize + x)) * 4;
                for c in 0..3 {
                    sum += (out[i + c] as f32 - clean[i + c] as f32).abs();
                    n += 1;
                }
            }
        }
        sum / n as f32
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
