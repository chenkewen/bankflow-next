use image::RgbImage;

fn groups(indices: impl Iterator<Item = usize>) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for index in indices {
        if let Some(group) = groups.last_mut() {
            if index - group[group.len() - 1] <= 4 {
                group.push(index);
                continue;
            }
        }
        groups.push(vec![index]);
    }
    groups
}

fn center(group: &[usize]) -> f64 {
    group.iter().sum::<usize>() as f64 / group.len() as f64
}

pub fn detect(image: &RgbImage, scale: f64) -> (Vec<f64>, Vec<f64>) {
    let (width, height) = (image.width() as usize, image.height() as usize);
    if width < 100 || height < 100 {
        return (vec![], vec![]);
    }
    let mask: Vec<u8> = image
        .pixels()
        .map(|pixel| u8::from(pixel.0.iter().map(|&v| u16::from(v)).sum::<u16>() < 708))
        .collect();
    let ratios: Vec<f64> = mask
        .chunks_exact(width)
        .map(|row| row.iter().map(|&v| v as usize).sum::<usize>() as f64 / width as f64)
        .collect();
    let horizontal: Vec<f64> = groups((0..height).filter(|&y| ratios[y] > 0.42))
        .iter()
        .filter(|g| {
            g.len() <= 6
                && ratios[g[0].saturating_sub(3)] < 0.22
                && ratios[(g[g.len() - 1] + 3).min(height - 1)] < 0.22
        })
        .map(|g| center(g))
        .collect();
    let points = |px: f64| (px * scale * 10.0).round_ties_even() / 10.0;
    let horizontal_pt = horizontal.iter().copied().map(points).collect();
    if horizontal.len() < 4 {
        return (horizontal_pt, vec![]);
    }

    // 页眉、页脚的孤立横线会缩短有效竖线，先选取最密集的一组连续表格横线。
    let mut best: &[f64] = &[];
    let mut start = 0;
    for end in 1..=horizontal.len() {
        if end == horizontal.len() || (horizontal[end] - horizontal[end - 1]) * scale > 65.0 {
            if end - start > best.len() {
                best = &horizontal[start..end];
            }
            start = end;
        }
    }
    let mut vertical = Vec::new();
    if best.len() >= 3 {
        // 多个候选区间共用列计数快照，每个像素仅访问一次。
        let starts = (best.len() - 2).min(5);
        let ends = if best.len() >= 6 { 2 } else { 1 };
        let mut snapshots: Vec<Vec<u32>> = Vec::with_capacity(best.len());
        let mut counts = vec![0u32; width];
        let mut previous = best[0] as usize;
        for &line in best {
            let y = line as usize;
            for row in mask[previous * width..y * width].chunks_exact(width) {
                for (count, &dark) in counts.iter_mut().zip(row) {
                    *count += u32::from(dark);
                }
            }
            snapshots.push(counts.clone());
            previous = y;
        }
        for top in 0..starts {
            for end_offset in 0..ends {
                let bottom = best.len() - 1 - end_offset;
                let span = best[bottom] as usize - best[top] as usize;
                if bottom <= top + 1 || span <= 60 {
                    continue;
                }
                let candidates = groups((0..width).filter(|&x| {
                    f64::from(snapshots[bottom][x] - snapshots[top][x]) / span as f64 > 0.86
                }));
                let lines: Vec<f64> = candidates
                    .iter()
                    .filter(|g| g.len() <= 7)
                    .map(|g| points(center(g)))
                    .collect();
                if lines.len() > vertical.len() {
                    vertical = lines;
                }
            }
        }
    }
    if vertical.len() < 4 {
        vertical.clear();
    }
    (horizontal_pt, vertical)
}
