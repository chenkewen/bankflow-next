use crate::{
    native_chars::Glyph,
    native_fields::{self, sanitize},
    native_statement::unruled::by_y,
};
use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};

type Pairs = Vec<(String, String)>;
static LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([\u4e00-\u9fff][\u4e00-\u9fff /（）()]+)[：:](.*)$").unwrap());
static TRANSLATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z /.'’()\-]+[:：]$").unwrap());
static PAGE_COUNTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)查询结果共|此页为第|Query\s*Result\s*Total\s*Page").unwrap());

fn y_center(w: &Glyph) -> f64 {
    (w.top + w.bottom) * 0.5
}

fn word_rows(words: Vec<&Glyph>) -> Vec<Vec<&Glyph>> {
    by_y(words, |w| w.top, |w| w.x0, 5.5)
}

fn join(words: &[&Glyph]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn grid_value(label: &Glyph, segment: Vec<&Glyph>, first_value: &str) -> String {
    let mut values = Vec::new();
    if !first_value.trim().is_empty() {
        values.push(first_value.trim().to_owned());
    }
    for row in word_rows(segment) {
        let mut prefix = String::new();
        let mut skip = 0;
        for (i, w) in row.iter().enumerate() {
            if i > 0 {
                prefix.push(' ');
            }
            prefix.push_str(&w.text);
            if TRANSLATION.is_match(&prefix) {
                skip = i + 1;
            }
        }
        let candidates: Vec<_> = row
            .into_iter()
            .skip(skip)
            .filter(|w| !std::ptr::eq(*w, label))
            .collect();
        // 印在字段右端的页码不属于字段值，大间距两侧应独立判断。
        for chunk in candidates.chunk_by(|a, b| b.x0 - a.x1 <= 24.) {
            let text = join(chunk);
            if !text.is_empty()
                && !PAGE_COUNTER.is_match(&text)
                && !native_fields::skip_text(&text, true)
            {
                values.push(text);
            }
        }
    }
    sanitize(&values.join(" "))
}

fn plain_pairs(words: &[&Glyph]) -> Pairs {
    let line = join(words);
    if native_fields::skip_text(&line, false) || crate::ocr_table::english(line.trim()) {
        return vec![];
    }
    let mut tokens = line.split_whitespace().peekable();
    let mut pairs = Pairs::new();
    while let Some(first) = tokens.next() {
        let token = if first.chars().count() <= 2
            && first
                .chars()
                .all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            && tokens.peek().is_some_and(|s| s.contains([':', '：']))
        {
            format!("{first}{}", tokens.next().unwrap())
        } else {
            first.to_owned()
        };
        let separator = native_fields::meta_separators(&token).next();
        if let Some((i, n)) = separator {
            pairs.push((sanitize(&token[..i]), sanitize(&token[i + n..])));
        } else if let Some((_, value)) = pairs.last_mut() {
            *value = sanitize(&format!("{value} {token}"));
        } else {
            pairs.push((String::new(), sanitize(&token)));
        }
    }
    pairs
}

pub fn extract(words: &[Glyph], lines: &[f64], width: f64, hdr_top: f64) -> Vec<Pairs> {
    let above: Vec<_> = words.iter().filter(|w| y_center(w) < hdr_top).collect();
    let mut lines: Vec<_> = lines.iter().copied().filter(|y| *y < hdr_top).collect();
    lines.sort_by(f64::total_cmp);
    let mut used = HashSet::new();
    let mut grid = Vec::new();
    for band in lines.windows(2) {
        if band[1] - band[0] > 60. {
            continue;
        }
        let words: Vec<_> = above
            .iter()
            .copied()
            .filter(|w| band[0] < y_center(w) && y_center(w) < band[1])
            .collect();
        let mut labels: Vec<_> = words
            .iter()
            .filter_map(|w| LABEL.captures(&w.text).map(|m| (*w, m)))
            .collect();
        labels.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
        if labels.is_empty() || labels.windows(2).any(|p| p[1].0.x0 - p[0].0.x0 < 20.) {
            continue;
        }
        let mut pairs = Vec::with_capacity(labels.len());
        for (i, (label, capture)) in labels.iter().enumerate() {
            let end_x = labels.get(i + 1).map_or(width, |v| v.0.x0);
            let segment = words
                .iter()
                .copied()
                .filter(|w| w.x0 >= label.x0 - 2. && w.x0 < end_x - 2.)
                .collect();
            pairs.push((
                sanitize(&capture[1]),
                grid_value(label, segment, &capture[2]),
            ));
        }
        used.extend(words.into_iter().map(std::ptr::from_ref));
        grid.push((band[0], pairs));
    }
    let mut result = Vec::new();
    for group in word_rows(
        above
            .into_iter()
            .filter(|w| !used.contains(&std::ptr::from_ref(*w)))
            .collect(),
    ) {
        let pairs = plain_pairs(&group);
        if !pairs.is_empty() {
            result.push((group.iter().map(|w| w.top).reduce(f64::min).unwrap(), pairs));
        }
    }
    result.extend(grid);
    result.sort_by(|a, b| a.0.total_cmp(&b.0));
    result.into_iter().map(|(_, pairs)| pairs).collect()
}
