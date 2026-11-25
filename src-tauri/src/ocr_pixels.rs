use crate::ocr_boxes::Quad;
use image::{Rgb, RgbImage};

// OCR 模型对细笔画敏感。分两遍做带抗锯齿的双线性缩放，每遍按原预处理舍入到 8 位。
fn coefficients(source: u32, target: u32) -> Vec<(u32, Vec<i32>)> {
    let scale = f64::from(source) / f64::from(target);
    let support = scale.max(1.);
    (0..target)
        .map(|i| {
            let center = (f64::from(i) + 0.5) * scale;
            let start = ((center - support + 0.5) as i64).max(0) as u32;
            let end = ((center + support + 0.5) as u32).min(source);
            let mut weights: Vec<_> = (start..end)
                .map(|j| (1. - ((f64::from(j) + 0.5 - center) / support).abs()).max(0.))
                .collect();
            let sum: f64 = weights.iter().sum();
            for w in &mut weights {
                *w /= sum;
            }
            (
                start,
                weights
                    .into_iter()
                    .map(|v| (v * 4194304.).round() as i32)
                    .collect(),
            )
        })
        .collect()
}

pub fn resize(src: &RgbImage, w: u32, h: u32) -> RgbImage {
    let (sw, sh) = src.dimensions();
    if sw == w && sh == h {
        return src.clone();
    }
    let horizontal = coefficients(sw, w);
    let temporary = RgbImage::from_fn(w, sh, |x, y| {
        let (start, weights) = &horizontal[x as usize];
        let mut sum = [1 << 21; 3];
        for (k, &wt) in weights.iter().enumerate() {
            let v = src.get_pixel(start + k as u32, y);
            for c in 0..3 {
                sum[c] += i32::from(v[c]) * wt;
            }
        }
        Rgb(sum.map(|n| (n >> 22).clamp(0, 255) as u8))
    });
    if sh == h {
        return temporary;
    }
    let vertical = coefficients(sh, h);
    RgbImage::from_fn(w, h, |x, y| {
        let (start, weights) = &vertical[y as usize];
        let mut sum = [1 << 21; 3];
        for (k, &wt) in weights.iter().enumerate() {
            let v = temporary.get_pixel(x, start + k as u32);
            for c in 0..3 {
                sum[c] += i32::from(v[c]) * wt;
            }
        }
        Rgb(sum.map(|n| (n >> 22).clamp(0, 255) as u8))
    })
}

pub fn remove_red_stamp(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let red = |p: &Rgb<u8>| {
        i16::from(p[0]) - i16::from(p[1]) > 16 && i16::from(p[0]) - i16::from(p[2]) > 14
    };
    let dark = |x, y| {
        let p = img.get_pixel(x, y);
        !red(p) && p.0.iter().map(|v| u16::from(*v)).sum::<u16>() < 495
    };
    RgbImage::from_fn(w, h, |x, y| {
        let p = img.get_pixel(x, y);
        if !red(p) {
            return *p;
        }
        let bridge = (1..=2).any(|k| {
            (y >= k && y + k < h && dark(x, y - k) && dark(x, y + k))
                || (x >= k && x + k < w && dark(x - k, y) && dark(x + k, y))
        });
        Rgb([if p[0] <= 135 || bridge { 35 } else { 255 }; 3])
    })
}

fn cubic(v: [f64; 4], t: f64) -> f64 {
    let [a, b, c, d] = v;
    b + t * ((c - a) + t * ((2. * (a - b) + c - d) + t * (-a + b - c + d)))
}

fn sample(img: &RgbImage, x: f64, y: f64, ox: i32, oy: i32) -> Rgb<u8> {
    let (w, h) = img.dimensions();
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let dx = x - x.floor();
    let dy = y - y.floor();
    let mut colors = [[0.; 4]; 3];
    for j in 0..4 {
        let row = (iy + oy + j as i32 - 1).clamp(0, h as i32 - 1) as u32;
        let p: [Rgb<u8>; 4] = std::array::from_fn(|i| {
            *img.get_pixel((ix + ox + i as i32 - 1).clamp(0, w as i32 - 1) as u32, row)
        });
        for c in 0..3 {
            colors[c][j] = cubic(p.map(|p| f64::from(p[c])), dx);
        }
    }
    Rgb(colors.map(|v| cubic(v, dy).clamp(0., 255.) as u8))
}

