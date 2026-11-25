use crate::{native_chars::Glyph, native_ocr_page::OcrItem};
use regex::Regex;
use std::sync::LazyLock;

pub static OCR_HEADERS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    let extra = "转入金额 转出金额 收入金额 支出金额 借方发生额 贷方发生额
收入/支出金额 借贷标志 储种 钞汇 地区 对方单位 子账号 交易余额 外部系统流水 交易单号 商户单号
交易对方 交易方式 金额(元) 对方银行 对方开户机构 交易地点 收支方向 凭证代号";
    let mut headers: Vec<_> = crate::native_statement::unruled::KEYWORDS
        .iter()
        .copied()
        .chain(extra.split_whitespace())
        .collect();
    headers.sort_by_key(|h| std::cmp::Reverse(h.chars().count()));
    headers
});

static ORDER_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{12,})\s*(20\d{2}[-/.]\d{1,2}[-/.]\d{1,2}.*)$").unwrap());
static DATE_CN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(20\d{2}[-/.]\d{1,2}[-/.]\d{1,2})\s*([\u4e00-\u9fff].*)$").unwrap()
});
static CROSS_CN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([+-]?[0-9A-Za-z.,\-:]{4,})\s*([\u4e00-\u9fff（(）)].*)$").unwrap()
});
static RIGHT_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?[^\d\s])\s*(\d{7,}[A-Za-z0-9_\-]*)$").unwrap());
static AMOUNTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([+-]?[¥￥$]?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2})\s*([+-]?[¥￥$]?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2})$").unwrap()
});
static DATETIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}[-/.]?\d{1,2}[-/.]?\d{1,2}\s+\d{1,2}:").unwrap());
static ATOMS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
    r"\d{4}[-‑–/.年]\d{1,2}[-‑–/.月]\d{1,2}日?|\d{4}[-‑–/.]\d{1,2}[-‑–/.]",
    r"|\d{1,2}:\d{2}(?::\d{2})?|[+-]?[¥￥$]?(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2}|\d+|[A-Za-z0-9_]+|[^\s]"
)).unwrap()
});
static NUMERIC_ATOM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9A-Za-z.,:;/+-¥￥$年月日]+$").unwrap());
// 与旧解析器的 isdigit 一致，包括圈号和上下标数字，排除分数、汉字数词。
static DIGIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"[\d²³¹፩-፱᧚⁰⁴-⁹₀-₉①-⑨⑴-⑼⒈-⒐⓪⓵-⓽⓿❶-❾➀-➈➊-➒",
        r"\x{10a40}-\x{10a43}\x{10e60}-\x{10e68}\x{11052}-\x{1105a}\x{1f100}-\x{1f10a}]"
    ))
    .unwrap()
});

fn estimate_skew(items: &[OcrItem], width: f64) -> f64 {
    let mut angles = Vec::new();
    let mut centers = Vec::with_capacity(items.len());
    for item in items {
        let [p0, p1, p2, p3] = item.points.map(|p| p.map(f64::from));
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        let h = (p3[1] - p0[1]).abs();
        let angle = dy.atan2(dx);
        if dx >= (width * 0.04).max(36.) && dx > h * 1.8 && angle.abs() <= 8_f64.to_radians() {
            angles.push(angle);
        }
        centers.push(((p0[0] + p2[0]) * 0.5, (p0[1] + p2[1]) * 0.5, h.max(8.)));
    }
    // 单元格框过短时，同一行内的远端单元格仍可提供稳定的倾角。
    if angles.len() < 5 && centers.len() >= 4 {
        centers.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (i, &(x1, y1, h1)) in centers.iter().enumerate() {
            for &(x2, y2, h2) in &centers[i + 1..] {
                let dx = x2 - x1;
                let dy = y2 - y1;
                if dx >= width * 0.25
                    && dy.abs() <= ((h1 + h2) * 0.5 * 0.9).min(dx * 5_f64.to_radians().tan())
                {
                    angles.push(dy.atan2(dx));
                }
            }
        }
    }
    if angles.len() < 3 {
        return 0.;
    }
    angles.sort_by(f64::total_cmp);
    let angle = angles[angles.len() / 2];
    if angle.abs() < 0.12_f64.to_radians() {
        0.
    } else {
        angle
    }
}

fn visual_weight(c: char) -> f64 {
    match c {
        ' ' => 0.5,
        '.' | ',' | ':' | ';' | '|' | '\'' | '!' | 'I' | '1' | 'l' | '-' => 0.65,
        c if u32::from(c) > 0x2e7f => 1.85,
        _ => 1.,
    }
}

fn header_parts(text: &str) -> Option<Vec<&str>> {
    let count = text.chars().count();
    if count < 4 || text.contains([' ', '/', '(', ')', '（', '）']) || DIGIT.is_match(text) {
        return None;
    }
    let mut offsets = Vec::new();
    let mut matched = 0;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if let Some(&kw) = OCR_HEADERS.iter().find(|kw| rest.starts_with(**kw)) {
            offsets.push(at);
            matched += kw.chars().count();
            at += kw.len();
        } else {
            if offsets.is_empty() {
                offsets.push(0);
            }
            at += rest.chars().next().unwrap().len_utf8();
        }
    }
    if offsets.len() < 2 || (matched as f64) < count as f64 * 0.85 {
        return None;
    }
    offsets.push(text.len());
    Some(offsets.windows(2).map(|p| &text[p[0]..p[1]]).collect())
}

