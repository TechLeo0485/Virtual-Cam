//! The few custom pieces the control panel is built from: section cards, aligned
//! label + control rows, a slider that resets on double-click, the big ON/OFF button,
//! status lines and the preview tab strip.

use super::theme::{palette, semibold};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Response, RichText, Sense, Stroke, StrokeKind,
    Ui, Vec2,
};

/// Width of the label column, so every control in every card starts at the same x.
pub const LABEL_W: f32 = 82.0;
/// Width of the value shown to the right of a slider.
pub const VALUE_W: f32 = 44.0;

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

/// A titled section: filled, rounded, padded, full width.
pub fn card<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    let inner = egui::Frame::NONE
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).heading());
            ui.add_space(2.0);
            add(ui)
        })
        .inner;
    ui.add_space(6.0);
    inner
}

/// Small muted text (one short line under a card's controls).
pub fn hint(ui: &mut Ui, text: &str) {
    let color = palette(ui).muted;
    ui.add(egui::Label::new(RichText::new(text).small().color(color)).wrap());
}

/// `label` in a fixed-width column, then the control(s) in what is left of the row.
pub fn field_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let h = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(Vec2::new(LABEL_W, h), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(label);
        });
        add(ui)
    })
    .inner
}

/// A dropdown as wide as the rest of the row.
pub fn combo<R>(
    ui: &mut Ui,
    id: &str,
    selected: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    egui::ComboBox::from_id_salt(id).width(ui.available_width()).selected_text(selected).truncate().show_ui(ui, add)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Error,
    Muted,
}

pub fn tone_color(ui: &Ui, tone: Tone) -> Color32 {
    let p = palette(ui);
    match tone {
        Tone::Ok => p.ok,
        Tone::Warn => p.warn,
        Tone::Error => p.error,
        Tone::Muted => p.muted,
    }
}

/// A coloured dot followed by the text in the same colour.
pub fn status(ui: &mut Ui, tone: Tone, text: &str) {
    let color = tone_color(ui, tone);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, ui.spacing().interact_size.y * 0.7), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
    });
}

/// The big full-width ON/OFF button. `danger` = the outlined red "turn off" form.
pub fn primary_button(ui: &mut Ui, text: &str, danger: bool) -> Response {
    let p = palette(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    let hovered = resp.hovered();
    let down = resp.is_pointer_button_down_on();
    let radius = CornerRadius::same(8);
    let painter = ui.painter();
    let text_color = if danger {
        let fill = if down {
            tint(p.error, 70)
        } else if hovered {
            tint(p.error, 36)
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, radius, fill);
        painter.rect_stroke(rect, radius, Stroke::new(1.5_f32, p.error), StrokeKind::Inside);
        p.error
    } else {
        let fill = if down {
            mix(p.accent, Color32::BLACK, 0.18)
        } else if hovered {
            mix(p.accent, Color32::WHITE, 0.14)
        } else {
            p.accent
        };
        painter.rect_filled(rect, radius, fill);
        p.on_accent
    };
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), radius, Stroke::new(1.0_f32, p.text), StrokeKind::Outside);
    }
    painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::new(14.0, semibold()), text_color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One tab of the preview tab strip: plain text, accent underline when selected.
