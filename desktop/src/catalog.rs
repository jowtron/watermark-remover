//! Gemini watermark **size catalog** — a port of the strategy used by the
//! geminiwatermarkremover.io engine (`src/core/geminiSizeCatalog.js`).
//!
//! Gemini image generation does not emit arbitrary dimensions: the models use a
//! discrete set of official sizes, and the ✦ watermark is stamped at a fixed
//! `{size, marginRight, marginBottom}` for each. So rather than blindly scanning
//! the corners for the best correlation (which can lock onto a coincidental peak
//! on a flat area — see the regression test in `engine.rs`), we enumerate a small
//! ordered list of *known* anchors for the image's dimensions and validate each.
//!
//! Each `WmConfig` is a bottom-right anchor: the tile of `size×size` sits at
//! `(w - margin_right - size, h - margin_bottom - size)`.

/// One candidate watermark geometry (bottom-right anchored).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WmConfig {
    pub size: usize,
    pub margin_right: usize,
    pub margin_bottom: usize,
}

impl WmConfig {
    const fn new(size: usize, margin_right: usize, margin_bottom: usize) -> Self {
        WmConfig {
            size,
            margin_right,
            margin_bottom,
        }
    }
    /// Top-left origin of the tile in an image of the given dimensions, if it fits.
    pub fn origin(&self, w: usize, h: usize) -> Option<(i64, i64)> {
        let x = w as i64 - self.margin_right as i64 - self.size as i64;
        let y = h as i64 - self.margin_bottom as i64 - self.size as i64;
        if x >= 0 && y >= 0 {
            Some((x, y))
        } else {
            None
        }
    }
}

// Canonical geometries (mirrors the named configs in geminiSizeCatalog.js).
const CURRENT_48: WmConfig = WmConfig::new(48, 32, 32); // gemini-3.x 1k / 0.5k
const LEGACY_96: WmConfig = WmConfig::new(96, 64, 64); // older 1k, and 2k/4k tiers
const NEW_MARGIN_96: WmConfig = WmConfig::new(96, 192, 192); // 20260520 large-margin variant
const LARGE_MARGIN_48: WmConfig = WmConfig::new(48, 96, 96); // current 1k large-margin variant

/// Model family — only matters for disambiguating the 1k tier.
#[derive(Clone, Copy)]
enum Family {
    G3x,
    G25Flash,
}
use Family::*;

#[derive(Clone, Copy)]
enum Tier {
    T05,
    T1,
    T2,
    T2NewMargin,
    T4,
}
use Tier::*;

/// One row of the official-size table.
struct Official {
    w: u32,
    h: u32,
    family: Family,
    tier: Tier,
}

const fn o(w: u32, h: u32, family: Family, tier: Tier) -> Official {
    Official { w, h, family, tier }
}

/// The discrete set of official Gemini output sizes (from `OFFICIAL_GEMINI_IMAGE_SIZES`).
#[rustfmt::skip]
const OFFICIAL: &[Official] = &[
    // gemini-3.x-image 0.5k
    o(512,512,G3x,T05), o(256,1024,G3x,T05), o(192,1536,G3x,T05), o(424,632,G3x,T05),
    o(632,424,G3x,T05), o(448,600,G3x,T05), o(1024,256,G3x,T05), o(600,448,G3x,T05),
    o(464,576,G3x,T05), o(576,464,G3x,T05), o(1536,192,G3x,T05), o(384,688,G3x,T05),
    o(688,384,G3x,T05), o(792,168,G3x,T05),
    // gemini-3.x-image 1k
    o(1024,1024,G3x,T1), o(512,2048,G3x,T1), o(384,3072,G3x,T1), o(848,1264,G3x,T1),
    o(1264,848,G3x,T1), o(896,1200,G3x,T1), o(2048,512,G3x,T1), o(1200,896,G3x,T1),
    o(928,1152,G3x,T1), o(1152,928,G3x,T1), o(3072,384,G3x,T1), o(768,1376,G3x,T1),
    o(1376,768,G3x,T1), o(1408,768,G3x,T1), o(1584,672,G3x,T1),
    // gemini-3.x-image 2k
    o(2048,2048,G3x,T2), o(1024,4096,G3x,T2), o(768,6144,G3x,T2), o(1696,2528,G3x,T2),
    o(2528,1696,G3x,T2), o(1792,2400,G3x,T2), o(4096,1024,G3x,T2), o(2400,1792,G3x,T2),
    o(1856,2304,G3x,T2), o(2304,1856,G3x,T2), o(6144,768,G3x,T2), o(1536,2752,G3x,T2),
    o(2752,1536,G3x,T2), o(3168,1344,G3x,T2),
    // gemini-3.x-image 2k-new-margin
    o(2816,1536,G3x,T2NewMargin),
    // gemini-3.x-image 4k
    o(4096,4096,G3x,T4), o(2048,8192,G3x,T4), o(1536,12288,G3x,T4), o(3392,5056,G3x,T4),
    o(5056,3392,G3x,T4), o(3584,4800,G3x,T4), o(8192,2048,G3x,T4), o(4800,3584,G3x,T4),
    o(3712,4608,G3x,T4), o(4608,3712,G3x,T4), o(12288,1536,G3x,T4), o(3072,5504,G3x,T4),
    o(5504,3072,G3x,T4), o(6336,2688,G3x,T4),
    // gemini-2.5-flash-image 1k
    o(1024,1024,G25Flash,T1), o(832,1248,G25Flash,T1), o(1248,832,G25Flash,T1),
    o(864,1184,G25Flash,T1), o(1184,864,G25Flash,T1), o(896,1152,G25Flash,T1),
    o(1152,896,G25Flash,T1), o(768,1344,G25Flash,T1), o(1344,768,G25Flash,T1),
    o(1536,672,G25Flash,T1),
];

