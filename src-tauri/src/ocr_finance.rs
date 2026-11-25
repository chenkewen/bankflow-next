#[path = "ocr_finalize.rs"]
pub(crate) mod finalize;
use regex::Regex;
use std::sync::LazyLock;

#[derive(Default)]
pub(crate) struct Corrections {
    pub repaired: usize,
    pub warnings: Vec<usize>,
}

const AMOUNT: &[&str] = &[
    "金额",
    "借方",
    "贷方",
    "收入",
    "支出",
    "出账",
    "入账",
    "发生额",
];
const DIRECTION: &[&str] = &[
    "支/收",
    "收/支",
    "借/贷",
    "收支",
    "借贷",
    "收支标志",
    "收支方向",
];

fn contains_any(name: &str, keys: &[&str]) -> bool {
    keys.iter().any(|key| name.contains(key))
}

fn digit(ch: char) -> char {
    match ch {
        'O' | 'o' | 'D' | 'Q' | 'J' => '0',
        'l' | 'I' | '|' => '1',
        'Z' | 'z' => '2',
        'S' | 's' => '5',
        'b' | 'G' => '6',
        'B' | '$' => '8',
        'g' | 'q' => '9',
        '，' => ',',
        '。' => '.',
        other => other,
    }
}

fn clean_amount(raw: &str) -> String {
    static MISSING_DOT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\d{1,3}(?:,\d{3})+\d{2}$").unwrap());
    let raw = raw.trim();
    if raw.chars().any(|ch| ch as u32 > 0x2e7f) {
        return raw.into();
    }
    let body = raw.trim_start_matches(['¥', '￥', '$', '+', '-']);
    let prefix = &raw[..raw.len() - body.len()];
    let mut clean: String = body
        .trim_start()
        .chars()
        .filter(|&ch| ch != ' ')
        .map(digit)
        .collect();
    if clean.is_empty()
        || !clean
            .chars()
            .all(|ch| ch.is_numeric() || ch == '.' || ch == ',')
    {
        return raw.into();
    }
    if let Some(pos) = clean.rfind('.') {
        clean = clean[..pos].replace('.', ",") + &clean[pos..];
    } else if MISSING_DOT.is_match(&clean) {
        let pos = clean.char_indices().rev().nth(1).unwrap().0;
        clean.insert(pos, '.');
    } else if let Some((left, right)) = clean.rsplit_once(',') {
        if !left.is_empty() && right.chars().count() == 2 {
            clean = format!("{left}.{right}");
        }
    }
    format!("{prefix}{clean}")
}

fn clean_numbers(columns: &[String], rows: &mut [Vec<String>]) {
    static DATETIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\b\d{4}(?:[-/.]\d{1,2}[-/.]\d{1,2}|\d{4}))\s+(\d{1,2}:\d{2})").unwrap()
    });
    static COMPANY: LazyLock<Regex> = LazyLock::new(|| Regex::new("有限(?:有限)+公司").unwrap());
    static DECIMAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.\d$").unwrap());
    static LEAK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d)([\u{4e00}-\u{9fff}].*)$").unwrap());
    for row in rows {
        for value in row.iter_mut().filter(|v| !v.is_empty()) {
            let joined = value.replace(['\r', '\n'], "");
            let mut text = DATETIME
                .replace_all(&joined, "${1}__DT_SPACE__${2}")
                .into_owned();
            // 只保留日期和时间之间的空格。
            text.retain(|ch| !ch.is_whitespace());
            *value = COMPANY
                .replace_all(&text.replace("__DT_SPACE__", " "), "有限公司")
                .into_owned();
        }
        for (i, name) in columns.iter().take(row.len()).enumerate() {
            if contains_any(name, AMOUNT) || name.contains("余额") {
                row[i] = clean_amount(&row[i]);
                if DECIMAL.is_match(&row[i]) {
                    if let Some(caps) = row.get(i + 1).and_then(|s| LEAK.captures(s)) {
                        let decimal = caps[1].to_owned();
                        let rest = caps[2].to_owned();
                        row[i].push_str(&decimal);
                        row[i + 1] = rest;
                    }
                }
            }
        }
        for (name, value) in columns.iter().zip(row.iter_mut()) {
            if DIRECTION.contains(&name.as_str())
                && !["支", "收", "借", "贷", "支出", "收入"].contains(&value.as_str())
            {
                if let Some(ch) = ["支", "收", "借", "贷"]
                    .into_iter()
                    .find(|ch| value.contains(ch))
                {
                    *value = ch.into();
                }
            }
        }
    }
}

