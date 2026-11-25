use crate::{native_ocr_page::OcrEngine, ocr_table::Table};
use image::RgbImage;
use regex::Regex;
use std::{error::Error, sync::LazyLock};

pub(super) fn timestamp(s: &str) -> Option<String> {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"^(20\d\d)[-/](0[1-9]|1[0-2])[-/]([0-2]\d|3[01])\s*([01]\d|2[0-3]):([0-5]\d):([0-5]\d)$"
    ).unwrap()
    });
    let c = PATTERN.captures(s.trim())?;
    let month = c[2].parse::<u32>().ok()?;
    let year = c[1].parse::<u32>().ok()?;
    let limit = match month {
        2 if year % 4 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=limit).contains(&c[3].parse::<u32>().ok()?) {
        return None;
    }
    Some(format!(
        "{}-{}-{} {}:{}:{}",
        &c[1], &c[2], &c[3], &c[4], &c[5], &c[6]
    ))
}

pub(super) fn reread(
    table: &mut Table,
    image: &RgbImage,
    engine: &mut OcrEngine,
    scale: f64,
) -> Result<(), Box<dyn Error>> {
    let Some(col) = table.columns.iter().position(|h| h.contains("交易时间")) else {
        return Ok(());
    };
    let Some(book) = table.columns.iter().position(|h| h.contains("记账日期")) else {
        return Ok(());
    };
    let Some(&(left, right)) = table.page_bounds.get(col) else {
        return Ok(());
    };
    for &(idx, top, bottom) in &table.page_rows {
        let Some(value) = table.rows[idx].get_mut(col) else {
            continue;
        };
        if !value.contains(':') || timestamp(value).is_some() {
            continue;
        }
        let px = |pt: f64, max: u32| (pt / scale).round().clamp(0., max as f64) as u32;
        let pad = if table.page_unruled { 0. } else { 1. };
        let bounds = [
            px(left + pad, image.width()),
            px(top + pad, image.height()),
            px(right - pad, image.width()),
            px(bottom - pad, image.height()),
        ];
        let mut text = String::new();
        let mut score = 1.0f64;
        let crops = super::field_cells::crops(image, bounds);
        for crop in &crops {
            let (s, confidence) = engine.read_crop(crop)?;
            text.push_str(s.trim());
            score = score.min(confidence);
        }
        // 黑白阈值复读只用于两次均读出非法时刻的单行，保留已验证的日期与分钟、秒。
        if text == value.trim()
            && timestamp(&text).is_none()
            && text.len() >= 18
            && crops.len() == 1
        {
            for threshold in [180, 130, 210] {
                let mut bw = crops[0].clone();
                for pixel in bw.pixels_mut() {
                    let v = if pixel.0.iter().copied().min().unwrap_or(255) < threshold {
                        0
                    } else {
                        255
                    };
                    *pixel = image::Rgb([v; 3]);
                }
                let (alternate, conf) = engine.read_crop(&bw)?;
                let before = value.replace(' ', "");
                let after = alternate.replace(' ', "");
                if conf >= 0.97
                    && timestamp(&alternate).is_some()
                    && before.is_ascii()
                    && after.is_ascii()
                    && before.len() == after.len()
                    && before.len() == 18
                    && before[..10] == after[..10]
                    && before[12..] == after[12..]
                {
                    text = alternate;
                    score = conf;
                    break;
                }
            }
        }
        // 日期和时间之间的单个杂点仅在记账日期能独立印证当天时剔除。
        let clean = if let Some((date, clock)) = text.split_once(['<', '>']) {
            let same_day = table.rows[idx]
                .get(book)
                .is_some_and(|v| date.replace('-', "") == v.trim());
            if !same_day {
                continue;
            }
            format!("{date} {clock}")
        } else {
            text
        };
        if score >= 0.92 {
            if let Some(candidate) = timestamp(&clean) {
                table.rows[idx][col] = candidate;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_complete_calendar_dates_and_clocks() {
        assert_eq!(
            timestamp("2024-02-2912:30:50").as_deref(),
            Some("2024-02-29 12:30:50")
        );
        for raw in [
            "2025-02-29 12:30:50",
            "2025-02-28 42:30:50",
            "2025-02-28 12:30:506",
            "2025-02-28 12:30:50ER",
        ] {
            assert!(timestamp(raw).is_none());
        }
    }
}
