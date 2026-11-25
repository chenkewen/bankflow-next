use std::sync::Mutex;
use tauri::{Emitter, Manager, menu::{Menu, MenuItem}, tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState}};
use tauri_plugin_dialog::DialogExt;

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
#[serde(rename_all="snake_case")]
pub enum CloseAction { #[default] Ask, Tray, Exit }
#[derive(Default)]
pub struct DesktopState { action: Mutex<CloseAction> }

// 旧版只有用户主动切换托盘菜单才会写文件，因此可保留已有明确选择。
fn parse_preference(bytes: &[u8])->Result<CloseAction,String> {
    let value:serde_json::Value=serde_json::from_slice(bytes).map_err(|e|e.to_string())?;
    if let Some(action)=value.get("close_action") {
        return serde_json::from_value(action.clone()).map_err(|e|e.to_string());
    }
    Ok(match value.get("close_to_tray").and_then(|v|v.as_bool()) {
        Some(true)=>CloseAction::Tray, Some(false)=>CloseAction::Exit, None=>CloseAction::Ask
    })
}

#[tauri::command]
pub fn desktop_preference(app:tauri::AppHandle)->Result<CloseAction,String> {
    app.state::<DesktopState>().action.lock().map(|v|*v).map_err(|_|"无法读取关闭偏好".into())
}

#[tauri::command]
pub fn save_desktop_preference(app:tauri::AppHandle, action:CloseAction)->Result<(),String> {
    let state=app.state::<DesktopState>();
    let mut current=state.action.lock().map_err(|_|"无法保存关闭偏好")?;
    let config=app.path().app_config_dir().map_err(|e|e.to_string())?.join("desktop.json");
    std::fs::create_dir_all(config.parent().ok_or("配置目录不可用")?).map_err(|e|e.to_string())?;
    let json=serde_json::to_string(&serde_json::json!({"close_action":action})).map_err(|e|e.to_string())?;
    std::fs::write(config,format!("{json}\n")).map_err(|e|format!("偏好保存失败：{e}"))?;
    *current=action;
    Ok(())
}

#[tauri::command]
pub fn apply_close_choice(app:tauri::AppHandle, action:CloseAction, remember:bool)->Result<(),String> {
    if action==CloseAction::Ask {return Err("请选择最小化到托盘或退出应用".into());}
    if action==CloseAction::Exit && crate::services::snapshot(&app.state::<crate::services::ConversionState>())?.running {
        return Err("任务仍在处理中，请先取消任务或等待完成后再退出。也可以最小化到托盘继续处理。".into());
    }
    if remember {save_desktop_preference(app.clone(),action)?;}
    match action {
        CloseAction::Tray=>app.get_webview_window("main").ok_or("主窗口不可用")?.hide().map_err(|e|e.to_string()),
        CloseAction::Exit=>exit_application(app),
        CloseAction::Ask=>Ok(())
    }
}

pub fn request_close(app:&tauri::AppHandle) {
    match desktop_preference(app.clone()) {
        Ok(CloseAction::Ask)=>{show_main(app);let _=app.emit_to("main","confirm-close",());},
        Ok(action)=>{if let Err(message)=apply_close_choice(app.clone(),action,false) {app.dialog().message(message).title("BankFlow Next").show(|_|{});}},
        Err(message)=>{app.dialog().message(message).title("BankFlow Next").show(|_|{});}
    }
}

pub fn show_main(app: &tauri::AppHandle) {
    if let Some(window)=app.get_webview_window("main") {
        let _=window.unminimize();let _=window.show();let _=window.set_focus();
    }
}

const IDLE_TOOLTIP: &str = "BankFlow Next · 本地文件工作台";

pub fn update_status(app: &tauri::AppHandle) {
    let handle = app.clone();
    // 刷新时读取最新状态，避免上一任务较迟抵达的提示覆盖清除或重新开始后的托盘状态。
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id("bankflow") else { return };
        let Ok(snapshot) = crate::services::snapshot(&handle.state::<crate::services::ConversionState>()) else { return };
        if snapshot.items.is_empty() {
            let _ = tray.set_tooltip(Some(IDLE_TOOLTIP));
            return;
        }
        let completed = snapshot.items.iter().filter(|item| item.status == "completed").count();
        let failed = snapshot.items.iter().filter(|item| item.status == "error").count();
        let phase = if snapshot.running { "处理中" } else { "任务结束" };
        let _ = tray.set_tooltip(Some(format!("BankFlow Next · {phase} · 成功 {completed}/{} · 失败 {failed}", snapshot.items.len())));
    });
}

