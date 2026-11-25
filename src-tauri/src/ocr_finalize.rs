use super::{clean_amount, contains_any, number, AMOUNT};
use crate::{native_fields, statement_rows};
use regex::Regex;
use std::sync::LazyLock;

pub type Metadata = Vec<Vec<(String, String)>>;
macro_rules! rx {
    ($pattern:literal) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
        &*RE
    }};
}
fn date_cols(columns: &[String]) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .filter(|(_, c)| statement_rows::date_header(c))
        .map(|(i, _)| i)
        .collect()
}

fn repair_repeated_separator(value: &str) -> Option<String> {
    if !["--", "//", ".."].iter().any(|sep| value.contains(sep)) {
        return None;
    }
    // 范围、残缺日期以及额外文字都可能具有业务含义，只修正一个完整日期内部的重复符号。
    let c = rx!(r"^(20[0-9]{2})([-/.]{1,2})([0-9]{2})([-/.]{1,2})([0-9]{2})(?:\s*(([01][0-9]|2[0-3]):[0-5][0-9](?::[0-5][0-9])?))?$")
        .captures(value.trim())?;
    let sep = c[2].as_bytes()[0];
    if !c[2].bytes().chain(c[4].bytes()).all(|b| b == sep) {
        return None;
    }
    rust_xlsxwriter::ExcelDateTime::from_ymd(
        c[1].parse().ok()?,
        c[3].parse().ok()?,
        c[5].parse().ok()?,
    )
    .ok()?;
    let mut date = format!("{}-{}-{}", &c[1], &c[3], &c[5]);
    if let Some(clock) = c.get(6) {
        date.push(' ');
        date.push_str(clock.as_str());
    }
    Some(date)
}

fn fix_date(row: &mut [String], ci: usize, date_col: bool) {
    let mut value = row[ci].trim().to_owned();
    let short_year = rx!(
        r"^(?:-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])|0?2\d-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01]))"
    );
    if ci > 0 && short_year.is_match(&value) {
        let previous = row[ci - 1].trim();
        if value.starts_with('-') {
            if let Some(year) = rx!(r"20\d{2}$").find(previous) {
                value.insert_str(0, year.as_str());
                row[ci - 1] = previous[..year.start()].trim().into();
            }
        } else {
            let prefix = if value.starts_with("02") { "2" } else { "20" };
            value.insert_str(0, prefix);
            if let Some(trimmed) = previous.strip_suffix(prefix) {
                row[ci - 1] = trimmed.trim().into();
            }
        }
    }
    if let Some(m) = rx!(r"[\u4e00-\u9fff]+").find(&value) {
        let leaked = m.as_str().to_owned();
        if !leaked.contains(['年', '月', '日']) {
            value.replace_range(m.range(), " ");
            value = value.trim().to_owned();
            if let Some(next) = row.get_mut(ci + 1) {
                if !next.starts_with(&leaked) {
                    next.insert_str(0, &leaked)
                }
            }
        }
    }
    let compact = value.replace(' ', "");
    if let Some(c) =
        rx!(r"^(0?20\d{2}[-/.]?\d{2}[-/.]?\d{2})(\d)(\d{2}:\d{2}(?::\d{2})?)$").captures(&compact)
    {
        value = format!(
            "{} {}",
            c[1].strip_prefix("020")
                .map(|_| &c[1][1..])
                .unwrap_or(&c[1]),
            &c[3]
        );
        if ci > 0 && !row[ci - 1].is_empty() && row[ci - 1].chars().all(|c| c.is_numeric()) {
            row[ci - 1].push_str(&c[2]);
        }
    }
    // 分隔符也纳入匹配并原样写回，避免从长流水号内部截出一个日期。
    for (re, replace) in [
        (rx!(r"(\d{4}[-/.]?\d{2}[-/.]?\d{2})(\d{1,2}:)"), "$1 $2"),
        (
            rx!(r"\b(\d{4})[-/.]?(0[1-9]|1[0-2])[-/.]?(0[1-9]|[12]\d|3[01])(\s+\d{1,2}:)"),
            "$1-$2-$3$4",
        ),
        (
            rx!(r"(^|\D)(\d{4})\s*[-/.]\s*(0[1-9]|1[0-2])\s*[-/.]?\s*(0[1-9]|[12]\d|3[01])(\D|$)"),
            "$1$2-$3-$4$5",
        ),
        (
            rx!(r"(^|\D)(\d{4})\s*(0[1-9]|1[0-2])\s*[-/.]\s*(0[1-9]|[12]\d|3[01])(\D|$)"),
            "$1$2-$3-$4$5",
        ),
    ] {
        if let std::borrow::Cow::Owned(s) = re.replace_all(&value, replace) {
            value = s
        }
    }
    if let Some(m) = rx!(r"\d{4}-\d{2}-\d{2}\s+\d{1,2}:.*$").find(&value) {
        value = value[m.start()..].to_owned();
    }
    for (re, replace) in [
        (rx!(r"(\d{1,2}:\d{2}:\d)\s+(\d)(\D|$)"), "$1$2$3"),
        (rx!(r"(\d{1,2}:\d{2})\s+(\d{2})(\D|$)"), "$1:$2$3"),
        (rx!(r"(\d{1,2}:\d)\s+(\d:\d{2})"), "$1$2"),
        (rx!(r"(\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d)$"), "${1}0"),
        (rx!(r"(\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:)$"), "${1}00"),
    ] {
        if let std::borrow::Cow::Owned(s) = re.replace_all(&value, replace) {
            value = s
        }
    }
    if date_col && !value.contains([':', '-']) {
        let compact = value.replace(' ', "");
        if compact.chars().count() <= 10 {
            if let Some(m) = rx!(r"20\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])").find(&compact)
            {
                value = m.as_str().to_owned();
            }
        }
    }
    row[ci] = value;
}

