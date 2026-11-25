use crate::{native_statement::StatementData, ocr_table::Table, statement_rows};
use std::collections::HashMap;

fn compact(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn money(name: &str) -> bool {
    [
        "金额",
        "发生额",
        "余额",
        "支出",
        "收入",
        "转入",
        "转出",
        "借方",
        "贷方",
    ]
    .iter()
    .any(|key| name.contains(key))
}

fn identity(name: &str) -> bool {
    ["流水号", "交易单号", "交易编号", "日志号", "凭证号"]
        .iter()
        .any(|key| name.contains(key))
}

fn matches_row(columns: &[String], source: &[String], ocr: &[String]) -> bool {
    if source.len() != columns.len() || ocr.len() != columns.len() {
        return false;
    }
    let mut has_date_or_id = false;
    let mut independent = 0;
    let mut amount_matches = false;
    for ((name, left), right) in columns.iter().zip(source).zip(ocr) {
        let left = compact(left);
        if left.is_empty() || left != compact(right) {
            continue;
        }
        if statement_rows::date_header(name)
            && (statement_rows::starts_with_date(&left) || left.contains(':'))
            || identity(name) && left.len() >= 8
        {
            has_date_or_id = true;
        }
        if money(name) && left.replace([',', '￥', '¥'], "").parse::<f64>().is_ok() {
            amount_matches = true;
        }
        if left.chars().count() >= 3 || money(name) && left.chars().any(|c| c.is_ascii_digit()) {
            independent += 1;
        }
    }
    has_date_or_id && independent >= 3 && (amount_matches || independent >= 4)
}

fn compatible(column: &str, other: &str) -> bool {
    let a = compact(column);
    let b = compact(other);
    a == b
        || (a.chars().count().min(b.chars().count()) >= 3
            && a.chars().count().abs_diff(b.chars().count()) == 1
            && (a.starts_with(&b) || b.starts_with(&a)))
}

#[derive(Hash, Eq, PartialEq)]
enum RowKey {
    Id(usize, String),
    DateAndAmounts(Vec<String>),
}

fn row_keys(columns: &[String], row: &[String]) -> Vec<RowKey> {
    let mut keys = Vec::new();
    let mut date_amounts = Vec::new();
    let (mut date, mut amount) = (false, false);
    for (col, (header, value)) in columns.iter().zip(row).enumerate() {
        let value = compact(value);
        if statement_rows::date_header(header) {
            date |= statement_rows::starts_with_date(&value);
            date_amounts.push(value);
        } else if money(header) {
            amount |= crate::native_statement::amount_cell(&value);
            date_amounts.push(value);
        } else if (identity(header) || header.contains("商户单号"))
            && value.len() >= 8
            && value.bytes().any(|c| c.is_ascii_digit())
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            keys.push(RowKey::Id(col, value));
        }
    }
    if date && amount {
        keys.push(RowKey::DateAndAmounts(date_amounts));
    }
    keys
}

// 只有物理页边界一致、每笔交易都在本页唯一对应时，才逐格免去重复识别。
pub(super) fn matched_page<'a>(
    reference: &'a StatementData,
    page: usize,
    table: &Table,
) -> Option<&'a [Vec<String>]> {
    let end = *reference.page_ends.get(page)?;
    let start = page
        .checked_sub(1)
        .map_or(0, |prev| reference.page_ends[prev]);
    let rows = reference.rows.get(start..end)?;
    if !reference.all_pages_have_rows
        || rows.is_empty()
        || rows.len() != table.page_rows.len()
        || table.rows.len() != end
        || !table
            .columns
            .iter()
            .map(|s| compact(s))
            .eq(reference.columns.iter().map(|s| compact(s)))
    {
        return None;
    }
    let mut unique = HashMap::new();
    for (i, source) in rows.iter().enumerate() {
        for key in row_keys(&reference.columns, source) {
            unique
                .entry(key)
                .and_modify(|value| *value = None)
                .or_insert(Some(i));
        }
    }
    for (i, (source, &(row, _, _))) in rows.iter().zip(&table.page_rows).enumerate() {
        let ocr = table.rows.get(row)?;
        if row != start + i || !matches_row(&reference.columns, source, ocr) {
            return None;
        }
        let mut matched = false;
        for key in row_keys(&reference.columns, ocr) {
            if let Some(Some(found)) = unique.get(&key) {
                if *found != i {
                    return None;
                }
                matched = true;
            }
        }
        if !matched {
            return None;
        }
    }
    Some(rows)
}

pub(super) fn same_field(source: &str, ocr: &str) -> bool {
    !source.trim().is_empty()
        && !source.contains(['\u{fffd}', '\0'])
        && source
            .chars()
            .filter(|c| !c.is_whitespace())
            .eq(ocr.chars().filter(|c| !c.is_whitespace()))
}

