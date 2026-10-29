//! What Live Preview draws of the Markdown being edited: the text formatted
//! as the preview formats it, with the marks that format it (`**`, `` ` ``,
//! `[…](…)`, tags) hidden except where the caret is. Line breaks inside a
//! paragraph show as the spaces they render as.
//!
//! Everything here works on the source text, by byte offset, and is pure;
//! [`Display`] maps between the source and the text drawn.

use std::ops::Range;

use markdown::mdast::Node;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::{analysis::preview_parse_options, html_inline};

/// How a piece of source is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A paragraph, a list item's text or a table cell.
    Prose,
    /// A heading of this level.
    Heading(u8),
    /// Shown as written: a code block, a quote, HTML, math.
    Raw { code: bool },
}

/// How a stretch of text is formatted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub underline: bool,
    pub code: bool,
    pub link: bool,
    pub mark: bool,
    /// Markdown syntax: shown because the caret is at it, or text that is
    /// only drawn as written here (an image, a footnote reference).
    pub syntax: bool,
}

impl Style {
    fn merge(&mut self, other: Style) {
        self.bold |= other.bold;
        self.italic |= other.italic;
        self.strike |= other.strike;
        self.underline |= other.underline;
        self.code |= other.code;
        self.link |= other.link;
        self.mark |= other.mark;
        self.syntax |= other.syntax;
    }

    fn bold() -> Self {
        Self {
            bold: true,
            ..Self::default()
        }
    }

    fn italic() -> Self {
        Self {
            italic: true,
            ..Self::default()
        }
    }

    fn strike() -> Self {
        Self {
            strike: true,
            ..Self::default()
        }
    }

    fn code() -> Self {
        Self {
            code: true,
            ..Self::default()
        }
    }

    fn link() -> Self {
        Self {
            link: true,
            ..Self::default()
        }
    }

    fn syntax() -> Self {
        Self {
            syntax: true,
            ..Self::default()
        }
    }
}

/// Source drawn as something else (often nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conceal {
    pub range: Range<usize>,
    pub replacement: &'static str,
    /// The construct the mark belongs to: the mark shows while the caret
    /// touches it. `None` for what is never shown as written (the line
    /// breaks inside a paragraph).
    pub owner: Option<Range<usize>>,
}

/// The formatting and marks of a piece of source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    pub styles: Vec<(Range<usize>, Style)>,
    pub conceals: Vec<Conceal>,
    /// Links, by the source they span, and where they lead.
    pub links: Vec<(Range<usize>, String)>,
    /// Whether it has what the preview draws as objects (images, math,
    /// inline HTML), which are drawn here as written.
    pub objects: bool,
}

impl Analysis {
    /// Where the caret can't stop, being inside text never shown as
    /// written: from `offset` (inside one) to its end, or `None`.
    fn always_hidden_at(&self, offset: usize) -> Option<&Range<usize>> {
        self.conceals
            .iter()
            .filter(|conceal| conceal.owner.is_none())
            .map(|conceal| &conceal.range)
            .find(|range| range.start < offset && offset < range.end)
    }
}

