//! Inline HTML in the preview: `<small>`, `<sub>`, `<sup>`, `<kbd>`, `<mark>`
//! and other tags Markdown writers reach for.
//!
//! GPUI Kit styles `<b>`, `<i>`, `<u>` and `<s>` itself and drops other tags,
//! keeping the text between them. Each raw tag reaches a plugin as a node of
//! its own, apart from the text it wraps, so tags are paired up for a whole
//! paragraph first: a block parser that never claims a block notes, for each
//! paragraph, heading or table it's about to parse, which nodes open a known
//! tag and which lie inside one. The inline plugin then draws each opening
//! tag as the whole styled run and hides the nodes up to its closing tag.
//!
//! A run is drawn as one object in the line, so it can be any size and sit
//! above or below the baseline, which GPUI's text runs can't. Long runs wrap
//! within the text column.

use std::{
    cell::RefCell,
    collections::HashMap,
    mem::Discriminant,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use gpui_kit::{
    App, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveElement as _,
    InteractiveText, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, StrikethroughStyle, Styled as _, StyledText, UnderlineStyle,
    Window,
    component::{
        ActiveTheme as _,
        text::{
            InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            markdown_ast::Node,
        },
        tooltip::Tooltip,
    },
    div, px,
};

use crate::{actions::FollowLink, analysis::content_hash};

/// Text standing in for the nodes a run hides. GPUI Kit replaces an empty
/// text with the node's source, so they need one invisible character.
const HIDDEN_TEXT: &str = "\u{200B}";
/// How wide the hidden nodes are drawn.
const HIDDEN_WIDTH: f32 = 1.;

/// The width of the preview's text column, in logical pixels, which long
/// runs wrap within.
static COLUMN_WIDTH: AtomicU32 = AtomicU32::new(0);

pub fn set_column_width(width: f32) {
    COLUMN_WIDTH.store(width.to_bits(), Ordering::Relaxed);
}

fn column_width() -> f32 {
    match f32::from_bits(COLUMN_WIDTH.load(Ordering::Relaxed)) {
        width if width > 0. => width,
        _ => 640.,
    }
}

/// The tags drawn here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Small,
    Big,
    Sub,
    Sup,
    Kbd,
    Mark,
    Ins,
    Abbr,
    /// `<span>` and `<font>`: only their color, if any, shows.
    Span,
    /// `<code>`, `<samp>` and `<tt>`.
    Code,
    /// `<cite>`, `<var>`, `<dfn>`: italic.
    Italic,
    Q,
}

impl Tag {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "small" => Self::Small,
            "big" => Self::Big,
            "sub" => Self::Sub,
            "sup" => Self::Sup,
            "kbd" => Self::Kbd,
            "mark" => Self::Mark,
            "ins" => Self::Ins,
            "abbr" | "acronym" => Self::Abbr,
            "span" | "font" => Self::Span,
            "code" | "samp" | "tt" => Self::Code,
            // `<b>`, `<i>`, `<u>`, `<s>` and their synonyms are GPUI Kit's.
            "cite" | "var" | "dfn" => Self::Italic,
            "q" => Self::Q,
            _ => return None,
        })
    }

    /// Text size relative to the text around it.
    fn scale(self) -> f32 {
        match self {
            Self::Small => 0.85,
            Self::Big => 1.2,
            Self::Sub | Self::Sup => 0.75,
            Self::Kbd | Self::Code => 0.875,
            _ => 1.,
        }
    }
}

/// A raw HTML tag: whether it closes, its lowercase name and its attributes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawTag {
    pub closing: bool,
    pub name: String,
    pub attributes: Vec<(String, String)>,
}

