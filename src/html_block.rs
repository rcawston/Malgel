//! Block-level HTML in the preview: `<details>` sections that open and
//! close, and content centered or right-aligned with `align="…"` (on
//! `<div>`, `<p>` and headings) or `<center>`.
//!
//! GPUI Kit draws HTML blocks itself but ignores these tags. A section often
//! spans several Markdown blocks: its opening tag, a blank line, Markdown,
//! and its closing tag later on. Blocks reach plugins one at a time, in
//! document order, so the plugin, given an opening tag, finds the closing
//! tag in the source that follows, draws the whole section (its Markdown in
//! a nested view) and hides the blocks up to the closing tag as they come.

use std::{
    cell::RefCell,
    collections::HashSet,
    ops::Range,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window,
    assets::IconName,
    base::{TextView, TextViewState},
    component::{
        ActiveTheme as _, Icon, Sizable as _, h_flex,
        text::{
            MarkdownExtensions, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            markdown_ast::Node,
        },
        v_flex,
    },
    div,
    prelude::FluentBuilder as _,
};

use crate::{
    analysis::{content_hash, preview_parse_options},
    html_inline::{RawTag, decode_entities, parse_tag},
    images,
};

/// What the preview's nested views need from the document around them.
#[derive(Default)]
pub struct PreviewEnv {
    /// The document's folder, which relative image paths start from.
    base_dir: Mutex<Option<Arc<Path>>>,
}

impl PreviewEnv {
    pub fn set_base_dir(&self, dir: Option<Arc<Path>>) {
        *self.base_dir.lock().expect("preview env poisoned") = dir;
    }

    pub fn base_dir(&self) -> Option<Arc<Path>> {
        self.base_dir.lock().expect("preview env poisoned").clone()
    }
}

/// A nested view of `markdown`, rendered like the preview around it.
pub fn nested_view(
    id: impl Into<gpui_kit::ElementId>,
    markdown: SharedString,
    extensions: MarkdownExtensions,
    env: &Arc<PreviewEnv>,
) -> TextView {
    let base_dir = env.base_dir();
    TextView::markdown(id, markdown)
        .markdown_extensions(extensions)
        .image_source(move |url| images::resolve(url, base_dir.as_deref()))
        .on_link_click(crate::links::on_click)
        .w_full()
}

// -------------------------------------------------------------------------
// Reading sections

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Details { open: bool },
    Align(Align),
}

/// A part of an HTML block, or of a section begun in one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Other HTML, drawn by GPUI Kit as before.
    Raw(String),
    Section(Section),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub kind: Kind,
    /// A `<details>` section's summary, as plain text.
    pub summary: Option<String>,
    /// The section's content, as Markdown.
    pub body: String,
}

/// What opens a section: the tag, and the kind of section it opens.
fn section_kind(tag: &RawTag) -> Option<Kind> {
    if tag.closing {
        return None;
    }
    if tag.name == "details" {
        return Some(Kind::Details {
            open: tag.attribute("open").is_some(),
        });
    }
    if tag.name == "center" {
        return Some(Kind::Align(Align::Center));
    }
    let aligned = matches!(
        tag.name.as_str(),
        "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    );
    if !aligned {
        return None;
    }
    let align = tag
        .attribute("align")
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            tag.attribute("style")?.split(';').find_map(|declaration| {
                let (property, value) = declaration.split_once(':')?;
                property
                    .trim()
                    .eq_ignore_ascii_case("text-align")
                    .then(|| value.trim().to_ascii_lowercase())
            })
        })?;
    match align.as_str() {
        "center" => Some(Kind::Align(Align::Center)),
        "right" => Some(Kind::Align(Align::Right)),
        _ => None,
    }
}

/// The tags in `html`, with where each is, skipping comments.
fn tags(html: &str) -> Vec<(Range<usize>, RawTag)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(start) = html[at..].find('<').map(|ix| ix + at) {
        if html[start..].starts_with("<!--") {
            match html[start..].find("-->") {
                Some(end) => at = start + end + 3,
                None => break,
            }
            continue;
        }
        let Some(end) = html[start..].find('>').map(|ix| ix + start + 1) else {
            break;
        };
        if let Some(tag) = parse_tag(&html[start..end]) {
            found.push((start..end, tag));
        }
        at = end;
    }
    found
}

