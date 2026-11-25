#[path = "image_compat.rs"]
mod compat;
#[path = "image_tiff.rs"]
mod image_tiff;
#[path = "image_lab_tiff.rs"]
mod image_lab_tiff;
use crate::{domain::{ConversionEvent, ConversionRequest}, worker::{emit, finish, sources}};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgb};
use std::{
    error::Error,
    fs::{self, File},
    io::BufReader,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn built_in(format: Option<ImageFormat>) -> bool {
    matches!(
        format,
        Some(
            ImageFormat::Jpeg
                | ImageFormat::Png
                | ImageFormat::Bmp
                | ImageFormat::WebP
                | ImageFormat::Gif
                | ImageFormat::Pnm
                | ImageFormat::Farbfeld
                | ImageFormat::Ico
                | ImageFormat::Dds
                | ImageFormat::Tga
                | ImageFormat::Tiff
                | ImageFormat::Qoi
        )
    )
}

pub(crate) fn read_image(path: &Path) -> Result<DynamicImage> {
    Ok(load_image(path)?.0)
}

pub(crate) fn read_special_tiff(path: &Path, first_only: bool) -> Result<Option<(DynamicImage, Option<Vec<u8>>)>> {
    if let Some(fax) = image_tiff::read_fax(path, first_only)? {
        return Ok(Some(fax));
    }
    if let Some(gray) = image_tiff::read_numeric_gray(path, first_only)? {
        return Ok(Some(gray));
    }
    image_lab_tiff::read(path, first_only)
}

fn load_image(path: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    // XV 缩略图与 PAM 共用 P7 前缀，从数据头区分；普通 PAM 继续走通用实现。
    if reader.format() == Some(ImageFormat::Pnm) {
        use std::io::Read;
        let mut prefix = [0; 6];
        if File::open(path)?.read(&mut prefix)? == prefix.len() && &prefix == b"P7 332" {
            return compat::Decoder::TextRaster.read(path);
        }
    }
    if reader.format() == Some(ImageFormat::Tiff) {
        if let Some(image) = read_special_tiff(path, false)? {
            return Ok(image);
        }
    }
    if !built_in(reader.format()) {
        if let Some(codec) = compat::Decoder::detect(path) {
            return codec.read(path);
        }
        return Err("当前解码器不支持此图片的格式或编码变体，请导出为 PNG 或 TIFF 后重试".into());
    }
    let input = || File::open(path).map(BufReader::new);
    let animated = match reader.format() {
        Some(ImageFormat::Png) => image::codecs::png::PngDecoder::new(input()?)?.is_apng()?,
        Some(ImageFormat::WebP) => image::codecs::webp::WebPDecoder::new(input()?)?.has_animation(),
        Some(ImageFormat::Gif) => {
            let frames = image::codecs::gif::GifDecoder::new(input()?)?.into_frames();
            frames.into_iter().nth(1).transpose()?.is_some()
        }
        Some(ImageFormat::Tiff) => tiff::decoder::Decoder::new(input()?)?.more_images(),
        _ => false,
    };
    if animated {
        return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into());
    }
    let mut decoder = reader.into_decoder()?;
    let profile = decoder.icc_profile()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok((image, profile))
}

pub(crate) fn resized(mut image: DynamicImage, width: u32) -> Result<DynamicImage> {
    if width != 0 && width != image.width() {
        let height = (image.height() as f64 * width as f64 / image.width() as f64)
            .round_ties_even()
            .max(1.0) as u64;
        if u64::from(width) * height > 40_000_000 {
            return Err("输出图片过大，请调小宽度".into());
        }
        let filter = image::imageops::FilterType::Lanczos3;
        if image.color().has_alpha() {
            // 先预乘透明度，避免透明像素的颜色渗入缩放后的边缘。
            let mut rgba = image.into_rgba8();
            for pixel in rgba.pixels_mut() {
                let alpha = u16::from(pixel[3]);
                for channel in &mut pixel.0[..3] {
                    *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
                }
            }
            let mut resized = image::imageops::resize(&rgba, width, height as u32, filter);
            for pixel in resized.pixels_mut() {
                let alpha = u16::from(pixel[3]);
                if alpha > 0 {
                    for channel in &mut pixel.0[..3] {
                        *channel = ((u16::from(*channel) * 255) / alpha).min(255) as u8;
                    }
                }
            }
            image = DynamicImage::ImageRgba8(resized);
        } else {
            image = image.resize_exact(width, height as u32, filter);
        }
    }
    Ok(image)
}