/// Read a single tag like `<small class="detail">` or `</small>`.
/// Self-closing tags and anything else return `None`.
pub fn parse_tag(html: &str) -> Option<RawTag> {
    let inner = html.trim().strip_prefix('<')?.strip_suffix('>')?;
    if inner.ends_with('/') || inner.starts_with('!') {
        return None;
    }
    let (closing, rest) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let name_len = rest
        .find(|ch: char| !ch.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    let name = rest[..name_len].to_ascii_lowercase();
    if name.is_empty() || name.contains('<') {
        return None;
    }
    let mut attributes = Vec::new();
    let mut rest = rest[name_len..].trim_start();
    while !rest.is_empty() {
        let key_len = rest
            .find(|ch: char| ch == '=' || ch.is_whitespace())
            .unwrap_or(rest.len());
        let key = rest[..key_len].to_ascii_lowercase();
        rest = rest[key_len..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            match after.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let body = &after[1..];
                    let end = body.find(quote).unwrap_or(body.len());
                    rest = body.get(end + 1..).unwrap_or("").trim_start();
                    body[..end].to_string()
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    rest = after[end..].trim_start();
                    after[..end].to_string()
                }
            }
        } else {
            String::new()
        };
        if key.is_empty() {
            break;
        }
        attributes.push((key, decode_entities(&value)));
    }
    Some(RawTag {
        closing,
        name,
        attributes,
    })
}

impl RawTag {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The color a `style="color: …"` or `<font color="…">` gives the text.
    pub fn color(&self) -> Option<Color> {
        let from_style = self.attribute("style").and_then(|style| {
            style.split(';').find_map(|declaration| {
                let (property, value) = declaration.split_once(':')?;
                (property.trim().eq_ignore_ascii_case("color")).then(|| value.trim())
            })
        });
        Color::parse(from_style.or_else(|| self.attribute("color"))?)
    }
}

fn decode_entities(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// A text color given in HTML.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    /// One the theme has its own shade of.
    Theme(ThemeColor),
    Rgb(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    Red,
    Green,
    Blue,
    Yellow,
    Magenta,
    Cyan,
    Muted,
}

impl Color {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim().trim_end_matches("!important").trim();
        if let Some(hex) = value.strip_prefix('#') {
            let digits = match hex.len() {
                3 => hex.chars().flat_map(|ch| [ch, ch]).collect::<String>(),
                6 => hex.to_string(),
                _ => return None,
            };
            return u32::from_str_radix(&digits, 16).ok().map(Self::Rgb);
        }
        if let Some(args) = value
            .strip_prefix("rgb(")
            .or_else(|| value.strip_prefix("rgba("))
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let parts: Vec<u32> = args
                .split([',', ' '])
                .filter(|part| !part.is_empty())
                .take(3)
                .map(|part| {
                    part.trim()
                        .parse::<f32>()
                        .ok()
                        .map(|v| v.clamp(0., 255.) as u32)
                })
                .collect::<Option<_>>()?;
            let [r, g, b] = parts[..] else { return None };
            return Some(Self::Rgb((r << 16) | (g << 8) | b));
        }
        Some(match value.to_ascii_lowercase().as_str() {
            "red" | "crimson" | "darkred" => Self::Theme(ThemeColor::Red),
            "green" | "darkgreen" | "seagreen" | "forestgreen" => Self::Theme(ThemeColor::Green),
            "blue" | "navy" | "royalblue" | "darkblue" => Self::Theme(ThemeColor::Blue),
            "yellow" | "gold" | "orange" | "darkorange" => Self::Theme(ThemeColor::Yellow),
            "purple" | "magenta" | "violet" | "fuchsia" => Self::Theme(ThemeColor::Magenta),
            "cyan" | "teal" | "aqua" => Self::Theme(ThemeColor::Cyan),
            "gray" | "grey" | "darkgray" | "darkgrey" | "dimgray" | "dimgrey" | "silver" => {
                Self::Theme(ThemeColor::Muted)
            }
            "black" => Self::Rgb(0x000000),
            "white" => Self::Rgb(0xffffff),
            "brown" => Self::Rgb(0xa52a2a),
            "pink" => Self::Rgb(0xff69b4),
            _ => return None,
        })
    }

    /// The color for printed documents, which are light, as `rrggbb`.
    pub fn document_hex(self) -> String {
        let rgb = match self {
            Self::Theme(color) => match color {
                ThemeColor::Red => 0xd1242f,
                ThemeColor::Green => 0x1a7f37,
                ThemeColor::Blue => 0x0969da,
                ThemeColor::Yellow => 0x9a6700,
                ThemeColor::Magenta => 0x8250df,
                ThemeColor::Cyan => 0x1b7c83,
                ThemeColor::Muted => 0x59636e,
            },
            Self::Rgb(rgb) => rgb,
        };
        format!("{rgb:06x}")
    }

    fn resolve(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Theme(color) => match color {
                ThemeColor::Red => theme.red,
                ThemeColor::Green => theme.green,
                ThemeColor::Blue => theme.blue,
                ThemeColor::Yellow => theme.yellow,
                ThemeColor::Magenta => theme.magenta,
                ThemeColor::Cyan => theme.cyan,
                ThemeColor::Muted => theme.muted_foreground,
            },
            Self::Rgb(rgb) => gpui_kit::rgb(rgb).into(),
        }
    }
}

