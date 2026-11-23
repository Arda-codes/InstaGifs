mod gif_gen;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, ColorImage, Rounding, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Layout, Margin, RichText, Stroke, TextureHandle, TextureOptions, Vec2,
};
use image::RgbaImage;

use gif_gen::{
    current_frame_noise_pct, generate_gif_file, is_supported_image, load_font_bytes_cached,
    make_preview_thumbnail, render_single_frame, scan_folder, Corner, GifSettings, NoiseMode,
};

// --- Graphite & Film-Amber Design Tokens ---
const BG_CANVAS: Color32 = Color32::from_rgb(14, 15, 18);       // #0E0F12
const BG_SIDEBAR: Color32 = Color32::from_rgb(21, 22, 26);      // #15161A
const BG_CONTROL: Color32 = Color32::from_rgb(30, 32, 38);      // #1E2026
const BG_HOVER: Color32 = Color32::from_rgb(40, 43, 51);        // #282B33
const BG_ACTIVE: Color32 = Color32::from_rgb(48, 51, 60);       // #30333C
const BORDER_HAIRLINE: Color32 = Color32::from_rgb(42, 45, 53); // #2A2D35

const TEXT_PRIMARY: Color32 = Color32::from_rgb(241, 242, 244);   // #F1F2F4
const TEXT_SECONDARY: Color32 = Color32::from_rgb(160, 164, 176); // #A0A4B0
const TEXT_MUTED: Color32 = Color32::from_rgb(107, 112, 125);     // #6B707D

const ACCENT_AMBER: Color32 = Color32::from_rgb(229, 169, 60);    // #E5A93C
const STATUS_DONE: Color32 = Color32::from_rgb(107, 203, 119);    // #6BCB77

enum WorkerEvent {
    Progress(usize, usize, String),
    Done(String),
    Error(String),
}

struct InstaGifsApp {
    current_dir: PathBuf,
    folder_images: Vec<PathBuf>,
    selected_image: Option<PathBuf>,
    overwrite_existing: bool,

    settings: GifSettings,
    font_bytes: Option<Vec<u8>>,

    // Live preview state
    preview_base: Option<RgbaImage>,
    orig_dimensions: Option<(u32, u32)>,
    preview_texture: Option<TextureHandle>,
    preview_playing: bool,
    preview_dirty: bool,
    preview_frame: usize,
    last_frame_tick: Instant,

    // Background export state
    worker_rx: Option<Receiver<WorkerEvent>>,
    is_busy: bool,
    progress: Option<(usize, usize)>,
    status_text: String,
}

