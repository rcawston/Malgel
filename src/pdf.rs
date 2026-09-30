//! Export a document as PDF by translating Markdown to Typst.
//!
//! The Markdown tree is written out as Typst markup with a small preamble of
//! styles, then compiled by the Typst compiler and exported with `typst-pdf`.
//! Headings become the PDF outline, math is native Typst math, and local
//! images are embedded. The compiler runs sealed: it sees only this document,
//! its images and fonts.

use std::{collections::HashMap, path::Path};

use gpui_kit::{AssetSource as _, assets::AllAssets};
use markdown::mdast::{AlignKind, Node};
use typst::foundations::Bytes;

use crate::{
    analysis::preview_parse_options,
    diagram::{self, DiagramTheme},
    images,
    preview_ext::{AlertKind, display_math_source, is_inline_math, parse_alert},
    typst_world::{self, VirtualFiles, escape_markup, string_literal},
};

/// Paper size for paged exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paper {
    A4,
    Letter,
}

impl Paper {
    /// US Letter where it is the norm, A4 everywhere else.
    pub fn from_locale() -> Self {
        let locale = sys_locale::get_locale().unwrap_or_default();
        Self::for_locale(&locale)
    }

    fn for_locale(locale: &str) -> Self {
        let region = locale
            .split(['-', '_', '.'])
            .nth(1)
            .unwrap_or_default()
            .to_ascii_uppercase();
        match region.as_str() {
            "US" | "CA" | "MX" | "PH" | "CL" | "CO" | "VE" | "GT" | "CR" | "PR" | "DO" | "SV"
            | "NI" | "PA" | "BZ" => Paper::Letter,
            _ => Paper::A4,
        }
    }

    fn typst_name(self) -> &'static str {
        match self {
            Paper::A4 => "a4",
            Paper::Letter => "us-letter",
        }
    }
}

/// Render `source` as a PDF. Relative image paths resolve against
/// `base_dir`.
pub fn to_pdf(
    source: &str,
    title: &str,
    base_dir: Option<&Path>,
    paper: Paper,
) -> Result<Vec<u8>, String> {
    let (markup, files) = to_typst(source, title, base_dir, paper);
    let document = typst_world::compile(markup, files, &typst_world::DOCUMENT_FONTS)
        .map_err(|error| format!("Couldn’t lay out the document: {error}."))?;
    typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default()).map_err(|errors| {
        let message = errors
            .first()
            .map(|error| error.message.to_string())
            .unwrap_or_default();
        format!("Couldn’t write the PDF: {message}.")
    })
}

/// The Typst source for `source` and the files it references.
pub fn to_typst(
    source: &str,
    title: &str,
    base_dir: Option<&Path>,
    paper: Paper,
) -> (String, VirtualFiles) {
    let mut writer = Writer {
        base_dir,
        files: VirtualFiles::new(),
        definitions: HashMap::new(),
        footnotes: HashMap::new(),
        images: 0,
        list_depth: 0,
    };
    let body = writer.document(source);
    let preamble = PREAMBLE
        .replace("{title}", &string_literal(title))
        .replace("{paper}", paper.typst_name());
    (format!("{preamble}\n{body}\n"), writer.files)
}