// -------------------------------------------------------------------------
// Pairing tags

/// An inline tag exports understand: one of the tags drawn here, or one
/// GPUI Kit styles itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportTag {
    Run(Tag),
    Bold,
    Italic,
    Underline,
    Strike,
}

impl ExportTag {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "b" | "strong" => Self::Bold,
            "i" | "em" => Self::Italic,
            "u" => Self::Underline,
            "s" | "del" | "strike" => Self::Strike,
            other => Self::Run(Tag::from_name(other)?),
        })
    }
}

/// Inline nodes with known tags paired up, for exports.
pub enum Group<'a> {
    Node(&'a Node),
    Tagged {
        tag: ExportTag,
        open: RawTag,
        inner: &'a [Node],
    },
}

pub fn group_inlines(nodes: &[Node]) -> Vec<Group<'_>> {
    let mut groups = Vec::with_capacity(nodes.len());
    let mut ix = 0;
    while ix < nodes.len() {
        if let Some(open) = html_tag(&nodes[ix])
            && !open.closing
            && let Some(tag) = ExportTag::from_name(&open.name)
            && let Some(close) = matching_close(nodes, ix, &open.name)
        {
            groups.push(Group::Tagged {
                tag,
                open,
                inner: &nodes[ix + 1..close],
            });
            ix = close + 1;
            continue;
        }
        groups.push(Group::Node(&nodes[ix]));
        ix += 1;
    }
    groups
}

/// The plain text of `nodes`, as a run shows it.
pub fn plain_text(nodes: &[Node]) -> String {
    let mut run = build_run(Tag::Span, &RawTag::default(), nodes, 0);
    std::mem::take(&mut run.text)
}

/// `html`, an inline tag, rebuilt to go into an HTML document: known tags
/// with their `title`, `class` and color, and `<br>`. Anything else is
/// `None`, to be escaped as before.
pub fn sanitized_tag(html: &str) -> Option<String> {
    let trimmed = html.trim();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("<br")
        && rest.trim_start_matches([' ', '/']) == ">"
    {
        return Some("<br>".to_string());
    }
    let tag = parse_tag(trimmed)?;
    ExportTag::from_name(&tag.name)?;
    if tag.closing {
        return Some(format!("</{}>", tag.name));
    }
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut out = format!("<{}", tag.name);
    for name in ["title", "class"] {
        if let Some(value) = tag.attribute(name) {
            out.push_str(&format!(" {name}=\"{}\"", escape(value)));
        }
    }
    if let Some(color) = tag.color() {
        out.push_str(&format!(" style=\"color:#{}\"", color.document_hex()));
    }
    out.push('>');
    Some(out)
}

/// A styled run: the tag, the text it wraps and that text's own formatting.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub tag: Tag,
    title: Option<String>,
    color: Option<Color>,
    pub text: String,
    styles: Vec<(Range<usize>, Style)>,
    links: Vec<(Range<usize>, String)>,
    /// How many hidden nodes GPUI Kit draws after the run.
    hidden: usize,
    /// Where the opening tag starts in the document, to tell runs apart.
    id: usize,
}

