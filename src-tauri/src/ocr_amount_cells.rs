use crate::{
    native_ocr_page::{OcrEngine, OcrItem},
    ocr_table::Table,
};
use image::{Rgb, RgbImage};
use regex::Regex;
use std::{error::Error, sync::LazyLock};

fn amount(text: &str) -> Option<String> {
    static VALUE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[+-]?(?:[0-9]+|[0-9]{1,3}(?:[,.][0-9]{3})+)(?:\.[0-9]{2})?$").unwrap()
    });
    let mut text = text.trim().trim_start_matches(['¥', '￥']).replace(' ', "");
    if !VALUE.is_match(&text) {
        return None;
    }
    if let Some(i) = text.rfind('.') {
        if text.len() - i == 3 {
            text = text[..i].replace('.', ",") + &text[i..];
        }
    }
    Some(text)
}

// 只允许整段连续的原金额前后带有汉字。两个数、缺位数字和正负号不能靠删字拼出。
fn watermarked_value(value: &str) -> Option<String> {
    let value = value.trim();
    let digits =
        value.trim_matches(|ch: char| matches!(ch, '\u{4e00}'..='\u{9fff}') || ch.is_whitespace());
    if digits == value {
        return None;
    }
    amount(digits)
}

pub(super) fn mark_unreadable(
    columns: &[String],
    rows: &[Vec<String>],
    notes: &mut super::field_cells::Notes,
) {
    for (col, _) in columns
        .iter()
        .enumerate()
        .filter(|(_, name)| crate::statement_excel::amount_column(name))
    {
        for (row, values) in rows.iter().enumerate() {
            let Some(raw) = values.get(col) else { continue };
            let value = raw.trim();
            if matches!(value, "" | "-" | "—" | "/" | "--") {
                continue;
            }
            if !crate::statement_excel::amount_literal(value) {
                notes.insert((row, col), "OCR 未能确认此格金额，请与原件核对后再汇总。");
            }
        }
    }
}

fn two_amounts(text: &str) -> Option<(String, String)> {
    // 连框的金额与余额常各有两位小数，中间没有空格，后面还连着中文户名。
    // 要求整段数字恰好切成两个合法金额，避免把单一流水号截成金额。
    static PAIR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"^[¥￥]?([+-]?(?:[0-9]{1,3}(?:[,.][0-9]{3})+|[0-9]+)\.[0-9]{2})\s*[¥￥]?([+-]?(?:[0-9]{1,3}(?:[,.][0-9]{3})+|[0-9]+)\.[0-9]{2})(?:[^0-9.,]|$)"
    ).unwrap()
    });
    let cap = PAIR.captures(text.trim())?;
    Some((amount(&cap[1])?, amount(&cap[2])?))
}

fn completes_balance(table: &Table, row: usize, column: usize, candidate: &str) -> bool {
    if !table.columns[column].contains("余额") {
        return false;
    }
    let current = table.rows[row][column].trim().replace(',', "");
    let proposed = candidate.replace(',', "");
    let Some((integer, tail)) = current.split_once('.') else {
        return false;
    };
    let Some((new_integer, fraction)) = proposed.split_once('.') else {
        return false;
    };
    if integer != new_integer || tail.len() != 1 || fraction.len() != 2 || !fraction.contains(tail)
    {
        return false;
    }
    let Some(debit) = table
        .columns
        .iter()
        .position(|s| s == "支出" || s.contains("借方发生额"))
    else {
        return false;
    };
    let Some(credit) = table
        .columns
        .iter()
        .position(|s| s == "收入" || s.contains("贷方发生额"))
    else {
        return false;
    };
    let cents = |s: &str| -> Option<i64> {
        let n = s.trim().replace(',', "").parse::<f64>().ok()?;
        (n.is_finite() && n.abs() < 1e13).then(|| (n * 100.).round() as i64)
    };
    let Some(balance) = cents(&proposed) else {
        return false;
    };
    let net =
        |r: &[String]| -> Option<i64> { Some(cents(r.get(credit)?)? - cents(r.get(debit)?)?) };
    // 缺一位小数时，原图复读出的两位小数还必须与相邻交易的余额和发生额逐分吻合。
    [row.checked_sub(1), row.checked_add(1)]
        .into_iter()
        .flatten()
        .filter_map(|i| table.rows.get(i))
        .any(|adjacent| {
            let Some(other) = adjacent.get(column).and_then(|s| cents(s)) else {
                return false;
            };
            net(&table.rows[row]).is_some_and(|n| other + n == balance)
                || net(adjacent).is_some_and(|n| balance + n == other)
        })
}

