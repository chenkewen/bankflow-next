use crate::{
    native_chars::Glyph,
    native_fields::{self, Word},
    statement_grid::record_rows,
};
use pdfium_render::prelude::*;
use regex::Regex;
use std::{collections::BTreeMap, sync::LazyLock};

pub(crate) const KEYWORDS: &[&str] = &[
    "日期",
    "交易日期",
    "记账日期",
    "交易日",
    "记账时间",
    "交易时间",
    "时间",
    "金额",
    "交易金额",
    "借方",
    "贷方",
    "借方金额",
    "贷方金额",
    "收入",
    "支出",
    "借方(出账)",
    "贷方(入账)",
    "余额",
    "本次余额",
    "账户余额",
    "本次金额",
    "摘要",
    "交易摘要",
    "用途",
    "备注",
    "附言",
    "交易附言",
    "对方户名",
    "对方账户",
    "对方账号",
    "对方卡号",
    "对方开户行",
    "对手信息",
    "对于信息",
    "收(付)方",
    "收(付)方名称",
    "收(付)方账号",
    "序号",
    "流水号",
    "企业流水号",
    "日志号",
    "交易流水号",
    "水号",
    "渠道",
    "交易渠道",
    "交易名称",
    "交易介质",
    "凭证种",
    "凭证号",
    "网点名称",
    "交易类型",
    "交易流",
];

pub(crate) fn by_y<'a, T>(
    mut items: Vec<&'a T>,
    y: impl Fn(&T) -> f64,
    x: impl Fn(&T) -> f64,
    tol: f64,
) -> Vec<Vec<&'a T>> {
    items.sort_by(|a, b| y(a).total_cmp(&y(b)));
    let mut groups: Vec<Vec<&T>> = Vec::new();
    for item in items {
        if groups.last().is_none_or(|g| y(item) - y(g[0]) > tol) {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(item);
    }
    for group in &mut groups {
        group.sort_by(|a, b| x(a).total_cmp(&x(b)));
    }
    groups
}

fn english(s: &str) -> bool {
    let s = s.replace(' ', "");
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())
}
fn header_words(glyphs: &[Glyph]) -> Vec<Word> {
    // PDFium 在独立文字对象间生成的零宽空格会拆开紧邻的表头，分词应使用真实字形间距。
    native_fields::words(
        glyphs.iter().filter(|g| !(g.x0 == g.x1 && g.text == " ")),
        f64::INFINITY,
    )
}