/// Formatting inside a run, from Markdown or nested tags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    highlight: bool,
}

/// What the inline plugin does with a node.
#[derive(Debug, Clone)]
enum Role {
    /// Draw this run in place of the opening tag.
    Open(Arc<Run>),
    /// Inside a run (or its closing tag): drawn by the run already.
    Hidden,
}

type NodeKey = (usize, usize, Discriminant<Node>);

/// The roles noted for the block being parsed.
struct Noted {
    range: Range<usize>,
    hash: u64,
    roles: HashMap<NodeKey, Role>,
}

thread_local! {
    // A block is parsed start to finish on one thread, its inline nodes
    // right after the block parser sees it, so the roles noted for the
    // current block are all the inline plugin needs.
    static NOTED: RefCell<Option<Noted>> = const { RefCell::new(None) };
}

fn key(node: &Node, offset: usize) -> Option<NodeKey> {
    let position = node.position()?;
    Some((
        offset + position.start.offset,
        offset + position.end.offset,
        std::mem::discriminant(node),
    ))
}

/// The block parser: note the runs in a paragraph, heading or table about
/// to be parsed. It never claims a block.
pub fn observe_block(node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
    if !matches!(node, Node::Paragraph(_) | Node::Heading(_) | Node::Table(_)) {
        return None;
    }
    let source = cx.node_source(node).unwrap_or_default();
    let noted = node
        .position()
        .filter(|_| source.contains('<'))
        .and_then(|position| {
            let mut roles = HashMap::new();
            note_runs(node.children()?, cx.offset(), &mut roles);
            (!roles.is_empty()).then(|| Noted {
                range: cx.offset() + position.start.offset..cx.offset() + position.end.offset,
                hash: content_hash(source),
                roles,
            })
        });
    NOTED.with(|current| *current.borrow_mut() = noted);
    None
}

/// Pair known tags among `children`, as GPUI Kit pairs `<b>` and the like,
/// and look inside the rest for more.
fn note_runs(children: &[Node], offset: usize, roles: &mut HashMap<NodeKey, Role>) {
    let mut ix = 0;
    while ix < children.len() {
        if let Some(open) = html_tag(&children[ix])
            && !open.closing
            && let Some(tag) = Tag::from_name(&open.name)
            && let Some(close) = matching_close(children, ix, &open.name)
            && let Some(open_key) = key(&children[ix], offset)
        {
            let mut run = build_run(tag, &open, &children[ix + 1..close], open_key.0);
            run.hidden = drawn_count(&children[ix + 1..=close]);
            roles.insert(open_key, Role::Open(Arc::new(run)));
            for node in &children[ix + 1..=close] {
                if let Some(key) = key(node, offset) {
                    roles.insert(key, Role::Hidden);
                }
            }
            ix = close + 1;
            continue;
        }
        if let Some(grandchildren) = children[ix].children() {
            note_runs(grandchildren, offset, roles);
        }
        ix += 1;
    }
}

/// How many of `nodes` GPUI Kit draws as nodes of their own: all but the
/// `<b>`, `<i>`, `<u>` and `<s>` tags it pairs up itself.
fn drawn_count(nodes: &[Node]) -> usize {
    let mut paired = 0;
    for (ix, node) in nodes.iter().enumerate() {
        if let Some(tag) = html_tag(node)
            && !tag.closing
            && is_kit_tag(&tag.name)
            && matching_close(nodes, ix, &tag.name).is_some()
        {
            paired += 2;
        }
    }
    nodes.len().saturating_sub(paired)
}

/// The tags GPUI Kit styles itself.
fn is_kit_tag(name: &str) -> bool {
    matches!(
        name,
        "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "strike"
    )
}

fn html_tag(node: &Node) -> Option<RawTag> {
    let Node::Html(html) = node else {
        return None;
    };
    parse_tag(&html.value)
}