impl InstaGifsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_fonts(&cc.egui_ctx);
        configure_theme(&cc.egui_ctx);

        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let folder_images = scan_folder(&current_dir);
        let first_image = folder_images.first().cloned();

        let mut app = Self {
            current_dir,
            folder_images,
            selected_image: None,
            overwrite_existing: false,
            settings: GifSettings::default(),
            font_bytes: load_font_bytes_cached(),
            preview_base: None,
            orig_dimensions: None,
            preview_texture: None,
            preview_playing: true,
            preview_dirty: true,
            preview_frame: 0,
            last_frame_tick: Instant::now(),
            worker_rx: None,
            is_busy: false,
            progress: None,
            status_text: "Ready".to_string(),
        };

        if let Some(path) = first_image {
            app.load_image_for_preview(&path);
        }
        app
    }

    fn refresh_folder(&mut self) {
        self.folder_images = scan_folder(&self.current_dir);
        if self.selected_image.as_ref().map(|p| !p.exists()).unwrap_or(true) {
            if let Some(first) = self.folder_images.first().cloned() {
                self.load_image_for_preview(&first);
            }
        }
    }

    fn load_image_for_preview(&mut self, path: &Path) {
        match image::open(path) {
            Ok(dyn_img) => {
                self.orig_dimensions = Some((dyn_img.width(), dyn_img.height()));
                self.preview_base = Some(make_preview_thumbnail(&dyn_img, 760));
                self.selected_image = Some(path.to_path_buf());
                self.preview_frame = 0;
                self.preview_texture = None;
                self.preview_dirty = true;
                self.status_text = format!(
                    "Loaded {}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("image")
                );
            }
            Err(e) => {
                self.status_text = format!("Could not open image: {e}");
            }
        }
    }

    fn start_export(&mut self, jobs: Vec<(PathBuf, PathBuf)>) {
        if jobs.is_empty() {
            self.status_text = "All images already have GIFs (enable Overwrite to regenerate)".to_string();
            return;
        }

        let (tx, rx) = mpsc::channel();
        let settings = self.settings.clone();
        let total = jobs.len();
        self.is_busy = true;
        self.progress = Some((0, total));
        self.worker_rx = Some(rx);

        thread::spawn(move || {
            let mut total_bytes = 0u64;
            for (idx, (input, output)) in jobs.into_iter().enumerate() {
                let name = input
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("image")
                    .to_string();
                let _ = tx.send(WorkerEvent::Progress(idx, total, format!("Exporting {name}...")));
                match generate_gif_file(&input, &output, &settings) {
                    Ok(bytes) => total_bytes += bytes,
                    Err(err) => {
                        let _ = tx.send(WorkerEvent::Error(err));
                        return;
                    }
                }
            }
            let mb = total_bytes as f64 / (1024.0 * 1024.0);
            let _ = tx.send(WorkerEvent::Done(format!(
                "Exported {total} GIF{} ({mb:.2} MB)",
                if total == 1 { "" } else { "s" }
            )));
        });
    }

    fn poll_worker(&mut self) {
        let Some(rx) = &self.worker_rx else {
            return;
        };
        let mut finished = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                WorkerEvent::Progress(idx, total, msg) => {
                    self.progress = Some((idx, total));
                    self.status_text = msg;
                }
                WorkerEvent::Done(msg) => {
                    self.status_text = msg;
                    finished = true;
                }
                WorkerEvent::Error(err) => {
                    self.status_text = format!("Export failed: {err}");
                    finished = true;
                }
            }
        }
        if finished {
            self.is_busy = false;
            self.progress = None;
            self.worker_rx = None;
            self.refresh_folder();
        }
    }

    fn update_preview_texture(&mut self, ctx: &egui::Context) {
        let Some(base) = &self.preview_base else {
            return;
        };

        let delay = Duration::from_millis(self.settings.duration_ms.max(16) as u64);
        let should_advance = self.preview_playing && self.last_frame_tick.elapsed() >= delay;
        if should_advance {
            self.preview_frame = (self.preview_frame + 1) % self.settings.frames.max(1);
            self.last_frame_tick = Instant::now();
        }

        if self.preview_texture.is_none() || should_advance || self.preview_dirty {
            self.preview_dirty = false;
            let mut preview_settings = self.settings.clone();
            if !preview_settings.auto_watermark_size {
                if let Some((orig_w, _)) = self.orig_dimensions {
                    if orig_w > 0 {
                        preview_settings.watermark_size *= base.width() as f32 / orig_w as f32;
                    }
                }
            }
            let frame_rgba = render_single_frame(
                base,
                &preview_settings,
                self.preview_frame % self.settings.frames.max(1),
                self.font_bytes.as_deref(),
                42,
            );
            let color_img = ColorImage::from_rgba_unmultiplied(
                [frame_rgba.width() as usize, frame_rgba.height() as usize],
                frame_rgba.as_raw(),
            );
            match &mut self.preview_texture {
                Some(tex) => tex.set(color_img, TextureOptions::LINEAR),
                None => {
                    self.preview_texture = Some(ctx.load_texture(
                        "live_preview",
                        color_img,
                        TextureOptions::LINEAR,
                    ));
                }
            }
        }

        if self.preview_playing {
            ctx.request_repaint_after(delay);
        }
    }
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Ok(sans_bytes) = std::fs::read("/usr/share/fonts/TTF/FiraSans-Regular.ttf") {
        fonts
            .font_data
            .insert("fira_sans".to_owned(), FontData::from_owned(sans_bytes));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "fira_sans".to_owned());
    }
    if let Ok(mono_bytes) = std::fs::read("/usr/share/fonts/TTF/DejaVuSansMono.ttf") {
        fonts
            .font_data
            .insert("dejavu_mono".to_owned(), FontData::from_owned(mono_bytes));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "dejavu_mono".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn configure_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.slider_width = 148.0;
    style.spacing.interact_size = Vec2::new(36.0, 24.0);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_SIDEBAR;
    visuals.window_fill = BG_SIDEBAR;
    visuals.extreme_bg_color = BG_CANVAS;
    visuals.faint_bg_color = BG_CONTROL;
    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(229, 169, 60, 42);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT_AMBER);

    let radius = Rounding::same(6.0);
    visuals.widgets.noninteractive.bg_fill = BG_SIDEBAR;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER_HAIRLINE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = radius;

    visuals.widgets.inactive.bg_fill = BG_CONTROL;
    visuals.widgets.inactive.weak_bg_fill = BG_CONTROL;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER_HAIRLINE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = radius;

    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.weak_bg_fill = BG_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(66, 70, 82));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = radius;

    visuals.widgets.active.bg_fill = BG_ACTIVE;
    visuals.widgets.active.weak_bg_fill = BG_ACTIVE;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT_AMBER);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.active.rounding = radius;

    style.visuals = visuals;
    ctx.set_style(style);
}

