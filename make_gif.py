#!/usr/bin/env python3
"""Auto-generate animated noisy GIFs from images with a customizable corner watermark."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys
from typing import Iterable

import numpy as np
from PIL import Image, ImageColor, ImageDraw, ImageFont

IMAGE_EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp", ".bmp"}
DEFAULT_FONT_CANDIDATES = (
    "/usr/share/fonts/TTF/FiraSans-Bold.ttf",
    "/usr/share/fonts/noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
)


def resolve_font(font_path: str | None, size: int) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Load a TrueType/OpenType font at the requested size, falling back to system fonts."""
    candidates = [font_path] if font_path else list(DEFAULT_FONT_CANDIDATES)
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            try:
                return ImageFont.truetype(candidate, size=size)
            except OSError:
                continue
    return ImageFont.load_default()


def compute_noise_level(
    frame_idx: int,
    num_frames: int,
    min_noise: float,
    max_noise: float,
    mode: str,
    rng: np.random.Generator,
) -> float:
    """Return the normalized noise level in [0.0, 1.0] for a given frame."""
    min_frac = min_noise / 100.0
    max_frac = max_noise / 100.0
    if mode == "random":
        return float(rng.uniform(min_frac, max_frac))
    if mode == "ramp":
        t = frame_idx / max(num_frames - 1, 1)
        return min_frac + (max_frac - min_frac) * t
    # Default 'cycle': smooth cosine wave for a seamless loop
    t = 0.5 * (1.0 - np.cos(2.0 * np.pi * frame_idx / num_frames))
    return min_frac + (max_frac - min_frac) * float(t)


