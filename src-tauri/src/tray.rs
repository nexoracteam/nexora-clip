//! Keeps Nexora Clip running in the system tray when the window is closed, starts it with Windows on request,
//! and stops the PC from sleeping while it runs in the background (so a phone can reach it later).

use crate::models::Settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// Set when the user chose Quit, so closing really closes.
pub static QUITTING: AtomicBool = AtomicBool::new(false);

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Nexora Clip", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Nexora Clip")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                crate::live::stop_all();
                QUITTING.store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Should closing the main window hide it instead of quitting?
pub fn hide_on_close(settings: &Settings) -> bool {
    settings.run_in_background && !QUITTING.load(Ordering::SeqCst)
}

static AWAKE: OnceLock<Mutex<Sender<bool>>> = OnceLock::new();

/// Ask Windows not to sleep (true) or allow sleep again (false). The state belongs to one thread, so one thread holds it.
pub fn keep_awake(on: bool) {
    let tx = AWAKE.get_or_init(|| {
        let (tx, rx) = channel::<bool>();
        std::thread::spawn(move || {
            for want in rx {
                set_execution_state(want);
            }
        });
        Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send(on);
}

#[cfg(windows)]
fn set_execution_state(on: bool) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
    unsafe {
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
    }
}

#[cfg(not(windows))]
fn set_execution_state(_on: bool) {}

/// Apply the background settings: stay awake, and start with Windows.
pub fn apply(app: &AppHandle, settings: &Settings) {
    use tauri_plugin_autostart::ManagerExt;
    keep_awake(settings.run_in_background && settings.keep_awake);
    let launcher = app.autolaunch();
    let enabled = launcher.is_enabled().unwrap_or(false);
    if settings.autostart && !enabled {
        let _ = launcher.enable();
    } else if !settings.autostart && enabled {
        let _ = launcher.disable();
    }
}
