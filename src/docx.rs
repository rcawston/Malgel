//! Export a document as a Word (.docx) file.
//!
//! Headings use Word's built-in heading styles, so the navigation pane and
//! tables of contents work; lists use real Word numbering. Alerts and code
//! blocks become shaded single-cell tables. Word cannot display LaTeX, so
//! formulas are typeset with the same renderer as the preview and embedded
//! as high-resolution images.

use std::{
    collections::HashMap,
    io::{Cursor, Read as _, Write as _},
    path::Path,
};

use docx_rs::{
    AbstractNumbering, AlignmentType, BorderType, BreakType, Docx, Footnote, HeightRule, Hyperlink,
    HyperlinkType, IndentLevel, Level, LevelJc, LevelOverride, LevelText, LineSpacing,
    LineSpacingType, NumberFormat, Numbering, NumberingId, PageMargin, Paragraph, ParagraphBorder,
    ParagraphBorderPosition, ParagraphBorders, Pic, Run, RunFonts, Shading, SpecialIndentType,
    Start, Style, StyleType, Table, TableBorder, TableBorderPosition, TableBorders, TableCell,
    TableCellBorder, TableCellBorderPosition, TableCellBorders, TableCellMargins, TableRow,
    WidthType,
};
use markdown::mdast::{AlignKind, Node};

use crate::{
    analysis::preview_parse_options,
    images, math,
    pdf::Paper,
    preview_ext::{AlertKind, display_math_source, is_inline_math, parse_alert},
    typst_world,
};

/// Body text size in points.
const BODY_SIZE: f32 = 11.;
const MONO_FONT: &str = "Consolas";
const TEXT_COLOR: &str = "1F2328";
const MUTED_COLOR: &str = "59636E";
const LINK_COLOR: &str = "0B5CAD";
const BORDER_COLOR: &str = "D0D7DE";
const SUBTLE_FILL: &str = "F6F8FA";
/// English Metric Units per point, the unit Word sizes pictures in.
const EMU_PER_PT: f32 = 12_700.;
/// Pixels rendered per point for formulas and vector images.
const RASTER_DENSITY: f32 = 4.;
/// Abstract numbering definitions: every bulleted list uses the first,
/// numbered lists instantiate the second so each restarts its count. docx-rs
/// writes a definition and instance of its own under id 1, so ids start
/// above it.
const BULLETS: usize = 10;
const DECIMAL: usize = 11;
/// The numbering instance every bulleted list shares; numbered lists take
/// the ids after `DECIMAL`.
const BULLET_NUMBERING: usize = 10;

/// Render `source` as a Word document. Relative image paths resolve against
/// `base_dir`.
pub fn to_docx(
    source: &str,
    _title: &str,
    base_dir: Option<&Path>,
    paper: Paper,
) -> Result<Vec<u8>, String> {
    let (page_width, page_height) = match paper {
        Paper::A4 => (11_906, 16_838),
        Paper::Letter => (12_240, 15_840),
    };
    let margin = 1_300;
    let mut writer = Writer {
        base_dir,
        definitions: HashMap::new(),
        footnotes: HashMap::new(),
        numberings: Vec::new(),
        text_width: (page_width - 2 * margin) as f32 / 20.,
    };
    let blocks = writer.document(source);

    let mut docx = Docx::new()
        .page_size(page_width, page_height)
        .page_margin(
            PageMargin::new()
                .top(margin as i32)
                .bottom(margin as i32)
                .left(margin as i32)
                .right(margin as i32),
        )
        .default_fonts(
            RunFonts::new()
                .ascii("Calibri")
                .hi_ansi("Calibri")
                .cs("Calibri"),
        )
        .default_size(half_points(BODY_SIZE))
        .default_line_spacing(line_spacing(264).after(140))
        .add_abstract_numbering(bullets())
        .add_abstract_numbering(decimals());
    for style in heading_styles() {
        docx = docx.add_style(style);
    }
    for numbering in writer.numberings {
        docx = docx.add_numbering(numbering);
    }
    for block in spaced(blocks) {
        docx = match block {
            Block::Paragraph(paragraph) => docx.add_paragraph(paragraph),
            Block::Table(table) => docx.add_table(*table),
        };
    }

    let mut out = Cursor::new(Vec::new());
    docx.build()
        .pack(&mut out)
        .map_err(|error| format!("Couldn’t write the document: {error}."))?;
    finish(out.into_inner()).map_err(|error| format!("Couldn’t write the document: {error}."))
}

