use super::Result;
use flate2::{Decompress, FlushDecompress, Status};
use image::{DynamicImage, GrayImage, ImageBuffer, RgbImage, RgbaImage};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

type Input = BufReader<File>;
const LIMIT: u64 = 512 * 1024 * 1024;
// LittleCMS 内置的标准 sRGB 配置，允许自由使用。
const SRGB_ICC: &[u8] = include_bytes!("psd_srgb.icc");
struct Composite {
    width: u32,
    height: u32,
    channels: usize,
    depth: u16,
    mode: u16,
    psb: bool,
    compression: u16,
    profile: Option<(u64, usize)>,
    palette: Vec<u8>,
    transparent: Option<u8>,
    alpha: bool,
}

fn bytes<const N: usize>(input: &mut Input) -> Result<[u8; N]> {
    let mut b = [0; N];
    input.read_exact(&mut b)?;
    Ok(b)
}
fn length(input: &mut Input, wide: bool) -> Result<u64> {
    if wide {
        Ok(u64::from_be_bytes(bytes(input)?))
    } else {
        Ok(u64::from(u32::from_be_bytes(bytes(input)?)))
    }
}
fn section_end(input: &mut Input, file_size: u64, wide: bool) -> Result<u64> {
    let len = length(input, wide)?;
    input
        .stream_position()?
        .checked_add(len)
        .filter(|&p| p <= file_size)
        .ok_or_else(|| "PSD 区段长度超过文件边界".into())
}

fn merged_alpha(input: &mut Input, end: u64, psb: bool, fallback: bool) -> Result<bool> {
    if input.stream_position()? == end {
        return Ok(fallback);
    }
    let records_end = section_end(input, end, psb)?;
    if records_end - input.stream_position()? >= 2 && i16::from_be_bytes(bytes(input)?) < 0 {
        return Ok(true);
    }
    input.seek(SeekFrom::Start(records_end))?;
    if end - records_end < 4 {
        return Ok(false);
    }
    let mask_end = section_end(input, end, false)?;
    input.seek(SeekFrom::Start(mask_end))?;
    // 16/32 位通常把图层记录与合成透明度标记放在附加区。
    while end - input.stream_position()? >= 12 {
        if !matches!(&bytes::<4>(input)?, b"8BIM" | b"8B64") {
            break;
        }
        let key = bytes::<4>(input)?;
        let wide = psb
            && matches!(
                &key,
                b"LMsk"
                    | b"Lr16"
                    | b"Lr32"
                    | b"Layr"
                    | b"Mt16"
                    | b"Mt32"
                    | b"Mtrn"
                    | b"Alph"
                    | b"FMsk"
                    | b"lnk2"
                    | b"FEid"
                    | b"FXid"
                    | b"PxSD"
            );
        let data_end = section_end(input, end, wide)?;
        let size = data_end - input.stream_position()?;
        if matches!(&key, b"Mt16" | b"Mt32" | b"Mtrn")
            || (matches!(&key, b"Lr16" | b"Lr32" | b"Layr")
                && size >= 2
                && i16::from_be_bytes(bytes(input)?) < 0)
        {
            return Ok(true);
        }
        let next = data_end + (4 - size % 4) % 4;
        if next > end {
            break;
        }
        input.seek(SeekFrom::Start(next))?;
    }
    Ok(false)
}

