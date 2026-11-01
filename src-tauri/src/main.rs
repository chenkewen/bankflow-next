#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

mod commands;
mod domain;
mod engines;
mod services;
mod shortcuts;
mod drag_gesture;
mod desktop;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(services::ConversionState::default())
        .manage(shortcuts::ShortcutState::default())
        .manage(drag_gesture::DragState::default())
        .manage(desktop::DesktopState::default())
        .setup(|app| {
            desktop::install(app.handle())?;
            shortcuts::prepare_window(app.handle())?;
            shortcuts::install(app.handle().clone());
            drag_gesture::install(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![commands::start_conversion, commands::cancel_conversion, commands::open_result, commands::task_snapshot, desktop::desktop_preference, desktop::save_desktop_preference, desktop::apply_close_choice, desktop::show_tasks, desktop::exit_application, shortcuts::shortcut_status, shortcuts::quick_selection, shortcuts::quick_present, drag_gesture::drag_ready, drag_gesture::drag_status, drag_gesture::drag_present])
        .on_window_event(|window, event| {
            if window.label() == "quick" && matches!(event, tauri::WindowEvent::Focused(false)) {
                let _ = window.hide();
            }
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested {api,..}=event {
                    api.prevent_close();
                    desktop::request_close(window.app_handle());
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("桌面程序启动失败");
}
