# InstaGifs

A fast, native Rust desktop tool (`eframe` / `egui`) for turning still images into GIFs without the hassle—built for uploading to Giphy so you can send them to friends as GIFs on Discord and Instagram.

## Features

- **Similar-Tone Grain Engine:** Generates monochromatic luminance noise across frames (default: `15` frames, `20%–60%` noise, `100%` strength, `80 ms` frame delay) with `Cycle`, `Random`, or `Ramp` curves.
- **Customizable Corner Watermark:** Renders crisp TrueType text (default: `@YapWizard`) in any corner (`TL`, `TR`, `BL`, `BR`) with configurable tint color, opacity, auto or manual font size, optional drop shadow, and optional grain-over-text blending.
- **Live Darkroom Preview:** Real-time animated preview with Play/Pause (`Space`) and frame stepping (`←` / `→`), plus drag-and-drop image loading.
- **Parallel Batch Export:** Uses `rayon` to render and quantize GIF frames across all CPU cores. Export the selected image or batch-convert every pending image in a folder.

## Requirements

- Rust toolchain (`cargo`, `rustc`)
- Linux desktop environment (Wayland or X11) with system TrueType fonts (`FiraSans-Bold`, `NotoSans-Bold`, or `DejaVuSans-Bold`)

## Build & Run

```bash
cargo build --release
./target/release/instagifs
```

Or run directly:

```bash
cargo run --release
```

## Tests

```bash
cargo test
```