fn number(value: &str) -> Option<f64> {
    value
        .trim()
        .replace([',', '¥', '￥', ' '], "")
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
}

fn cents(value: f64) -> f64 {
    // 与 Python round(value, 2) 一样对浮点数的十进制表示舍入。
    format!("{value:.2}").parse().unwrap_or(value)
}

fn formatted(value: f64, reference: &str) -> String {
    let plain = format!("{:.2}", value.abs());
    let Some((whole, decimal)) = plain.split_once('.') else {
        return format!("{value:.2}");
    };
    if !reference.contains(',') && value.abs() < 1000.0 {
        return format!("{value:.2}");
    }
    let mut result = String::new();
    if value.is_sign_negative() {
        result.push('-');
    }
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    format!("{result}.{decimal}")
}

#[derive(Default)]
struct Columns {
    balance: Option<usize>,
    debit: Option<usize>,
    credit: Option<usize>,
    amount: Option<usize>,
    direction: Option<usize>,
}

fn cell(row: &[String], col: Option<usize>) -> Option<&str> {
    row.get(col?).map(String::as_str)
}

impl Columns {
    fn new(names: &[String]) -> Self {
        let mut c = Self::default();
        for (i, name) in names.iter().enumerate() {
            if c.balance.is_none() && name.contains("余额") {
                c.balance = Some(i);
            } else if (name.contains("收入") && name.contains("支出")
                || name.contains("借方") && name.contains("贷方"))
                && name.contains("金额")
            {
                c.amount = Some(i);
            } else if c.debit.is_none() && contains_any(name, &["借方", "支出", "转出", "出账"])
            {
                c.debit = Some(i);
            } else if c.credit.is_none() && contains_any(name, &["贷方", "收入", "转入", "入账"])
            {
                c.credit = Some(i);
            } else if c.amount.is_none() && contains_any(name, AMOUNT) {
                c.amount = Some(i);
            }
            if c.direction.is_none() && DIRECTION.contains(&name.as_str()) {
                c.direction = Some(i);
            }
        }
        if c.debit.is_some() && c.credit.is_some() {
            c.amount = None;
        }
        c
    }

    fn net(&self, row: &[String]) -> Option<f64> {
        if self.credit.is_some() || self.debit.is_some() {
            let get = |pos| cell(row, pos).and_then(number).unwrap_or(0.0);
            return Some(cents(get(self.credit) - get(self.debit)));
        }
        let amount = number(cell(row, self.amount)?)?;
        Some(
            if amount > 0.0 && matches!(cell(row, self.direction), Some("支" | "支出" | "借")) {
                -amount
            } else {
                amount
            },
        )
    }

    fn balance(&self, row: &[String]) -> Option<f64> {
        cell(row, self.balance).and_then(number)
    }
}

fn truncated(actual: f64, recognized: f64) -> bool {
    let actual = format!("{:.2}", actual.abs());
    let recognized = format!("{:.2}", recognized.abs());
    actual.len() > recognized.len() && actual.ends_with(&recognized)
}

