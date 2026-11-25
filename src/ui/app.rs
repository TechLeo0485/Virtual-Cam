//! vcam control panel.
//!
//!   * Install / Uninstall the OBS Virtual Camera (so it appears in Google Meet,
//!     Zoom, Teams, ... as a camera option).
//!   * Turn the camera ON / OFF (start/stop feeding it; ON == webcam is in use).
//!   * Pick the background: blur, an image, or none.
//!   * Live preview with a crop editor and a zoomable output view.

use super::settings::{Settings, CROP_MIN};
use super::theme::{self, ThemeChoice};
use super::view::fit_in_box;
use super::widgets as w;
use super::*;
use crate::background;
use crate::color::{self, COLOR_LABELS, CUSTOM_COLOR_PRESET, DEFAULT_COLOR_PRESET, DELL_PRESETS};
use crate::engine::{available_devices, Device};
use crate::compositor::THRESHOLD_MAX;
use crate::geometry::{crop_pixels, ZOOM_MAX, ZOOM_MIN};
use crate::imgproc;
use crate::models;
use crate::virtualcam;
use crate::worker::{BgMode, RunState, Status, VCamWorker, VcamMode};
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Height kept free under the preview box for its status rows and the gallery
/// (title + two rows of tiles), so a tall preview never pushes the gallery out.
const BELOW_PREVIEW: f32 = 330.0;
/// px: how close to a corner counts as grabbing it
const CROP_HIT: f32 = 12.0;
/// px: half the size of a corner point
const CROP_HANDLE: f32 = 5.0;
const RED: Color32 = Color32::from_rgb(0xff, 0x20, 0x20);
const FULL_UV: Rect = Rect { min: Pos2 { x: 0.0, y: 0.0 }, max: Pos2 { x: 1.0, y: 1.0 } };

#[derive(Clone, Copy, Debug)]
enum CropDrag {
    /// 0 TL, 1 TR, 2 BR, 3 BL — the opposite corner stays where it is.
    Corner(usize),
    /// Move the whole rectangle, keeping its size.
    Move { sx: f32, sy: f32, start: [f32; 4] },
}

pub struct VcamApp {
    worker: VCamWorker,
    // options
    cam_idx: usize,
    bg_enabled: bool,
    blur_image: bool,
    blur_strength: f32,
    speed_idx: usize,
    quality_idx: usize,
    model: String,
    mirror: bool,
    one_person: bool,
    devices: Vec<Device>,
    device: Device,
    rotation: u32,
    out_res: (u32, u32),
    zoom: f32,
    /// matte threshold, 0 = off
    threshold: f32,
    color_preset: String,
    color_custom: [i32; 5],
    bg_path: Option<String>,
    gal_folder: Option<String>,
    show_bounds: bool,
    // crop editor
    crop: [f32; 4],
    crop_drag: Option<CropDrag>,
    /// where the image sits on the crop canvas: x, y, w, h
    crop_area: (f32, f32, f32, f32),
    crop_tex: Option<TextureHandle>,
    /// size of the camera image shown in the crop editor (None = camera off)
    crop_frame: Option<(f32, f32)>,
    // output preview
    tab: usize,
    pv_geom: Option<(f32, f32)>,
    out_tex: Option<TextureHandle>,
    last_draw: Instant,
    // gallery: tiles filled in by a background thumbnail thread; `gal_gen` invalidates
    // a stale load when another folder is browsed mid-way.
    tiles: Vec<(String, TextureHandle)>,
    thumb_rx: Option<Receiver<(u64, String, Vec<u8>)>>,
    gal_gen: Arc<AtomicU64>,
    // virtual camera install / uninstall
    vc_text: String,
    vc_tone: w::Tone,
    theme: ThemeChoice,
    /// names of the capture devices, in index order (refreshed when the dropdown opens)
    cameras: Vec<String>,
    busy: bool,
    vc_rx: Option<Receiver<(bool, String)>>,
    allow_close: bool,
    /// the output preview fills the whole screen (F11 / the Full view button)
    full_view: bool,
    full_since: Instant,
}

fn message(level: rfd::MessageLevel, title: &str, text: &str) {
    rfd::MessageDialog::new()
        .set_level(level)
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        std::fs::canonicalize(p)
            .map(|c| c.to_string_lossy().to_lowercase())
            .unwrap_or_else(|_| p.replace('/', "\\").to_lowercase())
    };
    !a.is_empty() && !b.is_empty() && norm(a) == norm(b)
}

/// Thumbnail of an image file: cover-fit to the tile size, RGB bytes.
fn make_thumb(path: &Path) -> Option<Vec<u8>> {
    let img = background::decode_image(path).ok()?;
    let thumb = background::resize_cover(&img, THUMB_W, THUMB_H);
    Some(imgproc::bgr_to_rgb_bytes(&thumb.data))
}

