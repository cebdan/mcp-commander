use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use serde::{Deserialize, Serialize};

pub const MAX_ATTACHMENTS: usize = 5;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CLIPBOARD_RGBA_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Attachment {
    Text { name: String, text: String },
    Image { name: String, mime: String, data: String, bytes: usize },
}

impl Attachment {
    pub fn name(&self) -> &str {
        match self { Self::Text { name, .. } | Self::Image { name, .. } => name }
    }

    pub fn image_bytes(&self) -> usize {
        match self { Self::Image { bytes, .. } => *bytes, Self::Text { .. } => 0 }
    }

    pub fn is_image(&self) -> bool { matches!(self, Self::Image { .. }) }

    pub fn from_clipboard_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<Self> {
        let raw_bytes = width.checked_mul(height).and_then(|pixels| pixels.checked_mul(4))
            .context("Clipboard image dimensions are too large")?;
        if width == 0 || height == 0 || raw_bytes != rgba.len() {
            bail!("Clipboard image has invalid dimensions or pixel data");
        }
        if raw_bytes > MAX_CLIPBOARD_RGBA_BYTES {
            bail!("Clipboard image is too large to attach");
        }
        let width = u32::try_from(width).context("Clipboard image is too wide")?;
        let height = u32::try_from(height).context("Clipboard image is too tall")?;
        let mut encoded = Vec::new();
        PngEncoder::new(&mut encoded).write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .context("Cannot encode clipboard image")?;
        if encoded.len() > MAX_IMAGE_BYTES { bail!("Clipboard image is larger than 4 MiB"); }
        Ok(Self::Image { name: "clipboard.png".into(), mime: "image/png".into(),
            data: STANDARD.encode(&encoded), bytes: encoded.len() })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let name = path.file_name().context("File has no name")?.to_string_lossy().into_owned();
        let file = File::open(path).with_context(|| format!("Cannot open {name}"))?;
        let mut data = Vec::new();
        file.take((MAX_IMAGE_BYTES + 1) as u64).read_to_end(&mut data)
            .with_context(|| format!("Cannot read {name}"))?;
        if data.len() > MAX_IMAGE_BYTES { bail!("{name} is larger than 4 MiB"); }

        if let Some(mime) = image_mime(&data) {
            return Ok(Self::Image {
                name, mime: mime.into(), data: STANDARD.encode(&data), bytes: data.len(),
            });
        }
        if data.starts_with(b"%PDF-") {
            bail!("{name}: PDF files are not supported; attach extracted text or page images");
        }
        if data.len() > MAX_TEXT_BYTES { bail!("{name} is larger than the 256 KiB text limit"); }
        let text = String::from_utf8(data)
            .with_context(|| format!("{name} is not UTF-8 text or a supported image"))?;
        if text.contains('\0') { bail!("{name} contains binary data"); }
        Ok(Self::Text { name, text })
    }
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some("image/png") }
    else if bytes.starts_with(b"\xff\xd8\xff") { Some("image/jpeg") }
    else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_images_by_content() {
        assert_eq!(image_mime(b"\x89PNG\r\n\x1a\nmore"), Some("image/png"));
        assert_eq!(image_mime(b"\xff\xd8\xffmore"), Some("image/jpeg"));
        assert_eq!(image_mime(b"RIFFxxxxWEBPmore"), Some("image/webp"));
        assert_eq!(image_mime(b"not an image"), None);
    }

    #[test]
    fn clipboard_rgba_becomes_png_attachment() {
        let image = Attachment::from_clipboard_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
        match image {
            Attachment::Image { mime, data, bytes, .. } => {
                assert_eq!(mime, "image/png");
                let decoded = STANDARD.decode(data).unwrap();
                assert_eq!(bytes, decoded.len());
                assert_eq!(image_mime(&decoded), Some("image/png"));
            }
            _ => panic!("Expected image attachment"),
        }
        assert!(Attachment::from_clipboard_rgba(2, 1, &[255, 0, 0, 255]).is_err());
    }
}
