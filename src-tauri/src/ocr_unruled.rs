use super::{cells, english, header_name, ignored, joined, lines, mid};
use crate::{
    native_chars::Glyph,
    native_fields::{self, sanitize},
    native_statement::unruled::{gap_limit, KEYWORDS},
    statement_rows::starts_with_date,
};
use regex::Regex;
use std::sync::LazyLock;

pub(super) type Bounds = Vec<(f64, f64)>;
pub(super) struct Header {
    pub names: Vec<String>,
    pub bounds: Bounds,
    pub bottom: f64,
    pub meta_limit: f64,
}

fn hits(text: &str) -> usize {
    KEYWORDS.iter().filter(|word| text.contains(**word)).count()
}

fn header_range(groups: &mut [Vec<&Glyph>], h: &[f64]) -> Option<(usize, usize, f64)> {
    static NOISE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{2,}|[￥¥$：:]").unwrap());
    static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{4,}").unwrap());
    let scores: Vec<_> = groups
        .iter()
        .map(|line| {
            let words: Vec<_> = line
                .iter()
                .copied()
                .filter(|w| !NOISE.is_match(&w.text))
                .collect();
            hits(&joined(&words, " "))
        })
        .collect();
    let best_score = *scores.iter().max()?;
    let best = if best_score >= 3 {
        scores.iter().position(|&s| s == best_score)?
    } else {
        scores
            .windows(2)
            .enumerate()
            .find(|(i, s)| {
                s[0] > 0
                    && s[1] > 0
                    && s[0] + s[1] >= 4
                    && groups[i + 1][0].top - groups[*i][0].top < 22.
            })?
            .0
    };
    groups[best].retain(|w| !DIGITS.is_match(&w.text));
    if groups[best].is_empty() {
        return None;
    }
    let mut aligned: Vec<_> = groups[best]
        .iter()
        .map(|g| g.x0.round_ties_even())
        .collect();
    let center = |g: &[&Glyph]| g.iter().map(|v| mid(v)).sum::<f64>() / g.len() as f64;
    let continues = |a: &[&Glyph], b: &[&Glyph], xs: &[f64]| {
        let (ya, yb) = (center(a), center(b));
        if (a[0].top - b[0].top).abs() >= 20.
            || h.iter().any(|y| ya.min(yb) < *y && *y < ya.max(yb))
        {
            return false;
        }
        let s = joined(b, " ");
        !s.contains([':', '：', '￥', '¥', '$'])
            && !s.bytes().any(|c| c.is_ascii_digit())
            && !english(&s)
            && (hits(&s) > 0
                || (b.len() <= 5
                    && !s.bytes().any(|c| c.is_ascii_alphanumeric())
                    && b.iter()
                        .all(|w| xs.iter().any(|x| (w.x0.round_ties_even() - x).abs() < 15.))))
    };
    let mut first = best;
    let mut last = best;
    while first > 0 && continues(&groups[first], &groups[first - 1], &aligned) {
        first -= 1;
        aligned.extend(groups[first].iter().map(|w| w.x0.round_ties_even()));
    }
    while last + 1 < groups.len() && continues(&groups[last], &groups[last + 1], &aligned) {
        last += 1;
        aligned.extend(groups[last].iter().map(|w| w.x0.round_ties_even()));
    }
    let mut extra = 0.;
    for i in last + 1..groups.len() {
        if groups[i][0].top - groups[i - 1][0].top >= 25. || !english(&joined(&groups[i], " ")) {
            break;
        }
        extra = groups[i].iter().map(|w| w.bottom).fold(0., f64::max);
    }
    Some((first, last, extra))
}

