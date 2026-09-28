//! LaTeX math rendering, entirely in Rust.
//!
//! A formula is translated from LaTeX to Typst math markup, laid out by the
//! Typst compiler with its bundled New Computer Modern Math font, and
//! exported as SVG. Nothing here touches the file system or the network: the
//! compiler runs in a sealed world that only knows the formula's own source.

use std::sync::LazyLock;

use typst::{
    Library, LibraryExt as _, World,
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime, Duration},
    layout::{Abs, Frame, FrameItem},
    syntax::{FileId, Source},
    text::{Font, FontBook},
    utils::LazyHash,
};
use typst_layout::PagedDocument;

/// A rendered formula. Sizes are in the pixels the formula was laid out for.
#[derive(Debug, Clone)]
pub struct MathSvg {
    pub svg: String,
    pub width: f32,
    pub height: f32,
    /// Distance from the top to the text baseline, for aligning inline math.
    pub baseline: f32,
}

/// How a formula sits in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MathStyle {
    /// Inside a line of text (`$…$`).
    Inline,
    /// On its own, centered and in display style (`$$…$$`).
    Display,
}

/// Render the LaTeX formula `tex` at `font_size` pixels in `color`
/// (`#rrggbbaa`). Returns a message suitable for the user on failure.
pub fn render(tex: &str, style: MathStyle, font_size: f32, color: &str) -> Result<MathSvg, String> {
    let math = tex2typst_rs::tex2typst(tex.trim())
        .map_err(|err| format!("Couldn’t read the formula: {err}."))?;
    let body = match style {
        // Spaces inside the dollars make Typst lay out a display equation.
        MathStyle::Display => format!("$ {math} $"),
        MathStyle::Inline => format!("${math}$"),
    };
    let source = format!(
        "#set page(width: auto, height: auto, margin: 0pt, fill: none)\n\
         #set text(size: {font_size}pt, fill: rgb(\"{color}\"), top-edge: \"bounds\", bottom-edge: \"bounds\")\n\
         #set math.equation(numbering: none)\n\
         {body}\n"
    );

    let world = MathWorld {
        source: Source::detached(source),
    };
    let document = typst::compile::<PagedDocument>(&world)
        .output
        .map_err(|errors| {
            errors
                .first()
                .map(|error| format!("Couldn’t typeset the formula: {}.", error.message))
                .unwrap_or_else(|| "Couldn’t typeset the formula.".to_string())
        });
    // Typst memoizes layout across compilations; keep only recent entries so
    // many distinct formulas don't accumulate.
    comemo::evict(30);
    let document = document?;

    let page = document
        .pages()
        .first()
        .ok_or_else(|| "The formula is empty.".to_string())?;
    let size = page.frame.size();
    let svg = typst_svg::svg(page, &typst_svg::SvgOptions::default());
    Ok(MathSvg {
        svg,
        width: size.x.to_pt() as f32,
        height: size.y.to_pt() as f32,
        baseline: baseline(&page.frame).to_pt() as f32,
    })
}

/// The baseline of the first line of `frame`: the page holds a paragraph
/// whose line frames carry their baseline.
fn baseline(frame: &Frame) -> Abs {
    for (position, item) in frame.items() {
        if let FrameItem::Group(group) = item {
            if group.frame.has_baseline() {
                return position.y + group.frame.baseline();
            }
            let inner = baseline(&group.frame);
            if inner < group.frame.height() {
                return position.y + inner;
            }
        }
    }
    frame.baseline()
}

static LIBRARY: LazyLock<LazyHash<Library>> = LazyLock::new(|| LazyHash::new(Library::default()));

static FONTS: LazyLock<(LazyHash<FontBook>, Vec<Font>)> = LazyLock::new(|| {
    let fonts: Vec<Font> = typst_assets::fonts()
        .flat_map(|data| Font::iter(Bytes::new(data)))
        .collect();
    (LazyHash::new(FontBook::from_fonts(&fonts)), fonts)
});

/// A Typst world containing a single source file and the bundled fonts.
struct MathWorld {
    source: Source,
}

impl World for MathWorld {
    fn library(&self) -> &LazyHash<Library> {
        &LIBRARY
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &FONTS.0
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

    fn file(&self, _: FileId) -> FileResult<Bytes> {
        Err(FileError::AccessDenied)
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.1.get(index).cloned()
    }

    fn today(&self, _: Option<Duration>) -> Option<Datetime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline_and_display_math() {
        let inline = render("x^2 + y_1", MathStyle::Inline, 16., "#000000ff").unwrap();
        assert!(inline.svg.starts_with("<svg"));
        assert!(inline.width > 10. && inline.height > 5.);
        assert!(inline.baseline > 0. && inline.baseline < inline.height);

        let display = render(
            r"\sum_{i=1}^{n} \frac{1}{i^2}",
            MathStyle::Display,
            16.,
            "#000000ff",
        )
        .unwrap();
        // Display style stacks the limits and the fraction.
        assert!(display.height > inline.height * 1.5);
    }

    #[test]
    fn scales_with_font_size() {
        let small = render(r"\alpha", MathStyle::Inline, 12., "#000000ff").unwrap();
        let large = render(r"\alpha", MathStyle::Inline, 24., "#000000ff").unwrap();
        assert!((large.width / small.width - 2.).abs() < 0.1);
    }

    #[test]
    fn reports_unsupported_input() {
        assert!(render(r"\frac{1}{", MathStyle::Inline, 16., "#000000ff").is_err());
    }
}