fn word_text(words: &[&Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
fn keywords(s: &str) -> usize {
    KEYWORDS.iter().filter(|k| s.contains(**k)).count()
}

fn boundary(lines: &[Vec<&Word>], names: &[String]) -> Option<f64> {
    let header = lines.iter().position(|line| {
        let text = word_text(line);
        names
            .iter()
            .filter(|s| !s.is_empty() && text.contains(s.as_str()))
            .count()
            >= 3
    })?;
    let last = lines[header + 1..]
        .iter()
        .take_while(|line| english(&word_text(line)))
        .last()
        .unwrap_or(&lines[header]);
    Some(last.iter().map(|w| w.bottom).fold(0., f64::max))
}

pub struct Layout {
    pub columns: Vec<String>,
    bounds: Vec<(f64, f64)>,
    base_x: Option<f64>,
    pub top: f64,
    bottom: f64,
    zones: bool,
    fixed_header: bool,
}

impl Layout {
    pub fn detect(glyphs: &[Glyph], width: f64, zones: bool) -> Option<Self> {
        let words = header_words(glyphs);
        let lines = by_y(words.iter().collect(), |w| w.top, |w| w.x, 5.);
        let mut best = None;
        let mut score = 2;
        for (i, row) in lines.iter().enumerate() {
            let s = keywords(&word_text(row));
            if s > score {
                best = Some(i);
                score = s;
            }
        }
        let Some(mut first) = best else {
            return Self::from_transactions(&lines, width, zones);
        };
        // 相邻列没有留白或表头竖排时，按原文字流验证整组字段，再使用各字段的首字位置。
        let y = lines[first][0].top;
        let band: Vec<_> = glyphs
            .iter()
            .filter(|g| (g.top - y).abs() < 27. && !g.text.trim().is_empty())
            .collect();
        let names = [
            "流水号",
            "交易日期",
            "借方金额",
            "贷方金额",
            "余额",
            "对方户名",
            "对方账户",
            "摘要",
            "交易渠道",
            "用途",
            "备注",
        ];
        let stream: String = band.iter().map(|g| g.text.as_str()).collect();
        let label = names.concat();
        if let Some(start) = stream.find(&label) {
            let offset = stream[..start].chars().count();
            if band.iter().all(|g| g.text.chars().count() == 1) {
                let mut pos = offset;
                let mut xs = Vec::new();
                for name in names {
                    xs.push(band[pos].x0);
                    pos += name.chars().count();
                }
                if xs.windows(2).all(|p| p[1] > p[0] + 2.) {
                    let head = &band[offset..pos];
                    return Some(Self {
                        columns: names.map(str::to_owned).to_vec(),
                        bounds: xs
                            .iter()
                            .enumerate()
                            .map(|(i, &x)| (x - 1., xs.get(i + 1).map_or(width, |n| n - 1.)))
                            .collect(),
                        base_x: None,
                        top: head.iter().map(|g| g.top).fold(f64::INFINITY, f64::min),
                        bottom: head.iter().map(|g| g.bottom).fold(0., f64::max),
                        zones,
                        fixed_header: true,
                    });
                }
            }
        }
        let mut end = first;
        let mut xs: Vec<_> = lines[first].iter().map(|w| w.x.round_ties_even()).collect();
        let aligned =
            |w: &Word, xs: &[f64]| xs.iter().any(|x| (x - w.x.round_ties_even()).abs() < 15.);
        let continuation = |row: &[&Word], xs: &[f64]| {
            let text = word_text(row);
            !text.chars().any(|c| c.is_ascii_digit())
                && !text.contains([':', '：'])
                && (keywords(&text) > 0
                    || row.len() <= 5
                        && !text.bytes().any(|b| b.is_ascii_alphanumeric())
                        && row.iter().all(|w| aligned(w, xs)))
        };
        while first > 0
            && lines[first][0].top - lines[first - 1][0].top < 25.
            && continuation(&lines[first - 1], &xs)
        {
            first -= 1;
            xs.extend(lines[first].iter().map(|w| w.x.round_ties_even()));
        }
        while end + 1 < lines.len()
            && lines[end + 1][0].top - lines[end][0].top < 25.
            && continuation(&lines[end + 1], &xs)
        {
            end += 1;
            xs.extend(lines[end].iter().map(|w| w.x.round_ties_even()));
        }
        let mut header: Vec<_> = lines[first..=end].iter().flatten().copied().collect();
        let top = header.iter().map(|w| w.top).fold(f64::INFINITY, f64::min);
        let mut bottom = header.iter().map(|w| w.bottom).fold(0., f64::max);
        for i in end + 1..lines.len() {
            if lines[i][0].top - lines[i - 1][0].top >= 25.
                || !english(&word_text(&lines[i]))
                || !lines[i].iter().any(|w| aligned(w, &xs))
            {
                break;
            }
            bottom = bottom.max(lines[i].iter().map(|w| w.bottom).fold(0., f64::max));
        }
        header.sort_by(|a, b| a.x.total_cmp(&b.x));
        let clusters: Vec<_> = header.chunk_by(|a, b| b.x - a.x < 12.).collect();
        let mut columns = Vec::new();
        let mut bounds = Vec::new();
        for (i, cluster) in clusters.iter().enumerate() {
            let mut v = cluster.to_vec();
            v.sort_by(|a, b| a.top.total_cmp(&b.top));
            let mut names = Vec::new();
            for layer in v.chunk_by(|a, b| b.top - a.top < 5.) {
                let mut layer = layer.to_vec();
                layer.sort_by(|a, b| a.x.total_cmp(&b.x));
                let name: String = layer.iter().map(|w| w.text.as_str()).collect();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            columns.push(native_fields::sanitize(&native_fields::clean_header(
                &names.concat(),
            )));
            bounds.push((
                cluster[0].x - 4.,
                clusters.get(i + 1).map_or(width, |next| next[0].x - 0.5),
            ));
        }
        Some(Self {
            columns,
            bounds,
            base_x: None,
            top,
            bottom,
            zones,
            fixed_header: false,
        })
    }
    fn from_transactions(lines: &[Vec<&Word>], width: f64, zones: bool) -> Option<Self> {
        // 无表头时仅采纳两行以上列位置一致、同时含日期和金额的记录，避免将说明段落分列。
        let records: Vec<_> = lines
            .iter()
            .filter(|line| {
                line.len() >= 2
                    && line
                        .iter()
                        .any(|w| crate::statement_rows::starts_with_date(&w.text))
                    && line.iter().any(|w| super::amount_cell(&w.text))
            })
            .collect();
        let first = *records.first()?;
        if records.len() < 2
            || records.iter().any(|r| {
                r.len() != first.len() || r.iter().zip(first).any(|(a, b)| (a.x - b.x).abs() > 6.)
            })
        {
            return None;
        }
        let bounds = first
            .iter()
            .enumerate()
            .map(|(i, w)| (w.x - 4., first.get(i + 1).map_or(width, |n| n.x - 0.5)))
            .collect();
        let columns = (1..=first.len()).map(|i| format!("列{i}")).collect();
        let top = first.iter().map(|w| w.top).fold(f64::INFINITY, f64::min);
        Some(Self {
            columns,
            bounds,
            top,
            bottom: top - 0.01,
            base_x: None,
            zones,
            fixed_header: false,
        })
    }
    // 部分页在同一份流水中切换为紧凑日期，文字与原先横线不再对齐。
    // 用同一基线上的流水号、日期、借贷金额和余额确认每笔，再按实际文字位置分配附言。
    fn anchored_rows(&self, chars: &[&Glyph]) -> Option<Vec<Vec<String>>> {
        static AMOUNTS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^([+-]?[\d,]+\.\d{2})([+-]?[\d,]+\.\d{2})([+-]?[\d,]+\.\d{2})").unwrap()
        });
        static DATE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^20\d{2}-?(?:0[1-9]|1[0-2])-?(?:0[1-9]|[12]\d|3[01])$").unwrap()
        });
        let left = self.bounds[0].0;
        let id_glyphs = chars
            .iter()
            .copied()
            .filter(|g| g.x0 >= left - 15. && g.x0 < self.bounds[1].0 - 1.)
            .collect();
        let mut anchors = Vec::new();
        for line in by_y(id_glyphs, |g| g.top, |g| g.x0, 2.) {
            let id: String = line.iter().map(|g| g.text.as_str()).collect();
            // 整页可能水平平移，使流水号的尾字落到上一版式的日期栏；按首字重新取完整的行。
            if id.len() < 4 || !id.bytes().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let x = line[0].x0;
            let y = line[0].top;
            let mut numeric: Vec<_> = chars
                .iter()
                .copied()
                .filter(|g| {
                    (g.top - y).abs() <= 2.5 && g.x0 >= x - 0.5 && g.x0 < self.bounds[5].0 + 25.
                })
                .collect();
            numeric.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            let id: String = numeric.iter().take(9).map(|g| g.text.as_str()).collect();
            if id.len() != 9 || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let date_x = self.bounds[1].0 - left + x;
            let date_chars = chars
                .iter()
                .copied()
                .filter(|g| (g.top - y).abs() <= 12. && g.x0 >= date_x - 3. && g.x0 < date_x + 31.)
                .collect();
            let day: String = by_y(date_chars, |g| g.top, |g| g.x0, 2.)
                .into_iter()
                .flatten()
                .map(|g| g.text.as_str())
                .collect();
            if !DATE.is_match(&day) {
                return None;
            }
            let amounts: String = numeric
                .iter()
                .filter(|g| g.x0 >= date_x + 31.)
                .map(|g| g.text.as_str())
                .collect();
            let capture = AMOUNTS.captures(&amounts)?;
            anchors.push((
                y,
                x,
                vec![
                    id,
                    day,
                    capture[1].to_owned(),
                    capture[2].to_owned(),
                    capture[3].to_owned(),
                ],
            ));
        }
        if anchors.is_empty() {
            return None;
        }
        // 同一页的不同段可能整体水平平移，列位置以每笔流水号为原点。
        let mut groups = vec![Vec::new(); anchors.len()];
        let cuts: Vec<_> = anchors
            .windows(2)
            .map(|pair| (pair[0].0 + pair[1].0) / 2.)
            .collect();
        for line in by_y(chars.to_vec(), |g| g.top, |g| g.x0, 3.) {
            if skip_line(&line) {
                continue;
            }
            let i = cuts.partition_point(|y| *y < line[0].top);
            let shift = anchors[i].1 - left - 1.;
            groups[i].extend(
                line.iter()
                    .map(|g| (g.x0 - shift, g.x1 - shift, g.top, g.text.clone())),
            );
        }
        let mut rows = record_rows(&self.bounds, groups);
        for (r, (_, _, values)) in rows.iter_mut().zip(anchors) {
            r[..5].clone_from_slice(&values);
            for v in r.iter_mut() {
                *v = native_fields::sanitize(v);
            }
        }
        Some(rows)
    }

    pub fn rows(&mut self, glyphs: &[Glyph], first: bool, lines: &[f64]) -> Vec<Vec<String>> {
        let words = header_words(glyphs);
        let headers = by_y(words.iter().collect(), |w| w.top, |w| w.x, 5.);
        let cutoff = boundary(&headers, &self.columns)
            .unwrap_or(0.)
            .max(if first { self.bottom } else { 0. });
        let chars: Vec<_> = glyphs
            .iter()
            .filter(|g| g.top > cutoff && !g.text.trim().is_empty())
            .collect();
        let anchored = if self.fixed_header {
            self.anchored_rows(&chars)
        } else {
            None
        };
        let mut starts: BTreeMap<i64, (usize, f64)> = BTreeMap::new();
        for &g in &chars {
            let e = starts
                .entry(g.top.round_ties_even() as i64)
                .or_insert((0, g.x0));
            e.0 += 1;
            e.1 = e.1.min(g.x0);
        }
        let max = starts.values().map(|e| e.0).max().unwrap_or(0);
        let mut xs: Vec<_> = starts
            .values()
            .filter(|e| max >= 10 && e.0 * 2 >= max)
            .map(|e| e.1)
            .collect();
        xs.sort_by(f64::total_cmp);
        let x = xs.get(xs.len() / 2).copied();
        if first {
            self.base_x = x
        }
        let shift = x.zip(self.base_x).map_or(0., |(x, base)| x - base);
        let bounds = self
            .bounds
            .iter()
            .map(|&(a, b)| {
                if !self.fixed_header && shift.abs() >= 2. {
                    (a + shift, b + shift)
                } else {
                    (a, b)
                }
            })
            .collect::<Vec<_>>();
        let groups = if self.zones {
            let mut separators: Vec<_> = lines
                .iter()
                .filter(|y| **y > cutoff)
                .map(|y| (y * 10.).round_ties_even() / 10.)
                .collect();
            separators.dedup();
            let mut buckets = vec![vec![]; separators.len() + 1];
            for g in chars {
                buckets[separators.partition_point(|y| *y < g.top)].push(g);
            }
            buckets
                .into_iter()
                .map(|v| {
                    by_y(v, |g| g.top, |g| g.x0, 4.)
                        .into_iter()
                        .filter(|line| !skip_line(line))
                        .flatten()
                        .collect()
                })
                .collect()
        } else {
            text_groups(chars)
        };
        let groups = groups
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|g| {
                        (
                            g.x0,
                            g.x1,
                            (g.top * 10.).round_ties_even() / 10.,
                            g.text.clone(),
                        )
                    })
                    .collect()
            })
            .collect();
        let conventional: Vec<_> = record_rows(&bounds, groups)
            .into_iter()
            .map(|row| {
                row.iter()
                    .map(|s| native_fields::sanitize(s))
                    .collect::<Vec<_>>()
            })
            .filter(|row| !native_fields::skip(row))
            .collect();
        if let Some(anchored) = anchored {
            if !anchored
                .iter()
                .map(|r| &r[..5])
                .eq(conventional.iter().map(|r| &r[..5]))
            {
                return anchored;
            }
        }
        conventional
    }
}
fn skip_line(row: &[&Glyph]) -> bool {
    native_fields::skip(&[row.iter().map(|g| g.text.as_str()).collect()])
}