fn label(cluster: &[&Glyph]) -> String {
    let mut words = cluster.to_vec();
    words.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut names = Vec::new();
    for line in words.chunk_by(|a, b| b.top - a.top < 5.) {
        let mut line = line.to_vec();
        line.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let name = joined(&line, "");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    sanitize(&native_fields::clean_header(&names.concat()))
}

fn split_columns(words: &[&Glyph], width: f64, v: &[f64]) -> (Vec<String>, Bounds) {
    let mut words = words.to_vec();
    words.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let groups = words
        .chunk_by(|a, b| {
            let same = (b.top - a.top).abs() < 5.;
            let known = KEYWORDS.contains(&a.text.as_str()) && KEYWORDS.contains(&b.text.as_str());
            !(same && known) && (b.x0 - a.x0) < if same { 8.5 } else { 12. }
        })
        .collect::<Vec<_>>();
    let mut raw = groups.iter().map(|g| label(g)).collect::<Vec<_>>();
    for i in 1..raw.len() {
        if matches!(raw[i - 1].as_str(), "摘要渠" | "摘渠" | "要渠")
            && matches!(raw[i].as_str(), "交易道" | "要交易道" | "渠道")
        {
            raw[i - 1] = "摘要".into();
            raw[i] = "交易渠道".into();
        }
    }
    let mut names = Vec::new();
    let mut spans = Bounds::new();
    for (name, g) in raw.into_iter().zip(&groups) {
        let left = g[0].x0;
        let right = g.iter().map(|w| w.x1).fold(f64::NEG_INFINITY, f64::max);
        if name.contains(['摘', '要']) && name.contains(['渠', '道']) && name != "交易摘要"
        {
            let mut labels = vec!["摘要", "交易渠道"];
            for other in ["用途", "备注"] {
                if name.contains(other) {
                    labels.push(other);
                }
            }
            let step = (right - left).max(12.) / labels.len() as f64;
            for (i, text) in labels.into_iter().enumerate() {
                names.push(text.into());
                spans.push((left + i as f64 * step, left + (i + 1) as f64 * step - 1.));
            }
        } else if name.contains("收/支/其他") && name.contains("交易方式") {
            let x = left + (right - left) * 0.42;
            names.extend(["具体交易明细收/支/其他".into(), "交易方式".into()]);
            spans.extend([(left, x - 1.), (x, right)]);
        } else {
            names.push(header_name(&name));
            spans.push((left, right));
        }
    }
    if v.len() == 9
        && ["交易单号", "商户单号"]
            .iter()
            .all(|key| names.iter().any(|n| n.contains(key)))
    {
        names = super::wechat_columns();
    }
    if names.len() <= 1 {
        let groups = words.chunk_by(|a, b| b.x0 - a.x0 < 12.).collect::<Vec<_>>();
        return (
            groups.iter().map(|g| label(g)).collect(),
            groups
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    (
                        g[0].x0 - 4.,
                        groups.get(i + 1).map_or(width, |n| n[0].x0 - 0.5),
                    )
                })
                .collect(),
        );
    }
    if names.len() + 1 == v.len() {
        return (names, v.windows(2).map(|s| (s[0], s[1])).collect());
    }
    let margin = |gap: f64, overlap: f64| {
        if gap > 0. {
            (gap * 0.2).clamp(0.5, 3.)
        } else {
            overlap
        }
    };
    let bounds = spans
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let left = if i == 0 {
                (a - 10.).max(0.)
            } else {
                a - margin(a - spans[i - 1].1, 2.)
            };
            let right = spans
                .get(i + 1)
                .map_or(width, |(x, _)| x - margin(x - b, 0.5));
            (left, right)
        })
        .collect();
    (names, bounds)
}

