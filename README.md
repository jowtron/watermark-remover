# Watermark Remover

A single-file, in-browser tool that removes a fixed semi-transparent watermark (e.g. the Gemini ✦ star) by reversing its alpha composite. No uploads, no server — all processing runs locally in the browser tab.

## How it works

A fixed watermark is laid over an image by alpha compositing:

```
observed = original·(1 − α) + W·α
```

where `α` is the watermark's per-pixel opacity and `W` its colour (white for the Gemini mark). Both are constant, so the operation is reversed exactly:

```
original = (observed − W·α) / (1 − α)
```

The app ships with a Gemini-star α profile **measured from a real watermarked image** via harmonic-inpaint + alpha-matting (peak α ≈ 0.52, white). The watermark's position in each new image is found automatically by correlating the α shape against the local brightening.

The **Learn** tab can derive a profile for any other fixed watermark from a batch of ~20+ watermarked images (no clean originals, no labelling) using a low-percentile blind estimate — the watermark is the one consistent thing across varied images.

## Use

Open `public/index.html` in a browser, drop an image, download the result. No build step or dependencies.

## Notes

- Everything is client-side; no image ever leaves the browser.
- The bundled watermark profile is an independent measurement of a publicly-visible watermark, not data taken from any third-party product.
