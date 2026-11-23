use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use gif::{Encoder, Frame, Repeat};
use image::{imageops::FilterType, DynamicImage, Rgba, RgbaImage};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;

const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/TTF/FiraSans-Bold.ttf",
    "/usr/share/fonts/noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
];

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseMode {
    Cycle,
    Random,
    Ramp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GifSettings {
    pub frames: usize,
    pub min_noise: f32,
    pub max_noise: f32,
    pub strength: f32,
    pub duration_ms: u16,
    pub noise_mode: NoiseMode,
    pub limit_width: bool,
    pub max_width: u32,
    pub watermark_enabled: bool,
    pub watermark_text: String,
    pub corner: Corner,
    pub auto_watermark_size: bool,
    pub watermark_size: f32,
    pub watermark_color: [u8; 3],
    pub watermark_opacity: f32,
    pub watermark_shadow: bool,
    pub noise_over_watermark: bool,
}

impl Default for GifSettings {
    fn default() -> Self {
        Self {
            frames: 15,
            min_noise: 20.0,
            max_noise: 60.0,
            strength: 100.0,
            duration_ms: 80,
            noise_mode: NoiseMode::Cycle,
            limit_width: false,
            max_width: 1080,
            watermark_enabled: true,
            watermark_text: "@YapWizard".to_string(),
            corner: Corner::BottomRight,
            auto_watermark_size: true,
            watermark_size: 32.0,
            watermark_color: [255, 255, 255],
            watermark_opacity: 85.0,
            watermark_shadow: true,
            noise_over_watermark: false,
        }
    }
}

pub fn is_supported_image(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
}

pub fn scan_folder(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| is_supported_image(p))
        .collect();
    files.sort();
    files
}

fn load_system_font_bytes() -> Option<Vec<u8>> {
    for path in FONT_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

fn compute_noise_level(
    frame_idx: usize,
    total_frames: usize,
    min_noise: f32,
    max_noise: f32,
    mode: NoiseMode,
    rng: &mut SmallRng,
) -> f32 {
    let min_f = (min_noise / 100.0).clamp(0.0, 1.0);
    let max_f = (max_noise / 100.0).clamp(min_f, 1.0);
    match mode {
        NoiseMode::Random => {
            if (max_f - min_f).abs() < f32::EPSILON {
                min_f
            } else {
                rng.gen_range(min_f..=max_f)
            }
        }
        NoiseMode::Ramp => {
            let t = if total_frames <= 1 {
                0.0
            } else {
                frame_idx as f32 / (total_frames - 1) as f32
            };
            min_f + (max_f - min_f) * t
        }
        NoiseMode::Cycle => {
            let t = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * frame_idx as f32 / total_frames.max(1) as f32).cos());
            min_f + (max_f - min_f) * t
        }
    }
}

/// Fast approximate Gaussian sample (mean 0, std 1) via sum of 4 uniforms (Irwin-Hall).
#[inline]
fn fast_normal(rng: &mut SmallRng) -> f32 {
    let u1: f32 = rng.gen();
    let u2: f32 = rng.gen();
    let u3: f32 = rng.gen();
    let u4: f32 = rng.gen();
    // Sum of 4 U(0,1) has mean 2.0, variance 4/12 = 1/3 -> std = 1/sqrt(3)
    ((u1 + u2 + u3 + u4) - 2.0) * 1.7320508
}

struct TextStyle<'a> {
    font: &'a FontRef<'a>,
    scale: PxScale,
    color: [u8; 3],
    alpha_scale: f32,
}

fn draw_text_onto(img: &mut RgbaImage, font_bytes: &[u8], settings: &GifSettings, font_size: f32) {
    let Ok(font) = FontRef::try_from_slice(font_bytes) else {
        return;
    };
    let text = &settings.watermark_text;
    let (w, h) = img.dimensions();
    let scale = PxScale::from(font_size.max(10.0));
    let scaled = font.as_scaled(scale);

    // Measure text bounding box
    let mut caret_x = 0.0f32;
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    let mut prev_glyph = None;

    for ch in text.chars() {
        let gid = scaled.glyph_id(ch);
        if let Some(prev) = prev_glyph {
            caret_x += scaled.kern(prev, gid);
        }
        let glyph = gid.with_scale_and_position(scale, ab_glyph::point(caret_x, scaled.ascent()));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let b = outlined.px_bounds();
            min_x = min_x.min(b.min.x);
            max_x = max_x.max(b.max.x);
            min_y = min_y.min(b.min.y);
            max_y = max_y.max(b.max.y);
        }
        caret_x += scaled.h_advance(gid);
        prev_glyph = Some(gid);
    }

    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let text_w = max_x - min_x;
    let text_h = max_y - min_y;
    let margin = (w.min(h) as f32 * 0.025).max(10.0);

    let origin_x = match settings.corner {
        Corner::BottomRight | Corner::TopRight => w as f32 - margin - text_w - min_x,
        Corner::BottomLeft | Corner::TopLeft => margin - min_x,
    };
    let origin_y = match settings.corner {
        Corner::BottomRight | Corner::BottomLeft => h as f32 - margin - text_h - min_y,
        Corner::TopRight | Corner::TopLeft => margin - min_y,
    };

    let alpha_scale = (settings.watermark_opacity / 100.0).clamp(0.0, 1.0);
    if settings.watermark_shadow {
        let offset = (font_size / 16.0).max(1.0);
        let shadow_style = TextStyle {
            font: &font,
            scale,
            color: [0, 0, 0],
            alpha_scale: alpha_scale * 0.65,
        };
        rasterize_glyphs(
            img,
            &shadow_style,
            text,
            origin_x + offset,
            origin_y + offset + scaled.ascent(),
        );
    }
    let fg_style = TextStyle {
        font: &font,
        scale,
        color: settings.watermark_color,
        alpha_scale,
    };
    rasterize_glyphs(img, &fg_style, text, origin_x, origin_y + scaled.ascent());
}

