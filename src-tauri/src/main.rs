#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

#[cfg(any(debug_assertions, test))]
mod diagnostics;
mod worker;
mod commands;
mod cli;
mod domain;
mod engines;
mod ocr_lines;
mod ocr_document;
mod ocr_table;
mod ocr_geometry;
mod ocr_metadata;
mod ocr_finance;
mod statement_rows;
mod statement_grid;
mod statement_excel;
mod image_convert;
mod image_pdf;
mod statement_ocr;
mod native_ocr_page;
mod ocr_boxes;
mod ocr_pixels;
mod pdf_tools;
mod native_statement;
mod native_table;
mod native_chars;
mod native_fields;
mod native_font;
mod pdfium_engine;
mod services;
mod shortcuts;
mod drag_gesture;
mod desktop;

fn main() {
    let operation = std::env::args_os().nth(1);
    match operation.as_deref().and_then(|s| s.to_str()) {
        Some("--convert") => std::process::exit(cli::run()),
        Some("--image-worker") => return worker::run(),
        Some(flag) if flag.starts_with("--") => {
            #[cfg(any(debug_assertions, test))]
            if let Some(result) = diagnostics::run(flag) {
                if let Err(e) = result {
                    eprintln!("验证命令执行失败：{e}");
                    std::process::exit(1);
                }
                return;
            }
            eprintln!("未知命令：{flag}");
            std::process::exit(2);
        }
        _ => {}
    }
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
        .invoke_handler(tauri::generate_handler![commands::start_conversion, commands::cancel_conversion, commands::open_result, commands::task_snapshot, commands::clear_task, desktop::desktop_preference, desktop::save_desktop_preference, desktop::apply_close_choice, desktop::show_tasks, desktop::exit_application, shortcuts::shortcut_status, shortcuts::quick_selection, shortcuts::quick_present, drag_gesture::drag_ready, drag_gesture::drag_status, drag_gesture::drag_present])
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
