use super::Result;
use image::{DynamicImage, ImageBuffer};
use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    path::Path,
};

pub(super) fn detect(header: &[u8]) -> bool {
    header.split(|&b| b == b'\n').any(|line| {
        let line = line.strip_prefix(b"\r").unwrap_or(line);
        [
            b"Image type:".as_slice(),
            b"Image size (x*y):",
            b"File size (no of images):",
        ]
        .iter()
        .any(|tag| line.starts_with(tag))
    })
}

#[derive(Clone, Copy)]
enum Samples {
    Bits(u8),
    Index(u8),
    Gray,
    GrayAlpha,
    Rgb,
    PackedRgb,
    WholePlanes,
    Rgbx,
    Rgba,
    Cmyk,
    Ycc,
    U8,
    I8,
    U16(bool),
    I16,
    U32,
    I32,
    Float,
    Packed(u8),
}

impl Samples {
    fn parse(name: &str) -> Result<Self> {
        let mode = name.strip_suffix(" image").unwrap_or(name);
        Ok(match mode {
            "0 1" | "L 1" | "B1" => Self::Bits(1),
            "B2" => Self::Index(2),
            "B4" => Self::Index(4),
            "Greyscale" | "Grayscale" => Self::Gray,
            "LA" | "PA" => Self::GrayAlpha,
            "RGB" => Self::Rgb,
            "X 24" => Self::PackedRgb,
            "RGB3" | "RYB3" => Self::WholePlanes,
            "RGBA" => Self::Rgba,
            "RGBX" => Self::Rgbx,
            "CMYK" => Self::Cmyk,
            "YCC" => Self::Ycc,
            "L 32 S" => Self::I32,
            "L 32 F" => Self::U32,
            _ => {
                let token = mode
                    .strip_prefix("L ")
                    .or_else(|| mode.strip_prefix("L*"))
                    .ok_or("不支持此 IM 像素布局")?;
                match token {
                    "8" => Self::U8,
                    "8S" => Self::I8,
                    "16" | "16L" => Self::U16(false),
                    "16B" => Self::U16(true),
                    "16S" => Self::I16,
                    "32" => Self::U32,
                    "32S" => Self::I32,
                    "32F" => Self::Float,
                    _ if mode.starts_with("L*") => {
                        let n: u8 = token.parse()?;
                        if !(2..=31).contains(&n) {
                            return Err("不支持此 IM 位深".into());
                        }
                        Self::Packed(n)
                    }
                    _ => return Err("不支持此 IM 数值格式".into()),
                }
            }
        })
    }
    fn bits(self) -> usize {
        match self {
            Self::Bits(b) | Self::Index(b) | Self::Packed(b) => b.into(),
            Self::Gray | Self::U8 | Self::I8 => 8,
            Self::GrayAlpha | Self::U16(_) | Self::I16 => 16,
            Self::Rgb | Self::PackedRgb | Self::WholePlanes | Self::Ycc => 24,
            _ => 32,
        }
    }
}

fn ycc(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    // JFIF 变换采用与原读取器一致的六位定点色差系数。
    let term = |v: u8, k: f64| ((f64::from(v) - 128.0) * k * 64.0 + 0.5) as i32;
    [
        term(cr, 1.402),
        term(cb, -0.34414) + term(cr, -0.71414),
        term(cb, 1.772),
    ]
    .map(|c| (i32::from(y) + (c >> 6)).clamp(0, 255) as u8)
}