/// Where the tag closing `name` starts in `html`, given how many are open.
/// Without one, how many are still open at the end.
fn find_close(html: &str, name: &str, mut depth: usize) -> Result<Range<usize>, usize> {
    for (range, tag) in tags(html) {
        if tag.name != name {
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return Ok(range);
            }
        } else {
            depth += 1;
        }
    }
    Err(depth)
}

/// The lines of `html` without their indentation, which in Markdown would
/// turn them into code.
fn dedent(html: &str) -> String {
    html.lines()
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// A section's content: for `<details>`, its summary taken out; for an
/// aligned heading, the heading in Markdown.
fn section(tag: &RawTag, kind: Kind, content: &str) -> Section {
    let mut summary = None;
    let mut body = content.to_string();
    if matches!(kind, Kind::Details { .. }) {
        let trimmed = body.trim_start();
        if trimmed.to_ascii_lowercase().starts_with("<summary")
            && let Some(open_end) = trimmed.find('>')
            && let Ok(close) = find_close(&trimmed[open_end + 1..], "summary", 1)
        {
            let inner = &trimmed[open_end + 1..open_end + 1 + close.start];
            summary = Some(summary_text(inner));
            body = trimmed[open_end + 1 + close.end..].to_string();
        }
    }
    if let Some(level) = tag
        .name
        .strip_prefix('h')
        .and_then(|level| level.parse::<usize>().ok())
    {
        let text = content.split_whitespace().collect::<Vec<_>>().join(" ");
        body = format!("{} {text}", "#".repeat(level));
    }
    Section {
        kind,
        summary,
        body,
    }
}

/// A summary's text, without its tags.
fn summary_text(html: &str) -> String {
    let mut text = String::new();
    let mut at = 0;
    for (range, _) in tags(html) {
        text.push_str(&html[at..range.start]);
        at = range.end;
    }
    text.push_str(&html[at..]);
    decode_entities(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Split an HTML block into sections and other HTML. A section left open
/// at the end is returned apart, with its content so far, to be continued
/// by the blocks that follow.
pub fn segments(html: &str) -> (Vec<Segment>, Option<(RawTag, Kind, String)>) {
    let mut segments = Vec::new();
    let mut at = 0;
    loop {
        let next = tags(&html[at..])
            .into_iter()
            .find_map(|(range, tag)| section_kind(&tag).map(|kind| (range, tag, kind)));
        let Some((range, tag, kind)) = next else {
            break;
        };
        let (open_start, open_end) = (at + range.start, at + range.end);
        let raw = html[at..open_start].trim();
        if !raw.is_empty() {
            segments.push(Segment::Raw(raw.to_string()));
        }
        match find_close(&html[open_end..], &tag.name, 1) {
            Ok(close) => {
                let content = dedent(&html[open_end..open_end + close.start]);
                segments.push(Segment::Section(section(&tag, kind, &content)));
                at = open_end + close.end;
            }
            Err(_) => {
                let rest = dedent(&html[open_end..]);
                return (segments, Some((tag, kind, rest)));
            }
        }
    }
    let raw = html[at..].trim();
    if !raw.is_empty() {
        segments.push(Segment::Raw(raw.to_string()));
    }
    (segments, None)
}

/// A section continued past its opening HTML block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continued {
    /// The Markdown up to the closing tag.
    pub body: String,
    /// Where the block holding the closing tag ends in the source after
    /// the opening block.
    pub end: usize,
    /// How many top-level blocks that covers.
    pub blocks: usize,
}

/// Continue a section left open in an HTML block through `rest`, the
/// source after that block.
pub fn continue_section(rest: &str, name: &str) -> Option<Continued> {
    let root = markdown::to_mdast(rest, &preview_parse_options()).ok()?;
    let mut html_nodes = Vec::new();
    collect_html(&root, &mut html_nodes);
    let mut depth = 1;
    for range in html_nodes {
        let html = &rest[range.clone()];
        match find_close(html, name, depth) {
            Ok(close) => {
                let mut body = rest[..range.start]
                    .trim_start_matches(['\r', '\n'])
                    .trim_end()
                    .to_string();
                let before = dedent(&html[..close.start]);
                if !before.is_empty() {
                    body.push_str("\n\n");
                    body.push_str(&before);
                }
                let blocks = root
                    .children()
                    .into_iter()
                    .flatten()
                    .filter_map(Node::position)
                    .filter(|position| position.start.offset < range.end)
                    .count();
                return Some(Continued {
                    body,
                    end: range.end,
                    blocks,
                });
            }
            Err(open) => depth = open,
        }
    }
    None
}

fn collect_html(node: &Node, out: &mut Vec<Range<usize>>) {
    if let Node::Html(_) = node
        && let Some(position) = node.position()
    {
        out.push(position.start.offset..position.end.offset);
    }
    for child in node.children().into_iter().flatten() {
        collect_html(child, out);
    }
}

// -------------------------------------------------------------------------
// Exports

/// A block for exports: a node as it is, or an HTML block (with any
/// section it opens continued through the blocks it covers).
pub enum FlowItem<'a> {
    Node(&'a Node),
    Html(Vec<Segment>),
}

/// `nodes`, sibling blocks of `source`, with HTML sections read.
pub fn group_blocks<'a>(nodes: &'a [Node], source: &str) -> Vec<FlowItem<'a>> {
    let mut items = Vec::with_capacity(nodes.len());
    let mut skip_until = 0;
    for node in nodes {
        let Some(position) = node.position() else {
            items.push(FlowItem::Node(node));
            continue;
        };
        if position.start.offset < skip_until {
            continue;
        }
        let Node::Html(html) = node else {
            items.push(FlowItem::Node(node));
            continue;
        };
        let text = source
            .get(position.start.offset..position.end.offset)
            .unwrap_or(&html.value);
        let (mut segments, open) = segments(text);
        if let Some((tag, kind, content)) = open {
            let rest = source.get(position.end.offset..).unwrap_or_default();
            match continue_section(rest, &tag.name) {
                Some(continued) => {
                    let content = if content.is_empty() {
                        continued.body
                    } else {
                        format!("{content}\n\n{}", continued.body)
                    };
                    segments.push(Segment::Section(section(&tag, kind, &content)));
                    skip_until = position.end.offset + continued.end;
                }
                None => {
                    items.push(FlowItem::Node(node));
                    continue;
                }
            }
        }
        items.push(FlowItem::Html(segments));
    }
    items
}

