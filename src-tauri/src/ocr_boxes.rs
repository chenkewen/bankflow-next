use geo_clipper::{ClipperInt, EndType, JoinType};
use geo_types::{LineString, Polygon};
use std::collections::VecDeque;

pub type Quad = [[f32; 2]; 4];
type Point = [f32; 2];
fn cross(a: Point, b: Point, c: Point) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn hull(mut points: Vec<Point>) -> Vec<Point> {
    if points.len() <= 4 {
        return points;
    }
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    let mut chain = Vec::new();
    for &p in &points {
        while chain.len() >= 2 && cross(chain[chain.len() - 2], chain[chain.len() - 1], p) <= 0. {
            chain.pop();
        }
        chain.push(p);
    }
    let lower = chain.len();
    for &p in points.iter().rev().skip(1) {
        while chain.len() > lower && cross(chain[chain.len() - 2], chain[chain.len() - 1], p) <= 0.
        {
            chain.pop();
        }
        chain.push(p);
    }
    chain.pop();
    chain
}

fn ordered(mut points: Quad) -> Quad {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]));
    if points[0][1] > points[1][1] {
        points.swap(0, 1);
    }
    if points[2][1] > points[3][1] {
        points.swap(2, 3);
    }
    [points[0], points[2], points[3], points[1]]
}

fn rectangle(points: Vec<Point>) -> Option<(Quad, f32)> {
    let hull = hull(points);
    if hull.len() < 3 {
        return None;
    }
    let mut best = f32::INFINITY;
    let mut result = None;
    for i in 0..hull.len() {
        let a = hull[i];
        let b = hull[(i + 1) % hull.len()];
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let len = dx.hypot(dy);
        if len < 1e-6 {
            continue;
        }
        let (c, s) = (dx / len, dy / len);
        let (mut xmin, mut xmax, mut ymin, mut ymax) = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        for &[x, y] in &hull {
            let px = c * x + s * y;
            let py = -s * x + c * y;
            xmin = xmin.min(px);
            xmax = xmax.max(px);
            ymin = ymin.min(py);
            ymax = ymax.max(py);
        }
        let (w, h) = (xmax - xmin, ymax - ymin);
        if w * h >= best {
            continue;
        }
        best = w * h;
        let (cxp, cyp) = (f64::from(xmin + xmax) * 0.5, f64::from(ymin + ymax) * 0.5);
        let (cx, cy) = (
            (f64::from(c) * cxp - f64::from(s) * cyp) as f32,
            (f64::from(s) * cxp + f64::from(c) * cyp) as f32,
        );
        let angle = f64::from(s).atan2(f64::from(c)).to_degrees().to_radians();
        let (s, c) = (angle.sin() as f32, angle.cos() as f32);
        let quad = [
            [-w / 2., -h / 2.],
            [w / 2., -h / 2.],
            [w / 2., h / 2.],
            [-w / 2., h / 2.],
        ]
        .map(|[x, y]| [x * c - y * s + cx, x * s + y * c + cy]);
        result = Some((ordered(quad), w.min(h)));
    }
    result
}

fn expand(quad: Quad) -> Option<(Quad, f32)> {
    let mut area = 0.;
    let mut length = 0.;
    for i in 0..4 {
        let [x, y] = quad[i].map(f64::from);
        let [u, v] = quad[(i + 1) % 4].map(f64::from);
        area += x * v - y * u;
        length += (x - u).hypot(y - v);
    }
    let ring: LineString<i64> = quad.iter().map(|p| (p[0] as i64, p[1] as i64)).collect();
    let polygons = Polygon::new(ring, vec![]).offset(
        area.abs() * 0.75 / length.max(1e-6),
        JoinType::Round(0.25),
        EndType::ClosedPolygon,
    );
    let mut points: Vec<_> = polygons
        .0
        .first()?
        .exterior()
        .0
        .iter()
        .map(|p| [p.x as f32, p.y as f32])
        .collect();
    if points.last() == points.first() {
        points.pop();
    }
    rectangle(points)
}

