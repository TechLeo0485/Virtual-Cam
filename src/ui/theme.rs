//! Look of the control panel: the Segoe UI font, a dark and a light palette, and the
//! egui styles built from them. The user picks Dark, Light or "follow Windows".

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Theme,
    ThemePreference, Vec2,
};
use std::sync::Arc;

/// Every colour the control panel draws with. One instance per theme.
pub struct Palette {
    /// window background
    pub window: Color32,
    /// section cards and popup menus
    pub card: Color32,
    pub border: Color32,
    /// buttons and dropdowns at rest
    pub input: Color32,
    /// checkbox boxes and slider rails
    pub input_strong: Color32,
    pub input_hover: Color32,
    pub input_active: Color32,
    pub text: Color32,
    /// hints and secondary values
    pub muted: Color32,
    pub accent: Color32,
    /// text on an accent-coloured surface
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub error: Color32,
    /// behind the video previews (dark in both themes, like a video player)
    pub preview_bg: Color32,
}

pub const DARK: Palette = Palette {
    window: Color32::from_rgb(0x1b, 0x1d, 0x21),
    card: Color32::from_rgb(0x25, 0x28, 0x2d),
    border: Color32::from_rgb(0x3a, 0x3e, 0x46),
    input: Color32::from_rgb(0x31, 0x35, 0x3c),
    input_strong: Color32::from_rgb(0x43, 0x48, 0x51),
    input_hover: Color32::from_rgb(0x3d, 0x42, 0x4b),
    input_active: Color32::from_rgb(0x4a, 0x50, 0x5a),
    text: Color32::from_rgb(0xe6, 0xe8, 0xeb),
    muted: Color32::from_rgb(0x9a, 0xa0, 0xa8),
    accent: Color32::from_rgb(0x4c, 0x8d, 0xff),
    on_accent: Color32::WHITE,
    ok: Color32::from_rgb(0x3f, 0xb9, 0x50),
    warn: Color32::from_rgb(0xd2, 0x99, 0x22),
    error: Color32::from_rgb(0xf8, 0x51, 0x49),
    preview_bg: Color32::from_rgb(0x11, 0x12, 0x14),
};

pub const LIGHT: Palette = Palette {
    window: Color32::from_rgb(0xf1, 0xf3, 0xf5),
    card: Color32::WHITE,
    border: Color32::from_rgb(0xd5, 0xd9, 0xde),
    input: Color32::from_rgb(0xf1, 0xf3, 0xf5),
    input_strong: Color32::from_rgb(0xd9, 0xdd, 0xe2),
    input_hover: Color32::from_rgb(0xe4, 0xe7, 0xeb),
    input_active: Color32::from_rgb(0xd5, 0xd9, 0xde),
    text: Color32::from_rgb(0x1f, 0x23, 0x28),
    muted: Color32::from_rgb(0x65, 0x6d, 0x76),
    accent: Color32::from_rgb(0x1a, 0x73, 0xe8),
    on_accent: Color32::WHITE,
    ok: Color32::from_rgb(0x1a, 0x7f, 0x37),
    warn: Color32::from_rgb(0x9a, 0x67, 0x00),
    error: Color32::from_rgb(0xcf, 0x22, 0x2e),
    preview_bg: Color32::from_rgb(0x22, 0x25, 0x2a),
};

/// The palette of the theme this `ui` is drawn with.
pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

/// What the Theme dropdown offers. Saved as `theme` in the settings file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Follow Windows",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        }
    }

    /// Anything unknown falls back to following Windows.
    pub fn from_key(key: &str) -> ThemeChoice {
        match key {
            "dark" => ThemeChoice::Dark,
            "light" => ThemeChoice::Light,
            _ => ThemeChoice::System,
        }
    }
}

const SEMIBOLD: &str = "semibold";

/// Font family for headings and the big ON/OFF button. Registered by `install`.
pub fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

/// Segoe UI from the Windows font folder; egui's built-in font stays as the fallback
/// (and is all there is when a file is missing).
fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let dir = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string())).join("Fonts");
    let load = |defs: &mut FontDefinitions, name: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                defs.font_data.insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let regular = load(&mut defs, "segoe", "segoeui.ttf");
    let bold = load(&mut defs, "segoe_semibold", "seguisb.ttf");
    let symbols = load(&mut defs, "segoe_symbol", "seguisym.ttf");

    let mut proportional = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if symbols {
        proportional.insert(0, "segoe_symbol".to_string());
    }
    if regular {
        proportional.insert(0, "segoe".to_string());
    }
    let mut heading = proportional.clone();
    if bold {
        heading.insert(0, "segoe_semibold".to_string());
    }
    defs.families.insert(FontFamily::Proportional, proportional);
    // Always registered: using an unknown family would panic.
    defs.families.insert(semibold(), heading);
    defs
}

fn style_for(p: &Palette, dark: bool) -> egui::Style {
    let mut s = egui::Style::default();
    s.text_styles = [
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Heading, FontId::new(14.0, semibold())),
        (TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();

    s.spacing.item_spacing = Vec2::new(8.0, 5.0);
    s.spacing.button_padding = Vec2::new(8.0, 3.0);
    s.spacing.interact_size = Vec2::new(40.0, 24.0);
    s.spacing.icon_width = 16.0;
    s.spacing.icon_width_inner = 9.0;
    s.spacing.combo_width = 120.0;
    s.spacing.menu_margin = egui::Margin::same(6);

    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.window;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.extreme_bg_color = p.input;
    v.faint_bg_color = p.input;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.error;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0_f32, p.on_accent);
    v.slider_trailing_fill = true;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(8);

    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.input_strong;
    w.inactive.weak_bg_fill = p.input;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.input_hover;
    w.hovered.weak_bg_fill = p.input_hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.hovered.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.input_active;
    w.active.weak_bg_fill = p.input_active;
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.input;
    w.open.weak_bg_fill = p.input_hover;
    w.open.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.open.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.open.corner_radius = r;
    w.open.expansion = 0.0;

    s.visuals = v;
    s
}

/// Load the fonts and register the dark and the light style. Call once per window,
/// before anything is drawn.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_style_of(Theme::Dark, style_for(&DARK, true));
    ctx.set_style_of(Theme::Light, style_for(&LIGHT, false));
}

/// Switch theme now. `System` follows the Windows light / dark setting, live.
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Dark => ThemePreference::Dark,
        ThemeChoice::Light => ThemePreference::Light,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_key_round_trips_and_unknown_follows_windows() {
        for t in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_key(t.key()), t);
        }
        assert_eq!(ThemeChoice::from_key("purple"), ThemeChoice::System);
        assert_eq!(ThemeChoice::from_key(""), ThemeChoice::System);
    }
}
