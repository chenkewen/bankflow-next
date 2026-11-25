use super::Result;
use image::{DynamicImage, RgbImage};
use std::{fs::File, io::Read, path::Path};
use windows::Win32::{
    Foundation::RECT, Graphics::Gdi::*, System::DataExchange::SetWinMetaFileBits,
};

pub(super) fn detect(header: &[u8]) -> bool {
    header.starts_with(&[0xd7, 0xcd, 0xc6, 0x9a, 0, 0])
        || header.starts_with(&1u32.to_le_bytes()) && header.get(40..44) == Some(b" EMF")
}

#[derive(Default)]
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    metafile: HENHMETAFILE,
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // 位图仍选入 DC 时不能删除。失败路径也按创建顺序逆序释放。
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
            if !self.metafile.is_invalid() {
                let _ = DeleteEnhMetaFile(Some(self.metafile));
            }
        }
    }
}

pub(super) fn decode(path: &Path) -> Result<DynamicImage> {
    let file = File::open(path)?;
    let mut data = Vec::new();
    file.take((512 << 20) + 1).read_to_end(&mut data)?;
    if data.len() < 44 || data.len() > 512 << 20 || !detect(&data) {
        return Err("WMF / EMF 文件尺寸或头部无效".into());
    }
    let placeable = data[0] == 0xd7;
    let (w, h) = if placeable {
        if data[22..26] != [1, 0, 9, 0] {
            return Err("不支持此 WMF 文件头".into());
        }
        let inch = u16::from_le_bytes([data[14], data[15]]) as i64;
        if inch == 0 {
            return Err("WMF 每英寸单位数无效".into());
        }
        let short = |i| i16::from_le_bytes([data[i], data[i + 1]]) as i64;
        (
            (short(10) - short(6)) * 72 / inch,
            (short(12) - short(8)) * 72 / inch,
        )
    } else {
        let long = |i: usize| i32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as i64;
        (long(16) - long(8), long(20) - long(12))
    };
    if !(1..=65535).contains(&w) || !(1..=65535).contains(&h) || w * h * 4 > 512 << 20 {
        return Err("WMF / EMF 画布尺寸无效或过大".into());
    }
    let (w, h) = (w as i32, h as i32);
    let rect = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    let mut canvas = Canvas::default();
    // DIB 附着在内存 DC 上；GdiFlush 后才能从 Rust 读取其底层 BGR 像素。
    let image = unsafe {
        canvas.metafile = if placeable {
            SetWinMetaFileBits(&data[22..], None, None)
        } else {
            SetEnhMetaFileBits(&data)
        };
        if canvas.metafile.is_invalid() {
            return Err("无法读取 Windows 图元数据".into());
        }
        canvas.dc = CreateCompatibleDC(None);
        if canvas.dc.is_invalid() {
            return Err("无法创建图元画布".into());
        }
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: h,
            biPlanes: 1,
            biBitCount: 24,
            ..Default::default()
        };
        let mut ptr = std::ptr::null_mut();
        canvas.bitmap =
            CreateDIBSection(Some(canvas.dc), &info, DIB_RGB_COLORS, &mut ptr, None, 0)?;
        canvas.previous = SelectObject(canvas.dc, canvas.bitmap.into());
        if ptr.is_null() || canvas.previous.is_invalid() {
            return Err("无法选择图元位图".into());
        }
        let white = HBRUSH(GetStockObject(WHITE_BRUSH).0);
        if FillRect(canvas.dc, &rect, white) == 0
            || !PlayEnhMetaFile(canvas.dc, canvas.metafile, &rect).as_bool()
            || !GdiFlush().as_bool()
        {
            return Err("Windows 图元绘制失败".into());
        }
        let row_len = w as usize * 3;
        let stride = row_len.next_multiple_of(4);
        let pixels = std::slice::from_raw_parts(ptr.cast::<u8>(), stride * h as usize);
        let mut rgb = Vec::with_capacity(row_len * h as usize);
        for row in pixels.chunks_exact(stride).rev() {
            for pixel in row[..row_len].chunks_exact(3) {
                rgb.extend([pixel[2], pixel[1], pixel[0]]);
            }
        }
        RgbImage::from_raw(w as u32, h as u32, rgb).unwrap()
    };
    Ok(DynamicImage::ImageRgb8(image))
}