fn dates(columns: &[String], rows: &mut [Vec<String>]) {
    if columns.is_empty() {
        return;
    }
    let mut indices = date_cols(columns);
    let has_date_header = !indices.is_empty();
    if !has_date_header {
        indices.push(0);
    }
    for row in rows.iter_mut() {
        for &ci in &indices {
            if row.get(ci).is_some_and(|c| !c.is_empty()) {
                fix_date(row, ci, columns[ci].contains("日期"));
                if has_date_header {
                    if let Some(date) = repair_repeated_separator(&row[ci]) {
                        row[ci] = date;
                    }
                }
            }
        }
    }
    for ci in indices {
        let mut n = 0;
        let mut hyphens = 0;
        for val in rows
            .iter()
            .filter_map(|r| r.get(ci))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            n += 1;
            hyphens += usize::from(rx!(r"^\d{4}-\d{2}-\d{2}").is_match(val));
        }
        if hyphens >= 2 && hyphens * 100 >= n * 15 {
            for val in rows
                .iter_mut()
                .filter_map(|r| r.get_mut(ci))
                .filter(|v| !v.is_empty())
            {
                *val = rx!(r"^(20\d{2})(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])($|\s)")
                    .replace(val.trim(), "$1-$2-$3$4")
                    .into_owned();
            }
        }
    }
}

fn discard(columns: &[String], row: &[String]) -> bool {
    let joined = row.concat().replace(' ', "");
    if matches!(
        joined.as_str(),
        "" | "具体交易明细" | "账户明细" | "交易明细"
    ) || contains_any(
        &joined,
        &[
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
        ],
    ) {
        return true;
    }
    let s = joined.trim();
    let letters = s.chars().filter(|c| c.is_alphabetic()).count();
    if s.chars().all(|c| c as u32 <= 0x2e7f)
        && letters >= 5
        && letters * 100 >= s.chars().count() * 65
    {
        return true;
    }
    let matches = row
        .iter()
        .zip(columns)
        .filter(|(v, c)| !c.is_empty() && native_fields::sanitize(v).contains(c.as_str()))
        .count();
    matches >= 3.max(row.len().min(columns.len()) / 2) || native_fields::skip(row)
}

