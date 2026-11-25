use super::Result;
use image::{DynamicImage, RgbImage, Rgba, RgbaImage};
use std::{fs::File, io::Read, path::Path};

const LIMIT: u64 = 512 * 1024 * 1024;

fn take<'a>(data: &mut &'a [u8], size: usize) -> Result<&'a [u8]> {
    let (head, tail) = data.split_at_checked(size).ok_or("游戏图像数据截断")?;
    *data = tail;
    Ok(head)
}
fn u16_at(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        data.get(offset..offset + 2)
            .ok_or("缺少图像头字段")?
            .try_into()?,
    ))
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(offset..offset + 4)
            .ok_or("缺少图像头字段")?
            .try_into()?,
    ))
}
fn pixels(w: u32, h: u32) -> Result<usize> {
    let n = u64::from(w) * u64::from(h);
    if n == 0 || n > LIMIT / 4 {
        return Err("游戏图像尺寸无效或过大".into());
    }
    Ok(n as usize)
}
pub(super) fn detect(header: &[u8]) -> bool {
    matches!(header.get(..4), Some(b"BLP1" | b"BLP2" | b"FTEX"))
        || matches!(header.get(4..6), Some([0x11 | 0x12, 0xaf]))
            && matches!(header.get(14..16), Some([0 | 3, 0]))
}
pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let file = File::open(path)?;
    let mut data = Vec::new();
    if file.metadata()?.len() > LIMIT {
        return Err("游戏图像文件过大".into());
    }
    file.take(LIMIT + 1).read_to_end(&mut data)?;
    if data.len() as u64 > LIMIT {
        return Err("游戏图像文件过大".into());
    }
    match data.get(..4) {
        Some(b"BLP1" | b"BLP2") => blp(&data),
        Some(b"FTEX") => ftex(&data),
        _ => fli(&data),
    }
}

fn ftex(data: &[u8]) -> Result<DynamicImage> {
    let (w, h) = (u32_at(data, 8)?, u32_at(data, 12)?);
    let n = pixels(w, h)?;
    if u32_at(data, 20)? != 1 {
        return Err("暂不支持多种像素格式的 FTEX".into());
    }
    let mut at = data
        .get(u32_at(data, 28)? as usize..)
        .ok_or("FTEX 偏移无效")?;
    let len = u32_at(take(&mut at, 4)?, 0)? as usize;
    let mut level = take(&mut at, len)?;
    match u32_at(data, 24)? {
        0 => blocks(w, h, level, 0, true, true),
        1 => Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, h, take(&mut level, n * 3)?.to_vec()).ok_or("FTEX 像素无效")?,
        )),
        _ => Err("不支持的 FTEX 像素格式".into()),
    }
}

fn palette_image(
    w: u32,
    h: u32,
    index: &[u8],
    palette: &[[u8; 4]; 256],
    alpha: bool,
) -> DynamicImage {
    let mut out = RgbaImage::new(w, h);
    for (px, &i) in out.pixels_mut().zip(index) {
        *px = Rgba(palette[usize::from(i)])
    }
    let rgb = DynamicImage::ImageRgba8(out);
    if alpha {
        rgb
    } else {
        DynamicImage::ImageRgb8(rgb.into_rgb8())
    }
}

fn blp(data: &[u8]) -> Result<DynamicImage> {
    let (w, h) = (u32_at(data, 12)?, u32_at(data, 16)?);
    let n = pixels(w, h)?;
    let first = data.starts_with(b"BLP1");
    let alpha = if first {
        u32_at(data, 8)? != 0
    } else {
        data[9] != 0
    };
    let table = if first { 28 } else { 20 };
    let off = u32_at(data, table)? as usize;
    let len = u32_at(data, table + 64)? as usize;
    let mut body = data.get(off..).ok_or("BLP 首层偏移无效")?;
    let mut level = take(&mut body, len)?;
    let mut extra = data.get(table + 128..off).ok_or("BLP 首层与目录重叠")?;
    let ty = u32_at(data, 4)?;
    if first && ty == 0 {
        let head = u32_at(take(&mut extra, 4)?, 0)? as usize;
        let mut jpeg = take(&mut extra, head)?.to_vec();
        jpeg.extend_from_slice(level);
        return blp_jpeg(w, h, &jpeg, alpha);
    }
    if ty != 1 {
        return Err("不支持的 BLP 压缩类型".into());
    }
    if !first && data[8] == 2 {
        return blocks(w, h, level, data[10], alpha, false);
    }
    if !(first && matches!(u32_at(data, 20)?, 4 | 5) || !first && data[8] == 1) {
        return Err("不支持的 BLP 像素格式".into());
    }
    let pal = take(&mut extra, 1024)?;
    let palette =
        std::array::from_fn(|i| [pal[i * 4 + 2], pal[i * 4 + 1], pal[i * 4], pal[i * 4 + 3]]);
    Ok(palette_image(w, h, take(&mut level, n)?, &palette, alpha))
}

