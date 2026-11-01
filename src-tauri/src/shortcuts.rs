pub fn prepare_window(app: &tauri::AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    if let Some(window) = app.get_webview_window("quick") { return Ok(window); }
    tauri::WebviewWindowBuilder::new(app, "quick", tauri::WebviewUrl::App("index.html?quick=1".into()))
        .title("BankFlow Next 快捷转换").inner_size(360.0, 400.0)
        .decorations(false).always_on_top(true).skip_taskbar(true)
        .transparent(true).shadow(false)
        .background_color(tauri::window::Color(0, 0, 0, 0))
        .visible(false).resizable(false).build()
}

pub fn position_window(window: &tauri::WebviewWindow, cursor_x: i32, cursor_y: i32) {
    let monitor = window.available_monitors().ok().and_then(|monitors| monitors.into_iter().find(|m| {
        let p=m.position(); let s=m.size();
        cursor_x>=p.x && cursor_y>=p.y && cursor_x<p.x+s.width as i32 && cursor_y<p.y+s.height as i32
    }));
    let scale = monitor.as_ref().map(|m|m.scale_factor()).unwrap_or(1.0);
    let mut x = cursor_x - (180.0*scale) as i32;
    let mut y = cursor_y - (180.0*scale) as i32;
    if let Some(monitor) = monitor {
        let pos = monitor.position(); let size = monitor.size();
        x = x.max(pos.x).min((pos.x+size.width as i32-(360.0*scale) as i32).max(pos.x));
        y = y.max(pos.y).min((pos.y+size.height as i32-(400.0*scale) as i32).max(pos.y));
    }
    let _ = window.set_position(tauri::PhysicalPosition::new(x,y));
}

use tauri::{Manager, Emitter};
use std::sync::atomic::{AtomicBool, Ordering};
#[derive(Default)]
pub struct ShortcutState(pub AtomicBool, pub std::sync::Mutex<Vec<String>>);
#[tauri::command]
pub fn shortcut_status(state: tauri::State<ShortcutState>) -> bool { state.0.load(Ordering::Relaxed) }

#[tauri::command]
pub fn quick_selection(state: tauri::State<ShortcutState>) -> Result<Vec<String>, String> {
    state.1.lock().map(|paths| paths.clone()).map_err(|_| "选择状态不可用".into())
}

#[tauri::command]
pub fn quick_present(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != "quick" { return Err("仅限快捷窗口调用".into()); }
    // 等前端圆盘就绪后再显示，避免首次加载露出空白矩形。
    window.set_focusable(true).map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())
}

#[cfg(windows)]
fn explorer_selection(hwnd: isize) -> Result<Vec<String>, String> {
    use std::{io::Read, os::windows::process::CommandExt, process::{Command, Stdio}, time::{Duration, Instant}};
    let script = format!("$targetHwnd = {};\n{}", hwnd, include_str!("explorer_selection.ps1"));
    let mut child = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| format!("无法读取所选文件：{e}"))?;
    let mut stdout = child.stdout.take().ok_or("无法读取选择结果")?;
    let reader = std::thread::spawn(move || { let mut bytes=Vec::new(); stdout.read_to_end(&mut bytes).map(|_|bytes) });
    let deadline=Instant::now()+Duration::from_secs(5);
    let status=loop {
        if let Some(status)=child.try_wait().map_err(|e|e.to_string())? {break status;}
        if Instant::now()>deadline {let _=child.kill();let _=child.wait();return Err("读取选择超时，请重新选择文件后重试。".into());}
        std::thread::sleep(Duration::from_millis(20));
    };
    let bytes=reader.join().map_err(|_|"读取选择失败")?.map_err(|e|e.to_string())?;
    if !status.success() {return Err("请在资源管理器中选中本地文件（最多 100 个），再按快捷键。".into());}
    serde_json::from_slice(&bytes).map_err(|_|"无法识别资源管理器选择结果".into())
}

// 只在用户按快捷键时读取触发窗口的所选文件，不操作剪贴板。
pub fn install(app: tauri::AppHandle) {
    #[cfg(windows)]
    std::thread::spawn(move || {
        #[repr(C)] struct Point { x: i32, y: i32 }
        #[repr(C)] struct Msg { hwnd: isize, message: u32, wparam: usize, lparam: isize, time: u32, point: Point, private: u32 }
        #[link(name="user32")]
        unsafe extern "system" {
            fn RegisterHotKey(hwnd: isize, id: i32, modifiers: u32, key: u32) -> i32;
            fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
            fn GetCursorPos(point: *mut Point) -> i32;
            fn GetForegroundWindow() -> isize;
        }
        // MOD_NOREPEAT 防止按住快捷键反复创建窗口。
        if unsafe { RegisterHotKey(0, 1, 0x4003, 0x20) } == 0 {
            eprintln!("无法注册 Ctrl+Alt+Space，可能已被其他应用占用");
            return;
        }
        app.state::<ShortcutState>().0.store(true, Ordering::Relaxed);
        let mut message: Msg = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut message, 0, 0, 0) } > 0 {
            if message.message != 0x0312 { continue; }
            let mut point = Point { x: 0, y: 0 };
            if unsafe { GetCursorPos(&mut point) } == 0 { continue; }
            let foreground = unsafe { GetForegroundWindow() };
            if let Some(window) = app.get_webview_window("quick") {
                if window.hwnd().map(|h| h.0 as isize == foreground).unwrap_or(false) {
                    let _ = window.hide();
                    continue;
                }
            }
            let paths = match explorer_selection(foreground) {
                Ok(paths) if !paths.is_empty() => paths,
                _ => { let _=app.emit("shortcut-error", "请先在资源管理器中选中本地文件，再按快捷键。"); continue; }
            };
            if let Ok(mut selected)=app.state::<ShortcutState>().1.lock() { *selected=paths.clone(); }
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                let window = match prepare_window(&handle) {
                    Ok(window) => window,
                    Err(error) => { eprintln!("无法创建快捷窗口：{error}"); return; }
                };
                position_window(&window, point.x, point.y);
                let _ = window.emit("quick-selection", paths);
            });
        }
    });
}
