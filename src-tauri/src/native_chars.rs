use pdfium_render::prelude::*;
use std::{collections::HashMap, error::Error};

#[derive(Clone, Debug)]
#[cfg_attr(any(debug_assertions, test), derive(serde::Deserialize, serde::Serialize))]
pub struct Glyph {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
}

pub fn glyphs(page: &PdfPage) -> Result<(Vec<Glyph>, Vec<Glyph>), Box<dyn Error>> {
    let text = page.text()?;
    let chars = text.chars();
    let mut out: Vec<Glyph> = Vec::with_capacity(chars.len());
    let mut small = Vec::new();
    let mut preceding_baseline = None;
    let mut descents = HashMap::new();
    let page_height = f64::from(page.height().value);
    for ch in chars.iter() {
        if ch.is_generated()? {
            // PDFium 把部分源字体中的真实空格标为生成字符，以同一基线恢复分词。
            if ch.unicode_char() == Some(' ') {
                let (_, y) = ch.origin()?;
                if preceding_baseline == Some(y.value) {
                    if let Some(last) = out.last() {
                        out.push(Glyph {
                            text: String::from(" "),
                            x0: last.x1,
                            x1: last.x1,
                            top: last.top,
                            bottom: last.bottom,
                        });
                    }
                }
            }
            continue;
        }
        preceding_baseline = None;
        let matrix = ch.matrix()?;
        if matrix.b().abs() > 0.01 || matrix.c().abs() > 0.01 {
            continue;
        }
        let size = f64::from(ch.unscaled_font_size().value * matrix.d().abs());
        if !(2.0..=80.0).contains(&size) {
            continue;
        }
        let object = ch.text_object()?;
        // 扫描件可能叠有仅供搜索的隐藏 OCR 层。只使用实际会绘制的填充或描边。
        use PdfPageTextRenderMode::*;
        let visible = |color: PdfColor| {
            let (r, g, b) = (color.red(), color.green(), color.blue());
            color.alpha() > 0 && !(r > 183 && g > 183 && b > 183) && !(r > 178 && g < 77 && b < 77)
        };
        let visible = match object.render_mode() {
            Invisible | InvisibleClipping | Unknown => false,
            FilledUnstroked | FilledUnstrokedClipping => visible(ch.fill_color()?),
            StrokedUnfilled | StrokedUnfilledClipping => visible(ch.stroke_color()?),
            FilledThenStroked | FilledThenStrokedClipping => {
                visible(ch.fill_color()?) || visible(ch.stroke_color()?)
            }
        };
        if !visible {
            continue;
        }
        let Some(value) = ch.unicode_char() else {
            return Err("PDF 字符缺少 Unicode 映射".into());
        };
        let font = object.font();
        let key = font.name();
        let descent = *descents.entry(key.clone()).or_insert_with(|| {
            if key.is_empty() {
                0.0
            } else {
                font.descent(PdfPoints::new(1.0))
                    .map(|d| f64::from(d.value))
                    .unwrap_or(0.0)
            }
        });
        let (x, y) = ch.origin()?;
        preceding_baseline = (size >= 4.0).then_some(y.value);
        let bbox = ch.loose_bounds()?;
        let bottom = page_height - f64::from(y.value) - size * descent;
        if !(bottom.is_finite() && x.value.is_finite() && bbox.right().value.is_finite()) {
            return Err("PDF 字符坐标无效".into());
        }
        let output = if size < 4.0 { &mut small } else { &mut out };
        output.push(Glyph {
            text: if value == '\u{2}' && ch.is_hyphen()? {
                "-".into()
            } else {
                value.to_string()
            },
            x0: f64::from(x.value),
            x1: f64::from(bbox.right().value),
            top: bottom - size,
            bottom,
        });
    }
    Ok((out, small))
}

fn cluster_lines<'a>(mut chars: Vec<&'a Glyph>) -> Vec<Vec<&'a Glyph>> {
    chars.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut lines: Vec<Vec<&Glyph>> = Vec::new();
    let mut previous = f64::NEG_INFINITY;
    for g in chars {
        if g.top - previous > 3.0 {
            lines.push(Vec::new());
        }
        lines.last_mut().unwrap().push(g);
        previous = g.top;
    }
    lines
}