fn blp_jpeg(w: u32, h: u32, data: &[u8], alpha: bool) -> Result<DynamicImage> {
    use zune_jpeg::{
        zune_core::{bytestream::ZCursor, colorspace::ColorSpace},
        JpegDecoder,
    };
    let mut decoder = JpegDecoder::new(ZCursor::new(data));
    decoder.decode_headers()?;
    if decoder.dimensions() != Some((w as usize, h as usize)) {
        return Err("BLP 与内嵌 JPEG 尺寸不符".into());
    }
    let cmyk = decoder.input_colorspace() == Some(ColorSpace::CMYK);
    if decoder.input_colorspace() == Some(ColorSpace::YCCK) {
        return Err("暂不支持 YCCK 型 BLP JPEG".into());
    }
    let options = decoder.options().jpeg_set_out_colorspace(if cmyk {
        ColorSpace::CMYK
    } else {
        ColorSpace::RGB
    });
    decoder.set_options(options);
    let mut samples = decoder.decode()?;
    if cmyk {
        // BLP 的 CMYK 极性沿用 JPEG 原始样本，输出顺序是 BGR。
        samples = samples
            .chunks_exact(4)
            .flat_map(|p| {
                let c = |x: u8| ((u16::from(x) * u16::from(p[3]) + 127) / 255) as u8;
                [c(p[2]), c(p[1]), c(p[0])]
            })
            .collect();
    } else {
        for rgb in samples.chunks_exact_mut(3) {
            rgb.swap(0, 2)
        }
    }
    let image =
        DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, samples).ok_or("BLP JPEG 像素无效")?);
    Ok(if alpha {
        DynamicImage::ImageRgba8(image.into_rgba8())
    } else {
        image
    })
}

fn blocks(
    w: u32,
    h: u32,
    mut data: &[u8],
    kind: u8,
    alpha: bool,
    full_range: bool,
) -> Result<DynamicImage> {
    if !matches!(kind, 0 | 1 | 7) {
        return Err("不支持的 DXT 透明度编码".into());
    }
    let size = if kind == 0 { 8 } else { 16 };
    let size_needed = (w.div_ceil(4) as usize) * (h.div_ceil(4) as usize) * size;
    if data.len() < size_needed {
        return Err("DXT 块数据截断".into());
    }
    let mut out = RgbaImage::new(w, h);
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let block = take(&mut data, size)?;
            let colors = &block[size - 8..];
            let (a, b) = (u16_at(colors, 0)?, u16_at(colors, 2)?);
            let color = |v: u16| {
                let r = ((v >> 11) as u8) << 3;
                let g = ((v >> 5) as u8 & 63) << 2;
                let b = ((v as u8) & 31) << 3;
                if full_range {
                    [r | r >> 5, g | g >> 6, b | b >> 5]
                } else {
                    [r, g, b]
                }
            };
            let first = color(a);
            let second = color(b);
            let four = kind != 0 || a > b;
            let blend = |wa: u16, wb: u16| {
                std::array::from_fn(|i| {
                    ((wa * u16::from(first[i]) + wb * u16::from(second[i])) / (wa + wb)) as u8
                })
            };
            let palette = [
                first,
                second,
                if four { blend(2, 1) } else { blend(1, 1) },
                if four { blend(1, 2) } else { [0; 3] },
            ];
            let selectors = u32_at(colors, 4)?;
            let alphas = u64::from_le_bytes(block[..8].try_into()?);
            for oy in 0..4.min(h - y) {
                for ox in 0..4.min(w - x) {
                    let i = oy * 4 + ox;
                    let idx = ((selectors >> (2 * i)) & 3) as usize;
                    let opacity = match kind {
                        0 => {
                            if !four && idx == 3 {
                                0
                            } else {
                                255
                            }
                        }
                        1 => (((alphas >> (4 * i)) & 15) * 17) as u8,
                        _ => {
                            let a = u16::from(block[0]);
                            let b = u16::from(block[1]);
                            let k = ((alphas >> (16 + 3 * i)) & 7) as u16;
                            (match k {
                                0 => a,
                                1 => b,
                                _ if a > b => ((8 - k) * a + (k - 1) * b) / 7,
                                6 => 0,
                                7 => 255,
                                _ => ((6 - k) * a + (k - 1) * b) / 5,
                            }) as u8
                        }
                    };
                    let [r, g, b] = palette[idx];
                    out.put_pixel(x + ox, y + oy, Rgba([r, g, b, opacity]));
                }
            }
        }
    }
    let image = DynamicImage::ImageRgba8(out);
    Ok(if alpha {
        image
    } else {
        DynamicImage::ImageRgb8(image.into_rgb8())
    })
}