impl VcamApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Restore the last gallery folder / image; else fall back to an image named
        // 2.png next to the app if there is one.
        let settings = Settings::load();
        let bg_path = settings.image.clone().or_else(|| {
            crate::util::find_near_exe("2.png").filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
        });
        let mut app = VcamApp {
            worker: VCamWorker::new(),
            cam_idx: 0,
            bg_enabled: settings.bg_enabled,
            blur_image: false,
            blur_strength: BLUR_DEFAULT,
            speed_idx: 0,
            quality_idx: QUALITY_DEFAULT_INDEX,
            model: settings.model.clone(),
            mirror: false,
            one_person: true,
            devices: available_devices(),
            device: Device::Auto,
            rotation: settings.rotation,
            out_res: settings.output,
            zoom: settings.zoom,
            threshold: settings.threshold,
            color_preset: DEFAULT_COLOR_PRESET.to_string(),
            color_custom: [0; 5],
            bg_path,
            gal_folder: settings.folder.clone(),
            show_bounds: settings.show_bounds,
            crop: settings.crop,
            crop_drag: None,
            crop_area: (0.0, 0.0, 480.0, 270.0),
            crop_tex: None,
            crop_frame: None,
            tab: 0,
            pv_geom: None,
            out_tex: None,
            last_draw: Instant::now() - Duration::from_secs(1),
            tiles: Vec::new(),
            thumb_rx: None,
            gal_gen: Arc::new(AtomicU64::new(0)),
            vc_text: String::new(),
            vc_tone: w::Tone::Muted,
            theme: settings.theme,
            cameras: crate::camera::list_cameras().unwrap_or_default(),
            busy: false,
            vc_rx: None,
            allow_close: false,
            full_view: false,
            full_since: Instant::now(),
        };
        app.refresh_vc_status();
        app.apply_color(); // push the colour defaults to the worker
        if let Some(folder) = app.gal_folder.clone() {
            app.load_folder(Some(folder));
        }
        app
    }

    // -------------------------------------------------------------- actions --

    /// An image background once one is picked; live blur of the real room until then.
    fn mode(&self) -> BgMode {
        if !self.bg_enabled {
            BgMode::Off // virtual background OFF: plain camera on black
        } else if self.bg_path.is_some() {
            BgMode::Image
        } else {
            BgMode::Blur
        }
    }

    fn crop_cfg(&self) -> Option<[f32; 4]> {
        let r = |t: f32| (t * 10000.0).round() / 10000.0;
        let c = [r(self.crop[0]), r(self.crop[1]), r(self.crop[2]), r(self.crop[3])];
        if c == [0.0, 0.0, 1.0, 1.0] {
            None
        } else {
            Some(c)
        }
    }

    fn save_settings(&self) {
        Settings {
            folder: self.gal_folder.clone(),
            image: self.bg_path.clone(),
            crop: self.crop,
            bg_enabled: self.bg_enabled,
            model: self.model.clone(),
            rotation: self.rotation,
            output: self.out_res,
            zoom: self.zoom,
            threshold: self.threshold,
            show_bounds: self.show_bounds,
            theme: self.theme,
        }
        .save();
    }

    fn refresh_vc_status(&mut self) {
        let st = virtualcam::virtualcam_status();
        if st.installed {
            self.vc_text = "Installed \u{2014} \"OBS Virtual Camera\" is available".to_string();
            self.vc_tone = w::Tone::Ok;
        } else if st.obs_found {
            self.vc_text = "Not registered \u{2014} click Install.".to_string();
            self.vc_tone = w::Tone::Warn;
        } else {
            self.vc_text = "OBS Studio not found. Install it from obsproject.com, then Install here.".to_string();
            self.vc_tone = w::Tone::Error;
        }
    }

    fn vcam_action(&mut self, install: bool) {
        if self.busy {
            return;
        }
        let verb = if install { "Install" } else { "Uninstall" };
        if !install && self.worker.status().running {
            message(rfd::MessageLevel::Warning, "Virtual Camera", "Turn the camera OFF first.");
            return;
        }
        let answer = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Info)
            .set_title("Virtual Camera")
            .set_description(format!(
                "{} the OBS Virtual Camera now?\n\nThis needs administrator rights \u{2014} a Windows UAC prompt will appear.",
                verb
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if !matches!(answer, rfd::MessageDialogResult::Yes) {
            return;
        }
        self.busy = true;
        self.vc_text = format!("{}ing\u{2026}", verb);
        self.vc_tone = w::Tone::Muted;
        let (tx, rx) = mpsc::channel();
        self.vc_rx = Some(rx);
        std::thread::spawn(move || {
            let result = if install { virtualcam::install_virtualcam() } else { virtualcam::uninstall_virtualcam() };
            let _ = tx.send(result);
        });
    }

    fn poll_vcam_action(&mut self) {
        let done = match self.vc_rx.as_ref() {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some((ok, msg)) = done {
            self.vc_rx = None;
            self.busy = false;
            self.refresh_vc_status();
            let level = if ok { rfd::MessageLevel::Info } else { rfd::MessageLevel::Error };
            message(level, "Virtual Camera", &msg);
        }
    }

    fn toggle(&mut self) {
        let st = self.worker.status();
        if st.running || st.state == RunState::Starting {
            self.worker.stop();
            return;
        }
        let (cw, ch) = SPEED_MODES[self.speed_idx].1;
        let mode = self.mode();
        let crop = self.crop_cfg();
        let quality = QUALITY_PRESETS[self.quality_idx].1;
        self.worker.configure(|c| {
            c.camera = self.cam_idx;
            c.model = Some(self.model.clone());
            c.mode = mode;
            c.bg_path = self.bg_path.as_ref().map(PathBuf::from);
            c.blur_image = self.blur_image;
            c.blur_strength = self.blur_strength;
            c.mirror = self.mirror;
            c.multi_person = !self.one_person;
            c.downsample = Some(quality);
            c.device = self.device;
            c.rotation = self.rotation;
            c.crop = crop;
            c.zoom = self.zoom;
            c.threshold = self.threshold;
            c.width = cw;
            c.height = ch;
            c.out_width = Some(self.out_res.0);
            c.out_height = Some(self.out_res.1);
            c.vcam = VcamMode::Required;
        });
        self.worker.start();
    }

    fn apply_bg(&mut self) {
        self.worker.set_background(self.mode(), self.bg_path.as_ref().map(PathBuf::from), None, Some(self.blur_image));
    }

    /// Live colour update: the preset name + the five values go to the running worker
    /// (no camera restart, no reconnect).
    fn apply_color(&mut self) {
        let preset = self.color_preset.clone();
        let values = self.color_custom;
        self.worker.configure(|c| {
            c.color_preset = preset;
            c.color_values = values;
        });
    }

    /// Live: no reconnect. 1.0 = actual size, 2.0 = half size (bottom centre), 0.5 = 2x zoom in.
    fn apply_zoom(&mut self) {
        let z = ((self.zoom * 10.0).round() / 10.0).clamp(ZOOM_MIN, ZOOM_MAX);
        self.zoom = z;
        self.worker.configure(|c| c.zoom = z);
    }

    fn set_crop(&mut self, rect: [f32; 4], commit: bool) {
        self.crop = rect;
        if commit {
            self.commit_crop();
        }
    }

    /// Applied when the mouse is released: the camera reconnects once (like Rotate).
    fn commit_crop(&mut self) {
        let crop = self.crop_cfg();
        self.worker.configure(|c| c.crop = crop);
        self.save_settings();
    }

    // -------------------------------------------------------------- gallery --

    fn gallery_title(&self) -> String {
        let name = self
            .gal_folder
            .as_ref()
            .and_then(|f| Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        if name.is_empty() {
            "Background images".to_string()
        } else {
            format!("Background images \u{2014} {}", name)
        }
    }

    fn browse_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose a folder of background images");
        if let Some(dir) = self.gal_folder.as_ref() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(folder) = dialog.pick_folder() {
            let folder = folder.to_string_lossy().into_owned();
            self.gal_folder = Some(folder.clone());
            self.save_settings();
            self.load_folder(Some(folder));
        }
    }

    /// Clear the gallery and thumbnail `folder`'s images on a background thread (big
    /// photos take a while to decode; the UI stays responsive). Tiles appear as they're
    /// ready, via `drain_thumbs`.
    fn load_folder(&mut self, folder: Option<String>) {
        let gen = self.gal_gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.tiles.clear();
        self.thumb_rx = None;
        let folder = match folder {
            Some(f) => f,
            None => return, // just cleared the gallery
        };
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&folder) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .map(|e| IMAGE_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
                            .unwrap_or(false)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
        let (tx, rx) = mpsc::channel();
        self.thumb_rx = Some(rx);
        let current = self.gal_gen.clone();
        std::thread::spawn(move || {
            for p in files {
                if current.load(Ordering::SeqCst) != gen {
                    return; // another folder was browsed; abandon
                }
                // unreadable/corrupt image: skip it
                if let Some(rgb) = make_thumb(&p) {
                    if tx.send((gen, p.to_string_lossy().into_owned(), rgb)).is_err() {
                        return;
                    }
                }
            }
        });
    }

    /// Turn finished thumbnails into tiles.
    fn drain_thumbs(&mut self, ctx: &egui::Context) {
        let mut ready = Vec::new();
        if let Some(rx) = self.thumb_rx.as_ref() {
            while let Ok(item) = rx.try_recv() {
                ready.push(item);
                if ready.len() >= 16 {
                    break;
                }
            }
        }
        let current = self.gal_gen.load(Ordering::SeqCst);
        for (gen, path, rgb) in ready {
            if gen == current && rgb.len() == THUMB_W * THUMB_H * 3 {
                let img = egui::ColorImage::from_rgb([THUMB_W, THUMB_H], &rgb);
                let tex = ctx.load_texture(format!("thumb:{}", path), img, TextureOptions::LINEAR);
                self.tiles.push((path, tex));
            }
        }
    }

    fn select_image(&mut self, path: String) {
        // Tiles are thumbnailed once; the folder may since have been moved, renamed or
        // deleted. Don't hand the worker a dead path (it would silently keep the live
        // blur) — rescan, or ask for the folder again.
        if !Path::new(&path).is_file() {
            let folder_ok = self.gal_folder.as_ref().map(|f| Path::new(f).is_dir()).unwrap_or(false);
            if folder_ok {
                let name = crate::util::file_name(Path::new(&path));
                message(
                    rfd::MessageLevel::Warning,
                    "Background image",
                    &format!("\"{}\" is no longer in this folder.\n\nThe gallery has been refreshed.", name),
                );
                let folder = self.gal_folder.clone();
                self.load_folder(folder);
            } else {
                message(
                    rfd::MessageLevel::Warning,
                    "Background image",
                    &format!(
                        "The folder \"{}\" was moved or deleted.\n\nClick Browse\u{2026} to choose it again.",
                        self.gal_folder.clone().unwrap_or_default()
                    ),
                );
                self.gal_folder = None;
                self.load_folder(None);
                self.save_settings();
            }
            return;
        }
        self.bg_path = Some(path.clone());
        self.bg_enabled = true; // picking a background turns the virtual background on
        self.worker.set_background(BgMode::Image, Some(PathBuf::from(path)), None, Some(self.blur_image));
        self.save_settings();
    }

    // ------------------------------------------------------------------ UI --

    fn camera_label(cameras: &[String], i: usize) -> String {
        match cameras.get(i) {
            Some(name) => format!("{}: {}", i, name),
            None => format!("Camera {}", i),
        }
    }

    fn ui_controls(&mut self, ui: &mut egui::Ui, st: &Status) {
        const RECONNECTS: &str = "Changing this briefly reconnects the camera.";

        // 1. Virtual camera install/uninstall
        let mut install_clicked = false;
        let mut uninstall_clicked = false;
        let (vc_text, vc_tone, busy) = (self.vc_text.clone(), self.vc_tone, self.busy);
        w::card(ui, "1. Virtual camera (for Google Meet)", |ui| {
            w::status(ui, vc_tone, &vc_text);
            ui.horizontal(|ui| {
                install_clicked = ui
                    .add_enabled(!busy, egui::Button::new("Install"))
                    .on_hover_text("Register the OBS Virtual Camera (asks for administrator rights)")
                    .clicked();
                uninstall_clicked = ui
                    .add_enabled(!busy, egui::Button::new("Uninstall"))
                    .on_hover_text("Remove the OBS Virtual Camera registration")
                    .clicked();
            });
        });
        if install_clicked {
            self.vcam_action(true);
        }
        if uninstall_clicked {
            self.vcam_action(false);
        }

        // 2. On / Off
        let mut toggle_clicked = false;
        let mut cam_idx = self.cam_idx;
        let mut cams_opened = false;
        let on = st.running || st.state == RunState::Starting;
        let (run_text, run_tone) = match st.state {
            RunState::Running if st.running => ("Running", w::Tone::Ok),
            RunState::Starting | RunState::Running => ("Starting\u{2026}", w::Tone::Warn),
            RunState::Error => ("Error (see below the preview)", w::Tone::Error),
            RunState::Stopped => ("Stopped", w::Tone::Muted),
        };
        let cameras = self.cameras.clone();
        w::card(ui, "2. Camera on / off", |ui| {
            toggle_clicked = w::primary_button(ui, if on { "Turn OFF" } else { "Turn ON" }, on).clicked();
            w::status(ui, run_tone, run_text);
            w::field_row(ui, "Camera", |ui| {
                let count = if cameras.is_empty() { 9 } else { cameras.len().max(cam_idx + 1) };
                let r = w::combo(ui, "camera", &Self::camera_label(&cameras, cam_idx), |ui| {
                    for i in 0..count {
                        ui.selectable_value(&mut cam_idx, i, Self::camera_label(&cameras, i));
                    }
                });
                cams_opened = r.response.clicked();
                r.response.on_hover_text(RECONNECTS);
            });
        });
        if cams_opened {
            // A camera may have been plugged in since the last look.
            self.cameras = crate::camera::list_cameras().unwrap_or_default();
        }
        if toggle_clicked {
            self.toggle();
        }
        if cam_idx != self.cam_idx {
            // Camera index change -> worker reopens the device (auto reconnect).
            self.cam_idx = cam_idx;
            self.worker.configure(|c| c.camera = cam_idx);
        }

        // 3. Background (the image itself is picked in the gallery under the preview)
        let mut bg_enabled = self.bg_enabled;
        let mut blur_image = self.blur_image;
        let mut blur_strength = self.blur_strength;
        w::card(ui, "3. Background", |ui| {
            ui.checkbox(&mut bg_enabled, "Virtual background").on_hover_text("Off = normal camera on black");
            ui.checkbox(&mut blur_image, "Blur image").on_hover_text("Soft, out-of-focus background");
            w::slider_row(
                ui,
                "Blur balance",
                &mut blur_strength,
                BLUR_MIN,
                BLUR_MAX,
                0.05,
                BLUR_DEFAULT,
                "How strong the blur is. Double-click to reset.",
                |v| format!("{:.2}x", v),
            );
        });
        if bg_enabled != self.bg_enabled || blur_image != self.blur_image {
            self.bg_enabled = bg_enabled;
            self.blur_image = blur_image;
            self.apply_bg();
            self.save_settings();
        }
        if (blur_strength - self.blur_strength).abs() > 1e-6 {
            // Live: rebuilds the compositor with the new sigma, no camera reconnect.
            self.blur_strength = blur_strength;
            self.worker.set_blur_strength(blur_strength);
        }

        // 4. Options
        let mut speed_idx = self.speed_idx;
        let mut quality_idx = self.quality_idx;
        let mut model = self.model.clone();
        let mut mirror = self.mirror;
        let mut one_person = self.one_person;
        let mut device = self.device;
        let mut rotation = self.rotation;
        let mut out_res = self.out_res;
        let mut zoom = self.zoom;
        let mut zoom_released = false;
        let mut threshold = self.threshold;
        let mut threshold_released = false;
        let devices = self.devices.clone();
        w::card(ui, "4. Options", |ui| {
            w::field_row(ui, "Speed", |ui| {
                w::combo(ui, "speed", SPEED_MODES[speed_idx].0, |ui| {
                    for (i, m) in SPEED_MODES.iter().enumerate() {
                        ui.selectable_value(&mut speed_idx, i, m.0);
                    }
                })
                .response
                .on_hover_text(RECONNECTS);
            });
            w::field_row(ui, "Quality", |ui| {
                w::combo(ui, "quality", QUALITY_PRESETS[quality_idx].0, |ui| {
                    for (i, q) in QUALITY_PRESETS.iter().enumerate() {
                        ui.selectable_value(&mut quality_idx, i, q.0);
                    }
                })
                .response
                .on_hover_text("Sharper edges cost speed. Applies live.");
            });
            w::field_row(ui, "Model", |ui| {
                let current = model.clone();
                w::combo(ui, "model", &current, |ui| {
                    for name in models::model_names() {
                        ui.selectable_value(&mut model, name.to_string(), name);
                    }
                })
                .response
                .on_hover_text(RECONNECTS);
            });
            w::field_row(ui, "", |ui| {
                ui.checkbox(&mut one_person, "Only one person");
                ui.checkbox(&mut mirror, "Mirror");
            });
            w::field_row(ui, "Device", |ui| {
                w::combo(ui, "device", device_label(device), |ui| {
                    for d in devices.iter() {
                        ui.selectable_value(&mut device, *d, device_label(*d));
                    }
                })
                .response
                .on_hover_text(RECONNECTS);
            });
            w::field_row(ui, "Rotate", |ui| {
                let label = ROTATIONS.iter().find(|r| r.1 == rotation).map(|r| r.0).unwrap_or(ROTATIONS[0].0);
                w::combo(ui, "rotate", label, |ui| {
                    for r in ROTATIONS.iter() {
                        ui.selectable_value(&mut rotation, r.1, r.0);
                    }
                })
                .response
                .on_hover_text(RECONNECTS);
            });
            w::field_row(ui, "Output", |ui| {
                let label = OUTPUT_RES.iter().find(|o| o.1 == out_res).map(|o| o.0).unwrap_or(OUTPUT_RES[0].0);
                w::combo(ui, "output", label, |ui| {
                    for o in OUTPUT_RES.iter() {
                        ui.selectable_value(&mut out_res, o.1, o.0);
                    }
                })
                .response
                .on_hover_text(RECONNECTS);
            });
            let ev = w::slider_row(
                ui,
                "Zoom",
                &mut zoom,
                ZOOM_MIN,
                ZOOM_MAX,
                0.1,
                1.0,
                "1.0 = actual size (a crop is not enlarged). Above 1 shrinks it, below 1 enlarges it. \
                 Double-click to reset.",
                |v| format!("{:.1}x", v),
            );
            zoom_released = ev.released;
            let ev = w::slider_row(
                ui,
                "Threshold",
                &mut threshold,
                0.0,
                THRESHOLD_MAX,
                0.05,
                0.0,
                "Makes uncertain, see-through parts of the person solid (for example light \
                 clothes against a light wall). Matte values above the threshold become \
                 person, below it background. Higher values also cut fine hair. \
                 Off = the matte as the model made it. Double-click to reset.",
                |v| if v <= 0.0 { "Off".to_string() } else { format!("{:.2}", v) },
            );
            threshold_released = ev.released;
            w::hint(ui, "Speed, Model, Device, Rotate, Output and Camera briefly reconnect the camera. The rest applies live.");
        });
        if speed_idx != self.speed_idx {
            // Capture size change -> worker reconnects the camera automatically.
            self.speed_idx = speed_idx;
            let (cw, ch) = SPEED_MODES[speed_idx].1;
            self.worker.configure(|c| {
                c.width = cw;
                c.height = ch;
            });
        }
        if quality_idx != self.quality_idx {
            // Live quality<->speed balance (RVM only); takes effect on the next frame.
            self.quality_idx = quality_idx;
            self.worker.set_quality(QUALITY_PRESETS[quality_idx].1);
        }
        if model != self.model {
            // Model change -> worker rebuilds the matting engine (auto reconnect).
            self.model = model.clone();
            self.worker.configure(|c| c.model = Some(model));
            self.save_settings();
        }
        if mirror != self.mirror {
            self.mirror = mirror;
            self.worker.configure(|c| c.mirror = mirror);
        }
        if one_person != self.one_person {
            // Live (in-place): keep only the largest person, or everyone.
            self.one_person = one_person;
            self.worker.configure(|c| c.multi_person = !one_person);
        }
        if device != self.device {
            // Device change -> worker rebuilds the ONNX session (auto reconnect).
            self.device = device;
            self.worker.configure(|c| c.device = device);
        }
        if rotation != self.rotation {
            // Rotation can swap width/height -> worker reconnects the camera automatically.
            self.rotation = rotation;
            self.crop = [0.0, 0.0, 1.0, 1.0]; // the frame turns -> the old crop is meaningless
            self.worker.configure(|c| {
                c.rotation = rotation;
                c.crop = None;
            });
            self.save_settings();
        }
        if out_res != self.out_res {
            // Output size change -> worker reconnects the virtual camera automatically.
            self.out_res = out_res;
            self.worker.configure(|c| {
                c.out_width = Some(out_res.0);
                c.out_height = Some(out_res.1);
            });
            self.save_settings();
        }
        if (zoom - self.zoom).abs() > 1e-6 {
            self.zoom = zoom;
            self.apply_zoom();
        }
        if zoom_released {
            self.save_settings();
        }
        let threshold = ((threshold * 20.0).round() / 20.0).clamp(0.0, THRESHOLD_MAX);
        if (threshold - self.threshold).abs() > 1e-6 {
            // Live: applied by the render thread on the next frame.
            self.threshold = threshold;
            self.worker.configure(|c| c.threshold = threshold);
        }
        if threshold_released {
            self.save_settings();
        }

        // 5. Colour management: Dell-monitor style presets + everyday colour sliders
        let mut preset_name = self.color_preset.clone();
        let custom = self.color_preset == CUSTOM_COLOR_PRESET;
        // A factory preset shows ITS values on the (disabled) sliders.
        let mut vals = if custom {
            self.color_custom
        } else {
            color::preset(&self.color_preset).map(|p| p.values).unwrap_or([0; 5])
        };
        let mut changed = false;
        w::card(ui, "5. Color (Dell monitor presets)", |ui| {
            w::field_row(ui, "Preset", |ui| {
                let current = preset_name.clone();
                w::combo(ui, "color_preset", &current, |ui| {
                    for p in DELL_PRESETS.iter() {
                        ui.selectable_value(&mut preset_name, p.name.to_string(), p.name);
                    }
                });
            });
            for i in 0..5 {
                let tip = match i {
                    2 => "Warmth: left is cooler, right is warmer. Double-click to reset.",
                    3 => "Tint: left is greener, right is more magenta. Double-click to reset.",
                    _ => "0 = neutral. Double-click to reset.",
                };
                if w::color_row(ui, COLOR_LABELS[i], &mut vals[i], custom, tip) {
                    changed = true;
                }
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(custom, egui::Button::new("Reset all")).clicked() {
                    vals = [0; 5];
                    changed = true;
                }
            });
            w::hint(ui, "A preset shows its own values, read-only. Choose 'Custom Color' to adjust them. Applied live.");
        });
        if preset_name != self.color_preset {
            self.color_preset = preset_name;
            self.apply_color();
        } else if custom && changed {
            self.color_custom = vals;
            self.apply_color();
        }
    }

    fn status_text(&self, st: &Status) -> String {
        match st.state {
            RunState::Running if st.running => {
                #[allow(unused_mut)]
                let mut text = format!(
                    "{:.0} fps \u{b7} {} \u{b7} {}",
                    st.fps,
                    st.provider,
                    st.device.clone().unwrap_or_default()
                );
                if !st.device_note.is_empty() {
                    text += &format!("\n{}", st.device_note);
                }
                if let Some(w) = st.bg_warning.as_ref() {
                    text += &format!("\n\u{26a0} {} (using blur)", w);
                }
                text
            }
            RunState::Error => st.error.clone().unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Size of the preview box: as wide as its card and 16:9, but never so tall that
    /// the gallery below is squeezed out.
    fn preview_size(ui: &egui::Ui) -> Vec2 {
        let w = ui.available_width().max(160.0);
        let room = ui.available_height() - BELOW_PREVIEW;
        let h = (w * 9.0 / 16.0).min(room).max(150.0);
        Vec2::new(w, h.round())
    }

    fn tick_refresh(&mut self) -> bool {
        let refresh = self.last_draw.elapsed() >= Duration::from_millis(40);
        if refresh {
            self.last_draw = Instant::now();
        }
        refresh
    }

    fn set_full_view(&mut self, ctx: &egui::Context, on: bool) {
        self.full_view = on;
        self.full_since = Instant::now();
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    fn ui_right(&mut self, ui: &mut egui::Ui, st: &Status) {
        let refresh = self.tick_refresh();
        let status_text = self.status_text(st);
        let mut go_full = false;
        w::card(ui, "Preview", |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                if w::tab(ui, self.tab == 0, "1. Camera input (crop)") {
                    self.tab = 0;
                }
                if w::tab(ui, self.tab == 1, "2. Output preview") {
                    self.tab = 1;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    go_full = ui
                        .button("Full view")
                        .on_hover_text("Show the output on the whole screen (F11). Press Esc or F11 to come back.")
                        .clicked();
                });
            });
            ui.add_space(2.0);
            let size = Self::preview_size(ui);
            if self.tab == 0 {
                self.draw_crop_tab(ui, st, refresh, size);
            } else {
                self.output_canvas(ui, st, refresh, size, false);
            }
            if !status_text.is_empty() {
                if st.state == RunState::Error {
                    w::status(ui, w::Tone::Error, &status_text);
                } else {
                    let mut lines = status_text.splitn(2, '\n');
                    w::status(ui, w::Tone::Ok, lines.next().unwrap_or_default());
                    if let Some(rest) = lines.next() {
                        w::hint(ui, rest);
                    }
                }
            }
            let before = self.show_bounds;
            ui.checkbox(&mut self.show_bounds, "Show camera bounds")
                .on_hover_text("Red rectangle around the camera image. Preview only, never sent to the virtual camera.");
            if before != self.show_bounds {
                self.save_settings();
            }
        });
        if go_full {
            self.set_full_view(ui.ctx(), true);
        }
        self.draw_gallery(ui);
    }

    /// Full view: nothing but the output picture, on the whole screen.
    fn ui_full_view(&mut self, ui: &mut egui::Ui, st: &Status) {
        let refresh = self.tick_refresh();
        let area = ui.max_rect();
        self.output_canvas(ui, st, refresh, area.size(), true);

        let mut leave = false;
        // The way out is shown for the first seconds, and whenever the mouse is near the top.
        let near_top = ui.input(|i| i.pointer.hover_pos()).map(|p| p.y < area.top() + 70.0).unwrap_or(false);
        if near_top || self.full_since.elapsed() < Duration::from_secs(4) {
            let btn = Rect::from_min_size(Pos2::new(area.right() - 150.0, area.top() + 14.0), Vec2::new(136.0, 30.0));
            if ui.put(btn, egui::Button::new("Exit full view (Esc)")).clicked() {
                leave = true;
            }
        }
        if leave {
            self.set_full_view(ui.ctx(), false);
        }
    }

    // ------------------------------------------------- tab 1: the crop editor --

    fn crop_corners_px(&self) -> [(f32, f32); 4] {
        let (ax, ay, aw, ah) = self.crop_area;
        let [x0, y0, x1, y1] = self.crop;
        let (px0, py0, px1, py1) = (ax + x0 * aw, ay + y0 * ah, ax + x1 * aw, ay + y1 * ah);
        [(px0, py0), (px1, py0), (px1, py1), (px0, py1)] // TL, TR, BR, BL
    }

    fn crop_press(&mut self, x: f32, y: f32) {
        let pts = self.crop_corners_px();
        let mut best = 0;
        let mut best_d2 = f32::MAX;
        for (i, p) in pts.iter().enumerate() {
            let d2 = (p.0 - x).powi(2) + (p.1 - y).powi(2);
            if d2 < best_d2 {
                best_d2 = d2;
                best = i;
            }
        }
        self.crop_drag = if best_d2 <= CROP_HIT * CROP_HIT {
            Some(CropDrag::Corner(best))
        } else if pts[0].0 <= x && x <= pts[2].0 && pts[0].1 <= y && y <= pts[2].1 {
            Some(CropDrag::Move { sx: x, sy: y, start: self.crop })
        } else {
            None
        };
    }

    fn crop_motion(&mut self, x: f32, y: f32) {
        let drag = match self.crop_drag {
            Some(d) => d,
            None => return,
        };
        let (ax, ay, aw, ah) = self.crop_area;
        let [mut x0, mut y0, mut x1, mut y1] = self.crop;
        match drag {
            CropDrag::Corner(i) => {
                let u = ((x - ax) / aw).clamp(0.0, 1.0);
                let v = ((y - ay) / ah).clamp(0.0, 1.0);
                if i == 0 || i == 3 {
                    x0 = u.min(x1 - CROP_MIN); // left-hand corners
                } else {
                    x1 = u.max(x0 + CROP_MIN); // right-hand corners
                }
                if i == 0 || i == 1 {
                    y0 = v.min(y1 - CROP_MIN); // top corners
                } else {
                    y1 = v.max(y0 + CROP_MIN); // bottom corners
                }
            }
            CropDrag::Move { sx, sy, start } => {
                let (w, h) = (start[2] - start[0], start[3] - start[1]);
                x0 = (start[0] + (x - sx) / aw).max(0.0).min(1.0 - w);
                y0 = (start[1] + (y - sy) / ah).max(0.0).min(1.0 - h);
                x1 = x0 + w;
                y1 = y0 + h;
            }
        }
        self.crop = [x0, y0, x1, y1];
    }

    fn draw_crop_tab(&mut self, ui: &mut egui::Ui, st: &Status, refresh: bool, size: Vec2) {
        // The camera image (after rotation, before anything else).
        if refresh || self.crop_tex.is_none() {
            match self.worker.latest_raw_frame() {
                Some(frame) => {
                    let rgb = imgproc::bgr_to_rgb_bytes(&frame.data);
                    let img = egui::ColorImage::from_rgb([frame.w, frame.h], &rgb);
                    if let Some(tex) = self.crop_tex.as_mut() {
                        tex.set(img, TextureOptions::LINEAR);
                    } else {
                        self.crop_tex = Some(ui.ctx().load_texture("crop_preview", img, TextureOptions::LINEAR));
                    }
                    self.crop_frame = Some((frame.w as f32, frame.h as f32));
                }
                None => self.crop_frame = None,
            }
        }

        let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let origin = resp.rect.min;
        let (bw, bh) = (resp.rect.width(), resp.rect.height());
        // The image is scaled to fit the box; the crop rectangle lives on the image.
        self.crop_area = match self.crop_frame {
            Some((fw, fh)) => fit_in_box(fw, fh, bw, bh),
            None => (0.0, 0.0, bw, bh),
        };
        let pal = theme::palette(ui);
        painter.rect_filled(resp.rect, 0.0, pal.preview_bg);
        painter.rect_stroke(resp.rect, 0.0, Stroke::new(1.0_f32, pal.border), StrokeKind::Outside);
        let (ax, ay, aw, ah) = self.crop_area;
        let pts = self.crop_corners_px();
        let crop_rect = Rect::from_min_max(
            origin + Vec2::new(pts[0].0, pts[0].1),
            origin + Vec2::new(pts[2].0, pts[2].1),
        );
        match (self.crop_frame.is_some(), self.crop_tex.as_ref()) {
            (true, Some(tex)) => {
                let img_rect = Rect::from_min_size(origin + Vec2::new(ax, ay), Vec2::new(aw, ah));
                painter.image(tex.id(), img_rect, FULL_UV, Color32::WHITE);
                // outside the crop = not used: shade it
                let shade = Color32::from_black_alpha(166);
                let (l, t, r, b) = (img_rect.min.x, img_rect.min.y, img_rect.max.x, img_rect.max.y);
                let c = crop_rect;
                painter.rect_filled(Rect::from_min_max(Pos2::new(l, t), Pos2::new(r, c.min.y)), 0.0, shade);
                painter.rect_filled(Rect::from_min_max(Pos2::new(l, c.max.y), Pos2::new(r, b)), 0.0, shade);
                painter.rect_filled(Rect::from_min_max(Pos2::new(l, c.min.y), Pos2::new(c.min.x, c.max.y)), 0.0, shade);
                painter.rect_filled(Rect::from_min_max(Pos2::new(c.max.x, c.min.y), Pos2::new(r, c.max.y)), 0.0, shade);
            }
            _ => {
                painter.text(
                    resp.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "(camera off)",
                    egui::FontId::proportional(14.0),
                    Color32::from_gray(0xaa),
                );
            }
        }
        painter.rect_stroke(crop_rect, 0.0, Stroke::new(2.0_f32, RED), StrokeKind::Middle);
        for p in pts.iter() {
            let h = Rect::from_center_size(origin + Vec2::new(p.0, p.1), Vec2::splat(CROP_HANDLE * 2.0));
            painter.rect_filled(h, 0.0, RED);
            painter.rect_stroke(h, 0.0, Stroke::new(1.0_f32, Color32::WHITE), StrokeKind::Middle);
        }

        // Drag a corner to resize (the opposite corner stays put), drag inside to move.
        if resp.drag_started() {
            let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
            if let Some(p) = press {
                self.crop_press(p.x - origin.x, p.y - origin.y);
            }
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.crop_motion(p.x - origin.x, p.y - origin.y);
            }
        }
        if resp.drag_stopped() && self.crop_drag.take().is_some() {
            self.commit_crop();
        }
        if let Some(p) = resp.hover_pos() {
            let (x, y) = (p.x - origin.x, p.y - origin.y);
            let near = pts.iter().any(|q| (q.0 - x).powi(2) + (q.1 - y).powi(2) <= CROP_HIT * CROP_HIT);
            let inside = pts[0].0 <= x && x <= pts[2].0 && pts[0].1 <= y && y <= pts[2].1;
            if near {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            } else if inside {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            }
        }

        let raw = if st.running { st.raw_size } else { None };
        let info = match raw {
            Some((rw, rh)) => match crop_pixels(self.crop_cfg(), rw, rh, 16) {
                Some(b) => format!("Crop {}x{} px of {}x{}", b.2 - b.0, b.3 - b.1, rw, rh),
                None => format!("Full frame {}x{}", rw, rh),
            },
            None => {
                if self.crop_cfg().is_none() {
                    "Full frame".to_string()
                } else {
                    "Crop set (applies when the camera is on)".to_string()
                }
            }
        };
        let mut reset = false;
        ui.horizontal(|ui| {
            reset = ui.button("Reset crop").clicked();
            ui.label(egui::RichText::new(info).color(pal.muted)).on_hover_text(
                "Drag the red points to resize, or drag inside the rectangle to move it. Only the area inside is used.",
            );
        });
        if reset {
            self.set_crop([0.0, 0.0, 1.0, 1.0], true);
        }
    }

    // ------------------------------------------------- tab 2: output preview --

    /// The output picture, scaled to fit a box of `size`.
    /// `full` = the full-screen view (black backdrop, no border).
    fn output_canvas(&mut self, ui: &mut egui::Ui, st: &Status, refresh: bool, size: Vec2, full: bool) {
        match self.worker.latest_frame() {
            Some(frame) => {
                self.pv_geom = Some((frame.w as f32, frame.h as f32));
                if refresh || self.out_tex.is_none() {
                    let rgb = imgproc::bgr_to_rgb_bytes(&frame.data);
                    let img = egui::ColorImage::from_rgb([frame.w, frame.h], &rgb);
                    if let Some(tex) = self.out_tex.as_mut() {
                        tex.set(img, TextureOptions::LINEAR);
                    } else {
                        self.out_tex = Some(ui.ctx().load_texture("output_preview", img, TextureOptions::LINEAR));
                    }
                }
            }
            None => {
                if !st.running {
                    self.pv_geom = None;
                }
            }
        }

        let (resp, painter) = ui.allocate_painter(size, Sense::hover());
        let rect = resp.rect;
        let origin = rect.min;
        let bx = (rect.width(), rect.height());
        let pal = theme::palette(ui);
        painter.rect_filled(rect, 0.0, if full { Color32::BLACK } else { pal.preview_bg });
        if !full {
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, pal.border), StrokeKind::Outside);
        }
        let mut drawn = false;
        if let Some((w, h)) = self.pv_geom {
            // The whole picture, scaled to fit the box.
            let (tx, ty, dw, _dh) = fit_in_box(w, h, bx.0, bx.1);
            let s = dw / w;
            if let Some(tex) = self.out_tex.as_ref() {
                let clip = painter.with_clip_rect(rect);
                let img_rect = Rect::from_min_size(origin + Vec2::new(tx, ty), Vec2::new(s * w, s * h));
                clip.image(tex.id(), img_rect, FULL_UV, Color32::WHITE);
                if self.show_bounds && !full {
                    // Preview only: nothing here reaches the virtual camera.
                    let (cx, cy, cw, ch) = match st.cam_rect {
                        Some(r) => (r.0 as f32, r.1 as f32, r.2 as f32, r.3 as f32),
                        None => (0.0, 0.0, w, h), // (cropped) camera input
                    };
                    let bounds = Rect::from_min_size(origin + Vec2::new(s * cx + tx, s * cy + ty), Vec2::new(s * cw, s * ch));
                    // keep the line visible when it runs along the edge of the box
                    clip.rect_stroke(bounds.intersect(rect), 0.0, Stroke::new(2.0_f32, RED), StrokeKind::Inside);
                }
                drawn = true;
            }
        }
        if !drawn {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "(camera off)",
                egui::FontId::proportional(14.0),
                Color32::from_gray(0xaa),
            );
        }
    }

    fn draw_gallery(&mut self, ui: &mut egui::Ui) {
        let title = self.gallery_title();
        let selected = self.bg_path.clone().unwrap_or_default();
        let tile = Vec2::new(THUMB_W as f32 + 2.0 * TILE_BORDER, THUMB_H as f32 + 2.0 * TILE_BORDER);
        let row_h = tile.y + 2.0 * TILE_PAD;
        let mut browse = false;
        let mut clicked: Option<String> = None;
        let tiles = &self.tiles;
        w::card(ui, &title, |ui| {
            let pal = theme::palette(ui);
            let min_h = GAL_ROWS_VISIBLE as f32 * row_h;
            egui::ScrollArea::vertical()
                .id_salt("gallery")
                .auto_shrink([false, false])
                .max_height((ui.available_height() - 16.0).max(min_h))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(2.0 * TILE_PAD);
                        // Slot 0: the Browse tile (same size as an image tile).
                        if ui
                            .add_sized(tile, egui::Button::new("+  Browse\u{2026}"))
                            .on_hover_text("Choose a folder of background images")
                            .clicked()
                        {
                            browse = true;
                        }
                        for (path, tex) in tiles.iter() {
                            let (rect, resp) = ui.allocate_exact_size(tile, Sense::click());
                            if ui.is_rect_visible(rect) {
                                let chosen = same_path(path, &selected);
                                if chosen {
                                    ui.painter().rect_filled(rect, CornerRadius::same(6), pal.accent);
                                }
                                egui::Image::from_texture((tex.id(), Vec2::new(THUMB_W as f32, THUMB_H as f32)))
                                    .corner_radius(CornerRadius::same(4))
                                    .paint_at(ui, rect.shrink(TILE_BORDER));
                                if resp.hovered() && !chosen {
                                    ui.painter().rect_stroke(
                                        rect.shrink(1.0),
                                        CornerRadius::same(6),
                                        Stroke::new(2.0_f32, pal.muted),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                            let resp = resp
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(crate::util::file_name(Path::new(path)));
                            if resp.clicked() {
                                clicked = Some(path.clone());
                            }
                        }
                    });
                });
        });
        if browse {
            self.browse_folder();
        }
        if let Some(path) = clicked {
            self.select_image(path);
        }
    }
}