pub(super) fn detect(
    words: &[Glyph],
    width: f64,
    height: f64,
    h: &[f64],
    v: &[f64],
) -> Option<Header> {
    let mut lines = lines(words.iter().collect(), 6.5);
    if let Some((first, last, extra)) = header_range(&mut lines, h) {
        let selected: Vec<_> = lines[first..=last].iter().flatten().copied().collect();
        let top = selected.iter().map(|w| w.top).fold(f64::INFINITY, f64::min);
        let bottom = selected.iter().map(|w| w.bottom).fold(extra, f64::max);
        // 证明页在表头上方可能另有标题和信息表；以识别到的表头顶部限定元数据。
        let meta_limit = top - 1.;
        let (names, bounds) = split_columns(&selected, width, v);
        return Some(Header {
            names,
            bounds,
            bottom,
            meta_limit,
        });
    }
    // 截图可能从流水正文开始，使用重复出现的左边缘推断列。
    let mut sorted = words.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    if sorted.is_empty() {
        return None;
    }
    let mut groups = sorted
        .chunk_by(|a, b| b.x0 - a.x0 < 20.)
        .collect::<Vec<_>>();
    if groups.iter().any(|g| g.len() >= 2) {
        groups.retain(|g| g.len() >= 2);
    }
    let names = (1..=groups.len()).map(|i| format!("列{i}")).collect();
    let bounds = groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            (
                g[0].x0 - 4.,
                groups.get(i + 1).map_or(width, |n| n[0].x0 - 1.),
            )
        })
        .collect();
    Some(Header {
        names,
        bounds,
        bottom: 0.,
        meta_limit: height,
    })
}

pub(super) fn start_x(chars: &[&Glyph]) -> Option<f64> {
    let rows = lines(chars.to_vec(), 6.);
    let max = rows.iter().map(Vec::len).max()?;
    if max < 8 {
        return None;
    }
    let mut starts = rows
        .iter()
        .filter(|g| g.len() as f64 >= max as f64 * 0.45 && g[0].x0 < 160.)
        .map(|g| g[0].x0)
        .collect::<Vec<_>>();
    starts.sort_by(f64::total_cmp);
    starts.get(starts.len() / 2).copied()
}

fn dated(row: &[String]) -> bool {
    row.iter().take(2).any(|s| starts_with_date(s))
}
fn move_previous_line(group: &[Vec<&Glyph>], cur: &[String], bounds: &[(f64, f64)]) -> bool {
    if group.len() < 2 {
        return false;
    }
    let prev = cells(group.last().unwrap(), bounds);
    if dated(&prev) {
        return false;
    }
    let rest: Vec<_> = group[..group.len() - 1].iter().flatten().copied().collect();
    let rest = cells(&rest, bounds);
    if !dated(&rest) {
        return false;
    }
    let short = prev
        .iter()
        .filter(|s| !s.is_empty())
        .all(|s| s.chars().count() <= 4)
        || prev.iter().any(|s| {
            matches!(
                s.as_str(),
                "限公司"
                    | "有限公司"
                    | "限有限公司"
                    | "公司"
                    | "款"
                    | "入"
                    | "出"
                    | "部"
                    | "户"
            )
        });
    !short
        && (2..bounds.len())
            .any(|i| !prev[i].is_empty() && cur[i].is_empty() && !rest[i].is_empty())
}

