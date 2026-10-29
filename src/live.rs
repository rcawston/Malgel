//! Live Preview: the formatted document, edited where it stands.
//!
//! The preview renders the whole document. The paragraph, heading, list
//! item or table cell holding the caret is drawn by an editor of its own
//! ([`LiveEditor`]) in the place it renders in, formatted the same way, with
//! its Markdown marks showing only around the caret. Other blocks (code,
//! callouts, HTML) are edited as written. The document's own editor stays
//! the source of truth: each change is applied to it, so saving, undo,
//! recovery and the outline work as ever.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, Mutex, OnceLock},
};

use gpui_kit::{
    App, Entity, EntityId, FontWeight, Global, IntoElement, ParentElement as _, Pixels,
    SharedString, Styled as _, WeakEntity, Window,
    base::TextViewState,
    component::{
        ActiveTheme as _,
        text::{
            MarkdownExtensions, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            markdown_ast::Node,
        },
    },
    div, px, relative,
};

use crate::{
    analysis::preview_parse_options,
    html_block::{PreviewEnv, nested_view},
    live_editor::LiveEditor,
    live_inline::Mode,
    live_table::{self, TableModel},
};

/// The top-level blocks of `source`, as byte ranges, without the line
/// break a list followed by a blank line ends in.
pub fn blocks(source: &str) -> Vec<Range<usize>> {
    let Ok(Node::Root(root)) = markdown::to_mdast(source, &preview_parse_options()) else {
        return Vec::new();
    };
    root.children
        .iter()
        .filter_map(|node| {
            let position = node.position()?;
            let text = &source[position.start.offset..position.end.offset];
            let end = position.start.offset + text.trim_end_matches(['\n', '\r']).len();
            Some(position.start.offset..end)
        })
        .collect()
}

// -------------------------------------------------------------------------
// What can be edited

/// What the editor stands in for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A paragraph, at the top level or in a list item or quote.
    Paragraph,
    Heading(u8),
    /// A table cell: the table, and the cell's row (the header is 0) and
    /// column.
    Cell {
        table: Range<usize>,
        row: usize,
        col: usize,
    },
    /// A block edited as written: code, a callout, HTML, math.
    Raw {
        code: bool,
    },
    /// A place with nothing in it yet inside `block`, a list (an empty
    /// item) or a quote (an empty line).
    Empty {
        block: Range<usize>,
    },
    /// A blank line, after (or, at the top, before) `block`.
    Blank {
        block: Range<usize>,
        after: bool,
    },
}

/// A part of the document the editor can stand in for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editable {
    /// The source it edits.
    pub range: Range<usize>,
    pub target: Target,
    /// What its continuation lines start with: a list item's indentation,
    /// a quote's `> `.
    pub line_prefix: String,
}

impl Editable {
    pub fn mode(&self) -> Mode {
        match &self.target {
            Target::Heading(level) => Mode::Heading(*level),
            Target::Raw { code } => Mode::Raw { code: *code },
            _ => Mode::Prose,
        }
    }