/// Placeholder character style marking a formula image to be lowered by
/// the half-points that follow its name.
const DROP_STYLE: &str = "MalgelDrop";
/// Header rows carry this row height (a 1/20 pt minimum, which changes
/// nothing) in place of the `tblHeader` docx-rs can't write.
const HEADER_ROW_MARKER: &str = "<w:cantSplit /><w:trHeight w:val=\"1\"";
/// Placeholder character style marking where a footnote shows its number.
const FOOTNOTE_NUMBER_STYLE: &str = "MalgelFootnoteNumber";

/// Write what docx-rs can't express into the packed document:
///
/// - Word puts the bottom of an inline picture on the baseline, which leaves
///   formulas with descenders (`y`, fractions) floating; runs carrying a
///   `DROP_STYLE` placeholder get a `<w:position>` instead.
/// - A footnote shows its number only where its text holds a
///   `<w:footnoteRef>`, which replaces the `FOOTNOTE_NUMBER_STYLE` run.
/// - Table header rows, marked by `HEADER_ROW_MARKER`, repeat on every page.
fn finish(docx: Vec<u8>) -> zip::result::ZipResult<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(docx))?;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for ix in 0..archive.len() {
        let mut file = archive.by_index(ix)?;
        let name = file.name().to_string();
        if !matches!(name.as_str(), "word/document.xml" | "word/footnotes.xml") {
            writer.raw_copy_file(file)?;
            continue;
        }
        let mut xml = String::new();
        file.read_to_string(&mut xml)?;
        let xml = lower_formulas(&xml)
            .replace(
                &format!("<w:rStyle w:val=\"{FOOTNOTE_NUMBER_STYLE}\" /></w:rPr>"),
                "<w:vertAlign w:val=\"superscript\" /></w:rPr><w:footnoteRef />",
            )
            .replace(
                HEADER_ROW_MARKER,
                &HEADER_ROW_MARKER.replace("<w:trHeight", "<w:tblHeader /><w:trHeight"),
            );
        writer.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )?;
        writer.write_all(xml.as_bytes())?;
    }
    Ok(writer.finish()?.into_inner())
}