const PREAMBLE: &str = r##"#set document(title: {title})
#set page(paper: "{paper}", margin: (x: 2.3cm, top: 2.4cm, bottom: 2.6cm), numbering: "1")
#set text(font: "Libertinus Serif", size: 11pt, hyphenate: true)
#set par(justify: true, leading: 0.62em, spacing: 1.15em)
#show heading: set text(weight: "bold")
#show heading: set block(above: 1.6em, below: 0.9em)
#show heading.where(level: 1): set text(size: 1.8em)
#show heading.where(level: 2): set text(size: 1.4em)
#show heading.where(level: 3): set text(size: 1.2em)
#show heading.where(level: 4): set text(size: 1.05em)
#show link: set text(fill: rgb("#0b5cad"))
#show raw: set text(font: "DejaVu Sans Mono", size: 0.84em)
#show raw.where(block: true): it => block(width: 100%, fill: luma(246), stroke: 0.5pt + luma(222), inset: 9pt, radius: 4pt, it)
#show raw.where(block: false): box.with(fill: luma(241), inset: (x: 2.5pt), outset: (y: 2.5pt), radius: 2pt)
#show quote.where(block: true): it => block(inset: (left: 11pt, y: 2pt), stroke: (left: 2.5pt + luma(205)), text(fill: luma(80), it.body))
#set table(stroke: 0.5pt + luma(205), inset: (x: 7pt, y: 5pt), fill: (_, y) => if y == 0 { luma(244) })
#show table.cell.where(y: 0): set text(weight: "bold")
#set list(indent: 0.3em, body-indent: 0.6em)
#set enum(indent: 0.3em, body-indent: 0.6em)
#set image(fit: "contain")
#let malgel-rule() = block(above: 1.4em, below: 1.4em, line(length: 100%, stroke: 0.5pt + luma(200)))
#let malgel-task(done) = { box(baseline: 0.12em, width: 0.8em, height: 0.8em, stroke: 0.6pt + luma(100), radius: 1.5pt, if done { align(center + horizon, text(size: 0.7em, font: "DejaVu Sans Mono", "✓")) }); h(0.45em) }
#let malgel-alert(color, icon, title, body) = block(width: 100%, breakable: true, fill: color.lighten(94%), stroke: 0.6pt + color.lighten(50%), radius: 4pt, inset: (x: 11pt, y: 10pt))[
  #set par(spacing: 0.75em)
  #text(fill: color, weight: "bold")[#if icon != none { box(baseline: 0.14em, image(icon, height: 0.95em)); h(0.35em) }#title]

  #body
]
#let malgel-image(path, alt) = context {
  let natural = measure(image(path))
  if natural.width > page.width - page.margin.left - page.margin.right {
    image(path, width: 100%, alt: alt)
  } else {
    image(path, alt: alt)
  }
}
#let malgel-diagram(path, width) = context {
  let available = page.width - page.margin.left - page.margin.right
  align(center, image(path, width: calc.min(width, available), alt: "Diagram"))
}
#let malgel-meta(pairs) = block(width: 100%, below: 1.4em, table(columns: (auto, 1fr), stroke: none, fill: none, inset: (x: 0pt, y: 2pt), column-gutter: 1.2em, ..pairs.map(((k, v)) => (text(fill: luma(100), k), v)).flatten()))
"##;

/// A link or image reference definition: `[id]: url "title"`.
#[derive(Clone)]
struct Definition {
    url: String,
}

struct Writer<'a> {
    base_dir: Option<&'a Path>,
    files: VirtualFiles,
    definitions: HashMap<String, Definition>,
    /// Footnote bodies, already written as Typst, by identifier.
    footnotes: HashMap<String, String>,
    images: usize,
    /// How many lists enclose the block being written.
    list_depth: usize,
}