fn numeric_gray(value: f32) -> u16 {
    // 旧流程先转换成 i32，再裁剪到灰度 PNG；超出有符号范围时产生 0。
    if value < 2_147_483_648.0 {
        value as u16
    } else {
        0
    }
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut width = 512u32;
    let mut height = 512u32;
    let mut mode = Samples::Gray;
    let mut has_palette = false;
    let mut line = Vec::new();
    let mut padding = false;
    let mut header_size = 0usize;
    loop {
        let mut next = [0];
        reader.read_exact(&mut next)?;
        header_size += 1;
        if header_size > 1 << 20 {
            return Err("IM 文件头过大".into());
        }
        if next[0] == 0x1a {
            break;
        }
        if next[0] == 0 {
            padding = true;
        }
        if padding || next[0] == b'\r' {
            continue;
        }
        if next[0] != b'\n' {
            line.push(next[0]);
            if line.len() > 100 {
                return Err("IM 文件头字段过长".into());
            }
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            return Err("IM 文件头字段无效".into());
        };
        let value = String::from_utf8_lossy(&line[colon + 1..]);
        match &line[..colon] {
            b"Image type" => mode = Samples::parse(value.trim())?,
            b"Image size (x*y)" => {
                let (w, h) = value.split_once(['*', ',']).ok_or("IM 图像尺寸无效")?;
                width = w.trim().parse()?;
                height = h.trim().parse()?;
            }
            b"File size (no of images)" if value.trim().parse::<u32>()? != 1 => {
                return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into())
            }
            b"Lut" => has_palette = true,
            _ => (),
        }
        line.clear();
    }
    let mut palette = [0; 768];
    if has_palette {
        reader.read_exact(&mut palette)?;
    }
    let colored =
        (0..256).any(|i| palette[i] != palette[i + 256] || palette[i] != palette[i + 512]);
    if colored && matches!(mode, Samples::Index(_)) {
        // IFUNC 为彩色 LUT 使用每像素一字节的索引，包括 B2/B4 声明。
        mode = Samples::Gray;
    }
    let w = width as usize;
    let h = height as usize;
    let count = w
        .checked_mul(h)
        .filter(|n| *n > 0 && *n <= (512 << 20) / 4)
        .ok_or("IM 图像尺寸过大或无效")?;
    let stride = (w * mode.bits()).div_ceil(8);
    let size = stride * h;
    if size as u64 > reader.get_ref().metadata()?.len() - reader.stream_position()? {
        return Err("IM 像素数据不完整".into());
    }
    let mut data = vec![0; size];
    reader.read_exact(&mut data)?;
    let pixels = (0..count).map(|i| {
        let x = i % w;
        let base = (h - 1 - i / w) * stride;
        (x, base)
    });
    macro_rules! image {
        ($kind:ident,$pixels:expr) => {
            DynamicImage::$kind(
                ImageBuffer::from_raw(width, height, $pixels).ok_or("IM 像素长度无效")?,
            )
        };
    }
    Ok(match mode {
        Samples::U8
        | Samples::I8
        | Samples::U16(_)
        | Samples::I16
        | Samples::U32
        | Samples::I32
        | Samples::Float
        | Samples::Packed(_) => {
            // 与旧单图导出一致：浮点或有符号灰度在转换至 PNG 时截断并裁剪至 16 位。
            let values = pixels
                .map(|(x, base)| {
                    let row = &data[base..base + stride];
                    match mode {
                        Samples::U8 => u16::from(row[x]),
                        Samples::I8 => (row[x] as i8).max(0) as u16,
                        Samples::U16(big) => {
                            let b = [row[2 * x], row[2 * x + 1]];
                            if big {
                                u16::from_be_bytes(b)
                            } else {
                                u16::from_le_bytes(b)
                            }
                        }
                        Samples::I16 => {
                            i16::from_le_bytes([row[2 * x], row[2 * x + 1]]).max(0) as u16
                        }
                        Samples::Packed(bits) => {
                            let start = x * bits as usize;
                            let mut value = 0u32;
                            for (j, i) in (start..start + bits as usize).enumerate() {
                                value |= u32::from((row[i / 8] >> (i % 8)) & 1) << j;
                            }
                            numeric_gray(value as f32)
                        }
                        _ => {
                            let b = row[4 * x..4 * x + 4].try_into().unwrap();
                            match mode {
                                Samples::Float => numeric_gray(f32::from_le_bytes(b)),
                                Samples::I32 => i32::from_le_bytes(b).clamp(0, 65535) as u16,
                                _ => numeric_gray(u32::from_le_bytes(b) as f32),
                            }
                        }
                    }
                })
                .collect();
            image!(ImageLuma16, values)
        }
        Samples::Bits(bits) | Samples::Index(bits) => {
            let values = pixels
                .map(|(x, base)| {
                    let bit = x * bits as usize;
                    let value =
                        (data[base + bit / 8] >> (8 - bit % 8 - bits as usize)) & ((1 << bits) - 1);
                    if bits == 1 {
                        value * 255
                    } else {
                        value
                    }
                })
                .collect();
            image!(ImageLuma8, values)
        }
        Samples::Gray if !colored => image!(ImageLuma8, pixels.map(|(x, b)| data[b + x]).collect()),
        Samples::GrayAlpha if !colored => image!(
            ImageLumaA8,
            pixels
                .flat_map(|(x, b)| [data[b + x], data[b + w + x]])
                .collect()
        ),
        _ => {
            let alpha = matches!(mode, Samples::Rgba | Samples::GrayAlpha);
            let mut out = Vec::with_capacity(count * (3 + usize::from(alpha)));
            for (x, base) in pixels {
                let at = |p: usize| data[base + p * w + x];
                let rgb = match mode {
                    Samples::Gray | Samples::GrayAlpha => {
                        let i = usize::from(at(0));
                        [palette[i], palette[256 + i], palette[512 + i]]
                    }
                    Samples::WholePlanes => {
                        let i = base / 3 + x;
                        [data[count + i], data[i], data[2 * count + i]]
                    }
                    Samples::PackedRgb => data[base + x * 3..base + x * 3 + 3].try_into().unwrap(),
                    Samples::Cmyk => [at(0), at(1), at(2)]
                        .map(|v| ((u32::from(255 - v) * u32::from(255 - at(3)) + 127) / 255) as u8),
                    Samples::Ycc => ycc(at(0), at(1), at(2)),
                    _ => [at(0), at(1), at(2)],
                };
                out.extend(rgb);
                if alpha {
                    out.push(if matches!(mode, Samples::Rgba) {
                        at(3)
                    } else {
                        at(1)
                    });
                }
            }
            if alpha {
                image!(ImageRgba8, out)
            } else {
                image!(ImageRgb8, out)
            }
        }
    })
}
