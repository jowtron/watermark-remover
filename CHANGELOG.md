# Changelog

All notable changes to the Watermark Remover (web tool + Rust desktop app) are
documented here. Format based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased] — web app + desktop

### Added
- **Auto-opacity.** Per-image opacity calibration (on by default) solves for the
  mark's true strength instead of assuming 100%, fixing the dark/over-saturated
  patch left when the profile is more opaque than a given image's mark.
- **Soften mark edge** (toggle) — dissolves the faint residual outline the
  profile's edge can leave.
- **Rebuild flat background** (toggle) — on gradient/solid backgrounds, fits a
  quadratic surface to the surrounding pixels and rebuilds the area under the
  mark, fully clearing the ghost a single opacity can't. Gated by a flatness
  test, so it never touches textured art.
- **± opacity stepper buttons** for fine manual control next to the slider.
- Shared 1:1 across the web app and the Rust desktop engine. Desktop also gains
  a headless `--learn` profile-training CLI.
- **Batch mode.** Drop multiple images on the Remove tab to clean them all
  in-browser and **download the results as a single ZIP** (store-only writer, no
  dependencies). Click any result to open it in the single-image editor.

### Changed
- **Redesigned the web page** — modern hero, segmented tabs, an icon feature
  grid, and refined cards/controls, with automatic **light/dark theme** that
  follows the visitor's system preference.

## [0.2.0] — 2026-06-22

### Changed
- **Catalog-based watermark detection** (both the web app and the desktop app).
  Auto-detect now validates the *known* Gemini watermark geometries
  (`{size, marginRight, marginBottom}`) for the image's dimensions — exact
  official sizes, near-official scaled projections, tier defaults, and the
  192px-margin / 48px variants — instead of free-scanning all four corners.
  Each candidate is scored on raw-grayscale + gradient cross-correlation and the
  best size-adjusted match is chosen.
- Candidates are kept within the image's tile tier (96px for images ≥1024px on
  both sides, 48px otherwise), so a 48px window can no longer win on part of a
  96px mark.

### Fixed
- The old detector could lock onto a coincidental correlation peak on a flat
  region and over-subtract it into a **black star** (e.g. a sign image where the
  real 96px / 64px-margin mark was missed and a hole was burned in the empty
  background). It could also remove only part of a mark on some sizes
  (e.g. 2816×1536) or pick the wrong corner on busy images.
- When nothing scores like a real watermark, the tool now removes **nothing**
  rather than damaging clean pixels.

### Notes
- Desktop and web share identical detection logic (Rust `detect_catalog` /
  `catalog.rs` ↔ JS `detectCatalog` / `searchConfigs`).
- Desktop macOS build remains a universal (arm64 + x86_64) binary, ad-hoc
  signed (not notarized).

## [0.1.0] — 2026-06-03

### Added
- In-browser watermark remover that reverses the Gemini ✦ alpha composite
  (`original = (observed − 255·α) / (1 − α)`), with a built-in measured Gemini
  profile, manual corner/nudge/colour/calibration controls, a "Teach a different
  watermark" tab that blind-learns a profile from a batch, and profile JSON
  import/export. Deployed to Cloudflare Pages.
- Native **desktop app** (pure Rust + iced) with full feature parity, drag-and-
  drop, and a headless `--batch` mode. Packaged as a universal macOS `.app`.
- Built-in 48px profile (derived by area-downscaling the 96px map) so images with
  a side < 1024px auto-work.
- Unified brand art (favicon, app icon, Open Graph card) generated from one
  parametric sparkle.

[0.2.0]: https://github.com/jowtron/watermark-remover/releases/tag/v0.2.0
[0.1.0]: https://github.com/jowtron/watermark-remover/releases/tag/v0.1.0
