fn main() {
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        // 开发版直接从 .venv 和 build/codecs 读取资源；显式指定的 Tauri 配置优先。
        let overrides = std::env::var("TAURI_CONFIG").unwrap_or_else(|_| "{}".into());
        let mut config: serde_json::Value =
            serde_json::from_str(&overrides).expect("无效的 TAURI_CONFIG");
        if config.pointer("/bundle/resources").is_none() {
            config["bundle"]["resources"] = serde_json::json!([]);
            std::env::set_var("TAURI_CONFIG", config.to_string());
        }
    }
    tauri_build::build()
}
