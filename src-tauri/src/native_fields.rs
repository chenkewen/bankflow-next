use crate::native_chars::Glyph;
use regex::{Captures, Regex};
use std::sync::LazyLock;

static SKIP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"温馨提示|本页合计|-{4,}END-{4,}|该交易明细因不可预测|以上内容仅供参考|",
        r"^\d{4}/\d{1,2}/\d{1,2}\s+\d{1,2}:\d{2}\s*$|",
        r"交易流水验真|提交系统验真|点击.*查询"
    ))
    .unwrap()
});
// 页码也可能出现在商户名和付款摘要中，只接受完整的页码字段。
static PAGE_COUNTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)^\s*[-—]?\s*(?:",
        r"第\s*\d+\s*页(?:\s*[,，/（(]?\s*共\s*\d+\s*页[）)]?)?",
        r"(?:\s*/\s*第\s*\d+\s*笔\s*[,，]\s*共\s*\d+\s*笔(?:\s*打印时间\s*[:：].*)?)?|",
        r"共\s*\d+\s*页\s*[,，]?\s*第\s*\d+\s*页|",
        r"Page\s*\d+(?:\s*(?:of|/)\s*\d+)?|",
        r"(?:…|(?:(?:交易区间|打印时间)\s*[:：].*)?页[数码次]\s*[:：]?)?\s*\d+\s*/\s*\d+",
        r")\s*[-—]?\s*$"
    ))
    .unwrap()
});
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://|www\.\w+").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(" {2,}").unwrap());
static WIDE_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{3,}").unwrap());
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{4})\s*[-‑–]\s*(\d{1,2})\s*[-‑–]\s*(\d{1,2})").unwrap());
static NULL_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{2}-\d{2} \d{1,2})\x00(\d{2})\x00(\d{2})").unwrap());
static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").unwrap());

fn digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && DIGIT.is_match(c.encode_utf8(&mut [0; 4])))
}

fn time_prefix(chars: &[char]) -> bool {
    chars.len() >= 8
        && [0, 1, 3, 4, 6, 7].into_iter().all(|i| digit(chars[i]))
        && [2, 5]
            .into_iter()
            .all(|i| matches!(chars[i], ':' | '：' | '∶' | '\0'))
}

pub fn sanitize(raw: &str) -> String {
    let chars: Vec<_> = raw.chars().collect();
    let mut s = String::with_capacity(raw.len());
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '\r' | '\n') {
            let start = i;
            while i < chars.len() && matches!(chars[i], '\r' | '\n') {
                i += 1;
            }
            if start > 0 && digit(chars[start - 1]) && time_prefix(&chars[i..]) {
                s.push(' ');
            }
        } else {
            s.push(if chars[i] == '∶' { ':' } else { chars[i] });
            i += 1;
        }
    }
    let s = SPACES.replace_all(s.trim(), " ");
    if s.chars().count() >= 3
        && s.chars()
            .all(|c| matches!(c, '-' | '—' | '=') || c.is_whitespace())
    {
        return String::new();
    }
    let s = NULL_TIME.replace_all(&s, "$1:$2:$3");
    let cs: Vec<_> = s.chars().collect();
    let mut repaired = String::with_capacity(s.len());
    for (i, &c) in cs.iter().enumerate() {
        if (c == '\0' || c == '：')
            && i > 0
            && digit(cs[i - 1])
            && cs.get(i + 1).is_some_and(|c| digit(*c))
            && cs.get(i + 2).is_some_and(|c| digit(*c))
            && (c == '：' || cs.get(i + 3) == Some(&':'))
        {
            repaired.push(':');
        } else {
            repaired.push(c);
        }
    }
    // 空格只在形如“数字 : 两位数字”的上下文清除，普通描述中的冒号保留。
    let cs: Vec<_> = repaired.chars().collect();
    let mut cleaned = String::with_capacity(repaired.len());
    let mut i = 0;
    while i < cs.len() {
        if digit(cs[i]) {
            let mut colon = i + 1;
            while colon < cs.len() && cs[colon].is_whitespace() {
                colon += 1;
            }
            if cs.get(colon) == Some(&':') {
                let mut next = colon + 1;
                while next < cs.len() && cs[next].is_whitespace() {
                    next += 1;
                }
                if next + 1 < cs.len() && digit(cs[next]) && digit(cs[next + 1]) {
                    cleaned.push(cs[i]);
                    cleaned.push(':');
                    i = next;
                    continue;
                }
            }
        }
        if !matches!(cs[i], '\x00'..='\x08'|'\x0b'|'\x0c'|'\x0e'..='\x1f') {
            cleaned.push(cs[i]);
        }
        i += 1;
    }
    DATE.replace_all(cleaned.trim(), |m: &Captures<'_>| {
        format!("{}-{:0>2}-{:0>2}", &m[1], &m[2], &m[3])
    })
    .into_owned()
}