impl eframe::App for VcamApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_thumbs(ctx);
        self.poll_vcam_action();
        let st = self.worker.status();

        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let msg = if st.running { "The virtual camera is ON \u{2014} quit and turn it off?" } else { "Quit vcam?" };
            let answer = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Info)
                .set_title("Quit vcam")
                .set_description(msg)
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show();
            if matches!(answer, rfd::MessageDialogResult::Ok) {
                self.worker.stop();
                self.allow_close = true;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }

        let (f11, esc) = ctx.input(|i| (i.key_pressed(egui::Key::F11), i.key_pressed(egui::Key::Escape)));
        if f11 || (esc && self.full_view) {
            self.set_full_view(ctx, !self.full_view);
        }
        if self.full_view {
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ctx, |ui| {
                self.ui_full_view(ui, &st);
            });
            ctx.request_repaint_after(Duration::from_millis(40));
            return;
        }

        let mut theme_choice = self.theme;
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                w::hint(ui, "In Google Meet: Settings \u{25b8} Video \u{25b8} Camera \u{25b8} \"OBS Virtual Camera\"");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("theme").width(130.0).selected_text(theme_choice.label()).show_ui(ui, |ui| {
                        for t in ThemeChoice::ALL {
                            ui.selectable_value(&mut theme_choice, t, t.label());
                        }
                    });
                    ui.label("Theme");
                });
            });
            ui.add_space(3.0);
        });
        if theme_choice != self.theme {
            self.theme = theme_choice;
            theme::apply(ctx, theme_choice);
            self.save_settings();
        }
        egui::SidePanel::left("controls").resizable(false).exact_width(340.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("controls_scroll").show(ui, |ui| {
                ui.add_space(4.0);
                self.ui_controls(ui, &st);
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            self.ui_right(ui, &st);
        });

        // Keep the preview and status moving; the camera keeps running regardless.
        ctx.request_repaint_after(Duration::from_millis(40));
    }
}

/// Run the control panel (blocks until the window is closed).
pub fn run() -> anyhow::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("vcam \u{2014} Virtual Camera")
        .with_inner_size([940.0, 800.0])
        .with_min_inner_size([760.0, 560.0]);
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "vcam \u{2014} Virtual Camera",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            let app = VcamApp::new(cc);
            theme::apply(&cc.egui_ctx, app.theme);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{}", e))
}