fn section_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    let rect = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.right(), rect.top()),
        ],
        Stroke::new(1.0_f32, BORDER_HAIRLINE),
    );
    ui.add_space(6.0);
    ui.label(RichText::new(title).size(13.0).strong().color(TEXT_PRIMARY));
    ui.add_space(3.0);
}

fn row_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        Vec2::new(92.0, 22.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.label(RichText::new(text).size(12.5).color(TEXT_SECONDARY));
        },
    );
}

fn segmented_noise_mode(ui: &mut egui::Ui, mode: &mut NoiseMode, total_w: f32) {
    let gap = 4.0_f32;
    let btn_w = ((total_w - gap * 2.0) / 3.0).max(48.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (variant, label) in [
            (NoiseMode::Cycle, "Cycle"),
            (NoiseMode::Random, "Random"),
            (NoiseMode::Ramp, "Ramp"),
        ] {
            let selected = *mode == variant;
            let fill = if selected { BG_ACTIVE } else { BG_CONTROL };
            let stroke = if selected {
                Stroke::new(1.0_f32, ACCENT_AMBER)
            } else {
                Stroke::new(1.0_f32, BORDER_HAIRLINE)
            };
            let text_color = if selected { TEXT_PRIMARY } else { TEXT_SECONDARY };
            let btn = egui::Button::new(RichText::new(label).size(12.0).color(text_color))
                .fill(fill)
                .stroke(stroke)
                .min_size(Vec2::new(btn_w, 24.0));
            if ui.add(btn).clicked() {
                *mode = variant;
            }
        }
    });
}

fn corner_selector(ui: &mut egui::Ui, corner: &mut Corner, total_w: f32) {
    let gap = 4.0_f32;
    let btn_w = ((total_w - gap * 3.0) / 4.0).max(36.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (c, label) in [
            (Corner::TopLeft, "TL"),
            (Corner::TopRight, "TR"),
            (Corner::BottomLeft, "BL"),
            (Corner::BottomRight, "BR"),
        ] {
            let selected = *corner == c;
            let fill = if selected { BG_ACTIVE } else { BG_CONTROL };
            let stroke = if selected {
                Stroke::new(1.0_f32, ACCENT_AMBER)
            } else {
                Stroke::new(1.0_f32, BORDER_HAIRLINE)
            };
            let text_color = if selected { ACCENT_AMBER } else { TEXT_SECONDARY };
            let btn = egui::Button::new(
                RichText::new(label)
                    .font(FontId::monospace(11.5))
                    .color(text_color),
            )
            .fill(fill)
            .stroke(stroke)
            .min_size(Vec2::new(btn_w, 24.0));
            if ui.add(btn).clicked() {
                *corner = c;
            }
        }
    });
}

