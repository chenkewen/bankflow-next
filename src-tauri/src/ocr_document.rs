#[path = "ocr_amount_cells.rs"]
mod amount_cells;
#[path = "ocr_field_cells.rs"]
mod field_cells;
#[path = "ocr_payment_dates.rs"]
mod payment_dates;
#[path = "ocr_timestamp.rs"]
mod timestamp;
#[path = "ocr_source.rs"]
mod source;
use crate::{
    domain::{ConversionEvent, ConversionRequest},
    native_ocr_page::{self, OcrEngine},
    ocr_table::Table,
    statement_excel, statement_rows,
    worker::{emit, finish},
};
use image::RgbImage;
use std::{
    error::Error,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn extract_page(
    table: &mut Table,
    engine: &mut OcrEngine,
    image: &RgbImage,
    scale: f64,
    reviews: &mut field_cells::Reviews,
    source_page: Option<(&crate::native_statement::StatementData, usize)>,
) -> Result<()> {
    let cleaned = native_ocr_page::remove_red_stamp(image);
    let (h, v) = crate::ocr_lines::detect(&cleaned, scale);
    // 封面会被推断成“列1”等临时列。在尚无交易时允许下一页的实际表头建立版式。
    let cover = table.columns.first().is_some_and(|name| name == "列1")
        && !table.rows.iter().any(|r| {
            r.iter().any(|v| statement_rows::starts_with_date(v))
                && r.iter().filter(|v| !v.is_empty()).count() >= 2
        });
    let reference_lines = if v.len() < 4 && !cover {
        table.reference_lines()
    } else {
        Vec::new()
    };
    let effective = if v.len() < 4 && !reference_lines.is_empty() {
        &reference_lines
    } else {
        &v
    };
    let items = engine.recognize(&cleaned)?;
    let (words, chars) = crate::ocr_geometry::convert(
        &items,
        image.width() as f64,
        image.height() as f64,
        scale,
        h.len() < 4,
        effective,
    );
    if words.is_empty() {
        return Ok(());
    }
    let parse = |target: &mut Table| {
        target.page(
            image.width() as f64 * scale,
            image.height() as f64 * scale,
            &words,
            &chars,
            &h,
            &v,
        )
    };
    let parsed = if cover {
        let mut next = Table::default();
        if parse(&mut next) && next.columns.first().is_some_and(|name| name != "列1") {
            let mut metadata = std::mem::take(&mut table.metadata);
            metadata.append(&mut next.metadata);
            next.metadata = metadata;
            *table = next;
            true
        } else {
            parse(table)
        }
    } else {
        parse(table)
    };
    if parsed {
        timestamp::reread(table, &cleaned, engine, scale)?;
        amount_cells::reread(table, &cleaned, engine, &items, scale)?;
        payment_dates::reread(table, &cleaned, engine, scale)?;
        let verified =
            source_page.and_then(|(source, index)| source::matched_page(source, index, table));
        field_cells::reread(table, &cleaned, engine, &items, scale, reviews, verified)?;
    }
    Ok(())
}

pub(crate) fn run(request: &ConversionRequest, cancelled: &AtomicBool) -> Result<ConversionEvent> {
    let input = std::path::absolute(&request.input)?;
    let parent = request
        .output_dir
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(Path::new)
        .or(input.parent())
        .ok_or("输出目录无效")?;
    let Some(extension) = input.extension().and_then(|e| e.to_str()) else {
        return Err("请选择 PDF 或流水图片".into());
    };
    let cancelled_event = || ConversionEvent::Cancelled {
        message: "任务已取消".into(),
    };
    let mut table = Table::default();
    let mut reviews = field_cells::Reviews::default();
    let mut reference = None;
    let mut engine = OcrEngine::new()?;
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => {
            let pdfium = crate::pdfium_engine::open()?;
            let password = (!request.password.is_empty()).then_some(request.password.as_str());
            // OCR 渲染会剥离印章，文字参考需从独立文档读取。
            {
                let source_doc = pdfium.load_pdf_from_file(&input, password)?;
                match crate::native_statement::extract(&source_doc, cancelled, false) {
                    Ok(data) => reference = data,
                    Err(_) => eprintln!("此文档的文字层无法用于交叉核对，保留 OCR 结果。"),
                }
            }
            if cancelled.load(Ordering::Acquire) {
                return Ok(cancelled_event());
            }
            let document = pdfium.load_pdf_from_file(&input, password)?;
            let total = document.pages().len() as u32;
            for (index, mut page) in document.pages().iter().enumerate() {
                if cancelled.load(Ordering::Acquire) {
                    return Ok(cancelled_event());
                }
                let image = native_ocr_page::render_pdf(&mut page)?;
                extract_page(
                    &mut table,
                    &mut engine,
                    &image,
                    0.5,
                    &mut reviews,
                    reference.as_ref().map(|source| (source, index)),
                )?;
                emit(&ConversionEvent::Progress {
                    page: index as u32 + 1,
                    total,
                })?;
            }
        }
        "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff" | "webp" => {
            // 图片 OCR 兼容多帧输入：保留原入口只读取首帧的行为。
            use image::ImageDecoder;
            let reader = image::ImageReader::open(&input)?.with_guessed_format()?;
            let fax = if reader.format() == Some(image::ImageFormat::Tiff) {
                crate::image_convert::read_special_tiff(&input, true)?
            } else {
                None
            };
            let image = match fax {
                Some((image, _)) => image,
                None => {
                    let mut decoder = reader.into_decoder()?;
                    let orientation = decoder.orientation()?;
                    let mut image = image::DynamicImage::from_decoder(decoder)?;
                    image.apply_orientation(orientation);
                    image
                }
            }
            .into_rgb8();
            extract_page(
                &mut table,
                &mut engine,
                &image,
                595. / image.width() as f64,
                &mut reviews,
                None,
            )?;
            emit(&ConversionEvent::Progress { page: 1, total: 1 })?;
        }
        _ => return Err("文件类型与解析方式不匹配".into()),
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(cancelled_event());
    }
    let mut rows = statement_rows::ocr(&table.columns, table.rows);
    crate::ocr_finance::finalize::prepare(&table.columns, &mut rows, &mut table.metadata);
    let report = crate::ocr_finance::correct(&table.columns, &mut rows);
    if !report.warnings.is_empty() {
        let sample = report
            .warnings
            .iter()
            .take(8)
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "余额钩稽提示：第 {sample} 等 {} 行的发生额与相邻余额不一致，建议人工复核",
            report.warnings.len()
        );
    }
    if report.repaired > 0 {
        eprintln!("余额钩稽核验：已自动修正 {} 处金额偏差", report.repaired);
    }
    let mut notes = reviews.locate(&rows);
    if let Some(statement) = reference {
        let changes = source::verify(&mut table.columns, &mut rows, statement, &mut notes);
        if changes > 0 {
            eprintln!("已与可见 PDF 原文逐笔核对，校正 {changes} 格。");
        }
    }
    amount_cells::mark_unreadable(&table.columns, &rows, &mut notes);
    if cancelled.load(Ordering::Acquire) {
        return Ok(cancelled_event());
    }
    let staging = tempfile::Builder::new()
        .prefix("bankflow-")
        .tempdir_in(parent)?;
    let file = staging.path().join("result.xlsx");
    statement_excel::save_reviewed(
        statement_excel::Export {
            output: file.clone(),
            columns: table.columns,
            rows,
            metadata: table.metadata,
        },
        &notes,
    )?;
    finish(&input, parent, &file, "", ".xlsx", cancelled)
}
