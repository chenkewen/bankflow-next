use super::Result;
use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

pub(super) fn detect(header: &[u8]) -> bool {
    let start = header.trim_ascii_start();
    start.starts_with(b"#define") || start.starts_with(b"/*")
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let mut reader = BufReader::new(File::open(path)?);
    if reader
        .fill_buf()?
        .trim_ascii_start()
        .starts_with(b"/* XPM */")
    {
        xpm(reader)
    } else {
        xbm(reader)
    }
}

fn buffer(width: u32, height: u32, channels: usize) -> Result<Vec<u8>> {
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels))
        .filter(|n| *n > 0 && *n <= 512 * 1024 * 1024)
        .ok_or("图片尺寸无效或过大")?;
    let mut data = Vec::new();
    data.try_reserve_exact(len)?;
    Ok(data)
}

fn xbm(reader: impl BufRead) -> Result<DynamicImage> {
    let mut lines = reader.split(b'\n');
    let (mut width, mut height) = (None, None);
    let mut in_comment = false;
    let first_bytes = loop {
        let line = lines.next().ok_or("XBM 缺少位图数组")??;
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if in_comment || text.starts_with("/*") {
            in_comment = !text.contains("*/");
            continue;
        }
        let mut fields = text.split_ascii_whitespace();
        if fields.next() == Some("#define") {
            let name = fields.next().unwrap_or("");
            if name.ends_with("_width") || name.ends_with("_height") {
                let value = fields.next().ok_or("XBM 尺寸定义为空")?.parse::<u32>()?;
                if name.ends_with("_width") {
                    width = Some(value);
                } else {
                    height = Some(value);
                }
            }
        } else if let Some((declaration, bytes)) = text.split_once('{') {
            if declaration.contains("_bits") && declaration.contains('[') {
                break bytes.as_bytes().to_vec();
            }
        }
    };
    let (w, h) = (width.ok_or("XBM 缺少宽度")?, height.ok_or("XBM 缺少高度")?);
    let mut pixels = buffer(w, h, 1)?;
    let stride = w.div_ceil(8);
    let mut byte_column = 0;
    for line in std::iter::once(Ok(first_bytes)).chain(lines) {
        let line = line?;
        let text = std::str::from_utf8(&line)?;
        for token in text
            .split(|c: char| c == ',' || c.is_ascii_whitespace() || c == '}' || c == ';')
            .filter(|s| !s.is_empty())
        {
            let byte = u8::from_str_radix(
                token
                    .strip_prefix("0x")
                    .or_else(|| token.strip_prefix("0X"))
                    .ok_or("XBM 无效字节")?,
                16,
            )?;
            // XBM 从低位向高位填像素，行末最后一个字节可能还有补齐位。
            let count = (w - byte_column * 8).min(8);
            pixels.extend((0..count).map(|bit| if byte & (1 << bit) != 0 { 255 } else { 0 }));
            byte_column += 1;
            if byte_column == stride {
                byte_column = 0;
                if pixels.len() == w as usize * h as usize {
                    return Ok(DynamicImage::ImageLuma8(
                        GrayImage::from_raw(w, h, pixels).ok_or("XBM 位图尺寸无效")?,
                    ));
                }
            }
        }
    }
    Err("XBM 像素数据不完整".into())
}

fn xpm(reader: impl BufRead) -> Result<DynamicImage> {
    // 保留引号内的首尾空格与非 ASCII 字节，空格本身是合法的 XPM 色码。
    let mut strings = reader.split(b'\n').filter_map(|line| match line {
        Err(e) => Some(Err(e)),
        Ok(line) => {
            let text = line.trim_ascii_start();
            if text.starts_with(b"/*") {
                return None;
            }
            let first = text.iter().position(|&b| b == b'"')?;
            let last = text.iter().rposition(|&b| b == b'"')?;
            (last > first).then(|| Ok(text[first + 1..last].to_vec()))
        }
    });
    let header = strings.next().ok_or("XPM 缺少尺寸与调色板信息")??;
    let mut fields = std::str::from_utf8(&header)?.split_ascii_whitespace();
    let mut next = || -> Result<u32> { Ok(fields.next().ok_or("XPM 缺少头字段")?.parse()?) };
    let (w, h, colors, cpp) = (next()?, next()?, next()?, next()? as usize);
    if colors == 0 || cpp == 0 {
        return Err("XPM 调色板或色码长度为空".into());
    }
    let mut palette = HashMap::new();
    let mut transparent = None;
    for _ in 0..colors {
        let line = strings.next().ok_or("XPM 调色板不完整")??;
        let (key, spec) = line.split_at_checked(cpp).ok_or("XPM 色码长度不足")?;
        let mut fields = spec
            .split(u8::is_ascii_whitespace)
            .filter(|s| !s.is_empty());
        let color = loop {
            let tag = fields.next().ok_or("XPM 缺少颜色定义")?;
            let value = fields.next().ok_or("XPM 颜色定义不完整")?;
            if tag == b"c" {
                break value;
            }
        };
        let rgb = if color == b"None" {
            transparent = Some(key.to_vec());
            [0, 0, 0]
        } else {
            let hex = color
                .strip_prefix(b"#")
                .filter(|v| !v.is_empty() && v.iter().all(u8::is_ascii_hexdigit))
                .ok_or("XPM 颜色须为十六进制或 None")?;
            // 与旧解码器一致，取整数色值的低 24 位；#RGB 亦按该规则读取。
            let value = u32::from_str_radix(
                std::str::from_utf8(&hex[hex.len().saturating_sub(6)..])?,
                16,
            )?;
            [(value >> 16) as u8, (value >> 8) as u8, value as u8]
        };
        palette.insert(key.to_vec(), rgb);
    }
    let alpha = transparent.is_some();
    let mut pixels = buffer(w, h, if alpha { 4 } else { 3 })?;
    let row_len = (w as usize).checked_mul(cpp).ok_or("XPM 行过长")?;
    for _ in 0..h {
        let line = strings.next().ok_or("XPM 缺少像素行")??;
        if line.len() != row_len {
            return Err("XPM 像素行长度与宽度不符".into());
        }
        for key in line.chunks_exact(cpp) {
            pixels.extend_from_slice(palette.get(key).ok_or("XPM 像素引用了未知颜色")?);
            if alpha {
                pixels.push(if transparent.as_deref() == Some(key) {
                    0
                } else {
                    255
                });
            }
        }
    }
    if alpha {
        Ok(DynamicImage::ImageRgba8(
            RgbaImage::from_raw(w, h, pixels).ok_or("XPM 图像尺寸无效")?,
        ))
    } else {
        Ok(DynamicImage::ImageRgb8(
            RgbImage::from_raw(w, h, pixels).ok_or("XPM 图像尺寸无效")?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xbm_resets_bit_position_each_row() {
        let bytes=b"#define im_width 10\n#define im_height 2\nstatic char im_bits[] = {\n0x01,0x02,0x80,0xfd\n};";
        assert_eq!(
            xbm(&bytes[..]).unwrap().to_luma8().as_raw(),
            &[255, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 255, 255, 0]
        );
    }
    #[test]
    fn xpm_reads_two_byte_keys_and_transparent_spaces() {
        let bytes=b"/* XPM */\nstatic char *icon[] = {\n\"2 1 2 2\",\n\"   c None\",\n\"[] c #123456\",\n/* pixels */\n\"[]  \"};";
        assert_eq!(
            xpm(&bytes[..]).unwrap().to_rgba8().as_raw(),
            &[18, 52, 86, 255, 0, 0, 0, 0]
        );
    }
}
