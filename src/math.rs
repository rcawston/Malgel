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

/// Translate the LaTeX formula `tex` to Typst math markup (without the
/// surrounding dollars).
pub fn to_typst(tex: &str) -> Result<String, String> {
    let tex = tex.trim();
    // The converter closes whatever is left open; say so instead of
    // quietly drawing something else.
    if let Some(problem) = unbalanced(tex) {
        return Err(format!("Couldn’t read the formula: {problem}."));
    }
    // The converter is written for well-formed input; formulas are converted
    // as they are typed, so never let a half-written one take down a thread.
    let math = std::panic::catch_unwind(|| tylax::latex_to_typst(tex))
        .map_err(|_| "Couldn’t read the formula.".to_string())?;
    // It reports what it can't match as a comment and carries on.
    if math.contains("/* LaTeX Error") {
        return Err("Couldn’t read the formula: a brace or environment isn’t closed.".into());
    }
    let math = math.trim();
    // Environments such as `align` come back as a whole equation.
    let math = math
        .strip_prefix('$')
        .and_then(|math| math.strip_suffix('$'))
        .unwrap_or(math)
        .trim();
    Ok(space_cases(math))
}

/// Put a quad between each case and its condition, as LaTeX's `cases` does;
/// Typst's aligns them with no gap.
fn space_cases(math: &str) -> String {
    const OPEN: &str = "cases(";
    let mut out = String::with_capacity(math.len());
    let mut rest = math;
    while let Some(start) = rest.find(OPEN) {
        let (before, after) = rest.split_at(start + OPEN.len());
        out.push_str(before);
        // Walk the arguments, spacing alignment points at their own level.
        let mut depth = 0usize;
        let mut in_string = false;
        let mut end = after.len();
        let mut chars = after.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            match ch {
                '"' => in_string = !in_string,
                '\\' if in_string => {
                    chars.next();
                }
                _ if in_string => {}
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => {
                    end = index;
                    break;
                }
                ')' | ']' | '}' => depth -= 1,
                '&' if depth == 0 => {
                    out.push_str("& quad");
                    continue;
                }
                _ => {}
            }
            out.push(ch);
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// What is left open (or closed without being opened) in `tex`, if anything.
fn unbalanced(tex: &str) -> Option<&'static str> {
    let (mut braces, mut environments, mut delimiters) = (0i32, 0i32, 0i32);
    let mut chars = tex.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                let mut name = String::new();
                while let Some(&next) = chars.peek() {
                    if !next.is_ascii_alphabetic() {
                        break;
                    }
                    name.push(next);
                    chars.next();
                }
                match name.as_str() {
                    // An escaped character such as `\{` or `\\`.
                    "" => {
                        chars.next();
                    }
                    "begin" => environments += 1,
                    "end" => environments -= 1,
                    "left" => delimiters += 1,
                    "right" => delimiters -= 1,
                    _ => {}
                }
            }
            '{' => braces += 1,
            '}' => braces -= 1,
            _ => {}
        }
        if braces < 0 {
            return Some("a closing brace has no opening one");
        }
        if environments < 0 {
            return Some("an \\end has no \\begin");
        }
        if delimiters < 0 {
            return Some("a \\right has no \\left");
        }
    }
    if braces > 0 {
        Some("a brace isn’t closed")
    } else if environments > 0 {
        Some("a \\begin has no \\end")
    } else if delimiters > 0 {
        Some("a \\left has no \\right")
    } else {
        None
    }
}

/// Typst markup for the Typst math `math` as an equation in `style`.
pub fn equation(math: &str, style: MathStyle) -> String {
    match style {
        // Spaces inside the dollars make Typst lay out a display equation.
        MathStyle::Display => format!("$ {math} $"),
        // A box keeps the formula's baseline in the page frame, which
        // Typst otherwise flattens away for single-term formulas.
        MathStyle::Inline => format!("#box[${math}$]"),
    }
}

/// The Typst math for `tex` once it is known to typeset, for documents
/// where one bad formula would otherwise stop the whole document.
pub fn checked_typst(tex: &str, style: MathStyle) -> Result<String, String> {
    let math = to_typst(tex)?;
    typeset(&equation(&math, style), 11., "#000000ff")?;
    Ok(math)
}

fn layout(tex: &str, style: MathStyle, font_size: f32, color: &str) -> Result<Page, String> {
    typeset(&equation(&to_typst(tex)?, style), font_size, color)
}

fn typeset(body: &str, font_size: f32, color: &str) -> Result<Page, String> {
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
        for tex in [
            r"\frac{1}{",
            r"\frac{a}",
            "}",
            r"\end{cases}",
            r"\foo{x}",
            r"\sqrt",
            r"x^{2",
            r"\left( x",
            r"\begin{matrix} a",
        ] {
            assert!(
                render(tex, MathStyle::Inline, 16., "#000000ff").is_err(),
                "{tex}"
            );
            assert!(checked_typst(tex, MathStyle::Inline).is_err(), "{tex}");
        }
    }

    #[test]
    fn finds_what_is_left_open() {
        assert_eq!(unbalanced(r"\frac{1}{"), Some("a brace isn’t closed"));
        assert_eq!(unbalanced("a}"), Some("a closing brace has no opening one"));
        assert_eq!(
            unbalanced(r"\begin{cases} x"),
            Some("a \\begin has no \\end")
        );
        assert_eq!(unbalanced(r"\left( x"), Some("a \\left has no \\right"));
        // Escaped braces and line breaks aren't groups.
        assert_eq!(unbalanced(r"\{ a \} \\ {b}"), None);
        assert_eq!(unbalanced(r"\left\{ x \right."), None);
        assert_eq!(unbalanced(r"\begin{cases} 1 \end{cases}"), None);
    }

    #[test]
    fn spaces_cases_like_latex() {
        assert_eq!(
            space_cases(r#"f(x) = cases(1 & x > 0, 0 & #text[otherwise])"#),
            r#"f(x) = cases(1 & quad x > 0, 0 & quad #text[otherwise])"#
        );
        // Only the alignment points of the cases themselves.
        assert_eq!(
            space_cases(r#"cases(mat(a & b) & "&", x) & y"#),
            r#"cases(mat(a & b) & quad "&", x) & y"#
        );
    }

    #[test]
    fn translates_common_latex() {
        let cases = [
            (r"\frac{a}{b}", "a/b"),
            (r"\sqrt[3]{x}", "root(3, x)"),
            (r"\alpha \le \beta", "alpha <= beta"),
            (r"\mathbb{R}^n", "RR^(n)"),
            (
                r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
                r#"mat(delim: "(", a, b ; c, d)"#,
            ),
            (
                r"\begin{align} a &= b \\ c &= d \end{align}",
                r"a & = b \ c & = d",
            ),
        ];
        for (tex, typst) in cases {
            assert_eq!(to_typst(tex).unwrap(), typst, "{tex}");
        }
    }

    #[test]
    fn typesets_the_usual_constructs() {
        for tex in [
            r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
            r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
            r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
            r"\begin{aligned} x &= 1 \\ y &= 2 \end{aligned}",
            r"\begin{align} a &= b \\ c &= d \end{align}",
            r"\left( \frac{1}{2} \right) \langle x, y \rangle \| v \|",
            r"\mathbf{v} \cdot \hat{x} \vec{v} \overline{AB} \operatorname{sin} x",
            r"\lim_{x \to 0} \frac{\sin x}{x} \neq \infty",
            r"\binom{n}{k} \color{red}{x} \mathrm{d}x",
        ] {
            assert!(checked_typst(tex, MathStyle::Display).is_ok(), "{tex}");
        }
    }
}