fn reconcile(names: &[String], rows: &mut [Vec<String>]) -> Corrections {
    let mut report = Corrections::default();
    if rows.len() < 2 {
        return report;
    }
    let c = Columns::new(names);
    if let (Some(dir), Some(amt)) = (c.direction, c.amount) {
        for row in rows.iter_mut() {
            if row.get(dir).is_some_and(|s| s.trim().is_empty()) {
                if let Some(n) = row.get(amt).and_then(|s| number(s)).filter(|n| *n != 0.0) {
                    row[dir] = if n < 0.0 { "支" } else { "收" }.into();
                }
            }
        }
    }
    let Some(bal) = c.balance else { return report };
    let mut asc = 0;
    let mut desc = 0;
    let mut checked = 0;
    for pair in rows[..rows.len().min(20)].windows(2) {
        if let (Some(a), Some(b)) = (c.balance(&pair[0]), c.balance(&pair[1])) {
            checked += 1;
            asc += usize::from(
                c.net(&pair[1])
                    .is_some_and(|net| (b - a - net).abs() <= 0.02),
            );
            desc += usize::from(
                c.net(&pair[0])
                    .is_some_and(|net| (a - b - net).abs() <= 0.02),
            );
        }
    }
    // 按收支筛选导出的流水本身不连续，禁止据差额改写。
    if checked >= 4 && asc.max(desc) * 100 < checked * 45 {
        return report;
    }
    let descending = desc > asc;
    let chronological = |index: usize| {
        if descending {
            rows.len() - 1 - index
        } else {
            index
        }
    };
    let order: Vec<usize> = (0..rows.len()).map(chronological).collect();
    // 有些流水用单列绝对值加文字描述表示收支。只有已观察到与余额吻合的负数时，
    // 才能认定该金额列本来带符号，避免把正常的无符号金额改成负数。
    let signed_amount = c.amount.is_some_and(|column| {
        order.windows(2).any(|pair| {
            let (previous, current) = (&rows[pair[0]], &rows[pair[1]]);
            match (
                c.balance(previous),
                c.balance(current),
                current.get(column).and_then(|s| number(s)),
            ) {
                (Some(before), Some(after), Some(amount)) => {
                    amount < 0. && ((after - before) - amount).abs() < 0.005
                }
                _ => false,
            }
        })
    });
    for i in 1..order.len() {
        let (previous, current) = (order[i - 1], order[i]);
        let Some(older) = c.balance(&rows[previous]) else {
            continue;
        };
        if signed_amount {
            // 串列会在符号前附上一个相邻字段的字符。保留印在图上的有符号金额，
            // 仅在该金额与前后两笔余额之差逐分吻合时剔除前缀。
            static SIGNED_SUFFIX: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"[+-](?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2}$").unwrap());
            if let (Some(column), Some(balance)) = (c.amount, c.balance(&rows[current])) {
                if let Some(value) = rows[current]
                    .get_mut(column)
                    .filter(|s| number(s).is_none())
                {
                    if let Some(suffix) = SIGNED_SUFFIX.find(value) {
                        if suffix.start() > 0
                            && number(suffix.as_str())
                                .is_some_and(|n| (balance - older - n).abs() < 0.005)
                        {
                            *value = suffix.as_str().to_owned();
                            report.repaired += 1;
                        }
                    }
                }
            }
        }
        let Some(net) = c.net(&rows[current]) else {
            continue;
        };
        if rows[current].len() <= bal {
            continue;
        }
        let reference = rows[previous][bal].clone();
        let newer = c.balance(&rows[current]);
        if signed_amount
            && net > 0.0
            && newer.is_some_and(|balance| (balance - older + net).abs() < 0.005)
        {
            if let Some(value) = c.amount.and_then(|k| rows[current].get_mut(k)) {
                *value = format!("-{}", value.trim_start_matches('+'));
                report.repaired += 1;
                continue;
            }
        }
        if newer.is_none_or(|n| (n - older - net).abs() > 0.015) {
            let mut running = older;
            let mut path: Vec<(usize, f64)> = vec![];
            let mut anchored = false;
            for &index in order.iter().skip(i).take(5) {
                let Some(delta) = c.net(&rows[index]) else {
                    break;
                };
                if rows[index].len() <= bal {
                    break;
                }
                running = cents(running + delta);
                if !path.is_empty()
                    && c.balance(&rows[index])
                        .is_some_and(|n| (running - n).abs() <= 0.02)
                {
                    for &(index, calculated) in &path {
                        rows[index][bal] = formatted(calculated, &reference);
                    }
                    report.repaired += path.len();
                    anchored = true;
                    break;
                }
                path.push((index, running));
            }
            if anchored {
                continue;
            }
        }
        let Some(newer) = newer else { continue };
        let diff = cents(newer - older);
        let amt = c.amount.filter(|&k| k < rows[current].len());
        if (diff.abs() - net.abs()).abs() <= 0.015 {
            if let Some(k) = amt {
                if diff < 0.0
                    && !rows[current][k].starts_with('-')
                    && matches!(cell(&rows[current], c.direction), Some("支" | "支出"))
                {
                    rows[current][k].insert(0, '-');
                    report.repaired += 1;
                }
            }
            continue;
        }
        if let Some(k) = amt {
            if (net.abs() / 100.0 - diff.abs()).abs() <= 0.02 || truncated(diff, net) {
                rows[current][k] = formatted(diff, &rows[current][k]);
                report.repaired += 1;
                continue;
            }
        } else {
            let split_col = if diff < 0.0 {
                c.debit
            } else if diff > 0.0 {
                c.credit
            } else {
                None
            };
            if let Some(k) = split_col.filter(|&k| k < rows[current].len()) {
                if rows[current][k].starts_with(',') || truncated(diff, net) {
                    rows[current][k] = formatted(diff.abs(), &reference);
                    report.repaired += 1;
                    continue;
                }
            }
        }
        if let Some(&next) = order.get(i + 1) {
            if let (Some(next_balance), Some(next_amount)) =
                (c.balance(&rows[next]), c.net(&rows[next]))
            {
                let balance = cents(next_balance - next_amount);
                let actual = cents(balance - older);
                let incomplete = rows[current][bal].starts_with(',');
                if (net - actual).abs() <= 0.02 || incomplete {
                    if (net - actual).abs() > 0.02 {
                        let split_col = if actual > 0.0 {
                            c.credit
                        } else if actual < 0.0 {
                            c.debit
                        } else {
                            None
                        };
                        if let Some(k) = split_col.filter(|&k| k < rows[current].len()) {
                            rows[current][k] = formatted(actual.abs(), &rows[current][k]);
                        }
                    }
                    rows[current][bal] = formatted(balance, &reference);
                    report.repaired += 1;
                    continue;
                }
            }
        }
        report.warnings.push(current + 1);
    }
    report
}