/// Find the formatting and marks of `source`.
pub fn analyze(source: &str, mode: Mode) -> Analysis {
    let mut analysis = Analysis::default();
    if matches!(mode, Mode::Raw { .. }) || source.is_empty() {
        return analysis;
    }
    let options = preview_parse_options();
    // A quote's `>` marks start its paragraph's later lines; read on its
    // own, a line starting with one would be a quote of its own. Blanked
    // out (offsets stay put), they go with the line break before them.
    let unquoted = unquote(source);
    let source = unquoted.as_str();
    let Ok(Node::Root(root)) = markdown::to_mdast(source, &options) else {
        return analysis;
    };
    let prose = |node: &Node| matches!(node, Node::Paragraph(_) | Node::Heading(_));
    if root.children.first().is_some_and(prose) || root.children.is_empty() {
        for child in &root.children {
            container(child, source, 0, &mut analysis);
        }
        return analysis;
    }
    // Text that would start another block on its own (a list item's text
    // reading `1. …`, a cell holding `# …`) is still inline here: read it
    // behind an invisible character, then shift the offsets back.
    const SHIELD: &str = "&#8203;";
    let shielded = format!("{SHIELD}{source}");
    let Ok(Node::Root(root)) = markdown::to_mdast(&shielded, &options) else {
        return analysis;
    };
    let mut shifted = Analysis::default();
    for child in &root.children {
        container(child, &shielded, SHIELD.len(), &mut shifted);
    }
    analysis.styles = shifted
        .styles
        .into_iter()
        .filter_map(|(range, style)| Some((unshield(range, SHIELD.len())?, style)))
        .collect();
    analysis.links = shifted
        .links
        .into_iter()
        .filter_map(|(range, url)| Some((unshield(range, SHIELD.len())?, url)))
        .collect();
    analysis.objects = shifted.objects;
    analysis.conceals = shifted
        .conceals
        .into_iter()
        .filter_map(|conceal| {
            Some(Conceal {
                range: unshield(conceal.range, SHIELD.len())?,
                replacement: conceal.replacement,
                owner: match conceal.owner {
                    Some(owner) => Some(unshield(owner, SHIELD.len())?),
                    None => None,
                },
            })
        })
        .collect();
    analysis
}

/// `source` with the `>` marks starting its lines after the first made
/// spaces.
fn unquote(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for (ix, line) in source.split_inclusive('\n').enumerate() {
        if ix == 0 {
            out.push_str(line);
            continue;
        }
        let marks = line.len() - line.trim_start_matches(['>', ' ', '\t']).len();
        out.extend(std::iter::repeat_n(' ', marks));
        out.push_str(&line[marks..]);
    }
    out
}

fn unshield(range: Range<usize>, shift: usize) -> Option<Range<usize>> {
    (range.start >= shift).then(|| range.start - shift..range.end - shift)
}

fn span(node: &Node) -> Option<Range<usize>> {
    let position = node.position()?;
    Some(position.start.offset..position.end.offset)
}

/// A paragraph or heading: its inline content, and a heading's `#`s.
fn container(node: &Node, source: &str, skip: usize, out: &mut Analysis) {
    let Some(range) = span(node) else {
        return;
    };
    if range.end <= skip {
        return;
    }
    let children = match node {
        Node::Paragraph(paragraph) => &paragraph.children,
        Node::Heading(heading) => {
            let inner = inner_span(&heading.children);
            let (open, close) = match &inner {
                Some(inner) => (range.start..inner.start, inner.end..range.end),
                None => (range.clone(), range.end..range.end),
            };
            for mark in [open, close] {
                if !mark.is_empty() {
                    out.conceals.push(Conceal {
                        range: mark,
                        replacement: "",
                        owner: Some(range.clone()),
                    });
                }
            }
            &heading.children
        }
        _ => return,
    };
    inlines(children, source, out);
}

/// The source the children of a node cover.
fn inner_span(children: &[Node]) -> Option<Range<usize>> {
    let first = span(children.first()?)?;
    let last = span(children.last()?)?;
    Some(first.start..last.end)
}

/// Marks before and after a construct's children, hidden unless the caret
/// touches the construct.
fn delimit(range: &Range<usize>, inner: &Range<usize>, out: &mut Analysis) {
    for mark in [range.start..inner.start, inner.end..range.end] {
        if !mark.is_empty() {
            out.conceals.push(Conceal {
                range: mark,
                replacement: "",
                owner: Some(range.clone()),
            });
        }
    }
}

