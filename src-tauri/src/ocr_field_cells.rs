use crate::{
    native_ocr_page::{OcrEngine, OcrItem},
    ocr_table::Table,
};
use image::{Rgb, RgbImage};
use std::{collections::HashMap, error::Error};

fn identity(header: &str) -> bool {
    [
        "账号",
        "账户",
        "卡号",
        "开户行",
        "银行",
        "户名",
        "交易对方",
        "对手信息",
        "流水号",
        "交易单号",
        "商户单号",
        "日志号",
        "序号",
        "交易介质编号",
    ]
    .iter()
    .any(|word| header.contains(word))
        && !["余额", "金额", "发生额"]
            .iter()
            .any(|word| header.contains(word))
}

#[derive(Default)]
pub(super) struct Reviews {
    // 分页识别后还会合并、过滤行。只把复核信息带到能唯一匹配原交易的最终单元格。
    cells: Vec<(Vec<String>, usize, &'static str)>,
}

pub(crate) type Notes = HashMap<(usize, usize), &'static str>;

// 商户单号有时用单个正斜杠表示缺省。检测器可能跳过这类窄字形。
// 仅当整格墨迹是一条从右上到左下的细直线时恢复该符号。
fn isolated_slash(image: &RgbImage, bounds: [u32; 4]) -> bool {
    let [left, top, right, bottom] = bounds;
    let mut centers = Vec::new();
    for y in top..bottom {
        let mut ink = (left..right).filter(|&x| image[(x, y)].0.iter().all(|&v| v < 172));
        if let Some(first) = ink.next() {
            let last = ink.last().unwrap_or(first);
            if last - first > 3 || centers.len() > 48 {
                return false;
            }
            centers.push((y, f64::from(first + last) / 2.));
        }
    }
    let (Some(&(y0, x0)), Some(&(y1, x1))) = (centers.first(), centers.last()) else {
        return false;
    };
    let height = y1 - y0;
    if height < 7 || centers.len() != (height + 1) as usize {
        return false;
    }
    let slope = (x0 - x1) / f64::from(height);
    (0.22..=0.85).contains(&slope)
        && centers
            .iter()
            .all(|&(y, x)| (x - x0 + slope * f64::from(y - y0)).abs() <= 1.)
}

impl Reviews {
    pub(super) fn locate(self, rows: &[Vec<String>]) -> Notes {
        let mut found = Notes::new();
        let compact = |s: &str| s.split_whitespace().collect::<String>();
        let final_rows: Vec<_> = rows
            .iter()
            .map(|r| r.iter().map(|s| compact(s)).collect::<Vec<_>>())
            .collect();
        let mut cells_by_value: HashMap<(usize, &str), Vec<usize>> = HashMap::new();
        for (idx, row) in final_rows.iter().enumerate() {
            for (col, value) in row
                .iter()
                .enumerate()
                .filter(|(_, value)| !value.is_empty())
            {
                cells_by_value
                    .entry((col, value.as_str()))
                    .or_default()
                    .push(idx);
            }
        }
        for (row, col, why) in self.cells {
            let row: Vec<_> = row.iter().map(|s| compact(s)).collect();
            let Some(candidates) = cells_by_value.get(&(col, row[col].as_str())) else {
                continue;
            };
            let mut best = 0;
            let mut unique = None;
            for &i in candidates {
                let matches = row
                    .iter()
                    .zip(&final_rows[i])
                    .enumerate()
                    .filter(|(j, (a, b))| *j != col && !a.is_empty() && a == b)
                    .count();
                if matches > best {
                    best = matches;
                    unique = Some(i);
                } else if matches == best {
                    unique = None;
                }
            }
            if let Some(i) = unique.filter(|_| best >= 2) {
                found.insert((i, col), why);
            }
        }
        found
    }
}

// 去除外侧表线，按像素中的连续文字行分段。保留每一行内全部字形及账号前导零。
pub(super) fn crops(image: &RgbImage, bounds: [u32; 4]) -> Vec<RgbImage> {
    let [x0, y0, x1, y1] = bounds;
    let mut bands: Vec<[u32; 4]> = Vec::new();
    for y in y0..y1 {
        let mut ink = (x0..x1).filter(|&x| image[(x, y)].0.into_iter().min().unwrap_or(255) < 172);
        let Some(left) = ink.next() else { continue };
        let right = ink.last().unwrap_or(left);
        if let Some(last) = bands.last_mut().filter(|b| y <= b[3] + 2) {
            last[0] = last[0].min(left);
            last[2] = last[2].max(right);
            last[3] = y;
        } else {
            bands.push([left, y, right, y]);
        }
    }
    let mut result = vec![];
    for [left, top, right, end] in bands {
        let (w, h) = (right - left + 1, end - top + 1);
        if h < 4 || w < 2 {
            continue;
        }
        if h > 72 || w > 45 * h || h > w * 3 {
            return vec![];
        }
        let pad = (h / 7).clamp(2, 5);
        let mut crop = RgbImage::from_pixel(w + 2 * pad, h + 2 * pad, Rgb([255; 3]));
        image::imageops::replace(
            &mut crop,
            &image::imageops::crop_imm(image, left, top, w, h).to_image(),
            pad.into(),
            pad.into(),
        );
        result.push(crop);
    }
    result
}

pub(super) fn reread(
    table: &mut Table,
    image: &RgbImage,
    engine: &mut OcrEngine,
    items: &[OcrItem],
    scale: f64,
    reviews: &mut Reviews,
    verified: Option<&[Vec<String>]>,
) -> Result<(), Box<dyn Error>> {
    let fields: Vec<_> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, h)| identity(h))
        .map(|(i, _)| i)
        .collect();
    let boxes: Vec<_> = items
        .iter()
        .map(|item| {
            let xmin = item
                .points
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min) as f64
                * scale;
            let xmax = item.points.iter().map(|p| p[0]).fold(0., f32::max) as f64 * scale;
            let mid = item.points.iter().map(|p| f64::from(p[1])).sum::<f64>() * 0.25 * scale;
            (xmin, xmax, mid, item)
        })
        .collect();
    let to_px = |a: f64, limit: u32| (a / scale).round().clamp(0., f64::from(limit)) as u32;
    let order_and_time = !table.page_unruled
        && matches!(table.columns.as_slice(), [id, time, ..] if id == "交易单号" && time == "交易时间");
    for (i, &(row, y0, y1)) in table.page_rows.iter().enumerate() {
        let mut notes = vec![];
        for &col in &fields {
            let Some(&(left, right)) = table.page_bounds.get(col) else {
                continue;
            };
            let Some(value) = table.rows[row].get_mut(col) else {
                continue;
            };
            if verified.is_some_and(|rows| super::source::same_field(&rows[i][col], value)) {
                continue;
            }
            let hits: Vec<_> = boxes
                .iter()
                .filter(|&&(a, b, y, _)| y >= y0 && y <= y1 && b.min(right) - a.max(left) > 2.)
                .collect();
            let weak = hits.iter().any(|b| b.3.score < 0.975);
            let crossed = hits.iter().any(|b| b.0 < left - 2. || b.1 > right + 2.);
            if !weak && !crossed && !hits.is_empty() {
                continue;
            }
            let pad = if table.page_unruled { 0. } else { 1.2 };
            let bounds = [
                to_px(left + pad, image.width()),
                to_px(y0 + pad, image.height()),
                to_px(right - pad, image.width()),
                to_px(y1 - pad, image.height()),
            ];
            if hits.is_empty() && value.is_empty() {
                if !table.page_unruled
                    && table.columns[col] == "商户单号"
                    && isolated_slash(image, bounds)
                {
                    value.push('/');
                }
                continue;
            }
            let mut text = String::new();
            let mut score = 1f64;
            for crop in crops(image, bounds) {
                let (line, conf) = engine.read_crop(&crop)?;
                text.extend(line.chars().filter(|c| !c.is_whitespace()));
                score = score.min(conf);
            }
            let original: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            if !text.is_empty() && text == original && score >= 0.96 {
                continue;
            }
            let best = hits.iter().map(|b| b.3.score).fold(0., f64::max);
            // 两个结果都支持边界内的子串，并且整页框明显越界，才去掉串入的前后缀。
            let boundary = crossed
                && !table.page_unruled
                && !original.is_empty()
                && (text.contains(&original) || original.contains(&text))
                && hits.iter().any(|b| b.3.text.contains(&text))
                && best < score;
            // 单个低分数字框裁漏了数位时，只接受明显更高分且完整保留原有数字的逐格复读。
            let digits = hits.len() == 1
                && weak
                && best + 0.025 < score
                && original.bytes().all(|b| b.is_ascii_digit())
                && text.bytes().all(|b| b.is_ascii_digit())
                && !original.is_empty()
                && text.len() > original.len()
                && (text.starts_with(&original) || text.ends_with(&original));
            if !text.is_empty() && text != original && score >= 0.985 && (boundary || digits) {
                *value = text;
            } else if !value.is_empty() && (weak || crossed || !text.is_empty() && text != original)
            {
                // 高可信完整框与现值一致，裁剪却只漏去首尾数字，采信完整框。
                if !weak
                    && crossed
                    && hits.len() == 1
                    && best >= 0.995
                    && hits[0].3.text == original
                    && !text.is_empty()
                    && text.len() < original.len()
                    && (original.starts_with(&text) || original.ends_with(&text))
                {
                    continue;
                }
                let reason = if !text.is_empty() && text != original {
                    "整页和单元格识别结果不同，请按原件核对。"
                } else if crossed {
                    "文字框跨越相邻列，请核对字段边界和完整性。"
                } else {
                    "文字识别置信度偏低，请按原件核对。"
                };
                notes.push((col, reason));
            }
        }
        if order_and_time {
            if let [id, time, ..] = table.rows[row].as_mut_slice() {
                if let Some(confirmed) =
                    order_timestamp(id, time, &boxes, y0, y1, &table.page_bounds)
                {
                    *time = confirmed;
                }
            }
        }
        reviews.cells.extend(
            notes
                .into_iter()
                .map(|(c, reason)| (table.rows[row].clone(), c, reason)),
        );
    }
    Ok(())
}

