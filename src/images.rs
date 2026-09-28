//! Resolves the image URLs a document references.
//!
//! Markdown files usually point at images relative to themselves
//! (`![diagram](assets/diagram.png)`), which only makes sense once the
//! document's folder is known. Remote URLs load through GPUI's HTTP client
//! and `data:` URLs are decoded once and cached.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use gpui_kit::{Image, ImageFormat, ImageSource, SharedUri};

use crate::analysis::content_hash;

/// Resolve `url` for display, relative to `base_dir` when it is a local path.
pub fn resolve(url: &SharedUri, base_dir: Option<&Path>) -> ImageSource {
    let raw = url.as_ref();
    if let Some(data) = raw.strip_prefix("data:") {
        return decode_data_url(data)
            .map(ImageSource::Image)
            .unwrap_or_else(|| url.clone().into());
    }
    match local_path(raw, base_dir) {
        Some(path) => ImageSource::from(path),
        None => url.clone().into(),
    }
}

/// The file an image URL refers to, or `None` for remote URLs.
pub fn local_path(url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = url.strip_prefix("file://") {
        return Some(PathBuf::from(percent_decode(path)));
    }
    if url.contains("://") || url.starts_with("//") || url.starts_with("mailto:") {
        return None;
    }
    // Drop a query or fragment; neither names part of a local file.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = PathBuf::from(percent_decode(path));
    if path.is_absolute() {
        return Some(path);
    }
    Some(match base_dir {
        Some(dir) => dir.join(path),
        None => std::env::current_dir().ok()?.join(path),
    })
}

/// An image embedded into an exported document.
pub struct ImageData {
    pub bytes: Vec<u8>,
    /// Lowercase file extension naming the format: `png`, `jpg`, `gif`,
    /// `webp`, `svg` or `bmp`.
    pub extension: &'static str,
}

/// Read the image `url` points at, for embedding into an export. Remote
/// images are not fetched; exports stay offline and deterministic.
pub fn load(url: &str, base_dir: Option<&Path>) -> Option<ImageData> {
    let bytes = match url.strip_prefix("data:") {
        Some(data) => data_url_bytes(data)?.0,
        None => std::fs::read(local_path(url, base_dir)?).ok()?,
    };
    let extension = sniff_format(&bytes)?;
    Some(ImageData { bytes, extension })
}

/// Identify an image format from its first bytes.
fn sniff_format(bytes: &[u8]) -> Option<&'static str> {
    let head = &bytes[..bytes.len().min(512)];
    Some(if head.starts_with(b"\x89PNG") {
        "png"
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if head.starts_with(b"GIF8") {
        "gif"
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        "webp"
    } else if head.starts_with(b"BM") {
        "bmp"
    } else if String::from_utf8_lossy(head).contains("<svg") {
        "svg"
    } else {
        return None;
    })
}

/// The bytes and MIME type of a `data:` URL (without the `data:` prefix).
fn data_url_bytes(data: &str) -> Option<(Vec<u8>, &str)> {
    let (meta, payload) = data.split_once(',')?;
    let mime = meta.split(';').next()?;
    let bytes = if meta.ends_with(";base64") {
        decode_base64(payload)?
    } else {
        percent_decode(payload).into_bytes()
    };
    Some((bytes, mime))
}

fn decode_data_url(data: &str) -> Option<Arc<Image>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Arc<Image>>>> = OnceLock::new();
    let key = content_hash(data);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(image) = cache.lock().ok()?.get(&key) {
        return Some(image.clone());
    }

    let (bytes, mime) = data_url_bytes(data)?;
    let format = ImageFormat::from_mime_type(mime)
        .or_else(|| (mime == "image/svg").then_some(ImageFormat::Svg))?;

    let image = Arc::new(Image::from_bytes(format, bytes));
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, image.clone());
    }
    Some(image)
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }

    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        buffer = (buffer << 6) | value(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        if bytes[ix] == b'%'
            && ix + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[ix + 1]), hex(bytes[ix + 2]))
        {
            out.push((high * 16 + low) as u8);
            ix += 3;
            continue;
        }
        out.push(bytes[ix]);
        ix += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths_against_the_document() {
        let base = Path::new("/docs/guide");
        assert_eq!(
            local_path("img/a%20b.png?raw=1", Some(base)),
            Some(PathBuf::from("/docs/guide/img/a b.png"))
        );
        assert_eq!(
            local_path("/abs/x.png", Some(base)),
            Some(PathBuf::from("/abs/x.png"))
        );
        assert_eq!(
            local_path("file:///tmp/x.png", Some(base)),
            Some(PathBuf::from("/tmp/x.png"))
        );
        assert_eq!(local_path("https://example.com/x.png", Some(base)), None);
    }

    #[test]
    fn loads_images_for_export() {
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let image = load(png, None).unwrap();
        assert_eq!(image.extension, "png");
        assert!(load("https://example.com/a.png", None).is_none());
        assert!(load("missing.png", Some(Path::new("/nonexistent"))).is_none());
    }

    #[test]
    fn decodes_base64() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGk").unwrap(), b"hi");
        assert!(decode_base64("a$b").is_none());
    }
}
