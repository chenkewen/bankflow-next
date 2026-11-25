use crate::{domain::ConversionRequest, services::{self, ConversionState}};

#[tauri::command]
pub fn start_conversion(app: tauri::AppHandle, window: tauri::WebviewWindow, state: tauri::State<'_, ConversionState>, request: ConversionRequest) -> Result<(), String> {
    services::start(app, &state, request, window.label().to_string())
}

#[tauri::command]
pub fn task_snapshot(state: tauri::State<'_, ConversionState>) -> Result<crate::domain::TaskSnapshot,String> {services::snapshot(&state)}

#[tauri::command]
pub fn clear_task(app: tauri::AppHandle, state: tauri::State<'_, ConversionState>) -> Result<(), String> {
    services::clear(&app, &state)
}

#[tauri::command]
pub fn cancel_conversion(state: tauri::State<'_, ConversionState>) -> Result<(), String> {
    services::cancel(&state)
}

#[tauri::command]
pub fn open_result(path: String, folder: bool) -> Result<(), String> {
    let file = std::path::PathBuf::from(path).canonicalize().map_err(|e| e.to_string())?;
    if !file.is_dir() && (!file.is_file() || !matches!(file.extension().and_then(|s| s.to_str()), Some("xlsx" | "pdf" | "png" | "jpg" | "webp" | "bmp" | "tiff"))) {
        return Err("只能打开导出的文件或目录".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(hwnd: isize, operation: *const u16, file: *const u16,
                parameters: *const u16, directory: *const u16, show: i32) -> isize;
        }
        let target = if folder && !file.is_dir() { file.parent().ok_or("找不到输出目录")? } else { &file };
        // canonicalize 的扩展路径前缀不适合交给 Windows Shell。
        let text = target.to_string_lossy();
        let shell_path = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{}", unc)
        } else { text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned() };
        let wide: Vec<u16> = std::ffi::OsStr::new(&shell_path).encode_wide().chain(Some(0)).collect();
        // 直接交给系统关联程序，不将文件路径拼进 shell 命令。
        let result = unsafe { ShellExecuteW(0, std::ptr::null(), wide.as_ptr(),
            std::ptr::null(), std::ptr::null(), 1) };
        if result <= 32 { return Err("无法打开，请确认已安装表格软件，或选择打开目录。".into()); }
        Ok(())
    }
    #[cfg(not(windows))]
    Err("当前版本仅支持 Windows 打开结果".into())
}