/// The text of `html`, without its tags or comments.
pub fn strip_tags(html: &str) -> String {
    let html = strip_comments(html);
    let mut text = String::new();
    let mut at = 0;
    for (range, _) in tags(&html) {
        text.push_str(&html[at..range.start]);
        text.push(' ');
        at = range.end;
    }
    text.push_str(&html[at..]);
    decode_entities(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// `html` rebuilt to go into an HTML document: the tags Malgel knows, with
/// only harmless attributes, and the text between tags. Other tags are
/// escaped, to show as text as they did before, and comments left out.
pub fn sanitize_fragment(html: &str) -> String {
    let html = strip_comments(html);
    let mut out = String::with_capacity(html.len());
    let mut rest = html.as_str();
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('>').map(|ix| start + ix + 1);
        let rebuilt = end.and_then(|end| {
            let text = &rest[start..end];
            // A self-closing tag (`<br/>`, `<img … />`) as an opening one.
            let open = match text.strip_suffix("/>") {
                Some(inner) => format!("{}>", inner.trim_end()),
                None => text.to_string(),
            };
            rebuild_tag(&parse_tag(&open)?)
        });
        match (rebuilt, end) {
            (Some(rebuilt), Some(end)) => {
                out.push_str(&rebuilt);
                rest = &rest[end..];
            }
            (None, Some(end)) => {
                out.push_str(&rest[start..end].replace('<', "&lt;").replace('>', "&gt;"));
                rest = &rest[end..];
            }
            (_, None) => {
                out.push_str("&lt;");
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// One tag, rebuilt with only the attributes Malgel keeps, or `None` for a
/// tag it leaves out.
fn rebuild_tag(tag: &RawTag) -> Option<String> {
    let name = tag.name.as_str();
    let block = matches!(
        name,
        "details"
            | "summary"
            | "div"
            | "p"
            | "center"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "a"
            | "img"
            | "br"
            | "hr"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "pre"
    );
    if !block && crate::html_inline::ExportTag::from_name(name).is_none() {
        return None;
    }
    if tag.closing {
        return (!matches!(name, "img" | "br" | "hr")).then(|| format!("</{name}>"));
    }
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut out = format!("<{name}");
    let mut keep = |attribute: &str| {
        if let Some(value) = tag.attribute(attribute) {
            out.push_str(&format!(" {attribute}=\"{}\"", escape(value)));
        }
    };
    keep("title");
    keep("class");
    match name {
        "details" if tag.attribute("open").is_some() => out.push_str(" open"),
        "img" => {
            for attribute in ["src", "alt", "width", "height"] {
                if attribute == "src" && tag.attribute("src").is_some_and(|src| !is_safe_url(src)) {
                    continue;
                }
                keep(attribute);
            }
        }
        "a" if tag.attribute("href").is_some_and(is_safe_url) => keep("href"),
        _ => {}
    }
    if let Some(Kind::Align(align)) = section_kind(tag) {
        out.push_str(match align {
            Align::Center => " align=\"center\"",
            Align::Right => " align=\"right\"",
        });
    }
    if let Some(color) = tag.color() {
        out.push_str(&format!(" style=\"color:#{}\"", color.document_hex()));
    }
    out.push('>');
    Some(out)
}

/// Whether a link or image address is one a page may use: not a script.
fn is_safe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    !(lower.starts_with("javascript:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("data:text"))
}

// -------------------------------------------------------------------------
// Hiding the blocks inside a section

/// Blocks inside a section, which its first block draws.
struct Covered {
    /// Where the blocks after the opening block start and end, in the
    /// document.
    range: Range<usize>,
    /// The section's source, from its opening tag, to tell it's still the
    /// same text.
    whole: Range<usize>,
    hash: u64,
}

thread_local! {
    // Blocks are parsed in document order on one thread, so a section's
    // later blocks come through right after its first.
    static COVERED: RefCell<Vec<Covered>> = const { RefCell::new(Vec::new()) };
}

fn is_covered(node: &Node, cx: &MarkdownParseContext<'_>) -> bool {
    let Some(position) = node.position() else {
        return false;
    };
    let start = cx.offset() + position.start.offset;
    let end = cx.offset() + position.end.offset;
    COVERED.with(|covered| {
        covered.borrow().iter().any(|covered| {
            covered.range.start <= start
                && end <= covered.range.end
                && covered
                    .whole
                    .start
                    .checked_sub(cx.offset())
                    .zip(covered.whole.end.checked_sub(cx.offset()))
                    .and_then(|(start, end)| cx.source().get(start..end))
                    .is_some_and(|text| content_hash(text) == covered.hash)
        })
    })
}

fn cover(covered: Covered) {
    COVERED.with(|list| {
        let mut list = list.borrow_mut();
        if list.len() >= 64 {
            list.remove(0);
        }
        list.push(covered);
    });
}

// -------------------------------------------------------------------------
// The plugin

/// What the plugin keeps on a node.
#[derive(Debug, Clone)]
enum Part {
    /// The block's sections and other HTML, and how many top-level blocks
    /// after it the last section covers.
    Segments(Arc<Vec<Segment>>, usize),
    Hidden,
}

pub struct HtmlBlockPlugin {
    env: Arc<PreviewEnv>,
    /// Extensions for the sections' content: the preview's own, this plugin
    /// included, set once they are all built.
    extensions: Arc<OnceLock<MarkdownExtensions>>,
    /// `<details>` sections opened or closed from how they started, by
    /// content.
    toggled: Arc<Mutex<HashSet<u64>>>,
    preview: WeakEntity<TextViewState>,
}

impl HtmlBlockPlugin {
    pub fn new(
        env: Arc<PreviewEnv>,
        extensions: Arc<OnceLock<MarkdownExtensions>>,
        preview: WeakEntity<TextViewState>,
    ) -> Self {
        Self {
            env,
            extensions,
            toggled: Arc::default(),
            preview,
        }
    }
}

impl MarkdownPlugin for HtmlBlockPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "html-block"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        if is_covered(node, cx) {
            return Some(
                MarkdownNode::new("html-block", Part::Hidden)
                    .markdown(cx.node_source(node).unwrap_or_default().to_string()),
            );
        }
        let Node::Html(html) = node else {
            return None;
        };
        let source = cx.node_source(node).unwrap_or(&html.value);
        let (mut segments, open) = segments(source);
        let mut covered_rows = 0;
        if let Some((tag, kind, content)) = open {
            let position = node.position()?;
            let rest = cx.source().get(position.end.offset..)?;
            let Continued { body, end, blocks } = continue_section(rest, &tag.name)?;
            // Hidden blocks are rows of the preview only at the top level,
            // where each still takes a paragraph gap.
            if position.start.column == 1 {
                covered_rows = blocks;
            }
            let content = if content.is_empty() {
                body
            } else {
                format!("{content}\n\n{body}")
            };
            segments.push(Segment::Section(section(&tag, kind, &content)));
            let opened = cx.offset() + position.start.offset;
            let after = cx.offset() + position.end.offset;
            let whole = position.start.offset..position.end.offset + end;
            cover(Covered {
                range: after..after + end,
                whole: opened..opened + whole.len(),
                hash: content_hash(&cx.source()[whole]),
            });
        }
        if !segments
            .iter()
            .any(|segment| matches!(segment, Segment::Section(_)))
        {
            return None;
        }
        let text = segments
            .iter()
            .map(|segment| match segment {
                Segment::Raw(raw) => raw.clone(),
                Segment::Section(section) => match &section.summary {
                    Some(summary) => format!("{summary}\n{}", section.body),
                    None => section.body.clone(),
                },
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(
            MarkdownNode::new(
                "html-block",
                Part::Segments(Arc::new(segments), covered_rows),
            )
            .text(text)
            .markdown(source.to_string()),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(Part::Segments(segments, covered_rows)) = node.data::<Part>() else {
            return div().into_any_element();
        };
        let extensions = self.extensions.get().cloned().unwrap_or_default();
        let at = node.source_range().map_or(0, |range| range.start);
        v_flex()
            .w_full()
            .gap_2()
            // GPUI Kit leaves a paragraph gap (1rem) under each hidden
            // block; take that space back by overlapping them.
            .mb(gpui_kit::rems(-(*covered_rows as f32)))
            .children(segments.iter().enumerate().map(|(ix, segment)| {
                let id = format!("html-block-{at}-{ix}");
                match segment {
                    Segment::Raw(raw) => {
                        nested_view(id, raw.clone().into(), extensions.clone(), &self.env)
                            .into_any_element()
                    }
                    Segment::Section(section) => {
                        self.render_section(id, section, extensions.clone(), cx)
                    }
                }
            }))
            .into_any_element()
    }
}

impl HtmlBlockPlugin {
    fn render_section(
        &self,
        id: String,
        section: &Section,
        extensions: MarkdownExtensions,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let body = nested_view(
            format!("{id}-body"),
            section.body.clone().into(),
            extensions,
            &self.env,
        );
        match section.kind {
            // GPUI Kit aligns plain text by the text-align it inherits, but
            // lays out paragraphs holding images or other objects from the
            // left whatever it is, so the content, as wide as it needs, is
            // aligned as a whole too.
            Kind::Align(align) => h_flex()
                .w_full()
                .map(|this| match align {
                    Align::Center => this.justify_center().text_center(),
                    Align::Right => this.justify_end().text_right(),
                })
                .child(div().max_w_full().child(body.w_auto()))
                .into_any_element(),
            Kind::Details { open } => {
                let key = content_hash(&format!("{:?}{}", section.summary, section.body));
                let toggled = self
                    .toggled
                    .lock()
                    .expect("details state poisoned")
                    .contains(&key);
                let is_open = open != toggled;
                let state = self.toggled.clone();
                let preview = self.preview.clone();
                let summary = section.summary.clone().unwrap_or_else(|| "Details".into());
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("{id}-summary")))
                            .gap_1()
                            .cursor_pointer()
                            .rounded(cx.theme().radius)
                            .hover(|style| style.bg(cx.theme().muted.opacity(0.6)))
                            .child(
                                Icon::new(if is_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .small()
                                .text_color(cx.theme().muted_foreground),
                            )
                            .child(summary)
                            .on_click(move |_, _, cx| {
                                {
                                    let mut toggled = state.lock().expect("details state poisoned");
                                    if !toggled.remove(&key) {
                                        toggled.insert(key);
                                    }
                                }
                                // The section's height changes.
                                _ = preview
                                    .update(cx, |preview, cx| preview.invalidate_inline_layout(cx));
                            }),
                    )
                    .when(is_open, |this| this.child(div().pl_5().child(body)))
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections(html: &str) -> Vec<Section> {
        segments(html)
            .0
            .into_iter()
            .filter_map(|segment| match segment {
                Segment::Section(section) => Some(section),
                Segment::Raw(_) => None,
            })
            .collect()
    }

    #[test]
    fn reads_aligned_blocks() {
        let found = sections(
            "<p align=\"center\">\n    <img src=\"logo.png\" width=\"120\"><br>\n    <small>Caption</small>\n</p>\n\
             <h1 align=\"center\">\n  My   Project\n</h1>\n<div style=\"text-align: right\">r</div>\n<center>c</center>",
        );
        assert_eq!(
            found
                .iter()
                .map(|section| section.kind.clone())
                .collect::<Vec<_>>(),
            [
                Kind::Align(Align::Center),
                Kind::Align(Align::Center),
                Kind::Align(Align::Right),
                Kind::Align(Align::Center)
            ]
        );
        // Indentation would make code of the content.
        assert_eq!(
            found[0].body,
            "<img src=\"logo.png\" width=\"120\"><br>\n<small>Caption</small>"
        );
        assert_eq!(found[1].body, "# My Project");
    }

    #[test]
    fn leaves_other_html_alone() {
        let (segments, open) = segments("<div>\n<p>plain</p>\n</div>");
        assert!(open.is_none());
        assert!(matches!(&segments[..], [Segment::Raw(_)]));
        assert!(sections("<p align=\"left\">x</p>").is_empty());
    }

    #[test]
    fn reads_details_in_one_block() {
        let found = sections(
            "<details open>\n<summary>Click <b>me</b> &amp; see</summary>\nInside <details><summary>n</summary>nested</details>\n</details>",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::Details { open: true });
        assert_eq!(found[0].summary.as_deref(), Some("Click me & see"));
        assert!(found[0].body.contains("nested</details>"));
    }

    #[test]
    fn continues_sections_through_later_blocks() {
        let source = "<details>\n<summary>More</summary>\n\nHidden **content**.\n\n```\n</details> in code\n```\n\n</details>\n\nAfter.\n";
        let first_block = "<details>\n<summary>More</summary>";
        let (found, open) = segments(first_block);
        assert!(found.is_empty());
        let (tag, kind, content) = open.unwrap();
        assert_eq!(kind, Kind::Details { open: false });
        assert_eq!(content, "<summary>More</summary>");
        let rest = &source[first_block.len()..];
        let Continued { body, end, blocks } = continue_section(rest, &tag.name).unwrap();
        assert_eq!(
            body.trim(),
            "Hidden **content**.\n\n```\n</details> in code\n```"
        );
        assert!(rest[end..].trim() == "After.");
        // The paragraph, the code block and the closing tag.
        assert_eq!(blocks, 3);
        let section = section(&tag, kind, &format!("{content}\n\n{body}"));
        assert_eq!(section.summary.as_deref(), Some("More"));
        assert!(section.body.trim().starts_with("Hidden"));
    }

    #[test]
    fn sanitizes_html_for_exports() {
        assert_eq!(
            sanitize_fragment(
                "<details open onclick=\"x()\">\n<summary>S</summary>\n<p align=\"center\" style=\"background:red\"><img src=\"logo.png\" width=\"90\" onerror=\"x()\"/><br>\n<a href=\"javascript:x()\">bad</a> <a href=\"https://ok\">ok</a></p><script>alert(1)</script><!-- c --><blink>b</blink>"
            ),
            "<details open>\n<summary>S</summary>\n<p align=\"center\"><img src=\"logo.png\" width=\"90\"><br>\n<a>bad</a> <a href=\"https://ok\">ok</a></p>&lt;script&gt;alert(1)&lt;/script&gt;&lt;blink&gt;b&lt;/blink&gt;"
        );
        assert_eq!(
            sanitize_fragment(r#"<small class="detail" onclick="evil()">"#),
            r#"<small class="detail">"#
        );
        assert_eq!(
            sanitize_fragment(r#"<span style="color:red;background:url(x)" title='a"b'>"#),
            r#"<span title="a&quot;b" style="color:#d1242f">"#
        );
        assert_eq!(sanitize_fragment("</SUP>"), "</sup>");
        assert_eq!(sanitize_fragment("<br/>"), "<br>");
        assert_eq!(strip_tags("<p>a <b>b</b></p><!-- c -->"), "a b");
    }

    #[test]
    fn groups_sections_for_exports() {
        let source = "Intro\n\n<div align=\"center\">\n\n**Mid**\n\n</div>\n\nEnd\n";
        let root = markdown::to_mdast(source, &preview_parse_options()).unwrap();
        let items = group_blocks(root.children().unwrap(), source);
        assert_eq!(items.len(), 3);
        let FlowItem::Html(segments) = &items[1] else {
            panic!("expected the section")
        };
        assert_eq!(
            segments[..],
            [Segment::Section(Section {
                kind: Kind::Align(Align::Center),
                summary: None,
                body: "**Mid**".to_string(),
            })]
        );
    }

    #[test]
    fn needs_a_closing_tag_to_continue() {
        assert!(continue_section("\n\nNever closed.\n", "details").is_none());
    }
}
