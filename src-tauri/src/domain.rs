use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
pub struct ConversionRequest {
    pub input: String,
    pub output_dir: Option<String>,
    pub mode: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub pages: String,
    #[serde(default = "default_angle")]
    pub angle: u16,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default = "default_angle")]
    pub quality: u16,
    #[serde(default)]
    pub separate: bool,
}

pub(crate) fn statement_input(path: &std::path::Path, mode: &str) -> bool {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    extension == "pdf" || (mode == "ocr" && matches!(
        extension.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff" | "webp"
    ))
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct TaskItem {
    pub input: String,
    pub status: String,
    pub output: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct TaskSnapshot {
    pub running: bool,
    pub owner: String,
    pub items: Vec<TaskItem>,
    pub logs: Vec<String>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConversionEvent {
    Cleared { snapshot: TaskSnapshot },
    Started { snapshot: TaskSnapshot },
    Item { snapshot: TaskSnapshot },
    Batchcompleted { snapshot: TaskSnapshot },
    Progress { page: u32, total: u32 },
    Log { message: String },
    Completed { output: String },
    Cancelled { message: String },
    Error { message: String },
}

fn default_angle() -> u16 { 90 }

fn default_format() -> String { "png".into() }
