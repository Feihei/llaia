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
    encode_scaled_jpeg(img)
}

/// 从 data URL（`data:image/<fmt>;base64,<b64>`）解码出原始图片字节。
/// 容忍 base64 中的换行/空白（部分 MCP 返回会折叠）。供工具图片落盘复用。
pub fn decode_data_url(data_url: &str) -> Result<Vec<u8>> {
    let b64 = data_url
        .split(";base64,")
        .nth(1)
        .ok_or_else(|| anyhow!("not a base64 data URL"))?;
    let b64_clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64_clean)
        .map_err(|e| anyhow!("decode base64: {}", e))
}

/// 把工具返回的 base64 data URL 图片（任意格式 png/gif/webp/jpeg）解码、
/// 必要时缩放、重编码为 vision 友好格式（JPEG 85，最长边 1024）。
/// 用于把 280k 字符的原始截图压缩到几十 KB，省 token 也避免撑爆上下文。
pub fn prepare_base64_for_vision(data_url: &str) -> Result<String> {
    let bytes = decode_data_url(data_url)?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| anyhow!("decode image bytes ({} bytes): {}", bytes.len(), e))?;
    encode_scaled_jpeg(img)
}

/// 编码公共部分：缩放 + JPEG 85 + base64 data URL
fn encode_scaled_jpeg(img: image::DynamicImage) -> Result<String> {
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

/// base64 data URL 字符集（字母数字 + '+' '/' '='，无空白、无逗号）。
/// 用于从工具结果文本中精确切出图片 data URL 的边界。
fn is_b64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='
}

/// 从工具结果文本中提取所有 `data:image/<fmt>;base64,...` 图片 data URL。
///
/// 返回 `(占位文本, 图片 data URL 列表)`：图片从原文中剥离并替换为 `[图片]`，
/// 其余文本原样保留。用于把 MCP 截图这类超大 base64 从文本上下文里摘出来，
/// 改走多模态图片通道（模型读图）或 vision 描述，避免撑爆上下文。
pub fn extract_data_url_images(text: &str) -> (String, Vec<String>) {
    const PREFIX: &str = "data:image/";
    let mut out = String::with_capacity(text.len());
    let mut images = Vec::new();
    let mut rest = text;
    loop {
        let Some(pos) = rest.find(PREFIX) else {
            out.push_str(rest);
            break;
        };
        // 复制前缀文本
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        // 定位 ";base64," 分隔符，找不到则当普通文本处理
        let Some(b64_pos) = tail.find(";base64,") else {
            out.push_str(tail);
            break;
        };
        let data_start = pos + b64_pos + ";base64,".len();
        // base64 数据直到第一个非 base64 字符（空白/引号/逗号等）
        let b64_len = rest[data_start..]
            .chars()
            .take_while(|c| is_b64_char(*c))
            .map(|c| c.len_utf8())
            .sum::<usize>();
        if b64_len == 0 {
            // "data:image/...;base64," 后没有数据，当普通文本
            out.push_str(tail);
            break;
        }
        let end = data_start + b64_len;
        images.push(rest[pos..end].to_string());
        out.push_str("[图片]");
        rest = &rest[end..];
    }
    (out, images)
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
    use image::ImageEncoder as _;

    /// 创建带 .png 后缀的临时文件，让 image crate 能根据扩展名推断格式
    fn png_tempfile() -> tempfile::NamedTempFile {
        tempfile::Builder::new().suffix(".png").tempfile().unwrap()
    }

    #[test]
    fn test_prepare_small_image() {
        // 生成 100x100 纯色 PNG
        let tmp = png_tempfile();
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(100, 100, Rgb([255, 0, 0]));
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
        assert!(
            b64_len < 200_000,
            "downscaled image base64 too large: {}",
            b64_len
        );
    }

    #[test]
    fn test_is_image_file() {
        assert!(is_image_file(Path::new("foo.jpg")));
        assert!(is_image_file(Path::new("foo.PNG")));
        assert!(is_image_file(Path::new("/abs/path/img.webp")));
        assert!(!is_image_file(Path::new("foo.txt")));
        assert!(!is_image_file(Path::new("foo")));
    }

    fn sample_data_url() -> String {
        // 100x100 红色 PNG → base64 data URL（模拟工具返回的截图）
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(100, 100, Rgb([255, 0, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 100, 100, image::ExtendedColorType::Rgb8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
        format!("data:image/png;base64,{}", b64)
    }

    #[test]
    fn test_extract_data_url_images_single() {
        let url = sample_data_url();
        let text = format!("viewport: {}\nscene ok", url);
        let (placeholder, images) = extract_data_url_images(&text);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0], url);
        assert_eq!(placeholder, "viewport: [图片]\nscene ok");
    }

    #[test]
    fn test_extract_data_url_images_multiple() {
        let url = sample_data_url();
        let text = format!("a {} b {}", url, url);
        let (placeholder, images) = extract_data_url_images(&text);
        assert_eq!(images.len(), 2);
        assert_eq!(placeholder, "a [图片] b [图片]");
    }

    #[test]
    fn test_extract_data_url_images_none() {
        let (placeholder, images) = extract_data_url_images("no image here");
        assert!(images.is_empty());
        assert_eq!(placeholder, "no image here");
    }

    #[test]
    fn test_extract_data_url_images_empty_b64_falls_back() {
        // ";base64," 后无数据 → 不作为图片提取，原样保留
        let (placeholder, images) =
            extract_data_url_images("data:image/png;base64,");
        assert!(images.is_empty());
        assert_eq!(placeholder, "data:image/png;base64,");
    }

    #[test]
    fn test_prepare_base64_for_vision_reencodes() {
        let url = sample_data_url();
        let prepared = prepare_base64_for_vision(&url).unwrap();
        // 重编码为 jpeg data URL
        assert!(prepared.starts_with("data:image/jpeg;base64,"));
        // 100x100 小图不缩放，但 JPEG 编码后仍应非空
        assert!(prepared.len() > 100);
    }

    #[test]
    fn test_prepare_base64_for_vision_ignores_newlines() {
        let url = sample_data_url();
        let b64 = url.split(";base64,").nth(1).unwrap();
        let wrapped = format!("data:image/png;base64,{}\n", b64);
        let prepared = prepare_base64_for_vision(&wrapped).unwrap();
        assert!(prepared.starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn test_prepare_base64_for_vision_downscales() {
        // 2000x2000 → 应缩到 1024x1024，输出明显小于原始尺寸直接编码
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(2000, 2000, Rgb([0, 255, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 2000, 2000, image::ExtendedColorType::Rgb8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
        let url = format!("data:image/png;base64,{}", b64);

        let prepared = prepare_base64_for_vision(&url).unwrap();
        assert!(prepared.starts_with("data:image/jpeg;base64,"));
        let prepared_b64_len = prepared.len() - "data:image/jpeg;base64,".len();
        assert!(
            prepared_b64_len < 200_000,
            "downscaled base64 still too large: {}",
            prepared_b64_len
        );
    }
}
