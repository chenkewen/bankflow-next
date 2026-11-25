use super::{field_cells::crops, timestamp::timestamp};
use crate::{native_ocr_page::OcrEngine, ocr_table::Table};
use image::RgbImage;
use std::error::Error;

// 日期末位挤到交易类型的开头时，先确认原格复读的日期和同一笔显示的时分秒。
// 对仅有一位或一个汉字串列的情况，保留后续通用字段归位逻辑。
fn apply(row: &mut [String], col: usize, date: &str, clock: &str) {
    let Some(verified) = timestamp(&format!("{date} {clock}")) else {
        return;
    };
    if date.len() != 10 || clock.len() != 8 || !row[col].ends_with(clock) {
        return;
    }
    let value = &row[col];
    let raw_day = value[..value.len() - clock.len()].trim();
    if raw_day == date {
        return;
    }
    let kind = &row[col + 1];
    let spill = raw_day.len() == 10
        && raw_day.get(..8) == date.get(..8)
        && raw_day.as_bytes()[8] == b'0'
        && raw_day.as_bytes()[9] == date.as_bytes()[8]
        && kind.as_bytes().first() == date.as_bytes().last()
        && kind
            .get(1..)
            .and_then(|s| s.chars().next())
            .is_some_and(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    if spill {
        row[col + 1].remove(0);
    } else if !raw_day.is_empty()
        && !raw_day
            .strip_prefix(date)
            .is_some_and(|s| s.len() >= 2 && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return;
    }
    row[col] = verified;
}

pub(super) fn reread(
    table: &mut Table,
    image: &RgbImage,
    engine: &mut OcrEngine,
    scale: f64,
) -> Result<(), Box<dyn Error>> {
    let Some(col) = table.columns.iter().position(|s| s == "交易时间") else {
        return Ok(());
    };
    if col == 0
        || table.columns[col - 1] != "交易单号"
        || table.columns.get(col + 1).is_none_or(|s| s != "交易类型")
        || !table.columns.iter().any(|s| s == "商户单号")
    {
        return Ok(());
    }
    let Some(&(left, right)) = table.page_bounds.get(col) else {
        return Ok(());
    };
    let px = |v: f64, n: u32| (v / scale).round().clamp(0., f64::from(n)) as u32;
    let pad = if table.page_unruled { 0. } else { 1. };
    for &(i, y0, y1) in &table.page_rows {
        let row = &mut table.rows[i];
        if row.len() <= col + 1 || !row[col].contains(':') {
            continue;
        }
        if !row[col + 1].starts_with(|c: char| c.is_ascii_digit()) && timestamp(&row[col]).is_some()
        {
            continue;
        }
        let bounds = [
            px(left + pad, image.width()),
            px(y0 + pad, image.height()),
            px(right - pad, image.width()),
            px(y1 - pad, image.height()),
        ];
        let bands = crops(image, bounds);
        let [day, clock] = bands.as_slice() else {
            continue;
        };
        let (date, score) = engine.read_crop(day)?;
        if score < 0.98 {
            continue;
        }
        let (time, confidence) = engine.read_crop(clock)?;
        if confidence >= 0.90 {
            apply(row, col, date.trim(), time.trim());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_confirmed_day_digit_and_matching_clock() {
        let row = ["2025-04-02 12:25:40", "3扫码付款"].map(str::to_owned);
        let mut changed = row.clone();
        apply(&mut changed, 0, "2025-04-23", "12:25:40");
        assert_eq!(changed, ["2025-04-23 12:25:40", "扫码付款"]);
        let mut kept = row.clone();
        apply(&mut kept, 0, "2025-04-23", "12:25:41");
        apply(&mut kept, 0, "2025-04-24", "12:25:40");
        assert_eq!(kept, row);
        let mut mixed = ["2025-04-231212:25:40", "微信红包"].map(str::to_owned);
        apply(&mut mixed, 0, "2025-04-23", "12:25:40");
        assert_eq!(mixed, ["2025-04-23 12:25:40", "微信红包"]);
    }
}