fn layout(input: &mut Input) -> Result<Option<Composite>> {
    let h = bytes::<26>(input)?;
    let short = |i: usize| u16::from_be_bytes([h[i], h[i + 1]]);
    let version = short(4);
    let depth = short(22);
    let mode = short(24);
    if &h[..4] != b"8BPS" || !matches!(version, 1 | 2) {
        return Ok(None);
    }
    let channels = usize::from(short(12));
    let base = match (mode, depth) {
        (0, 1 | 8) | (1, 8 | 16 | 32) | (2 | 8, 8) | (7, 8 | 16) => 1,
        (3, 8 | 16 | 32) | (9, 8 | 16) => 3,
        (4, 8 | 16) => 4,
        _ => return Ok(None),
    };
    if channels < base || channels > 56 {
        return Err("Photoshop 通道数无效".into());
    }
    let height = u32::from_be_bytes(h[14..18].try_into()?);
    let width = u32::from_be_bytes(h[18..22].try_into()?);
    let psb = version == 2;
    let max = if psb { 300_000 } else { 30_000 };
    let row_bytes = (u64::from(width) * u64::from(depth)).div_ceil(8);
    if !(1..=max).contains(&width)
        || !(1..=max).contains(&height)
        || row_bytes * u64::from(height) * channels as u64 > LIMIT
        || u64::from(width) * u64::from(height) * 4 > LIMIT
    {
        return Err("PSD 合成图尺寸过大或无效".into());
    }
    let file_size = input.get_ref().metadata()?.len();
    let color_end = section_end(input, file_size, false)?;
    let mut palette = Vec::new();
    if mode == 2 {
        if color_end - input.stream_position()? != 768 {
            return Err("PSD 索引色表长度无效".into());
        }
        palette = bytes::<768>(input)?.to_vec();
    }
    input.seek(SeekFrom::Start(color_end))?;
    let resources_end = section_end(input, file_size, false)?;
    let mut profile = None;
    let mut transparent = None;
    while input.stream_position()? < resources_end {
        if bytes::<4>(input)? != *b"8BIM" {
            return Ok(None);
        }
        let id = u16::from_be_bytes(bytes(input)?);
        let name_len = u64::from(bytes::<1>(input)?[0]);
        input.seek_relative((name_len + (name_len + 1) % 2) as i64)?;
        let size = u32::from_be_bytes(bytes(input)?);
        let start = input.stream_position()?;
        let end = start + u64::from(size) + u64::from(size % 2);
        if end > resources_end {
            return Err("PSD 资源长度超过区段".into());
        }
        match id {
            1039 if size > 0 => {
                if size > 16 * 1024 * 1024 {
                    return Err("PSD 色彩配置过大".into());
                }
                profile = Some((start, size as usize));
            }
            1047 if size == 2 => transparent = u8::try_from(u16::from_be_bytes(bytes(input)?)).ok(),
            1057 if size >= 5 => {
                if bytes::<5>(input)?[4] == 0 {
                    return Err(
                        "PSD 未保存真实合成图，请在 Photoshop 中启用最大兼容性并保存".into(),
                    );
                }
            }
            _ => (),
        }
        input.seek(SeekFrom::Start(end))?;
    }
    let layers_end = section_end(input, file_size, psb)?;
    // 无图层标记的四通道 RGB 延用旧读法；专色及双色调按旧入口读取首个灰度通道。
    let alpha = channels > base
        && matches!(mode, 1..=4 | 9)
        && merged_alpha(input, layers_end, psb, mode == 3 && channels == 4)?;
    input.seek(SeekFrom::Start(layers_end))?;
    if file_size - layers_end < 2 {
        return Err("PSD 缺少合成图".into());
    }
    let compression = u16::from_be_bytes(bytes(input)?);
    if compression > 3 || (depth == 1 && compression == 3) {
        return Ok(None);
    }
    Ok(Some(Composite {
        width,
        height,
        channels,
        depth,
        mode,
        psb,
        compression,
        profile,
        palette,
        transparent,
        alpha,
    }))
}
pub(super) fn supports(path: &Path) -> bool {
    let parse = || layout(&mut BufReader::new(File::open(path)?));
    // 已识别但损坏或过大的 PSD 应让原生解码给出明确错误。
    !matches!(parse(), Ok(None))
}

fn unpack(mut src: &[u8], mut row: &mut [u8]) -> Result<()> {
    while let Some((&control, tail)) = src.split_first() {
        src = tail;
        if control == 128 {
            continue;
        }
        let repeat = control > 128;
        let count = if repeat {
            257 - usize::from(control)
        } else {
            usize::from(control) + 1
        };
        let (dst, rest) = row
            .split_at_mut_checked(count)
            .ok_or("PSD RLE 数据超出行宽")?;
        let used = if repeat { 1 } else { count };
        let (payload, remainder) = src.split_at_checked(used).ok_or("PSD RLE 数据不完整")?;
        if repeat {
            dst.fill(payload[0]);
        } else {
            dst.copy_from_slice(payload);
        }
        src = remainder;
        row = rest;
    }
    if !row.is_empty() {
        return Err("PSD RLE 行没有填满".into());
    }
    Ok(())
}