pub(crate) fn skip_text(text: &str, skip_print_labels: bool) -> bool {
    URL.is_match(text) || PAGE_COUNTER.is_match(text) || skip_non_url_text(text, skip_print_labels)
}

fn skip_non_url_text(text: &str, skip_print_labels: bool) -> bool {
    SKIP.is_match(text)
        || (skip_print_labels && (text.contains("本次查询时间") || text.contains("打印日期")))
}

pub fn skip(row: &[String]) -> bool {
    let text = row
        .iter()
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if text.trim().is_empty()
        || skip_non_url_text(&text, true)
        || ((URL.is_match(&text)
            || PAGE_COUNTER.is_match(&text)
            || row.iter().any(|cell| PAGE_COUNTER.is_match(cell)))
            && !crate::native_statement::transaction_row(row))
    {
        return true;
    }
    if text.trim_start().starts_with("收入合计")
        && text
            .split_whitespace()
            .collect::<String>()
            .contains("支出合计")
    {
        return true;
    }
    if text.contains("本对账单") || text.contains("承上余额") || text.contains("转下余额")
    {
        let compact: String = text.split_whitespace().collect();
        return compact == "本对账单仅供查询使用，不能作为资产证明"
            || ["承上余额", "转下余额"].iter().any(|label| {
                compact
                    .strip_prefix(label)
                    .is_some_and(crate::native_statement::amount_cell)
            });
    }
    false
}

pub fn clean_header(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        if c != ' ' || previous <= '\u{2e7f}' || !chars.peek().is_some_and(|c| *c > '\u{2e7f}') {
            output.push(c);
        }
        previous = c;
    }
    output
}

pub(crate) struct Word {
    pub text: String,
    pub x: f64,
    pub top: f64,
    pub bottom: f64,
}

pub(crate) fn words<'a>(chars: impl IntoIterator<Item = &'a Glyph>, cutoff: f64) -> Vec<Word> {
    let mut glyphs: Vec<_> = chars.into_iter().filter(|g| g.top < cutoff + 3.).collect();
    glyphs.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut words = Vec::<Word>::new();
    let mut start = 0;
    while start < glyphs.len() {
        // pdfplumber 按相邻 Y 坐标聚类，再按 X 坐标生成词。
        let mut end = start + 1;
        while end < glyphs.len() && glyphs[end].top - glyphs[end - 1].top <= 3. {
            end += 1;
        }
        let line = &mut glyphs[start..end];
        line.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut prev: Option<&Glyph> = None;
        for &g in line.iter() {
            if !g.text.is_empty() && g.text.chars().all(char::is_whitespace) {
                prev = None;
                continue;
            }
            if prev.is_none_or(|p| g.x0 > p.x1 + 3. || (g.top - p.top).abs() > 3.) {
                words.push(Word {
                    text: String::new(),
                    x: g.x0,
                    top: g.top,
                    bottom: g.bottom,
                });
            }
            let word = words.last_mut().unwrap();
            word.top = word.top.min(g.top);
            word.bottom = word.bottom.max(g.bottom);
            word.text.push_str(match g.text.as_str() {
                "ﬀ" => "ff",
                "ﬃ" => "ffi",
                "ﬄ" => "ffl",
                "ﬁ" => "fi",
                "ﬂ" => "fl",
                "ﬅ" | "ﬆ" => "st",
                s => s,
            });
            prev = Some(g);
        }
        start = end;
    }
    words.retain(|w| w.top < cutoff);
    words
}

pub(crate) fn meta_separators(line: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    line.char_indices().filter_map(|(i, c)| {
        if !matches!(c, ':' | '：') {
            return None;
        }
        let after = &line[i + c.len_utf8()..];
        let digit_before = line[..i].chars().next_back().is_some_and(digit);
        (!after.starts_with("//") && !(digit_before && after.chars().next().is_some_and(digit)))
            .then_some((i, c.len_utf8()))
    })
}