fn inlines(children: &[Node], source: &str, out: &mut Analysis) {
    // Inline HTML tags come as separate nodes; pair them up here.
    let mut open_tags: Vec<(String, Range<usize>)> = Vec::new();
    for child in children {
        let Some(range) = span(child) else {
            continue;
        };
        match child {
            Node::Text(_) => soft_breaks(source, &range, out),
            Node::Strong(node) => styled(&range, &node.children, Style::bold(), source, out),
            Node::Emphasis(node) => styled(&range, &node.children, Style::italic(), source, out),
            Node::Delete(node) => styled(&range, &node.children, Style::strike(), source, out),
            Node::InlineCode(_) => inline_code(source, &range, out),
            Node::InlineMath(_) => {
                out.objects = true;
                out.styles.push((range, Style::code()))
            }
            Node::Link(link) => {
                out.links.push((range.clone(), link.url.clone()));
                link_like(source, &range, &link.children, out);
                inlines(&link.children, source, out);
            }
            Node::LinkReference(link) => {
                link_like(source, &range, &link.children, out);
                inlines(&link.children, source, out);
            }
            Node::Image(_) | Node::ImageReference(_) | Node::FootnoteReference(_) => {
                out.objects = true;
                out.styles.push((range, Style::syntax()))
            }
            Node::Break(_) => hard_break(source, &range, out),
            Node::Html(html) => {
                out.objects = true;
                let tag = html_inline::parse_tag(&html.value);
                match tag {
                    Some(tag) if tag.name == "br" => out.conceals.push(Conceal {
                        range: range.clone(),
                        replacement: "\n",
                        owner: Some(range),
                    }),
                    Some(tag) if !tag.closing && tag_style(&tag.name).is_some() => {
                        open_tags.push((tag.name, range));
                    }
                    Some(tag) if tag.closing => {
                        match open_tags.iter().rposition(|(name, _)| *name == tag.name) {
                            Some(ix) => {
                                let (name, open) = open_tags.remove(ix);
                                let owner = open.start..range.end;
                                let content = open.end..range.start;
                                for mark in [open, range] {
                                    out.conceals.push(Conceal {
                                        range: mark,
                                        replacement: "",
                                        owner: Some(owner.clone()),
                                    });
                                }
                                let style = tag_style(&name).unwrap_or_default();
                                if style != Style::default() {
                                    out.styles.push((content, style));
                                }
                            }
                            None => out.styles.push((range, Style::syntax())),
                        }
                    }
                    _ => out.styles.push((range, Style::syntax())),
                }
            }
            _ => {
                if let Some(children) = child.children() {
                    inlines(children, source, out);
                }
            }
        }
    }
    // Tags never closed are shown as written.
    for (_, range) in open_tags {
        out.styles.push((range, Style::syntax()));
    }
}

/// The formatting a paired HTML tag gives its content.
fn tag_style(name: &str) -> Option<Style> {
    Some(match name {
        "b" | "strong" => Style::bold(),
        "i" | "em" | "cite" | "var" | "dfn" => Style::italic(),
        "u" | "ins" => Style {
            underline: true,
            ..Style::default()
        },
        "s" | "del" | "strike" => Style::strike(),
        "mark" => Style {
            mark: true,
            ..Style::default()
        },
        "code" | "kbd" | "samp" | "tt" => Style::code(),
        _ if html_inline::Tag::from_name(name).is_some() => Style::default(),
        _ => return None,
    })
}

fn styled(range: &Range<usize>, children: &[Node], style: Style, source: &str, out: &mut Analysis) {
    if let Some(inner) = inner_span(children) {
        delimit(range, &inner, out);
        out.styles.push((inner, style));
    }
    inlines(children, source, out);
}

fn inline_code(source: &str, range: &Range<usize>, out: &mut Analysis) {
    let text = &source[range.clone()];
    let open = text.bytes().take_while(|byte| *byte == b'`').count();
    let close = text.bytes().rev().take_while(|byte| *byte == b'`').count();
    if open == 0 || open + close > text.len() {
        return;
    }
    let mut inner = range.start + open..range.end - close;
    // One space inside each backtick run is padding, not code.
    let body = &source[inner.clone()];
    if body.len() >= 2 && body.starts_with(' ') && body.ends_with(' ') && !body.trim().is_empty() {
        inner = inner.start + 1..inner.end - 1;
    }
    delimit(range, &inner, out);
    soft_breaks(source, &inner, out);
    out.styles.push((inner, Style::code()));
}

