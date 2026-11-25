use pdfium_render::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub type Tables = Vec<Vec<Vec<Option<[f64; 4]>>>>;

#[derive(Clone, Copy)]
struct Edge {
    pos: f64,
    start: f64,
    end: f64,
}

fn add_line(a: [f64; 2], b: [f64; 2], h: &mut Vec<Edge>, v: &mut Vec<Edge>) {
    let edge = |pos: f64, start: f64, end: f64| Edge {
        pos,
        start: start.min(end),
        end: start.max(end),
    };
    if (a[1] - b[1]).abs() < 0.001 && (a[0] - b[0]).abs() >= 1.0 {
        h.push(edge(a[1], a[0], b[0]));
    }
    if (a[0] - b[0]).abs() < 0.001 && (a[1] - b[1]).abs() >= 1.0 {
        v.push(edge(a[0], a[1], b[1]));
    }
}

fn collect(
    object: PdfPageObject,
    parent: PdfMatrix,
    height: f64,
    h: &mut Vec<Edge>,
    v: &mut Vec<Edge>,
) -> Result<(), PdfiumError> {
    match object {
        PdfPageObject::XObjectForm(form) => {
            let matrix = form.matrix()?.multiply(parent);
            for child in form.iter() {
                collect(child, matrix, height, h, v)?;
            }
        }
        PdfPageObject::Path(path) => {
            let transform = path.matrix()?.multiply(parent);
            let mut start = [0.; 2];
            let mut previous = [0.; 2];
            for segment in path.segments().transform(transform).iter() {
                let (x, y) = segment.point();
                let point = [f64::from(x.value), height - f64::from(y.value)];
                match segment.segment_type() {
                    PdfPathSegmentType::MoveTo => start = point,
                    PdfPathSegmentType::LineTo => add_line(previous, point, h, v),
                    _ => {}
                }
                if segment.is_close() {
                    add_line(point, start, h, v);
                }
                previous = point;
            }
        }
        _ => {}
    }
    Ok(())
}

// 相邻 4 点以内的坐标吸附到均值，3 点以内的断线合并，保留至少 15 点长的边。
fn axes(mut edges: Vec<Edge>) -> Vec<(f64, Vec<(f64, f64)>)> {
    edges.sort_unstable_by(|a, b| a.pos.total_cmp(&b.pos));
    let mut output = Vec::new();
    let mut i = 0;
    while i < edges.len() {
        let mut end = i + 1;
        while end < edges.len() && edges[end].pos - edges[end - 1].pos <= 4.0 {
            end += 1;
        }
        let group = &mut edges[i..end];
        let pos = group.iter().map(|e| e.pos).sum::<f64>() / group.len() as f64;
        group.sort_unstable_by(|a, b| a.start.total_cmp(&b.start));
        let mut spans: Vec<(f64, f64)> = Vec::new();
        for e in group {
            if let Some(last) = spans.last_mut() {
                if e.start <= last.1 + 3.0 {
                    last.1 = last.1.max(e.end);
                    continue;
                }
            }
            spans.push((e.start, e.end));
        }
        spans.retain(|(a, b)| b - a >= 15.0);
        if !spans.is_empty() {
            output.push((pos, spans));
        }
        i = end;
    }
    output
}

fn covers(spans: &[(f64, f64)], a: f64, b: f64) -> bool {
    spans
        .iter()
        .any(|(start, end)| *start <= a + 3. && *end >= b - 3.)
}

type Cell = [usize; 4];
fn cells(x: &[(f64, Vec<(f64, f64)>)], y: &[(f64, Vec<(f64, f64)>)]) -> Vec<Cell> {
    let mut result = Vec::new();
    for (left, (x0, vertical)) in x.iter().enumerate() {
        for (top, (y0, horizontal)) in y.iter().enumerate() {
            if !covers(vertical, *y0, *y0) || !covers(horizontal, *x0, *x0) {
                continue;
            }
            'below: for (bottom, (y1, lower)) in y.iter().enumerate().skip(top + 1) {
                if !covers(vertical, *y0, *y1) {
                    break;
                }
                if !covers(lower, *x0, *x0) {
                    continue;
                }
                for (right, (x1, side)) in x.iter().enumerate().skip(left + 1) {
                    if !covers(horizontal, *x0, *x1) {
                        break;
                    }
                    if covers(side, *y0, *y1) && covers(lower, *x0, *x1) {
                        result.push([left, top, right, bottom]);
                        break 'below;
                    }
                }
            }
        }
    }
    result
}

pub fn tables(page: &PdfPage) -> Result<Tables, Box<dyn std::error::Error>> {
    let mut horizontal = Vec::new();
    let mut vertical = Vec::new();
    for object in page.objects().iter() {
        collect(
            object,
            PdfMatrix::IDENTITY,
            f64::from(page.height().value),
            &mut horizontal,
            &mut vertical,
        )?;
    }
    let xs = axes(vertical);
    let ys = axes(horizontal);
    let cells = cells(&xs, &ys);
    let mut corners: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (i, &[l, t, r, b]) in cells.iter().enumerate() {
        for p in [(l, t), (r, t), (l, b), (r, b)] {
            corners.entry(p).or_default().push(i);
        }
    }
    let mut visited = vec![false; cells.len()];
    let mut tables = Vec::new();
    for i in 0..cells.len() {
        if visited[i] {
            continue;
        }
        visited[i] = true;
        let mut stack = vec![i];
        let mut rows: BTreeMap<usize, BTreeMap<usize, [f64; 4]>> = BTreeMap::new();
        let mut cols = BTreeSet::new();
        let mut count = 0;
        while let Some(k) = stack.pop() {
            let [l, t, r, b] = cells[k];
            rows.entry(t)
                .or_default()
                .insert(l, [xs[l].0, ys[t].0, xs[r].0, ys[b].0]);
            cols.insert(l);
            count += 1;
            for p in [(l, t), (r, t), (l, b), (r, b)] {
                if let Some(neighbors) = corners.remove(&p) {
                    for neighbor in neighbors {
                        if !visited[neighbor] {
                            visited[neighbor] = true;
                            stack.push(neighbor);
                        }
                    }
                }
            }
        }
        if count > 1 {
            let (&top, first) = rows.first_key_value().unwrap();
            let left = *first.first_key_value().unwrap().0;
            let data = rows
                .into_values()
                .map(|row| cols.iter().map(|x| row.get(x).copied()).collect())
                .collect();
            tables.push(((top, left), data));
        }
    }
    tables.sort_by_key(|(key, _)| *key);
    Ok(tables.into_iter().map(|(_, data)| data).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_column_creates_a_merged_cell() {
        let columns = vec![
            (0., vec![(0., 60.)]),
            (40., vec![(30., 60.)]),
            (80., vec![(0., 60.)]),
        ];
        let rows = vec![
            (0., vec![(0., 80.)]),
            (30., vec![(0., 80.)]),
            (60., vec![(0., 80.)]),
        ];
        assert_eq!(
            cells(&columns, &rows),
            [[0, 0, 2, 1], [0, 1, 1, 2], [1, 1, 2, 2]]
        );
    }
}
