use crate::domain::{ConversionEvent, ConversionRequest};
use crate::worker::{emit, finish, sources};
use pdfium_render::prelude::{PdfPageIndex, PdfPageRenderRotation};
use std::{
    collections::HashSet,
    error::Error,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn page_indices(text: &str, count: PdfPageIndex) -> Result<Vec<PdfPageIndex>> {
    if text.trim().is_empty() {
        return Ok((0..count).collect());
    }
    let mut pages = Vec::new();
    let mut seen = HashSet::new();
    let parse = |s: &str| -> Result<PdfPageIndex> {
        let s = s.trim();
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err("页码格式应为 1-3,5".into());
        }
        Ok(s.parse().unwrap_or(PdfPageIndex::MAX))
    };
    for part in text.split([',', '，']) {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let first = parse(a)?;
        let last = parse(b)?;
        if first < 1 || first > last || last > count {
            return Err(format!("页码超出范围或顺序错误，文档共 {count} 页").into());
        }
        for i in first - 1..last {
            if !seen.insert(i) {
                return Err("页码不能重复".into());
            }
            pages.push(i);
        }
    }
    Ok(pages)
}

fn cancelled_event() -> ConversionEvent {
    ConversionEvent::Cancelled {
        message: "任务已取消，未保存结果。".into(),
    }
}

pub(crate) fn run(request: &ConversionRequest, cancelled: &AtomicBool) -> Result<ConversionEvent> {
    let mode = request.mode.as_str();
    let label = match mode {
        "pdf_merge" => "合并",
        "pdf_split" => "拆分",
        "pdf_extract" => "提取",
        "pdf_rotate" => "旋转",
        _ => return Err("不支持的 PDF 操作".into()),
    };
    let inputs = sources(request);
    if (mode == "pdf_merge" && inputs.len() < 2) || (mode != "pdf_merge" && inputs.len() != 1) {
        return Err("合并至少选择两个文件，其他操作请选择一个文件".into());
    }
    if mode == "pdf_rotate" && !matches!(request.angle, 90 | 180 | 270) {
        return Err("旋转角度必须为 90、180 或 270".into());
    }

    let first = std::path::absolute(&inputs[0])?;
    let parent = request
        .output_dir
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(Path::new)
        .or(first.parent())
        .ok_or("输出位置必须为目录")?;
    let pdfium = crate::pdfium_engine::open()?;
    let password = if request.password.is_empty() {
        None
    } else {
        Some(request.password.as_str())
    };
    let mut docs = Vec::new();
    let mut total = 0u32;
    let mut rotate = HashSet::new();
    for input in inputs {
        if cancelled.load(Ordering::Acquire) {
            return Ok(cancelled_event());
        }
        let path = Path::new(input);
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("pdf"))
        {
            return Err("请选择 PDF 文件".into());
        }
        let doc = pdfium.load_pdf_from_file(path, password)?;
        let count = doc.pages().len();
        let pages = match mode {
            "pdf_merge" => (0..count).collect(),
            "pdf_rotate" => {
                rotate.extend(page_indices(&request.pages, count)?);
                (0..count).collect()
            }
            _ => page_indices(&request.pages, count)?,
        };
        total = total
            .checked_add(u32::try_from(pages.len())?)
            .ok_or("PDF 页数过多")?;
        docs.push((doc, pages));
    }
    if total == 0 {
        return Err("没有可处理的页面".into());
    }
    let stage = tempfile::Builder::new()
        .prefix("bankflow-")
        .tempdir_in(parent)?;
    let single = stage.path().join("result.pdf");
    let split = mode == "pdf_split";
    let mut combined = pdfium.create_new_pdf()?;
    let mut done = 0;
    for (doc, pages) in &docs {
        for &index in pages {
            if cancelled.load(Ordering::Acquire) {
                return Ok(cancelled_event());
            }
            if split {
                let mut target = pdfium.create_new_pdf()?;
                target.pages_mut().copy_page_from_document(doc, index, 0)?;
                target.save_to_file(&stage.path().join(format!("第{}页.pdf", index + 1)))?;
            } else {
                let dest = combined.pages().len();
                combined
                    .pages_mut()
                    .copy_page_from_document(doc, index, dest)?;
                if rotate.contains(&index) {
                    let mut page = combined.pages().get(dest)?;
                    let angle = (page.rotation()?.as_degrees() as u16 + request.angle) % 360;
                    let rotation = match angle {
                        0 => PdfPageRenderRotation::None,
                        90 => PdfPageRenderRotation::Degrees90,
                        180 => PdfPageRenderRotation::Degrees180,
                        _ => PdfPageRenderRotation::Degrees270,
                    };
                    page.set_rotation(rotation);
                }
            }
            done += 1;
            emit(&ConversionEvent::Progress { page: done, total })?;
        }
    }
    if !split {
        combined.save_to_file(&single)?;
    }
    finish(
        &first,
        parent,
        if split { stage.path() } else { &single },
        &format!("_{label}"),
        if split { "" } else { ".pdf" },
        cancelled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_preserve_order_and_reject_duplicates() {
        assert_eq!(page_indices("3， 1 - 2", 3).unwrap(), [2, 0, 1]);
        assert_eq!(page_indices(" ", 2).unwrap(), [0, 1]);
        for invalid in ["0", "3-2", "3,3", "2,", "1-99", "-2", "1-2-3"] {
            assert!(page_indices(invalid, 3).is_err());
        }
    }
}