pub(super) fn text_records(
    chars: Vec<&Glyph>,
    bounds: &[(f64, f64)],
    columns: &[String],
) -> Vec<(Vec<String>, f64, f64)> {
    let rows = lines(chars, 5.5)
        .into_iter()
        .filter(|line| {
            let text = joined(line, "");
            !native_fields::skip_text(&text, false) && !english(&text)
        })
        .collect::<Vec<_>>();
    if rows.len() == 1 {
        let row = cells(&rows[0], bounds);
        return if native_fields::skip(&row) {
            vec![]
        } else {
            vec![(
                row,
                rows[0].iter().map(|g| g.top).fold(f64::INFINITY, f64::min),
                rows[0].iter().map(|g| g.bottom).fold(0., f64::max),
            )]
        };
    }
    let tops = rows
        .iter()
        .map(|row| row.iter().map(|g| g.top).fold(f64::INFINITY, f64::min))
        .collect::<Vec<_>>();
    let gaps = tops.windows(2).map(|p| p[1] - p[0]).collect::<Vec<_>>();
    let threshold = gap_limit(&gaps);
    // 日期可位于同笔多行交易的中线，先保留物理行再确定其所属交易。
    let mut groups: Vec<Vec<Vec<&Glyph>>> = vec![];
    for (i, line) in rows.into_iter().enumerate() {
        if i == 0 {
            groups.push(vec![line]);
            continue;
        }
        let preview = cells(&line, bounds);
        let date = dated(&preview);
        if date || threshold.is_some_and(|t| gaps[i - 1] > t) {
            let mut next = Vec::new();
            if date
                && gaps[i - 1] < 8.5
                && move_previous_line(groups.last().unwrap(), &preview, bounds)
            {
                next.push(groups.last_mut().unwrap().pop().unwrap());
            }
            groups.push(next);
        }
        groups.last_mut().unwrap().push(line);
    }
    groups
        .into_iter()
        .map(|g| {
            let glyphs: Vec<_> = g.into_iter().flatten().collect();
            let top = glyphs.iter().map(|g| g.top).fold(f64::INFINITY, f64::min);
            let bottom = glyphs.iter().map(|g| g.bottom).fold(0., f64::max);
            (cells(&glyphs, bounds), top, bottom)
        })
        .filter(|(r, _, _)| !ignored(r, columns))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_table::Table;

    fn glyph(text: &str, x: f64, y: f64) -> Glyph {
        Glyph {
            text: text.into(),
            x0: x,
            x1: x + text.chars().count() as f64 * 5.,
            top: y,
            bottom: y + 5.,
        }
    }

    #[test]
    fn unruled_records_keep_short_name_suffix_on_previous_transaction() {
        let words = [
            glyph("交易日期", 10., 10.),
            glyph("交易金额", 100., 10.),
            glyph("摘要", 200., 10.),
            glyph("2026-01-01", 10., 40.),
            glyph("20.00", 100., 40.),
            glyph("示例有限", 200., 40.),
            glyph("公司", 200., 47.),
            glyph("2026-01-02", 10., 54.),
            glyph("10.00", 100., 54.),
            glyph("还款", 200., 54.),
        ];
        let mut table = Table::default();
        assert!(table.page(300., 200., &words, &words, &[], &[]));
        assert_eq!(table.columns, ["交易日期", "交易金额", "摘要"]);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0], ["2026-01-01", "20.00", "示例有限公司"]);
        assert_eq!(table.rows[1], ["2026-01-02", "10.00", "还款"]);
    }

    #[test]
    fn horizontal_only_table_keeps_unruled_continuation_page() {
        let header = [
            glyph("交易日期", 10., 10.),
            glyph("交易金额", 100., 10.),
            glyph("余额", 200., 10.),
        ];
        let body = [
            glyph("2026-01-01", 10., 33.),
            glyph("20.00", 100., 33.),
            glyph("100.00", 200., 33.),
        ];
        let first = [header.as_slice(), body.as_slice()].concat();
        let mut table = Table::default();
        assert!(table.page(300., 200., &first, &first, &[8., 22., 52., 82.], &[]));
        let next = [
            glyph("2026-01-02", 10., 5.),
            glyph("10.00", 100., 5.),
            glyph("110.00", 200., 5.),
        ];
        assert!(table.page(300., 200., &next, &next, &[], &[]));
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1], ["2026-01-02", "10.00", "110.00"]);
    }

    #[test]
    fn headerless_image_projects_repeated_column_starts() {
        let words = [
            glyph("2026-01-01", 10., 20.),
            glyph("10.00", 130., 20.),
            glyph("2026-01-02", 10., 40.),
            glyph("11.00", 130., 40.),
        ];
        let mut table = Table::default();
        assert!(table.page(300., 200., &words, &words, &[], &[]));
        assert_eq!(table.columns, ["列1", "列2"]);
        assert_eq!(table.rows.len(), 2);
    }
}