/// Primary + secondary configs for an official entry (matches `getEntryConfig` /
/// `getEntryLegacyConfigs`): the 3.x 1k tier currently stamps the 48px mark but
/// older outputs at the same size used the 96px mark, so both are offered.
fn entry_configs(e: &Official) -> Vec<WmConfig> {
    match (e.family, e.tier) {
        (_, T05) => vec![CURRENT_48],
        (G3x, T1) => vec![CURRENT_48, LEGACY_96, LARGE_MARGIN_48],
        (G25Flash, T1) => vec![LEGACY_96],
        (_, T2) | (_, T4) => vec![LEGACY_96],
        (_, T2NewMargin) => vec![NEW_MARGIN_96],
    }
}

fn default_by_tier(w: usize, h: usize) -> WmConfig {
    if w.min(h) >= 1024 {
        LEGACY_96
    } else {
        CURRENT_48
    }
}

/// Project an official anchor into near-official (uniformly scaled) dimensions.
fn project(base: WmConfig, scale_x: f64, scale_y: f64) -> WmConfig {
    let scale = (scale_x + scale_y) / 2.0;
    let size = ((base.size as f64 * scale).round() as i64).clamp(24, 192) as usize;
    WmConfig {
        size,
        margin_right: ((base.margin_right as f64 * scale_x).round() as i64).max(8) as usize,
        margin_bottom: ((base.margin_bottom as f64 * scale_y).round() as i64).max(8) as usize,
    }
}

/// Ordered list of candidate watermark geometries for an image, most-likely first.
///
/// 1. exact official size → its known config(s);
/// 2. otherwise the dimension-tier default (96@64 ≥1024px, else 48@32), plus
///    near-official scaled projections;
/// 3. then lower-priority variants (192px-margin, 48px large-margin, the
///    other tile size) so a fallback exists if the primary doesn't validate.
pub fn search_configs(w: usize, h: usize) -> Vec<WmConfig> {
    let mut out: Vec<WmConfig> = Vec::new();
    let push = |out: &mut Vec<WmConfig>, c: WmConfig| {
        if c.origin(w, h).is_some() && !out.contains(&c) {
            out.push(c);
        }
    };

    // Large images (both sides ≥ 1024) are the 96px tile tier; small images are
    // the 48px tier. We keep candidates within the image's tier: a 48px tile
    // partially overlapping a 96px mark correlates well enough to be picked by
    // mistake, so we must not offer it on a 96px-tier image (and vice-versa).
    let large = w.min(h) >= 1024;

    // (1) exact official match → its known config(s), in catalog order.
    let exact = OFFICIAL
        .iter()
        .find(|e| e.w as usize == w && e.h as usize == h);
    if let Some(e) = exact {
        for c in entry_configs(e) {
            push(&mut out, c);
        }
    } else {
        // (2) tier default, then near-official scaled projections.
        push(&mut out, default_by_tier(w, h));

        let target_ar = w as f64 / h as f64;
        let mut projected: Vec<(f64, WmConfig)> = Vec::new();
        for e in OFFICIAL {
            let (ew, eh) = (e.w as f64, e.h as f64);
            let (sx, sy) = (w as f64 / ew, h as f64 / eh);
            let ar_delta = (target_ar - ew / eh).abs() / (ew / eh);
            let scale_mismatch = (sx - sy).abs() / sx.max(sy);
            if ar_delta > 0.02 || scale_mismatch > 0.12 {
                continue;
            }
            let base = entry_configs(e)[0];
            projected.push((ar_delta * 100.0 + scale_mismatch * 20.0, project(base, sx, sy)));
        }
        projected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (_, c) in projected.into_iter().take(3) {
            push(&mut out, c);
        }
    }

    // (3) tier-appropriate fallback variants (a margin/position can change
    // between Gemini cohorts even at the same output size).
    if large {
        push(&mut out, LEGACY_96); // 96 @ 64
        push(&mut out, NEW_MARGIN_96); // 96 @ 192
    } else {
        push(&mut out, CURRENT_48); // 48 @ 32
        push(&mut out, LARGE_MARGIN_48); // 48 @ 96
        push(&mut out, LEGACY_96); // small images occasionally carry the 96px mark
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_official_3x_1k_prefers_48() {
        let c = search_configs(1024, 1024);
        assert_eq!(c[0], CURRENT_48, "3.x 1k should default to the 48px mark");
        assert!(c.contains(&LEGACY_96), "…but still offer the legacy 96px mark");
    }

    #[test]
    fn large_landscape_defaults_to_96_at_64() {
        // The dog-sign case: 2542×1664 is not official, so the ≥1024 tier
        // default (96px @ 64px margin) must come first.
        let c = search_configs(2542, 1664);
        assert_eq!(c[0], LEGACY_96);
        assert!(c.contains(&NEW_MARGIN_96), "and the 192px-margin variant as a fallback");
    }

    #[test]
    fn small_image_defaults_to_48() {
        let c = search_configs(600, 800);
        assert_eq!(c[0], CURRENT_48);
    }
}