fn pairs(line: &str) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, n) in meta_separators(line) {
        parts.push(line[start..i].trim());
        start = i + n;
    }
    parts.push(line[start..].trim());
    if parts.len() == 1 {
        return WIDE_SPACE
            .split(line)
            .map(|s| (String::new(), sanitize(s)))
            .collect();
    }
    let mut out = Vec::new();
    let mut key = parts[0].to_owned();
    for part in &parts[1..parts.len() - 1] {
        let tokens: Vec<_> = part.split_whitespace().collect();
        if tokens.is_empty() {
            out.push((sanitize(&key), String::new()));
            continue;
        }
        let end = tokens
            .iter()
            .rposition(|t| {
                let mut chars = t.chars();
                !chars.next().is_some_and(|c| c > '\u{2e7f}') || chars.next().is_some()
            })
            .map_or(0, |i| i + 1)
            .min(tokens.len() - 1);
        out.push((sanitize(&key), sanitize(&tokens[..end].join(" "))));
        key = tokens[end..].concat();
    }
    out.push((sanitize(&key), sanitize(parts.last().unwrap())));
    out
}

pub fn metadata(chars: &[Glyph], cutoff: f64) -> Vec<Vec<(String, String)>> {
    let mut words = words(chars, cutoff);
    words.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut start = 0;
    let mut out = Vec::new();
    while start < words.len() {
        let mut end = start + 1;
        while end < words.len() && words[end].top - words[start].top <= 5. {
            end += 1;
        }
        let line = &mut words[start..end];
        line.sort_by(|a, b| a.x.total_cmp(&b.x));
        let text = line
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let text = text.trim();
        if !text.is_empty()
            && !skip_text(text, true)
            && !text.chars().all(|c| c == ' ' || c.is_ascii_alphabetic())
        {
            out.push(pairs(text));
        }
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn damaged_colons_dates_and_metadata_keys() {
        assert_eq!(sanitize("2024-1-2\n10：15∶34"), "2024-01-02 10:15:34");
        assert_eq!(
            sanitize("2024-01-02 9\0\x31\x35\0\x33\x34"),
            "2024-01-02 9:15:34"
        );
        assert_eq!(sanitize("18 : 15 : 00"), "18:15:00");
        assert_eq!(clean_header("交 易 日期 Date"), "交易日期 Date");
        assert_eq!(
            pairs("日期：2024-01-02 10:15:30 开 户 行：A"),
            vec![
                ("日期".into(), "2024-01-02 10:15:30".into()),
                ("开户行".into(), "A".into())
            ]
        );
    }
    #[test]
    fn transaction_can_contain_a_merchant_url() {
        let row = ["2026-01-02", "-23.45", "https://www.example.org/pay"].map(str::to_owned);
        assert!(!skip(&row));
        assert!(skip(&[row[2].clone()]));
    }
    #[test]
    fn page_labels_are_distinct_from_transaction_descriptions() {
        for memo in ["合同第3页打印费", "Page 1 Books", "第 3 页", "2/12"] {
            let row = ["2026-01-03", "20.00", memo].map(str::to_owned);
            assert!(!skip(&row), "{memo}");
            assert!(!skip_text(&row.concat(), false), "{memo}");
        }
        for footer in [
            "- 第 1 页 / 共 15 页 -",
            "第 1 页（共 15 页）",
            "共15页 第 1 页",
            "第1页，共3页/第1笔，共60笔 打印时间: 2026-01-03",
            "交易区间：2026-01-01 至 2026-01-03 客户姓名：示例 页数: 1 / 2",
            "Page 1 of 2",
            "Page 1/2",
        ] {
            assert!(skip_text(footer, false), "{footer}");
            assert!(skip(&[footer.to_owned()]), "{footer}");
        }
    }
    #[test]
    fn skip_repeated_footers_and_preserve_digit_colons() {
        assert!(skip(&["第 3 页".into(), "银行".into()]));
        assert!(skip(&["打印时间：2026年".into(), "页数: 1 / 12".into()]));
        assert!(skip(&["2/12".into()]));
        assert!(!skip(&[
            "收款".into(),
            "10.00".into(),
            "/".into(),
            "12345".into()
        ]));
        assert!(!skip(&["2026-11-24 15:22:51".into(), "转账".into()]));
        assert_eq!(
            pairs("时间：12:34:56 账户：00012"),
            vec![
                ("时间".into(), "12:34:56".into()),
                ("账户".into(), "00012".into())
            ]
        );
    }
}
