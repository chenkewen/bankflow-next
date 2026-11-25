use regex::Regex;
use std::sync::LazyLock;

type Row = Vec<String>;

fn trimmed(text: &str) -> &str {
    // Python str.strip 还把四个 ASCII 分隔控制符视为空白。
    text.trim_matches(python_space)
}

fn merge_continuations(rows: Vec<Row>) -> Vec<Row> {
    let mut result: Vec<Row> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(previous) = result.last_mut() {
            if row.first().is_some_and(|s| trimmed(s).is_empty()) {
                let populated = row.iter().filter(|s| !trimmed(s).is_empty()).count();
                if populated > 0 && populated < (row.len() / 2).max(2) {
                    for (target, value) in previous.iter_mut().zip(row) {
                        if !trimmed(&value).is_empty() {
                            if !target.is_empty() {
                                target.push(' ');
                            }
                            target.push_str(&value);
                        }
                    }
                    continue;
                }
            }
        }
        result.push(row);
    }
    result
}

pub(crate) fn starts_with_date(text: &str) -> bool {
    static DATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"^\d{4}(?:[-‑–]\d{1,2}[-‑–]\d{1,2}|/\d{1,2}/\d{1,2}|\.\d{1,2}\.\d{1,2}|年\d{1,2}月\d{1,2}日|(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?:\D|$))"
    ).unwrap()
    });
    DATE.is_match(trimmed(text))
}

fn partial_date(text: &str) -> bool {
    static FRAGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+[-‑–/.]\d+").unwrap());
    let text = trimmed(text);
    !starts_with_date(text) && FRAGMENT.is_match(text)
}

fn merge(columns: &[String], rows: Vec<Row>) -> Vec<Row> {
    let rows = merge_continuations(rows);
    let Some(date) = columns.iter().position(|name| date_header(name)) else {
        return rows;
    };
    let mut output = Vec::with_capacity(rows.len());
    let mut rows = rows.into_iter().peekable();
    while let Some(mut row) = rows.next() {
        if date < row.len()
            && row.iter().filter(|s| trimmed(s).is_empty()).count() > row.len() / 2
            && rows
                .peek()
                .and_then(|next| next.get(date))
                .is_some_and(|s| partial_date(s))
        {
            let next = rows.next().unwrap();
            row.resize_with(row.len().max(next.len()), String::new);
            for (i, cell) in row.iter_mut().enumerate() {
                let first = trimmed(cell);
                let second = next.get(i).map_or("", |s| trimmed(s));
                *cell = match (first.is_empty(), second.is_empty()) {
                    (true, _) => second.to_owned(),
                    (_, true) => first.to_owned(),
                    _ => format!("{first} {second}"),
                };
            }
        }
        output.push(row);
    }
    output
}

pub(crate) fn date_header(name: &str) -> bool {
    let name = name.to_lowercase();
    ["日期", "时间", "date", "time"]
        .iter()
        .any(|kw| name.contains(kw))
}

pub(crate) fn contains_date(value: &str) -> bool {
    static CONTENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"(?:^|\D)\d{4}(?:[-‑–/.]\d{1,2}[-‑–/.]\d{1,2}|(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?:\D|$)|年\d{1,2}月\d{1,2}日)"
    ).unwrap()
    });
    CONTENT.is_match(value)
}

fn date_columns(columns: &[String], rows: &[Row]) -> Vec<usize> {
    let named: Vec<_> = columns
        .iter()
        .enumerate()
        .filter(|(_, name)| date_header(name))
        .map(|(i, _)| i)
        .collect();
    if !named.is_empty() {
        return named;
    }
    // 只在没有日期表头时抽取前 20 行，日期命中必须超过该列非空样本的一半。
    (0..columns.len())
        .filter(|&i| {
            let mut count = 0;
            let mut hits = 0;
            for value in rows
                .iter()
                .take(20)
                .filter_map(|row| row.get(i))
                .filter(|v| !v.is_empty())
            {
                count += 1;
                hits += usize::from(contains_date(value));
            }
            2 * hits > count
        })
        .collect()
}