fn lower_formulas(xml: &str) -> String {
    let marker = format!("<w:rStyle w:val=\"{DROP_STYLE}");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find(&marker) {
        out.push_str(&rest[..start]);
        let after = &rest[start + marker.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let Some(end) = after.find("/>") else {
            break;
        };
        out.push_str(&format!("<w:position w:val=\"-{}\" />", &after[..digits]));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

// Blocks and pieces are moved straight into the document once built, so
// their size doesn't matter.
#[allow(clippy::large_enum_variant)]
enum Block {
    Paragraph(Paragraph),
    Table(Box<Table>),
}

impl Block {
    fn table(table: Table) -> Self {
        Block::Table(Box::new(table))
    }
}

/// Pieces of inline content: plain runs, or runs inside a hyperlink.
#[allow(clippy::large_enum_variant)]
enum Piece {
    Run(Run),
    Link(Hyperlink),
}

#[derive(Clone, Copy, Default)]
struct Marks {
    bold: bool,
    italic: bool,
    strike: bool,
    link: bool,
}

struct Writer<'a> {
    base_dir: Option<&'a Path>,
    definitions: HashMap<String, String>,
    footnotes: HashMap<String, Vec<Paragraph>>,
    numberings: Vec<Numbering>,
    /// Width available to content, in points.
    text_width: f32,
}

impl Writer<'_> {
    fn document(&mut self, source: &str) -> Vec<Block> {
        let Ok(root) = markdown::to_mdast(source, &preview_parse_options()) else {
            return vec![Block::Paragraph(
                Paragraph::new().add_run(Run::new().add_text(source)),
            )];
        };
        self.collect_definitions(&root, source);
        self.blocks(
            root.children().map(Vec::as_slice).unwrap_or_default(),
            source,
            0,
        )
    }

    fn collect_definitions(&mut self, node: &Node, source: &str) {
        match node {
            Node::Definition(definition) => {
                self.definitions
                    .insert(definition.identifier.to_lowercase(), definition.url.clone());
            }
            Node::FootnoteDefinition(footnote) => {
                let paragraphs = footnote
                    .children
                    .iter()
                    .filter_map(|child| match child {
                        Node::Paragraph(paragraph) => Some(paragraph),
                        _ => None,
                    })
                    .enumerate()
                    .map(|(ix, paragraph)| {
                        let mut start = Paragraph::new();
                        if ix == 0 {
                            start = start
                                .add_run(Run::new().style(FOOTNOTE_NUMBER_STYLE))
                                .add_run(Run::new().add_text(" "));
                        }
                        self.paragraph(start, &paragraph.children, source, Marks::default())
                    })
                    .collect();
                self.footnotes
                    .insert(footnote.identifier.to_lowercase(), paragraphs);
            }
            _ => {
                for child in node.children().into_iter().flatten() {
                    self.collect_definitions(child, source);
                }
            }
        }
    }

    fn blocks(&mut self, nodes: &[Node], source: &str, depth: usize) -> Vec<Block> {
        nodes
            .iter()
            .flat_map(|node| self.block(node, source, depth))
            .collect()
    }

    fn block(&mut self, node: &Node, source: &str, depth: usize) -> Vec<Block> {
        let text = node_source(node, source);
        match node {
            Node::Heading(heading) => {
                let level = heading.depth.clamp(1, 6);
                vec![Block::Paragraph(self.paragraph(
                    Paragraph::new().style(&format!("Heading{level}")),
                    &heading.children,
                    source,
                    Marks::default(),
                ))]
            }
            Node::Paragraph(paragraph) => {
                if let Some(tex) = display_math_source(text) {
                    return vec![self.display_math(tex)];
                }
                if let [Node::Image(image)] = paragraph.children.as_slice() {
                    return vec![self.block_image(&image.url, &image.alt)];
                }
                vec![Block::Paragraph(self.paragraph(
                    Paragraph::new(),
                    &paragraph.children,
                    source,
                    Marks::default(),
                ))]
            }
            Node::Blockquote(quote) => match parse_alert(text) {
                Some((kind, body)) => vec![self.alert(kind, &body)],
                None => self
                    .blocks(&quote.children, source, depth)
                    .into_iter()
                    .map(|block| match block {
                        Block::Paragraph(paragraph) => Block::Paragraph(quoted(paragraph)),
                        table => table,
                    })
                    .collect(),
            },
            Node::List(list) => self.list(list, source, depth),
            Node::Code(code) => vec![code_block(&code.value)],
            Node::Math(math) => vec![self.display_math(&math.value)],
            Node::Table(table) => vec![self.table(table, source)],
            Node::ThematicBreak(_) => {
                let mut rule = Paragraph::new();
                rule.property = rule.property.set_borders(
                    ParagraphBorders::with_empty().set(
                        ParagraphBorder::new(ParagraphBorderPosition::Bottom)
                            .val(BorderType::Single)
                            .size(6)
                            .color(BORDER_COLOR),
                    ),
                );
                vec![Block::Paragraph(rule)]
            }
            Node::Yaml(yaml) => front_matter(&yaml.value, self.text_width)
                .into_iter()
                .collect(),
            Node::Html(html) => {
                let value = html.value.trim();
                if value.starts_with("<!--") {
                    return Vec::new();
                }
                vec![Block::Paragraph(
                    Paragraph::new().add_run(mono_run(value).color(MUTED_COLOR)),
                )]
            }
            Node::Definition(_) | Node::FootnoteDefinition(_) => Vec::new(),
            other => match other.children() {
                Some(children) => self.blocks(children, source, depth),
                None => Vec::new(),
            },
        }
    }

    /// Add `nodes` as inline content of `paragraph`.
    fn paragraph(
        &mut self,
        paragraph: Paragraph,
        nodes: &[Node],
        source: &str,
        marks: Marks,
    ) -> Paragraph {
        self.inlines(nodes, source, marks)
            .into_iter()
            .fold(paragraph, |paragraph, piece| match piece {
                Piece::Run(run) => paragraph.add_run(run),
                Piece::Link(link) => paragraph.add_hyperlink(link),
            })
    }

    fn inlines(&mut self, nodes: &[Node], source: &str, marks: Marks) -> Vec<Piece> {
        nodes
            .iter()
            .flat_map(|node| self.inline(node, source, marks))
            .collect()
    }

    fn inline(&mut self, node: &Node, source: &str, marks: Marks) -> Vec<Piece> {
        match node {
            Node::Text(text) => vec![Piece::Run(styled_run(
                &text.value.replace('\n', " "),
                marks,
            ))],
            Node::Emphasis(emphasis) => self.inlines(
                &emphasis.children,
                source,
                Marks {
                    italic: true,
                    ..marks
                },
            ),
            Node::Strong(strong) => self.inlines(
                &strong.children,
                source,
                Marks {
                    bold: true,
                    ..marks
                },
            ),
            Node::Delete(delete) => self.inlines(
                &delete.children,
                source,
                Marks {
                    strike: true,
                    ..marks
                },
            ),
            Node::InlineCode(code) => vec![Piece::Run(
                mono_run(&code.value).shading(Shading::new().fill("EFF1F3")),
            )],
            Node::Break(_) => vec![Piece::Run(Run::new().add_break(BreakType::TextWrapping))],
            Node::Link(link) => self.link(&link.url, &link.children, source, marks),
            Node::LinkReference(reference) => {
                match self
                    .definitions
                    .get(&reference.identifier.to_lowercase())
                    .cloned()
                {
                    Some(url) => self.link(&url, &reference.children, source, marks),
                    None => vec![Piece::Run(styled_run(node_source(node, source), marks))],
                }
            }
            Node::Image(image) => self.inline_image(&image.url, &image.alt),
            Node::ImageReference(reference) => {
                match self
                    .definitions
                    .get(&reference.identifier.to_lowercase())
                    .cloned()
                {
                    Some(url) => self.inline_image(&url, &reference.alt),
                    None => vec![Piece::Run(styled_run(&reference.alt, marks))],
                }
            }
            Node::InlineMath(math) => {
                let end = node.position().map_or(0, |position| position.end.offset);
                let following = source.get(end..).and_then(|rest| rest.chars().next());
                if is_inline_math(&math.value, following) {
                    vec![self.inline_math(&math.value)]
                } else {
                    vec![Piece::Run(styled_run(node_source(node, source), marks))]
                }
            }
            Node::FootnoteReference(reference) => {
                match self.footnotes.get(&reference.identifier.to_lowercase()) {
                    Some(paragraphs) => {
                        let footnote = paragraphs
                            .iter()
                            .cloned()
                            .fold(Footnote::new(), |mut footnote, paragraph| {
                                footnote.add_content(paragraph)
                            });
                        vec![Piece::Run(Run::new().add_footnote_reference(footnote))]
                    }
                    None => vec![Piece::Run(styled_run(node_source(node, source), marks))],
                }
            }
            Node::Html(html) if html.value.trim().to_ascii_lowercase().starts_with("<br") => {
                vec![Piece::Run(Run::new().add_break(BreakType::TextWrapping))]
            }
            // Other inline tags (`<kbd>`, `<sub>`, …) are dropped; the text
            // between them is kept.
            Node::Html(_) => Vec::new(),
            other => match other.children() {
                Some(children) => self.inlines(children, source, marks),
                None => Vec::new(),
            },
        }
    }

    fn link(&mut self, url: &str, children: &[Node], source: &str, marks: Marks) -> Vec<Piece> {
        let pieces = self.inlines(
            children,
            source,
            Marks {
                link: !url.starts_with('#'),
                ..marks
            },
        );
        if url.is_empty() || url.starts_with('#') {
            return pieces;
        }
        let link = pieces.into_iter().fold(
            Hyperlink::new(url, HyperlinkType::External),
            |link, piece| match piece {
                Piece::Run(run) => link.add_run(run),
                Piece::Link(_) => link,
            },
        );
        vec![Piece::Link(link)]
    }

    fn list(&mut self, list: &markdown::mdast::List, source: &str, depth: usize) -> Vec<Block> {
        let numbering_id = if list.ordered {
            let id = DECIMAL + 1 + self.numberings.len();
            let start = list.start.unwrap_or(1) as usize;
            self.numberings.push(
                Numbering::new(id, DECIMAL).add_override(LevelOverride::new(depth).start(start)),
            );
            id
        } else {
            self.bullet_numbering()
        };
        let after = if list.spread { 120 } else { 40 };
        let level = depth.min(8);
        // Checkboxes replace bullets: in mixed lists they sit where the
        // bullets do, lists of only tasks tuck under their parent's text.
        let all_tasks = list
            .children
            .iter()
            .all(|item| matches!(item, Node::ListItem(item) if item.checked.is_some()));
        let task_indent = if all_tasks {
            360 * (level as i32 + 1)
        } else {
            indent_twips(level) - 360
        };
        let mut blocks = Vec::new();
        for item in &list.children {
            let Node::ListItem(item) = item else {
                continue;
            };
            let mut first = true;
            for child in &item.children {
                let child_blocks = match child {
                    Node::Paragraph(paragraph) if first => {
                        let paragraph =
                            Paragraph::new().line_spacing(LineSpacing::new().after(after));
                        let paragraph = match item.checked {
                            // Like GitHub, tasks show a checkbox instead of
                            // a bullet.
                            Some(done) => paragraph
                                .indent(Some(task_indent), None, None, None)
                                .add_run(Run::new().add_text(if done { "☑ " } else { "☐ " })),
                            None => paragraph
                                .numbering(NumberingId::new(numbering_id), IndentLevel::new(level)),
                        };
                        vec![Block::Paragraph(self.paragraph(
                            paragraph,
                            &paragraph_children(child),
                            source,
                            Marks::default(),
                        ))]
                    }
                    Node::List(nested) => self.list(nested, source, depth + 1),
                    other => self
                        .block(other, source, depth + 1)
                        .into_iter()
                        .map(|block| match block {
                            Block::Paragraph(paragraph) => Block::Paragraph(paragraph.indent(
                                Some(indent_twips(level)),
                                None,
                                None,
                                None,
                            )),
                            table => table,
                        })
                        .collect(),
                };
                first = false;
                blocks.extend(child_blocks);
            }
        }
        // Space the list from what follows like any other paragraph.
        if depth == 0
            && matches!(blocks.last(), Some(Block::Paragraph(_)))
            && let Some(Block::Paragraph(last)) = blocks.pop()
        {
            blocks.push(Block::Paragraph(
                last.line_spacing(LineSpacing::new().after(140)),
            ));
        }
        blocks
    }

    /// The numbering instance every bulleted list shares.
    fn bullet_numbering(&mut self) -> usize {
        if !self
            .numberings
            .iter()
            .any(|numbering| numbering.id == BULLET_NUMBERING)
        {
            self.numberings
                .push(Numbering::new(BULLET_NUMBERING, BULLETS));
        }
        BULLET_NUMBERING
    }

    fn table(&mut self, table: &markdown::mdast::Table, source: &str) -> Block {
        let columns = table.align.len().max(1);
        let rows = table
            .children
            .iter()
            .enumerate()
            .filter_map(|(row_ix, row)| {
                let Node::TableRow(row) = row else {
                    return None;
                };
                let header = row_ix == 0;
                let mut cells: Vec<TableCell> = row
                    .children
                    .iter()
                    .enumerate()
                    .map(|(column, cell)| {
                        let children = match cell {
                            Node::TableCell(cell) => cell.children.as_slice(),
                            _ => &[],
                        };
                        let alignment = match table.align.get(column) {
                            Some(AlignKind::Center) => AlignmentType::Center,
                            Some(AlignKind::Right) => AlignmentType::Right,
                            _ => AlignmentType::Left,
                        };
                        let paragraph = self.paragraph(
                            Paragraph::new()
                                .align(alignment)
                                .line_spacing(LineSpacing::new().after(0).before(0)),
                            children,
                            source,
                            Marks {
                                bold: header,
                                ..Marks::default()
                            },
                        );
                        let cell = TableCell::new().add_paragraph(paragraph);
                        if header {
                            cell.shading(Shading::new().fill(SUBTLE_FILL))
                        } else {
                            cell
                        }
                    })
                    .collect();
                while cells.len() < columns {
                    cells.push(TableCell::new().add_paragraph(Paragraph::new()));
                }
                let row = TableRow::new(cells).cant_split();
                Some(if header {
                    row.row_height(1.).height_rule(HeightRule::AtLeast)
                } else {
                    row
                })
            })
            .collect();
        let border = |position| {
            TableBorder::new(position)
                .border_type(BorderType::Single)
                .size(4)
                .color(BORDER_COLOR)
        };
        let borders = [
            TableBorderPosition::Top,
            TableBorderPosition::Bottom,
            TableBorderPosition::Left,
            TableBorderPosition::Right,
            TableBorderPosition::InsideH,
            TableBorderPosition::InsideV,
        ]
        .into_iter()
        .fold(TableBorders::with_empty(), |borders, position| {
            borders.set(border(position))
        });
        Block::table(
            Table::new(rows)
                .set_borders(borders)
                .margins(TableCellMargins::new().margin(50, 110, 50, 110)),
        )
    }

    fn alert(&mut self, kind: AlertKind, body: &str) -> Block {
        let (color, fill, border) = alert_colors(kind);
        let title = Paragraph::new()
            .line_spacing(LineSpacing::new().after(60))
            .add_run(Run::new().add_text(kind.title()).bold().color(color));
        let body = spaced(self.document(body));
        let cell = body.into_iter().fold(
            TableCell::new()
                .add_paragraph(title)
                .shading(Shading::new().fill(fill))
                .set_borders(cell_borders(border, Some(color))),
            |cell, block| match block {
                Block::Paragraph(paragraph) => cell.add_paragraph(paragraph),
                Block::Table(table) => cell.add_table(*table),
            },
        );
        Block::table(boxed(cell))
    }

    fn display_math(&mut self, tex: &str) -> Block {
        let size = BODY_SIZE * crate::preview_ext::DISPLAY_MATH_SCALE;
        let paragraph = Paragraph::new().align(AlignmentType::Center);
        Block::Paragraph(
            match math::render_png(
                tex,
                math::MathStyle::Display,
                size,
                "#1f2328ff",
                RASTER_DENSITY,
            ) {
                Ok(png) => {
                    paragraph.add_run(Run::new().add_image(picture(png.png, png.width, png.height)))
                }
                Err(_) => paragraph.add_run(mono_run(tex.trim()).color("D1242F")),
            },
        )
    }

    fn inline_math(&mut self, tex: &str) -> Piece {
        let size = BODY_SIZE * crate::preview_ext::INLINE_MATH_SCALE;
        match math::render_png(
            tex,
            math::MathStyle::Inline,
            size,
            "#1f2328ff",
            RASTER_DENSITY,
        ) {
            Ok(png) => {
                let mut run = Run::new();
                let drop = half_points(png.height - png.baseline);
                if drop > 0 {
                    run = run.style(&format!("{DROP_STYLE}{drop}"));
                }
                Piece::Run(run.add_image(picture(png.png, png.width, png.height)))
            }
            Err(_) => Piece::Run(mono_run(tex.trim()).color("D1242F")),
        }
    }

    /// Load `url` as a PNG with its natural size in points.
    fn load_image(&self, url: &str) -> Option<(Vec<u8>, f32, f32)> {
        let image = images::load(url, self.base_dir)?;
        if image.extension == "svg" {
            return typst_world::svg_to_png(image.bytes, RASTER_DENSITY / 2.).ok();
        }
        let decoded = image::load_from_memory(&image.bytes).ok()?;
        let (width, height) = (decoded.width(), decoded.height());
        let png = if image.extension == "png" {
            image.bytes
        } else {
            let mut out = Cursor::new(Vec::new());
            decoded.write_to(&mut out, image::ImageFormat::Png).ok()?;
            out.into_inner()
        };
        // Pixels at 96 per inch, the web's assumption for untagged images.
        Some((png, width as f32 * 0.75, height as f32 * 0.75))
    }

    fn block_image(&mut self, url: &str, alt: &str) -> Block {
        let paragraph = Paragraph::new().align(AlignmentType::Center);
        Block::Paragraph(match self.load_image(url) {
            Some((png, width, height)) => {
                let scale = (self.text_width / width).min(1.);
                paragraph.add_run(Run::new().add_image(picture(png, width * scale, height * scale)))
            }
            None => paragraph.add_run(missing_image(alt)),
        })
    }

    fn inline_image(&mut self, url: &str, alt: &str) -> Vec<Piece> {
        match self.load_image(url) {
            Some((png, width, height)) => {
                // Inline images are usually badges and icons: size them to
                // the line.
                let line = BODY_SIZE * 1.3;
                let scale = (line / height).min(1.);
                vec![Piece::Run(Run::new().add_image(picture(
                    png,
                    width * scale,
                    height * scale,
                )))]
            }
            None => vec![Piece::Run(missing_image(alt))],
        }
    }
}

fn node_source<'a>(node: &Node, source: &'a str) -> &'a str {
    node.position()
        .and_then(|position| source.get(position.start.offset..position.end.offset))
        .unwrap_or_default()
}