impl Writer<'_> {
    fn document(&mut self, source: &str) -> String {
        let Ok(root) = markdown::to_mdast(source, &preview_parse_options()) else {
            return escape_markup(source);
        };
        self.collect_definitions(&root, source);
        self.blocks(
            root.children().map(Vec::as_slice).unwrap_or_default(),
            source,
        )
    }

    /// Link definitions and footnotes may appear anywhere, including after
    /// their first use, so gather them first.
    fn collect_definitions(&mut self, node: &Node, source: &str) {
        match node {
            Node::Definition(definition) => {
                self.definitions.insert(
                    definition.identifier.to_lowercase(),
                    Definition {
                        url: definition.url.clone(),
                    },
                );
            }
            Node::FootnoteDefinition(footnote) => {
                let body = self.blocks(&footnote.children, source);
                self.footnotes
                    .insert(footnote.identifier.to_lowercase(), body);
            }
            _ => {
                for child in node.children().into_iter().flatten() {
                    self.collect_definitions(child, source);
                }
            }
        }
    }

    fn blocks(&mut self, nodes: &[Node], source: &str) -> String {
        nodes
            .iter()
            .filter_map(|node| self.block(node, source))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn block(&mut self, node: &Node, source: &str) -> Option<String> {
        let text = node_source(node, source);
        Some(match node {
            Node::Heading(heading) => format!(
                "#heading(level: {})[{}]",
                heading.depth.clamp(1, 6),
                self.inlines(&heading.children, source)
            ),
            Node::Paragraph(paragraph) => {
                if let Some(tex) = display_math_source(text) {
                    return Some(display_math(tex));
                }
                if let [Node::Image(image)] = paragraph.children.as_slice() {
                    return Some(self.block_image(&image.url, &image.alt));
                }
                self.inlines(&paragraph.children, source)
            }
            Node::Blockquote(quote) => match parse_alert(text) {
                Some((kind, body)) => {
                    let body = self.document_fragment(&body);
                    let icon = self.alert_icon(kind);
                    format!(
                        "#malgel-alert(rgb(\"{}\"), {icon}, {})[{body}]",
                        alert_color(kind),
                        string_literal(kind.title())
                    )
                }
                None => format!(
                    "#quote(block: true)[{}]",
                    self.blocks(&quote.children, source)
                ),
            },
            Node::List(list) => {
                self.list_depth += 1;
                let items = list
                    .children
                    .iter()
                    .map(|item| {
                        let Node::ListItem(item) = item else {
                            return String::new();
                        };
                        // In a tight list an item's blocks (text, then a
                        // nested list) stay together without a paragraph gap.
                        let body = if list.spread {
                            self.blocks(&item.children, source)
                        } else {
                            item.children
                                .iter()
                                .filter_map(|child| self.block(child, source))
                                .collect::<Vec<_>>()
                                .join("\n")
                        };
                        match item.checked {
                            Some(done) => format!("[#malgel-task({done});{body}]"),
                            None => format!("[{body}]"),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.list_depth -= 1;
                let tight = !list.spread;
                // Like GitHub, a list of tasks shows checkboxes, not bullets.
                let all_tasks = list
                    .children
                    .iter()
                    .all(|item| matches!(item, Node::ListItem(item) if item.checked.is_some()));
                if all_tasks && !list.ordered {
                    // Without bullets, nesting shows only through indentation.
                    let indent = if self.list_depth > 0 {
                        "1.25em"
                    } else {
                        "0.3em"
                    };
                    format!(
                        "#list(tight: {tight}, marker: [], indent: {indent}, body-indent: 0pt, {items})"
                    )
                } else if list.ordered {
                    format!(
                        "#enum(tight: {tight}, start: {}, {items})",
                        list.start.unwrap_or(1)
                    )
                } else {
                    format!("#list(tight: {tight}, {items})")
                }
            }
            Node::Code(code) if diagram::is_mermaid(code.lang.as_deref()) => {
                self.diagram(&code.value)
            }
            Node::Code(code) => match code.lang.as_deref() {
                Some(lang) if !lang.is_empty() => format!(
                    "#raw(block: true, lang: {}, {})",
                    string_literal(lang),
                    string_literal(&code.value)
                ),
                _ => format!("#raw(block: true, {})", string_literal(&code.value)),
            },
            Node::Math(math) => display_math(&math.value),
            Node::Table(table) => {
                let columns = table.align.len().max(1);
                let align = table
                    .align
                    .iter()
                    .map(|align| match align {
                        AlignKind::Center => "center",
                        AlignKind::Right => "right",
                        AlignKind::Left | AlignKind::None => "left",
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut rows = Vec::new();
                for (ix, row) in table.children.iter().enumerate() {
                    let Node::TableRow(row) = row else {
                        continue;
                    };
                    let mut cells: Vec<String> = row
                        .children
                        .iter()
                        .map(|cell| match cell {
                            Node::TableCell(cell) => {
                                format!("[{}]", self.inlines(&cell.children, source))
                            }
                            _ => "[]".to_string(),
                        })
                        .collect();
                    cells.resize(columns, "[]".to_string());
                    let cells = cells.join(", ");
                    rows.push(if ix == 0 {
                        format!("table.header({cells})")
                    } else {
                        cells
                    });
                }
                format!(
                    "#table(columns: {columns}, align: ({align},), {})",
                    rows.join(", ")
                )
            }
            Node::ThematicBreak(_) => "#malgel-rule()".to_string(),
            Node::Yaml(yaml) => {
                let pairs = front_matter(&yaml.value);
                if pairs.is_empty() {
                    return None;
                }
                let pairs = pairs
                    .iter()
                    .map(|(key, value)| {
                        format!("({}, {})", string_literal(key), string_literal(value))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("#malgel-meta(({pairs},))")
            }
            Node::Html(html) => {
                let value = html.value.trim();
                if value.starts_with("<!--") {
                    return None;
                }
                format!("#raw({})", string_literal(value))
            }
            Node::Definition(_) | Node::FootnoteDefinition(_) => return None,
            other => self.blocks(other.children()?, source),
        })
    }

    /// Write a nested Markdown document (an alert's body) in this document's
    /// context, sharing its images and definitions.
    fn document_fragment(&mut self, markdown: &str) -> String {
        let Ok(root) = markdown::to_mdast(markdown, &preview_parse_options()) else {
            return escape_markup(markdown);
        };
        self.collect_definitions(&root, markdown);
        self.blocks(
            root.children().map(Vec::as_slice).unwrap_or_default(),
            markdown,
        )
    }

    fn inlines(&mut self, nodes: &[Node], source: &str) -> String {
        nodes.iter().map(|node| self.inline(node, source)).collect()
    }

    fn inline(&mut self, node: &Node, source: &str) -> String {
        let mut markup = self.inline_markup(node, source);
        // Following text such as `.x` or `(y)` would otherwise continue an
        // embedded expression as a field access or call; `;` ends it.
        if markup.starts_with('#') {
            markup.push(';');
        }
        markup
    }

    fn inline_markup(&mut self, node: &Node, source: &str) -> String {
        match node {
            Node::Text(text) => escape_markup(&text.value),
            Node::Emphasis(emphasis) => {
                format!("#emph[{}]", self.inlines(&emphasis.children, source))
            }
            Node::Strong(strong) => format!("#strong[{}]", self.inlines(&strong.children, source)),
            Node::Delete(delete) => format!("#strike[{}]", self.inlines(&delete.children, source)),
            Node::InlineCode(code) => format!("#raw({})", string_literal(&code.value)),
            Node::Break(_) => "#linebreak()".to_string(),
            Node::Link(link) => self.link(&link.url, &link.children, source),
            Node::LinkReference(reference) => {
                match self.definitions.get(&reference.identifier.to_lowercase()) {
                    Some(definition) => {
                        let url = definition.url.clone();
                        self.link(&url, &reference.children, source)
                    }
                    None => escape_markup(node_source(node, source)),
                }
            }
            Node::Image(image) => self.inline_image(&image.url, &image.alt),
            Node::ImageReference(reference) => {
                match self.definitions.get(&reference.identifier.to_lowercase()) {
                    Some(definition) => {
                        let url = definition.url.clone();
                        self.inline_image(&url, &reference.alt)
                    }
                    None => escape_markup(&reference.alt),
                }
            }
            Node::InlineMath(math) => {
                let text = node_source(node, source);
                let end = node.position().map_or(0, |position| position.end.offset);
                let following = source.get(end..).and_then(|rest| rest.chars().next());
                if is_inline_math(&math.value, following) {
                    inline_math(&math.value)
                } else {
                    escape_markup(text)
                }
            }
            Node::FootnoteReference(reference) => {
                match self.footnotes.get(&reference.identifier.to_lowercase()) {
                    Some(body) => format!("#footnote[{body}]"),
                    None => escape_markup(node_source(node, source)),
                }
            }
            Node::Html(html) => {
                let tag = html.value.trim().to_ascii_lowercase();
                if tag.starts_with("<br") {
                    "#linebreak()".to_string()
                } else {
                    // Other inline tags (`<kbd>`, `<sub>`, …) are dropped;
                    // the text between them is kept.
                    String::new()
                }
            }
            other => match other.children() {
                Some(children) => self.inlines(children, source),
                None => String::new(),
            },
        }
    }

    fn link(&mut self, url: &str, children: &[Node], source: &str) -> String {
        let label = self.inlines(children, source);
        // Fragment links point into the rendered page, which a PDF lacks.
        if url.is_empty() || url.starts_with('#') {
            return label;
        }
        let label = if label.is_empty() {
            escape_markup(url)
        } else {
            label
        };
        format!("#link({})[{label}]", string_literal(url))
    }

    /// A Mermaid diagram as vector art, or its source with the reason when
    /// it can't be drawn.
    fn diagram(&mut self, source: &str) -> String {
        self.images += 1;
        let id = format!("mermaid-{}", self.images);
        match diagram::render(source, &DiagramTheme::document(), &id) {
            Ok(diagram) => {
                let name = format!("diagram-{}.svg", self.images);
                self.files
                    .insert(name.clone(), Bytes::new(diagram.svg.into_bytes()));
                // CSS pixels, as the diagram was laid out in, to points.
                format!(
                    "#malgel-diagram({}, {:.1}pt)",
                    string_literal(&format!("/{name}")),
                    diagram.width * 0.75
                )
            }
            Err(error) => format!(
                "#raw(block: true, {})\n#text(size: 0.85em, fill: rgb(\"#d1242f\"), {})",
                string_literal(source.trim_end()),
                string_literal(&error)
            ),
        }
    }

    /// Embed the image at `url`, returning its virtual path.
    fn embed(&mut self, url: &str) -> Option<String> {
        let image = images::load(url, self.base_dir)?;
        self.images += 1;
        let name = format!("image-{}.{}", self.images, image.extension);
        self.files.insert(name.clone(), Bytes::new(image.bytes));
        Some(format!("/{name}"))
    }

    fn block_image(&mut self, url: &str, alt: &str) -> String {
        match self.embed(url) {
            Some(path) => format!(
                "#align(center, malgel-image({}, {}))",
                string_literal(&path),
                string_literal(alt)
            ),
            None => missing_image(alt),
        }
    }

    fn inline_image(&mut self, url: &str, alt: &str) -> String {
        match self.embed(url) {
            // Inline images are usually badges and icons: size them to the
            // line so they don't break its rhythm.
            Some(path) => format!(
                "#box(baseline: 0.2em, image({}, height: 1.25em, alt: {}))",
                string_literal(&path),
                string_literal(alt)
            ),
            None => missing_image(alt),
        }
    }

    fn alert_icon(&mut self, kind: AlertKind) -> String {
        let name = format!("alert-{}.svg", kind.title().to_ascii_lowercase());
        if !self.files.contains_key(&name) {
            let Some(svg) = AllAssets
                .load(&kind.icon().path())
                .ok()
                .flatten()
                .and_then(|svg| String::from_utf8(svg.into_owned()).ok())
            else {
                return "none".to_string();
            };
            let svg = svg.replace("currentColor", alert_color(kind));
            self.files
                .insert(name.clone(), Bytes::new(svg.into_bytes()));
        }
        string_literal(&format!("/{name}"))
    }
}

fn node_source<'a>(node: &Node, source: &'a str) -> &'a str {
    node.position()
        .and_then(|position| source.get(position.start.offset..position.end.offset))
        .unwrap_or_default()
}

fn alert_color(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Note => "#0969da",
        AlertKind::Tip => "#1a7f37",
        AlertKind::Important => "#8250df",
        AlertKind::Warning => "#9a6700",
        AlertKind::Caution => "#d1242f",
    }
}

fn missing_image(alt: &str) -> String {
    if alt.is_empty() {
        String::new()
    } else {
        format!("#emph[{}]", escape_markup(alt))
    }
}

/// Typst math for a LaTeX formula, or its source in red if it can't be read.
fn math(tex: &str, display: bool) -> String {
    match tex2typst_rs::tex2typst(tex.trim()) {
        Ok(math) if display => format!("$ {math} $"),
        Ok(math) => format!("${math}$"),
        Err(_) => format!(
            "#text(fill: rgb(\"#d1242f\"), raw({}))",
            string_literal(tex.trim())
        ),
    }
}

fn display_math(tex: &str) -> String {
    math(tex, true)
}

fn inline_math(tex: &str) -> String {
    math(tex, false)
}

/// Top-level `key: value` pairs of YAML front matter, as shown in the
/// preview. Nested values are skipped.
fn front_matter(yaml: &str) -> Vec<(String, String)> {
    yaml.lines()
        .filter(|line| !line.starts_with([' ', '\t', '-', '#']))
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim().trim_matches(['"', '\'']);
            (!key.trim().is_empty() && !value.is_empty())
                .then(|| (key.trim().to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
title: Sample
author: Sam
---

# Heading *one* & more

Text with **bold**, _italic_, ~~gone~~, `code`, a [link](https://example.com), a
[ref link][docs], a footnote[^1], $x^2$ math and $5 or $10 prices.
Special characters: #hash $ @at <tag> [br] * _ // not a comment ~tilde 'q' "q" `raw`.field and **b**(call).

> [!NOTE]
> Alert with $a+b$ inside.

> Plain quote.

1. first
2. second
   - nested
   - [x] done
   - [ ] todo

| Left | Center | Right |
| :--- | :----: | ----: |
| a | b | c |
| d | e |

```rust
fn main() { println!("hi \"there\""); }
```

$$
\sum_{i=1}^n i = \frac{n(n+1)}{2}
$$

$$\frac{1}{$$

---

![missing](nope.png)

[docs]: https://docs.example.com
[^1]: The footnote text.
"#;

    #[test]
    fn exports_a_full_document() {
        let pdf = to_pdf(SAMPLE, "Sample", None, Paper::A4).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pdf.len() > 5_000);
    }

    #[test]
    fn writes_typst_for_each_construct() {
        let (markup, files) = to_typst(SAMPLE, "Sample", None, Paper::Letter);
        assert!(markup.contains("paper: \"us-letter\""));
        assert!(markup.contains("#heading(level: 1)"));
        assert!(markup.contains("#malgel-meta"));
        assert!(markup.contains("#malgel-alert(rgb(\"#0969da\")"));
        assert!(markup.contains("#footnote[The footnote text.]"));
        assert!(markup.contains("#link(\"https://docs.example.com\")"));
        assert!(markup.contains("#malgel-task(true)"));
        assert!(markup.contains("table.header("));
        assert!(markup.contains("\\$5 or \\$"));
        assert!(markup.contains("#emph[missing]"));
        // The alert icon is the only embedded file.
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn draws_mermaid_diagrams() {
        let source = "```mermaid\nflowchart LR\n  A[Start] --> B[End]\n```\n\n```mermaid\nflowchart LR\n  A -->\n```\n";
        let (markup, files) = to_typst(source, "t", None, Paper::A4);
        assert_eq!(markup.matches("#malgel-diagram(").count(), 1);
        assert!(files.keys().any(|name| name.starts_with("diagram-")));
        // The broken one shows its source.
        assert!(markup.contains("A -->"));
        let pdf = to_pdf(source, "t", None, Paper::A4);
        assert!(pdf.is_ok(), "{pdf:?}");
    }

    #[test]
    fn embeds_local_images() {
        let dir = std::env::temp_dir().join(format!("malgel-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("dot.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><circle cx="4" cy="4" r="4"/></svg>"#,
        )
        .unwrap();
        let pdf = to_pdf(
            "![dot](dot.svg) and inline ![dot](dot.svg)",
            "t",
            Some(&dir),
            Paper::A4,
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(pdf.is_ok(), "{pdf:?}");
    }

    #[test]
    fn picks_paper_from_locale() {
        assert_eq!(Paper::for_locale("en-US"), Paper::Letter);
        assert_eq!(Paper::for_locale("en_CA.UTF-8"), Paper::Letter);
        assert_eq!(Paper::for_locale("de-DE"), Paper::A4);
        assert_eq!(Paper::for_locale("en"), Paper::A4);
    }
}
