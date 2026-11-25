use crate::domain::{ConversionEvent, ConversionRequest};
use std::{
    error::Error,
    io::{self, BufRead, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) fn emit(event: &ConversionEvent) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, event)?;
    writeln!(out)?;
    Ok(())
}

fn execute() -> Result<ConversionEvent> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let request: ConversionRequest = serde_json::from_str(&line)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    std::thread::spawn(move || {
        // 控制管道关闭或损坏时一并取消，避免父进程退出后继续保存文件。
        for line in io::stdin().lock().lines() {
            let control = line
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
            if control.is_none_or(|value| value["cancel"] == true) {
                break;
            }
        }
        flag.store(true, Ordering::Release);
    });
    match request.mode.as_str() {
        "ocr" => crate::ocr_document::run(&request, &cancelled),
        "pdf" => crate::native_statement::run(&request, &cancelled),
        "images_pdf" | "pdf_images" => crate::image_pdf::run(&request, &cancelled),
        "pdf_merge" | "pdf_split" | "pdf_extract" | "pdf_rotate" => {
            crate::pdf_tools::run(&request, &cancelled)
        }
        "image_convert" => crate::image_convert::run(&request, &cancelled),
        _ => Err("不支持的转换任务".into()),
    }
}

pub(crate) fn run() {
    let event = execute().unwrap_or_else(|e| ConversionEvent::Error {
        message: format!("转换失败：{e}"),
    });
    if emit(&event).is_err() {
        std::process::exit(1);
    }
}

pub(crate) fn sources(request: &ConversionRequest) -> &[String] {
    if request.inputs.is_empty() {
        std::slice::from_ref(&request.input)
    } else {
        &request.inputs
    }
}

#[cfg(windows)]
fn publish(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileW(source: *const u16, target: *const u16) -> i32;
    }
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    // canonicalize 在 Windows 返回支持长路径的绝对路径，目标尚未创建，只解析其父目录。
    let source = wide(&source.canonicalize()?);
    let parent = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let name = target.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let target = wide(&parent.canonicalize()?.join(name));
    // MoveFileW 在目标已存在时失败，避免并发任务覆盖同名结果。
    if unsafe { MoveFileW(source.as_ptr(), target.as_ptr()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn finish(
    first: &Path,
    parent: &Path,
    staged: &Path,
    operation: &str,
    extension: &str,
    cancelled: &AtomicBool,
) -> Result<ConversionEvent> {
    let stem = first
        .file_stem()
        .ok_or("输入路径无文件名")?
        .to_string_lossy();
    for n in 0u64.. {
        if cancelled.load(Ordering::Acquire) {
            return Ok(ConversionEvent::Cancelled {
                message: "任务已取消，未保存结果。".into(),
            });
        }
        let number = if n == 0 {
            String::new()
        } else {
            format!(" ({n})")
        };
        let target = parent.join(format!("{stem}{operation}{number}{extension}"));
        match publish(staged, &target) {
            Ok(()) => {
                return Ok(ConversionEvent::Completed {
                    output: target.to_string_lossy().into_owned(),
                })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists || target.exists() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{fs, os::windows::ffi::OsStrExt};

    #[test]
    fn publishes_in_long_paths_without_overwriting_existing_results() {
        let temp = tempfile::tempdir().unwrap();
        let mut parent = temp.path().to_owned();
        while parent.as_os_str().encode_wide().count() < 270 {
            parent.push("结果归档".repeat(12));
        }
        fs::create_dir_all(&parent).unwrap();
        let existing = parent.join("流水.xlsx");
        fs::write(&existing, b"existing").unwrap();
        let staged = parent.join("staged.xlsx");
        fs::write(&staged, b"new").unwrap();

        let result = finish(
            Path::new("流水.pdf"),
            &parent,
            &staged,
            "",
            ".xlsx",
            &AtomicBool::new(false),
        )
        .unwrap();
        let numbered = parent.join("流水 (1).xlsx");
        assert!(
            matches!(result, ConversionEvent::Completed { output } if Path::new(&output) == numbered)
        );
        assert_eq!(fs::read(&existing).unwrap(), b"existing");
        assert_eq!(fs::read(&numbered).unwrap(), b"new");
        assert!(!staged.exists());
    }
}