fn paragraph_children(node: &Node) -> Vec<Node> {
    match node {
        Node::Paragraph(paragraph) => paragraph.children.clone(),
        _ => Vec::new(),
    }
}

fn half_points(points: f32) -> usize {
    (points * 2.).round() as usize
}

fn indent_twips(level: usize) -> i32 {
    720 * (level as i32 + 1)
}

fn styled_run(text: &str, marks: Marks) -> Run {
    let mut run = Run::new().add_text(text);
    if marks.bold {
        run = run.bold();
    }
    if marks.italic {
        run = run.italic();
    }
    if marks.strike {
        run = run.strike();
    }
    if marks.link {
        run = run.color(LINK_COLOR).underline("single");
    }
    run
}

fn mono_run(text: &str) -> Run {
    Run::new()
        .add_text(text)
        .fonts(
            RunFonts::new()
                .ascii(MONO_FONT)
                .hi_ansi(MONO_FONT)
                .cs(MONO_FONT),
        )
        .size(half_points(BODY_SIZE * 0.9))
}

fn missing_image(alt: &str) -> Run {
    Run::new().add_text(alt).italic().color(MUTED_COLOR)
}

fn picture(png: Vec<u8>, width_pt: f32, height_pt: f32) -> Pic {
    let (width_px, height_px) = png_dimensions(&png).unwrap_or((1, 1));
    Pic::new_with_dimensions(png, width_px, height_px).size(
        (width_pt * EMU_PER_PT).round() as u32,
        (height_pt * EMU_PER_PT).round() as u32,
    )
}

