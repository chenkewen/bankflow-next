use super::Result;
use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

fn uint16(bytes: &[u8]) -> usize {
    usize::from(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(super) fn supported(h: &[u8]) -> bool {
    h.len() >= 12
        && h[..2] == [1, 218]
        && matches!(h[2], 0 | 1)
        && matches!(h[3], 1 | 2)
        && matches!((uint16(&h[4..]), uint16(&h[10..])), (1 | 2, 1) | (3, 3 | 4))
}

fn unpack(row: &[u8], bpc: usize, dst: &mut [u8]) -> Result<()> {
    let mut words = row.chunks_exact(bpc);
    let mut n = 0;
    while let Some(control) = words.next() {
        let op = control[bpc - 1];
        let count = usize::from(op & 127);
        if count == 0 {
            break;
        }
        let part = dst.get_mut(n..n + count).ok_or("SGI 行数据过宽")?;
        if op & 128 != 0 {
            for x in part {
                *x = words.next().ok_or("SGI 行数据不完整")?[0];
            }
        } else {
            part.fill(words.next().ok_or("SGI 行数据不完整")?[0]);
        }
        n += count;
    }
    if n != dst.len() {
        return Err("SGI 行数据不完整".into());
    }
    Ok(())
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let mut src = BufReader::new(File::open(path)?);
    let file_size = src.get_ref().metadata()?.len();
    let mut h = [0; 512];
    src.read_exact(&mut h)?;
    if !supported(&h) {
        return Err("不支持此 SGI 像素格式".into());
    }
    let w = uint16(&h[6..]);
    let height = uint16(&h[8..]);
    let channels = uint16(&h[10..]);
    let bpc = usize::from(h[3]);
    let count = height * channels;
    let size = w
        .checked_mul(count)
        .filter(|n| *n > 0 && *n <= 512 * 1024 * 1024)
        .ok_or("SGI 图片尺寸无效")?;
    let compressed = h[2] == 1;
    let (mut offsets, mut lengths) = (vec![], vec![]);
    if compressed {
        for table in [&mut offsets, &mut lengths] {
            for _ in 0..count {
                let mut n = [0; 4];
                src.read_exact(&mut n)?;
                table.push(u32::from_be_bytes(n));
            }
        }
    }
    let mut pixels = vec![0; size];
    let mut row = vec![0; w];
    let mut raw = vec![0; w * bpc];
    for i in 0..count {
        if compressed {
            let pos = u64::from(offsets[i]);
            let len = lengths[i] as usize;
            // 每个单像素最多占一个控制字和一个数据字，另有终止字。
            if len > (w * 2 + 1) * bpc || pos + len as u64 > file_size {
                return Err("SGI 行偏移或长度无效".into());
            }
            src.seek(SeekFrom::Start(pos))?;
            raw.resize(len, 0);
        }
        src.read_exact(&mut raw)?;
        if compressed {
            unpack(&raw, bpc, &mut row)?;
        } else {
            for (dst, p) in row.iter_mut().zip(raw.chunks_exact(bpc)) {
                *dst = p[0];
            }
        }
        let channel = i / height;
        let y = height - 1 - (i % height);
        let dest = &mut pixels[y * w * channels..(y + 1) * w * channels];
        for (px, &val) in dest.chunks_exact_mut(channels).zip(&row) {
            px[channel] = val;
        }
    }
    // 文件以底部为第零行，16 位分量按原有解码器的高 8 位导出。
    let (w, h) = (w as u32, height as u32);
    Ok(match channels {
        1 => DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, pixels).unwrap()),
        3 => DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, pixels).unwrap()),
        _ => DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, pixels).unwrap()),
    })
}