fn matching_close(children: &[Node], open: usize, name: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (ix, child) in children.iter().enumerate().skip(open + 1) {
        match html_tag(child) {
            Some(tag) if tag.name == name && !tag.closing => depth += 1,
            Some(tag) if tag.name == name => {
                if depth == 0 {
                    return Some(ix);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The role noted for `node`, if the block it belongs to is still the one
/// noted (the same text at the same place).
fn role(node: &Node, cx: &MarkdownParseContext<'_>) -> Option<Role> {
    let key = key(node, cx.offset())?;
    NOTED.with(|current| {
        let current = current.borrow();
        let noted = current.as_ref()?;
        let role = noted.roles.get(&key)?;
        let start = noted.range.start.checked_sub(cx.offset())?;
        let end = noted.range.end.checked_sub(cx.offset())?;
        let block = cx.source().get(start..end)?;
        (content_hash(block) == noted.hash).then(|| role.clone())
    })
}

/// Gather the text of `nodes` with its formatting.
fn build_run(tag: Tag, open: &RawTag, nodes: &[Node], id: usize) -> Run {
    let mut run = Run {
        tag,
        title: open.attribute("title").map(str::to_string),
        color: open.color(),
        text: String::new(),
        styles: Vec::new(),
        links: Vec::new(),
        hidden: 0,
        id,
    };
    if tag == Tag::Q {
        run.text.push('“');
    }
    let mut stack = Vec::new();
    collect(nodes, Style::default(), &mut stack, &mut run);
    if tag == Tag::Q {
        run.text.push('”');
    }
    run
}

fn push_text(run: &mut Run, text: &str, style: Style) {
    // Soft line breaks inside a run read as spaces, as in HTML.
    let text = text.replace(['\r', '\n'], " ");
    let start = run.text.len();
    run.text.push_str(&text);
    if style != Style::default() && !text.is_empty() {
        run.styles.push((start..run.text.len(), style));
    }
}

fn collect(nodes: &[Node], style: Style, stack: &mut Vec<(String, Style)>, run: &mut Run) {
    for node in nodes {
        // Nested tags change the style of what follows until they close.
        let style = stack.last().map_or(style, |(_, style)| *style);
        match node {
            Node::Text(text) => push_text(run, &text.value, style),
            Node::InlineCode(code) => push_text(run, &code.value, style),
            Node::InlineMath(math) => push_text(run, &format!("${}$", math.value), style),
            Node::Break(_) => push_text(run, " ", style),
            Node::Strong(strong) => collect(
                &strong.children,
                Style {
                    bold: true,
                    ..style
                },
                stack,
                run,
            ),
            Node::Emphasis(emphasis) => collect(
                &emphasis.children,
                Style {
                    italic: true,
                    ..style
                },
                stack,
                run,
            ),
            Node::Delete(delete) => collect(
                &delete.children,
                Style {
                    strikethrough: true,
                    ..style
                },
                stack,
                run,
            ),
            Node::Link(link) => {
                let start = run.text.len();
                collect(
                    &link.children,
                    Style {
                        underline: true,
                        ..style
                    },
                    stack,
                    run,
                );
                run.links.push((start..run.text.len(), link.url.clone()));
            }
            Node::Image(image) => push_text(run, &image.alt, style),
            Node::Html(html) => match parse_tag(&html.value) {
                Some(tag) if tag.closing => {
                    if let Some(at) = stack.iter().rposition(|(name, _)| *name == tag.name) {
                        stack.truncate(at);
                    }
                }
                Some(tag) => {
                    let nested = match tag.name.as_str() {
                        "b" | "strong" => Style {
                            bold: true,
                            ..style
                        },
                        "i" | "em" | "cite" | "var" | "dfn" => Style {
                            italic: true,
                            ..style
                        },
                        "u" | "ins" => Style {
                            underline: true,
                            ..style
                        },
                        "s" | "del" | "strike" => Style {
                            strikethrough: true,
                            ..style
                        },
                        "mark" => Style {
                            highlight: true,
                            ..style
                        },
                        _ => style,
                    };
                    stack.push((tag.name, nested));
                }
                None if html.value.trim().to_ascii_lowercase().starts_with("<br") => {
                    push_text(run, " ", style)
                }
                None => {}
            },
            other => {
                if let Some(children) = other.children() {
                    collect(children, style, stack, run);
                }
            }
        }
    }
}

// -------------------------------------------------------------------------
// Drawing runs

/// What the plugin keeps on a node.
#[derive(Debug, Clone)]
enum Part {
    Run(Arc<Run>),
    Hidden,
}

pub struct HtmlInlinePlugin;

impl MarkdownPlugin for HtmlInlinePlugin {
    fn name(&self) -> &str {
        "html-inline"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        match role(node, cx)? {
            Role::Open(run) => {
                let text = if run.text.is_empty() {
                    HIDDEN_TEXT.to_string()
                } else {
                    run.text.clone()
                };
                Some(
                    MarkdownNode::new("html-inline", Part::Run(run))
                        .text(text)
                        .markdown(cx.node_source(node).unwrap_or(HIDDEN_TEXT).to_string()),
                )
            }
            Role::Hidden => Some(
                MarkdownNode::new("html-inline", Part::Hidden)
                    .text(HIDDEN_TEXT)
                    .markdown(cx.node_source(node).unwrap_or(HIDDEN_TEXT).to_string()),
            ),
        }
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        match node.data::<Part>()? {
            // GPUI Kit takes an empty object (or one it rounds to empty) for
            // a failed one and draws the node's text instead, so this one is
            // a pixel wide, and the run overlaps it.
            Part::Hidden => {
                Some(InlineElement::new(div().w(px(HIDDEN_WIDTH)).h(px(1.))).with_baseline(px(0.)))
            }
            Part::Run(run) => Some(render_run(run, context, window, cx)),
        }
    }
}

fn render_run(
    run: &Arc<Run>,
    context: &InlineRenderContext,
    window: &mut Window,
    cx: &mut App,
) -> InlineElement {
    let theme = cx.theme();
    let outer = context.text_style();
    let font_size = context.font_size() * run.tag.scale();
    let line_height = context.line_height() * run.tag.scale();
    let mut font = outer.font();
    if run.tag == Tag::Code || run.tag == Tag::Kbd {
        font.family = theme.mono_font_family.clone();
    }
    let color = run.color.map_or(outer.color, |color| color.resolve(cx));

    // Where the text's baseline falls in the box, as GPUI lays a line out.
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&font);
    let ascent = text_system.ascent(font_id, font_size);
    let descent = text_system.descent(font_id, font_size).abs();
    let mut baseline = (line_height - ascent - descent) / 2. + ascent;

    let highlight = theme.warning.opacity(0.3);
    let mut styles: Vec<(Range<usize>, HighlightStyle)> = run
        .styles
        .iter()
        .map(|(range, style)| {
            let mut highlight_style = HighlightStyle::default();
            if style.bold {
                highlight_style.font_weight = Some(FontWeight::BOLD);
            }
            if style.italic {
                highlight_style.font_style = Some(FontStyle::Italic);
            }
            if style.underline {
                highlight_style.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    ..Default::default()
                });
            }
            if style.strikethrough {
                highlight_style.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.),
                    ..Default::default()
                });
            }
            if style.highlight {
                highlight_style.background_color = Some(highlight);
            }
            (range.clone(), highlight_style)
        })
        .collect();
    for (range, _) in &run.links {
        styles.push((
            range.clone(),
            HighlightStyle {
                color: Some(theme.link),
                ..Default::default()
            },
        ));
    }
    let whole = 0..run.text.len();
    match run.tag {
        // Set on the text itself: the box's font style doesn't reach it.
        Tag::Italic => styles.push((
            whole,
            HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            },
        )),
        Tag::Ins => styles.push((
            whole,
            HighlightStyle {
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        Tag::Abbr if run.title.is_some() => styles.push((
            whole,
            HighlightStyle {
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(theme.muted_foreground),
                    wavy: false,
                }),
                ..Default::default()
            },
        )),
        _ => {}
    }
    let styles = merge_highlights(styles);

    let id = ElementId::from(SharedString::from(format!("html-run-{}", run.id)));
    let styled = StyledText::new(run.text.clone()).with_highlights(styles);
    let text = if run.links.is_empty() {
        div().child(styled).into_any_element()
    } else {
        let ranges: Vec<Range<usize>> = run.links.iter().map(|(range, _)| range.clone()).collect();
        let urls: Vec<String> = run.links.iter().map(|(_, url)| url.clone()).collect();
        InteractiveText::new(id.clone(), styled)
            .on_click(ranges, move |ix, window, cx| {
                window.dispatch_action(
                    Box::new(FollowLink {
                        url: urls[ix].clone().into(),
                        alternate: false,
                    }),
                    cx,
                );
            })
            .into_any_element()
    };

    // Raise or lower sub- and superscripts by a third of the surrounding
    // text's size, padding the box so the baseline stays inside it.
    let shift = context.font_size() * 0.33;
    let mut element = div()
        .id(id)
        .max_w(px(column_width()))
        // The hidden pieces after the run take a pixel each; overlap them.
        .mr(px(-(run.hidden as f32 * HIDDEN_WIDTH)))
        .font_family(font.family.clone())
        .text_size(font_size)
        .line_height(line_height)
        .text_color(color);
    match run.tag {
        Tag::Sup => {
            element = element.pb(shift);
            baseline += shift;
        }
        Tag::Sub => element = element.pt(shift),
        Tag::Kbd => {
            element = element
                .px_1()
                .border_1()
                .border_b_2()
                .border_color(theme.border)
                .rounded(theme.radius)
                .bg(theme.muted);
            baseline += px(1.);
        }
        Tag::Code => {
            element = element.px_1().rounded(theme.radius).bg(theme.muted);
        }
        Tag::Mark => {
            element = element.px_0p5().rounded(px(2.)).bg(highlight);
        }
        _ => {}
    }
    if let Some(title) = run.title.clone() {
        element = element.tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx));
    }
    InlineElement::new(element.child(text)).with_baseline(baseline)
}