/// Width and height from a PNG's header.
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    let width = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

fn quoted(paragraph: Paragraph) -> Paragraph {
    let mut paragraph = paragraph.indent(Some(300), None, None, None);
    paragraph.property = paragraph.property.set_borders(
        ParagraphBorders::with_empty().set(
            ParagraphBorder::new(ParagraphBorderPosition::Left)
                .val(BorderType::Single)
                .size(18)
                .space(10)
                .color(BORDER_COLOR),
        ),
    );
    paragraph.color(MUTED_COLOR)
}

/// Borders for a boxed block: a thin frame, optionally with a thick
/// leading edge in `accent`.
fn cell_borders(color: &str, accent: Option<&str>) -> TableCellBorders {
    let edge = |position| {
        TableCellBorder::new(position)
            .border_type(BorderType::Single)
            .size(4)
            .color(color)
    };
    TableCellBorders::new()
        .set(edge(TableCellBorderPosition::Top))
        .set(edge(TableCellBorderPosition::Bottom))
        .set(edge(TableCellBorderPosition::Right))
        .set(match accent {
            Some(accent) => TableCellBorder::new(TableCellBorderPosition::Left)
                .border_type(BorderType::Single)
                .size(24)
                .color(accent),
            None => edge(TableCellBorderPosition::Left),
        })
}