fn rasterize_glyphs(
    img: &mut RgbaImage,
    style: &TextStyle<'_>,
    text: &str,
    start_x: f32,
    baseline_y: f32,
) {
    let scaled = style.font.as_scaled(style.scale);
    let (w, h) = img.dimensions();
    let mut caret_x = start_x;
    let mut prev_glyph = None;

    for ch in text.chars() {
        let gid = scaled.glyph_id(ch);
        if let Some(prev) = prev_glyph {
            caret_x += scaled.kern(prev, gid);
        }
        let glyph = gid.with_scale_and_position(style.scale, ab_glyph::point(caret_x, baseline_y));
        if let Some(outlined) = style.font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                    let a = (coverage * style.alpha_scale).clamp(0.0, 1.0);
                    if a > 0.001 {
                        let dst = img.get_pixel_mut(px as u32, py as u32);
                        let inv_a = 1.0 - a;
                        dst[0] = (style.color[0] as f32 * a + dst[0] as f32 * inv_a).round() as u8;
                        dst[1] = (style.color[1] as f32 * a + dst[1] as f32 * inv_a).round() as u8;
                        dst[2] = (style.color[2] as f32 * a + dst[2] as f32 * inv_a).round() as u8;
                        dst[3] = 255;
                    }
                }
            });
        }
        caret_x += scaled.h_advance(gid);
        prev_glyph = Some(gid);
    }
}

pub fn prepare_base_image(img: &DynamicImage, settings: &GifSettings) -> RgbaImage {
    if settings.limit_width && img.width() > settings.max_width && settings.max_width > 0 {
        let new_h = ((img.height() as f64) * (settings.max_width as f64 / img.width() as f64))
            .round()
            .max(1.0) as u32;
        img.resize_exact(settings.max_width, new_h, FilterType::Lanczos3)
            .to_rgba8()
    } else {
        img.to_rgba8()
    }
}

