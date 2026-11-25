use crate::{
    ocr_boxes::{self, Quad},
    ocr_pixels, statement_ocr,
};
use image::RgbImage;
use ort::{session::Session, value::Tensor};
use pdfium_render::prelude::*;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    path::PathBuf,
    sync::LazyLock,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub use crate::ocr_pixels::remove_red_stamp;

#[derive(Clone)]
#[cfg_attr(any(debug_assertions, test), derive(serde::Deserialize, serde::Serialize))]
pub struct OcrItem {
    pub points: Quad,
    pub text: String,
    pub score: f64,
}
pub struct OcrEngine {
    det: Session,
    rec: Session,
    alphabet: Vec<String>,
    crops: HashMap<[u8; 32], (String, f64)>,
    insertion_order: VecDeque<[u8; 32]>,
}

fn assets() -> Result<(PathBuf, PathBuf)> {
    let dir = std::env::current_exe()?
        .parent()
        .ok_or("程序目录不可用")?
        .to_owned();
    let mut roots = vec![dir.clone(), dir.join("runtime")];
    if cfg!(debug_assertions) {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.venv/Lib/site-packages"));
    }
    for root in roots {
        let dll = root.join("onnxruntime/capi/onnxruntime.dll");
        let models = root.join("rapidocr_onnxruntime/models");
        if dll.is_file() && models.is_dir() {
            return Ok((dll, models));
        }
        if root.join("onnxruntime.dll").is_file() && root.join("models").is_dir() {
            return Ok((root.join("onnxruntime.dll"), root.join("models")));
        }
    }
    Err("找不到 OCR 模型及 ONNX Runtime".into())
}

fn nchw(image: &RgbImage, width: usize, detector: bool) -> Vec<f32> {
    let height = image.height() as usize;
    let mut values = vec![0.; width * height * 3];
    let table: [f32; 256] = std::array::from_fn(|i| {
        if detector {
            ((f64::from(i as f32 * (1f32 / 255.)) - 0.5) / 0.5) as f32
        } else {
            ((i as f32 / 255.) - 0.5) / 0.5
        }
    });
    for (y, line) in image.rows().enumerate() {
        for (x, p) in line.enumerate() {
            for c in 0..3 {
                values[(c * height + y) * width + x] = table[p[c] as usize];
            }
        }
    }
    values
}

fn model_side(val: u32, ratio: f64) -> u32 {
    (((f64::from(val) * ratio).floor() / 32.).round_ties_even() as u32 * 32).max(32)
}

impl OcrEngine {
    pub fn new() -> Result<Self> {
        let (dll, dir) = assets()?;
        ort::init_from(dll.to_string_lossy()).commit()?;
        let det = statement_ocr::session(dir.join("ch_PP-OCRv4_det_infer.onnx"))?;
        let rec = statement_ocr::session(dir.join("ch_PP-OCRv4_rec_infer.onnx"))?;
        let alphabet = statement_ocr::alphabet(&rec)?;
        Ok(Self {
            det,
            rec,
            alphabet,
            crops: HashMap::new(),
            insertion_order: VecDeque::new(),
        })
    }

    pub(crate) fn read_crop(&mut self, crop: &RgbImage) -> Result<(String, f64)> {
        // 重复栏头和空值只在像素、宽、高都匹配时复用推理结果。
        let mut hash = Sha256::new();
        hash.update(crop.width().to_le_bytes());
        hash.update(crop.height().to_le_bytes());
        hash.update(crop.as_raw());
        let key: [u8; 32] = hash.finalize().into();
        if let Some(result) = self.crops.get(&key) {
            return Ok(result.clone());
        }
        let ratio = f64::from(crop.width()) / f64::from(crop.height());
        let width = (48. * ratio.max(2.5)) as usize;
        let resized = (48. * ratio).ceil() as usize;
        let rec_input = ocr_pixels::resize(crop, width.min(resized).max(1) as u32, 48);
        let tensor = Tensor::from_array(([1usize, 3, 48, width], nchw(&rec_input, width, false)))?;
        let pred = self.rec.run(ort::inputs![tensor])?;
        let (dims, scores) = pred[0].try_extract_tensor::<f32>()?;
        let result = statement_ocr::decode(scores, dims[2] as usize, &self.alphabet)?;
        if self.crops.len() == 4096 {
            if let Some(first) = self.insertion_order.pop_front() {
                self.crops.remove(&first);
            }
        }
        self.insertion_order.push_back(key);
        self.crops.insert(key, result.clone());
        Ok(result)
    }