fn code_block(code: &str) -> Block {
    let code = code.strip_suffix('\n').unwrap_or(code);
    let cell = code.split('\n').fold(
        TableCell::new()
            .shading(Shading::new().fill(SUBTLE_FILL))
            .set_borders(cell_borders(BORDER_COLOR, None)),
        |cell, line| {
            cell.add_paragraph(
                Paragraph::new()
                    .line_spacing(line_spacing(240).after(0).before(0))
                    // Keep leading indentation, which Word would collapse.
                    .add_run(mono_run(
                        &line.replace('\t', "    ").replace(' ', "\u{00A0}"),
                    )),
            )
        },
    );
    Block::table(boxed(cell))
}

/// A full-width, single-cell table drawn only by `cell`'s own borders.
fn boxed(cell: TableCell) -> Table {
    Table::without_borders(vec![TableRow::new(vec![cell])])
        .width(5_000, WidthType::Pct)
        .margins(TableCellMargins::new().margin(90, 150, 90, 150))
}

/// Auto (proportional) line spacing, in 240ths of a line.
fn line_spacing(line: i32) -> LineSpacing {
    LineSpacing::new()
        .line_rule(LineSpacingType::Auto)
        .line(line)
}

/// Follow every table with a short empty paragraph: Word merges adjacent
/// tables, requires a paragraph after a table that ends a cell, and puts
/// no space below tables otherwise.
fn spaced(blocks: Vec<Block>) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        let table = matches!(block, Block::Table(_));
        out.push(block);
        if table {
            out.push(Block::Paragraph(
                Paragraph::new().line_spacing(line_spacing(120).before(0).after(0)),
            ));
        }
    }
    out
}