    /// Whether `other` is the same thing, as text is typed into this one.
    pub fn same_place(&self, other: &Editable) -> bool {
        self.range.start == other.range.start
            && match (&self.target, &other.target) {
                (Target::Cell { row, col, .. }, Target::Cell { row: r, col: c, .. }) => {
                    row == r && col == c
                }
                (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
            }
    }
}

fn span(node: &Node) -> Option<Range<usize>> {
    let position = node.position()?;
    Some(position.start.offset..position.end.offset)
}

fn line_start(source: &str, offset: usize) -> usize {
    source[..offset].rfind('\n').map_or(0, |ix| ix + 1)
}

fn line_end(source: &str, offset: usize) -> usize {
    source[offset..]
        .find('\n')
        .map_or(source.len(), |ix| offset + ix)
}

/// `range` taken on to the end of its last line when only spaces follow,
/// so a space typed at the end of a paragraph stays in it.
fn to_line_end(source: &str, range: Range<usize>) -> Range<usize> {
    let end = line_end(source, range.end);
    if source[range.end..end].trim().is_empty() {
        range.start..end
    } else {
        range
    }
}

/// What lines continuing the text at `start` begin with: the line's `>`
/// marks, and spaces for the rest (a list marker's width).
fn line_prefix(source: &str, start: usize) -> String {
    source[line_start(source, start)..start]
        .chars()
        .map(|ch| if ch == '>' { '>' } else { ' ' })
        .collect()
}

/// Whether a quote is a GitHub alert, drawn by its own plugin.
fn is_alert(source: &str, range: &Range<usize>) -> bool {
    let first = &source[range.start..line_end(source, range.start)];
    first.trim_start_matches(['>', ' ']).starts_with("[!")
}

/// The parts of `source` the editor can stand in for, in order.
pub fn editables(source: &str) -> Vec<Editable> {
    let mut out = Vec::new();
    if let Ok(root) = markdown::to_mdast(source, &preview_parse_options()) {
        collect(&root, source, &mut out);
    }
    out
}

fn collect(node: &Node, source: &str, out: &mut Vec<Editable>) {
    let Some(range) = span(node) else {
        if let Node::Root(root) = node {
            for child in &root.children {
                collect(child, source, out);
            }
        }
        return;
    };
    match node {
        Node::Root(root) => {
            for child in &root.children {
                collect(child, source, out);
            }
        }
        Node::Paragraph(_) => out.push(Editable {
            range: to_line_end(source, range.clone()),
            target: Target::Paragraph,
            line_prefix: line_prefix(source, range.start),
        }),
        Node::Heading(heading) => out.push(Editable {
            range: to_line_end(source, range),
            target: Target::Heading(heading.depth),
            line_prefix: String::new(),
        }),
        Node::List(list) => {
            for item in &list.children {
                match item {
                    Node::ListItem(item_node) if item_node.children.is_empty() => {
                        if let Some(item_range) = span(item) {
                            let at = empty_item_text(source, &item_range);
                            out.push(Editable {
                                range: at..at,
                                target: Target::Empty {
                                    block: range.clone(),
                                },
                                line_prefix: line_prefix(source, at),
                            });
                        }
                    }
                    Node::ListItem(item_node) => {
                        // A task box with nothing after it yet reads as
                        // text; it's an empty task item, the text to come
                        // after the box.
                        if let [paragraph @ Node::Paragraph(_)] = item_node.children.as_slice()
                            && let Some(text) = span(paragraph)
                            && matches!(source[text.clone()].trim(), "[ ]" | "[x]" | "[X]")
                        {
                            let at = line_end(source, text.start);
                            out.push(Editable {
                                range: at..at,
                                target: Target::Empty {
                                    block: range.clone(),
                                },
                                line_prefix: line_prefix(source, text.start),
                            });
                            continue;
                        }
                        for child in &item_node.children {
                            collect(child, source, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        Node::Blockquote(quote) if !is_alert(source, &range) => {
            for child in &quote.children {
                collect(child, source, out);
            }
        }
        Node::Table(table) => {
            for (row_ix, row) in table.children.iter().enumerate() {
                let Some(cells) = row.children() else {
                    continue;
                };
                for (col, cell) in cells.iter().enumerate() {
                    out.push(Editable {
                        range: cell_range(source, cell),
                        target: Target::Cell {
                            table: range.clone(),
                            row: row_ix,
                            col,
                        },
                        line_prefix: String::new(),
                    });
                }
            }
        }
        _ => out.push(Editable {
            range: range.clone(),
            target: Target::Raw {
                code: matches!(
                    node,
                    Node::Code(_) | Node::Math(_) | Node::Yaml(_) | Node::Toml(_)
                ),
            },
            line_prefix: line_prefix(source, range.start),
        }),
    }
}

/// Where an empty list item's text goes: after its marker, the space after
/// it and any task box.
fn empty_item_text(source: &str, item: &Range<usize>) -> usize {
    let end = line_end(source, item.start);
    let text = &source[item.start..end];
    let bytes = text.as_bytes();
    let mut ix = match bytes.first() {
        Some(b'-' | b'*' | b'+') => 1,
        _ => {
            let digits = bytes
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            digits + usize::from(matches!(bytes.get(digits), Some(b'.' | b')')))
        }
    };
    if matches!(bytes.get(ix), Some(b' ' | b'\t')) {
        ix += 1;
    }
    for task in ["[ ] ", "[x] ", "[X] "] {
        if text[ix..].starts_with(task) {
            ix += task.len();
        }
    }
    item.start + ix.min(text.len())
}

/// The text of a table cell: between its pipes, less the space padding
/// them. An empty cell's place is just inside its first pipe and space.
pub fn cell_range(source: &str, cell: &Node) -> Range<usize> {
    let Some(range) = span(cell) else {
        return 0..0;
    };
    if let Some(children) = cell.children()
        && let (Some(first), Some(last)) = (
            children.first().and_then(span),
            children.last().and_then(span),
        )
    {
        return first.start..last.end;
    }
    let text = &source[range.clone()];
    let start = range.start + usize::from(text.starts_with('|'));
    let inner_end = range.end - usize::from(text.len() > 1 && text.ends_with('|'));
    let at = (start + 1).min(inner_end.max(start));
    at..at
}

/// The part of `source` to edit for a caret at `offset`.
pub fn editable_at(source: &str, offset: usize) -> Editable {
    let offset = offset.min(source.len());
    let all = editables(source);
    if let Some(editable) = all
        .iter()
        .find(|editable| editable.range.start <= offset && offset <= editable.range.end)
    {
        return editable.clone();
    }
    let blocks = blocks(source);
    if let Some(block) = blocks
        .iter()
        .find(|block| block.start <= offset && offset <= block.end)
    {
        // An empty line in a quote: a paragraph to come.
        let line = line_start(source, offset)..line_end(source, offset);
        if source[block.clone()].starts_with('>')
            && !is_alert(source, block)
            && source[line.clone()]
                .trim_matches(['>', ' ', '\t'])
                .is_empty()
        {
            return Editable {
                range: line.end..line.end,
                target: Target::Empty {
                    block: block.clone(),
                },
                line_prefix: line_prefix(source, line.end),
            };
        }
        // Among a block's marks (a list marker, a table's pipes, a quote's
        // `>`): the nearest part of it.
        let distance = |range: &Range<usize>| {
            if offset < range.start {
                range.start - offset
            } else {
                offset.saturating_sub(range.end)
            }
        };
        if let Some(editable) = all
            .iter()
            .filter(|editable| {
                block.start <= editable.range.start && editable.range.end <= block.end
            })
            .min_by_key(|editable| distance(&editable.range))
        {
            return editable.clone();
        }
    }
    // A blank line, between blocks or around them.
    let line = line_start(source, offset)..line_end(source, offset);
    let before = blocks.iter().rev().find(|block| block.end <= line.start);
    let after = blocks.iter().find(|block| block.start >= line.end);
    let (range, target) = match (before, after) {
        (Some(block), _) => (
            line,
            Target::Blank {
                block: block.clone(),
                after: true,
            },
        ),
        (None, Some(block)) => (
            line,
            Target::Blank {
                block: block.clone(),
                after: false,
            },
        ),
        // Nothing written yet.
        (None, None) => (
            0..source.len(),
            Target::Blank {
                block: 0..0,
                after: true,
            },
        ),
    };
    Editable {
        range,
        target,
        line_prefix: String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// The part to edit next, moving from `current` by an arrow key past its
/// edge.
pub fn neighbor(source: &str, current: &Editable, direction: Direction) -> Option<Editable> {
    let all = editables(source);
    if let Target::Cell { table, row, col } = &current.target
        && matches!(direction, Direction::Up | Direction::Down)
    {
        // Up and down a table's column, then out of the table.
        let row = match direction {
            Direction::Up => row.checked_sub(1),
            _ => Some(row + 1),
        };
        let in_column = row.and_then(|row| {
            all.iter().find(|editable| {
                matches!(&editable.target, Target::Cell { table: other, row: r, col: c }
                    if other.start == table.start && *r == row && c == col)
            })
        });
        if let Some(editable) = in_column {
            return Some(editable.clone());
        }
        return match direction {
            Direction::Up => all
                .iter()
                .rev()
                .find(|editable| editable.range.end <= table.start)
                .cloned(),
            _ => all
                .iter()
                .find(|editable| editable.range.start >= table.end)
                .cloned(),
        };
    }
    match direction {
        Direction::Up | Direction::Left => all
            .iter()
            .rev()
            .find(|editable| {
                editable.range.end <= current.range.start && editable.range != current.range
            })
            .cloned(),
        Direction::Down | Direction::Right => all
            .iter()
            .find(|editable| {
                editable.range.start >= current.range.end && editable.range != current.range
            })
            .cloned(),
    }
}

/// The smallest replacement turning `old` into `new`: the range of `old`
/// to replace, and what replaces it.
pub fn diff<'a>(old: &str, new: &'a str) -> (Range<usize>, &'a str) {
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or(old.len().min(new.len()), |((ix, _), _)| ix);
    let prefix = floor_boundary(new, floor_boundary(old, prefix));
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    let mut suffix = 0;
    for (a, b) in old.bytes().rev().zip(new.bytes().rev()).take(max_suffix) {
        if a != b {
            break;
        }
        suffix += 1;
    }
    // Back off to a character boundary in both.
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }
    (prefix..old.len() - suffix, &new[prefix..new.len() - suffix])
}

fn floor_boundary(text: &str, mut ix: usize) -> usize {
    ix = ix.min(text.len());
    while !text.is_char_boundary(ix) {
        ix -= 1;
    }
    ix
}

/// A byte range of `text` in UTF-16 code units, as platform text input
/// counts.
pub fn utf16_range(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = text[..range.start].encode_utf16().count();
    start..start + text[range.clone()].encode_utf16().count()
}

// -------------------------------------------------------------------------
// The preview plugin

/// What the preview needs to know of a Live Preview session: the part of
/// the document being edited.
#[derive(Default)]
pub struct LiveState {
    editable: Mutex<Option<Editable>>,
}

impl LiveState {
    pub fn set(&self, editable: Option<Editable>) {
        *self.editable.lock().expect("live state poisoned") = editable;
    }

    pub fn get(&self) -> Option<Editable> {
        self.editable.lock().expect("live state poisoned").clone()
    }
}

/// The editors of the previews showing Live Preview, by preview. Plugins
/// render on the main thread, where the editor entity lives.
#[derive(Default)]
pub struct LiveEditors(HashMap<EntityId, WeakEntity<LiveEditor>>);

impl Global for LiveEditors {}

pub fn register_editor(preview: EntityId, editor: &Entity<LiveEditor>, cx: &mut App) {
    let editors = &mut cx.default_global::<LiveEditors>().0;
    // Forget the editors of documents since closed.
    editors.retain(|_, editor| editor.upgrade().is_some());
    editors.insert(preview, editor.downgrade());
}

fn editor_for(preview: &WeakEntity<TextViewState>, cx: &App) -> Option<Entity<LiveEditor>> {
    let preview = preview.upgrade()?;
    cx.try_global::<LiveEditors>()?
        .0
        .get(&preview.entity_id())?
        .upgrade()
}

/// The block before or after a heading, which sets the space around it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Neighbor {
    None,
    Heading(u8),
    Rule,
    Other,
}

/// What the plugin keeps on a node it draws.
#[derive(Debug, Clone)]
enum Part {
    /// The editor, standing in for a paragraph.
    Paragraph,
    /// The editor, standing in for a heading between these blocks.
    Heading {
        level: u8,
        before: Neighbor,
        after: Neighbor,
    },
    /// The editor, with a block as written.
    Raw {
        code: bool,
    },
    Table(TableModel),
    /// A block drawn by a view of its own with an empty paragraph the editor
    /// stands in for: an empty list item, or a blank line after the block.
    Nested {
        markdown: SharedString,
        key: usize,
    },
}

/// A paragraph holding only this is where the editor goes in a nested view.
pub const PLACEHOLDER: &str = "\u{200B}";

pub struct LiveBlockPlugin {
    state: Arc<LiveState>,
    preview: WeakEntity<TextViewState>,
    /// The extensions a nested view renders with.
    nested: Arc<OnceLock<MarkdownExtensions>>,
    env: Arc<PreviewEnv>,
}

impl LiveBlockPlugin {
    pub fn new(
        state: Arc<LiveState>,
        preview: WeakEntity<TextViewState>,
        nested: Arc<OnceLock<MarkdownExtensions>>,
        env: Arc<PreviewEnv>,
    ) -> Self {
        Self {
            state,
            preview,
            nested,
            env,
        }
    }
}

fn neighbor_kind(node: Option<&Node>) -> Neighbor {
    match node {
        None => Neighbor::None,
        Some(Node::Heading(heading)) => Neighbor::Heading(heading.depth),
        Some(Node::ThematicBreak(_)) => Neighbor::Rule,
        Some(_) => Neighbor::Other,
    }
}

/// The blocks before and after the top-level block at `start`.
fn neighbors(source: &str, start: usize) -> (Neighbor, Neighbor) {
    let Ok(Node::Root(root)) = markdown::to_mdast(source, &preview_parse_options()) else {
        return (Neighbor::None, Neighbor::None);
    };
    let Some(ix) = root
        .children
        .iter()
        .position(|node| span(node).is_some_and(|range| range.start == start))
    else {
        return (Neighbor::Other, Neighbor::Other);
    };
    (
        neighbor_kind(ix.checked_sub(1).and_then(|ix| root.children.get(ix))),
        neighbor_kind(root.children.get(ix + 1)),
    )
}

/// `markdown` with `text` put in at `at` (rebased to `base`), and the
/// lines dedented by the first's indentation, so a nested list reads as a
/// list on its own.
fn with_placeholder(source: &str, block: &Range<usize>, at: usize) -> String {
    let indent = block.start - line_start(source, block.start);
    let mut text = String::new();
    text.push_str(&source[block.start..at]);
    // A marker typed without its space yet (`-`) needs one to be a marker.
    if !text.ends_with([' ', '\t', '\n']) {
        text.push(' ');
    }
    text.push_str(PLACEHOLDER);
    text.push_str(&source[at..block.end]);
    let pad = " ".repeat(indent);
    text.lines()
        .map(|line| line.strip_prefix(pad.as_str()).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

impl MarkdownPlugin for LiveBlockPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "live-block"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let editable = self.state.get()?;
        let position = node.position()?;
        let range = cx.offset() + position.start.offset..cx.offset() + position.end.offset;
        let source = cx.source();
        let local = |range: &Range<usize>| -> Option<Range<usize>> {
            Some(range.start.checked_sub(cx.offset())?..range.end.checked_sub(cx.offset())?)
        };
        let overlaps = range.start <= editable.range.end && range.end >= editable.range.start;
        let container = matches!(node, Node::Root(_) | Node::List(_) | Node::ListItem(_));
        let part = match (&editable.target, node) {
            (Target::Paragraph, Node::Paragraph(_)) if overlaps => Part::Paragraph,
            (Target::Heading(level), Node::Heading(_)) if overlaps => {
                let (before, after) = neighbors(source, range.start - cx.offset());
                Part::Heading {
                    level: *level,
                    before,
                    after,
                }
            }
            (Target::Raw { code }, _) if range.start == editable.range.start && !container => {
                Part::Raw { code: *code }
            }
            (Target::Cell { table, .. }, Node::Table(_)) if range.start == table.start => {
                Part::Table(live_table::model(node, source, cx.offset(), &editable)?)
            }
            (Target::Empty { block: list }, Node::List(_) | Node::Blockquote(_))
                if range.start == list.start =>
            {
                let block = local(&range)?;
                let at = editable.range.start.checked_sub(cx.offset())?;
                Part::Nested {
                    markdown: with_placeholder(source, &block, at).into(),
                    key: range.start,
                }
            }
            (Target::Blank { block, after }, _)
                if range.start == block.start && !matches!(node, Node::Root(_)) =>
            {
                let block = local(&range)?;
                let text = source.get(block.clone())?.trim_end();
                let markdown = if *after {
                    format!("{text}\n\n{PLACEHOLDER}")
                } else {
                    format!("{PLACEHOLDER}\n\n{text}")
                };
                Part::Nested {
                    markdown: markdown.into(),
                    key: range.start,
                }
            }
            _ => return None,
        };
        let text = cx.node_source(node).unwrap_or_default().to_string();
        Some(MarkdownNode::new("live-block", part).markdown(text))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (Some(part), Some(editor)) = (node.data::<Part>(), editor_for(&self.preview, cx))
        else {
            return div().into_any_element();
        };
        match part {
            Part::Paragraph => div().w_full().child(editor).into_any_element(),
            Part::Heading {
                level,
                before,
                after,
            } => heading(editor, *level, *before, *after, window).into_any_element(),
            Part::Raw { code: true } => {
                let theme = cx.theme();
                div()
                    .w_full()
                    .px_4()
                    .py_3()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_size(window.rem_size() * 0.875)
                    .child(editor)
                    .into_any_element()
            }
            Part::Raw { code: false } => div().w_full().child(editor).into_any_element(),
            Part::Table(model) => live_table::render(model, &editor, window, cx),
            Part::Nested { markdown, key } => {
                let Some(extensions) = self.nested.get() else {
                    return div().into_any_element();
                };
                nested_view(
                    ("live-nested", *key),
                    markdown.clone(),
                    extensions.clone(),
                    &self.env,
                )
                .into_any_element()
            }
        }
    }
}

/// A heading's size, line height and the gap above it, relative to the
/// text, as GPUI Kit sets them.
fn heading_metrics(level: u8) -> (f32, f32, f32) {
    match level {
        1 => (1.8, 1.25, 1.1),
        2 => (4. / 3., 1.35, 1.6),
        3 => (17. / 15., 1.35, 1.4),
        4 => (1., 1.35, 1.4),
        5 => (14. / 15., 1.35, 1.4),
        _ => (13. / 15., 1.35, 1.4),
    }
}

/// The editor standing in for a heading, spaced as GPUI Kit spaces the
/// heading. The preview takes it for a block of another kind, so the space
/// it leaves around it is taken back.
fn heading(
    editor: Entity<LiveEditor>,
    level: u8,
    before: Neighbor,
    after: Neighbor,
    window: &Window,
) -> impl IntoElement {
    let body = window.text_style().font_size.to_pixels(window.rem_size());
    // GPUI Kit's paragraph gap.
    let gap = window.rem_size() * 0.75;
    let rule = body * 1.6;
    let (scale, line_height, section) = heading_metrics(level);
    let size = body * scale;
    let top = match before {
        Neighbor::None => px(0.),
        Neighbor::Heading(_) => size * 0.25,
        Neighbor::Rule => (size * section - rule).max(px(0.)),
        Neighbor::Other => (size * section - gap).max(px(0.)),
    };
    let last = after == Neighbor::None;
    let bottom = if last { px(0.) } else { size * 0.35 };
    let given: Pixels = if last { px(0.) } else { gap };
    // What the block after adds above itself, following a heading rather
    // than another block.
    let extra = match after {
        Neighbor::Heading(next) => {
            let (scale, _, section) = heading_metrics(next);
            let next_size = body * scale;
            (next_size * section - gap).max(px(0.)) - next_size * 0.25
        }
        Neighbor::Rule => (rule - gap).max(px(0.)) - (rule - size * 0.35).max(px(0.)),
        _ => px(0.),
    };
    div()
        .w_full()
        .pt(top)
        .mb(bottom - given - extra)
        .text_size(size)
        .line_height(relative(line_height))
        .font_weight(FontWeight::SEMIBOLD)
        .child(editor)
}

/// Draws the editor in the empty paragraph of a nested view.
pub struct PlaceholderPlugin {
    preview: WeakEntity<TextViewState>,
}

impl PlaceholderPlugin {
    pub fn new(preview: WeakEntity<TextViewState>) -> Self {
        Self { preview }
    }
}

impl MarkdownPlugin for PlaceholderPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "live-placeholder"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Paragraph(_) = node else {
            return None;
        };
        let text = cx.node_source(node)?;
        (text.trim() == PLACEHOLDER).then(|| MarkdownNode::new("live-placeholder", ()))
    }

    fn render(&self, _: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        match editor_for(&self.preview, cx) {
            Some(editor) => div().w_full().child(editor).into_any_element(),
            None => div().into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Title\n\nFirst paragraph\nstill first.\n\n- a **b**\n- [ ] task\n  more\n-\n\n| A | B |\n|---|---|\n| x |   |\n\n```\ncode\n```\n";

    fn text_at(offset: usize) -> (String, Target) {
        let editable = editable_at(DOC, offset);
        (DOC[editable.range.clone()].to_string(), editable.target)
    }

    #[test]
    fn finds_what_to_edit() {
        assert_eq!(text_at(3), ("# Title".into(), Target::Heading(1)));
        assert_eq!(
            text_at(DOC.find("still").unwrap()).0,
            "First paragraph\nstill first."
        );
        // A list item's own text, not the whole list.
        assert_eq!(text_at(DOC.find("a **b**").unwrap() + 1).0, "a **b**");
        assert_eq!(text_at(DOC.find("task").unwrap()).0, "task\n  more");
        // On a list marker: the item's text.
        assert_eq!(text_at(DOC.find("- a").unwrap()).0, "a **b**");
        // An empty item: its place after the marker.
        let empty = DOC.find("\n-\n").unwrap() + 2;
        let (text, target) = text_at(empty);
        assert_eq!(text, "");
        assert!(matches!(target, Target::Empty { .. }));
        // Table cells, an empty one just inside its pipe.
        let (text, target) = text_at(DOC.find("| x").unwrap() + 2);
        assert_eq!(text, "x");
        assert!(matches!(target, Target::Cell { row: 1, col: 0, .. }));
        let empty_cell = editable_at(DOC, DOC.find("|   |").unwrap() + 2);
        assert!(matches!(
            empty_cell.target,
            Target::Cell { row: 1, col: 1, .. }
        ));
        assert!(empty_cell.range.is_empty());
        // Code as written.
        assert_eq!(
            text_at(DOC.find("code").unwrap()),
            ("```\ncode\n```".into(), Target::Raw { code: true })
        );
        // A blank line after a block.
        let (text, target) = text_at(DOC.find("\n\nFirst").unwrap() + 1);
        assert_eq!(text, "");
        assert!(matches!(target, Target::Blank { after: true, .. }));
    }

    #[test]
    fn moves_between_parts() {
        let item = editable_at(DOC, DOC.find("a **b**").unwrap());
        let next = neighbor(DOC, &item, Direction::Down).unwrap();
        assert_eq!(&DOC[next.range.clone()], "task\n  more");
        let previous = neighbor(DOC, &item, Direction::Up).unwrap();
        assert_eq!(
            &DOC[previous.range.clone()],
            "First paragraph\nstill first."
        );
        // Down a table's column, then out of it.
        let header = editable_at(DOC, DOC.find("| B").unwrap() + 2);
        let below = neighbor(DOC, &header, Direction::Down).unwrap();
        assert!(matches!(below.target, Target::Cell { row: 1, col: 1, .. }));
        let out = neighbor(DOC, &below, Direction::Down).unwrap();
        assert_eq!(out.target, Target::Raw { code: true });
        let left = neighbor(DOC, &header, Direction::Left).unwrap();
        assert!(matches!(left.target, Target::Cell { row: 0, col: 0, .. }));
    }

    #[test]
    fn finds_empty_quote_lines() {
        let quoted = "> one\n>\n> \n\nafter";
        let editable = editable_at(quoted, quoted.find("> \n").unwrap() + 2);
        assert!(matches!(editable.target, Target::Empty { .. }));
        assert_eq!(editable.range, 10..10);
    }

    #[test]
    fn prefixes_continuation_lines() {
        let quoted = "> quoted *text*\n> more";
        let editable = editable_at(quoted, 4);
        assert_eq!(editable.line_prefix, "> ");
        let listed = "1. item\n   more";
        assert_eq!(editable_at(listed, 4).line_prefix, "   ");
    }

    #[test]
    fn puts_placeholders_in_nested_lists() {
        let source = "- a\n  - b\n  - \n";
        let list = source.find("  - b").unwrap() + 2..source.len() - 1;
        let at = source.len() - 1;
        assert_eq!(with_placeholder(source, &list, at), "- b\n- \u{200B}");
        assert_eq!(with_placeholder("-", &(0..1), 1), "- \u{200B}");
    }

    #[test]
    fn diffs_minimally() {
        assert_eq!(diff("hello world", "hello brave world"), (6..6, "brave "));
        assert_eq!(diff("abc", "ac"), (1..2, ""));
        assert_eq!(diff("same", "same"), (4..4, ""));
        assert_eq!(diff("", "new"), (0..0, "new"));
        assert_eq!(diff("aaa", "aaaa"), (3..3, "a"));
        assert_eq!(diff("café", "cafés"), (5..5, "s"));
        assert_eq!(diff("naïve", "naive"), (2..4, "i"));
        assert_eq!(utf16_range("a😀b", &(1..5)), 1..3);
    }
}
