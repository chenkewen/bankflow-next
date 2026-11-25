use super::Result;
use image::{DynamicImage, GenericImageView, ImageDecoder, RgbaImage};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Cursor, Read, Seek, SeekFrom},
    path::Path,
};

pub(super) fn decode(path: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let mut f = BufReader::new(File::open(path)?);
    let mut header = [0; 8];
    f.read_exact(&mut header)?;
    let total = u64::from(u32::from_be_bytes(header[4..].try_into()?));
    if total > f.get_ref().metadata()?.len() {
        return Err("ICNS 文件不完整".into());
    }
    let mut blocks = HashMap::new();
    let mut offset = 8;
    while offset < total {
        f.read_exact(&mut header)?;
        let size = u64::from(u32::from_be_bytes(header[4..].try_into()?));
        if size < 8 || offset + size > total {
            return Err("ICNS 资源长度无效".into());
        }
        blocks.insert(<[u8; 4]>::try_from(&header[..4])?, (offset + 8, size - 8));
        offset += size;
        f.seek(SeekFrom::Start(offset))?;
    }
    let mut block = |entry: (u64, u64)| -> Result<Vec<u8>> {
        if entry.1 > 64 << 20 {
            return Err("ICNS 图像资源过大".into());
        }
        f.seek(SeekFrom::Start(entry.0))?;
        let mut a = vec![0; entry.1 as usize];
        f.read_exact(&mut a)?;
        Ok(a)
    };
    // 按旧入口的逻辑尺寸和缩放倍率优先级选择默认图标。
    for (key, n, mask) in [
        (b"ic10", 1024, None),
        (b"ic09", 512, None),
        (b"ic14", 512, None),
        (b"ic08", 256, None),
        (b"ic13", 256, None),
        (b"ic07", 128, None),
        (b"it32", 128, Some(b"t8mk")),
        (b"icp6", 64, None),
        (b"ih32", 48, Some(b"h8mk")),
        (b"ic12", 64, None),
        (b"icp5", 32, None),
        (b"il32", 32, Some(b"l8mk")),
        (b"ic11", 32, None),
        (b"icp4", 16, None),
        (b"is32", 16, Some(b"s8mk")),
    ] {
        let Some(&entry) = blocks.get(key) else {
            continue;
        };
        let data = block(entry)?;
        if mask.is_none() {
            let (img, profile) = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
                let mut png = image::codecs::png::PngDecoder::new(Cursor::new(&data))?;
                if png.dimensions() != (n, n) {
                    return Err("ICNS 图像尺寸与目录不符".into());
                }
                let profile = png.icc_profile()?;
                (DynamicImage::from_decoder(png)?, profile)
            } else {
                let jp = openjp2::Image::from_bytes(&data)?;
                (DynamicImage::try_from(&jp)?, None)
            };
            if img.dimensions() != (n, n) {
                return Err("ICNS 图像尺寸与目录不符".into());
            }
            return Ok((img, profile));
        }
        let mask = mask.unwrap();
        let mut data = if key == b"it32" {
            data.strip_prefix(&[0; 4])
                .ok_or("ICNS 128 像素图标头无效")?
        } else {
            &data
        };
        let pixels = (n * n) as usize;
        let mut rgba = vec![255; pixels * 4];
        if data.len() == pixels * 3 {
            for (p, rgb) in rgba.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                p[..3].copy_from_slice(rgb);
            }
        } else {
            for band in 0..3 {
                let mut i = 0;
                while i < pixels {
                    let (&code, rest) = data.split_first().ok_or("ICNS RLE 数据不完整")?;
                    let repeated = code >= 128;
                    let len = if repeated {
                        usize::from(code) - 125
                    } else {
                        usize::from(code) + 1
                    };
                    if i + len > pixels {
                        return Err("ICNS RLE 通道长度无效".into());
                    }
                    let (bytes, rest) = rest
                        .split_at_checked(if repeated { 1 } else { len })
                        .ok_or("ICNS RLE 数据不完整")?;
                    for j in 0..len {
                        rgba[4 * (i + j) + band] = bytes[if repeated { 0 } else { j }];
                    }
                    i += len;
                    data = rest;
                }
            }
        }
        if let Some(&entry) = blocks.get(mask) {
            let alpha = block(entry)?;
            if alpha.len() < pixels {
                return Err("ICNS 透明蒙版数据不完整".into());
            }
            for (p, &a) in rgba.chunks_exact_mut(4).zip(&alpha) {
                p[3] = a;
            }
        }
        return Ok((
            DynamicImage::ImageRgba8(RgbaImage::from_raw(n, n, rgba).unwrap()),
            None,
        ));
    }
    Err("ICNS 没有支持的彩色图标".into())
}
