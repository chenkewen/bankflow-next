use super::Result;
use image::DynamicImage;
use std::path::Path;

pub(super) fn read_numeric_gray(
    path: &Path,
    first_only: bool,
) -> Result<Option<(DynamicImage, Option<Vec<u8>>)>> {
    use image::metadata::Orientation;
    use std::{fs::File, io::BufReader};
    use tiff::{
        decoder::{Decoder, DecodingResult},
        tags::Tag,
    };
    let mut decoder = Decoder::new(BufReader::new(File::open(path)?))?;
    if decoder.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation)? != Some(1)
        || decoder
            .find_tag_unsigned::<u16>(Tag::SamplesPerPixel)?
            .unwrap_or(1)
            != 1
        || decoder.find_tag_unsigned::<u16>(Tag::BitsPerSample)? != Some(32)
    {
        return Ok(None);
    }
    if !first_only && decoder.more_images() {
        return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into());
    }
    let (w, h) = decoder.dimensions()?;
    let orientation = decoder
        .find_tag_unsigned::<u8>(Tag::Orientation)?
        .and_then(Orientation::from_exif);
    let profile = decoder
        .find_tag(Tag::IccProfile)?
        .map(|v| v.into_u8_vec())
        .transpose()?;
    // 与原 PNG 兼容入口一致，保留大于 255 的灰度，裁剪超出整数范围的值。
    let values = match decoder.read_image()? {
        DecodingResult::F32(v) => v
            .into_iter()
            .map(|f| if f < 2_147_483_648.0 { f as u16 } else { 0 })
            .collect(),
        DecodingResult::I32(v) => v.into_iter().map(|n| n.clamp(0, 65535) as u16).collect(),
        DecodingResult::U32(v) => v.into_iter().map(|n| n.min(65535) as u16).collect(),
        _ => return Err("不支持此 32 位 TIFF 灰度编码".into()),
    };
    let mut image = DynamicImage::ImageLuma16(
        image::ImageBuffer::from_raw(w, h, values).ok_or("TIFF 灰度尺寸不符")?,
    );
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(Some((image, profile)))
}

pub(crate) fn read_fax(
    path: &Path,
    first_only: bool,
) -> Result<Option<(DynamicImage, Option<Vec<u8>>)>> {
    #[cfg(windows)]
    {
        use std::{fs::File, io::BufReader};
        use tiff::{decoder::Decoder, tags::Tag};

        let mut decoder = Decoder::new(BufReader::new(File::open(path)?))?;
        let compression = decoder.find_tag_unsigned::<u16>(Tag::Compression)?;
        let reversed_group4 =
            compression == Some(4) && decoder.find_tag_unsigned::<u16>(Tag::FillOrder)? == Some(2);
        if !matches!(compression, Some(2 | 3)) && !reversed_group4 {
            return Ok(None);
        }
        let profile = decoder
            .find_tag(Tag::IccProfile)?
            .map(|v| v.into_u8_vec())
            .transpose()?;
        // 传真图只有黑白像素，保持灰度色彩空间以便沿用源灰度 ICC。
        let mut image = super::compat::wic::read(path, first_only)?.into_luma8();
        if compression == Some(2)
            && decoder.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation)? == Some(1)
        {
            // WIC 按零为白解释 CCITT RLE，遇到显式零为黑须反转一次。
            image::imageops::invert(&mut image);
        }
        Ok(Some((DynamicImage::ImageLuma8(image), profile)))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, first_only);
        Ok(None)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs::File;
    use tiff::{encoder::TiffEncoder, tags::Tag};

    fn fax_tiff(path: &Path, pages: &[u16], photometric: u16, fill_order: u16) -> Result<()> {
        let mut encoder = TiffEncoder::new(File::create(path)?)?;
        for &compression in pages {
            // 以 libtiff 编码的 8×2 黑白条带，每行各有一个黑点。
            let data: &[u8] = match compression {
                2 => &[53, 199, 48, 53, 33, 208],
                3 => &[0, 19, 92, 115, 0, 19, 82, 29, 0],
                4 => &[38, 185, 28, 226, 18, 128, 8, 0, 128],
                _ => unreachable!(),
            };
            let encoded: Vec<u8> = data
                .iter()
                .map(|&byte| {
                    if fill_order == 2 {
                        byte.reverse_bits()
                    } else {
                        byte
                    }
                })
                .collect();
            let mut dir = encoder.image_directory()?;
            let offset = dir.write_data(encoded.as_slice())?;
            dir.write_tag(Tag::ImageWidth, 8u32)?;
            dir.write_tag(Tag::ImageLength, 2u32)?;
            dir.write_tag(Tag::BitsPerSample, 1u16)?;
            dir.write_tag(Tag::Compression, compression)?;
            dir.write_tag(Tag::PhotometricInterpretation, photometric)?;
            dir.write_tag(Tag::FillOrder, fill_order)?;
            dir.write_tag(Tag::StripOffsets, u32::try_from(offset)?)?;
            dir.write_tag(Tag::SamplesPerPixel, 1u16)?;
            dir.write_tag(Tag::RowsPerStrip, 2u32)?;
            dir.write_tag(Tag::StripByteCounts, data.len() as u32)?;
            dir.write_tag(Tag::Orientation, 6u16)?;
            dir.finish()?;
        }
        Ok(())
    }

    #[test]
    fn decodes_photometric_and_bit_order_for_fax_compressions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("fax.tif");
        for compression in [2, 3, 4] {
            for photometric in [0, 1] {
                for fill_order in [1, 2] {
                    fax_tiff(&path, &[compression], photometric, fill_order)?;
                    let decoded = read_fax(&path, false)?;
                    if compression == 4 && fill_order == 1 {
                        assert!(decoded.is_none());
                        continue;
                    }
                    let gray = decoded.unwrap().0.into_luma8();
                    assert_eq!(gray.dimensions(), (2, 8));
                    for (x, y, pixel) in gray.enumerate_pixels() {
                        let dot = matches!((x, y), (1, 2) | (0, 6));
                        let expected = if dot == (photometric == 1) { 0 } else { 255 };
                        assert_eq!(
                            pixel[0], expected,
                            "compression={compression} photo={photometric} order={fill_order}"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn conversion_rejects_multiple_pages_and_ocr_reads_first_page() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pages.tif");
        fax_tiff(&path, &[3, 2], 1, 1)?;
        assert!(read_fax(&path, false)
            .unwrap_err()
            .to_string()
            .contains("多帧"));
        let (first, _) = read_fax(&path, true)?.unwrap();
        let gray = first.into_luma8();
        assert_eq!(gray.dimensions(), (2, 8));
        assert_eq!(gray[(1, 2)][0], 0);
        Ok(())
    }
}
