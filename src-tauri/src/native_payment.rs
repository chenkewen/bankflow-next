use super::unruled::by_y;
use crate::{native_chars::Glyph, statement_rows};

pub(super) const COLUMNS: [&str; 8] = [
    "交易单号",
    "交易时间",
    "交易类型",
    "收/支/其他",
    "交易方式",
    "金额(元)",
    "交易对方",
    "商户单号",
];
type Table = [Vec<Option<[f64; 4]>>];

pub(super) fn header(table: &Table, glyphs: &[Glyph]) -> Option<f64> {
    if !table
        .iter()
        .any(|r| r.len() == 8 && r.iter().all(Option::is_some))
    {
        return None;
    }
    let stream: String = glyphs.iter().map(|g| g.text.trim()).collect();
    if !stream.contains("具体交易明细") || !stream.contains("交易单号") {
        return None;
    }
    let line: Vec<_> = glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .collect();
    let pos = line.windows(4).position(|s| {
        s.iter()
            .map(|g| g.text.as_str())
            .eq(["商", "户", "单", "号"])
    })?;
    let top = line[pos].top;
    let mut labels: Vec<_> = line
        .into_iter()
        .filter(|g| (g.top - top).abs() < 4.)
        .collect();
    labels.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let labels: String = labels.iter().map(|g| g.text.as_str()).collect();
    COLUMNS[1..]
        .iter()
        .all(|name| labels.contains(name))
        .then_some(top)
}

// 每列文字的行距可能与印出的横线不同。用日期与金额两组独立锚点验证笔数。
pub(super) fn rows(
    table: &Table,
    glyphs: &[Glyph],
    header: Option<f64>,
) -> Option<Vec<Vec<String>>> {
    let cells = table
        .iter()
        .find(|r| r.len() == 8 && r.iter().all(Option::is_some))?;
    let edges: Vec<_> = cells.iter().map(|r| r.unwrap()[0]).collect();
    let right = cells.last()?.as_ref()?[2];
    let top = table
        .iter()
        .flat_map(|r| r.iter().flatten())
        .map(|r| r[1])
        .fold(f64::INFINITY, f64::min);
    let bottom = table
        .iter()
        .flat_map(|r| r.iter().flatten())
        .map(|r| r[3])
        .fold(0., f64::max);
    let mut columns = vec![Vec::new(); 8];
    for g in glyphs {
        let x = (g.x0 + g.x1) * 0.5;
        if g.text.trim().is_empty()
            || x < edges[0] - 1.
            || x >= right + 1.
            || g.top < top - 18.
            || g.top > bottom + 18.
        {
            continue;
        }
        let c = edges.partition_point(|left| *left <= x).saturating_sub(1);
        if header.is_some_and(|y| {
            if c == 0 {
                g.top < y - 3.
            } else {
                g.top < y + 5.
            }
        }) {
            continue;
        }
        columns[c].push(g);
    }
    let lines: Vec<Vec<(f64, String)>> = columns
        .into_iter()
        .map(|col| {
            by_y(col, |g| g.top, |g| g.x0, 3.)
                .into_iter()
                .map(|line| (line[0].top, line.iter().map(|g| g.text.as_str()).collect()))
                .collect()
        })
        .collect();
    let dates: Vec<_> = lines[1]
        .iter()
        .filter(|(_, s)| statement_rows::starts_with_date(s))
        .collect();
    let amounts: Vec<_> = lines[5]
        .iter()
        .filter(|(_, s)| s.contains('.') && super::amount_cell(s))
        .collect();
    let n = dates.len();
    if n == 0 || n != amounts.len() {
        return None;
    }
    let mut result = vec![vec![String::new(); 8]; n];
    for (c, lines) in lines.iter().enumerate() {
        if c != 7 && lines.len() == n {
            for (row, (_, s)) in result.iter_mut().zip(lines) {
                row[c] = s.clone();
            }
            continue;
        }
        if c == 0 && lines.len() == 2 * n {
            for (row, parts) in result.iter_mut().zip(lines.chunks_exact(2)) {
                row[c] = format!("{}{}", parts[0].1, parts[1].1);
            }
            continue;
        }
        let anchors = if c <= 1 { &dates } else { &amounts };
        if c != 1 && c != 7 && lines.len() > n && n > 1 {
            if let Some(starts) = wrapped_starts(lines, anchors) {
                for (i, row) in result.iter_mut().enumerate() {
                    let end = starts.get(i + 1).copied().unwrap_or(lines.len());
                    let separator = if c == 0 || c == 7 { "" } else { " " };
                    row[c] = lines[starts[i]..end]
                        .iter()
                        .map(|v| v.1.as_str())
                        .collect::<Vec<_>>()
                        .join(separator);
                }
                continue;
            }
        }
        let offset = lines.first().map_or(0., |v| v.0 - anchors[0].0);
        for (y, s) in lines {
            let i = anchors
                .partition_point(|a| a.0 + offset - 4. <= *y)
                .saturating_sub(1);
            if !result[i][c].is_empty() && c != 0 && c != 7 {
                result[i][c].push(' ');
            }
            result[i][c].push_str(s);
        }
    }
    for (i, row) in result.iter_mut().enumerate() {
        row[5] = amounts[i].1.clone();
    }
    Some(result)
}

// 同一列内的行距稳定，但各列的缩放可能不同。对整页选取单调的起始行，避免逐笔累积偏移。
// “/”是这种支付凭证中的空值标记，独占一笔。
fn wrapped_starts(lines: &[(f64, String)], anchors: &[&(f64, String)]) -> Option<Vec<usize>> {
    let n = anchors.len();
    let m = lines.len();
    let mut best = f64::INFINITY;
    let mut answer = None;
    let span = anchors[n - 1].0 - anchors[0].0;
    if span <= 0. {
        return None;
    }
    for last in m.saturating_sub(6).max(n - 1)..m {
        let scale = (lines[last].0 - lines[0].0) / span;
        let mut prev = vec![f64::INFINITY; m];
        prev[0] = 0.;
        let mut parents = vec![vec![0; m]; n];
        for i in 1..n {
            let expected = lines[0].0 + (anchors[i].0 - anchors[0].0) * scale;
            let mut next = vec![f64::INFINITY; m];
            for j in i..=last - (n - 1 - i) {
                let drift = lines[j].0 - expected;
                if drift.abs() > 35. {
                    continue;
                }
                for k in j.saturating_sub(6)..j {
                    if !prev[k].is_finite() {
                        continue;
                    }
                    let parts = &lines[k..j];
                    let glued = parts.len() > 1 && parts.iter().any(|s| s.1 == "/");
                    let actual = lines[j].0 - lines[k].0;
                    let target = (anchors[i].0 - anchors[i - 1].0) * scale;
                    let cost = prev[k]
                        + drift * drift * 0.2
                        + (actual - target).powi(2)
                        + if glued { 10000. } else { 0. };
                    if cost < next[j] {
                        next[j] = cost;
                        parents[i][j] = k;
                    }
                }
            }
            prev = next;
        }
        let cost = prev[last]
            + if m - last > 1 && lines[last..].iter().any(|s| s.1 == "/") {
                10000.
            } else {
                0.
            };
        if cost < best {
            best = cost;
            let mut starts = vec![0; n];
            starts[n - 1] = last;
            for i in (1..n).rev() {
                starts[i - 1] = parents[i][starts[i]];
            }
            answer = Some(starts);
        }
    }
    answer
}