pub(crate) fn correct(columns: &[String], rows: &mut [Vec<String>]) -> Corrections {
    clean_numbers(columns, rows);
    reconcile(columns, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows<const N: usize>(values: &[[&str; N]]) -> Vec<Vec<String>> {
        values
            .iter()
            .map(|row| row.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn same_holder_can_have_distinct_accounts_with_leading_zeroes_and_letters() {
        let columns = ["对方户名".to_owned(), "对方账号".to_owned()];
        let expected = rows(&[
            ["示例测试公司", "0012345678901234"],
            ["示例测试公司", "1012345678901234"],
            ["示例测试公司", "0O12345678901234"],
        ]);
        let mut actual = expected.clone();
        correct(&columns, &mut actual);
        assert_eq!(actual, expected);
    }

    #[test]
    fn signed_amounts_follow_the_printed_convention_and_balance() {
        let columns = ["收入/支出金额".into(), "余额".into()];
        let mut data = rows(&[
            ["2", "100"],
            ["-5", "95"],
            ["15", "80"],
            ["I-12.00", "68"],
            ["+7", "75"],
            ["3", "78"],
        ]);
        correct(&columns, &mut data);
        assert_eq!(data[2][0], "-15");
        assert_eq!(data[3][0], "-12.00");
        let mut unsigned = rows(&[
            ["2", "100"],
            ["5", "95"],
            ["15", "80"],
            ["2", "82"],
            ["7", "89"],
            ["3", "92"],
        ]);
        correct(&columns, &mut unsigned);
        assert_eq!(unsigned[2][0], "15");
    }

    #[test]
    fn repairs_missing_balances_in_both_orders() {
        let columns = ["金额".into(), "余额".into()];
        let input = [
            ["10", "100.00"],
            ["10", "模糊"],
            ["20", "模糊"],
            ["30", "160.00"],
            ["5", "165.00"],
            ["3", "168.00"],
        ];
        for reverse in [false, true] {
            let mut data = rows(&input);
            if reverse {
                data.reverse();
            }
            let report = correct(&columns, &mut data);
            if reverse {
                data.reverse();
            }
            assert_eq!(data[1][1], "110.00");
            assert_eq!(data[2][1], "130.00");
            assert_eq!(report.repaired, 2);
            assert!(report.warnings.is_empty());
        }
    }

    #[test]
    fn discontinuous_balances_in_filtered_statements_are_preserved() {
        let columns = [
            "交易日期".into(),
            "金额".into(),
            "余额".into(),
            "收/支".into(),
        ];
        let mut data = rows(&[
            ["2026-01-01 12:30", "10", "100", ""],
            ["2026-01-02 14:30", "1O", "130", ""],
            ["2026-01-03 12:30", "10", "200", ""],
            ["2026-01-04 14:30", "10", "350", ""],
            ["2026-01-05 12:30", "10", "500", ""],
        ]);
        let balances: Vec<_> = data.iter().map(|r| r[2].clone()).collect();
        let report = correct(&columns, &mut data);
        assert_eq!(report.repaired, 0);
        assert_eq!(data[1], ["2026-01-02 14:30", "10", "130", "收"]);
        assert_eq!(
            balances,
            data.iter().map(|r| r[2].clone()).collect::<Vec<_>>()
        );
    }
}
