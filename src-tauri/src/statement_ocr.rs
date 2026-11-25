use ort::{
    execution_providers::CPUExecutionProvider,
    session::{builder::GraphOptimizationLevel, Session},
};
use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn first_maximum(scores: &[f32]) -> usize {
    if scores[0].is_nan() {
        return 0;
    }
    // 每个时间步有数千个类别。分组求最大值可向量化，最后按原顺序选首个同分字符。
    let mut maxima = [f32::NEG_INFINITY; 8];
    let (blocks, tail) = scores.as_chunks::<8>();
    for block in blocks {
        for i in 0..8 {
            if block[i] > maxima[i] {
                maxima[i] = block[i];
            }
        }
    }
    let best = maxima.iter().chain(tail).copied().fold(scores[0], f32::max);
    scores.iter().position(|&score| score == best).unwrap_or(0)
}

pub(crate) fn decode(scores: &[f32], classes: usize, alphabet: &[String]) -> Result<(String, f64)> {
    if classes != alphabet.len() || classes == 0 || scores.len() % classes != 0 {
        return Err("识别模型输出与字符表不一致".into());
    }
    let mut previous = 0;
    let mut text = String::new();
    let mut sum = 0.0;
    let mut count = 0;
    for step in scores.chunks_exact(classes) {
        let best = first_maximum(step);
        if best != 0 && best != previous {
            text.push_str(&alphabet[best]);
            sum += f64::from(step[best]);
            count += 1;
        }
        previous = best;
    }
    Ok((text, sum / f64::from(count.max(1))))
}

pub(crate) fn session(model: std::path::PathBuf) -> Result<Session> {
    // 检测与文字识别共用 CPU，每个模型最多使用四个线程，避免单任务占满整机。
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    Ok(Session::builder()?
        .with_execution_providers([CPUExecutionProvider::default()
            // 同一文档的检测与逐格复读持续分配临时张量，复用内存到当前 worker 退出。
            .with_arena_allocator(true)
            .build()])?
        .with_intra_threads(threads)?
        .with_inter_threads(1)?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .commit_from_file(model)?)
}

pub(crate) fn alphabet(session: &Session) -> Result<Vec<String>> {
    let metadata = session
        .metadata()?
        .custom("character")?
        .ok_or("模型缺少字符表")?;
    let mut chars = vec![String::new()];
    chars.extend(metadata.lines().map(String::from));
    chars.push(" ".into());
    Ok(chars)
}

#[cfg(test)]
mod tests {
    use super::first_maximum;

    #[test]
    fn maximum_matches_sequential_search_with_ties_padding_and_nan() {
        let scores = [
            0.1,
            f32::NAN,
            0.8,
            -0.0,
            0.2,
            0.5,
            0.8,
            0.1,
            0.8,
            0.3,
            f32::INFINITY,
        ];
        for start in 0..scores.len() {
            for end in start + 1..=scores.len() {
                let block = &scores[start..end];
                let expected =
                    (1..block.len()).fold(
                        0,
                        |best, i| {
                            if block[i] > block[best] {
                                i
                            } else {
                                best
                            }
                        },
                    );
                assert_eq!(first_maximum(block), expected);
            }
        }
    }
}
