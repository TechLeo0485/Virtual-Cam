//! The control panel (port of `vcam_ui.py`) and the CLI preview window.

pub mod app;
pub mod settings;
pub mod theme;
pub mod view;
pub mod widgets;

use eframe::egui;

/// Rotate dropdown: label -> degrees clockwise. Applied to the camera image before
/// Mirror, so "Mirror" always means a horizontal flip of what you see.
pub const ROTATIONS: [(&str, u32); 4] = [
    ("0\u{b0} (none)", 0),
    ("90\u{b0} clockwise", 90),
    ("180\u{b0}", 180),
    ("270\u{b0} (90\u{b0} counter-clockwise)", 270),
];

/// Output (virtual camera) resolution dropdown. The frame sent to Meet is always this
/// size; a camera with a different aspect is scaled to FIT and the rest is filled with
/// the background. Meet sends 720p by default (1080p is opt-in).
pub const OUTPUT_RES: [(&str, (u32, u32)); 6] = [
    ("1280x720 (16:9 HD)", (1280, 720)),
    ("1920x1080 (16:9 Full HD)", (1920, 1080)),
    ("960x540 (16:9)", (960, 540)),
    ("640x360 (16:9)", (640, 360)),
    ("1024x768 (4:3)", (1024, 768)),
    ("720x1280 (9:16 portrait)", (720, 1280)),
];
pub const OUTPUT_DEFAULT: (u32, u32) = (1280, 720);

/// Background gallery: a Browse tile + one thumbnail per image in the chosen folder.
pub const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "bmp", "webp"];
pub const THUMB_W: usize = 108;
pub const THUMB_H: usize = 61;
/// selection highlight width (px)
pub const TILE_BORDER: f32 = 3.0;
pub const TILE_PAD: f32 = 3.0;
pub const GAL_ROWS_VISIBLE: usize = 2;

/// label -> (capture_w, capture_h). "Smooth" captures at a fast low-res the webcam can
/// deliver at 30 fps, then upscales the matted output. The capture size is 16:9
/// (matches the 1280x720 output) so the upscale is uniform, and 360p mattes ~20%
/// faster than 480p (fewer pixels through the net).
pub const SPEED_MODES: [(&str, (u32, u32)); 2] =
    [("Sharp (720p, ~camera fps)", (1280, 720)), ("Smooth (360p capture, faster)", (640, 360))];

/// label -> RVM downsample_ratio (the quality<->speed dial; resolution unchanged).
/// Higher = sharper hair/edges but slower; lower = faster but softer. Applies live
/// while the camera is on (RVM engine only). 0.375 is RVM's recommended 720p point.
/// NOTE: above ~0.5 you leave RVM's trained coarse-resolution range, so the matte
/// barely gets sharper while speed craters (0.75 is about 6x slower for no visible
/// gain). The 0.60/0.75 rows are included so you can judge for yourself; 0.50 is the
/// practical sharpest.
pub const QUALITY_PRESETS: [(&str, f32); 8] = [
    ("Max sharp (0.75, very slow)", 0.75),
    ("Extra sharp (0.60, slow)", 0.60),
    ("Sharpest (0.50)", 0.50),
    ("Sharper (0.40)", 0.40),
    ("Balanced (0.375, default)", 0.375),
    ("Smoother (0.30)", 0.30),
    ("Fast (0.25)", 0.25),
    ("Fastest (0.20)", 0.20),
];
pub const QUALITY_DEFAULT_INDEX: usize = 4;

/// Blur sigma multiplier range for the "Blur balance" slider. Governs both the live
/// "Blur" background mode and the "Blur image" checkbox; 1.0 reproduces the original
/// fixed blur amount. Applies live (rebuilds the compositor, no camera reconnect).
pub const BLUR_MIN: f32 = 0.1;
pub const BLUR_MAX: f32 = 3.0;
pub const BLUR_DEFAULT: f32 = 0.25;

/// Device dropdown labels.
pub fn device_label(d: crate::engine::Device) -> &'static str {
    match d {
        crate::engine::Device::Auto => "Auto (fastest)",
        crate::engine::Device::Cpu => "CPU",
        crate::engine::Device::Cuda => "GPU (NVIDIA CUDA)",
        crate::engine::Device::Dml => "GPU (DirectML)",
    }
}

/// The window / taskbar icon from `assets\icon-256.png` (found next to the app or above it).
pub fn load_icon() -> Option<egui::IconData> {
    let path = crate::util::find_near_exe("assets\\icon-256.png")?;
    let img = image::open(path).ok()?.into_rgba8();
    let (width, height) = (img.width(), img.height());
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}
