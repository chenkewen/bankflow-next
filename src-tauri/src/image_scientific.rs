use super::{bytes, length, Result};
use flate2::read::GzDecoder;
use image::{DynamicImage, GrayImage, ImageBuffer, RgbImage};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
};

pub(super) fn spider_order(header: &[u8]) -> Option<bool> {
    let values = header.get(..108)?;
    for big in [true, false] {
        let field = |index: usize| {
            let b = values[(index - 1) * 4..index * 4].try_into().unwrap();
            if big {
                f32::from_be_bytes(b)
            } else {
                f32::from_le_bytes(b)
            }
        };
        if field(1) == 1.
            && field(5) == 1.
            && [2, 12, 13, 22, 23].iter().all(|&i| {
                let n = field(i);
                n >= 1. && n < u32::MAX as f32 && n.fract() == 0.
            })
            && field(23) == 4. * field(12)
            && field(22) == field(13) * field(23)
            && field(22) >= 108.
        {
            return Some(big);
        }
    }
    None
}

fn gray16(w: u32, h: u32, data: Vec<u16>) -> Result<DynamicImage> {
    Ok(DynamicImage::ImageLuma16(
        ImageBuffer::from_raw(w, h, data).ok_or("灰度像素数量错误")?,
    ))
}

fn gray_float(v: f32) -> u16 {
    if v < 2_147_483_648. {
        v as u16
    } else {
        0
    }
}

pub(super) fn spider(r: &mut BufReader<File>, header: &[u8], big: bool) -> Result<DynamicImage> {
    let number = |index: usize| {
        let b = header[(index - 1) * 4..index * 4].try_into().unwrap();
        if big {
            f32::from_be_bytes(b)
        } else {
            f32::from_le_bytes(b)
        }
    };
    let (w, h) = (number(12) as u32, number(2) as u32);
    let n = length(w, h, 4)?;
    let offset = number(22) as u64;
    let offset = match (number(24), number(27)) {
        (0., 0.) => offset,
        (stack, 0.) if stack > 0. && number(26) == 1. => offset * 2,
        (stack, 0.) if stack > 0. && number(26) > 1. => {
            return Err("暂不支持多帧 SPIDER 图片，请先拆分帧".into())
        }
        _ => return Err("SPIDER 堆栈头无效".into()),
    };
    if r.get_ref().metadata()?.len() < offset + n as u64 {
        return Err("SPIDER 像素数据不完整".into());
    }
    r.seek(SeekFrom::Start(offset))?;
    let mut pixels = Vec::with_capacity(n / 4);
    for _ in 0..n / 4 {
        let b = bytes(r)?;
        pixels.push(gray_float(if big {
            f32::from_be_bytes(b)
        } else {
            f32::from_le_bytes(b)
        }));
    }
    gray16(w, h, pixels)
}