fn split_words(
    text: &str,
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
    lines: &[f64],
    words: &mut Vec<Glyph>,
) {
    let mut add = |t: &str, left: f64, right: f64| {
        words.push(Glyph {
            text: t.to_owned(),
            x0: left,
            x1: right,
            top,
            bottom,
        })
    };
    let crossing: Vec<_> = lines
        .iter()
        .copied()
        .filter(|x| x0 + 3. < *x && *x < x1 - 3.)
        .collect();
    if let Some(&line) = crossing.first() {
        if let Some(parts) = ORDER_DATE.captures(text) {
            add(parts[1].trim(), x0 + 1., line - 1.);
            let right = parts[2].trim();
            if let (Some(&second), Some(date)) = (crossing.get(1), DATE_CN.captures(right)) {
                add(&date[1], line + 1., second - 1.);
                add(&date[2], second + 1., x1 - 1.);
            } else {
                add(right, line + 1., x1 - 1.);
            }
            return;
        }
        if crossing.len() == 1 {
            for rule in [&*CROSS_CN, &*RIGHT_NUMBER, &*AMOUNTS] {
                if let Some(parts) = rule.captures(text) {
                    add(parts[1].trim(), x0 + 1., line - 1.);
                    add(parts[2].trim(), line + 1., x1 - 1.);
                    return;
                }
            }
        }
    }
    let headers = header_parts(text);
    let gap = if headers.is_some() { 0.15 } else { 0.8 };
    let parts = headers.unwrap_or_else(|| text.split_whitespace().collect());
    if parts.len() <= 1 {
        add(text, x0, x1);
        return;
    }
    if DATETIME.is_match(text) {
        let pad = (x1 - x0) * 0.12;
        add(text, x0 + pad, x1 - pad);
        return;
    }
    let span = (x1 - x0).max(1.);
    let weights: Vec<f64> = parts
        .iter()
        .map(|s| s.chars().map(visual_weight).sum())
        .collect();
    let total = (weights.iter().sum::<f64>() + gap * (parts.len() - 1) as f64).max(1.);
    let mut x = x0;
    for (s, w) in parts.into_iter().zip(weights) {
        let length = span * (w / total);
        add(s, x, x + length);
        x += length + span * (gap / total);
    }
}

fn add_chars(word: &Glyph, lines: &[f64], chars: &mut Vec<Glyph>) {
    let text = word.text.trim();
    let crossing = lines.iter().any(|x| word.x0 + 4. < *x && *x < word.x1 - 4.);
    let pad = if crossing {
        0.
    } else {
        (word.x1 - word.x0) * 0.08
    };
    let mut x = word.x0 + pad;
    let span = (word.x1 - pad - x).max(1.);
    let total: f64 = text.chars().map(visual_weight).sum();
    let mut add = |c: char, left: f64, right: f64| {
        chars.push(Glyph {
            text: c.to_string(),
            x0: left,
            x1: right,
            top: word.top,
            bottom: word.bottom,
        })
    };
    let mut spans = Vec::new();
    for (offset, c) in text.char_indices() {
        let w = span * (visual_weight(c) / total);
        if crossing {
            if !c.is_whitespace() {
                add(c, x, x + w);
            }
        } else {
            spans.push((offset, c, x, x + w));
        }
        x += w;
    }
    if crossing {
        return;
    }
    let mut idx = 0;
    for token in ATOMS.find_iter(text) {
        while spans[idx].0 < token.start() {
            idx += 1;
        }
        let start = idx;
        while idx < spans.len() && spans[idx].0 < token.end() {
            idx += 1;
        }
        let group = &spans[start..idx];
        let left = group[0].2;
        let right = group.last().unwrap().3;
        if group.len() > 1 && right - left < 65. && NUMERIC_ATOM.is_match(token.as_str()) {
            let n = group.len() as f64;
            let step = ((right - left) / n).min(0.4);
            let x0 = (left + right) * 0.5 - step * n * 0.5;
            for (i, &(_, c, _, _)) in group.iter().enumerate() {
                let x = x0 + i as f64 * step;
                add(c, x, x + step * 0.9);
            }
        } else {
            for &(_, c, x0, x1) in group {
                add(c, x0, x1);
            }
        }
    }
}

pub fn convert(
    items: &[OcrItem],
    width: f64,
    height: f64,
    scale: f64,
    allow_deskew: bool,
    vlines: &[f64],
) -> (Vec<Glyph>, Vec<Glyph>) {
    let angle = if allow_deskew {
        estimate_skew(items, width)
    } else {
        0.
    };
    let (sin, cos) = (-angle).sin_cos();
    let (cx, cy) = (width * 0.5, height * 0.5);
    let mut words = Vec::with_capacity(items.len());
    let mut chars = Vec::new();
    for item in items {
        let text = item.text.trim();
        if text.is_empty()
            || (text.chars().count() == 1
                && item.score < 0.85
                && !DIGIT.is_match(text)
                && !matches!(text, "支" | "收" | "借" | "贷"))
        {
            continue;
        }
        let points = item.points.map(|p| {
            let [x, y] = p.map(f64::from);
            if angle == 0. {
                [x * scale, y * scale]
            } else {
                [
                    ((x - cx) * cos - (y - cy) * sin + cx) * scale,
                    ((x - cx) * sin + (y - cy) * cos + cy) * scale,
                ]
            }
        });
        let bound =
            |i: usize, f: fn(f64, f64) -> f64| points.iter().map(|p| p[i]).reduce(f).unwrap();
        let start = words.len();
        split_words(
            text,
            bound(0, f64::min),
            bound(0, f64::max),
            bound(1, f64::min),
            bound(1, f64::max),
            vlines,
            &mut words,
        );
        for w in &words[start..] {
            add_chars(w, vlines, &mut chars);
        }
    }
    (words, chars)
}
