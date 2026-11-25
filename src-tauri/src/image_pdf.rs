use crate::domain::{ConversionEvent, ConversionRequest};
use crate::{image_convert::{encode_image, opaque, read_image, resized}, worker::{emit, finish, sources}};
use image::{codecs::jpeg::JpegEncoder, DynamicImage};
use pdfium_render::prelude::*;
use std::{
    error::Error,
    io::Cursor,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn size_in_pixels(page: &PdfPage, width: u32) -> Result<(i32, i32)> {
    let w = f64::from(page.width().value);
    let h = f64::from(page.height().value);
    if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return Err("PDF 页面尺寸无效".into());
    }
    let scale = if width == 0 {
        2.0
    } else {
        f64::from(width) / w
    };
    let width = (w * scale).ceil();
    let height = (h * scale).ceil();
    if width * height > 40_000_000.0 {
        return Err("渲染图片过大，请调小输出宽度".into());
    }
    Ok((width as i32, height as i32))
}

fn render_pages(
    pdfium: &Pdfium,
    request: &ConversionRequest,
    folder: &Path,
    cancelled: &AtomicBool,
) -> Result<()> {
    let paths = sources(request);
    if paths.len() != 1 || !matches!(request.format.as_str(), "png" | "jpg") {
        return Err("PDF 转图片请选择一个 PDF，格式为 PNG 或 JPG".into());
    }
    let password = (!request.password.is_empty()).then_some(request.password.as_str());
    let document = pdfium.load_pdf_from_file(&paths[0], password)?;
    let pages = document.pages();
    let count = pages.len();
    if count == 0 {
        return Err("没有可转换内容".into());
    }
    for i in 0..count {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let page = pages.get(i)?;
        let (w, h) = size_in_pixels(&page, request.width)?;
        let bitmap = PdfBitmap::empty(w, h, PdfBitmapFormat::BGR)?;
        let bindings = pdfium.bindings();
        let bitmap_handle = bindings.get_handle_from_bitmap(&bitmap);
        let page_handle = bindings.get_handle_from_page(&page);
        // 使用与旧引擎相同的批注标志，避免额外绘制表单控件的浮层。
        const FPDF_ANNOT: i32 = 1;
        unsafe {
            bindings.FPDFBitmap_FillRect(bitmap_handle, 0, 0, w, h, 0xffffffff);
            bindings.FPDF_RenderPageBitmap(bitmap_handle, page_handle, 0, 0, w, h, 0, FPDF_ANNOT);
        }
        let output = folder.join(format!("{:03}.{}", i + 1, request.format));
        let image = DynamicImage::ImageRgb8(bitmap.as_image()?.into_rgb8());
        encode_image(image, &output, request)?;
        emit(&ConversionEvent::Progress {
            page: i as u32 + 1,
            total: count as u32,
        })?;
    }
    Ok(())
}

fn combine_images(
    pdfium: &Pdfium,
    request: &ConversionRequest,
    output: &Path,
    cancelled: &AtomicBool,
) -> Result<()> {
    let mut document = pdfium.create_new_pdf()?;
    let paths = sources(request);
    for (i, path) in paths.iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let image = opaque(resized(read_image(Path::new(path))?, request.width)?);
        // 原导出按 144 DPI 排版，每个像素对应半个 PDF 点。
        let width = PdfPoints::new(image.width() as f32 * 0.5);
        let height = PdfPoints::new(image.height() as f32 * 0.5);
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 75).encode_image(&image)?;
        let mut object = PdfPageImageObject::new_from_jpeg_reader(&document, Cursor::new(jpeg))?;
        object.scale(width.value, height.value)?;
        let mut page = document
            .pages_mut()
            .create_page_at_end(PdfPagePaperSize::Custom(width, height))?;
        page.objects_mut().add_image_object(object)?;
        emit(&ConversionEvent::Progress {
            page: i as u32 + 1,
            total: paths.len() as u32,
        })?;
    }
    if !cancelled.load(Ordering::Acquire) {
        document.save_to_file(output)?;
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
    if paths.iter().any(|p| !Path::new(p).is_file()) {
        return Err("请选择存在的文件".into());
    }
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
    let pdfium = crate::pdfium_engine::open()?;
    match request.mode.as_str() {
        "pdf_images" => {
            render_pages(&pdfium, request, stage.path(), cancelled)?;
            finish(&first, parent, stage.path(), "_页面图片", "", cancelled)
        }
        "images_pdf" => {
            let file = stage.path().join("result.pdf");
            combine_images(&pdfium, request, &file, cancelled)?;
            finish(&first, parent, &file, "_图片合并", ".pdf", cancelled)
        }
        _ => Err("不支持的转换任务".into()),
    }
}
