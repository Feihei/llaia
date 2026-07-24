use anyhow::{anyhow, Result};
use base64::Engine;
use std::path::Path;

/// vision 图片最大边长：超过则等比缩放
const MAX_DIMENSION: u32 = 1024;
/// JPEG 重新编码质量
const JPEG_QUALITY: u8 = 85;

/// 读取图片文件，必要时缩放，返回 base64 data URL（image/jpeg）。
///
/// 流程：
/// 1. 用 image crate 解码（支持 jpeg/png/gif/webp）
/// 2. 若任一边超过 MAX_DIMENSION，等比缩放
/// 3. 重新编码为 JPEG 质量 85
/// 4. base64 编码，组成 `data:image/jpeg;base64,...`
pub fn prepare_image_for_vision(path: &Path) -> Result<String> {
    let img = image::open(path).map_err(|e| anyhow!("decode image {:?}: {}", path, e))?;

    let (w, h) = (img.width(), img.height());
    let scaled = if w > MAX_DIMENSION || h > MAX_DIMENSION {
        // 等比缩放，使最长边 = MAX_DIMENSION
        let ratio = MAX_DIMENSION as f64 / w.max(h) as f64;
        let new_w = (w as f64 * ratio).round() as u32;
        let new_h = (h as f64 * ratio).round() as u32;
        img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    // 编码为 JPEG
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    encoder
        .encode_image(&scaled)
        .map_err(|e| anyhow!("encode jpeg: {}", e))?;
    let bytes = buf.into_inner();

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:image/jpeg;base64,{}", b64))
}

/// 判断路径是否为图片（按扩展名）。用于区分 send_image 和 send_file 的参数校验，
/// 以及 CLI @path 解析时决定走多模态还是纯文本。
pub fn is_image_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => matches!(
            ext.to_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
        ),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    /// 创建带 .png 后缀的临时文件，让 image crate 能根据扩展名推断格式
    fn png_tempfile() -> tempfile::NamedTempFile {
        tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .unwrap()
    }

    #[test]
    fn test_prepare_small_image() {
        // 生成 100x100 纯色 PNG
        let tmp = png_tempfile();
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(100, 100, Rgb([255, 0, 0]));
        img.save(tmp.path()).unwrap();

        let data_url = prepare_image_for_vision(tmp.path()).unwrap();
        assert!(data_url.starts_with("data:image/jpeg;base64,"));
        assert!(data_url.len() > 100);
    }

    #[test]
    fn test_prepare_large_image_downscaled() {
        // 生成 2000x2000 纯色 PNG，应缩放到 1024x1024
        let tmp = png_tempfile();
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(2000, 2000, Rgb([0, 255, 0]));
        img.save(tmp.path()).unwrap();

        let data_url = prepare_image_for_vision(tmp.path()).unwrap();
        assert!(data_url.starts_with("data:image/jpeg;base64,"));

        // 缩放后的 base64 应明显小于原图直接编码
        // （2000x2000 JPEG 约 100KB+，1024x1024 约 30KB）
        let b64_len = data_url.len() - "data:image/jpeg;base64,".len();
        assert!(b64_len < 200_000, "downscaled image base64 too large: {}", b64_len);
    }

    #[test]
    fn test_is_image_file() {
        assert!(is_image_file(Path::new("foo.jpg")));
        assert!(is_image_file(Path::new("foo.PNG")));
        assert!(is_image_file(Path::new("/abs/path/img.webp")));
        assert!(!is_image_file(Path::new("foo.txt")));
        assert!(!is_image_file(Path::new("foo")));
    }
}
