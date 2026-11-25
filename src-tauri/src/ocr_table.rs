#[path = "ocr_unruled.rs"]
mod unruled;
use crate::{
    native_chars::Glyph,
    native_fields::{sanitize, skip},
    native_statement::unruled::{by_y, KEYWORDS},
};
use regex::Regex;
use std::sync::LazyLock;

type Metadata = Vec<Vec<(String, String)>>;
type Rows = Vec<Vec<String>>;

fn lines(chars: Vec<&Glyph>, tolerance: f64) -> Vec<Vec<&Glyph>> {
    by_y(chars, |g| g.top, |g| g.x0, tolerance)
}
fn joined(line: &[&Glyph], separator: &str) -> String {
    line.iter()
        .map(|g| g.text.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}
fn mid(g: &Glyph) -> f64 {
    (g.top + g.bottom) * 0.5
}

fn cells(chars: &[&Glyph], bounds: &[(f64, f64)]) -> Vec<String> {
    static DATE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:\d{4}[-/.]\d{1,2}[-/.]\d{1,2}|\b20\d{6})$").unwrap());
    static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}:\d{2}").unwrap());
    let mut buckets = vec![Vec::new(); bounds.len()];
    for &g in chars.iter().filter(|g| !g.text.trim().is_empty()) {
        let x = (g.x0 + g.x1) * 0.5;
        let col = bounds
            .iter()
            .position(|&(a, b)| a - 2.0 <= x && x <= b + 2.0)
            .or_else(|| {
                bounds
                    .iter()
                    .map(|&(a, b)| (x - a).abs().min((x - b).abs()))
                    .enumerate()
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .filter(|v| v.1 <= 15.0)
                    .map(|v| v.0)
            });
        if let Some(i) = col {
            buckets[i].push(g);
        }
    }
    buckets
        .into_iter()
        .map(|mut chars| {
            let y = |g: &Glyph| (g.top / 4.0).round_ties_even();
            chars.sort_by(|a, b| y(a).total_cmp(&y(b)).then(a.x0.total_cmp(&b.x0)));
            let mut result = String::new();
            for line in chars.chunk_by(|a, b| y(a) == y(b)) {
                let text = joined(line, "");
                let text = text.trim();
                if DATE.is_match(&result) && TIME.is_match(text) {
                    result.push(' ');
                }
                result.push_str(text);
            }
            sanitize(&result)
        })
        .collect()
}

fn header_name(text: &str) -> String {
    let text = sanitize(text).replace(' ', "").replace("流永号", "流水号");
    if text.starts_with("借方发生额") && text.contains(['支', '取', ')']) {
        return "借方发生额(支取)".into();
    }
    if text.starts_with("贷方发生额") && text.contains(['收', '入', ')']) {
        return "贷方发生额(收入)".into();
    }
    if (text.contains("明细编号") || text.starts_with("细编号")) && text.contains("流水号")
    {
        return "账户明细编号-交易流水号".into();
    }
    text
}

pub(crate) fn english(text: &str) -> bool {
    let text = text.replace(' ', "");
    text.chars()
        .all(|c| c.is_ascii_alphabetic() || "/.'’()-".contains(c))
        && text.chars().filter(char::is_ascii_alphabetic).count() >= 3
}

fn page_header(words: &[Glyph], columns: &[String]) -> f64 {
    let groups = lines(words.iter().collect(), 6.5);
    for (i, line) in groups.iter().enumerate() {
        let text = joined(line, " ");
        if columns
            .iter()
            .filter(|n| !n.is_empty() && text.contains(n.as_str()))
            .count()
            < 3
        {
            continue;
        }
        let mut bottom = line.iter().map(|g| g.bottom).fold(0., f64::max);
        for next in &groups[i + 1..] {
            if !english(&joined(next, " ")) {
                break;
            }
            bottom = next.iter().map(|g| g.bottom).fold(bottom, f64::max);
        }
        return bottom;
    }
    0.
}

fn footer(chars: &[Glyph], cutoff: f64) -> f64 {
    static FOOTER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
        r"^说明[:：]$|^说明[:：]\s*1[.、]|^[1-5][.、]\s*本?《微信支付|仅证明[:：]在用户选择|仅用于用户申请本证明|不得用于其他用途|账户记录为准|",
        r"对于红包[，,]转账[，,]二维码|零钱的充值和提现|^重要提示[:：]|^温馨提示[:：]|^该交易明细因不可预测|^(?:打印完毕|打印完成|打印结束)[。.!！]?$"
    )).unwrap()
    });
    static DATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\d{4}(?:[-‑–]\d{1,2}[-‑–]\d{1,2}|/\d{1,2}/\d{1,2}|\.\d{1,2}\.\d{1,2}|年\d{1,2}月\d{1,2}日|(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?:\D|$))").unwrap()
    });
    let grouped = lines(chars.iter().filter(|g| g.top > cutoff).collect(), 5.);
    for (i, line) in grouped.iter().enumerate() {
        let text = joined(line, "").replace(' ', "");
        if FOOTER.is_match(&text) {
            if text.starts_with("打印")
                && grouped[i + 1..]
                    .iter()
                    .any(|next| DATE.is_match(joined(next, "").trim()))
            {
                continue;
            }
            return line.iter().map(|g| g.top).fold(f64::INFINITY, f64::min) - 1.5;
        }
    }
    f64::INFINITY
}

