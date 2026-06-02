//! Watermark profile: a set of α-maps keyed by tile size, plus the baked-in
//! Gemini default and JSON import/export (compatible with the web app's format).

use std::collections::BTreeMap;

use crate::engine::AlphaMap;

/// The Gemini ✦ α-map measured from a real watermarked image (harmonic inpaint +
/// white-alpha matting), 96×96, one byte per pixel (value/255 = α).
const DEFAULT96: &[u8] = include_bytes!("../assets/default96.bin");

/// The 48px Gemini mark (used on images with a side < 1024). Derived purely by
/// 2×2 area-downscaling DEFAULT96 — the 48px and 96px marks are the same star at
/// two sizes, so this is a clean-room profile from our own data (it matches an
/// independently-extracted 48px map at cosine 0.9995).
const DEFAULT48: &[u8] = include_bytes!("../assets/default48.bin");

pub struct Profile {
    /// size (px) -> α-map. BTreeMap keeps a stable order for display.
    pub maps: BTreeMap<usize, AlphaMap>,
}

impl Profile {
    /// Profile pre-loaded with the built-in Gemini 96px and 48px maps.
    pub fn with_default() -> Self {
        let mut maps = BTreeMap::new();
        let a96: Vec<f32> = DEFAULT96.iter().map(|&b| b as f32 / 255.0).collect();
        maps.insert(96usize, AlphaMap::new(96, a96));
        let a48: Vec<f32> = DEFAULT48.iter().map(|&b| b as f32 / 255.0).collect();
        maps.insert(48usize, AlphaMap::new(48, a48));
        Profile { maps }
    }

    pub fn set_map(&mut self, m: AlphaMap) {
        self.maps.insert(m.size, m);
    }

    /// Pick which tile size to use for an image of the given dimensions
    /// (96 if ≥1024 in both dims, else 48), falling back to whatever exists.
    pub fn pick_size(&self, w: usize, h: usize) -> Option<usize> {
        if self.maps.is_empty() {
            return None;
        }
        let want = if w >= 1024 && h >= 1024 { 96 } else { 48 };
        if self.maps.contains_key(&want) {
            return Some(want);
        }
        self.maps.keys().next().copied()
    }

    /// Serialize to the web app's JSON shape: `{ "96": [u8;…], "calibration": n }`.
    pub fn to_json(&self, calibration: u32) -> String {
        let mut obj = serde_json::Map::new();
        for (k, m) in &self.maps {
            let arr: Vec<u8> = m.a.iter().map(|v| (v * 255.0).round() as u8).collect();
            obj.insert(k.to_string(), serde_json::json!(arr));
        }
        obj.insert("calibration".into(), serde_json::json!(calibration));
        serde_json::Value::Object(obj).to_string()
    }

    /// Parse a profile JSON, returning the maps and any saved calibration.
    pub fn from_json(s: &str) -> Result<(Profile, Option<u32>), String> {
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
        let obj = v.as_object().ok_or("profile JSON is not an object")?;
        let mut maps = BTreeMap::new();
        let mut calibration = None;
        for (k, val) in obj {
            if k == "calibration" {
                calibration = val.as_u64().map(|x| x as u32);
                continue;
            }
            let size: usize = match k.parse() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let arr = match val.as_array() {
                Some(a) => a,
                None => continue,
            };
            let a: Vec<f32> = arr
                .iter()
                .map(|x| x.as_f64().unwrap_or(0.0) as f32 / 255.0)
                .collect();
            if a.len() == size * size {
                maps.insert(size, AlphaMap::new(size, a));
            }
        }
        if maps.is_empty() {
            return Err("no valid α-maps found in file".into());
        }
        Ok((Profile { maps }, calibration))
    }
}