fn repair_fragment(value: &str) -> Option<String> {
    static FULL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(\d{4})-(\d)[\s\x1c-\x1f]+(\d)-(\d{2})[01][\s\x1c-\x1f]+(\d{1,2})(:\d{2}:\d{2})",
        )
        .unwrap()
    });
    static NO_YEAR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d)-(\d{2})[01][\s\x1c-\x1f]+(\d{1,2})(:\d{2}:\d{2})\n?$").unwrap()
    });
    if let Some(c) = FULL.captures(value) {
        return Some(format!(
            "{}-{}{}-{} {:0>2}{}",
            &c[1], &c[2], &c[3], &c[4], &c[5], &c[6]
        ));
    }
    NO_YEAR
        .captures(value)
        .map(|c| format!("-0{}-{} {:0>2}{}", &c[1], &c[2], &c[3], &c[4]))
}

fn normalize_dates(columns: &[String], rows: &mut [Row]) {
    static YEAR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(\d{4})(?:[-‑–/.年]|(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?:\D|$))")
            .unwrap()
    });
    for col in date_columns(columns, rows) {
        let mut previous_year = String::new();
        for value in rows.iter_mut().filter_map(|row| row.get_mut(col)) {
            if let Some(date) = repair_fragment(value) {
                *value = date;
            }
            if let Some(c) = YEAR.captures(value) {
                previous_year.clear();
                previous_year.push_str(&c[1]);
            } else if value.starts_with('-') {
                value.insert_str(0, &previous_year);
            }
        }
    }
}

fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn repair_adjacent_fragments(columns: &[String], rows: &mut [Row]) {
    let Some(date) = columns.iter().position(|c| date_header(c)) else {
        return;
    };
    let amount_next = columns.get(date + 1).is_some_and(|c| c.contains("金额"));
    let mut previous: Option<&mut String> = None;
    for row in rows {
        if row.len() <= date {
            previous = None;
            continue;
        }
        if amount_next && row.len() > date + 1 {
            if let Some(sign) = row[date].chars().last().filter(|c| matches!(c, '+' | '-')) {
                let head = &row[date][..row[date].len() - 1];
                if head.len() == 10
                    && starts_with_date(head)
                    && row[date + 1].starts_with(|c: char| c.is_ascii_digit())
                    && crate::native_statement::amount_cell(&row[date + 1])
                {
                    row[date].pop();
                    row[date + 1].insert(0, sign);
                }
            }
        }
        let cell = &mut row[date];
        if let Some(prev) = previous {
            if let Some((full, year)) = prev.rsplit_once(' ') {
                let y = year.as_bytes();
                if starts_with_date(full)
                    && y.len() == 5
                    && y[4] == b'-'
                    && y[..4].iter().all(u8::is_ascii_digit)
                    && cell.len() == 5
                    && cell.as_bytes()[2] == b'-'
                {
                    let joined = format!("{year}{cell}");
                    if starts_with_date(&joined) {
                        *cell = joined;
                        prev.truncate(full.len());
                    }
                }
            }
        }
        previous = Some(cell);
    }
}

pub(crate) fn pdf(columns: &[String], rows: Vec<Row>) -> Vec<Row> {
    let mut rows = merge(columns, rows);
    repair_adjacent_fragments(columns, &mut rows);
    rows.retain(|row| {
        !row.first().is_some_and(|v| {
            v.starts_with("账户明细")
                && v.contains("账号")
                && v.contains("户名")
                && v.contains("日期")
                && row[1..].iter().all(String::is_empty)
        })
    });
    normalize_dates(columns, &mut rows);
    rows
}