fn raster(input: &mut Input, c: &Composite) -> Result<Vec<u8>> {
    let width = c.width as usize;
    let stride = (width * usize::from(c.depth)).div_ceil(8);
    let rows = c.height as usize * c.channels;
    let size = rows * stride;
    let available = input.get_ref().metadata()?.len() - input.stream_position()?;
    let mut planes = Vec::new();
    match c.compression {
        0 => {
            if available < size as u64 {
                return Err("PSD 合成图数据不完整".into());
            }
            planes.resize(size, 0);
            input.read_exact(&mut planes)?;
        }
        1 => {
            let entry_size = if c.psb { 4 } else { 2 };
            if (rows * entry_size) as u64 > available {
                return Err("PSD RLE 行长度表不完整".into());
            }
            let mut lengths = Vec::with_capacity(rows);
            for _ in 0..rows {
                let n = if c.psb {
                    u32::from_be_bytes(bytes(input)?) as usize
                } else {
                    usize::from(u16::from_be_bytes(bytes(input)?))
                };
                if n > LIMIT as usize {
                    return Err("PSD RLE 行超出解码限制".into());
                }
                lengths.push(n);
            }
            if lengths.iter().map(|&n| n as u64).sum::<u64>()
                > available - (rows * entry_size) as u64
            {
                return Err("PSD 合成图数据不完整".into());
            }
            planes.resize(size, 0);
            let mut packed = Vec::new();
            for (line, n) in planes.chunks_exact_mut(stride).zip(lengths) {
                packed.resize(n, 0);
                input.read_exact(&mut packed)?;
                unpack(&packed, line)?;
            }
        }
        2 | 3 => {
            // 解压结果固定为所有平面的行数乘行宽，并检查 Zlib 流的结束标记。
            if available > LIMIT {
                return Err("PSD ZIP 数据超过解码限制".into());
            }
            let mut encoded = Vec::new();
            input.read_to_end(&mut encoded)?;
            planes.resize(size + 1, 0);
            let mut inflater = Decompress::new(true);
            let status = inflater.decompress(&encoded, &mut planes, FlushDecompress::Finish)?;
            if status != Status::StreamEnd || inflater.total_out() != size as u64 {
                return Err("PSD ZIP 数据不完整或尺寸不符".into());
            }
            planes.truncate(size);
            if c.compression == 3 {
                let mut tmp = vec![0; stride];
                for row in planes.chunks_exact_mut(stride) {
                    if c.depth == 16 {
                        let mut prev = 0u16;
                        for sample in row.chunks_exact_mut(2) {
                            prev = prev.wrapping_add(u16::from_be_bytes([sample[0], sample[1]]));
                            sample.copy_from_slice(&prev.to_be_bytes());
                        }
                    } else {
                        for i in 1..stride {
                            row[i] = row[i].wrapping_add(row[i - 1]);
                        }
                        if c.depth == 32 {
                            tmp.copy_from_slice(row);
                            for (x, sample) in row.chunks_exact_mut(4).enumerate() {
                                for b in 0..4 {
                                    sample[b] = tmp[b * width + x];
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(planes)
}

fn sample_order(count: usize, channels: usize) -> impl Iterator<Item = usize> {
    (0..count).flat_map(move |i| (0..channels).map(move |p| p * count + i))
}
fn rgb_or_gray(c: &Composite, data: &[u8]) -> Result<DynamicImage> {
    let n = c.width as usize * c.height as usize;
    let ch = if c.mode == 3 { 3 } else { 1 } + usize::from(c.alpha);
    let (w, h) = (c.width, c.height);
    macro_rules! make {
        ($variant:ident,$samples:expr)=>{DynamicImage::$variant(ImageBuffer::from_raw(w,h,$samples).ok_or("PSD 像素尺寸无效")?)};
    }
    Ok(match c.depth {
        8 => {
            let v = sample_order(n, ch).map(|i| data[i]).collect();
            match ch {
                1 => make!(ImageLuma8, v),
                2 => make!(ImageLumaA8, v),
                3 => make!(ImageRgb8, v),
                _ => make!(ImageRgba8, v),
            }
        }
        16 => {
            let v = sample_order(n, ch)
                .map(|i| u16::from_be_bytes([data[2 * i], data[2 * i + 1]]))
                .collect();
            match ch {
                1 => make!(ImageLuma16, v),
                2 => make!(ImageLumaA16, v),
                3 => make!(ImageRgb16, v),
                _ => make!(ImageRgba16, v),
            }
        }
        32 => {
            // DynamicImage 仅有 RGB/RGBA 浮点类型，灰度值在三个色彩通道保留原精度。
            let read = |i: usize| f32::from_be_bytes(data[4 * i..4 * i + 4].try_into().unwrap());
            let dst = 3 + usize::from(c.alpha);
            if n as u64 * dst as u64 * 4 > LIMIT {
                return Err("PSD 浮点合成图过大".into());
            }
            let mut v = Vec::with_capacity(n * dst);
            for i in 0..n {
                if c.mode == 3 {
                    v.extend([read(i), read(n + i), read(2 * n + i)]);
                } else {
                    v.extend([read(i); 3]);
                }
                if c.alpha {
                    v.push(read((ch - 1) * n + i));
                }
            }
            if c.alpha {
                make!(ImageRgba32F, v)
            } else {
                make!(ImageRgb32F, v)
            }
        }
        _ => return Err("灰度或 RGB 位深无效".into()),
    })
}

fn gray_profile_for_rgb(gray: &[u8]) -> Result<Vec<u8>> {
    // 浮点灰度在 DynamicImage 中须用三路相同的 RGB 值保存。沿用灰度曲线
    // 作为三个 RGB 通道的曲线，这样保留 HDR 原值也能正确显示。
    ColorProfile::new_from_slice(gray)?;
    let count = u32::from_be_bytes(gray.get(128..132).ok_or("灰度 ICC 头不完整")?.try_into()?);
    let entries = gray
        .get(132..132 + count as usize * 12)
        .ok_or("灰度 ICC 标签表不完整")?;
    let tag = entries
        .chunks_exact(12)
        .find(|t| &t[..4] == b"kTRC")
        .ok_or("不支持没有灰度曲线的浮点灰度 ICC")?;
    let start = u32::from_be_bytes(tag[4..8].try_into()?) as usize;
    let len = u32::from_be_bytes(tag[8..12].try_into()?) as usize;
    let curve = gray
        .get(start..start + len)
        .ok_or("灰度 ICC 曲线超出文件边界")?;
    let mut rgb = SRGB_ICC.to_vec();
    let new_start = rgb.len() as u32;
    let count = u32::from_be_bytes(rgb[128..132].try_into()?) as usize;
    for tag in rgb[132..132 + count * 12].chunks_exact_mut(12) {
        if matches!(&tag[..4], b"rTRC" | b"gTRC" | b"bTRC") {
            tag[4..8].copy_from_slice(&new_start.to_be_bytes());
            tag[8..12].copy_from_slice(&(len as u32).to_be_bytes());
        }
    }
    rgb.extend_from_slice(curve);
    rgb.resize(rgb.len().next_multiple_of(4), 0);
    let total = rgb.len() as u32;
    rgb[..4].copy_from_slice(&total.to_be_bytes());
    rgb[84..100].fill(0);
    Ok(rgb)
}

fn inks(
    c: &Composite,
    data: &[u8],
    profile: Option<Vec<u8>>,
) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let n = c.width as usize * c.height as usize;
    let cmyk = c.mode == 4;
    let channels = if cmyk { 4 } else { 3 };
    let src_layout = if cmyk { Layout::Rgba } else { Layout::Rgb };
    let source = if let Some(bytes) = profile.as_ref() {
        Some(ColorProfile::new_from_slice(bytes)?)
    } else if !cmyk {
        // 此通用配置使用单位 LUT，输入、输出两端均为 D50 Lab。
        let mut lab = ColorProfile::new_lab();
        lab.pcs = DataColorSpace::Lab;
        Some(lab)
    } else {
        None
    };
    // LittleCMS 内置的标准 sRGB 配置；输出 RGB 像素须与嵌入的配置一致。
    let srgb = ColorProfile::new_from_slice(SRGB_ICC)?;
    let output = source.as_ref().map(|_| SRGB_ICC.to_vec());
    macro_rules! render {
        ($t:ty, $read:expr, $transform:ident,$rgb:ident,$rgba:ident) => {{
            let max = <$t>::MAX;
            let sample = $read;
            let color: Vec<$t> = sample_order(n, channels)
                .map(|i| if cmyk { max - sample(i) } else { sample(i) })
                .collect();
            let mut rgb = vec![0; n * 3];
            if let Some(prof) = source {
                let transform =
                    prof.$transform(src_layout, &srgb, Layout::Rgb, TransformOptions::default())?;
                transform.transform(&color, &mut rgb)?;
            } else {
                for (src, dst) in color.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
                    for k in 0..3 {
                        dst[k] = ((u64::from(max - src[k]) * u64::from(max - src[3])
                            + u64::from(max) / 2)
                            / u64::from(max)) as $t;
                    }
                }
            }
            drop(color);
            if c.alpha {
                let mut rgba = Vec::with_capacity(4 * n);
                for (i, pixel) in rgb.chunks_exact(3).enumerate() {
                    rgba.extend_from_slice(pixel);
                    rgba.push(sample(channels * n + i));
                }
                DynamicImage::$rgba(ImageBuffer::from_raw(c.width, c.height, rgba).unwrap())
            } else {
                DynamicImage::$rgb(ImageBuffer::from_raw(c.width, c.height, rgb).unwrap())
            }
        }};
    }
    let image = if c.depth == 8 {
        render!(
            u8,
            |i: usize| data[i],
            create_transform_8bit,
            ImageRgb8,
            ImageRgba8
        )
    } else {
        render!(
            u16,
            |i: usize| u16::from_be_bytes([data[i * 2], data[i * 2 + 1]]),
            create_transform_16bit,
            ImageRgb16,
            ImageRgba16
        )
    };
    Ok((image, output))
}

pub(super) fn decode(path: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let mut input = BufReader::new(File::open(path)?);
    let c = layout(&mut input)?.ok_or("不支持此 Photoshop 合成图格式")?;
    let data = raster(&mut input, &c)?;
    let profile = if let Some((offset, len)) = c.profile {
        input.seek(SeekFrom::Start(offset))?;
        let mut b = vec![0; len];
        input.read_exact(&mut b)?;
        Some(if c.mode == 1 && c.depth == 32 {
            gray_profile_for_rgb(&b)?
        } else {
            b
        })
    } else {
        None
    };
    let n = c.width as usize * c.height as usize;
    let img = match c.mode {
        0 if c.depth == 1 => {
            let row = (c.width as usize).div_ceil(8);
            let pixels = (0..n)
                .map(|i| {
                    let x = i % c.width as usize;
                    let y = i / c.width as usize;
                    if data[y * row + x / 8] & (128 >> (x % 8)) != 0 {
                        0
                    } else {
                        255
                    }
                })
                .collect();
            DynamicImage::ImageLuma8(GrayImage::from_raw(c.width, c.height, pixels).unwrap())
        }
        0 | 1 | 3 | 7 | 8 => rgb_or_gray(&c, &data)?,
        2 => {
            let alpha = c.transparent.is_some() || c.alpha;
            let mut v = Vec::with_capacity(n * (3 + usize::from(alpha)));
            for (i, &index) in data[..n].iter().enumerate() {
                let j = usize::from(index);
                v.extend([c.palette[j], c.palette[256 + j], c.palette[512 + j]]);
                if alpha {
                    v.push(if Some(index) == c.transparent {
                        0
                    } else if c.alpha {
                        data[n + i]
                    } else {
                        255
                    });
                }
            }
            if alpha {
                DynamicImage::ImageRgba8(RgbaImage::from_raw(c.width, c.height, v).unwrap())
            } else {
                DynamicImage::ImageRgb8(RgbImage::from_raw(c.width, c.height, v).unwrap())
            }
        }
        4 | 9 => return inks(&c, &data, profile),
        _ => unreachable!(),
    };
    Ok((img, profile))
}