fn ignored(row: &[String], columns: &[String]) -> bool {
    if skip(row) {
        return true;
    }
    let text = row.concat().replace(' ', "");
    let alpha = text.chars().filter(|c| c.is_alphabetic()).count();
    let size = row.len().min(columns.len());
    if columns
        .iter()
        .zip(row)
        .filter(|(c, v)| !c.is_empty() && sanitize(v).contains(c.as_str()))
        .count()
        >= 3.max(size / 2)
    {
        return true;
    }
    if !text.chars().any(|c| c > '\u{2e7f}')
        && alpha >= 5
        && alpha * 100 >= text.chars().count() * 65
    {
        return true;
    }
    matches!(text.as_str(), "" | "具体交易明细" | "账户明细" | "交易明细")
        || [
            "交易明细涉及您的",
            "仅供个人参考",
            "AccountName",
            "AccountNo",
            "TransferAmountRange",
            "收支类别：",
            "转账金额区间：",
            "PrintTime",
            "本期总笔数",
            "期总笔数",
            "页面总笔数",
            "总收入金额",
            "总支出金额",
            "重要提示",
            "电子回单专用章",
            "打印回单或账单",
            "兹证明：",
            "居民身份证：",
            "交易明细对应时间段",
            "承上余额",
            "起止日期：",
            "起止日期:",
            "仅证明：在用户选择",
            "仅用于用户申请",
            "不得用于其他用途",
            "账户记录为准",
            "零钱的充值和提现",
            "微信支付交易明细证明》",
        ]
        .iter()
        .any(|k| text.contains(k))
}

fn wechat_columns() -> Vec<String> {
    [
        "交易单号",
        "交易时间",
        "交易类型",
        "收/支/其他",
        "交易方式",
        "金额(元)",
        "交易对方",
        "商户单号",
    ]
    .map(str::to_string)
    .to_vec()
}

#[derive(Default)]
pub(crate) struct Table {
    pub columns: Vec<String>,
    pub rows: Rows,
    pub metadata: Metadata,
    bounds: unruled::Bounds,
    start_x: Option<f64>,
    zones: bool,
    pub page_bounds: Vec<(f64, f64)>,
    pub page_rows: Vec<(usize, f64, f64)>,
    pub page_unruled: bool,
}