def build_watermark_layer(
    size: tuple[int, int],
    text: str,
    corner: str,
    font_path: str | None,
    font_size: int | None,
    color: str,
    opacity: float,
    margin: int | None,
    shadow: bool,
) -> Image.Image:
    """Create an RGBA overlay image containing the positioned corner watermark."""
    width, height = size
    layer = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    if not text:
        return layer

    draw = ImageDraw.Draw(layer)
    resolved_size = font_size if font_size is not None else max(16, int(round(min(width, height) * 0.042)))
    resolved_margin = margin if margin is not None else max(10, int(round(min(width, height) * 0.025)))
    font = resolve_font(font_path, resolved_size)

    bbox = draw.textbbox((0, 0), text, font=font)
    text_w = bbox[2] - bbox[0]
    text_h = bbox[3] - bbox[1]

    if "right" in corner:
        x = width - resolved_margin - text_w - bbox[0]
    else:
        x = resolved_margin - bbox[0]

    if "bottom" in corner:
        y = height - resolved_margin - text_h - bbox[1]
    else:
        y = resolved_margin - bbox[1]

    rgb = ImageColor.getrgb(color)[:3]
    alpha = int(round(np.clip(opacity / 100.0, 0.0, 1.0) * 255))

    if shadow:
        offset = max(1, resolved_size // 16)
        shadow_alpha = int(round(alpha * 0.65))
        draw.text((x + offset, y + offset), text, font=font, fill=(0, 0, 0, shadow_alpha))

    draw.text((x, y), text, font=font, fill=(*rgb, alpha))
    return layer


def resize_if_needed(im: Image.Image, max_width: int | None, max_height: int | None) -> Image.Image:
    """Downscale the image proportionally if it exceeds max_width or max_height."""
    if max_width is None and max_height is None:
        return im
    w, h = im.size
    scale = 1.0
    if max_width is not None and w > max_width:
        scale = min(scale, max_width / w)
    if max_height is not None and h > max_height:
        scale = min(scale, max_height / h)
    if scale >= 1.0:
        return im
    new_size = (max(1, int(round(w * scale))), max(1, int(round(h * scale))))
    return im.resize(new_size, Image.Resampling.LANCZOS)


def generate_gif(
    input_path: Path,
    output_path: Path,
    *,
    frames: int,
    min_noise: float,
    max_noise: float,
    strength: float,
    duration: int,
    noise_mode: str,
    watermark: str,
    corner: str,
    font_path: str | None,
    watermark_size: int | None,
    watermark_color: str,
    watermark_opacity: float,
    watermark_margin: int | None,
    watermark_shadow: bool,
    noise_over_watermark: bool,
    max_width: int | None,
    max_height: int | None,
    seed: int | None,
) -> None:
    """Transform a single image into a noisy animated GIF with an optional watermark."""
    im = Image.open(input_path).convert("RGB")
    im = resize_if_needed(im, max_width, max_height)

    watermark_layer = build_watermark_layer(
        im.size,
        text=watermark,
        corner=corner,
        font_path=font_path,
        font_size=watermark_size,
        color=watermark_color,
        opacity=watermark_opacity,
        margin=watermark_margin,
        shadow=watermark_shadow,
    )

    if noise_over_watermark and watermark:
        im = Image.alpha_composite(im.convert("RGBA"), watermark_layer).convert("RGB")

    arr = np.asarray(im, dtype=np.float32)
    h, w, _ = arr.shape
    strength_frac = float(np.clip(strength / 100.0, 0.0, 1.0))
    rng = np.random.default_rng(seed)

    gif_frames: list[Image.Image] = []
    for i in range(frames):
        level = compute_noise_level(i, frames, min_noise, max_noise, noise_mode, rng)
        noise = rng.normal(0.0, level * 64.0, (h, w, 1)).astype(np.float32)
        noisy = np.clip(arr + noise, 0.0, 255.0)
        blended = np.clip((1.0 - strength_frac) * arr + strength_frac * noisy, 0.0, 255.0).astype(np.uint8)
        frame_img = Image.fromarray(blended)

        if not noise_over_watermark and watermark:
            frame_img = Image.alpha_composite(frame_img.convert("RGBA"), watermark_layer).convert("RGB")

        gif_frames.append(frame_img.quantize(colors=256, method=Image.Quantize.FASTOCTREE))

    output_path.parent.mkdir(parents=True, exist_ok=True)
    gif_frames[0].save(
        output_path,
        save_all=True,
        append_images=gif_frames[1:],
        duration=duration,
        loop=0,
        optimize=False,
    )


def collect_inputs(targets: list[str], input_dir: Path) -> list[Path]:
    """Resolve input image paths from CLI targets or by scanning input_dir."""
    if not targets:
        return sorted(
            p for p in input_dir.iterdir()
            if p.is_file() and p.suffix.lower() in IMAGE_EXTENSIONS
        )

    resolved: list[Path] = []
    for item in targets:
        p = Path(item)
        if p.is_dir():
            resolved.extend(
                sorted(f for f in p.iterdir() if f.is_file() and f.suffix.lower() in IMAGE_EXTENSIONS)
            )
        elif p.is_file():
            resolved.append(p)
        else:
            print(f"Warning: skipping missing path '{item}'", file=sys.stderr)
    return resolved


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Auto-make noisy animated GIFs from images with a customizable corner watermark."
    )
    parser.add_argument(
        "inputs",
        nargs="*",
        help="Image file(s) or directories to process. Defaults to scanning --input-dir for new images.",
    )
    parser.add_argument("-i", "--input-dir", type=Path, default=Path("."), help="Directory to scan when no inputs are given.")
    parser.add_argument("-o", "--output", type=Path, default=None, help="Output GIF path (single-input mode only).")
    parser.add_argument("--output-dir", type=Path, default=None, help="Directory for generated GIFs (defaults to each image's folder).")
    parser.add_argument("--overwrite", action="store_true", help="Regenerate GIFs even if the output .gif already exists.")

    noise_group = parser.add_argument_group("Noise & Animation")
    noise_group.add_argument("--frames", type=int, default=15, help="Number of GIF frames (default: 15).")
    noise_group.add_argument("--min-noise", type=float, default=20.0, help="Minimum noise %% (default: 20).")
    noise_group.add_argument("--max-noise", type=float, default=60.0, help="Maximum noise %% (default: 60).")
    noise_group.add_argument("--strength", type=float, default=100.0, help="Noise blend strength %% (default: 100).")
    noise_group.add_argument("--duration", type=int, default=80, help="Frame duration in ms (default: 80).")
    noise_group.add_argument(
        "--noise-mode",
        choices=("cycle", "random", "ramp"),
        default="cycle",
        help="How noise varies across frames (default: cycle).",
    )
    noise_group.add_argument("--max-width", type=int, default=None, help="Optional max width to downscale large images.")
    noise_group.add_argument("--max-height", type=int, default=None, help="Optional max height to downscale large images.")
    noise_group.add_argument("--seed", type=int, default=None, help="Optional random seed for reproducible noise.")

    wm_group = parser.add_argument_group("Watermark")
    wm_group.add_argument("--watermark", default="@YapWizard", help="Watermark text (default: '@YapWizard').")
    wm_group.add_argument("--no-watermark", action="store_true", help="Disable the watermark.")
    wm_group.add_argument(
        "--corner",
        choices=("bottom-right", "bottom-left", "top-right", "top-left"),
        default="bottom-right",
        help="Watermark corner (default: bottom-right).",
    )
    wm_group.add_argument("--watermark-size", type=int, default=None, help="Font size in px (default: auto-scaled).")
    wm_group.add_argument("--watermark-color", default="#FFFFFF", help="Watermark color (default: '#FFFFFF').")
    wm_group.add_argument("--watermark-opacity", type=float, default=85.0, help="Watermark opacity %% (default: 85).")
    wm_group.add_argument("--watermark-margin", type=int, default=None, help="Corner margin in px (default: auto-scaled).")
    wm_group.add_argument("--no-watermark-shadow", action="store_true", help="Disable drop shadow behind watermark.")
    wm_group.add_argument("--font", default=None, help="Path to a custom .ttf/.otf font file.")
    wm_group.add_argument(
        "--noise-over-watermark",
        action="store_true",
        help="Apply noise on top of the watermark instead of keeping the text crisp.",
    )
    return parser.parse_args(list(argv) if argv is not None else None)