pub(crate) fn gap_limit(gaps: &[f64]) -> Option<f64> {
    let mut v = gaps.to_vec();
    v.sort_by(f64::total_cmp);
    if v.is_empty() || v[0] <= 0. || v.last()? / v[0] <= 1.25 {
        return None;
    }
    for x in &mut v {
        *x = (*x * 10.).round_ties_even() / 10.;
    }
    v.dedup();
    let mut step = 0.;
    let mut threshold = None;
    for pair in v.windows(2) {
        let jump = pair[1] - pair[0];
        if jump > step {
            step = jump;
            threshold = (pair[1] / pair[0] >= 1.3).then_some((pair[0] + pair[1]) / 2.);
        }
    }
    threshold
}

fn text_groups(chars: Vec<&Glyph>) -> Vec<Vec<&Glyph>> {
    static DATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\d{4}(?:[-‑–]\d{1,2}[-‑–]\d{1,2}|/\d{1,2}/\d{1,2}|\.\d{1,2}\.\d{1,2}|(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])(?:\D|$)|年\d{1,2}月\d{1,2}日)").unwrap()
    });
    let rows: Vec<_> = by_y(chars, |g| g.top, |g| g.x0, 6.)
        .into_iter()
        .filter(|r| {
            !skip_line(r) && !english(&r.iter().map(|g| g.text.as_str()).collect::<String>())
        })
        .collect();
    let tops: Vec<_> = rows
        .iter()
        .map(|row| row.iter().map(|g| g.top).fold(f64::INFINITY, f64::min))
        .collect();
    let gaps: Vec<_> = tops.windows(2).map(|p| p[1] - p[0]).collect();
    let Some(threshold) = gap_limit(&gaps) else {
        return rows;
    };
    let mut groups: Vec<Vec<&Glyph>> = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i == 0
            || gaps[i - 1] > threshold
            || DATE.is_match(&row.iter().map(|g| g.text.as_str()).collect::<String>())
        {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().extend(row);
    }
    groups
}

