use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
pub struct ConversionRequest {
    pub input: String,
    pub output_dir: Option<String>,
    pub mode: String,
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

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct TaskItem {
    pub input: String,
    pub status: String,
    pub output: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct TaskSnapshot {
    pub running: bool,
    pub owner: String,
    pub items: Vec<TaskItem>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConversionEvent {
    Started { snapshot: TaskSnapshot },
    Item { snapshot: TaskSnapshot },
    Batchcompleted { snapshot: TaskSnapshot },
    Progress { page: u32, total: u32 },
    Completed { output: String },
    Cancelled { message: String },
    Error { message: String },
}

fn default_angle() -> u16 { 90 }

fn default_format() -> String { "png".into() }
