//! A sealed Typst environment shared by math rendering and PDF export.
//!
//! The compiler sees one source file, the fonts bundled with Typst, and any
//! files the caller hands over in memory (images, icons). It cannot read the
//! file system or reach the network.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{LazyLock, OnceLock},
};

use typst::{
    Library, LibraryExt as _, World,
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime, Duration},
    syntax::{FileId, Source},
    text::{Font, FontBook, FontInfo},
    utils::LazyHash,
};
use typst_layout::PagedDocument;

static LIBRARY: LazyLock<LazyHash<Library>> = LazyLock::new(|| LazyHash::new(Library::default()));

/// A font the compiler can use, loaded on first use.
enum FontSlot {
    Bundled(Font),
    System {
        path: PathBuf,
        index: u32,
        font: OnceLock<Option<Font>>,
    },
}

impl FontSlot {
    fn get(&self) -> Option<Font> {
        match self {
            FontSlot::Bundled(font) => Some(font.clone()),
            FontSlot::System { path, index, font } => font
                .get_or_init(|| {
                    let data = std::fs::read(path).ok()?;
                    Font::new(Bytes::new(data), *index)
                })
                .clone(),
        }
    }
}

/// The fonts a world offers, with the book the compiler selects them from.
pub struct FontSet {
    book: LazyHash<FontBook>,
    slots: Vec<FontSlot>,
}

fn bundled_fonts() -> (FontBook, Vec<FontSlot>) {
    let fonts: Vec<Font> = typst_assets::fonts()
        .flat_map(|data| Font::iter(Bytes::new(data)))
        .collect();
    let book = FontBook::from_fonts(&fonts);
    (book, fonts.into_iter().map(FontSlot::Bundled).collect())
}

/// Only the fonts bundled with Typst: fast to set up, identical everywhere.
pub static BUNDLED_FONTS: LazyLock<FontSet> = LazyLock::new(|| {
    let (book, slots) = bundled_fonts();
    FontSet {
        book: LazyHash::new(book),
        slots,
    }
});

/// The bundled fonts first, then the system's, so documents keep their
/// designed look while scripts and symbols the bundled fonts lack (CJK,
/// emoji, …) fall back to installed fonts. Scanning happens once, on first
/// use, and font data is only read when a font is actually needed.
pub static DOCUMENT_FONTS: LazyLock<FontSet> = LazyLock::new(|| {
    let (mut book, mut slots) = bundled_fonts();
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let mut files: Vec<PathBuf> = database
        .faces()
        .filter_map(|face| match &face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => Some(path.clone()),
            _ => None,
        })
        .collect();
    files.sort();
    files.dedup();
    for path in files {
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        for (index, info) in FontInfo::iter(&data).enumerate() {
            book.push(info);
            slots.push(FontSlot::System {
                path: path.clone(),
                index: index as u32,
                font: OnceLock::new(),
            });
        }
    }
    FontSet {
        book: LazyHash::new(book),
        slots,
    }
});

/// Files available to the document, keyed by their path from the project
/// root without a leading slash (`image("/img-1.png")` → `img-1.png`).
pub type VirtualFiles = HashMap<String, Bytes>;

struct SandboxWorld {
    source: Source,
    files: VirtualFiles,
    fonts: &'static FontSet,
}

impl World for SandboxWorld {
    fn library(&self) -> &LazyHash<Library> {
        &LIBRARY
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.fonts.book
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let path = id.vpath().get_without_slash();
        self.files.get(path).cloned().ok_or(FileError::AccessDenied)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.slots.get(index)?.get()
    }

    fn today(&self, _: Option<Duration>) -> Option<Datetime> {
        None
    }
}

/// Compile Typst `source` into pages. The error is the first diagnostic,
/// worded for the user.
pub fn compile(
    source: String,
    files: VirtualFiles,
    fonts: &'static FontSet,
) -> Result<PagedDocument, String> {
    let world = SandboxWorld {
        source: Source::detached(source),
        files,
        fonts,
    };
    let result = typst::compile::<PagedDocument>(&world).output;
    // Typst memoizes layout across compilations; keep only recent entries so
    // many distinct inputs don't accumulate.
    comemo::evict(30);
    result.map_err(|errors| {
        errors
            .first()
            .map(|error| error.message.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    })
}

/// Rasterize a laid-out page as PNG.
pub fn rasterize(page: &typst_layout::Page, pixels_per_point: f32) -> Result<Vec<u8>, String> {
    let pixmap = typst_render::render(
        page,
        &typst_render::RenderOptions {
            pixel_per_pt: (pixels_per_point as f64).into(),
            render_bleed: false,
        },
    );
    pixmap
        .encode_png()
        .map_err(|error| format!("Couldn’t encode the image: {error}."))
}

/// Rasterize an SVG image (for formats that only take bitmaps), returning
/// the PNG and the image's size in points. Text in the SVG may use any
/// installed font.
pub fn svg_to_png(svg: Vec<u8>, pixels_per_point: f32) -> Result<(Vec<u8>, f32, f32), String> {
    let mut files = VirtualFiles::new();
    files.insert("image.svg".into(), Bytes::new(svg));
    let document = compile(
        "#set page(width: auto, height: auto, margin: 0pt, fill: none)\n#image(\"/image.svg\")\n"
            .into(),
        files,
        &DOCUMENT_FONTS,
    )?;
    let page = document
        .pages()
        .first()
        .ok_or_else(|| "The image is empty.".to_string())?;
    let size = page.frame.size();
    Ok((
        rasterize(page, pixels_per_point)?,
        size.x.to_pt() as f32,
        size.y.to_pt() as f32,
    ))
}

/// Escape `text` for use inside a Typst string literal.
pub fn string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Escape `text` so Typst markup shows it literally.
pub fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '#' | '*' | '_' | '`' | '$' | '<' | '>' | '@' | '[' | ']' | '~' | '=' | '-'
            | '+' | '/' | '"' | '\'' => {
                out.push('\\');
                out.push(ch);
            }
            // Line starts are structural in markup; text never starts one.
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_virtual_files_only() {
        let mut files = VirtualFiles::new();
        files.insert(
            "dot.svg".into(),
            Bytes::new(
                br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"#
                    .to_vec(),
            ),
        );
        assert!(compile("#image(\"/dot.svg\")".into(), files, &BUNDLED_FONTS).is_ok());
        assert!(
            compile(
                "#image(\"/etc/passwd\")".into(),
                VirtualFiles::new(),
                &BUNDLED_FONTS
            )
            .is_err()
        );
    }

    #[test]
    fn escapes_markup_and_strings() {
        let doc = compile(
            format!(
                "{} {}",
                escape_markup("*not bold* #x $y$ // no comment <l> @r [c] 'q' \"q\""),
                "#raw(".to_string() + &string_literal("a \"b\" \\ c") + ")"
            ),
            VirtualFiles::new(),
            &BUNDLED_FONTS,
        );
        assert!(doc.is_ok(), "{doc:?}");
    }
}