#[tauri::command]
pub fn show_tasks(app: tauri::AppHandle) {show_main(&app);let _=app.emit_to("main","show-tasks",());}

#[tauri::command]
pub fn exit_application(app: tauri::AppHandle)->Result<(),String> {
    if crate::services::snapshot(&app.state::<crate::services::ConversionState>())?.running {
        return Err("任务仍在处理中，请先取消任务或等待完成后再退出。".into());
    }
    app.exit(0);Ok(())
}

pub fn request_exit(app: &tauri::AppHandle) {
    if let Err(message)=exit_application(app.clone()) {
        show_main(app);
        app.dialog().message(message).title("BankFlow Next · 暂不能退出").show(|_|{});
    }
}

pub fn install(app: &tauri::AppHandle)->Result<(),Box<dyn std::error::Error>> {
    let config=app.path().app_config_dir()?.join("desktop.json");
    let action=match std::fs::read(&config) {
        Ok(bytes)=>parse_preference(&bytes).unwrap_or_else(|error| {eprintln!("无法解析桌面偏好：{error}");CloseAction::Ask}),
        Err(error)=>{if error.kind()!=std::io::ErrorKind::NotFound {eprintln!("无法读取桌面偏好：{error}");} CloseAction::Ask}
    };
    *app.state::<DesktopState>().action.lock().map_err(|_|"无法初始化关闭偏好")?=action;
    let open=MenuItem::with_id(app,"open","打开 BankFlow Next",true,None::<&str>)?;
    let tasks=MenuItem::with_id(app,"tasks","查看转换任务",true,None::<&str>)?;
    let settings=MenuItem::with_id(app,"settings","系统设置",true,None::<&str>)?;
    let exit=MenuItem::with_id(app,"exit","退出 BankFlow Next",true,None::<&str>)?;
    let menu=Menu::with_items(app,&[&open,&tasks,&settings,&exit])?;
    let icon=app.default_window_icon().ok_or("缺少托盘图标")?.clone();
    TrayIconBuilder::with_id("bankflow").icon(icon).tooltip(IDLE_TOOLTIP)
        .menu(&menu).show_menu_on_left_click(false)
        .on_tray_icon_event(|tray,event| {
            if matches!(event,TrayIconEvent::Click {button:MouseButton::Left,button_state:MouseButtonState::Up,..}) {show_main(tray.app_handle());}
        })
        .on_menu_event(move |app,event| match event.id.as_ref() {
            "open"=>show_main(app),
            "tasks"=>show_tasks(app.clone()),
            "exit"=>request_exit(app),
            "settings"=>{show_main(app);let _=app.emit_to("main","show-settings",());},
            _=>{}
        }).build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_preferences_migrate_and_validate() {
        assert_eq!(parse_preference(br#"{}"#).unwrap(),CloseAction::Ask);
        assert_eq!(parse_preference(br#"{"close_to_tray":true}"#).unwrap(),CloseAction::Tray);
        assert_eq!(parse_preference(br#"{"close_to_tray":false}"#).unwrap(),CloseAction::Exit);
        for action in [CloseAction::Ask,CloseAction::Tray,CloseAction::Exit] {
            let bytes=serde_json::to_vec(&serde_json::json!({"close_action":action})).unwrap();
            assert_eq!(parse_preference(&bytes).unwrap(),action);
        }
        assert!(parse_preference(br#"{"close_action":"invalid"}"#).is_err());
        assert!(parse_preference(b"broken").is_err());
    }
}
