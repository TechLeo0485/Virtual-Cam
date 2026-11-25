//! System-tray icon for the control panel.
//!
//! Closing the window hides it here instead of quitting, so vcam keeps serving the
//! virtual camera from the tray. Left-click the icon (or "Open vcam" in its menu) to
//! bring the window back; "Quit vcam" stops the camera and exits.
//!
//! The window is hidden and shown with Win32 calls rather than egui viewport commands:
//! egui stops running `update` for a hidden window, so it could not process a "show"
//! command sent while hidden. The tray's event handlers run on the main thread (the
//! thread that created the icon and pumps the window messages).

use crate::worker::VCamWorker;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW};

/// The control panel's window title (also how the window is found).
pub const WINDOW_TITLE: &str = "vcam \u{2014} Virtual Camera";

static HIDDEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    // Lives on the main thread; dropped before exit so the icon leaves the tray at once.
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

fn main_window() -> Option<HWND> {
    let title = HSTRING::from(WINDOW_TITLE);
    unsafe { FindWindowW(PCWSTR::null(), &title).ok() }
}

/// True while the window is hidden in the tray (the UI skips drawing then).
pub fn is_hidden() -> bool {
    HIDDEN.load(Ordering::Relaxed)
}

/// Hide the window to the tray.
pub fn hide_window() {
    if let Some(hwnd) = main_window() {
        HIDDEN.store(true, Ordering::Relaxed);
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Bring the window back from the tray (also restores it if minimized).
pub fn show_window() {
    HIDDEN.store(false, Ordering::Relaxed);
    if let Some(hwnd) = main_window() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn tray_image() -> Option<Icon> {
    let img = image::load_from_memory(include_bytes!("../../assets/icon-256.png")).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Put vcam in the tray. Call once, on the main thread, after the window exists.
/// Returns false when the icon could not be created (the window then quits on close
/// as before).
pub fn create(worker: Arc<VCamWorker>) -> bool {
    let Some(icon) = tray_image() else {
        return false;
    };
    let menu = Menu::new();
    let open = MenuItem::new("Open vcam", true, None);
    let quit = MenuItem::new("Quit vcam", true, None);
    if menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]).is_err() {
        return false;
    }
    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            show_window();
        } else if event.id == quit_id {
            worker.stop(); // turns the webcam and the virtual camera off
            TRAY.with(|t| t.borrow_mut().take());
            std::process::exit(0);
        }
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            show_window();
        }
    }));
    let tray = TrayIconBuilder::new()
        .with_tooltip("vcam \u{2014} virtual camera")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build();
    match tray {
        Ok(t) => {
            TRAY.with(|slot| *slot.borrow_mut() = Some(t));
            true
        }
        Err(e) => {
            crate::vlog!("[vcam] could not create the tray icon: {}", e);
            false
        }
    }
}