/// GPUI wants highlights in order and apart; split overlapping ones and
/// combine their styles.
fn merge_highlights(
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut bounds: Vec<usize> = highlights
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    bounds
        .windows(2)
        .filter_map(|pair| {
            let piece = pair[0]..pair[1];
            let mut style: Option<HighlightStyle> = None;
            for (range, highlight) in &highlights {
                if range.start <= piece.start && piece.end <= range.end {
                    style = Some(match style {
                        Some(style) => style.highlight(*highlight),
                        None => *highlight,
                    });
                }
            }
            style.map(|style| (piece, style))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(markdown: &str) -> Vec<Run> {
        let options = crate::analysis::preview_parse_options();
        let root = markdown::to_mdast(markdown, &options).unwrap();
        let mut roles = HashMap::new();
        note_runs(root.children().unwrap(), 0, &mut roles);
        let mut runs: Vec<Run> = roles
            .into_values()
            .filter_map(|role| match role {
                Role::Open(run) => Some((*run).clone()),
                Role::Hidden => None,
            })
            .collect();
        runs.sort_by_key(|run| run.id);
        runs
    }

    #[test]
    fn reads_tags_and_attributes() {
        let tag = parse_tag(r#"<small class="detail" title='a &amp; b' hidden>"#).unwrap();
        assert!(!tag.closing);
        assert_eq!(tag.name, "small");
        assert_eq!(tag.attribute("class"), Some("detail"));
        assert_eq!(tag.attribute("title"), Some("a & b"));
        assert_eq!(tag.attribute("hidden"), Some(""));
        assert!(parse_tag("</SUP>").unwrap().closing);
        assert!(parse_tag("<br/>").is_none());
        assert!(parse_tag("<!-- note -->").is_none());
    }

    #[test]
    fn reads_colors() {
        let color = |html: &str| parse_tag(html).unwrap().color();
        assert_eq!(
            color(r#"<span style="color: #c00">"#),
            Some(Color::Rgb(0xcc0000))
        );
        assert_eq!(
            color(r#"<span style="font-weight:bold; color:rgb(0, 128, 255)">"#),
            Some(Color::Rgb(0x0080ff))
        );
        assert_eq!(
            color(r#"<font color="red">"#),
            Some(Color::Theme(ThemeColor::Red))
        );
        assert_eq!(color(r#"<span style="color: nonsense">"#), None);
        assert_eq!(color("<span>"), None);
    }

    #[test]
    fn pairs_tags_with_their_text() {
        let found = runs(
            "Price <small class=\"detail\">(plus **tax**)</small>, H<sub>2</sub>O, \
             <kbd>Ctrl</kbd>+<kbd>S</kbd> and <q>hi</q>.",
        );
        let summary: Vec<(Tag, &str)> = found
            .iter()
            .map(|run| (run.tag, run.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (Tag::Small, "(plus tax)"),
                (Tag::Sub, "2"),
                (Tag::Kbd, "Ctrl"),
                (Tag::Kbd, "S"),
                (Tag::Q, "“hi”"),
            ]
        );
        let tax = 6..9;
        assert_eq!(
            found[0].styles,
            [(
                tax,
                Style {
                    bold: true,
                    ..Style::default()
                }
            )]
        );
    }

    #[test]
    fn nests_and_ignores_what_it_cannot_pair() {
        let found = runs("<small>a <small>b</small> <a href=\"x\">c</a></small> and <small>open");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "a b c");
        assert!(runs("`<small>code</small>`").is_empty());
        assert!(runs("<blink>old</blink>").is_empty());
    }

    #[test]
    fn counts_the_nodes_drawn_after_a_run() {
        // Text, `</small>`.
        assert_eq!(runs("<small>a</small>")[0].hidden, 2);
        // Text, `<b>`…`</b>` (drawn as their text), text, `</small>`.
        assert_eq!(runs("<small>a <b>b</b> c</small>")[0].hidden, 4);
        // Text, strong, `</sub>`.
        assert_eq!(runs("<sub>a **b**</sub>")[0].hidden, 3);
    }

    #[test]
    fn keeps_links_inside_runs() {
        let found = runs("<small>see [the guide](guide.md)</small>");
        assert_eq!(found[0].text, "see the guide");
        assert_eq!(found[0].links, [(4..13, "guide.md".to_string())]);
    }

    #[test]
    fn rebuilds_tags_for_html() {
        assert_eq!(
            sanitized_tag(r#"<small class="detail" onclick="evil()">"#).as_deref(),
            Some(r#"<small class="detail">"#)
        );
        assert_eq!(
            sanitized_tag(r#"<span style="color:red;background:url(x)" title='a"b'>"#).as_deref(),
            Some(r#"<span title="a&quot;b" style="color:#d1242f">"#)
        );
        assert_eq!(sanitized_tag("</SUP>").as_deref(), Some("</sup>"));
        assert_eq!(sanitized_tag("<br/>").as_deref(), Some("<br>"));
        assert_eq!(sanitized_tag("<script>"), None);
        assert_eq!(sanitized_tag("<img src=x onerror=y>"), None);
    }

    #[test]
    fn groups_inlines_for_exports() {
        let options = crate::analysis::preview_parse_options();
        let root = markdown::to_mdast("a <sup>2</sup> <b>b</b> <x>c</x>", &options).unwrap();
        let paragraph = &root.children().unwrap()[0];
        let tags: Vec<ExportTag> = group_inlines(paragraph.children().unwrap())
            .iter()
            .filter_map(|group| match group {
                Group::Tagged { tag, inner, .. } => {
                    assert_eq!(inner.len(), 1);
                    Some(*tag)
                }
                Group::Node(_) => None,
            })
            .collect();
        assert_eq!(tags, [ExportTag::Run(Tag::Sup), ExportTag::Bold]);
    }

    #[test]
    fn splits_overlapping_highlights() {
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let italic = HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..Default::default()
        };
        let merged = merge_highlights(vec![(0..6, bold), (3..9, italic)]);
        let ranges: Vec<_> = merged.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..3, 3..6, 6..9]);
        assert_eq!(merged[1].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(merged[1].1.font_style, Some(FontStyle::Italic));
    }
}
