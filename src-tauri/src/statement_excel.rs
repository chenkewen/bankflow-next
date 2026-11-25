use regex::Regex;
use rust_xlsxwriter::{Format, FormatAlign, FormatBorder, Note, Workbook, Worksheet, XlsxError};
use std::collections::HashMap;
use std::{error::Error, path::PathBuf, sync::LazyLock};

#[cfg_attr(any(debug_assertions, test), derive(serde::Deserialize))]
pub(crate) struct Export {
    pub output: PathBuf,
    pub metadata: Vec<Vec<(String, String)>>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

pub(crate) fn amount_literal(text: &str) -> bool {
    static MONEY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[+-]?(?:[0-9]+|[0-9]{1,3}(?:,[0-9]{3})+)(?:\.[0-9]{1,2})?$").unwrap()
    });
    MONEY.is_match(text.trim())
}

fn amount(text: &str) -> Option<f64> {
    let text = text.trim();
    if !amount_literal(text) {
        return None;
    }
    let raw = text.replace(',', "");
    let digits = raw.trim_start_matches(['+', '-']).replace('.', "");
    if digits.trim_start_matches('0').len() > 15 {
        return None;
    }
    raw.parse().ok()
}

pub(crate) fn amount_column(name: &str) -> bool {
    // 表头可能跨行、中英并列，或使用 PDF 中形近的康熙部首。只匹配完整名称，保护账号和编号。
    let compact: String = name
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '（' => '(',
            '）' => ')',
            '⾦' => '金',
            '⽀' => '支',
            _ => c.to_ascii_lowercase(),
        })
        .collect();
    let label = compact.strip_suffix("(元)").unwrap_or(&compact);
    matches!(
        label,
        "收入"
            | "支出"
            | "借方"
            | "贷方"
            | "借方(出账)"
            | "贷方(入账)"
            | "余额"
            | "账户余额"
            | "本次余额"
            | "交易余额"
            | "金额"
            | "交易金额"
            | "本次金额"
            | "收入金额"
            | "支出金额"
            | "收入/支出金额"
            | "转入金额"
            | "转出金额"
            | "发生额"
            | "借方发生额"
            | "贷方发生额"
            | "借方发生额(支取)"
            | "贷方发生额(收入)"
            | "借方金额"
            | "贷方金额"
            | "交易金额transactionamount"
            | "账户余额accountbalance"
            | "amount"
            | "transactionamount"
            | "credit"
            | "debit"
            | "balance"
            | "accountbalance"
    )
}

struct Sheet<'a> {
    sheet: &'a mut Worksheet,
    widths: Vec<usize>,
}

impl Sheet<'_> {
    fn cell(
        &mut self,
        r: u32,
        c: usize,
        value: &str,
        number: Option<f64>,
        style: &Format,
    ) -> Result<(), XlsxError> {
        let col = u16::try_from(c).map_err(|_| XlsxError::RowColumnLimitError)?;
        let width = if let Some(n) = number {
            self.sheet.write_number_with_format(r, col, n, style)?;
            if n == 0.0 {
                0
            } else {
                format!("{n:?}").len()
            }
        } else {
            self.sheet.write_string_with_format(r, col, value, style)?;
            value
                .chars()
                .map(|ch| if ch.is_ascii() { 1 } else { 2 })
                .sum()
        };
        if self.widths.len() <= c {
            self.widths.resize(c + 1, 0);
        }
        self.widths[c] = self.widths[c].max(width);
        Ok(())
    }
}

pub(crate) fn save(request: Export) -> Result<(), Box<dyn Error>> {
    save_reviewed(request, &HashMap::new())
}

pub(crate) fn save_reviewed(
    request: Export,
    notes: &HashMap<(usize, usize), &'static str>,
) -> Result<(), Box<dyn Error>> {
    let mut book = Workbook::new();
    let ws = book.add_worksheet();
    ws.set_name("流水明细")?;
    let mut sheet = Sheet {
        sheet: ws,
        widths: Vec::new(),
    };
    let base = Format::new()
        .set_font_name("微软雅黑")
        .set_font_size(9)
        .set_border(FormatBorder::Thin)
        .set_border_color("B8CCE4");
    let key = base
        .clone()
        .set_bold()
        .set_font_color("1F3864")
        .set_background_color("DCE6F1");
    let header = Format::new()
        .set_font_name("微软雅黑")
        .set_font_size(10)
        .set_bold()
        .set_font_color("FFFFFF")
        .set_background_color("2E75B6")
        .set_align(FormatAlign::Center)
        .set_align(FormatAlign::VerticalCenter)
        .set_border_left(FormatBorder::Thin)
        .set_border_left_color("FFFFFF")
        .set_border_right(FormatBorder::Thin)
        .set_border_right_color("FFFFFF");
    let data = base
        .clone()
        .set_align(FormatAlign::VerticalCenter)
        .set_text_wrap();
    let alt = data.clone().set_background_color("F2F7FF");
    let styles = [data, alt];
    let review_style = styles[0].clone().set_background_color("FFF2CC");
    let money_styles = styles.clone().map(|s| s.set_num_format("#,##0.00"));
    let mut r = 0;
    for pairs in &request.metadata {
        let mut c = 0;
        for (k, v) in pairs {
            if !k.is_empty() {
                sheet.cell(r, c, &format!("{k}："), None, &key)?;
                c += 1;
            }
            sheet.cell(r, c, v, None, &base)?;
            c += 1;
        }
        r += 1;
    }
    if !request.metadata.is_empty() {
        r += 1;
    }
    for (c, name) in request.columns.iter().enumerate() {
        sheet.cell(r, c, name, None, &header)?;
    }
    r += 1;
    if !request.columns.is_empty() {
        sheet.sheet.set_freeze_panes(r, 0)?;
    }
    let amounts: Vec<_> = request.columns.iter().map(|c| amount_column(c)).collect();
    for (index, row) in request.rows.iter().enumerate() {
        for (c, value) in row.iter().enumerate() {
            let number = if amounts.get(c) == Some(&true) {
                amount(value)
            } else {
                None
            };
            let style = if let Some(reason) = notes.get(&(index, c)) {
                let note = Note::new(*reason).set_author("OCR 复核");
                sheet.sheet.insert_note(r, u16::try_from(c)?, &note)?;
                &review_style
            } else if number.is_some() {
                &money_styles[index % 2]
            } else {
                &styles[index % 2]
            };
            sheet.cell(r, c, value, number, style)?;
        }
        r += 1;
    }
    // Excel 默认字体的一个列宽单位为 7 像素，匹配原导出的 OOXML 列宽。
    for (c, width) in sheet.widths.into_iter().enumerate() {
        sheet
            .sheet
            .set_column_width_pixels(c as u16, ((width + 2).clamp(8, 45) * 7) as u32)?;
    }
    book.save(request.output)?;
    Ok(())
}
