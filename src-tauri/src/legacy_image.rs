#[path = "image_scientific.rs"]
mod scientific;
use super::Result;
use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const DCX: [u8; 4] = 0x3ade68b1u32.to_le_bytes();
const PIXAR: [u8; 4] = [0x80, 0xe8, 0, 0];
const LIMIT: usize = 512 * 1024 * 1024;

fn gimp(header: &[u8]) -> bool {
    header.get(..8).is_some_and(|b| {
        u32::from_be_bytes(b[..4].try_into().unwrap()) >= 20
            && matches!(&b[4..8], [0, 0, 0, 1] | [0, 0, 0, 2])
    })
}

pub(super) fn detect(header: &[u8], path: &Path) -> bool {
    header.starts_with(b"SIMPLE  =")
        || (header.starts_with(&[255; 32])
            && File::open(path).is_ok_and(|mut f| {
                f.seek(SeekFrom::Start(2048)).is_ok()
                    && bytes::<4>(&mut f).is_ok_and(|tag| tag == *b"PCD_")
            }))
        || scientific::spider_order(header).is_some()
        || header.starts_with(&DCX)
        || header.starts_with(&PIXAR)
        || header.starts_with(b"DanM")
        || header.starts_with(b"LinS")
        || gimp(header)
}

fn bytes<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut data = [0; N];
    r.read_exact(&mut data)?;
    Ok(data)
}

fn length(w: u32, h: u32, channels: usize) -> Result<usize> {
    (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(channels))
        .filter(|n| (1..=LIMIT).contains(n))
        .ok_or_else(|| "旧图片尺寸无效或过大".into())
}

fn raw(
    r: &mut BufReader<File>,
    offset: u64,
    w: u32,
    h: u32,
    channels: usize,
) -> Result<DynamicImage> {
    let size = length(w, h, channels)?;
    let file_size = r.get_ref().metadata()?.len();
    if offset
        .checked_add(size as u64)
        .is_none_or(|end| end > file_size)
    {
        return Err("旧图片的像素数据不完整".into());
    }
    r.seek(SeekFrom::Start(offset))?;
    let mut pixels = vec![0; size];
    r.read_exact(&mut pixels)?;
    Ok(match channels {
        1 => DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, pixels).ok_or("灰度像素数量错误")?),
        3 => DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, pixels).ok_or("RGB 像素数量错误")?),
        4 => {
            DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, pixels).ok_or("RGBA 像素数量错误")?)
        }
        _ => return Err("不支持此像素布局".into()),
    })
}

fn msp(r: &mut BufReader<File>, compressed: bool) -> Result<DynamicImage> {
    r.rewind()?;
    let header = bytes::<32>(r)?;
    let le = |i: usize| u16::from_le_bytes([header[i], header[i + 1]]);
    if (0..32).step_by(2).fold(0u16, |sum, i| sum ^ le(i)) != 0 {
        return Err("MSP 头校验失败".into());
    }
    let (w, h) = (u32::from(le(4)), u32::from(le(6)));
    let total = length(w, h, 1)?;
    let stride = w.div_ceil(8) as usize;
    let mut row_lengths = Vec::new();
    if compressed {
        for _ in 0..h {
            row_lengths.push(usize::from(u16::from_le_bytes(bytes(r)?)));
        }
    }
    let mut pixels = Vec::with_capacity(total);
    let mut row = vec![0; stride];
    let mut encoded = Vec::new();
    for y in 0..h as usize {
        if !compressed {
            r.read_exact(&mut row)?;
        } else if row_lengths[y] == 0 {
            row.fill(255);
        } else {
            encoded.resize(row_lengths[y], 0);
            r.read_exact(&mut encoded)?;
            unpack_msp(&encoded, &mut row)?;
        }
        pixels.extend((0..w as usize).map(|x| {
            if row[x / 8] & (0x80 >> (x % 8)) != 0 {
                255
            } else {
                0
            }
        }));
    }
    Ok(DynamicImage::ImageLuma8(
        GrayImage::from_raw(w, h, pixels).ok_or("MSP 像素数量错误")?,
    ))
}

