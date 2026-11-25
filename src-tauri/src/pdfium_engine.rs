use pdfium_render::prelude::Pdfium;
use std::{error::Error, path::PathBuf};

pub(crate) fn open() -> Result<Pdfium, Box<dyn Error>> {
    // 与已有 PDF 引擎复用相同版本，避免开发版和安装版渲染结果不一致。
    let directory = std::env::current_exe()?
        .parent()
        .ok_or("无法定位程序目录")?
        .to_owned();
    let portable = directory.join("pdfium.dll");
    let library = if portable.is_file() {
        portable
    } else if cfg!(debug_assertions) {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.venv/Lib/site-packages/pypdfium2_raw/pdfium.dll")
    } else {
        directory.join("runtime/pdfium.dll")
    };
    Ok(Pdfium::new(Pdfium::bind_to_library(library)?))
}
