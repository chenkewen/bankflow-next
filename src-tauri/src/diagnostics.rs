use crate::{
    native_chars::Glyph,
    native_ocr_page::{remove_red_stamp, OcrEngine, OcrItem},
};
use image::RgbImage;
use serde::Deserialize;
use std::{
    error::Error,
    io::{self, BufRead, Read, Write},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(super) fn run(command: &str) -> Option<Result<()>> {
    let action = match command {
        "--statement-excel" => excel,
        "--ocr-geometry" => geometry,
        "--ocr-metadata" => metadata,
        "--ocr-page" => page,
        _ => return None,
    };
    if std::env::args_os().nth(2).is_some() {
        return Some(Err("验证命令不接受额外参数".into()));
    }
    Some(action())
}

fn excel() -> Result<()> {
    crate::statement_excel::save(serde_json::from_reader(io::stdin().lock())?)?;
    println!("true");
    Ok(())
}

#[derive(Deserialize)]
struct GeometryRequest {
    items: Vec<OcrItem>,
    width: f64,
    height: f64,
    scale_to_pt: f64,
    allow_deskew: bool,
    vlines: Vec<f64>,
}

fn geometry() -> Result<()> {
    let r: GeometryRequest = serde_json::from_reader(io::stdin().lock())?;
    let result = crate::ocr_geometry::convert(
        &r.items,
        r.width,
        r.height,
        r.scale_to_pt,
        r.allow_deskew,
        &r.vlines,
    );
    let mut output = io::BufWriter::new(io::stdout().lock());
    serde_json::to_writer(&mut output, &result)?;
    output.flush()?;
    Ok(())
}

#[derive(Deserialize)]
struct MetadataRequest {
    words: Vec<Glyph>,
    lines: Vec<f64>,
    width: f64,
    hdr_top: f64,
}

fn metadata() -> Result<()> {
    let r: MetadataRequest = serde_json::from_reader(io::stdin().lock())?;
    serde_json::to_writer(
        io::stdout().lock(),
        &crate::ocr_metadata::extract(&r.words, &r.lines, r.width, r.hdr_top),
    )?;
    Ok(())
}

fn write_page(
    engine: &mut OcrEngine,
    image: &RgbImage,
    scale: f64,
    writer: &mut impl Write,
) -> Result<()> {
    let cleaned = remove_red_stamp(image);
    let items = engine.recognize(&cleaned)?;
    let (h, v) = crate::ocr_lines::detect(&cleaned, scale);
    serde_json::to_writer(
        &mut *writer,
        &serde_json::json!({"width":image.width(),"height":image.height(),"items":items,"h":h,"v":v}),
    )?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn page() -> Result<()> {
    let mut engine = OcrEngine::new()?;
    let mut reader = io::stdin().lock();
    let mut writer = io::stdout().lock();
    writer.write_all(b"OK\n")?;
    writer.flush()?;
    while !reader.fill_buf()?.is_empty() {
        let mut hdr = [0; 16];
        reader.read_exact(&mut hdr)?;
        let w = u32::from_le_bytes(hdr[0..4].try_into()?);
        let h = u32::from_le_bytes(hdr[4..8].try_into()?);
        let scale = f64::from_le_bytes(hdr[8..16].try_into()?);
        let size = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(3))
            .ok_or("图片尺寸过大")?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(size)?;
        pixels.resize(size, 0);
        reader.read_exact(&mut pixels)?;
        let image = RgbImage::from_raw(w, h, pixels).ok_or("图片尺寸异常")?;
        write_page(&mut engine, &image, scale, &mut writer)?;
    }
    Ok(())
}