impl Table {
    pub fn reference_lines(&self) -> Vec<f64> {
        self.bounds
            .first()
            .map(|first| {
                std::iter::once(first.0)
                    .chain(self.bounds.iter().map(|b| b.1))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn page(
        &mut self,
        width: f64,
        height: f64,
        words: &[Glyph],
        chars: &[Glyph],
        h: &[f64],
        v: &[f64],
    ) -> bool {
        let grid_bounds: Vec<_> = v.windows(2).map(|x| (x[0], x[1])).collect();
        self.page_rows.clear();
        self.page_unruled = false;
        let first = self.columns.is_empty();
        let mut header_bottom = 0.;
        if first {
            self.zones = h.len() >= 4;
        }
        if first && v.len() >= 5 && h.len() >= 2 {
            let bounds = &grid_bounds;
            static TRAILING: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"[A-Za-z/.'’()\-]+$").unwrap());
            for band in h
                .windows(2)
                .take(6)
                .filter(|b| (8.0..=80.0).contains(&(b[1] - b[0])))
            {
                let chars = chars
                    .iter()
                    .filter(|g| mid(g) > band[0] && mid(g) <= band[1])
                    .collect::<Vec<_>>();
                let names: Vec<_> = cells(&chars, &bounds)
                    .iter()
                    .map(|s| header_name(s))
                    .collect();
                let hits = names
                    .iter()
                    .filter(|s| KEYWORDS.iter().any(|k| s.contains(k)))
                    .count();
                if hits < 3
                    || names.iter().filter(|s| !s.is_empty()).count() * 10 < bounds.len() * 6
                {
                    continue;
                }
                self.columns = names
                    .into_iter()
                    .map(|s| {
                        if s.chars().any(|ch| ch > '\u{2e7f}') {
                            let stripped = TRAILING.replace(&s, "");
                            header_name(if stripped.trim().is_empty() {
                                &s
                            } else {
                                stripped.trim()
                            })
                        } else {
                            s
                        }
                    })
                    .collect();
                if self.columns.len() == 8
                    && ["交易单号", "商户单号"]
                        .iter()
                        .all(|key| self.columns.iter().any(|s| s.contains(key)))
                {
                    self.columns = wechat_columns();
                }
                self.metadata = crate::ocr_metadata::extract(words, h, width, band[0] + 2.);
                self.bounds = grid_bounds.clone();
                header_bottom = band[1];
                break;
            }
        }
        if first && self.columns.is_empty() {
            let Some(header) = unruled::detect(words, width, height, h, v) else {
                return false;
            };
            self.columns = header.names;
            self.bounds = header.bounds;
            self.metadata = crate::ocr_metadata::extract(words, h, width, header.meta_limit);
            header_bottom = header.bottom;
        }
        let repeated = page_header(words, &self.columns);
        let cutoff = if repeated > 0. {
            repeated
        } else {
            header_bottom
        };
        let bottom = footer(chars, cutoff);
        let body: Vec<_> = chars
            .iter()
            .filter(|g| g.top > cutoff && g.top < bottom)
            .collect();
        let bounds = if grid_bounds.len() == self.columns.len() {
            grid_bounds
        } else {
            let current = unruled::start_x(&body);
            if first {
                self.start_x = current;
            }
            let shift = current
                .zip(self.start_x)
                .map(|(x, base)| x - base)
                .filter(|s| (2.0..=18.0).contains(&s.abs()))
                .unwrap_or(0.);
            self.bounds
                .iter()
                .map(|(a, b)| (a + shift, b + shift))
                .collect()
        };
        self.page_bounds.clone_from(&bounds);
        let mut zones: Vec<_> = h
            .iter()
            .copied()
            .filter(|y| *y >= (cutoff - 6.).max(0.))
            .collect();
        // 窄间距候选线可能穿过日期框，把同一笔的日期与金额切到不同区间。
        // 仅移除紧邻另一条线、且确实切过完整日期框内部的候选线。
        if let Some(date_col) = self
            .columns
            .iter()
            .position(|name| name.contains("日期") || name.contains("时间"))
        {
            if let Some(&(left, right)) = bounds.get(date_col) {
                let anchors: Vec<_> = words
                    .iter()
                    .filter(|word| {
                        let x = (word.x0 + word.x1) * 0.5;
                        x >= left
                            && x <= right
                            && crate::statement_rows::starts_with_date(&word.text)
                    })
                    .collect();
                let original = zones.clone();
                zones = original
                    .iter()
                    .copied()
                    .enumerate()
                    .filter_map(|(i, y)| {
                        let close = i > 0 && y - original[i - 1] <= 7.0
                            || original.get(i + 1).is_some_and(|next| next - y <= 7.0);
                        let cuts_date = close
                            && anchors
                                .iter()
                                .any(|w| w.top + 1.0 < y && y < w.bottom - 1.0);
                        (!cuts_date).then_some(y)
                    })
                    .collect();
            }
        }
        if !self.zones || zones.is_empty() {
            self.page_unruled = true;
            for (row, top, end) in unruled::text_records(body, &bounds, &self.columns) {
                self.page_rows.push((self.rows.len(), top, end));
                self.rows.push(row);
            }
            return true;
        }
        if (cutoff > 0. && zones[0] - cutoff > 10.) || (cutoff == 0. && v.len() < 4) {
            zones.insert(0, cutoff);
        }
        if v.is_empty() && zones.last().is_some_and(|y| *y < height - 20.) {
            zones.push(height + 1.);
        }
        for band in zones.windows(2).filter(|b| b[1] - b[0] >= 6.) {
            let raw = chars
                .iter()
                .filter(|g| g.top < bottom && mid(g) > band[0] && mid(g) <= band[1])
                .collect();
            let mut kept: Vec<_> = lines(raw, 4.)
                .into_iter()
                .filter(|line| !skip(&[joined(line, "")]))
                .flatten()
                .collect();
            if band[1] - band[0] > 55. {
                let groups = lines(kept, 5.);
                kept = vec![];
                let mut previous = f64::INFINITY;
                for group in groups {
                    if group.iter().map(|g| g.top).fold(f64::INFINITY, f64::min) - previous > 14. {
                        break;
                    }
                    previous = group.iter().map(|g| g.bottom).fold(0., f64::max);
                    kept.extend(group);
                }
            }
            let row = cells(&kept, &bounds);
            if !ignored(&row, &self.columns) {
                self.page_rows.push((self.rows.len(), band[0], band[1]));
                self.rows.push(row);
            }
        }
        true
    }
}