fn link_like(source: &str, range: &Range<usize>, children: &[Node], out: &mut Analysis) {
    let text = &source[range.clone()];
    if text.starts_with('[') {
        let inner = inner_span(children).unwrap_or(range.start + 1..range.start + 1);
        delimit(range, &inner, out);
        out.styles.push((inner, Style::link()));
    } else if text.starts_with('<') && text.ends_with('>') && text.len() >= 2 {
        let inner = range.start + 1..range.end - 1;
        delimit(range, &inner, out);
        out.styles.push((inner, Style::link()));
    } else {
        // A bare URL, linked as it stands.
        out.styles.push((range.clone(), Style::link()));
    }
}

/// Line breaks inside a paragraph render as a space; the indentation of
/// the line after one isn't drawn.
fn soft_breaks(source: &str, range: &Range<usize>, out: &mut Analysis) {
    let text = &source[range.clone()];
    for (ix, _) in text.match_indices('\n') {
        let newline = range.start + ix;
        let before = source[..newline]
            .bytes()
            .rev()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        let after = source[newline + 1..]
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        let start = (newline - before).max(range.start);
        out.conceals.push(Conceal {
            range: start..newline + 1 + after,
            replacement: " ",
            owner: None,
        });
    }
}

/// A hard break (`\` or two spaces before a line end) shows as a line
/// break; its mark only near the caret.
fn hard_break(source: &str, range: &Range<usize>, out: &mut Analysis) {
    let Some(newline) = source[range.clone()].find('\n').map(|ix| range.start + ix) else {
        return;
    };
    if newline > range.start {
        out.conceals.push(Conceal {
            range: range.start..newline,
            replacement: "",
            owner: Some(range.clone()),
        });
    }
    let indent = source[newline + 1..]
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    if indent > 0 {
        out.conceals.push(Conceal {
            range: newline + 1..newline + 1 + indent,
            replacement: "",
            owner: None,
        });
    }
}

// -------------------------------------------------------------------------
// The text drawn

/// The text drawn for a piece of source, and how its offsets map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Display {
    pub text: String,
    /// Source ranges and the text drawn for each, in order, covering the
    /// whole source.
    segments: Vec<Segment>,
    /// The formatting of the text drawn, in order, without overlaps.
    pub styles: Vec<(Range<usize>, Style)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    source: Range<usize>,
    shown: Range<usize>,
    /// Drawn as written (rather than replaced).
    copied: bool,
}

/// Whether a selection touches a construct, showing its marks.
fn touches(owner: &Range<usize>, selection: &Range<usize>) -> bool {
    selection.start <= owner.end && selection.end >= owner.start
}

impl Display {
    /// The text drawn for `source` with `selection` (`None` when it isn't
    /// being edited, hiding every mark).
    pub fn new(source: &str, analysis: &Analysis, selection: Option<&Range<usize>>) -> Self {
        let mut hidden: Vec<&Conceal> = analysis
            .conceals
            .iter()
            .filter(|conceal| match (&conceal.owner, selection) {
                (None, _) | (Some(_), None) => true,
                (Some(owner), Some(selection)) => !touches(owner, selection),
            })
            .collect();
        hidden.sort_by_key(|conceal| (conceal.range.start, conceal.range.end));

        let mut display = Display::default();
        let mut at = 0;
        for conceal in hidden {
            let range = conceal.range.start.min(source.len())..conceal.range.end.min(source.len());
            if range.start < at {
                // Overlaps one already hidden.
                continue;
            }
            display.copy(source, at..range.start);
            let shown = display.text.len();
            display.text.push_str(conceal.replacement);
            display.segments.push(Segment {
                source: range.clone(),
                shown: shown..display.text.len(),
                copied: false,
            });
            at = range.end;
        }
        display.copy(source, at..source.len());

        // Formatting, with the marks shown near the caret dimmed.
        let mut styles: Vec<(Range<usize>, Style)> = analysis.styles.clone();
        if let Some(selection) = selection {
            for conceal in &analysis.conceals {
                if let Some(owner) = &conceal.owner
                    && touches(owner, selection)
                {
                    styles.push((conceal.range.clone(), Style::syntax()));
                }
            }
        }
        let shown: Vec<(Range<usize>, Style)> = styles
            .into_iter()
            .map(|(range, style)| {
                (
                    display.to_display(range.start)..display.to_display(range.end),
                    style,
                )
            })
            .filter(|(range, _)| !range.is_empty())
            .collect();
        display.styles = flatten(&shown, display.text.len());
        display
    }