pub fn tab(ui: &mut Ui, selected: bool, text: &str) -> bool {
    let p = palette(ui);
    let color = if selected { p.text } else { p.muted };
    let resp = ui
        .add(egui::Button::new(RichText::new(text).color(color)).frame(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if selected {
        ui.painter().hline(resp.rect.x_range(), resp.rect.bottom() + 1.0, Stroke::new(2.0_f32, p.accent));
    } else if resp.hovered() {
        ui.painter().hline(resp.rect.x_range(), resp.rect.bottom() + 1.0, Stroke::new(2.0_f32, p.border));
    }
    resp.clicked()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SliderEvent {
    pub changed: bool,
    /// the mouse let go (or a key / double-click finished the change): time to save
    pub released: bool,
}

/// A slider `width` wide. Click or drag anywhere on it to set the value, double-click
/// to go back to `default`, arrow keys nudge by one `step` when it has focus.
#[allow(clippy::too_many_arguments)]
pub fn slider(
    ui: &mut Ui,
    value: &mut f32,
    lo: f32,
    hi: f32,
    step: f32,
    default: f32,
    enabled: bool,
    width: f32,
    tip: &str,
) -> SliderEvent {
    let p = palette(ui);
    let h = ui.spacing().interact_size.y;
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width.max(40.0), h), sense);
    let knob = 6.0;
    let (x0, x1) = (rect.left() + knob + 1.0, rect.right() - knob - 1.0);
    let snap = |v: f32| (lo + ((v - lo) / step).round() * step).clamp(lo, hi);
    let before = *value;
    let mut ev = SliderEvent::default();

    if enabled {
        if resp.double_clicked() {
            *value = default.clamp(lo, hi);
            ev.released = true;
        } else if resp.is_pointer_button_down_on() || resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let t = ((pos.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                *value = snap(lo + t * (hi - lo));
            }
        }
        if resp.drag_stopped() || resp.clicked() {
            ev.released = true;
        }
        if resp.clicked() {
            resp.request_focus();
        }
        if resp.has_focus() {
            let (left, right) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp),
                )
            });
            if left || right {
                *value = snap(*value + if right { step } else { -step });
                ev.released = true;
            }
        }
    }
    ev.changed = (*value - before).abs() > 1e-6;

    if ui.is_rect_visible(rect) {
        let t = if hi > lo { ((*value - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.0 };
        let cx = x0 + t * (x1 - x0);
        let cy = rect.center().y;
        let active = enabled && (resp.hovered() || resp.dragged() || resp.has_focus());
        let fill = if enabled { p.accent } else { p.muted };
        let rail = egui::Rect::from_min_max(egui::pos2(x0 - knob, cy - 2.0), egui::pos2(x1 + knob, cy + 2.0));
        let done = egui::Rect::from_min_max(rail.min, egui::pos2(cx, cy + 2.0));
        let painter = ui.painter();
        painter.rect_filled(rail, CornerRadius::same(2), p.input_strong);
        painter.rect_filled(done, CornerRadius::same(2), if enabled { fill } else { tint(fill, 120) });
        let radius = if active { knob + 1.0 } else { knob };
        painter.circle_filled(egui::pos2(cx, cy), radius, fill);
        painter.circle_stroke(egui::pos2(cx, cy), radius, Stroke::new(2.0_f32, p.card));
        if active {
            painter.circle_stroke(egui::pos2(cx, cy), radius + 2.0, Stroke::new(1.0_f32, tint(p.accent, 110)));
        }
    }
    if enabled && !tip.is_empty() {
        resp.on_hover_text(tip);
    }
    ev
}

/// Label, slider, and the value as text on the right.
#[allow(clippy::too_many_arguments)]
pub fn slider_row(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    lo: f32,
    hi: f32,
    step: f32,
    default: f32,
    tip: &str,
    fmt: impl Fn(f32) -> String,
) -> SliderEvent {
    field_row(ui, label, |ui| {
        let w = ui.available_width() - VALUE_W - ui.spacing().item_spacing.x;
        let ev = slider(ui, value, lo, hi, step, default, true, w, tip);
        let color = palette(ui).muted;
        let h = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(Vec2::new(VALUE_W, h), Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(fmt(*value)).color(color));
        });
        ev
    })
}

/// Label, a -100..100 slider (0 = neutral), and a number box for typing the value.
/// Returns true when the value changed.
pub fn color_row(ui: &mut Ui, label: &str, value: &mut i32, enabled: bool, tip: &str) -> bool {
    field_row(ui, label, |ui| {
        let before = *value;
        let w = ui.available_width() - VALUE_W - ui.spacing().item_spacing.x;
        let mut f = *value as f32;
        slider(ui, &mut f, -100.0, 100.0, 1.0, 0.0, enabled, w, tip);
        *value = f.round() as i32;
        let h = ui.spacing().interact_size.y;
        ui.add_enabled_ui(enabled, |ui| {
            ui.add_sized([VALUE_W, h], egui::DragValue::new(value).range(-100..=100));
        });
        *value != before
    })
}