type Matrix = [[f64; 3]; 3];

fn solve<const N: usize, const M: usize>(
    mut a: [[f64; N]; N],
    mut b: [[f64; M]; N],
) -> Option<[[f64; M]; N]> {
    for i in 0..N {
        let mut pivot = i;
        for k in i + 1..N {
            if a[k][i].abs() > a[pivot][i].abs() {
                pivot = k;
            }
        }
        a.swap(i, pivot);
        b.swap(i, pivot);
        let diagonal = a[i][i];
        if diagonal.abs() < 1e-15 {
            return None;
        }
        for v in &mut a[i] {
            *v /= diagonal;
        }
        for v in &mut b[i] {
            *v /= diagonal;
        }
        for k in 0..N {
            let factor = a[k][i];
            if k != i && factor != 0. {
                for j in 0..N {
                    a[k][j] -= factor * a[i][j];
                }
                for j in 0..M {
                    b[k][j] -= factor * b[i][j];
                }
            }
        }
    }
    Some(b)
}

fn product(a: Matrix, b: Matrix) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

// 先求正向透视矩阵，再逆变换到裁图局部坐标。即使矩形框也保留此运算顺序，
// 否则细笔画的双三次插值取整可差一个灰度级，足以改变相似字符的识别。
fn projection(q: Quad, w: u32, h: u32) -> Option<(Matrix, i32, i32)> {
    let corners = [
        [0., 0.],
        [f64::from(w), 0.],
        [f64::from(w), f64::from(h)],
        [0., f64::from(h)],
    ];
    let mut a = [[0.; 8]; 8];
    let mut b = [[0.]; 8];
    for i in 0..4 {
        let [x, y] = q[i].map(f64::from);
        let [u, v] = corners[i];
        a[i * 2] = [x, y, 1., 0., 0., 0., -u * x, -u * y];
        a[i * 2 + 1] = [0., 0., 0., x, y, 1., -v * x, -v * y];
        b[i * 2] = [u];
        b[i * 2 + 1] = [v];
    }
    let [[a], [b], [c], [d], [e], [f], [g], [k]] = solve(a, b)?;
    let inverse = solve(
        [[a, b, c], [d, e, f], [g, k, 1.]],
        [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
    )?;
    let mut minimum = [f64::INFINITY; 2];
    for [u, v] in corners {
        let p = inverse.map(|[a, b, c]| a * u + b * v + c);
        for i in 0..2 {
            minimum[i] = minimum[i].min(p[i] / p[2]);
        }
    }
    let [ox, oy] = minimum.map(|v| v.floor() as i32 - 2);
    let shift = [
        [1., 0., -f64::from(ox)],
        [0., 1., -f64::from(oy)],
        [0., 0., 1.],
    ];
    let post = [[1., 0., 0.5], [0., 1., 0.5], [0., 0., 1.]];
    let pre = [[1., 0., -0.5], [0., 1., -0.5], [0., 0., 1.]];
    let m = product(product(product(post, shift), inverse), pre);
    Some((m.map(|row| row.map(|val| val / m[2][2])), ox, oy))
}

pub fn crop(img: &RgbImage, q: Quad) -> Option<RgbImage> {
    let dist = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
    let w = (dist(q[0], q[1]).max(dist(q[2], q[3])) as u32).max(1);
    let h = (dist(q[0], q[3]).max(dist(q[1], q[2])) as u32).max(1);
    let (m, ox, oy) = projection(q, w, h)?;
    let result = RgbImage::from_fn(w, h, |x, y| {
        let (u, v) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
        let pt = m.map(|[a, b, c]| a * u + b * v + c);
        sample(img, pt[0] / pt[2] - 0.5, pt[1] / pt[2] - 0.5, ox, oy)
    });
    Some(if f64::from(h) / f64::from(w) >= 1.5 {
        image::imageops::rotate270(&result)
    } else {
        result
    })
}