fn fli(data: &[u8]) -> Result<DynamicImage> {
    if !detect(data) {
        return Err("FLI 文件头无效".into());
    }
    let frames = u16_at(data, 6)?;
    if frames != 1 {
        return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into());
    }
    let (w, h) = (u32::from(u16_at(data, 8)?), u32::from(u16_at(data, 10)?));
    let mut indices = vec![0; pixels(w, h)?];
    let mut palette = std::array::from_fn(|i| [i as u8, i as u8, i as u8, 255]);
    let mut stream = data.get(128..).ok_or("FLI 文件头截断")?;
    if u16_at(stream, 4)? == 0xf100 {
        let size = u32_at(stream, 0)? as usize;
        take(&mut stream, size)?;
    }
    let frame_size = u32_at(stream, 0)? as usize;
    // 部分编码器计入了最后一个字对齐填充字节，但在文件结尾省略它。
    let available = if frame_size % 2 == 0 && stream.len().checked_add(1) == Some(frame_size) {
        stream.len()
    } else {
        frame_size
    };
    let mut frame = take(&mut stream, available)?;
    let hdr = take(&mut frame, 16)?;
    if u16_at(hdr, 4)? != 0xf1fa {
        return Err("FLI 帧头无效".into());
    }
    for _ in 0..u16_at(hdr, 6)? {
        let head = take(&mut frame, 6)?;
        let len = (u32_at(head, 0)? as usize)
            .checked_sub(6)
            .ok_or("FLI 数据块长度无效")?;
        let mut chunk = take(&mut frame, len)?;
        match u16_at(head, 4)? {
            ty @ (4 | 11) => {
                let num = u16_at(take(&mut chunk, 2)?, 0)?;
                let mut i = 0;
                for _ in 0..num {
                    let packet = take(&mut chunk, 2)?;
                    i += usize::from(packet[0]);
                    let n = if packet[1] == 0 {
                        256
                    } else {
                        usize::from(packet[1])
                    };
                    for (entry, color) in palette
                        .get_mut(i..i + n)
                        .ok_or("FLI 调色板索引越界")?
                        .iter_mut()
                        .zip(take(&mut chunk, n * 3)?.chunks_exact(3))
                    {
                        for j in 0..3 {
                            entry[j] = color[j]
                                .checked_mul(if ty == 11 { 4 } else { 1 })
                                .ok_or("FLI 调色板无效")?
                        }
                    }
                    i += n;
                }
            }
            13 => indices.fill(0),
            12 => {
                let skip = usize::from(u16_at(take(&mut chunk, 2)?, 0)?);
                let lines = usize::from(u16_at(take(&mut chunk, 2)?, 0)?);
                if skip + lines > h as usize {
                    return Err("FLI 差分行越界".into());
                }
                for y in skip..skip + lines {
                    let mut x = 0;
                    for _ in 0..take(&mut chunk, 1)?[0] {
                        let desc = take(&mut chunk, 2)?;
                        x += usize::from(desc[0]);
                        let count = desc[1] as i8;
                        let end = x + usize::from(count.unsigned_abs());
                        if end > w as usize {
                            return Err("FLI 差分像素越界".into());
                        }
                        let start = y * w as usize;
                        let dest = &mut indices[start + x..start + end];
                        if count < 0 {
                            dest.fill(take(&mut chunk, 1)?[0]);
                        } else {
                            dest.copy_from_slice(take(&mut chunk, end - x)?);
                        }
                        x = end;
                    }
                }
            }
            7 => {
                let lines = u16_at(take(&mut chunk, 2)?, 0)?;
                let stride = w as usize;
                let mut y = 0usize;
                for _ in 0..lines {
                    let packets = loop {
                        let op = u16_at(take(&mut chunk, 2)?, 0)?;
                        match op >> 14 {
                            3 => y += usize::from((op as i16).unsigned_abs()),
                            2 if y < h as usize => indices[(y + 1) * stride - 1] = op as u8,
                            0 if y < h as usize => break op,
                            _ => return Err("FLC 差分行指令无效".into()),
                        }
                    };
                    let mut x = 0;
                    for _ in 0..packets {
                        let desc = take(&mut chunk, 2)?;
                        x += usize::from(desc[0]);
                        let count = desc[1] as i8;
                        let end = x + 2 * usize::from(count.unsigned_abs());
                        if end > stride {
                            return Err("FLC 差分字越界".into());
                        }
                        let dest = &mut indices[y * stride + x..y * stride + end];
                        if count < 0 {
                            let word = take(&mut chunk, 2)?;
                            for target in dest.chunks_exact_mut(2) {
                                target.copy_from_slice(word);
                            }
                        } else {
                            dest.copy_from_slice(take(&mut chunk, end - x)?);
                        }
                        x = end;
                    }
                    y += 1;
                }
            }
            15 => {
                for row in indices.chunks_exact_mut(w as usize) {
                    take(&mut chunk, 1)?;
                    let mut target = row;
                    while !target.is_empty() {
                        let count = take(&mut chunk, 1)?[0] as i8;
                        if count == 0 {
                            return Err("FLI 重复长度无效".into());
                        }
                        let (part, rest) = target
                            .split_at_mut_checked(usize::from(count.unsigned_abs()))
                            .ok_or("FLI 行越界")?;
                        if count > 0 {
                            part.fill(take(&mut chunk, 1)?[0])
                        } else {
                            part.copy_from_slice(take(&mut chunk, part.len())?)
                        }
                        target = rest;
                    }
                }
            }
            16 => {
                let size = indices.len();
                indices.copy_from_slice(take(&mut chunk, size)?)
            }
            18 => {}
            _ => return Err("暂不支持此 FLI 帧压缩类型".into()),
        }
    }
    Ok(palette_image(w, h, &indices, &palette, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animation(ty: u16, payload: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; 128];
        header[4..6].copy_from_slice(&0xaf12u16.to_le_bytes());
        header[6..8].copy_from_slice(&1u16.to_le_bytes());
        header[8..10].copy_from_slice(&4u16.to_le_bytes());
        header[10..12].copy_from_slice(&1u16.to_le_bytes());
        header[12..14].copy_from_slice(&8u16.to_le_bytes());
        let frame = 22 + payload.len() as u32;
        header[..4].copy_from_slice(&(128 + frame).to_le_bytes());
        header.extend(frame.to_le_bytes());
        header.extend(0xf1fau16.to_le_bytes());
        header.extend(1u16.to_le_bytes());
        header.extend([0; 8]);
        header.extend((6 + payload.len() as u32).to_le_bytes());
        header.extend(ty.to_le_bytes());
        header.extend_from_slice(payload);
        header
    }

    #[test]
    fn fli_copy_rle_and_delta_preserve_pixels_and_reject_extra_frames() {
        for (kind, payload) in [
            (16, &[1, 2, 3, 4][..]),
            (15, &[1, 252, 1, 2, 3, 4]),
            (12, &[0, 0, 1, 0, 1, 0, 4, 1, 2, 3, 4]),
            (7, &[1, 0, 1, 0, 0, 2, 1, 2, 3, 4]),
        ] {
            let data = animation(kind, payload);
            let img = fli(&data).unwrap().into_luma8();
            assert_eq!(img.as_raw(), &[1, 2, 3, 4]);
        }
        let mut two = animation(13, &[]);
        two[6] = 2;
        assert!(fli(&two).unwrap_err().to_string().contains("多帧"));
        assert!(fli(&animation(15, &[1, 5, 1])).is_err());
    }

    #[test]
    fn dxt_transparency_and_partial_edge_blocks() {
        let mut data = vec![0; 16];
        data[4..8].fill(255);
        data[8..10].copy_from_slice(&0xf800u16.to_le_bytes());
        let image = blocks(5, 3, &data, 0, true, true).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (5, 3));
        assert_eq!(image[(3, 2)].0, [0, 0, 0, 0]);
        assert_eq!(image[(4, 2)].0, [255, 0, 0, 255]);
        assert!(blocks(5, 3, &data[..15], 0, true, true).is_err());
    }
}