// 逐笔核对已识别的字段。不从文字层增删交易，防止漏字页、混合扫描页或相邻重号导致串行。
pub(super) fn verify(
    columns: &mut Vec<String>,
    rows: &mut [Vec<String>],
    reference: StatementData,
    notes: &mut super::field_cells::Notes,
) -> usize {
    if !reference.all_pages_have_rows
        || columns.len() != reference.columns.len()
        || rows.len() != reference.rows.len()
        || !columns
            .iter()
            .zip(&reference.columns)
            .all(|(a, b)| compatible(a, b))
    {
        return 0;
    }
    let mut changes = 0;
    let mut verified = 0;
    for (row_idx, (row, source)) in rows.iter_mut().zip(&reference.rows).enumerate() {
        if !matches_row(&reference.columns, source, row) {
            continue;
        }
        verified += 1;
        for (col, ((value, original), name)) in row
            .iter_mut()
            .zip(source)
            .zip(&reference.columns)
            .enumerate()
        {
            // 源单元格为空可能是嵌入图片或缺字。金额只采用能完整解析的数字。
            if original.trim().is_empty() || original.contains('\u{fffd}') {
                continue;
            }
            if money(name)
                && !original
                    .trim()
                    .replace([',', '¥', '￥'], "")
                    .parse::<f64>()
                    .is_ok_and(f64::is_finite)
            {
                continue;
            }
            notes.remove(&(row_idx, col));
            if value != original {
                *value = original.clone();
                changes += 1;
            }
        }
    }
    if verified > 0 && verified * 10 >= rows.len() * 9 {
        columns.clone_from(&reference.columns);
    }
    changes
}
#[cfg(test)]
mod tests {
    use super::*;
    fn text<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.map(str::to_owned).to_vec()
    }
    #[test]
    fn skip_reread_only_on_the_aligned_physical_page_and_matching_cells() {
        let mut table = Table::default();
        table.columns = text(["交易时间", "交易单号", "账号", "金额", "余额"]);
        table.rows = vec![
            text(["2026-01-01", "12340001", "001", "50.00", "100.00"]),
            text(["2026-01-02", "12340002", "002", "40.00", "140.00"]),
            text(["2026-01-02", "12340003", "003", "40.00", "180.00"]),
        ];
        table.page_rows = vec![(1, 10., 20.), (2, 20., 30.)];
        let mut reference = StatementData {
            all_pages_have_rows: true,
            columns: table.columns.clone(),
            rows: table.rows.clone(),
            page_ends: vec![1, 3],
            ..Default::default()
        };
        // 账号漏零时，交易仍可靠对应，但该格须按图复读。
        table.rows[1][2] = "02".into();
        let aligned = matched_page(&reference, 1, &table).unwrap();
        assert!(!same_field(&aligned[0][2], &table.rows[1][2]));
        assert!(same_field(&aligned[1][2], "00 3"));
        for missing in ["", "\u{fffd}", "\0"] {
            assert!(!same_field(missing, missing));
        }
        reference.all_pages_have_rows = false;
        assert!(matched_page(&reference, 1, &table).is_none());
        reference.all_pages_have_rows = true;
        table.rows.swap(1, 2);
        assert!(matched_page(&reference, 1, &table).is_none());
        table.rows.swap(1, 2);
        table.page_rows.pop();
        assert!(matched_page(&reference, 1, &table).is_none());
    }
    #[test]
    fn ambiguous_or_conflicting_transaction_keys_require_reread() {
        let mut table = Table::default();
        table.columns = text(["交易时间", "交易单号", "对方户名", "金额"]);
        let a = text(["2026-01-01", "12340001", "甲公司", "30.00"]);
        table.rows = vec![a.clone(), a];
        table.page_rows = vec![(0, 0., 10.), (1, 10., 20.)];
        let mut reference = StatementData {
            all_pages_have_rows: true,
            columns: table.columns.clone(),
            rows: table.rows.clone(),
            page_ends: vec![2],
            ..Default::default()
        };
        assert!(matched_page(&reference, 0, &table).is_none());
        table.rows[1][1] = "12340002".into();
        table.rows[1][3] = "40.00".into();
        reference.rows.clone_from(&table.rows);
        // 首行的日期金额仍然吻合，但单号指向本页的另一笔。
        table.rows[0][1] = "12340002".into();
        assert!(matched_page(&reference, 0, &table).is_none());
    }
    #[test]
    fn corrects_a_sign_only_in_an_identified_transaction() {
        let mut columns = text(["交易日期", "对方户名", "对方账号", "金额", "余额"]);
        let mut rows = vec![
            text(["2026-01-01", "甲公司", "000000123", "30.00", "108.00"]),
            text(["2026-01-01", "乙公司", "000000456", "25.00", "8.00"]),
        ];
        let reference = StatementData {
            all_pages_have_rows: true,
            page_ends: vec![2],
            columns: columns.clone(),
            metadata: vec![],
            rows: vec![
                text(["2026-01-01", "甲公司", "000000123", "-30.00", "108.00"]),
                text(["2026-01-01", "丙公司", "456000000", "-25.00", "899.00"]),
            ],
        };
        assert_eq!(
            verify(&mut columns, &mut rows, reference, &mut Default::default()),
            1
        );
        assert_eq!(
            (&rows[0][3], &rows[1][3]),
            (&"-30.00".into(), &"25.00".into())
        );
    }
}