impl eframe::App for InstaGifsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        if self.is_busy {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // Drag-and-drop support
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .filter(|p| is_supported_image(p))
                .collect()
        });
        if let Some(first_dropped) = dropped.first() {
            if let Some(parent) = first_dropped.parent() {
                self.current_dir = parent.to_path_buf();
                self.refresh_folder();
            }
            self.load_image_for_preview(first_dropped);
        }

        // Keyboard transport shortcuts when not typing in a text field
        if !ctx.wants_keyboard_input() {
            let total = self.settings.frames.max(1);
            if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
                self.preview_playing = !self.preview_playing;
                self.last_frame_tick = Instant::now();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.preview_playing = false;
                self.preview_frame = (self.preview_frame + total - 1) % total;
                self.preview_dirty = true;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.preview_playing = false;
                self.preview_frame = (self.preview_frame + 1) % total;
                self.preview_dirty = true;
            }
        }

        // Snapshot settings to detect any live control edits when paused
        let prev_settings = self.settings.clone();

        self.update_preview_texture(ctx);

        // --- Left Inspector Sidebar ---
        egui::SidePanel::left("inspector")
            .resizable(true)
            .default_width(348.0)
            .min_width(328.0)
            .max_width(420.0)
            .frame(
                Frame::none()
                    .fill(BG_SIDEBAR)
                    .inner_margin(Margin::symmetric(16.0, 16.0))
                    .stroke(Stroke::new(1.0_f32, BORDER_HAIRLINE)),
            )
            .show(ctx, |ui| {
                // Header
                ui.horizontal(|ui| {
                    ui.label(RichText::new("InstaGifs").size(17.0).strong().color(TEXT_PRIMARY));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let dir_name = self
                            .current_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(".");
                        ui.label(
                            RichText::new(format!("{dir_name}/"))
                                .font(FontId::monospace(11.5))
                                .color(TEXT_MUTED),
                        );
                    });
                });

                // Bottom-anchored export bar
                egui::TopBottomPanel::bottom("export_footer")
                    .frame(
                        Frame::none()
                            .fill(BG_SIDEBAR)
                            .inner_margin(Margin {
                                left: 0.0,
                                right: 0.0,
                                top: 12.0,
                                bottom: 0.0,
                            }),
                    )
                    .show_inside(ui, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        ui.painter().line_segment(
                            [
                                egui::pos2(rect.left(), rect.top() - 8.0),
                                egui::pos2(rect.right(), rect.top() - 8.0),
                            ],
                            Stroke::new(1.0_f32, BORDER_HAIRLINE),
                        );

                        if let Some((done, total)) = self.progress {
                            let frac = if total > 0 {
                                done as f32 / total as f32
                            } else {
                                0.0
                            };
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .fill(ACCENT_AMBER)
                                    .show_percentage(),
                            );
                            ui.add_space(4.0);
                        }

                        ui.add_enabled_ui(!self.is_busy, |ui| {
                            let has_sel = self.selected_image.is_some();
                            let primary_btn = egui::Button::new(
                                RichText::new("Export Selected GIF")
                                    .size(13.5)
                                    .strong()
                                    .color(BG_CANVAS),
                            )
                            .fill(if has_sel { TEXT_PRIMARY } else { BG_ACTIVE })
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(ui.available_width(), 34.0));

                            if ui.add_enabled(has_sel, primary_btn).clicked() {
                                if let Some(sel) = &self.selected_image {
                                    let out = sel.with_extension("gif");
                                    self.start_export(vec![(sel.clone(), out)]);
                                }
                            }

                            ui.add_space(4.0);

                            let pending_count = self
                                .folder_images
                                .iter()
                                .filter(|p| self.overwrite_existing || !p.with_extension("gif").exists())
                                .count();
                            let batch_label = if self.overwrite_existing {
                                format!("Batch Export All ({pending_count})")
                            } else {
                                format!("Batch Export Pending ({pending_count})")
                            };
                            let secondary_btn = egui::Button::new(
                                RichText::new(batch_label).size(12.5).color(TEXT_PRIMARY),
                            )
                            .fill(BG_CONTROL)
                            .stroke(Stroke::new(1.0_f32, BORDER_HAIRLINE))
                            .min_size(Vec2::new(ui.available_width(), 30.0));

                            if ui
                                .add_enabled(pending_count > 0, secondary_btn)
                                .clicked()
                            {
                                let jobs: Vec<(PathBuf, PathBuf)> = self
                                    .folder_images
                                    .iter()
                                    .filter_map(|p| {
                                        let out = p.with_extension("gif");
                                        if self.overwrite_existing || !out.exists() {
                                            Some((p.clone(), out))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                self.start_export(jobs);
                            }
                        });

                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(&self.status_text)
                                .size(11.5)
                                .color(TEXT_MUTED),
                        );
                    });

                // Scrollable controls
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    section_header(ui, "Source images");

                    let avail_w = ui.available_width();
                    let btn_gap = 6.0_f32;
                    let third_w = ((avail_w - btn_gap * 2.0) / 3.0).max(60.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = btn_gap;
                        if ui
                            .add(egui::Button::new("Open File").min_size(Vec2::new(third_w, 28.0)))
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Images", &["jpg", "jpeg", "png", "webp", "bmp"])
                                .pick_file()
                            {
                                if let Some(parent) = path.parent() {
                                    self.current_dir = parent.to_path_buf();
                                    self.refresh_folder();
                                }
                                self.load_image_for_preview(&path);
                            }
                        }
                        if ui
                            .add(egui::Button::new("Folder").min_size(Vec2::new(third_w, 28.0)))
                            .clicked()
                        {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.current_dir = dir;
                                self.refresh_folder();
                            }
                        }
                        if ui
                            .add(egui::Button::new("Refresh").min_size(Vec2::new(third_w, 28.0)))
                            .clicked()
                        {
                            self.refresh_folder();
                        }
                    });

                    Frame::none()
                        .fill(BG_CANVAS)
                        .stroke(Stroke::new(1.0_f32, BORDER_HAIRLINE))
                        .rounding(Rounding::same(8.0))
                        .inner_margin(Margin::same(6.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("folder_list")
                                .max_height(82.0)
                                .show(ui, |ui| {
                                    if self.folder_images.is_empty() {
                                        ui.label(
                                            RichText::new("Drop an image or pick a folder")
                                                .size(12.0)
                                                .color(TEXT_MUTED),
                                        );
                                    } else {
                                        let paths = self.folder_images.clone();
                                        for path in paths {
                                            let name = path
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("?");
                                            let gif_exists = path.with_extension("gif").exists();
                                            let is_selected = self.selected_image.as_ref() == Some(&path);

                                            let row_fill = if is_selected { BG_ACTIVE } else { Color32::TRANSPARENT };
                                            let row_resp = Frame::none()
                                                .fill(row_fill)
                                                .rounding(Rounding::same(5.0))
                                                .inner_margin(Margin::symmetric(8.0, 5.0))
                                                .show(ui, |ui| {
                                                    ui.set_width(ui.available_width());
                                                    ui.horizontal(|ui| {
                                                        let (dot_rect, _) = ui.allocate_exact_size(
                                                            Vec2::splat(8.0),
                                                            egui::Sense::hover(),
                                                        );
                                                        if gif_exists {
                                                            ui.painter().circle_filled(
                                                                dot_rect.center(),
                                                                3.5,
                                                                STATUS_DONE,
                                                            );
                                                        } else {
                                                            ui.painter().circle_stroke(
                                                                dot_rect.center(),
                                                                3.5,
                                                                Stroke::new(1.2_f32, TEXT_MUTED),
                                                            );
                                                        }
                                                        ui.label(
                                                            RichText::new(name).size(12.5).color(
                                                                if is_selected {
                                                                    TEXT_PRIMARY
                                                                } else {
                                                                    TEXT_SECONDARY
                                                                },
                                                            ),
                                                        );
                                                    });
                                                })
                                                .response
                                                .interact(egui::Sense::click());

                                            if row_resp.clicked() {
                                                self.load_image_for_preview(&path);
                                            }
                                        }
                                    }
                                });
                        });

                    ui.checkbox(&mut self.overwrite_existing, "Overwrite existing GIFs");

                    // --- Grain & Timing (strict 2-column alignment) ---
                    section_header(ui, "Grain & timing");
                    let ctrl_w = (ui.available_width() - 100.0).max(140.0);
                    ui.spacing_mut().slider_width = (ctrl_w - 58.0).max(90.0);

                    ui.horizontal(|ui| {
                        row_label(ui, "Frames");
                        ui.add(egui::Slider::new(&mut self.settings.frames, 1..=60));
                    });
                    ui.horizontal(|ui| {
                        row_label(ui, "Min noise");
                        ui.add(
                            egui::Slider::new(&mut self.settings.min_noise, 0.0..=100.0)
                                .suffix("%")
                                .fixed_decimals(0),
                        );
                    });
                    if self.settings.max_noise < self.settings.min_noise {
                        self.settings.max_noise = self.settings.min_noise;
                    }
                    ui.horizontal(|ui| {
                        row_label(ui, "Max noise");
                        ui.add(
                            egui::Slider::new(&mut self.settings.max_noise, 0.0..=100.0)
                                .suffix("%")
                                .fixed_decimals(0),
                        );
                    });
                    if self.settings.min_noise > self.settings.max_noise {
                        self.settings.min_noise = self.settings.max_noise;
                    }
                    ui.horizontal(|ui| {
                        row_label(ui, "Strength");
                        ui.add(
                            egui::Slider::new(&mut self.settings.strength, 0.0..=100.0)
                                .suffix("%")
                                .fixed_decimals(0),
                        );
                    });
                    ui.horizontal(|ui| {
                        row_label(ui, "Delay");
                        ui.add(
                            egui::Slider::new(&mut self.settings.duration_ms, 20..=300)
                                .suffix(" ms"),
                        );
                    });
                    ui.horizontal(|ui| {
                        row_label(ui, "Curve");
                        segmented_noise_mode(ui, &mut self.settings.noise_mode, ui.available_width());
                    });
                    ui.horizontal(|ui| {
                        row_label(ui, "Cap width");
                        ui.checkbox(&mut self.settings.limit_width, "");
                        ui.add_enabled(
                            self.settings.limit_width,
                            egui::DragValue::new(&mut self.settings.max_width)
                                .range(128..=4096)
                                .suffix(" px"),
                        );
                    });

                    // --- Corner Watermark (strict 2-column alignment) ---
                    section_header(ui, "Watermark");

                    ui.checkbox(&mut self.settings.watermark_enabled, "Show watermark");

                    ui.add_enabled_ui(self.settings.watermark_enabled, |ui| {
                        ui.horizontal(|ui| {
                            row_label(ui, "Handle");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.settings.watermark_text)
                                    .desired_width(ui.available_width()),
                            );
                        });

                        ui.horizontal(|ui| {
                            row_label(ui, "Corner");
                            corner_selector(ui, &mut self.settings.corner, ui.available_width());
                        });

                        ui.horizontal(|ui| {
                            row_label(ui, "Opacity");
                            ui.add(
                                egui::Slider::new(&mut self.settings.watermark_opacity, 0.0..=100.0)
                                    .suffix("%")
                                    .fixed_decimals(0),
                            );
                        });

                        ui.horizontal(|ui| {
                            row_label(ui, "Size");
                            ui.checkbox(&mut self.settings.auto_watermark_size, "Auto");
                            if !self.settings.auto_watermark_size {
                                ui.add(
                                    egui::DragValue::new(&mut self.settings.watermark_size)
                                        .range(10.0..=200.0)
                                        .suffix(" px"),
                                );
                            }
                        });

                        ui.horizontal(|ui| {
                            row_label(ui, "Color");
                            ui.color_edit_button_srgb(&mut self.settings.watermark_color);
                            ui.add_space(6.0);
                            ui.checkbox(&mut self.settings.watermark_shadow, "Shadow");
                        });

                        ui.horizontal(|ui| {
                            row_label(ui, "Blend");
                            ui.checkbox(&mut self.settings.noise_over_watermark, "Grain over text");
                        });
                    });

                    ui.add_space(12.0);
                });
            });

        // --- Right Darkroom Preview Canvas ---
        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(BG_CANVAS)
                    .inner_margin(Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                let frame_total = self.settings.frames.max(1);
                let cur_frame = self.preview_frame % frame_total;
                let cur_noise = current_frame_noise_pct(&self.settings, cur_frame);

                // Transport & Telemetry Bar
                Frame::none()
                    .fill(BG_SIDEBAR)
                    .stroke(Stroke::new(1.0_f32, BORDER_HAIRLINE))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button(if self.preview_playing { "Pause" } else { "Play" })
                                .clicked()
                            {
                                self.preview_playing = !self.preview_playing;
                                self.last_frame_tick = Instant::now();
                            }

                            if ui.button("<").clicked() {
                                self.preview_playing = false;
                                self.preview_frame = (cur_frame + frame_total - 1) % frame_total;
                                self.preview_dirty = true;
                            }
                            if ui.button(">").clicked() {
                                self.preview_playing = false;
                                self.preview_frame = (cur_frame + 1) % frame_total;
                                self.preview_dirty = true;
                            }

                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!(
                                    "{:02} / {:02}",
                                    cur_frame + 1,
                                    frame_total
                                ))
                                .font(FontId::monospace(12.5))
                                .color(ACCENT_AMBER),
                            );

                            ui.label(RichText::new("·").color(TEXT_MUTED));
                            ui.label(
                                RichText::new(format!("Noise {cur_noise:.0}%"))
                                    .font(FontId::monospace(12.0))
                                    .color(TEXT_SECONDARY),
                            );

                            if let Some((w, h)) = self.orig_dimensions {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format!("{w}×{h}"))
                                            .font(FontId::monospace(12.0))
                                            .color(TEXT_MUTED),
                                    );
                                });
                            }
                        });
                    });

                ui.add_space(16.0);

                if let Some(tex) = &self.preview_texture {
                    let avail = ui.available_size();
                    let tex_size = tex.size_vec2();
                    let scale = ((avail.x - 4.0) / tex_size.x)
                        .min((avail.y - 4.0) / tex_size.y)
                        .min(1.0);
                    let display_size = tex_size * scale;

                    let pad_x = ((avail.x - display_size.x - 4.0) * 0.5).max(0.0);
                    let pad_y = ((avail.y - display_size.y - 4.0) * 0.5).max(0.0);
                    ui.add_space(pad_y);
                    ui.horizontal(|ui| {
                        ui.add_space(pad_x);
                        Frame::none()
                            .stroke(Stroke::new(1.0_f32, BORDER_HAIRLINE))
                            .rounding(Rounding::same(4.0))
                            .inner_margin(Margin::same(1.0))
                            .show(ui, |ui| {
                                ui.image((tex.id(), display_size));
                            });
                    });
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("Drop an image here or select one from the sidebar")
                                .size(14.0)
                                .color(TEXT_MUTED),
                        );
                    });
                }
            });

        if self.settings != prev_settings {
            self.preview_dirty = true;
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1160.0, 820.0])
            .with_min_inner_size([780.0, 560.0])
            .with_title("InstaGifs"),
        ..Default::default()
    };
    eframe::run_native(
        "InstaGifs",
        options,
        Box::new(|cc| Ok(Box::new(InstaGifsApp::new(cc)))),
    )
}