pub fn current_frame_noise_pct(settings: &GifSettings, frame_idx: usize) -> f32 {
    let mut rng = SmallRng::seed_from_u64(
        42u64.wrapping_add((frame_idx as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    );
    compute_noise_level(
        frame_idx,
        settings.frames,
        settings.min_noise,
        settings.max_noise,
        settings.noise_mode,
        &mut rng,
    ) * 100.0
}

pub fn render_single_frame(
    base: &RgbaImage,
    settings: &GifSettings,
    frame_idx: usize,
    font_bytes: Option<&[u8]>,
    seed: u64,
) -> RgbaImage {
    let mut working = base.clone();
    let (w, h) = working.dimensions();
    let wm_size = if settings.auto_watermark_size {
        (w.min(h) as f32 * 0.042).max(16.0)
    } else {
        settings.watermark_size
    };

    if settings.watermark_enabled && settings.noise_over_watermark && !settings.watermark_text.is_empty() {
        if let Some(fb) = font_bytes {
            draw_text_onto(&mut working, fb, settings, wm_size);
        }
    }

    let mut rng = SmallRng::seed_from_u64(
        seed.wrapping_add((frame_idx as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    );
    let level = compute_noise_level(
        frame_idx,
        settings.frames,
        settings.min_noise,
        settings.max_noise,
        settings.noise_mode,
        &mut rng,
    );
    let strength_frac = (settings.strength / 100.0).clamp(0.0, 1.0);
    let std_dev = level * 64.0 * strength_frac;

    if std_dev > 0.001 {
        let buf = working.as_mut();
        for pixel in buf.as_chunks_mut::<4>().0 {
            let delta = fast_normal(&mut rng) * std_dev;
            pixel[0] = (pixel[0] as f32 + delta).clamp(0.0, 255.0) as u8;
            pixel[1] = (pixel[1] as f32 + delta).clamp(0.0, 255.0) as u8;
            pixel[2] = (pixel[2] as f32 + delta).clamp(0.0, 255.0) as u8;
            pixel[3] = 255;
        }
    }

    if settings.watermark_enabled && !settings.noise_over_watermark && !settings.watermark_text.is_empty() {
        if let Some(fb) = font_bytes {
            draw_text_onto(&mut working, fb, settings, wm_size);
        }
    }

    working
}

pub fn generate_gif_file(input: &Path, output: &Path, settings: &GifSettings) -> Result<u64, String> {
    let dyn_img = image::open(input).map_err(|e| format!("Failed to open {}: {e}", input.display()))?;
    let base = prepare_base_image(&dyn_img, settings);
    let (w, h) = base.dimensions();
    if w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err("Image dimensions exceed GIF 65535px limit".to_string());
    }

    let font_bytes = load_system_font_bytes();
    let frame_count = settings.frames.max(1);
    let delay_cs = ((settings.duration_ms as f32) / 10.0).round().max(1.0) as u16;

    // Generate and quantize all frames in parallel across CPU cores
    let encoded_frames: Vec<Frame<'static>> = (0..frame_count)
        .into_par_iter()
        .map(|i| {
            let mut rgba = render_single_frame(&base, settings, i, font_bytes.as_deref(), 42);
            let mut frame = Frame::from_rgba_speed(w as u16, h as u16, rgba.as_mut(), 10);
            frame.delay = delay_cs;
            frame
        })
        .collect();

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create output dir: {e}"))?;
    }
    let file = File::create(output).map_err(|e| format!("Failed to create {}: {e}", output.display()))?;
    let writer = BufWriter::new(file);
    let mut encoder = Encoder::new(writer, w as u16, h as u16, &[])
        .map_err(|e| format!("GIF encoder init error: {e}"))?;
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| format!("GIF repeat error: {e}"))?;

    for frame in encoded_frames {
        encoder
            .write_frame(&frame)
            .map_err(|e| format!("GIF write frame error: {e}"))?;
    }
    drop(encoder);

    let size = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    Ok(size)
}

pub fn load_font_bytes_cached() -> Option<Vec<u8>> {
    load_system_font_bytes()
}

pub fn make_preview_thumbnail(img: &DynamicImage, max_dim: u32) -> RgbaImage {
    let w = img.width();
    let h = img.height();
    if w <= max_dim && h <= max_dim {
        return img.to_rgba8();
    }
    img.resize(max_dim, max_dim, FilterType::Triangle).to_rgba8()
}

#[allow(dead_code)]
fn _ensure_rgba(c: [u8; 3]) -> Rgba<u8> {
    Rgba([c[0], c[1], c[2], 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_pct_stays_within_bounds() {
        let mut settings = GifSettings::default();
        for mode in [NoiseMode::Cycle, NoiseMode::Random, NoiseMode::Ramp] {
            settings.noise_mode = mode;
            for i in 0..settings.frames {
                let pct = current_frame_noise_pct(&settings, i);
                assert!(
                    (19.99..=60.01).contains(&pct),
                    "mode {mode:?} frame {i} gave {pct}%"
                );
            }
        }
    }

    #[test]
    fn watermark_renders_in_requested_corner() {
        let base = RgbaImage::from_pixel(320, 200, Rgba([0, 0, 0, 255]));
        let font_bytes = load_font_bytes_cached().expect("system font should exist");
        let mut settings = GifSettings {
            min_noise: 0.0,
            max_noise: 0.0,
            watermark_enabled: true,
            watermark_text: "@YapWizard".to_string(),
            watermark_shadow: false,
            watermark_opacity: 100.0,
            ..Default::default()
        };

        settings.corner = Corner::BottomRight;
        let br = render_single_frame(&base, &settings, 0, Some(&font_bytes), 1);
        let br_bright = br
            .enumerate_pixels()
            .filter(|(x, y, p)| *x > 160 && *y > 100 && p[0] > 50)
            .count();
        let tl_bright = br
            .enumerate_pixels()
            .filter(|(x, y, p)| *x < 160 && *y < 100 && p[0] > 50)
            .count();
        assert!(br_bright > 50, "expected watermark pixels in bottom-right");
        assert_eq!(tl_bright, 0, "expected no watermark pixels in top-left");
    }

    #[test]
    fn generate_gif_creates_valid_15_frame_gif() {
        let tmp_dir = std::env::temp_dir().join("instagifs_test");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let in_path = tmp_dir.join("input.png");
        let out_path = tmp_dir.join("output.gif");

        let img = RgbaImage::from_pixel(160, 120, Rgba([60, 100, 140, 255]));
        img.save(&in_path).expect("save input png");

        let settings = GifSettings::default();
        let bytes = generate_gif_file(&in_path, &out_path, &settings).expect("generate gif");
        assert!(bytes > 0);

        let file = File::open(&out_path).expect("open output gif");
        let mut decoder = gif::DecodeOptions::new();
        decoder.set_color_output(gif::ColorOutput::RGBA);
        let mut reader = decoder.read_info(file).expect("decode gif");
        let mut count = 0;
        while reader.read_next_frame().expect("read frame").is_some() {
            count += 1;
        }
        assert_eq!(count, 15);

        let _ = std::fs::remove_file(&in_path);
        let _ = std::fs::remove_file(&out_path);
    }
}