    fn copy(&mut self, source: &str, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let shown = self.text.len();
        self.text.push_str(&source[range.clone()]);
        self.segments.push(Segment {
            source: range,
            shown: shown..self.text.len(),
            copied: true,
        });
    }

    /// Where a source offset is drawn.
    pub fn to_display(&self, offset: usize) -> usize {
        for segment in &self.segments {
            if offset < segment.source.end
                || (offset == segment.source.end
                    && segment.copied
                    && offset == segment.source.start)
            {
                if offset <= segment.source.start {
                    return segment.shown.start;
                }
                return if segment.copied {
                    segment.shown.start + (offset - segment.source.start)
                } else {
                    segment.shown.end
                };
            }
        }
        self.text.len()
    }

    /// The source offset of a place in the text drawn. Where hidden marks
    /// sit at that place, the offset before them.
    pub fn to_source(&self, offset: usize) -> usize {
        for segment in &self.segments {
            if offset <= segment.shown.end && (segment.copied || offset == segment.shown.start) {
                if !segment.copied {
                    return segment.source.start;
                }
                if offset >= segment.shown.start {
                    return segment.source.start + (offset - segment.shown.start);
                }
            }
            if !segment.copied && offset > segment.shown.start && offset <= segment.shown.end {
                return segment.source.end;
            }
        }
        self.segments.last().map_or(0, |segment| segment.source.end)
    }
}

/// Split overlapping styles into consecutive ranges, each with the
/// formatting of every style covering it.
fn flatten(styles: &[(Range<usize>, Style)], len: usize) -> Vec<(Range<usize>, Style)> {
    let mut bounds: Vec<usize> = styles
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .chain([0, len])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut out: Vec<(Range<usize>, Style)> = Vec::new();
    for pair in bounds.windows(2) {
        let range = pair[0]..pair[1];
        let mut style = Style::default();
        for (covering, part) in styles {
            if covering.start <= range.start && range.end <= covering.end {
                style.merge(*part);
            }
        }
        if style == Style::default() {
            continue;
        }
        match out.last_mut() {
            Some((last, last_style)) if last.end == range.start && *last_style == style => {
                last.end = range.end;
            }
            _ => out.push((range, style)),
        }
    }
    out
}

// -------------------------------------------------------------------------
// Moving through the source