fn filter_rows(columns: &[String], rows: &mut Vec<Vec<String>>) {
    let mut dates = date_cols(columns);
    if dates.is_empty() {
        if rows
            .iter()
            .take(15)
            .filter_map(|r| r.first())
            .filter(|c| statement_rows::contains_date(c))
            .count()
            < 2
        {
            rows.retain(|r| !discard(columns, r));
            return;
        }
        dates.push(0);
    }
    let money: Vec<_> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.contains("余额") || contains_any(c, AMOUNT))
        .map(|(i, _)| i)
        .collect();
    let mut kept: Vec<Vec<String>> = Vec::new();
    for row in rows.drain(..) {
        if discard(columns, &row) {
            continue;
        }
        if dates
            .iter()
            .filter_map(|&i| row.get(i))
            .any(|v| statement_rows::contains_date(v))
            || money
                .iter()
                .filter_map(|&i| row.get(i))
                .any(|v| number(&clean_amount(v)).is_some())
        {
            kept.push(row);
            continue;
        }
        if !row.first().is_some_and(|v| v.trim().is_empty()) {
            continue;
        }
        let cells: Vec<_> = row
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.trim()))
            .filter(|(_, s)| !s.is_empty())
            .collect();
        let joined: String = cells.iter().map(|(_, v)| *v).collect();
        let footer = rx!(
            r"(?:^说明[:：]$|^说明[:：]\s*1[.、]|^[1-5][.、]\s*本?《微信支付|仅证明[:：]在用户选择|仅用于用户申请本证明|不得用于其他用途|账户记录为准|对于红包[，,]转账[，,]二维码|零钱的充值和提现|^重要提示[:：]|^温馨提示[:：]|^该交易明细因不可预测|^(?:打印完毕|打印完成|打印结束)[。.!！]?$)"
        );
        if cells.is_empty()
            || cells.len() > 2
            || joined.chars().count() > 18
            || joined.contains(['。', '；', '！', '？', '《', '》', '：', ':'])
            || footer.is_match(&joined)
        {
            continue;
        }
        if let Some(last) = kept.last_mut() {
            for (i, val) in cells {
                if let Some(prev) = last.get_mut(i) {
                    if !prev.contains(val) {
                        *prev = format!("{prev}{val}").trim().into();
                    }
                }
            }
        }
    }
    *rows = kept;
}

pub(crate) fn prepare(columns: &[String], rows: &mut Vec<Vec<String>>, metadata: &mut Metadata) {
    dates(columns, rows);
    filter_rows(columns, rows);
    for pairs in &mut *metadata {
        pairs.retain(|(k, v)| {
            let combined = format!("{k} {v}");
            let s = combined.trim();
            !s.is_empty()
                && (s.chars().any(|c| c as u32 > 0x2e7f) || !s.chars().any(|c| c.is_alphabetic()))
        });
    }
    metadata.retain(|r| !r.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubled_separators_require_one_complete_calendar_date() {
        assert_eq!(
            repair_repeated_separator("2024--02-29").as_deref(),
            Some("2024-02-29")
        );
        assert_eq!(
            repair_repeated_separator("2026/01//02 08:15:30").as_deref(),
            Some("2026-01-02 08:15:30")
        );
        for value in [
            "2025--02-29",
            "2026--01",
            "2026-01-01--2026-01-02",
            "2026--01/02",
            "2026--01-02 25:01:02",
        ] {
            assert!(repair_repeated_separator(value).is_none(), "{value}");
        }
        let mut rows = vec![vec!["2026--01-02".to_owned()]];
        dates(&["流水号".to_owned()], &mut rows);
        assert_eq!(rows[0][0], "2026--01-02");
    }

    #[test]
    fn dates_filtering_and_metadata_use_document_context() {
        let columns = ["流水号", "交易日期", "摘要", "金额"].map(str::to_string);
        let mut rows = [
            ["12026", "-02-28扫09:31:57", "码付款", "3.00"],
            ["2", "20250227 15:42:5", "交易", "5.00"],
            ["3", "20250226", "余额", "7.00"],
            ["", "", "续行", ""],
            ["", "", "仅供个人参考", ""],
        ]
        .map(|r| r.map(str::to_string).to_vec())
        .to_vec();
        let mut meta = vec![vec![
            ("Account".into(), "No".into()),
            ("账户".into(), "示例".into()),
        ]];
        prepare(&columns, &mut rows, &mut meta);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ["1", "2026-02-28 09:31:57", "扫码付款", "3.00"]);
        assert_eq!(rows[1][1], "2025-02-27 15:42:50");
        assert_eq!(rows[2][1], "2025-02-26");
        assert_eq!(rows[2][2], "余额续行");
        assert_eq!(meta, vec![vec![("账户".into(), "示例".into())]]);
    }
}
