use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{Emitter, Manager};

#[derive(Default)]
pub struct DragState {
    ready: AtomicBool,
    installed: AtomicBool,
    active: AtomicU64,
}

#[tauri::command]
pub fn drag_ready(app: tauri::AppHandle, state: tauri::State<DragState>) {
    state.ready.store(true, Ordering::Relaxed);
    if state.installed.load(Ordering::Relaxed) { let _ = app.emit("drag-enabled", ()); }
}

#[tauri::command]
pub fn drag_status(state: tauri::State<DragState>) -> bool {
    state.ready.load(Ordering::Relaxed) && state.installed.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn drag_present(window: tauri::WebviewWindow, state: tauri::State<DragState>, id: u64) -> Result<(), String> {
    if window.label() != "quick" || state.active.load(Ordering::Relaxed) != id { return Ok(()); }
    #[cfg(windows)]
    unsafe {
        // OLE 拖放仍属于资源管理器，显示浮窗时不能夺走它的焦点。
        if windows::GetAsyncKeyState(1) >= 0 || windows::GetAsyncKeyState(27) < 0 { return Ok(()); }
    }
    // 显示/隐藏必须共用 Tauri 的可见状态，直接 ShowWindow 会留下透明挡板。
    window.set_focusable(false).map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())
}

#[derive(Default)]
struct Gesture {
    origin: Option<(i32, i32)>,
    fired: bool,
}

impl Gesture {
    fn press(&mut self, point: (i32, i32), allowed: bool) {
        self.origin = allowed.then_some(point);
        self.fired = false;
    }
    fn moved(&mut self, point: (i32, i32), ctrl: bool, escape: bool, threshold: i32) -> bool {
        if escape { self.fired = true; }
        if self.fired || !ctrl { return false; }
        if let Some(origin) = self.origin {
            if (point.0 - origin.0).abs() > threshold || (point.1 - origin.1).abs() > threshold {
                self.fired = true;
                return true;
            }
        }
        false
    }
    fn release(&mut self) { self.origin = None; }
}

fn trigger_profile(ctrl: bool, alt: bool, shift: bool) -> Option<&'static str> {
    if !ctrl || shift { None } else if alt { Some("general") } else { Some("bank") }
}

pub fn install(app: tauri::AppHandle) {
    #[cfg(windows)] windows::install(app);
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::time::Duration;

    #[repr(C)] #[derive(Clone, Copy, Default)]
    struct Point { x: i32, y: i32 }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn WindowFromPoint(point: Point) -> isize;
        fn GetAncestor(hwnd: isize, flags: u32) -> isize;
        fn GetClassNameW(hwnd: isize, text: *mut u16, max: i32) -> i32;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
        pub(super) fn GetAsyncKeyState(key: i32) -> i16;
    }

    fn explorer_at(point: Point) -> bool {
        unsafe {
            let hwnd = GetAncestor(WindowFromPoint(point), 2);
            let mut name = [0u16; 128];
            let length = GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32);
            let class = String::from_utf16_lossy(&name[..length.max(0) as usize]);
            matches!(class.as_str(), "CabinetWClass" | "ExploreWClass" | "Progman" | "WorkerW")
        }
    }

    pub(super) fn install(app: tauri::AppHandle) {
        std::thread::spawn(move || {
            // 不安装输入钩子，不接管鼠标捕获；检测线程停顿也不能阻塞资源管理器输入。
            app.state::<DragState>().installed.store(true, Ordering::Relaxed);
            let mut gesture = Gesture::default();
            let mut previous_down = false;
            let mut sequence = 0u64;
            let mut active = 0u64;
            let threshold = unsafe { GetSystemMetrics(68).max(GetSystemMetrics(69)).max(6) };
            loop {
                let down = unsafe { GetAsyncKeyState(1) < 0 };
                if !down && previous_down {
                    gesture.release();
                    if active != 0 {
                        let id = active; active = 0;
                        let handle = app.clone();
                        // 只由真实 Drop 提交任务；延迟收起用于等待 OLE 投放消息。
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(250));
                            if handle.state::<DragState>().active.compare_exchange(id, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                                let _ = handle.emit_to("quick", "quick-drag-end", id);
                            }
                        });
                    }
                }
                if down {
                    let mut point = Point::default();
                    if unsafe { GetCursorPos(&mut point) } != 0 {
                        if !previous_down { gesture.press((point.x, point.y), explorer_at(point)); }
                        let (profile, escape) = unsafe {
                            (trigger_profile(GetAsyncKeyState(17) < 0, GetAsyncKeyState(18) < 0, GetAsyncKeyState(16) < 0), GetAsyncKeyState(27) < 0)
                        };
                        if escape && active != 0 {
                            let id = active; active = 0;
                            app.state::<DragState>().active.store(0, Ordering::Relaxed);
                            let _ = app.emit_to("quick", "quick-drag-end", id);
                        }
                        if gesture.moved((point.x, point.y), profile.is_some(), escape, threshold) && app.state::<DragState>().ready.load(Ordering::Relaxed) {
                            sequence += 1;
                            active = sequence;
                            app.state::<DragState>().active.store(active, Ordering::Relaxed);
                            let handle = app.clone();
                            let id = active;
                            let _ = app.run_on_main_thread(move || {
                                if handle.state::<DragState>().active.load(Ordering::Relaxed) != id { return; }
                                if let Some(window) = handle.get_webview_window("quick") {
                                    crate::shortcuts::position_window(&window, point.x, point.y);
                                    let _ = window.emit("quick-drag-begin", serde_json::json!({"id":id,"profile":profile.unwrap_or("bank")}));
                                }
                            });
                        }
                    }
                }
                previous_down = down;
                std::thread::sleep(Duration::from_millis(if down { 12 } else { 24 }));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Gesture, trigger_profile};
    #[test]
    fn ctrl_and_ctrl_alt_trigger_separate_profiles() {
        assert_eq!(trigger_profile(true,true,false), Some("general"));
        assert_eq!(trigger_profile(true,false,false), Some("bank"));
        assert_eq!(trigger_profile(false,true,false), None);
        assert_eq!(trigger_profile(true,true,true), None);
    }
    #[test]
    fn click_and_normal_drag_do_not_trigger() {
        let mut g = Gesture::default(); g.press((10, 10), true);
        assert!(!g.moved((13, 12), true, false, 6));
        assert!(!g.moved((80, 90), false, false, 6));
        g.release(); assert!(!g.moved((80, 90), true, false, 6));
    }
    #[test]
    fn modifiers_before_or_after_movement_fire_once() {
        for late in [false, true] {
            let mut g = Gesture::default(); g.press((0, 0), true);
            if late { assert!(!g.moved((30, 0), false, false, 6)); }
            assert!(g.moved((30, 0), true, false, 6));
            assert!(!g.moved((50, 0), true, false, 6));
        }
    }
    #[test]
    fn escape_and_wrong_source_cannot_rearm_until_next_press() {
        let mut g = Gesture::default(); g.press((0, 0), true);
        assert!(!g.moved((30, 0), true, true, 6));
        assert!(!g.moved((30, 0), true, false, 6));
        g.press((0, 0), false); assert!(!g.moved((30, 0), true, false, 6));
        g.press((0, 0), true); assert!(g.moved((30, 0), true, false, 6));
    }
}
