# InstaGifs

Turn still images into GIFs without the hassle. I built this to prep images for Giphy so I can send them to friends as GIFs on Discord and Instagram.

## What it does

- Adds 15 frames of monochromatic luminance grain (default 20% to 60% noise, 100% strength, 80 ms per frame) using Cycle, Random, or Ramp curves.
- Stamps a corner watermark (default `@YapWizard`) in any corner (`TL`, `TR`, `BL`, `BR`) with configurable color, opacity, size, and drop shadow.
- Shows a live looping preview with `Space` to play/pause, `←` and `→` to step frames, and drag-and-drop image loading.
- Exports one image or every pending image in a folder in parallel across CPU cores via `rayon`.

## Requirements

- Rust (`cargo`, `rustc`)
- Linux (Wayland or X11) with a bold sans-serif system font (`FiraSans-Bold`, `NotoSans-Bold`, or `DejaVuSans-Bold`)

## Build and run

```bash
cargo build --release
./target/release/instagifs
```

Or run it directly:

```bash
cargo run --release
```

## Tests

```bash
cargo test
```
