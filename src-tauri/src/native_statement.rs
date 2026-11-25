#[path = "native_payment.rs"]
mod payment;
#[path = "native_unruled.rs"]
pub(crate) mod unruled;
use crate::{
    domain::{ConversionEvent, ConversionRequest},
    native_chars, native_fields, native_table, statement_excel, statement_rows,
    worker::{emit, finish},
};
use std::{
    error::Error,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

fn normalize_page(page: &mut pdfium_render::prelude::PdfPage) -> Result<(), Box<dyn Error>> {
    use pdfium_render::prelude::*;
    let crop = page.boundaries().bounding()?.bounds;
    let media = match page.boundaries().media() {
        Ok(media) => media.bounds,
        Err(_) => {
            // PDFium 的 MediaBox getter 只读取页字典。空 CropBox 让有效边界按页树继承 MediaBox。
            page.boundaries_mut()
                .set_crop(PdfRect::new_from_values(0., 0., 0., 0.))?;
            page.boundaries().bounding()?.bounds
        }
    };
    let (x0, y0, x1, y1) = (
        media.left().value as f64,
        media.bottom().value as f64,
        media.right().value as f64,
        media.top().value as f64,
    );
    let rotation = page.rotation()?;
    if rotation == PdfPageRenderRotation::None && x0 == 0. && y0 == 0. && crop == media {
        return Ok(());
    }
    // pdfminer 以 MediaBox 为基准先旋转后平移。仅修改内存页，不改变源 PDF。
    let (m, w, h) = match rotation {
        PdfPageRenderRotation::Degrees90 => ([0., -1., 1., 0., -y0, x1], y1 - y0, x1 - x0),
        PdfPageRenderRotation::Degrees180 => ([-1., 0., 0., -1., x1, y1], x1 - x0, y1 - y0),
        PdfPageRenderRotation::Degrees270 => ([0., 1., -1., 0., y1, -x0], y1 - y0, x1 - x0),
        PdfPageRenderRotation::None => ([1., 0., 0., 1., -x0, -y0], x1 - x0, y1 - y0),
    };
    for o in page.objects().iter() {
        let b = o.bindings();
        unsafe {
            b.FPDFPageObj_Transform(
                b.get_handle_from_object(&o),
                m[0],
                m[1],
                m[2],
                m[3],
                m[4],
                m[5],
            );
        }
    }
    page.set_rotation(PdfPageRenderRotation::None);
    let size = PdfRect::new_from_values(0., 0., h as f32, w as f32);
    page.boundaries_mut().set_media(size)?;
    page.boundaries_mut().set_crop(size)?;
    Ok(())
}

pub fn run(
    request: &ConversionRequest,
    cancelled: &AtomicBool,
) -> Result<ConversionEvent, Box<dyn Error>> {
    let input = std::path::absolute(&request.input)?;
    if !input.is_file()
        || !input
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        return Err("请选择 PDF 文件".into());
    }
    let parent = request
        .output_dir
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(Path::new)
        .or(input.parent())
        .ok_or("输出目录无效")?;
    let pdfium = crate::pdfium_engine::open()?;
    let doc = pdfium.load_pdf_from_file(
        &input,
        (!request.password.is_empty()).then_some(request.password.as_str()),
    )?;
    let parsed = extract(&doc, cancelled, true)?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(ConversionEvent::Cancelled {
            message: "任务已取消".into(),
        });
    }
    let Some(StatementData {
        columns,
        rows,
        metadata,
        ..
    }) = parsed
    else {
        return Err("普通 PDF 解析未找到可识别的流水表头或记录。扫描件可选择 OCR 解析。".into());
    };
    let stage = tempfile::Builder::new()
        .prefix("bankflow-")
        .tempdir_in(parent)?;
    let file = stage.path().join("result.xlsx");
    statement_excel::save(statement_excel::Export {
        output: file.clone(),
        metadata,
        columns,
        rows,
    })?;
    finish(&input, parent, &file, "", ".xlsx", cancelled)
}

#[derive(Default)]
pub(crate) struct StatementData {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub metadata: Vec<Vec<(String, String)>>,
    pub all_pages_have_rows: bool,
    pub page_ends: Vec<usize>,
}

pub(super) fn amount_cell(value: &str) -> bool {
    !statement_rows::starts_with_date(value)
        && value
            .trim()
            .replace([',', '￥', '¥'], "")
            .parse::<f64>()
            .is_ok_and(f64::is_finite)
}