def main(argv: Iterable[str] | None = None) -> int:
    args = parse_args(argv)
    if args.frames < 1:
        print("Error: --frames must be at least 1.", file=sys.stderr)
        return 1
    if args.min_noise > args.max_noise:
        print("Error: --min-noise cannot exceed --max-noise.", file=sys.stderr)
        return 1

    inputs = collect_inputs(args.inputs, args.input_dir)
    if not inputs:
        print("No input images found.")
        return 0

    if args.output is not None and len(inputs) > 1:
        print("Error: --output can only be used with a single input image; use --output-dir for batch mode.", file=sys.stderr)
        return 1

    watermark_text = "" if args.no_watermark else args.watermark
    auto_skip_existing = not args.inputs and not args.overwrite

    processed = 0
    for img_path in inputs:
        out_path = (
            args.output
            if args.output is not None
            else (args.output_dir / f"{img_path.stem}.gif" if args.output_dir else img_path.with_suffix(".gif"))
        )
        if (auto_skip_existing or not args.overwrite) and out_path.exists() and args.output is None:
            if not args.inputs:
                print(f"Skipping {img_path.name} ({out_path.name} already exists; use --overwrite to regenerate)")
                continue

        print(f"Generating {out_path} from {img_path.name}...")
        generate_gif(
            img_path,
            out_path,
            frames=args.frames,
            min_noise=args.min_noise,
            max_noise=args.max_noise,
            strength=args.strength,
            duration=args.duration,
            noise_mode=args.noise_mode,
            watermark=watermark_text,
            corner=args.corner,
            font_path=args.font,
            watermark_size=args.watermark_size,
            watermark_color=args.watermark_color,
            watermark_opacity=args.watermark_opacity,
            watermark_margin=args.watermark_margin,
            watermark_shadow=not args.no_watermark_shadow,
            noise_over_watermark=args.noise_over_watermark,
            max_width=args.max_width,
            max_height=args.max_height,
            seed=args.seed,
        )
        size_mb = out_path.stat().st_size / (1024 * 1024)
        print(f"  -> Saved {out_path} ({size_mb:.2f} MB)")
        processed += 1

    if processed == 0:
        print("Nothing new to process.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