pub(crate) fn ocr(columns: &[String], rows: Vec<Row>) -> Vec<Row> {
    let mut rows = merge(columns, rows);
    normalize_dates(columns, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_and_amount_fragments_keep_their_source_digits() {
        let columns = ["流水号", "交易日期", "交易金额"].map(str::to_owned);
        let mut r = [
            ["001", "2026-02-01 2026-", "30.00"],
            ["002", "02-02", "20.00"],
            ["003", "2026-02-03-", "10.00"],
        ]
        .map(|v| v.map(str::to_owned).to_vec());
        repair_adjacent_fragments(&columns, &mut r);
        assert_eq!(r[0][1], "2026-02-01");
        assert_eq!(r[1][1], "2026-02-02");
        assert_eq!(r[2][1..], ["2026-02-03", "-10.00"]);
    }
    #[test]
    fn continuation_then_page_fragment() {
        let columns = ["日期", "金额", "摘要", "账号", "备注", "余额"].map(String::from);
        let rows = [
            ["2024", "", "收", "", "", ""],
            ["", "", "款", "", "", ""],
            ["-01-02", "10", "", "001", "", "20"],
        ]
        .map(|row| row.map(String::from).to_vec());
        assert_eq!(
            merge(&columns, rows.into()),
            [vec!["2024 -01-02", "10", "收 款", "001", "", "20"]]
        );
    }

    #[test]
    fn complete_date_starts_a_new_record() {
        for text in [
            "2024-01-02",
            "2024年1月2日",
            "20240102 12-30",
            "２０２４/01/02",
        ] {
            assert!(!partial_date(text));
        }
        assert!(partial_date("202401021 12-30"));
    }

    #[test]
    fn mixed_record_ids_keep_their_own_prefix_and_leading_zeroes() {
        let columns = ["流水号", "交易日期", "交易金额"].map(str::to_owned);
        let ids = [
            "000001",
            "POS 000002",
            "000003",
            "ATM 000004",
            "000005",
            "POS 000006",
            "000007",
        ];
        let rows: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(day, id)| {
                vec![
                    id.to_string(),
                    format!("2026-01-{:02}", day + 1),
                    "12.00".to_owned(),
                ]
            })
            .collect();
        assert_eq!(pdf(&columns, rows.clone()), rows);
    }

    #[test]
    fn mixed_dates_and_times_are_preserved_without_a_record_id() {
        for label in ["交易日期", "流水日期", "列1"] {
            let columns = [label, "金额"].map(str::to_owned);
            let rows = [
                ["2026-01-01", "20.00"],
                ["2026-01-02 09:00:00", "20.00"],
                ["2026-01-03", "20.00"],
                ["2026-01-04 10:00:00", "20.00"],
                ["2026-01-05", "20.00"],
            ]
            .map(|r| r.map(str::to_owned).to_vec())
            .to_vec();
            assert_eq!(pdf(&columns, rows.clone()), rows);
        }
    }

    #[test]
    fn date_inference_preserves_the_other_fields() {
        let columns = ["流水号", "值"].map(String::from);
        let rows = [
            ["X 1", "前缀2024/12/31"],
            ["X 2", "2024-0 1-021 7:04:48"],
            ["3", "1-091 9:45:56"],
            ["X 4", "20250101"],
            ["5", "-12-01"],
            ["X 6", "2025年1月2日"],
            ["X 7", "2025.01.03"],
        ]
        .map(|row| row.map(String::from).to_vec());
        let rows = pdf(&columns, rows.into());
        assert_eq!(rows[1][1], "2024-01-02 07:04:48");
        assert_eq!(rows[2], ["3", "2024-01-09 09:45:56"]);
        assert_eq!(rows[4], ["5", "2025-12-01"]);
    }

    #[test]
    fn ocr_leaves_identifiers_for_its_later_pass() {
        let columns = ["编号", "交易时间"].map(String::from);
        let rows = [
            ["P 1", "202401019"],
            ["2", "-01-02"],
            ["P 3", "2024年1月2日"],
            ["4", "1-091 9:45:56"],
            ["5", ""],
        ]
        .map(|row| row.map(String::from).to_vec());
        let rows = ocr(&columns, rows.into());
        assert_eq!(rows[1], ["2", "-01-02"]);
        assert_eq!(rows[3], ["4", "2024-01-09 09:45:56"]);
    }
}