fn score(map: &[f32], width: usize, height: usize, quad: Quad) -> f64 {
    let p = quad.map(|[x, y]| [x as i32, y as i32]);
    let ymin = p.iter().map(|v| v[1]).min().unwrap().max(0);
    let ymax = p.iter().map(|v| v[1]).max().unwrap().min(height as i32 - 1);
    let mut total = 0.;
    let mut n = 0;
    for y in ymin..=ymax {
        let (mut left, mut right) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..4 {
            let [x1, y1] = p[i];
            let [x2, y2] = p[(i + 1) % 4];
            if y < y1.min(y2) || y > y1.max(y2) {
                continue;
            }
            if y1 == y2 {
                left = left.min(x1.min(x2) as f64);
                right = right.max(x1.max(x2) as f64);
            } else {
                let x = x1 as f64 + ((y - y1) as f64) * ((x2 - x1) as f64) / ((y2 - y1) as f64);
                left = left.min(x);
                right = right.max(x);
            }
        }
        for x in (left.round() as i32).max(0)..=(right.round() as i32).min(width as i32 - 1) {
            total += f64::from(map[y as usize * width + x as usize]);
            n += 1;
        }
    }
    total / f64::from(n.max(1))
}

pub fn detect(map: &[f32], w: usize, h: usize, dw: u32, dh: u32) -> Vec<Quad> {
    let mut mask = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            mask[y * w + x] = [
                map[y * w + x],
                map[y * w + (x + 1).min(w - 1)],
                map[(y + 1).min(h - 1) * w + x],
                map[(y + 1).min(h - 1) * w + (x + 1).min(w - 1)],
            ]
            .iter()
            .any(|p| *p > 0.3);
        }
    }
    let mut count = 0;
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    let mut pts = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] {
            continue;
        }
        if count >= 1000 {
            break;
        }
        count += 1;
        pts.clear();
        queue.push_back(start);
        mask[start] = false;
        while let Some(k) = queue.pop_front() {
            let (y, x) = (k / w, k % w);
            pts.push([x as f32, y as f32]);
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let i = yy * w + xx;
                    if mask[i] {
                        mask[i] = false;
                        queue.push_back(i);
                    }
                }
            }
        }
        let Some((rect, side)) = rectangle(pts.clone()) else {
            continue;
        };
        if side < 3. || score(map, w, h, rect) < 0.45 {
            continue;
        }
        let Some((rect, side)) = expand(rect) else {
            continue;
        };
        if side < 5. {
            continue;
        }
        let q = ordered(rect.map(|[x, y]| {
            [
                (x / w as f32 * dw as f32)
                    .round_ties_even()
                    .clamp(0., (dw - 1) as f32),
                (y / h as f32 * dh as f32)
                    .round_ties_even()
                    .clamp(0., (dh - 1) as f32),
            ]
        }));
        let dx = q[1][0] - q[0][0];
        let dy = q[1][1] - q[0][1];
        let width = dx.hypot(dy);
        let height = (q[3][0] - q[0][0]).hypot(q[3][1] - q[0][1]);
        if width as u32 <= 3
            || height as u32 <= 3
            || (width > height * 1.35 && dy.abs() > dx.abs() * 0.203)
        {
            continue;
        }
        out.push(q);
    }
    out.sort_by(|a, b| {
        a[0][1]
            .total_cmp(&b[0][1])
            .then(a[0][0].total_cmp(&b[0][0]))
    });
    for i in 1..out.len() {
        for j in (1..=i).rev() {
            if (out[j][0][1] - out[j - 1][0][1]).abs() < 10. && out[j][0][0] < out[j - 1][0][0] {
                out.swap(j, j - 1);
            } else {
                break;
            }
        }
    }
    out
}