/// Title, fill and border colors of an alert, matching the HTML export.
fn alert_colors(kind: AlertKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        AlertKind::Note => ("0969DA", "EFF5FC", "9CC3EF"),
        AlertKind::Tip => ("1A7F37", "EFF6F1", "A3CCAF"),
        AlertKind::Important => ("8250DF", "F5F2FC", "C9B6F1"),
        AlertKind::Warning => ("9A6700", "F8F4EC", "D6C196"),
        AlertKind::Caution => ("D1242F", "FCEFF0", "EDA3A8"),
    }
}

/// Top-level `key: value` pairs of YAML front matter as a borderless table.
fn front_matter(yaml: &str, text_width: f32) -> Option<Block> {
    let key_width = 2_000;
    let value_width = (text_width * 20.) as usize - key_width;
    let rows: Vec<TableRow> = yaml
        .lines()
        .filter(|line| !line.starts_with([' ', '\t', '-', '#']))
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim().trim_matches(['"', '\'']);
            (!key.trim().is_empty() && !value.is_empty()).then(|| {
                TableRow::new(vec![
                    TableCell::new()
                        .width(key_width, WidthType::Dxa)
                        .add_paragraph(
                            Paragraph::new()
                                .line_spacing(LineSpacing::new().after(0))
                                .add_run(Run::new().add_text(key.trim()).color(MUTED_COLOR)),
                        ),
                    TableCell::new()
                        .width(value_width, WidthType::Dxa)
                        .add_paragraph(
                            Paragraph::new()
                                .line_spacing(LineSpacing::new().after(0))
                                .add_run(Run::new().add_text(value)),
                        ),
                ])
            })
        })
        .collect();
    (!rows.is_empty())
        .then(|| Block::table(Table::without_borders(rows).set_grid(vec![key_width, value_width])))
}