fn compose(chars: Vec<&Glyph>) -> String {
    let mut words = Vec::new();
    for mut line in cluster_lines(chars) {
        line.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut word = String::new();
        let mut top = f64::INFINITY;
        let mut prev: Option<&Glyph> = None;
        for g in line {
            let blank = g.text.chars().all(char::is_whitespace);
            if blank || prev.is_some_and(|p| g.x0 > p.x1 + 3.0 || (g.top - p.top).abs() > 3.0) {
                if !word.is_empty() {
                    words.push((top, std::mem::take(&mut word)));
                    top = f64::INFINITY;
                }
            }
            if !blank {
                word.push_str(match g.text.as_str() {
                    "ﬀ" => "ff",
                    "ﬁ" => "fi",
                    "ﬂ" => "fl",
                    "ﬃ" => "ffi",
                    "ﬄ" => "ffl",
                    "ﬅ" | "ﬆ" => "st",
                    t => t,
                });
                top = top.min(g.top);
            }
            prev = Some(g);
        }
        if !word.is_empty() {
            words.push((top, word));
        }
    }
    // 按词的最高点再次聚行，保留同行词第一次生成时的顺序。
    let mut indices: Vec<usize> = (0..words.len()).collect();
    indices.sort_by(|&a, &b| words[a].0.total_cmp(&words[b].0));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut last = f64::NEG_INFINITY;
    for i in indices {
        if words[i].0 - last > 3.0 {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(i);
        last = words[i].0;
    }
    groups
        .iter_mut()
        .map(|group| {
            group.sort_unstable();
            group
                .iter()
                .map(|&i| words[i].1.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn cells(chars: &[Glyph], tables: &[Vec<Vec<Option<[f64; 4]>>>]) -> Vec<Vec<Vec<String>>> {
    let mut order: Vec<_> = (0..chars.len()).collect();
    let center_y = |i: usize| (chars[i].top + chars[i].bottom) / 2.0;
    order.sort_by(|&a, &b| center_y(a).total_cmp(&center_y(b)));
    tables
        .iter()
        .map(|table| {
            table
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| {
                            let Some([left, top, right, bottom]) = cell else {
                                return String::new();
                            };
                            let start = order.partition_point(|&i| center_y(i) < *top);
                            let end = order.partition_point(|&i| center_y(i) < *bottom);
                            let mut selected: Vec<_> = order[start..end]
                                .iter()
                                .copied()
                                .filter(|&i| {
                                    let mut owner = &chars[i];
                                    if i > 0 && owner.x0 == owner.x1 && owner.text == " " {
                                        // PDFium 的零宽分隔符属于前一字形，字形恰在列边缘时不能把空格推入相邻格。
                                        let before = &chars[i - 1];
                                        if before.x1 == owner.x0
                                            && before.top == owner.top
                                            && before.bottom == owner.bottom
                                        {
                                            owner = before;
                                        }
                                    }
                                    let x = (owner.x0 + owner.x1) / 2.0;
                                    *left <= x && x < *right
                                })
                                .collect();
                            selected.sort_unstable();
                            compose(selected.into_iter().map(|i| &chars[i]).collect())
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

// 下调字号门槛仅用于有日期的已定位网格数据行。小字必须完全落在一个空单元格内。
pub(super) fn recover_small_cells(
    row: &mut [String],
    geometry: &[Option<[f64; 4]>],
    small: &[Glyph],
) {
    if small.is_empty()
        || !row
            .iter()
            .any(|v| crate::statement_rows::starts_with_date(v))
    {
        return;
    }
    for (cell, rect) in row.iter_mut().zip(geometry) {
        let Some([x0, y0, x1, y1]) = *rect else {
            continue;
        };
        if !cell.is_empty() {
            continue;
        }
        let inside = small
            .iter()
            .filter(|g| g.x0 >= x0 && g.x1 <= x1 && g.top >= y0 && g.bottom <= y1)
            .collect::<Vec<_>>();
        if inside.is_empty() {
            continue;
        }
        let value = crate::native_fields::sanitize(&compose(inside));
        if !value.is_empty() && !crate::native_fields::skip_text(&value, true) {
            *cell = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, x: f64, y: f64) -> Glyph {
        Glyph {
            text: text.into(),
            x0: x,
            x1: x + 3.0,
            top: y,
            bottom: y + 5.0,
        }
    }

    #[test]
    fn preserves_line_breaks_and_explicit_spaces() {
        let chars = [
            glyph("T", 10.0, 8.0),
            glyph("A", 18.0, 8.0),
            glyph("ﬁ", 10.0, 0.0),
            glyph(" ", 14.0, 0.0),
            glyph("B", 15.0, 0.0),
        ];
        let tables = vec![vec![vec![Some([0.0, 0.0, 30.0, 20.0])]]];
        assert_eq!(cells(&chars, &tables)[0][0][0], "fi B\nT A");
    }

    #[test]
    fn synthetic_space_on_border_belongs_to_preceding_cell() {
        let chars = [
            glyph("甲", 10., 0.),
            glyph("乙", 13., 0.),
            glyph("丙", 10., 5.),
            glyph("丁", 13., 5.),
            glyph("邻", 7., 2.5),
            Glyph {
                text: " ".into(),
                x0: 10.,
                x1: 10.,
                top: 2.5,
                bottom: 7.5,
            },
        ];
        let tables = vec![vec![vec![
            Some([0., 0., 10., 12.]),
            Some([10., 0., 30., 12.]),
        ]]];
        assert_eq!(cells(&chars, &tables)[0][0], ["邻", "甲乙\n丙丁"]);
    }

    #[test]
    fn small_glyphs_require_empty_transaction_cell_and_full_containment() {
        let geometry = [
            Some([0., 0., 20., 8.]),
            Some([20., 0., 40., 8.]),
            Some([40., 0., 60., 8.]),
        ];
        let small = [
            glyph("甲", 21., 1.),
            glyph("乙", 24., 1.),
            glyph("注", 38., 1.),
            glyph("1/9", 42., 1.),
        ];
        let mut row = ["2026-11-24".into(), String::new(), String::new()];
        recover_small_cells(&mut row, &geometry, &small);
        assert_eq!(row, ["2026-11-24", "甲乙", ""]);
        let mut footer = ["页脚".into(), String::new(), String::new()];
        recover_small_cells(&mut footer, &geometry, &small);
        assert_eq!(footer, ["页脚", "", ""]);
    }

    #[test]
    fn boundaries_match_centers_and_exclude_right_edge() {
        let chars = [
            glyph("甲", 0.5, 5.0),
            glyph("右", 8.5, 0.0),
            glyph("乙", -0.5, 0.0),
            glyph("下", 2.5, 7.5),
            glyph("外", 18.5, 0.0),
        ];
        let tables = vec![vec![vec![
            Some([0.0, 0.0, 10.0, 10.0]),
            None,
            Some([10.0, 0.0, 20.0, 10.0]),
        ]]];
        assert_eq!(cells(&chars, &tables)[0][0], ["乙\n甲", "", "右"]);
    }
}