fn trim_ink(image: &RgbImage, bounds: [u32; 4], ink_limit: u8) -> Option<(RgbImage, u32, u32)> {
    let [x0, y0, x1, y1] = bounds;
    let (mut left, mut top, mut right, mut bottom) = (x1, y1, x0, y0);
    for y in y0..y1 {
        for x in x0..x1 {
            if image.get_pixel(x, y).0.into_iter().min()? < ink_limit {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    if right <= left + 1 || bottom <= top + 2 {
        return None;
    }
    let w = right - left + 1;
    let h = bottom - top + 1;
    // 多行附言、整根跨行表线都不属于可直接识别的一行金额。
    if h > 96 || w > h * 25 {
        return None;
    }
    let pad = (f64::from(h) * 0.12).round().max(2.) as u32;
    let mut result = RgbImage::from_pixel(w + 2 * pad, h + 2 * pad, Rgb([255; 3]));
    image::imageops::replace(
        &mut result,
        &image::imageops::crop_imm(image, left, top, w, h).to_image(),
        pad.into(),
        pad.into(),
    );
    if ink_limit < 215 {
        for pixel in result.pixels_mut() {
            if pixel.0.iter().all(|v| *v >= ink_limit) {
                *pixel = Rgb([255; 3]);
            }
        }
    }
    Some((result, left, right))
}

pub(super) fn reread(
    table: &mut Table,
    image: &RgbImage,
    engine: &mut OcrEngine,
    items: &[OcrItem],
    scale: f64,
) -> Result<(), Box<dyn Error>> {
    if table.page_unruled {
        return Ok(());
    }
    let columns: Vec<_> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            ["金额", "发生额", "余额"].iter().any(|k| name.contains(k))
                || crate::statement_excel::amount_column(name)
        })
        .map(|(i, _)| i)
        .collect();
    let boxes: Vec<_> = items
        .iter()
        .map(|item| {
            let x0 = item
                .points
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min) as f64
                * scale;
            let x1 = item.points.iter().map(|p| p[0]).fold(0., f32::max) as f64 * scale;
            let y = item.points.iter().map(|p| f64::from(p[1])).sum::<f64>() * 0.25 * scale;
            (x0, x1, y, item)
        })
        .collect();
    for &(row, y0, y1) in &table.page_rows {
        let line: Vec<_> = boxes
            .iter()
            .filter(|(_, _, y, _)| *y > y0 && *y <= y1)
            .collect();
        let mut paired = Vec::new();
        for adjacent in columns.windows(2).filter(|c| c[0] + 1 == c[1]) {
            let (i, j) = (adjacent[0], adjacent[1]);
            let Some(&(left, right)) = table.page_bounds.get(i) else {
                continue;
            };
            let Some(&(next, _)) = table.page_bounds.get(j) else {
                continue;
            };
            for &&(x0, x1, _, it) in &line {
                if it.score < 0.97 || x0 < left - 2. || x0 >= right - 2. || x1 < next + 5. {
                    continue;
                }
                if let Some((first, second)) = two_amounts(&it.text) {
                    table.rows[row][i] = first;
                    table.rows[row][j] = second;
                    paired.extend([i, j]);
                    break;
                }
            }
        }
        for &i in &columns {
            if paired.contains(&i) {
                continue;
            }
            let Some(&(left, right)) = table.page_bounds.get(i) else {
                continue;
            };
            let Some(value) = table.rows[row].get_mut(i) else {
                continue;
            };
            // 原框横跨相邻列时，按字符均摊宽度会将数位分给相邻字段。
            let related: Vec<_> = line
                .iter()
                .filter(|(x0, x1, _, _)| x1.min(right) - x0.max(left) > 2.)
                .collect();
            let solid = related.len() == 1
                && related[0].0 >= left - 2.
                && related[0].1 <= right + 2.
                && related[0].3.score >= 0.97;
            let to_x = |x: f64| (x / scale).round().clamp(0., image.width() as f64) as u32;
            let to_y = |y: f64| (y / scale).round().clamp(0., image.height() as f64) as u32;
            let margin = 1.25;
            let bounds = [
                to_x(left + margin),
                to_y(y0 + margin),
                to_x(right - margin),
                to_y(y1 - margin),
            ];
            if let Some(candidate) = watermarked_value(value) {
                // 金额格混入汉字时，用较深的原图笔画单独复读，符号和每位数字都须与整页读数一致。
                if let Some((crop, _, _)) = trim_ink(image, bounds, 150) {
                    let (text, score) = engine.read_crop(&crop)?;
                    if score >= 0.97 && amount(&text).as_ref() == Some(&candidate) {
                        *value = candidate;
                    }
                }
                continue;
            }
            let Some((crop, ink_left, ink_right)) = trim_ink(image, bounds, 215) else {
                continue;
            };
            if solid {
                let (x0, x1, _, item) = *related[0];
                if let Some(original) = amount(&item.text) {
                    *value = original;
                    // 高置信度也可能来自被裁掉首位的检测框。仅在框外还能看到数字笔画时复读。
                    if ink_left + 2 >= to_x(*x0) && ink_right <= to_x(*x1) + 2 {
                        continue;
                    }
                }
            }
            let (text, score) = engine.read_crop(&crop)?;
            if let Some(new) = amount(&text) {
                if score >= 0.97 || (score >= 0.90 && completes_balance(table, row, i, &new)) {
                    table.rows[row][i] = new;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watermark_must_leave_a_complete_signed_amount() {
        assert_eq!(
            watermarked_value("电子 -123.45 回单").as_deref(),
            Some("-123.45")
        );
        assert_eq!(watermarked_value("123电.45"), None);
        assert_eq!(watermarked_value("123.45或789.00"), None);
        assert_eq!(watermarked_value("123.45"), None);
    }

    #[test]
    fn unresolved_amount_gets_an_excel_note() {
        let columns = ["交易金额", "账户余额", "摘要"].map(str::to_owned);
        let rows = [
            ["123.45电子", "1,123.45", "电子"],
            ["—", "/103.45", "转入"],
            ["12,34.56", "1e3", "消费"],
            ["￥123.45", "1234567890123456.78", "转入"],
        ]
        .map(|row| row.map(str::to_owned).to_vec());
        let mut notes = super::super::field_cells::Notes::new();
        mark_unreadable(&columns, &rows, &mut notes);
        assert_eq!(notes.len(), 5);
        for cell in [(0, 0), (1, 1), (2, 0), (2, 1), (3, 0)] {
            assert!(notes.contains_key(&cell));
        }
    }

    #[test]
    fn partial_balance_requires_matching_reread_and_adjacent_amount() {
        let mut table = Table::default();
        table.columns = ["支出", "收入", "账户余额"].map(str::to_owned).to_vec();
        table.rows = [["0.00", "20.00", "134.59"], ["5.00", "0.00", "114.9"]]
            .map(|row| row.map(str::to_owned).to_vec())
            .to_vec();
        assert!(completes_balance(&table, 1, 2, "114.59"));
        assert!(!completes_balance(&table, 1, 2, "114.89"));
        table.rows[1][2] = "114.90".into();
        assert!(!completes_balance(&table, 1, 2, "114.59"));
    }

    #[test]
    fn splits_two_currency_values_without_moving_digits_between_columns() {
        assert_eq!(
            two_amounts("12.345.67234.56户名"),
            Some(("12,345.67".into(), "234.56".into()))
        );
        assert_eq!(two_amounts("1234567890.12"), None);
    }
}