/// The place one character after `offset`, past text never shown as
/// written.
pub fn next_offset(source: &str, analysis: &Analysis, offset: usize) -> usize {
    let next = source[offset.min(source.len())..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(source.len(), |(ix, _)| offset + ix);
    match analysis.always_hidden_at(next) {
        Some(range) => range.end,
        None => next,
    }
}

/// The place one character before `offset`.
pub fn previous_offset(source: &str, analysis: &Analysis, offset: usize) -> usize {
    let previous = source[..offset.min(source.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(ix, _)| ix);
    match analysis.always_hidden_at(previous) {
        Some(range) => range.start,
        None => previous,
    }
}

/// The start of the word before `offset` (skipping spaces first).
pub fn previous_word(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    source[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, word)| !word.trim().is_empty())
        .map_or(0, |(ix, _)| ix)
}

/// The end of the word after `offset` (skipping spaces first).
pub fn next_word(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    source[offset..]
        .split_word_bound_indices()
        .find(|(_, word)| !word.trim().is_empty())
        .map_or(source.len(), |(ix, word)| offset + ix + word.len())
}

/// The word around `offset`, for a double click.
pub fn word_at(source: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(source.len());
    for (ix, word) in source.split_word_bound_indices() {
        let range = ix..ix + word.len();
        if range.start <= offset && offset < range.end
            || (offset == source.len() && range.end == offset)
        {
            return range;
        }
    }
    offset..offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(source: &str, caret: Option<usize>) -> String {
        let analysis = analyze(source, Mode::Prose);
        let selection = caret.map(|caret| caret..caret);
        Display::new(source, &analysis, selection.as_ref()).text
    }

    #[test]
    fn hides_marks_away_from_the_caret() {
        let source = "A **bold** and [link](https://x.y) `code`.";
        assert_eq!(shown(source, None), "A bold and link code.");
        // In the bold word: its marks show, the others stay hidden.
        assert_eq!(shown(source, Some(5)), "A **bold** and link code.");
        assert_eq!(
            shown(source, Some(18)),
            "A bold and [link](https://x.y) code."
        );
        assert_eq!(shown(source, Some(36)), "A bold and link `code`.");
    }

    #[test]
    fn joins_lines_of_a_paragraph() {
        let source = "**UI Builder.** A workspace\n  and script files ([doc](a.md)).";
        assert_eq!(
            shown(source, None),
            "UI Builder. A workspace and script files (doc)."
        );
        // A quote's later lines join the same way.
        assert_eq!(
            shown("A *quote*\n> on two lines.", None),
            "A quote on two lines."
        );
        // A hard break stays a line break.
        assert_eq!(shown("one\\\n  two", None), "one\ntwo");
        assert_eq!(shown("one\\\ntwo", Some(4)), "one\\\ntwo");
    }

    #[test]
    fn formats_what_it_hides() {
        let source = "x **b** *i* ~~s~~ `c` <kbd>K</kbd>";
        let analysis = analyze(source, Mode::Prose);
        let display = Display::new(source, &analysis, None);
        assert_eq!(display.text, "x b i s c K");
        let style_of = |text: &str| {
            let at = display.text.find(text).unwrap();
            display
                .styles
                .iter()
                .find(|(range, _)| range.start <= at && at < range.end)
                .map(|(_, style)| *style)
                .unwrap_or_default()
        };
        assert!(style_of("b").bold);
        assert!(style_of("i").italic);
        assert!(style_of("s").strike);
        assert!(style_of("c").code);
        assert!(style_of("K").code);
    }

    #[test]
    fn reads_headings_and_odd_starts() {
        let analysis = analyze("## Title", Mode::Heading(2));
        assert_eq!(Display::new("## Title", &analysis, None).text, "Title");
        assert_eq!(
            Display::new("## Title", &analysis, Some(&(4..4))).text,
            "## Title"
        );
        // Text that would be a list on its own line is still inline text.
        assert_eq!(shown("1986. A **good** year", None), "1986. A good year");
    }

    #[test]
    fn maps_offsets_both_ways() {
        let source = "a **b** c";
        let analysis = analyze(source, Mode::Prose);
        let display = Display::new(source, &analysis, None);
        assert_eq!(display.text, "a b c");
        assert_eq!(display.to_display(0), 0);
        assert_eq!(display.to_display(4), 2);
        assert_eq!(display.to_display(8), 4);
        assert_eq!(display.to_source(2), 2);
        assert_eq!(display.to_source(3), 5);
        assert_eq!(display.to_source(5), 9);
        assert_eq!(display.to_display(source.len()), display.text.len());
    }

    #[test]
    fn steps_over_joined_lines() {
        let source = "one\n   two";
        let analysis = analyze(source, Mode::Prose);
        assert_eq!(next_offset(source, &analysis, 3), 7);
        assert_eq!(previous_offset(source, &analysis, 7), 3);
        assert_eq!(next_offset(source, &analysis, 0), 1);
        assert_eq!(next_word("hello big world", 5), 9);
        assert_eq!(previous_word("hello big world", 10), 6);
        assert_eq!(word_at("hello big world", 7), 6..9);
    }
}
