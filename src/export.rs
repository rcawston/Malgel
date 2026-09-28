//! Export a document as a standalone HTML page.

use std::ops::Range;

use gpui_kit::{AssetSource as _, assets::AllAssets};
use markdown::{CompileOptions, Options, mdast::Node};

use crate::{
    analysis::{content_hash, preview_parse_options},
    math::{self, MathStyle},
    preview_ext::{
        AlertKind, DISPLAY_MATH_SCALE, INLINE_MATH_SCALE, display_math_source, is_inline_math,
        parse_alert,
    },
};

/// Font size formulas are laid out for; the page sizes them in `em`, so they
/// scale with the reader's text size.
const MATH_BASE_SIZE: f32 = 16.;
/// An unlikely color rendered formulas use, swapped for `currentColor` so
/// they follow the page's light or dark text color.
const MATH_SENTINEL_COLOR: &str = "#010203";

const STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --fg: #1f2328; --muted: #59636e; --bg: #ffffff; --subtle: #f6f8fa;
  --border: #d1d9e0; --link: #0969da;
  --note: #0969da; --tip: #1a7f37; --important: #8250df; --warning: #9a6700; --caution: #d1242f;
}
@media (prefers-color-scheme: dark) {
  :root {
    --fg: #e6edf3; --muted: #9198a1; --bg: #0d1117; --subtle: #151b23;
    --border: #3d444d; --link: #4493f8;
    --note: #4493f8; --tip: #3fb950; --important: #ab7df8; --warning: #d29922; --caution: #f85149;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg); color: var(--fg);
  font: 16px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif;
}
main { max-width: 46rem; margin: 0 auto; padding: 3rem 1.5rem 5rem; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.6em 0 0.6em; font-weight: 600; }
h1 { font-size: 2em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h2 { font-size: 1.5em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.25em; }
main > :first-child { margin-top: 0; }
p, ul, ol, blockquote, pre, table { margin: 0 0 1em; }
a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
blockquote { margin-left: 0; padding: 0 1em; color: var(--muted); border-left: .25em solid var(--border); }
code, pre { font-family: ui-monospace, SFMono-Regular, "JetBrains Mono", Menlo, Consolas, monospace; font-size: .875em; }
code { background: var(--subtle); padding: .15em .35em; border-radius: 6px; }
pre { background: var(--subtle); padding: 1em; border-radius: 8px; overflow: auto; border: 1px solid var(--border); }
pre code { background: none; padding: 0; font-size: 1em; }
table { border-collapse: collapse; display: block; overflow: auto; }
th, td { border: 1px solid var(--border); padding: .4em .8em; }
th { background: var(--subtle); font-weight: 600; }
img { max-width: 100%; }
hr { border: 0; border-top: 1px solid var(--border); margin: 2em 0; }
li + li { margin-top: .25em; }
input[type=checkbox] { margin-right: .4em; }
.math svg { display: inline-block; overflow: visible; }
.math-display { margin: 0 0 1em; text-align: center; overflow-x: auto; overflow-y: hidden; }
.math-error { color: var(--caution); }
.alert {
  --c: var(--note);
  margin: 0 0 1em; padding: .75em 1em; border-radius: 8px;
  border: 1px solid color-mix(in srgb, var(--c) 35%, transparent);
  background: color-mix(in srgb, var(--c) 6%, transparent);
}
.alert > :last-child { margin-bottom: 0; }
.alert-title {
  display: flex; align-items: center; gap: .5em; margin: 0 0 .35em;
  color: var(--c); font-weight: 600;
}
.alert-title svg { width: 1em; height: 1em; flex: none; }
.alert-tip { --c: var(--tip); }
.alert-important { --c: var(--important); }
.alert-warning { --c: var(--warning); }
.alert-caution { --c: var(--caution); }
"#;

/// Render `source` as a complete HTML document. Raw HTML in the source is
/// escaped, so the exported page cannot run scripts from the document.
/// GitHub alerts become styled callouts and LaTeX math is typeset to inline
/// SVG, so the page needs no scripts or network access to look like the
/// preview.
pub fn to_html(source: &str, title: &str) -> String {
    let body = Exporter::default().fragment(source);
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"generator\" content=\"Malgel\">\n<title>{}</title>\n<style>{STYLE}</style>\n\
         </head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n",
        escape(title)
    )
}

fn compile(source: &str) -> String {
    let options = Options {
        parse: preview_parse_options(),
        compile: CompileOptions::gfm(),
    };
    markdown::to_html_with_options(source, &options)
        .unwrap_or_else(|_| format!("<pre>{}</pre>", escape(source)))
}

/// Something the Markdown compiler cannot render, found in the source and
/// replaced by its own HTML.
struct Replacement {
    range: Range<usize>,
    /// Blocks stand alone, so the compiler wraps their token in a paragraph.
    block: bool,
    html: String,
}

#[derive(Default)]
struct Exporter {
    /// Numbers the formulas, so their SVG ids stay unique in the page.
    formulas: usize,
}

impl Exporter {
    /// Render a Markdown fragment. Alerts and math are cut out of the source
    /// and replaced by tokens the compiler passes through as text, then the
    /// tokens are swapped for their HTML.
    fn fragment(&mut self, source: &str) -> String {
        let Ok(root) = markdown::to_mdast(source, &preview_parse_options()) else {
            return compile(source);
        };
        let mut replacements = Vec::new();
        self.collect(&root, source, &mut replacements);
        replacements.sort_by_key(|replacement| replacement.range.start);

        let nonce = content_hash(source);
        let mut rewritten = String::with_capacity(source.len());
        let mut tokens = Vec::new();
        let mut copied = 0;
        for (ix, replacement) in replacements.into_iter().enumerate() {
            if replacement.range.start < copied {
                continue;
            }
            let token = format!("MALGEL{nonce:x}X{ix}Z");
            rewritten.push_str(&source[copied..replacement.range.start]);
            if replacement.block {
                push_block_token(&mut rewritten, source, &replacement.range, &token);
            } else {
                rewritten.push_str(&token);
            }
            copied = replacement.range.end;
            tokens.push((token, replacement.block, replacement.html));
        }
        rewritten.push_str(&source[copied..]);

        let mut html = compile(&rewritten);
        for (token, block, replacement) in tokens {
            let paragraph = format!("<p>{token}</p>");
            if block && html.contains(&paragraph) {
                html = html.replacen(&paragraph, &replacement, 1);
            } else {
                html = html.replacen(&token, &replacement, 1);
            }
        }
        html
    }

    fn collect(&mut self, node: &Node, source: &str, out: &mut Vec<Replacement>) {
        let Some(range) = node
            .position()
            .map(|position| position.start.offset..position.end.offset)
            .filter(|range| source.get(range.clone()).is_some())
            .or_else(|| matches!(node, Node::Root(_)).then(|| 0..source.len()))
        else {
            return;
        };
        let text = &source[range.clone()];

        match node {
            Node::Code(_) | Node::InlineCode(_) | Node::Html(_) | Node::Yaml(_) => {}
            Node::Blockquote(_) => match parse_alert(text) {
                Some((kind, body)) => {
                    let html = self.alert(kind, &body);
                    out.push(Replacement {
                        range,
                        block: true,
                        html,
                    });
                }
                None => self.collect_children(node, source, out),
            },
            Node::Math(math) => {
                let html = self.formula(&math.value, MathStyle::Display);
                out.push(Replacement {
                    range,
                    block: true,
                    html,
                });
            }
            Node::Paragraph(_) if display_math_source(text).is_some() => {
                let tex = display_math_source(text).unwrap_or_default().to_string();
                let html = self.formula(&tex, MathStyle::Display);
                out.push(Replacement {
                    range,
                    block: true,
                    html,
                });
            }
            Node::InlineMath(math) => {
                let following = source[range.end..].chars().next();
                // Like the preview, prose such as "$5 and $10" stays literal.
                let html = if is_inline_math(&math.value, following) {
                    self.formula(&math.value, MathStyle::Inline)
                } else {
                    escape(text)
                };
                out.push(Replacement {
                    range,
                    block: false,
                    html,
                });
            }
            _ => self.collect_children(node, source, out),
        }
    }

    fn collect_children(&mut self, node: &Node, source: &str, out: &mut Vec<Replacement>) {
        for child in node.children().into_iter().flatten() {
            self.collect(child, source, out);
        }
    }

    fn alert(&mut self, kind: AlertKind, body: &str) -> String {
        let name = kind.title();
        let icon = AllAssets
            .load(&kind.icon().path())
            .ok()
            .flatten()
            .and_then(|svg| String::from_utf8(svg.into_owned()).ok())
            .map(|svg| svg.replacen("<svg", "<svg aria-hidden=\"true\"", 1))
            .unwrap_or_default();
        format!(
            "<div class=\"alert alert-{}\">\n<p class=\"alert-title\">{icon}{name}</p>\n{}</div>\n",
            name.to_ascii_lowercase(),
            self.fragment(body)
        )
    }

    fn formula(&mut self, tex: &str, style: MathStyle) -> String {
        let (scale, element, class) = match style {
            MathStyle::Inline => (INLINE_MATH_SCALE, "span", "math math-inline"),
            MathStyle::Display => (DISPLAY_MATH_SCALE, "div", "math math-display"),
        };
        let size = MATH_BASE_SIZE * scale;
        match math::render(tex, style, size, &format!("{MATH_SENTINEL_COLOR}ff")) {
            Ok(rendered) => {
                self.formulas += 1;
                let svg = embeddable_svg(&rendered, self.formulas, style);
                format!(
                    "<{element} class=\"{class}\" role=\"img\" aria-label=\"{}\">{svg}</{element}>",
                    escape(tex.trim())
                )
            }
            Err(error) => match style {
                MathStyle::Inline => format!(
                    "<code class=\"math-error\" title=\"{}\">{}</code>",
                    escape(&error),
                    escape(tex.trim())
                ),
                MathStyle::Display => format!(
                    "<pre class=\"math-error\" title=\"{}\"><code>{}</code></pre>",
                    escape(&error),
                    escape(tex.trim())
                ),
            },
        }
    }
}

/// Place a block's token on a line of its own, so the compiler neither joins
/// it to a neighboring paragraph nor mistakes it for part of one.
fn push_block_token(out: &mut String, source: &str, range: &Range<usize>, token: &str) {
    let line_start = source[..range.start].rfind('\n').map_or(0, |ix| ix + 1);
    let indent = &source[line_start..range.start];
    let starts_line = indent.chars().all(char::is_whitespace);
    let previous_line_blank = source[..line_start]
        .trim_end_matches('\n')
        .rsplit('\n')
        .next()
        .is_none_or(|line| line.trim().is_empty());
    let next_line_blank = source[range.end..]
        .strip_prefix('\n')
        .map(|rest| rest.split('\n').next().unwrap_or_default())
        .is_none_or(|line| line.trim().is_empty());

    if starts_line && !previous_line_blank && line_start > 0 {
        out.push('\n');
        out.push_str(indent);
    }
    out.push_str(token);
    if starts_line && !next_line_blank {
        out.push('\n');
    }
}

/// Make a rendered formula embeddable in HTML: sized in `em` so it scales
/// with the page, aligned on the text baseline, colored by `currentColor`,
/// and with ids prefixed so several formulas can share a page.
fn embeddable_svg(rendered: &math::MathSvg, number: usize, style: MathStyle) -> String {
    let em = |value: f32| format!("{:.4}em", value / MATH_BASE_SIZE);
    let svg = &rendered.svg;
    let view_box = svg
        .split_once("viewBox=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(view_box, _)| view_box.to_string())
        .unwrap_or_else(|| format!("0 0 {} {}", rendered.width, rendered.height));
    let content = svg
        .split_once('>')
        .map(|(_, content)| content)
        .unwrap_or_default()
        .replace(MATH_SENTINEL_COLOR, "currentColor")
        .replace("id=\"", &format!("id=\"m{number}-"))
        .replace("href=\"#", &format!("href=\"#m{number}-"))
        .replace("url(#", &format!("url(#m{number}-"));
    let vertical_align = match style {
        MathStyle::Inline => format!(
            ";vertical-align:-{}",
            em((rendered.height - rendered.baseline).max(0.))
        ),
        MathStyle::Display => String::new(),
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         viewBox=\"{view_box}\" style=\"width:{};height:{}{vertical_align}\" aria-hidden=\"true\" focusable=\"false\">{content}",
        em(rendered.width),
        em(rendered.height)
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_gfm_and_escapes_html() {
        let html = to_html(
            "# Hi <there>\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<script>x</script>\n",
            "A & B",
        );
        assert!(html.contains("<title>A &amp; B</title>"));
        assert!(html.contains("<h1>Hi &lt;there&gt;</h1>"));
        assert!(html.contains("<table>"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn renders_alerts_and_math() {
        let html = to_html(
            "Intro\n> [!WARNING]\n> Mind $x^2$ here.\n\nInline $a+b$ and $5 or $10.\n\n$$\n\\frac{1}{2}\n$$\n\n- item\n  > [!NOTE]\n  > nested\n",
            "t",
        );
        assert!(html.contains("<p>Intro</p>"));
        assert!(html.contains("<div class=\"alert alert-warning\">"));
        assert!(html.contains("<div class=\"alert alert-note\">"));
        assert!(html.contains("Warning</p>"));
        // Math inside the alert is typeset too.
        assert_eq!(html.matches("class=\"math math-inline\"").count(), 2);
        assert_eq!(html.matches("class=\"math math-display\"").count(), 1);
        assert!(html.contains("currentColor"));
        assert!(!html.contains(MATH_SENTINEL_COLOR));
        assert!(html.contains("$5 or $"));
        assert!(!html.contains("MALGEL"));
        assert!(!html.contains("[!WARNING]"));
        // Each formula's SVG ids are its own.
        assert!(html.contains("id=\"m1-") && html.contains("id=\"m3-"));
    }

    #[test]
    fn shows_unreadable_formulas_as_source() {
        let html = to_html("$$\\frac{1}{$$\n", "t");
        assert!(html.contains("class=\"math-error\""));
        assert!(html.contains("\\frac{1}{"));
    }

    #[test]
    fn leaves_code_alone() {
        let html = to_html("`$x$`\n\n```\n> [!NOTE]\n$$y$$\n```\n", "t");
        assert!(html.contains("<code>$x$</code>"));
        assert!(html.contains("[!NOTE]"));
        assert!(!html.contains("class=\"math"));
    }

    #[test]
    fn skips_frontmatter() {
        let html = to_html("---\ntitle: x\n---\n\nBody\n", "t");
        assert!(!html.contains("title: x"));
        assert!(html.contains("<p>Body</p>"));
    }
}