pub fn horizontal_lines(page: &PdfPage) -> Result<Vec<f64>, PdfiumError> {
    fn visit(
        o: PdfPageObject,
        parent: PdfMatrix,
        page: &PdfPage,
        lines: &mut Vec<f64>,
    ) -> Result<(), PdfiumError> {
        match o {
            PdfPageObject::XObjectForm(form) => {
                let m = form.matrix()?.multiply(parent);
                for child in form.iter() {
                    visit(child, m, page, lines)?
                }
            }
            PdfPageObject::Path(path) if path.segments().len() == 2 => {
                let m = path.matrix()?.multiply(parent);
                let seg = path.segments().transform(m);
                let a = seg.get(0)?.point();
                let b = seg.get(1)?.point();
                if (a.1.value - b.1.value).abs() < 1.5
                    && (a.0.value - b.0.value).abs() > page.width().value * 0.25
                {
                    lines.push(f64::from(page.height().value - a.1.value.max(b.1.value)));
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut lines = Vec::new();
    for o in page.objects().iter() {
        visit(o, PdfMatrix::IDENTITY, page, &mut lines)?;
    }
    lines.sort_by(f64::total_cmp);
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, x0: f64, x1: f64, top: f64) -> Glyph {
        Glyph {
            text: text.into(),
            x0,
            x1,
            top,
            bottom: top + 10.,
        }
    }

    #[test]
    fn repeatable_dated_lines_can_supply_columns_without_a_header() {
        let glyphs = [
            glyph("20260102", 10., 45., 20.),
            glyph("10.00", 100., 125., 20.),
            glyph("20260103", 10., 45., 45.),
            glyph("12.00", 100., 125., 45.),
        ];
        let mut layout = Layout::detect(&glyphs, 200., false).unwrap();
        assert_eq!(layout.columns, ["列1", "列2"]);
        let rows = layout.rows(&glyphs, true, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ["20260102", "10.00"]);
    }

    #[test]
    fn double_header_uses_glyph_gaps_and_keeps_real_spaces() {
        let glyphs = vec![
            glyph("交易日期", 10., 50., 20.),
            glyph(" ", 50., 50., 20.),
            glyph("借方金额", 53., 93., 20.),
            glyph("交易摘要", 110., 150., 20.),
            glyph("账户", 200., 220., 20.),
            glyph("余额", 200., 220., 32.),
        ];
        let layout = Layout::detect(&glyphs, 300., true).unwrap();
        assert_eq!(layout.columns, ["交易日期借方金额", "交易摘要", "账户余额"]);
        let mut spaced = glyphs;
        spaced[1].x1 += 1.;
        assert_eq!(header_words(&spaced).len(), 5);
    }
}
