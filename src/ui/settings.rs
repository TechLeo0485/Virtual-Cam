//! Remembers the gallery folder, chosen image and a few options between launches.
//! Same file and keys as the Python app (`%LOCALAPPDATA%\vcam\ui_settings.json`).

use super::theme::ThemeChoice;
use super::{OUTPUT_DEFAULT, OUTPUT_RES};
use crate::compositor::THRESHOLD_MAX;
use crate::geometry::{ZOOM_MAX, ZOOM_MIN};
use crate::models;
use serde_json::{json, Value};
use std::path::PathBuf;

/// smallest crop, as a fraction of the frame
pub const CROP_MIN: f32 = 0.06;

#[derive(Clone, Debug)]
pub struct Settings {
    pub folder: Option<String>,
    pub image: Option<String>,
    /// normalized x0, y0, x1, y1
    pub crop: [f32; 4],
    pub bg_enabled: bool,
    pub model: String,
    pub rotation: u32,
    pub output: (u32, u32),
    pub zoom: f32,
    /// matte threshold, 0 = off
    pub threshold: f32,
    pub show_bounds: bool,
    /// Dark, Light, or follow Windows (the Python app ignores this key)
    pub theme: ThemeChoice,
    /// Run the webcam only while an app uses the virtual camera (standby otherwise).
    pub on_demand: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            folder: None,
            image: None,
            crop: [0.0, 0.0, 1.0, 1.0],
            bg_enabled: true,
            model: models::DEFAULT_MODEL.to_string(),
            rotation: 0,
            output: OUTPUT_DEFAULT,
            zoom: 1.0,
            threshold: 0.0,
            show_bounds: true,
            theme: ThemeChoice::System,
            on_demand: true,
        }
    }
}

pub fn settings_path() -> PathBuf {
    crate::util::app_data_dir().join("ui_settings.json")
}

impl Settings {
    /// Load, validating every value (anything odd falls back to its default).
    pub fn load() -> Settings {
        let mut s = Settings::default();
        let text = match std::fs::read_to_string(settings_path()) {
            Ok(t) => t,
            Err(_) => return s,
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return s,
        };
        if let Some(f) = v.get("folder").and_then(|x| x.as_str()) {
            if std::path::Path::new(f).is_dir() {
                s.folder = Some(f.to_string());
            }
        }
        if let Some(i) = v.get("image").and_then(|x| x.as_str()) {
            if std::path::Path::new(i).is_file() {
                s.image = Some(i.to_string());
            }
        }
        if let Some(arr) = v.get("crop").and_then(|x| x.as_array()) {
            let c: Vec<f32> = arr.iter().filter_map(|t| t.as_f64()).map(|t| (t as f32).clamp(0.0, 1.0)).collect();
            if c.len() == 4 && c[2] - c[0] >= CROP_MIN && c[3] - c[1] >= CROP_MIN {
                s.crop = [c[0], c[1], c[2], c[3]];
            }
        }
        if let Some(b) = v.get("bg_enabled").and_then(|x| x.as_bool()) {
            s.bg_enabled = b;
        }
        if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
            if models::spec(m).is_some() {
                s.model = m.to_string();
            }
        }
        if let Some(r) = v.get("rotation").and_then(|x| x.as_u64()) {
            if matches!(r, 0 | 90 | 180 | 270) {
                s.rotation = r as u32;
            }
        }
        if let Some(arr) = v.get("output").and_then(|x| x.as_array()) {
            if arr.len() == 2 {
                if let (Some(w), Some(h)) = (arr[0].as_u64(), arr[1].as_u64()) {
                    let res = (w as u32, h as u32);
                    if OUTPUT_RES.iter().any(|o| o.1 == res) {
                        s.output = res;
                    }
                }
            }
        }
        if let Some(z) = v.get("zoom").and_then(|x| x.as_f64()) {
            s.zoom = (z as f32).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        if let Some(t) = v.get("threshold").and_then(|x| x.as_f64()) {
            if t.is_finite() {
                s.threshold = (t as f32).clamp(0.0, THRESHOLD_MAX);
            }
        }
        if let Some(b) = v.get("show_bounds").and_then(|x| x.as_bool()) {
            s.show_bounds = b;
        }
        if let Some(t) = v.get("theme").and_then(|x| x.as_str()) {
            s.theme = ThemeChoice::from_key(t);
        }
        if let Some(b) = v.get("on_demand").and_then(|x| x.as_bool()) {
            s.on_demand = b;
        }
        s
    }

    /// Not critical; a failure just means nothing is remembered.
    pub fn save(&self) {
        let round4 = |t: f32| ((t as f64) * 10000.0).round() / 10000.0;
        let v = json!({
            "folder": self.folder,
            "image": self.image,
            "crop": [round4(self.crop[0]), round4(self.crop[1]), round4(self.crop[2]), round4(self.crop[3])],
            "bg_enabled": self.bg_enabled,
            "model": self.model,
            "rotation": self.rotation,
            "output": [self.output.0, self.output.1],
            "zoom": ((self.zoom as f64) * 10.0).round() / 10.0,
            "threshold": ((self.threshold as f64) * 100.0).round() / 100.0,
            "show_bounds": self.show_bounds,
            "theme": self.theme.key(),
            "on_demand": self.on_demand,
        });
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::write(path, text);
        }
    }
}