pub(crate) fn opaque(image: DynamicImage) -> DynamicImage {
    if !image.color().has_alpha() {
        return DynamicImage::ImageRgb8(image.into_rgb8());
    }
    let rgba = image.to_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let white = |v: u8| (255 - ((255 - v as u32) * a as u32 + 127) / 255) as u8;
        Rgb([white(r), white(g), white(b)])
    });
    DynamicImage::ImageRgb8(rgb)
}

pub(crate) fn encode_image(
    image: DynamicImage,
    output: &Path,
    request: &ConversionRequest,
) -> Result<()> {
    encode_with_profile(image, output, request, None)
}

fn encode_with_profile(
    mut image: DynamicImage,
    output: &Path,
    request: &ConversionRequest,
    profile: Option<Vec<u8>>,
) -> Result<()> {
    use image::ImageEncoder;
    if request.format == "png" {
        // PNG 可保留 16 位整数，浮点 TIFF 和 PSD 仅在转 PNG 时量化。
        image = match image {
            DynamicImage::ImageRgb32F(_) => DynamicImage::ImageRgb16(image.into_rgb16()),
            DynamicImage::ImageRgba32F(_) => DynamicImage::ImageRgba16(image.into_rgba16()),
            other => other,
        };
    }
    if let Some(icc) = profile {
        match request.format.as_str() {
            "png" => {
                let mut encoder = image::codecs::png::PngEncoder::new(File::create(output)?);
                encoder.set_icc_profile(icc)?;
                image.write_with_encoder(encoder)?;
                return Ok(());
            }
            "tiff" => {
                let mut encoder = image::codecs::tiff::TiffEncoder::new(File::create(output)?);
                encoder.set_icc_profile(icc)?;
                image.write_with_encoder(encoder)?;
                return Ok(());
            }
            _ => (),
        }
    }
    if matches!(request.format.as_str(), "jpg" | "bmp") {
        image = opaque(image);
    }
    match request.format.as_str() {
        "jpg" => image::codecs::jpeg::JpegEncoder::new_with_quality(
            File::create(output)?,
            request.quality as u8,
        )
        .encode_image(&image)?,
        "webp" => {
            let rgba = image.to_rgba8();
            let encoder = webp::Encoder::from_rgba(&rgba, rgba.width(), rgba.height());
            let encoded = encoder
                .encode_simple(false, request.quality as f32)
                .map_err(|e| format!("WebP 编码失败：{e:?}"))?;
            fs::write(output, &*encoded)?;
        }
        _ => image.save(output)?,
    }
    Ok(())
}

pub(crate) fn run(request: &ConversionRequest, cancelled: &AtomicBool) -> Result<ConversionEvent> {
    if request.width > 16000
        || !(1..=100).contains(&request.quality)
        || !matches!(
            request.format.as_str(),
            "png" | "jpg" | "bmp" | "webp" | "tiff"
        )
    {
        return Err("格式、宽度或质量参数无效".into());
    }
    let paths = sources(request);
    let first = std::path::absolute(&paths[0])?;
    let parent = request
        .output_dir
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(Path::new)
        .or(first.parent())
        .ok_or("输出位置必须为目录")?;
    let stage = tempfile::Builder::new()
        .prefix("bankflow-")
        .tempdir_in(parent)?;
    let batch = paths.len() > 1;
    let file = stage.path().join(format!("result.{}", request.format));
    let cancel = || ConversionEvent::Cancelled {
        message: "任务已取消，未保存结果。".into(),
    };
    for (i, path) in paths.iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Ok(cancel());
        }
        let out = if batch {
            stage
                .path()
                .join(format!("{:03}.{}", i + 1, request.format))
        } else {
            file.clone()
        };
        let (image, profile) = load_image(Path::new(path))?;
        encode_with_profile(resized(image, request.width)?, &out, request, profile)?;
        emit(&ConversionEvent::Progress {
            page: (i + 1) as u32,
            total: paths.len() as u32,
        })?;
    }
    let suffix = if batch {
        String::new()
    } else {
        format!(".{}", request.format)
    };
    finish(
        &first,
        parent,
        if batch { stage.path() } else { &file },
        "_转换",
        &suffix,
        cancelled,
    )
}