// 仅当相邻两格的账号片段在 PDF 中确实构成一个连续词时，把越界的尾数归还账号。
fn restore_account_tail(
    row: &mut [String],
    geometry: &[Option<[f64; 4]>],
    columns: &[String],
    glyphs: &[native_chars::Glyph],
) {
    for (i, pair) in columns.windows(2).enumerate() {
        if !(pair[0].contains("账号") || pair[0].contains("账户"))
            || !statement_rows::date_header(&pair[1])
            || i + 1 >= row.len()
        {
            continue;
        }
        let Some((suffix, date)) = row[i + 1].split_once(' ') else {
            continue;
        };
        if suffix.is_empty()
            || suffix.len() > 3
            || !suffix.bytes().all(|b| b.is_ascii_digit())
            || !statement_rows::starts_with_date(date)
        {
            continue;
        }
        let (Some(area), Some(next)) = (geometry[i], geometry[i + 1]) else {
            continue;
        };
        let account = format!("{}{suffix}", row[i]);
        if native_fields::words(
            glyphs.iter().filter(|g| {
                let y = (g.top + g.bottom) * 0.5;
                y >= area[1] && y < area[3] && g.x0 >= area[0] - 2. && g.x0 < next[2] && g.x1 > g.x0
            }),
            f64::INFINITY,
        )
        .iter()
        .any(|w| w.text == account)
        {
            let date = date.to_owned();
            row[i] = account;
            row[i + 1] = date;
        }
    }
}

pub(crate) fn transaction_row(row: &[String]) -> bool {
    row.iter().any(|v| statement_rows::starts_with_date(v)) && row.iter().any(|v| amount_cell(v))
}
// 元数据标题常含“交易”“时间”等字样；要确认多个独立字段，或从后续数据验证陌生表头。
struct GridHeader {
    columns: Vec<String>,
    prefix: usize,
    data_start: usize,
}

fn grid_header(rows: &[Vec<String>]) -> Option<GridHeader> {
    for (i, row) in rows.iter().enumerate() {
        let labels: Vec<_> = row.iter().filter(|s| !s.is_empty()).collect();
        if labels.len() >= 2
            && !transaction_row(row)
            && labels
                .iter()
                .filter(|s| unruled::KEYWORDS.iter().any(|k| s.contains(k)) || s.contains("交易"))
                .count()
                >= 2
        {
            return Some(GridHeader {
                data_start: i + 1,
                prefix: i,
                columns: row.iter().map(|c| native_fields::clean_header(c)).collect(),
            });
        }
    }
    let start = rows.iter().position(|r| transaction_row(r))?;
    let width = rows[start].len();
    if width < 2 {
        return None;
    }
    if start > 0 {
        let previous = &rows[start - 1];
        let names = previous
            .iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if previous.len() == width
            && names.len() * 2 >= width.max(4)
            && names
                .iter()
                .all(|v| v.chars().any(char::is_alphabetic) && !statement_rows::contains_date(v))
        {
            return Some(GridHeader {
                data_start: start,
                prefix: start - 1,
                columns: previous
                    .iter()
                    .map(|c| native_fields::clean_header(c))
                    .collect(),
            });
        }
    }
    Some(GridHeader {
        data_start: start,
        prefix: start,
        columns: (1..=width).map(|i| format!("列{i}")).collect(),
    })
}

