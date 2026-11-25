use super::Result;
use image::{DynamicImage, GrayImage, RgbImage};
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

#[derive(Default)]
struct Run {
    remaining: usize,
    value: u8,
}

impl Run {
    fn fill(&mut self, reader: &mut impl Read, mut out: &mut [u8]) -> Result<()> {
        while !out.is_empty() {
            if self.remaining == 0 {
                let mut byte = [0];
                reader.read_exact(&mut byte)?;
                self.value = byte[0];
                self.remaining = 1;
                if self.value == 0x80 {
                    reader.read_exact(&mut byte)?;
                    if byte[0] != 0 {
                        self.remaining += usize::from(byte[0]);
                        reader.read_exact(&mut byte)?;
                        self.value = byte[0];
                    }
                }
            }
            let count = out.len().min(self.remaining);
            let (head, tail) = out.split_at_mut(count);
            head.fill(self.value);
            self.remaining -= count;
            out = tail;
        }
        Ok(())
    }
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut header = [0u8; 32];
    reader.read_exact(&mut header)?;
    let mut fields = header
        .chunks_exact(4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()));
    let [magic, width, height, depth, _, kind, palette_type, palette_bytes] =
        std::array::from_fn(|_| fields.next().unwrap());
    if magic != 0x59A66A95 || !matches!(depth, 1 | 4 | 8 | 24 | 32) || kind > 5 {
        return Err("不支持的 Sun Raster 图像格式".into());
    }
    let entries = palette_bytes as usize / 3;
    if palette_bytes > 0 && (palette_type != 1 || !matches!(depth, 4 | 8) || entries > 256) {
        return Err("Sun Raster 色表类型或长度无效".into());
    }
    let mut palette = vec![0; palette_bytes as usize];
    reader.read_exact(&mut palette)?;

    let rgb = depth >= 24 || !palette.is_empty();
    let channels = if rgb { 3 } else { 1 };
    let count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels))
        .filter(|n| (1..=512 * 1024 * 1024).contains(n))
        .ok_or("Sun Raster 图像尺寸无效或过大")?;
    let bytes_per_row = (width as usize)
        .checked_mul(depth as usize)
        .ok_or("Sun Raster 行宽无效")?
        .div_ceil(8);
    let mut row = vec![0; bytes_per_row];
    let mut pixels = vec![0; count];
    let mut run = Run::default();
    for (i, output) in pixels
        .chunks_exact_mut(width as usize * channels)
        .enumerate()
    {
        if kind == 2 {
            // Pillow 的字节游程连续展开，跨行的游程保留；解压后的每行仅包含实际像素字节。
            run.fill(&mut reader, &mut row)?;
        } else {
            reader.read_exact(&mut row)?;
            // 普通行按 16 位对齐，最后一行末尾的填充可省略。
            if bytes_per_row % 2 == 1 && i + 1 < height as usize {
                reader.read_exact(&mut [0])?;
            }
        }
        if depth >= 24 {
            for (src, dst) in row
                .chunks_exact((depth / 8) as usize)
                .zip(output.chunks_exact_mut(3))
            {
                if kind == 3 {
                    dst.copy_from_slice(&src[..3]);
                } else {
                    dst.copy_from_slice(&[src[2], src[1], src[0]]);
                }
            }
            continue;
        }
        for (x, pixel) in output.chunks_exact_mut(channels).enumerate() {
            let index = match depth {
                1 => {
                    if row[x / 8] & (0x80 >> (x % 8)) == 0 {
                        255
                    } else {
                        0
                    }
                }
                4 => (row[x / 2] >> (4 - 4 * (x % 2))) & 15,
                _ => row[x],
            };
            if palette.is_empty() {
                pixel[0] = if depth == 4 { index * 17 } else { index };
            } else if usize::from(index) < entries {
                // 色表依次存放完整红、绿、蓝平面。未定义的索引沿用黑色。
                for (plane, ch) in pixel.iter_mut().enumerate() {
                    *ch = palette[plane * entries + usize::from(index)];
                }
            }
        }
    }
    if rgb {
        Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(width, height, pixels).ok_or("Sun Raster RGB 像素数量无效")?,
        ))
    } else {
        Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(width, height, pixels).ok_or("Sun Raster 灰度像素数量无效")?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn raster(depth: u32, kind: u32, palette: &[u8], data: &[u8]) -> Result<DynamicImage> {
        let mut file = tempfile::NamedTempFile::new()?;
        for field in [0x59A66A95, 3, 2, depth, 0, kind, 1, palette.len() as u32] {
            file.write_all(&field.to_be_bytes())?;
        }
        file.write_all(palette)?;
        file.write_all(data)?;
        file.flush()?;
        decode(file.path())
    }

    #[test]
    fn palette_planes_and_odd_width() {
        let img = raster(4, 1, &[10, 20, 30, 40, 50, 60], &[1, 0xF0, 0x10, 0])
            .unwrap()
            .into_rgb8();
        assert_eq!(
            img.as_raw(),
            &[10, 30, 50, 20, 40, 60, 0, 0, 0, 20, 40, 60, 10, 30, 50, 10, 30, 50]
        );
        let img = raster(8, 1, &[], &[1, 2, 3, 255, 4, 5, 6])
            .unwrap()
            .into_luma8();
        assert_eq!(img.as_raw(), &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn rle_escape_crosses_rows_and_rejects_truncation() {
        let img = raster(8, 2, &[], &[1, 0x80, 3, 42, 0x80, 0])
            .unwrap()
            .into_luma8();
        assert_eq!(img.as_raw(), &[1, 42, 42, 42, 42, 128]);
        assert!(raster(8, 2, &[], &[0x80, 4]).is_err());
    }
}