// 交易单号与日期首行经常被识别成一个跨列框。使用分列前的原框重新拼出整笔单号，
// 并与第二行独立的钟点框核对，防止后续日期修复把邻列噪声追加到已读全的单号。
fn order_timestamp(
    order: &str,
    time: &str,
    boxes: &[(f64, f64, f64, &OcrItem)],
    top: f64,
    bottom: f64,
    bounds: &[(f64, f64)],
) -> Option<String> {
    let value: String = time.split_whitespace().collect();
    if value.len() != 19 || !value.is_ascii() || !value.as_bytes()[10].is_ascii_digit() {
        return None;
    }
    let (day, clock) = (&value[..10], &value[11..]);
    let confirmed = super::timestamp::timestamp(&format!("{day} {clock}"))?;
    let &(left, border) = bounds.first()?;
    let &(_, right) = bounds.get(1)?;
    let mut hits: Vec<_> = boxes
        .iter()
        .filter(|b| b.2 >= top && b.2 <= bottom && b.0 >= left - 4. && b.0 < border - 3.)
        .collect();
    hits.sort_by(|a, b| a.2.total_cmp(&b.2));
    let mut id = String::new();
    let mut day_seen = false;
    for &(a, b, _, item) in hits {
        if item.score < 0.975 || a >= border || b > right + 3. {
            return None;
        }
        let text: String = item.text.split_whitespace().collect();
        if b > border + 3. {
            if day_seen {
                return None;
            }
            id.push_str(text.strip_suffix(day)?);
            day_seen = true;
        } else {
            id.push_str(&text);
        }
    }
    if !day_seen
        || id.is_empty()
        || !id.bytes().all(|c| c.is_ascii_digit())
        || !id.chars().eq(order.chars().filter(|c| !c.is_whitespace()))
    {
        return None;
    }
    let clock_seen = boxes.iter().any(|&(a, b, y, item)| {
        y >= top
            && y <= bottom
            && a >= border - 3.
            && b <= right + 3.
            && item.score >= 0.90
            && item.text.trim() == clock
    });
    clock_seen.then_some(confirmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn preserve_order_number_when_raw_boxes_confirm_order_day_and_time() {
        let [head, tail, clock] = ["00112026-10-09", "645", "02:35:56"].map(|s| OcrItem {
            text: s.to_owned(),
            points: [[0.; 2]; 4],
            score: 0.999,
        });
        let boxes = [
            (39., 208., 101., &head),
            (40., 79., 114., &tail),
            (159., 196., 114., &clock),
        ];
        let verify = |order, time| {
            order_timestamp(order, time, &boxes, 96., 119., &[(40., 160.), (160., 210.)])
        };
        assert_eq!(
            verify("0011645", "2026-10-093 02:35:56").as_deref(),
            Some("2026-10-09 02:35:56")
        );
        assert_eq!(verify("001164", "2026-10-093 02:35:56"), None);
        assert_eq!(verify("0011645", "2026-10-093 02:35:55"), None);
    }

    #[test]
    fn merchant_placeholder_requires_a_single_slanted_stroke() {
        let mut image = RgbImage::from_pixel(24, 24, Rgb([255; 3]));
        let bounds = [0, 0, 24, 24];
        assert!(!isolated_slash(&image, bounds));
        for y in 4..20 {
            image[(18 - y / 3, y)] = Rgb([0; 3]);
        }
        assert!(isolated_slash(&image, bounds));
        image[(2, 11)] = Rgb([0; 3]);
        assert!(!isolated_slash(&image, bounds));
    }

    #[test]
    fn reviews_follow_the_transaction_after_filtering_and_skip_ambiguous_matches() {
        let original = row(&["2026-03-15", "00123", "支取", "15.00"]);
        let previous = row(&["2026-03-14", "00123", "收入", "20.00"]);
        let notes = || Reviews {
            cells: vec![(original.clone(), 1, "核对账号")],
        };
        assert_eq!(
            notes().locate(&[previous, original.clone()]).get(&(1, 1)),
            Some(&"核对账号")
        );
        assert!(notes()
            .locate(&[original.clone(), original.clone()])
            .is_empty());
    }
}
