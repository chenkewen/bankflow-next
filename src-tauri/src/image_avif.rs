use super::{DynamicImage, Path, Result};
use std::{path::PathBuf, process::Command};

pub(super) fn executable() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_owned();
    let mut candidates = vec![dir.join("avifdec.exe"), dir.join("runtime/avifdec.exe")];
    if cfg!(debug_assertions) {
        candidates
            .push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/codecs/avifdec.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub(super) fn decode(program: &Path, path: &Path) -> Result<(DynamicImage, Option<Vec<u8>>)> {
    let stage = tempfile::tempdir()?;
    let png = stage.path().join("frame.png");
    let mut command = Command::new(program);
    command
        .args(["--depth", "8", "--png-compress", "0"])
        .arg(std::path::absolute(path)?)
        .arg(&png);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let decoded = command.output()?;
    if !decoded.status.success() {
        return Err(format!(
            "AVIF 解码失败：{}",
            String::from_utf8_lossy(&decoded.stderr).trim()
        )
        .into());
    }
    // 固定版本的 avifdec 同时输出容器帧数和帧零，静态转换只接受单帧。
    let log = String::from_utf8_lossy(&decoded.stdout);
    let count = log
        .lines()
        .filter(|s| s.starts_with(" * ") && s.contains(" timescales per second, "))
        .find_map(|s| {
            s.rsplit_once(", ")?
                .1
                .split_whitespace()
                .next()?
                .parse::<usize>()
                .ok()
        })
        .ok_or("无法读取 AVIF 帧数")?;
    if count != 1 {
        return Err("暂不支持多帧图片，请先拆分帧，避免丢失内容".into());
    }
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(png)?.into_decoder()?;
    let profile = decoder.icc_profile()?;
    Ok((DynamicImage::from_decoder(decoder)?, profile))
}
