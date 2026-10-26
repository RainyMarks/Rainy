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

/// Whether a dropped file is attached as an image (by its extension; the bytes decide the type sent).
pub fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
}

/// Why an image was not attached; the composer words it in the interface language.
#[derive(Debug)]
pub enum ImageError {
    /// Larger than `MAX_IMAGE_BYTES`.
    TooLarge,
    /// Not an image the decoder reads.
    Unreadable,
    /// The file could not be read.
    Read(std::io::Error),
}

/// An image file's bytes, refused before reading when it is too large.
pub fn read_image(path: &Path) -> Result<Vec<u8>, ImageError> {
    if std::fs::metadata(path).map_err(ImageError::Read)?.len() > MAX_IMAGE_BYTES as u64 {
        return Err(ImageError::TooLarge);
    }
    std::fs::read(path).map_err(ImageError::Read)
}

/// An image to attach: its model input, and the bytes of that input for the composer's thumbnail.
pub fn prepare_image(bytes: &[u8]) -> Result<(ImageContent, Vec<u8>), ImageError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageError::TooLarge);
    }
    let content = encode_image(bytes).map_err(|_| ImageError::Unreadable)?;
    let shown = base64::engine::general_purpose::STANDARD.decode(&content.data).map_err(|_| ImageError::Unreadable)?;
    Ok((content, shown))
}

/// Model input for an image, typed by its bytes rather than by a file name or the clipboard: a PNG, JPEG, GIF or WebP
/// small enough is sent as it is; another format (a Windows screenshot on the clipboard is a BMP) is sent as PNG when
/// that is small enough; anything else is scaled to `MAX_IMAGE_EDGE` and sent as JPEG.
pub fn encode_image(bytes: &[u8]) -> Result<ImageContent> {
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!("image larger than 20 MB");
    }
    let format = image::guess_format(bytes).context("unreadable image")?;
    let decoded = image::load_from_memory_with_format(bytes, format).context("unreadable image")?;
    let (width, height) = (decoded.width(), decoded.height());
    let fits = width.max(height) <= MAX_IMAGE_EDGE;
    let engine = base64::engine::general_purpose::STANDARD;
    let content = |data: &[u8], mime: &str| ImageContent { kind: "image".into(), data: engine.encode(data), mime_type: mime.into() };
    if fits {
        let model_type = match format {
            image::ImageFormat::Png => Some("image/png"),
            image::ImageFormat::Jpeg => Some("image/jpeg"),
            image::ImageFormat::Gif => Some("image/gif"),
            image::ImageFormat::WebP => Some("image/webp"),
            _ => None,
        };
        match model_type {
            Some(mime) if bytes.len() <= REENCODE_BYTES => return Ok(content(bytes, mime)),
            Some(_) => {}
            None => {
                let mut png = Cursor::new(Vec::new());
                if decoded.write_to(&mut png, image::ImageFormat::Png).is_ok() && png.get_ref().len() <= REENCODE_BYTES {
                    return Ok(content(png.get_ref(), "image/png"));
                }
            }
        }
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
    Ok(content(out.get_ref(), "image/jpeg"))
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
        let content = encode_image(&png).unwrap();
        assert_eq!(content.mime_type, "image/png");
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(4, 3, image::Rgb([1, 2, 3])).write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        // The type comes from the bytes, whatever name the file or the clipboard gave them.
        assert_eq!(encode_image(&jpeg).unwrap().mime_type, "image/jpeg");
        let large = image::RgbImage::from_pixel(3000, 100, image::Rgb([9, 9, 9]));
        let mut bytes = Vec::new();
        large.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        assert_eq!(encode_image(&bytes).unwrap().mime_type, "image/jpeg");
    }

    /// A clipboard image as GPUI reads it on Windows: the 32-bit `BI_RGB` DIB behind a file header, alpha bytes zero.
    fn clipboard_bmp(width: u32, height: u32, [red, green, blue]: [u8; 3]) -> Vec<u8> {
        let pixels = (width * height * 4) as usize;
        let mut bmp = Vec::with_capacity(54 + pixels);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54 + pixels as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(width as i32).to_le_bytes());
        bmp.extend_from_slice(&(height as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        for _ in 0..width * height {
            bmp.extend_from_slice(&[blue, green, red, 0]);
        }
        bmp
    }

    #[test]
    fn pasted_bitmaps_become_png() {
        let bmp = clipboard_bmp(64, 48, [70, 130, 180]);
        let (content, shown) = prepare_image(&bmp).unwrap();
        assert_eq!(content.mime_type, "image/png");
        assert_eq!(image::guess_format(&shown).unwrap(), image::ImageFormat::Png);
        let png = image::load_from_memory(&shown).unwrap().to_rgba8();
        assert_eq!(png.dimensions(), (64, 48));
        assert_eq!(png.get_pixel(10, 10).0, [70, 130, 180, 255]);
        // Too large for a PNG of its own, a bitmap goes the JPEG way.
        let mut noisy = clipboard_bmp(1500, 1000, [10, 20, 30]);
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        for (index, byte) in noisy[54..].iter_mut().enumerate() {
            if index % 4 != 3 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = (state >> 24) as u8;
            }
        }
        assert_eq!(encode_image(&noisy).unwrap().mime_type, "image/jpeg");
    }

    #[test]
    fn prepared_images_keep_their_thumbnail() {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(4, 3, image::Rgb([1, 2, 3])).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let (content, shown) = prepare_image(&png).unwrap();
        assert_eq!(content.mime_type, "image/png");
        assert_eq!(shown, png);
        assert!(matches!(prepare_image(b"not an image"), Err(ImageError::Unreadable)));
        assert!(matches!(prepare_image(&vec![0; MAX_IMAGE_BYTES + 1]), Err(ImageError::TooLarge)));
        assert!(matches!(read_image(Path::new("Z:/no/such/image.png")), Err(ImageError::Read(_))));
    }
}
