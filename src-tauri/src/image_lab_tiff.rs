use super::Result;
use image::{metadata::Orientation, DynamicImage, RgbImage};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
};
use tiff::{
    decoder::{Decoder, DecodingResult},
    tags::{ByteOrder, Tag},
};

pub(super) fn read(
    path: &Path,
    first_only: bool,
) -> Result<Option<(DynamicImage, Option<Vec<u8>>)>> {
    let mut file = BufReader::new(File::open(path)?);
    let mut tags = Decoder::new(&mut file)?;
    if tags.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation)? != Some(8) {
        return Ok(None);
    }
    if !first_only && tags.more_images() {
        return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into());
    }
    if tags.get_tag_u16_vec(Tag::BitsPerSample)? != [8, 8, 8]
        || tags.find_tag_unsigned::<u16>(Tag::SamplesPerPixel)? != Some(3)
    {
        return Err("暂不支持此 Lab TIFF 的位深或通道数".into());
    }
    let (width, height) = tags.dimensions()?;
    let orientation = tags
        .find_tag_unsigned::<u8>(Tag::Orientation)?
        .and_then(Orientation::from_exif);
    let source_icc = tags
        .find_tag(Tag::IccProfile)?
        .map(|value| value.into_u8_vec())
        .transpose()?;
    let patch = photometric_byte(&mut tags)?;
    drop(tags);
    file.rewind()?;
    let mut decoder = Decoder::new(BufReader::new(RgbView {
        file: file.into_inner(),
        patch,
    }))?;
    let mut decoded = DecodingResult::U8(Vec::new());
    let layout = decoder.read_image_to_buffer(&mut decoded)?;
    let DecodingResult::U8(mut bytes) = decoded else {
        return Err("Lab TIFF 需要无符号 8 位色样".into());
    };
    if bytes.len() != layout.complete_len {
        return Err("Lab TIFF 超过解码缓冲区上限".into());
    }
    if layout.planes == 3 {
        let plane = bytes.len() / 3;
        bytes = (0..plane)
            .flat_map(|i| [bytes[i], bytes[i + plane], bytes[i + plane * 2]])
            .collect();
    }
    // TIFF 用二补码保存 a*、b*，ICC 的 8 位输入以 128 表示中性。
    for pixel in bytes.chunks_exact_mut(3) {
        pixel[1] ^= 0x80;
        pixel[2] ^= 0x80;
    }

    let mut standard_lab = ColorProfile::new_lab();
    standard_lab.pcs = DataColorSpace::Lab;
    // 无可用 Lab 配置时按 TIFF 默认的 D50 Lab 解释，输出只附 sRGB 描述。
    let source = source_icc
        .as_deref()
        .and_then(|profile| ColorProfile::new_from_slice(profile).ok())
        .filter(|profile| profile.color_space == DataColorSpace::Lab)
        .unwrap_or(standard_lab);
    let srgb = include_bytes!("psd_srgb.icc");
    let target = ColorProfile::new_from_slice(srgb)?;
    let transform = source.create_transform_8bit(
        Layout::Rgb,
        &target,
        Layout::Rgb,
        TransformOptions::default(),
    )?;
    let mut rgb = vec![0; bytes.len()];
    transform.transform(&bytes, &mut rgb)?;
    let rgb = RgbImage::from_raw(width, height, rgb).ok_or("Lab TIFF 像素数与尺寸不符")?;
    let mut image = DynamicImage::ImageRgb8(rgb);
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(Some((image, Some(srgb.to_vec()))))
}

// tiff 支持条带、图块和压缩，但拒绝 CIELab 颜色标记。仅在本次解码的读取视图中
// 以 RGB 标记读取相同布局的三个原始通道，避免改动源文件或载入未使用的后续页面。
struct RgbView {
    file: File,
    patch: u64,
}

impl Read for RgbView {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let start = self.file.stream_position()?;
        let len = self.file.read(buf)?;
        if let Some(i) = self
            .patch
            .checked_sub(start)
            .and_then(|i| usize::try_from(i).ok())
        {
            if let Some(value) = buf[..len].get_mut(i) {
                *value = 2;
            }
        }
        Ok(len)
    }
}

impl Seek for RgbView {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

fn photometric_byte<R: Read + Seek>(tags: &mut Decoder<R>) -> Result<u64> {
    let ifd = tags.ifd_pointer().ok_or("Lab TIFF 缺少首页目录")?.0;
    tags.inner().seek(SeekFrom::Start(2))?;
    let big = tags.read_short()? == 43;
    tags.inner().seek(SeekFrom::Start(ifd))?;
    let count = if big {
        tags.read_long8()?
    } else {
        u64::from(tags.read_short()?)
    };
    for _ in 0..count {
        if tags.read_short()? == 262 {
            let width = match tags.read_short()? {
                3 => 2,
                4 => 4,
                _ => return Err("Lab TIFF 光度标记类型错误".into()),
            };
            if tags.read_ifd_offset()? != 1 {
                return Err("Lab TIFF 光度标记数量错误".into());
            }
            let position = tags.inner().stream_position()?;
            return Ok(if tags.byte_order() == ByteOrder::BigEndian {
                position + width - 1
            } else {
                position
            });
        }
        tags.inner()
            .seek(SeekFrom::Current(if big { 18 } else { 10 }))?;
    }
    Err("Lab TIFF 缺少光度标记".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::encoder::{colortype::RGB8, TiffEncoder};

    fn fixture(path: &Path, pages: usize) -> Result<()> {
        let mut encoder = TiffEncoder::new(File::create(path)?)?;
        for _ in 0..pages {
            let mut image = encoder.new_image::<RGB8>(4, 1)?;
            image
                .encoder()
                .write_tag(Tag::PhotometricInterpretation, 8u16)?;
            image.encoder().write_tag(Tag::Orientation, 6u16)?;
            image.write_data(&[0, 0, 0, 255, 0, 0, 128, 32, 228, 50, 148, 92])?;
        }
        Ok(())
    }

    #[test]
    fn converts_signed_channels_to_srgb_and_rotates() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("lab.tif");
        fixture(&path, 1)?;
        let (rgb, icc) = read(&path, false)?.unwrap();
        let rgb = rgb.into_rgb8();
        assert_eq!(rgb.dimensions(), (1, 4));
        assert_eq!(&icc.unwrap()[16..20], b"RGB ");
        // Pillow 13 / LittleCMS 将同一组黑、白和两组带正负色度的像素转为这些色值。
        let expected = [1, 0, 1, 254, 255, 254, 155, 99, 168, 0, 69, 0];
        assert!(rgb
            .as_raw()
            .iter()
            .zip(expected)
            .all(|(a, b)| a.abs_diff(b) <= 2));
        let file = File::open(path)?;
        assert_eq!(
            Decoder::new(file)?.get_tag_u32(Tag::PhotometricInterpretation)?,
            8
        );
        Ok(())
    }

    #[test]
    fn converts_first_page_only_on_request() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pages.tif");
        fixture(&path, 2)?;
        assert!(read(&path, false).unwrap_err().to_string().contains("多帧"));
        assert_eq!(read(&path, true)?.unwrap().0.height(), 4);
        Ok(())
    }
}
