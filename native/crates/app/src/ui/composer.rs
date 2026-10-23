//! What the message input does beyond typing (`Composer.tsx`): `@` references and `/` commands, and images.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use rainy_carrier::ExecutionTarget;
use rainy_protocol::ImageContent;

/// Most images one message carries.
pub const MAX_IMAGES: usize = 8;
/// Longest image edge sent to the model; larger images are scaled down.
const MAX_IMAGE_EDGE: u32 = 1568;
/// Largest image file accepted.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// Encoded size above which an image is sent as JPEG even when it fits.
const REENCODE_BYTES: usize = 3_500_000;

/// Commands typed as `/name` alone.
pub const COMMANDS: [(&str, &str); 2] = [("compact", "chat.commandCompact"), ("new", "chat.commandNew")];

/// What the text before the caret asks to complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// `/word` at the very start.
    Command { query: String },
    /// `@path` after a space or at the start; `start` is the byte offset of the `@`.
    Reference { start: usize, query: String },
}

/// The trigger at the caret of `before`, the text in front of it.
pub fn find_trigger(before: &str) -> Option<Trigger> {
    if let Some(word) = before.strip_prefix('/')
        && word.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return Some(Trigger::Command { query: word.to_owned() });
    }
    let at = before.rfind('@')?;
    let preceded = before[..at].chars().next_back().is_none_or(char::is_whitespace);
    let rest = &before[at + 1..];
    let quoted = rest.strip_prefix('"');
    let valid = match quoted {
        Some(inside) => !inside.contains('"'),
        None => !rest.chars().any(|c| c.is_whitespace() || c == '@' || c == '"'),
    };
    (preceded && valid).then(|| Trigger::Reference { start: at, query: quoted.unwrap_or(rest).to_owned() })
}

/// A path as typed after `@`: quoted when it has spaces.
pub fn quote_path(path: &str) -> String {
    if path.chars().any(char::is_whitespace) { format!("\"{}\"", path.replace('"', "")) } else { path.to_owned() }
}

/// The MIME type of an image file the model accepts.
pub fn image_mime(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Model input for an image: sent as it is when small enough, else scaled to `MAX_IMAGE_EDGE` and sent as JPEG.
pub fn encode_image(bytes: &[u8], mime: &str) -> Result<ImageContent> {
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!("image larger than 20 MB");
    }
    let decoded = image::load_from_memory(bytes).context("unreadable image")?;
    let (width, height) = (decoded.width(), decoded.height());
    let fits = width.max(height) <= MAX_IMAGE_EDGE;
    let engine = base64::engine::general_purpose::STANDARD;
    if fits && bytes.len() <= REENCODE_BYTES {
        return Ok(ImageContent { kind: "image".into(), data: engine.encode(bytes), mime_type: mime.into() });
    }
    let scale = (MAX_IMAGE_EDGE as f32 / width.max(height) as f32).min(1.0);
    let resized = decoded.resize(((width as f32 * scale).round() as u32).max(1), ((height as f32 * scale).round() as u32).max(1), image::imageops::FilterType::Triangle);
    // Transparency becomes white, as on the 3.x canvas.
    let mut flat = image::RgbImage::from_pixel(resized.width(), resized.height(), image::Rgb([255, 255, 255]));
    for (x, y, pixel) in resized.to_rgba8().enumerate_pixels() {
        let alpha = pixel[3] as f32 / 255.0;
        let blend = |channel: u8| (channel as f32 * alpha + 255.0 * (1.0 - alpha)).round() as u8;
        flat.put_pixel(x, y, image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]));
    }
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&flat)?;
    Ok(ImageContent { kind: "image".into(), data: engine.encode(out.into_inner()), mime_type: "image/jpeg".into() })
}

/// A dropped Windows path as the Host reads it: unchanged on Windows, under `/mnt/<drive>` in WSL; none over SSH.
pub fn host_path(path: &Path, target: Option<&ExecutionTarget>) -> Option<String> {
    let text = path.to_str()?;
    match target {
        None | Some(ExecutionTarget::Windows { .. }) => Some(text.to_owned()),
        Some(ExecutionTarget::Wsl { .. }) => {
            let mut chars = text.chars();
            let drive = chars.next()?.to_ascii_lowercase();
            (chars.next() == Some(':')).then(|| format!("/mnt/{drive}{}", text[2..].replace('\\', "/")))
        }
        Some(ExecutionTarget::Ssh { .. }) => None,
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn triggers() {
        assert_eq!(find_trigger("/com"), Some(Trigger::Command { query: "com".into() }));
        assert_eq!(find_trigger("看 @src/ma"), Some(Trigger::Reference { start: "看 ".len(), query: "src/ma".into() }));
        assert_eq!(find_trigger("@\"my fi"), Some(Trigger::Reference { start: 0, query: "my fi".into() }));
        assert_eq!(find_trigger("mail@host"), None);
        assert_eq!(find_trigger("@done "), None);
        assert_eq!(find_trigger("hi /x"), None);
    }

    #[test]
    fn wsl_paths() {
        let target = ExecutionTarget::wsl("id".into(), "Ubuntu");
        assert_eq!(host_path(Path::new(r"C:\Users\a b\x.txt"), Some(&target)).as_deref(), Some("/mnt/c/Users/a b/x.txt"));
        assert_eq!(quote_path("/mnt/c/a b"), "\"/mnt/c/a b\"");
    }

    #[test]
    fn small_images_pass_through() {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(4, 3, image::Rgb([1, 2, 3])).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let content = encode_image(&png, "image/png").unwrap();
        assert_eq!(content.mime_type, "image/png");
        let large = image::RgbImage::from_pixel(3000, 100, image::Rgb([9, 9, 9]));
        let mut bytes = Vec::new();
        large.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        assert_eq!(encode_image(&bytes, "image/png").unwrap().mime_type, "image/jpeg");
    }
}