fn unpack_msp(mut src: &[u8], mut dest: &mut [u8]) -> Result<()> {
    while let Some((&literal, rest)) = src.split_first() {
        src = rest;
        if literal == 0 {
            let (run, rest) = src.split_at_checked(2).ok_or("MSP 重复段不完整")?;
            let (out, remaining) = dest
                .split_at_mut_checked(usize::from(run[0]))
                .ok_or("MSP 重复段超出行宽")?;
            out.fill(run[1]);
            dest = remaining;
            src = rest;
        } else {
            let count = usize::from(literal);
            let (data, rest) = src.split_at_checked(count).ok_or("MSP 像素段不完整")?;
            let (out, remaining) = dest
                .split_at_mut_checked(count)
                .ok_or("MSP 像素段超出行宽")?;
            out.copy_from_slice(data);
            dest = remaining;
            src = rest;
        }
    }
    if !dest.is_empty() {
        return Err("MSP 行不完整".into());
    }
    Ok(())
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let mut r = BufReader::new(File::open(path)?);
    let magic = bytes::<4>(&mut r)?;
    if magic == *b"SIMP" {
        return scientific::fits(&mut r);
    }
    if magic == [255; 4] {
        return scientific::photo_cd(&mut r);
    }
    r.rewind()?;
    let mut probe = [0; 108];
    if r.read_exact(&mut probe).is_ok() {
        if let Some(big) = scientific::spider_order(&probe) {
            return scientific::spider(&mut r, &probe, big);
        }
    }
    r.seek(SeekFrom::Start(4))?;
    if magic == DCX {
        let offset = u32::from_le_bytes(bytes(&mut r)?);
        let second = u32::from_le_bytes(bytes(&mut r)?);
        if offset < 12 {
            return Err("DCX 页偏移无效".into());
        }
        if second != 0 {
            return Err("暂不支持多帧图片，请先拆分帧".into());
        }
        r.seek(SeekFrom::Start(u64::from(offset)))?;
        let mut pcx = pcx::Reader::new(r)?;
        let (w, h) = (u32::from(pcx.width()), u32::from(pcx.height()));
        let mut pixels = vec![0; length(w, h, 3)?];
        pcx.read_rgb_pixels(&mut pixels)?;
        return Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, h, pixels).ok_or("DCX 像素数量错误")?,
        ));
    }
    if magic == PIXAR {
        r.seek(SeekFrom::Start(416))?;
        let h = u32::from(u16::from_le_bytes(bytes(&mut r)?));
        let w = u32::from(u16::from_le_bytes(bytes(&mut r)?));
        r.seek(SeekFrom::Start(424))?;
        if bytes::<4>(&mut r)? != [14, 0, 2, 0] {
            return Err("不支持此 Pixar 通道布局".into());
        }
        return raw(&mut r, 1024, w, h, 3);
    }
    if magic == *b"DanM" || magic == *b"LinS" {
        return msp(&mut r, magic == *b"LinS");
    }
    let offset = u32::from_be_bytes(magic);
    let version = u32::from_be_bytes(bytes(&mut r)?);
    let w = u32::from_be_bytes(bytes(&mut r)?);
    let h = u32::from_be_bytes(bytes(&mut r)?);
    let channels = u32::from_be_bytes(bytes(&mut r)?);
    if !(matches!(version, 1 | 2) && offset >= 20 && matches!(channels, 1 | 4)) {
        return Err("不支持此 GIMP 笔刷头".into());
    }
    if version == 2 && (offset < 28 || bytes::<4>(&mut r)? != *b"GIMP") {
        return Err("GIMP 笔刷标记错误".into());
    }
    raw(&mut r, u64::from(offset), w, h, channels as usize)
}
