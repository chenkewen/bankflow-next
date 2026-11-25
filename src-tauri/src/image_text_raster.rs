use super::Result;
use image::{DynamicImage, GrayImage, RgbImage};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

pub(super) fn detect(header: &[u8]) -> bool {
    header.starts_with(b"P7 332")
        || header
            .split(|&b| b == b'\n')
            .any(|line| line.starts_with(b"width ") || line.starts_with(b"pixel n8"))
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let mut f = BufReader::new(File::open(path)?);
    let xv = f.fill_buf()?.starts_with(b"P7 332");
    let mut line = String::new();
    let (mut w, mut h, mut gray) = (0u32, 0u32, false);
    let mut total = 0;
    loop {
        if !xv && f.fill_buf()?.first() == Some(&12) {
            f.consume(1);
            break;
        }
        line.clear();
        let len = (&mut f).take(1025).read_line(&mut line)?;
        total += len;
        if len == 0 || len > 1024 || total > 1 << 20 {
            return Err("图片文本头过长或不完整".into());
        }
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if xv {
            if !line.starts_with(['P', '#']) {
                w = fields.first().ok_or("XV 宽度缺失")?.parse()?;
                h = fields.get(1).ok_or("XV 高度缺失")?.parse()?;
                break;
            }
        } else {
            match fields.as_slice() {
                ["width", value] => w = value.parse()?,
                ["height", value] => h = value.parse()?,
                ["pixel", value] => gray = *value == "n8",
                _ => (),
            }
        }
    }
    let count = w
        .checked_mul(h)
        .filter(|n| *n > 0 && *n <= 128 << 20)
        .ok_or("图片尺寸无效或过大")?;
    if !xv && !gray {
        return Err("不支持此 IMT 像素布局".into());
    }
    let mut raster = Vec::new();
    f.take(u64::from(count)).read_to_end(&mut raster)?;
    if raster.len() != count as usize {
        return Err("图片数据不完整".into());
    }
    if xv {
        let rgb = raster
            .into_iter()
            .flat_map(|v| {
                [
                    (u16::from(v >> 5) * 255 / 7) as u8,
                    (u16::from((v >> 2) & 7) * 255 / 7) as u8,
                    (v & 3) * 85,
                ]
            })
            .collect();
        Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, h, rgb).unwrap(),
        ))
    } else {
        Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(w, h, raster).unwrap(),
        ))
    }
}