pub(crate) fn extract(
    doc: &pdfium_render::prelude::PdfDocument,
    cancelled: &AtomicBool,
    progress: bool,
) -> Result<Option<StatementData>, Box<dyn Error>> {
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    let mut metadata = Vec::new();
    let mut payment_layout = false;
    let mut layout: Option<unruled::Layout> = None;
    let mut all_pages_have_rows = true;
    let mut page_ends = Vec::new();
    // 已确认物理网格时，旧解析器允许无交易的表和未收录的表头。保存其内容以便后页优先识别交易表。
    let mut other_grid: Option<StatementData> = None;
    let total = doc.pages().len();
    for (index, mut page) in doc.pages().iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }
        let before = rows.len();
        normalize_page(&mut page)?;
        let tables = native_table::tables(&page)?;
        crate::native_font::prepare_text_page(&page)?;
        let (glyphs, small_glyphs) = native_chars::glyphs(&page)?;
        let first = columns.is_empty();
        let has_grid = tables
            .iter()
            .any(|t| t.len() >= 2 && t.iter().any(|r| r.len() >= 3));
        if first && has_grid && index < 3 {
            other_grid.get_or_insert_with(StatementData::default);
        }
        if first && !has_grid {
            let separators = unruled::horizontal_lines(&page)?;
            if let Some(found) = unruled::Layout::detect(
                &glyphs,
                f64::from(page.width().value),
                separators.len() >= 4,
            ) {
                metadata = native_fields::metadata(&glyphs, found.top - 5.);
                columns = found.columns.clone();
                layout = Some(found);
            }
        }
        if let Some(layout) = layout.as_mut() {
            let separators = unruled::horizontal_lines(&page)?;
            rows.extend(
                layout
                    .rows(&glyphs, first, &separators)
                    .into_iter()
                    .filter(|r| r != &columns),
            );
        } else {
            let cells = native_chars::cells(&glyphs, &tables);
            for (table, values) in tables.iter().zip(cells) {
                let parent_header = columns.is_empty()
                    && values.iter().any(|row| {
                        row.iter()
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .eq(["交易单号", "具体交易明细"])
                    });
                let payment_header = parent_header
                    .then(|| payment::header(table, &glyphs))
                    .flatten();
                if payment_header.is_some() || payment_layout {
                    let extracted =
                        payment::rows(table, &glyphs, payment_header).ok_or_else(|| {
                            format!("第 {} 页的支付明细无法按交易日期与金额逐行对应", index + 1)
                        })?;
                    if let Some(top) = payment_header {
                        metadata = native_fields::metadata(&glyphs, top - 5.);
                        columns = payment::COLUMNS.map(str::to_owned).to_vec();
                        payment_layout = true;
                    }
                    rows.extend(extracted);
                    continue;
                }
                let mut page_rows = Vec::new();
                let mut top = None;
                for (geometry, row) in table.iter().zip(values) {
                    let mut row = row
                        .iter()
                        .map(|v| native_fields::sanitize(v))
                        .collect::<Vec<_>>();
                    native_chars::recover_small_cells(&mut row, geometry, &small_glyphs);
                    restore_account_tail(&mut row, geometry, &columns, &glyphs);
                    if !native_fields::skip(&row) {
                        page_rows.push(row);
                        top.get_or_insert_with(|| {
                            geometry
                                .iter()
                                .flatten()
                                .map(|r| r[1])
                                .fold(f64::INFINITY, f64::min)
                        });
                    }
                }
                let mut start = 0;
                if columns.is_empty() {
                    let Some(mut header) = grid_header(&page_rows) else {
                        if let Some(grid) = other_grid.as_mut() {
                            let mut remaining = page_rows.into_iter();
                            if grid.columns.is_empty() {
                                if let Some(first) = remaining.next() {
                                    grid.columns = first
                                        .iter()
                                        .map(|v| native_fields::clean_header(v))
                                        .collect();
                                    grid.metadata =
                                        native_fields::metadata(&glyphs, top.unwrap_or(0.));
                                }
                            }
                            grid.rows
                                .extend(remaining.filter(|row| row != &grid.columns));
                        }
                        continue;
                    };
                    metadata = native_fields::metadata(&glyphs, top.unwrap_or(0.));
                    if let Some(previous) = other_grid.take() {
                        // 陌生列名可能在前一页，续页从日期金额开始时沿用已有表头和前页记录。
                        if header.data_start == header.prefix
                            && previous.columns.len() == header.columns.len()
                            && previous.columns.iter().filter(|v| !v.is_empty()).count() >= 2
                        {
                            header.columns = previous.columns;
                            metadata = previous.metadata;
                            rows.extend(previous.rows);
                        }
                    }
                    // 表格中位于实际表头之前的键值或标题仍属于元数据。
                    for prefix in &page_rows[..header.prefix] {
                        let vals: Vec<_> =
                            prefix.iter().filter(|v| !v.is_empty()).cloned().collect();
                        match vals.as_slice() {
                            [key, value] => metadata.push(vec![(key.clone(), value.clone())]),
                            [title] => metadata.push(vec![(title.clone(), String::new())]),
                            _ => {}
                        }
                    }
                    columns = header.columns;
                    start = header.data_start;
                } else if let Some(header) = page_rows
                    .iter()
                    .take_while(|row| !transaction_row(row))
                    .position(|row| row == &columns)
                {
                    start = header + 1;
                }
                rows.extend(page_rows.into_iter().skip(start).filter(|r| r != &columns));
            }
        }
        // 有图无正文、仅有摘要或表头的页不能当作已经取得可用于校验的交易。
        all_pages_have_rows &= rows[before..]
            .iter()
            .any(|row| row.iter().any(|v| statement_rows::contains_date(v)));
        page_ends.push(rows.len());
        if progress {
            emit(&ConversionEvent::Progress {
                page: index as u32 + 1,
                total: total as u32,
            })?;
        }
    }
    if columns.is_empty() {
        if let Some(grid) = other_grid {
            return Ok(Some(StatementData {
                rows: statement_rows::pdf(&grid.columns, grid.rows),
                ..grid
            }));
        }
        return Ok(None);
    }
    let rows = statement_rows::pdf(&columns, rows);
    // 跨行合并后，物理页的原始行号不能再索引成品交易。
    if page_ends.last() != Some(&rows.len()) {
        page_ends.clear();
    }
    Ok(Some(StatementData {
        columns,
        rows,
        metadata,
        all_pages_have_rows,
        page_ends,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn period_and_caption_precede_actual_grid_columns() {
        let rows = [
            ["交易明细对应时间段", "2026-01-01 至 2026-01-31", ""],
            ["具体交易明细", "", ""],
            ["交易单号", "交易时间", "金额"],
            ["001", "2026-01-02", "15.00"],
        ]
        .map(|r| r.map(str::to_owned).to_vec());
        let h = grid_header(&rows).unwrap();
        assert_eq!((h.prefix, h.data_start), (2, 3));
        assert_eq!(h.columns, rows[2]);
    }
    #[test]
    fn preserve_foreign_header_and_headerless_first_payment() {
        let rows =
            [["Booking", "Value"], ["2026-01-02", "15.00"]].map(|r| r.map(str::to_owned).to_vec());
        let h = grid_header(&rows).unwrap();
        assert_eq!((h.columns, h.data_start), (rows[0].clone(), 1));
        let h = grid_header(&rows[1..]).unwrap();
        assert_eq!(h.data_start, 0);
        assert_eq!(h.columns, ["列1", "列2"]);
        assert!(!transaction_row(&["20260102".into(), "说明".into()]));
    }
}