pub(super) fn fits(r: &mut BufReader<File>) -> Result<DynamicImage> {
    r.rewind()?;
    // 主头可以是空 HDU，后面跟图像扩展。每个头由 80 字节卡片组成，按 2880 字节对齐。
    let mut fields = HashMap::new();
    let mut cards = 0;
    loop {
        let card = bytes::<80>(r)?;
        cards += 1;
        if cards > 32768 {
            return Err("FITS 头过长".into());
        }
        let key = std::str::from_utf8(&card[..8])?.trim();
        if key == "END" {
            let next = r.stream_position()?.div_ceil(2880) * 2880;
            r.seek(SeekFrom::Start(next))?;
            if fields.get("NAXIS").is_some_and(|s: &String| s != "0") {
                break;
            }
            fields.clear();
            continue;
        }
        if card[8] == b'=' {
            let value = std::str::from_utf8(&card[9..])?
                .split('/')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('\'')
                .trim()
                .to_owned();
            fields.insert(key.to_owned(), value);
        }
    }
    let number = |key: &str| -> Result<i32> {
        Ok(fields.get(key).ok_or("缺少 FITS 图像参数")?.parse()?)
    };
    let compressed = fields.get("ZIMAGE").is_some_and(|v| v == "T");
    if compressed && fields.get("ZCMPTYPE").map(String::as_str) != Some("GZIP_1") {
        return Err("不支持此 FITS 压缩方式".into());
    }
    if fields
        .get("XTENSION")
        .is_some_and(|v| v != "IMAGE" && !(v == "BINTABLE" && compressed))
    {
        return Err("不支持此 FITS 扩展".into());
    }
    let prefix = if compressed { "Z" } else { "" };
    let get = |name: &str| number(&format!("{prefix}{name}"));
    let (w, h) = match get("NAXIS")? {
        1 => (1, get("NAXIS1")?),
        2 => (get("NAXIS1")?, get("NAXIS2")?),
        _ => return Err("仅支持一维和二维 FITS 图像".into()),
    };
    let (w, h) = (u32::try_from(w)?, u32::try_from(h)?);
    let bits = get("BITPIX")?;
    if !matches!(bits, 8 | 16 | 32 | -32 | -64) {
        return Err("不支持此 FITS 像素位深".into());
    }
    let stride = bits.unsigned_abs() as usize / 8;
    let size = length(w, h, stride)?;
    let mut data;
    let mut raw_stride = stride;
    if compressed {
        // 按描述符定位每一行，支持物理位置与行号不同的压缩块。
        if get("TILE1")? != w as i32
            || get("TILE2")? != 1
            || number("NAXIS1")? != 8
            || number("NAXIS2")? != h as i32
            || !fields
                .get("TFORM1")
                .is_some_and(|value| value.starts_with("1PB") || value.starts_with("PB"))
        {
            return Err("不支持此 FITS 压缩分块布局".into());
        }
        let start = r.stream_position()?;
        let table_len = u64::try_from(length(8, h, 1)?)?;
        let heap_start = match fields.get("THEAP") {
            Some(v) => v.parse::<u64>()?,
            None => table_len,
        };
        let heap_size = u64::try_from(number("PCOUNT")?)?;
        let end = start
            .checked_add(table_len)
            .and_then(|n| n.checked_add(heap_size))
            .ok_or("FITS 数据区过大")?;
        if heap_start < table_len
            || heap_start > table_len + heap_size
            || end > r.get_ref().metadata()?.len()
        {
            return Err("FITS 压缩数据区无效".into());
        }
        data = Vec::new();
        let width = w as usize;
        let max_row = length(w, 1, stride.max(4))?;
        length(w, h, stride.max(4))?;
        let mut row = Vec::new();
        for index in 0..h {
            r.seek(SeekFrom::Start(start + u64::from(index) * 8))?;
            let n = u64::from(u32::from_be_bytes(bytes(r)?));
            let off = u64::from(u32::from_be_bytes(bytes(r)?));
            let pos = start + heap_start + off;
            if pos + n > end {
                return Err("FITS 压缩块超出数据区".into());
            }
            r.seek(SeekFrom::Start(pos))?;
            row.clear();
            GzDecoder::new(r.take(n))
                .take(max_row as u64 + 1)
                .read_to_end(&mut row)?;
            let stored = if stride < 4 && row.len() == width * 4 {
                4
            } else {
                stride
            };
            if row.len() != width * stored || (index > 0 && stored != raw_stride) {
                return Err("FITS 压缩行长度错误".into());
            }
            raw_stride = stored;
            data.extend_from_slice(&row);
        }
    } else {
        let offset = r.stream_position()?;
        if r.get_ref().metadata()?.len().saturating_sub(offset) < size as u64 {
            return Err("FITS 像素数据不完整".into());
        }
        data = vec![0; size];
        r.read_exact(&mut data)?;
    }
    let rows = data.chunks_exact(w as usize * raw_stride).rev();
    if bits == 8 {
        let pixels = rows
            .flat_map(|row| row.chunks_exact(raw_stride).map(|v| v[raw_stride - 1]))
            .collect();
        return Ok(DynamicImage::ImageLuma8(
            GrayImage::from_raw(w, h, pixels).ok_or("FITS 灰度像素数量错误")?,
        ));
    }
    let pixels = rows
        .flat_map(|row| row.chunks_exact(raw_stride))
        .map(|v| {
            let v = &v[raw_stride - stride..];
            match bits {
                16 => u16::from_be_bytes(v.try_into().unwrap()),
                32 => i32::from_be_bytes(v.try_into().unwrap()).clamp(0, 65535) as u16,
                -32 => gray_float(f32::from_be_bytes(v.try_into().unwrap())),
                _ => gray_float(f64::from_be_bytes(v.try_into().unwrap()) as f32),
            }
        })
        .collect();
    gray16(w, h, pixels)
}

pub(super) fn photo_cd(r: &mut BufReader<File>) -> Result<DynamicImage> {
    r.seek(SeekFrom::Start(2048))?;
    if bytes::<4>(r)? != *b"PCD_" {
        return Err("PCD 图片标记无效".into());
    }
    r.seek(SeekFrom::Start(3586))?;
    let orientation = bytes::<1>(r)?[0] & 3;
    r.seek(SeekFrom::Start(96 * 2048))?;
    const W: usize = 768;
    let mut pixels = RgbImage::new(W as u32, 512);
    let adjust =
        |v: u8, zero: f64, coefficient: f64| ((f64::from(v) - zero) * coefficient + 0.5) as i32;
    // PhotoCD 的 Base 分辨率为 768×512，每两个扫描行共用一行 2:1 色差信号。
    for pair in pixels.as_mut().chunks_exact_mut(W * 6) {
        let source = bytes::<{ W * 3 }>(r)?;
        for (i, pixel) in pair.chunks_exact_mut(3).enumerate() {
            let y = adjust(source[i], 0., 1.35839);
            let x = (i % W) / 2;
            let (cb, cr) = (source[2 * W + x], source[5 * W / 2 + x]);
            let rgb = [
                y + adjust(cr, 137., 1.82158),
                y + adjust(cb, 156., -0.43028) + adjust(cr, 137., -0.92713),
                y + adjust(cb, 156., 2.2179),
            ];
            pixel.copy_from_slice(&rgb.map(|v| v.clamp(0, 255) as u8));
        }
    }
    let img = DynamicImage::ImageRgb8(pixels);
    Ok(match orientation {
        1 => img.rotate270(),
        3 => img.rotate90(),
        _ => img,
    })
}
