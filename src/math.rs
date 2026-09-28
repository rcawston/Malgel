//! LaTeX math rendering, entirely in Rust.
//!
//! A formula is translated from LaTeX to Typst math markup, laid out by the
//! Typst compiler with its bundled New Computer Modern Math font, and
//! exported as SVG. Nothing here touches the file system or the network: the
//! compiler runs in a sealed world that only knows the formula's own source.

use typst::layout::{Abs, Frame, FrameItem};
use typst_layout::Page;

use crate::typst_world::{self, VirtualFiles};

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

/// A formula rasterized as PNG, for formats that can't embed SVG.
pub struct MathPng {
    pub png: Vec<u8>,
    /// Size and baseline in points, as laid out (not in image pixels).
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
}

/// Render the LaTeX formula `tex` at `font_size` pixels in `color`
/// (`#rrggbbaa`). Returns a message suitable for the user on failure.
pub fn render(tex: &str, style: MathStyle, font_size: f32, color: &str) -> Result<MathSvg, String> {
    let page = layout(tex, style, font_size, color)?;
    let size = page.frame.size();
    Ok(MathSvg {
        svg: typst_svg::svg(&page, &typst_svg::SvgOptions::default()),
        width: size.x.to_pt() as f32,
        height: size.y.to_pt() as f32,
        baseline: baseline(&page.frame).to_pt() as f32,
    })
}

/// Render `tex` as a PNG with `pixels_per_point` pixels for every point of
/// its laid-out size, so it stays sharp when printed.
pub fn render_png(
    tex: &str,
    style: MathStyle,
    font_size: f32,
    color: &str,
    pixels_per_point: f32,
) -> Result<MathPng, String> {
    let page = layout(tex, style, font_size, color)?;
    let size = page.frame.size();
    let png = typst_world::rasterize(&page, pixels_per_point)?;
    Ok(MathPng {
        png,
        width: size.x.to_pt() as f32,
        height: size.y.to_pt() as f32,
        baseline: baseline(&page.frame).to_pt() as f32,
    })
}

fn layout(tex: &str, style: MathStyle, font_size: f32, color: &str) -> Result<Page, String> {
    let math = tex2typst_rs::tex2typst(tex.trim())
        .map_err(|err| format!("Couldn’t read the formula: {err}."))?;
    let body = match style {
        // Spaces inside the dollars make Typst lay out a display equation.
        MathStyle::Display => format!("$ {math} $"),
        // A box keeps the formula's baseline in the page frame, which
        // Typst otherwise flattens away for single-term formulas.
        MathStyle::Inline => format!("#box[${math}$]"),
    };
    let source = format!(
        "#set page(width: auto, height: auto, margin: 0pt, fill: none)\n\
         #set text(size: {font_size}pt, fill: rgb(\"{color}\"), top-edge: \"bounds\", bottom-edge: \"bounds\")\n\
         #set math.equation(numbering: none)\n\
         {body}\n"
    );

    let document = typst_world::compile(source, VirtualFiles::new(), &typst_world::BUNDLED_FONTS)
        .map_err(|error| format!("Couldn’t typeset the formula: {error}."))?;
    document
        .pages()
        .first()
        .cloned()
        .ok_or_else(|| "The formula is empty.".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline_and_display_math() {
        let inline = render("x^2 + y_1", MathStyle::Inline, 16., "#000000ff").unwrap();
        assert!(inline.svg.starts_with("<svg"));
        assert!(inline.width > 10. && inline.height > 5.);
        assert!(inline.baseline > 0. && inline.baseline < inline.height);

        // Single terms too: the subscript hangs below the baseline.
        let term = render("y_1", MathStyle::Inline, 16., "#000000ff").unwrap();
        assert!(term.baseline < term.height * 0.8);

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
    fn rasterizes_for_documents() {
        let png = render_png(r"\frac{a}{b}", MathStyle::Display, 16., "#000000ff", 4.).unwrap();
        assert!(png.png.starts_with(b"\x89PNG"));
        assert!(png.height > png.width * 0.5);
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