    pub fn recognize(&mut self, image: &RgbImage) -> Result<Vec<OcrItem>> {
        let (w, h) = image.dimensions();
        if w == 0 || h == 0 {
            return Err("图片无像素".into());
        }
        let mut scaled = image.clone();
        let (mut rh, mut rw) = (1., 1.);
        for (condition, limit) in [(true, 2000), (false, 30)] {
            let (sw, sh) = scaled.dimensions();
            let edge = if condition { sw.max(sh) } else { sw.min(sh) };
            if (condition && edge > limit) || (!condition && edge < limit) {
                let ratio = f64::from(limit) / f64::from(edge);
                let (nw, nh) = (model_side(sw, ratio), model_side(sh, ratio));
                scaled = ocr_pixels::resize(&scaled, nw, nh);
                rh = f64::from(sh) / f64::from(nh);
                rw = f64::from(sw) / f64::from(nw);
            }
        }
        let (sw, sh) = scaled.dimensions();
        let pad = if sh <= 30 || f64::from(sw) / f64::from(sh) > 8. {
            (2 * (sw / 8).max(30)).abs_diff(sh) / 2
        } else {
            0
        };
        if pad != 0 {
            let mut canvas = RgbImage::new(sw, sh + 2 * pad);
            image::imageops::replace(&mut canvas, &scaled, 0, i64::from(pad));
            scaled = canvas;
        }
        let (sw, sh) = scaled.dimensions();
        let edge = sw.max(sh);
        let limit = if edge < 960 {
            960
        } else if edge < 1500 {
            1500
        } else {
            2000
        };
        let ratio = (f64::from(limit) / f64::from(edge)).min(1.);
        let (nw, nh) = (model_side(sw, ratio), model_side(sh, ratio));
        let det_input = ocr_pixels::resize(&scaled, nw, nh);
        let tensor = Tensor::from_array((
            [1usize, 3, nh as usize, nw as usize],
            nchw(&det_input, nw as usize, true),
        ))?;
        let detected = self.det.run(ort::inputs![tensor])?;
        let (dims, vals) = detected[0].try_extract_tensor::<f32>()?;
        let boxes = ocr_boxes::detect(vals, dims[3] as usize, dims[2] as usize, sw, sh);
        drop(detected);
        let mut items = Vec::new();
        static WATERMARK: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?:兴业银行\s*\d{4}[-/.]\d{1,2}(?:[-/.]\d{1,2})?(?:\s*\d{1,2}:\d{2}:\d{2})?|电子回单|电子签名专用|业务专用章|会计专用章)$").unwrap()
        });
        for q in boxes {
            let crop = ocr_pixels::crop(&scaled, q).ok_or("OCR 文字框无法进行透视变换")?;
            let (text, score) = self.read_crop(&crop)?;
            let text = text.trim();
            if score >= 0.5 && !text.is_empty() && !WATERMARK.is_match(text) {
                items.push(OcrItem {
                    points: q.map(|[x, y]| {
                        [
                            (x * rw as f32).clamp(0., w as f32),
                            ((y - pad as f32) * rh as f32).clamp(0., h as f32),
                        ]
                    }),
                    text: text.to_string(),
                    score,
                });
            }
        }
        Ok(items)
    }
}

fn strip_objects<'a>(
    objects: &mut impl PdfPageObjectsCommon<'a>,
    pw: f32,
    ph: f32,
    depth: u8,
) -> Result<bool> {
    let mut removed = false;
    for i in (0..objects.len()).rev() {
        let mut obj = objects.get(i)?;
        let skip = match &mut obj {
            PdfPageObject::Text(text) => {
                let m = text.matrix()?;
                [m.a(), m.b(), m.c(), m.d()].iter().all(|v| v.abs() > 0.08)
            }
            PdfPageObject::Image(image) => {
                let b = image.bounds()?.to_rect();
                b.width().value < pw * 0.45 && b.height().value < ph * 0.45
            }
            PdfPageObject::XObjectForm(form) if depth < 14 => {
                removed |= strip_objects(form, pw, ph, depth + 1)?;
                false
            }
            _ => false,
        };
        drop(obj);
        if skip {
            objects.remove_object_at_index(i)?;
            removed = true;
        }
    }
    Ok(removed)
}

pub fn render_pdf(page: &mut PdfPage) -> Result<RgbImage> {
    let (pw, ph) = (page.width().value, page.height().value);
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    let mut removed = strip_objects(page.objects_mut(), pw, ph, 0)?;
    let bindings = page.bindings();
    let raw = bindings.get_handle_from_page(page);
    unsafe {
        for index in (0..bindings.FPDFPage_GetAnnotCount(raw)).rev() {
            if bindings.FPDFPage_RemoveAnnot(raw, index) == 0 {
                return Err("PDF 批注移除失败".into());
            }
            removed = true;
        }
    }
    if removed {
        page.regenerate_content()?;
    }
    let (w, h) = ((pw * 2.).ceil() as i32, (ph * 2.).ceil() as i32);
    let bitmap = PdfBitmap::empty(w, h, PdfBitmapFormat::BGR)?;
    let bindings = page.bindings();
    let bmp = bindings.get_handle_from_bitmap(&bitmap);
    let raw = bindings.get_handle_from_page(page);
    unsafe {
        bindings.FPDFBitmap_FillRect(bmp, 0, 0, w, h, 0xffffffff);
        bindings.FPDF_RenderPageBitmap(bmp, raw, 0, 0, w, h, 0, 0);
    }
    Ok(bitmap.as_image()?.into_rgb8())
}