fn heading_styles() -> Vec<Style> {
    let sizes = [20., 16., 13.5, 12., 11., 11.];
    sizes
        .iter()
        .enumerate()
        .map(|(ix, size)| {
            let level = ix + 1;
            let mut style = Style::new(format!("Heading{level}"), StyleType::Paragraph)
                // Word recognizes its built-in headings by these names.
                .name(format!("heading {level}"))
                .based_on("Normal")
                .next("Normal")
                .q_format(true)
                .size(half_points(*size))
                .bold()
                .color(TEXT_COLOR);
            if level == 6 {
                style = style.italic();
            }
            style.paragraph_property = style
                .paragraph_property
                .keep_next(true)
                .outline_lvl(ix)
                .line_spacing(
                    LineSpacing::new()
                        .before(if level <= 2 { 360 } else { 240 })
                        .after(120),
                );
            style
        })
        .collect()
}

fn bullets() -> AbstractNumbering {
    const MARKERS: [&str; 3] = ["•", "◦", "▪"];
    (0..9).fold(AbstractNumbering::new(BULLETS), |numbering, level| {
        numbering.add_level(
            Level::new(
                level,
                Start::new(1),
                NumberFormat::new("bullet"),
                LevelText::new(MARKERS[level % MARKERS.len()]),
                LevelJc::new("left"),
            )
            .indent(
                Some(indent_twips(level)),
                Some(SpecialIndentType::Hanging(360)),
                None,
                None,
            ),
        )
    })
}

fn decimals() -> AbstractNumbering {
    (0..9).fold(AbstractNumbering::new(DECIMAL), |numbering, level| {
        numbering.add_level(
            Level::new(
                level,
                Start::new(1),
                NumberFormat::new(if level % 3 == 1 {
                    "lowerLetter"
                } else if level % 3 == 2 {
                    "lowerRoman"
                } else {
                    "decimal"
                }),
                LevelText::new(format!("%{}.", level + 1)),
                LevelJc::new("left"),
            )
            .indent(
                Some(indent_twips(level)),
                Some(SpecialIndentType::Hanging(360)),
                None,
                None,
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_xml(docx: &[u8]) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(docx)).unwrap();
        let mut xml = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        xml
    }

    #[test]
    fn exports_a_structured_document() {
        let source = "# Title\n\nSome **bold**, _italic_ and a [link](https://example.com).\n\n\
                      - one\n- two\n  1. nested\n- [x] done\n\n\
                      > [!WARNING]\n> Careful with $x^2$.\n\n\
                      | A | B |\n|---|:-:|\n| 1 | 2 |\n\n\
                      ```\nfn main() {}\n```\n\n$$\\frac{1}{2}$$\n\nText[^n].\n\n[^n]: A note.\n";
        let docx = to_docx(source, "t", None, Paper::A4).unwrap();
        assert!(docx.starts_with(b"PK"));
        let xml = document_xml(&docx);
        assert!(xml.contains("w:val=\"Heading1\""));
        assert!(xml.contains("<w:b />") || xml.contains("<w:b/>"));
        assert!(xml.contains("w:numId"));
        assert!(xml.contains("Warning"));
        assert!(xml.contains("fn\u{a0}main()"));
        assert!(xml.contains("<w:drawing>"));
        assert!(xml.contains("w:footnoteReference"));
        assert!(xml.contains("☑"));
        assert!(xml.contains("<w:tblHeader />"));
        assert!(!xml.contains("Malgel") && !xml.contains(HEADER_ROW_MARKER));
        // The alert and code block after the list survive.
        assert_eq!(xml.matches("<w:tbl>").count(), 3);
    }

    #[test]
    fn lowers_formulas_with_descenders() {
        let xml = document_xml(&to_docx("Where $y_1$ holds.\n", "t", None, Paper::A4).unwrap());
        assert!(xml.contains("<w:position w:val=\"-"), "{xml}");
        assert!(!xml.contains(DROP_STYLE));
    }

    #[test]
    fn keeps_prices_and_reports_bad_formulas_as_text() {
        let xml = document_xml(
            &to_docx("$5 and $10.\n\n$$\\frac{1}{$$\n", "t", None, Paper::Letter).unwrap(),
        );
        assert!(xml.contains("$5 and $"));
        assert!(xml.contains("\\frac{1}{"));
        assert!(!xml.contains("<w:drawing>"));
    }

    #[test]
    fn reads_png_dimensions() {
        let png = math::render_png("x", math::MathStyle::Inline, 11., "#000000ff", 2.).unwrap();
        let (width, height) = png_dimensions(&png.png).unwrap();
        assert!(width > 0 && height > 0);
    }
}
