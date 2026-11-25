pub(crate) fn record_rows(
    bounds: &[(f64, f64)],
    groups: Vec<Vec<(f64, f64, f64, String)>>,
) -> Vec<Vec<String>> {
    groups
        .into_iter()
        .map(|mut group| {
            let mut row = vec![String::new(); bounds.len()];
            let mut previous = vec![None::<f64>; row.len()];
            group.sort_by(|a, b| a.2.total_cmp(&b.2).then_with(|| a.0.total_cmp(&b.0)));
            for (left, right, top, text) in group {
                let x = (left + right) / 2.0;
                let col = bounds
                    .iter()
                    .position(|&(a, b)| a <= x && x <= b)
                    .or_else(|| {
                        bounds
                            .iter()
                            .enumerate()
                            .map(|(i, &(a, b))| (i, (x - a).abs().min((x - b).abs())))
                            .filter(|(_, d)| *d <= 15.)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(i, _)| i)
                    });
                if let Some(i) = col {
                    if previous[i].is_some_and(|v| (top - v).abs() > 3.) {
                        row[i].push(' ');
                    }
                    row[i].push_str(&text);
                    previous[i] = Some(top);
                }
            }
            row
        })
        .collect()
}
