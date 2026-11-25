#[path = "image_avif.rs"]
mod avif;
#[path = "image_game.rs"]
mod game;
#[path = "image_icns.rs"]
mod icns;
#[path = "image_im.rs"]
mod im;
#[path = "legacy_image.rs"]
mod legacy;
#[path = "image_psd.rs"]
mod psd;
#[path = "image_sgi.rs"]
mod sgi;
#[path = "image_sun.rs"]
mod sun;
#[path = "image_text_raster.rs"]
mod text_raster;
#[path = "image_xpix.rs"]
mod xpix;
use image::{codecs::bmp::BmpDecoder, DynamicImage, RgbImage};
use std::{
    error::Error,
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(super) enum Decoder {
    #[cfg(windows)]
    Metafile,
    Game,
    Im,
    Icns,
    TextRaster,
    Legacy,
    Dib,
    Pcx,
    Sgi,
    Sun,
    Xpix,
    Psd,
    Jp2,
    Avif(std::path::PathBuf),
    #[cfg(windows)]
    Wic,
}

impl Decoder {
    pub(super) fn detect(path: &Path) -> Option<Self> {
        let mut header = [0; 256];
        let n = File::open(path).ok()?.read(&mut header).ok()?;
        let bytes = &header[..n];
        #[cfg(windows)]
        if metafile::detect(bytes) {
            return Some(Self::Metafile);
        }
        if bytes.starts_with(b"icns") {
            return Some(Self::Icns);
        }
        if text_raster::detect(bytes) {
            return Some(Self::TextRaster);
        }
        if im::detect(bytes) {
            return Some(Self::Im);
        }
        if game::detect(bytes) {
            return Some(Self::Game);
        }
        if legacy::detect(bytes, path) {
            return Some(Self::Legacy);
        }
        if bytes.starts_with(b"8BPS") {
            return psd::supports(path).then_some(Self::Psd);
        }
        if bytes.starts_with(&[0x59, 0xa6, 0x6a, 0x95]) {
            return Some(Self::Sun);
        }
        if xpix::detect(bytes) {
            return Some(Self::Xpix);
        }
        if sgi::supported(bytes) {
            return Some(Self::Sgi);
        }
        if bytes.starts_with(&[0, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 135, 10])
            || bytes.starts_with(&[255, 79, 255, 81])
        {
            return Some(Self::Jp2);
        }
        if n >= 4
            && bytes[0] == 10
            && matches!(bytes[1], 0 | 2 | 3 | 4 | 5)
            && bytes[2] == 1
            && matches!(bytes[3], 1 | 2 | 4 | 8)
        {
            return Some(Self::Pcx);
        }
        if n >= 4
            && matches!(
                u32::from_le_bytes(header[..4].try_into().unwrap()),
                12 | 40 | 52 | 56 | 64 | 108 | 124
            )
            && BmpDecoder::new_without_file_header(BufReader::new(File::open(path).ok()?)).is_ok()
        {
            return Some(Self::Dib);
        }
        if bytes.get(4..8) == Some(b"ftyp")
            && bytes[8..]
                .chunks_exact(4)
                .any(|s| matches!(s, b"avif" | b"avis"))
        {
            return avif::executable().map(Self::Avif);
        }
        if bytes.get(4..8) == Some(b"ftyp") && matches!(bytes.get(8..12), Some(b"mif1" | b"msf1")) {
            // 无法确定编码的 HEIF 容器交还兼容解码器，避免 WIC 静默选用封面。
            return None;
        }
        #[cfg(windows)]
        if wic::can_open(path) {
            return Some(Self::Wic);
        }
        None
    }

    pub(super) fn read(self, path: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
        let image = match self {
            #[cfg(windows)]
            Self::Metafile => metafile::decode(path),
            Self::Game => game::decode(path),
            Self::Icns => return icns::decode(path),
            Self::TextRaster => text_raster::decode(path),
            Self::Im => im::decode(path),
            Self::Legacy => legacy::decode(path),
            Self::Dib => Ok(DynamicImage::from_decoder(
                BmpDecoder::new_without_file_header(BufReader::new(File::open(path)?))?,
            )?),
            Self::Pcx => {
                let mut decoder = pcx::Reader::from_file(path)?;
                let (w, h) = (u32::from(decoder.width()), u32::from(decoder.height()));
                let size = w
                    .checked_mul(h)
                    .and_then(|n| n.checked_mul(3))
                    .filter(|v| *v <= 512 * 1024 * 1024)
                    .ok_or("图片过大")?;
                let mut pixels = vec![0; size as usize];
                decoder.read_rgb_pixels(&mut pixels)?;
                Ok(DynamicImage::ImageRgb8(
                    RgbImage::from_raw(w, h, pixels).ok_or("PCX 图像尺寸无效")?,
                ))
            }
            Self::Avif(program) => return avif::decode(&program, path),
            Self::Psd => return psd::decode(path),
            Self::Sgi => sgi::decode(path),
            Self::Sun => sun::decode(path),
            Self::Xpix => xpix::decode(path),
            Self::Jp2 => {
                let img = openjp2::Image::from_file(path)?;
                Ok((&img).try_into()?)
            }
            #[cfg(windows)]
            Self::Wic => wic::read(path, false),
        }?;
        Ok((image, None))
    }
}

#[cfg(windows)]
pub(super) mod wic {
    use super::{DynamicImage, Path, Result};
    use image::{metadata::Orientation, RgbaImage};
    use windows::{
        core::{w, HSTRING},
        Win32::{
            Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE},
            Graphics::Imaging::*,
            System::Com::{
                StructuredStorage::{PropVariantClear, PropVariantToUInt16, PROPVARIANT},
                *,
            },
        },
    };
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() }
            }
        }
    }
    fn with_decoder<T>(
        path: &Path,
        process: impl FnOnce(&IWICImagingFactory, &IWICBitmapDecoder) -> Result<T>,
    ) -> Result<T> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr != RPC_E_CHANGED_MODE {
            hr.ok()?;
        }
        let _apartment = Apartment(hr.is_ok());
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?
        };
        process(&factory, &decoder)
    }
    pub(super) fn can_open(path: &Path) -> bool {
        with_decoder(path, |_, decoder| {
            Ok(unsafe { decoder.GetFrameCount()? } > 0)
        })
        .unwrap_or(false)
    }
    pub(in crate::image_convert) fn read(path: &Path, first_only: bool) -> Result<DynamicImage> {
        with_decoder(path, |factory, decoder| unsafe {
            if !first_only && decoder.GetFrameCount()? != 1 {
                return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into());
            }
            let frame = decoder.GetFrame(0)?;
            let mut w = 0;
            let mut h = 0;
            frame.GetSize(&mut w, &mut h)?;
            let stride = w.checked_mul(4).ok_or("图片过大")?;
            let size = stride
                .checked_mul(h)
                .filter(|n| *n <= 512 * 1024 * 1024)
                .ok_or("图片过大")?;
            let converter = factory.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let mut pixels = vec![0; size as usize];
            converter.CopyPixels(std::ptr::null(), stride, &mut pixels)?;
            let mut image =
                DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, pixels).ok_or("图片尺寸无效")?);
            if let Ok(metadata) = frame.GetMetadataQueryReader() {
                for key in [w!("/app1/ifd/{ushort=274}"), w!("/ifd/{ushort=274}")] {
                    let mut prop = PROPVARIANT::default();
                    let value = metadata
                        .GetMetadataByName(key, &mut prop)
                        .and_then(|_| PropVariantToUInt16(&prop));
                    PropVariantClear(&mut prop)?;
                    if let Some(o) = value
                        .ok()
                        .and_then(|n| u8::try_from(n).ok())
                        .and_then(Orientation::from_exif)
                    {
                        image.apply_orientation(o);
                        break;
                    }
                }
            }
            Ok(image)
        })
    }
}
#[cfg(windows)]
#[path = "image_metafile.rs"]
mod metafile;
