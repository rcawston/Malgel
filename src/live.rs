//! Live Preview: the formatted document, edited where it stands.
//!
//! The preview renders the whole document, except the block holding the
//! caret, which it shows as its Markdown in a small editor of its own (the
//! block's Markdown only, growing with it). Leaving the block, by arrow keys
//! or a click elsewhere, renders it again and opens the next. The document's
//! own editor stays the source of truth: each change in the block editor is
//! applied to it, so saving, undo, recovery and the outline work as ever.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, Mutex},
};

use gpui_kit::{
    AbsoluteLength, App, Entity, EntityId, FontWeight, Global, IntoElement, ParentElement as _,
    Styled as _, WeakEntity, Window,
    base::TextViewState,
    component::{
        Sizable as _, Size,
        input::{Textarea, TextareaState},
        text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown_ast::Node},
    },
    div, px, relative, rems,
};

use crate::analysis::preview_parse_options;

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

/// The part of `source` to edit for a caret at `offset`: the block holding
/// it. On a blank line between blocks, the block above taken down to that
/// line (or, at the top, the block below from it), as the editor must show
/// in some block's place.
pub fn region_at(source: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(source.len());
    let blocks = blocks(source);
    if let Some(block) = blocks
        .iter()
        .find(|block| block.start <= offset && offset <= block.end)
    {
        return block.clone();
    }
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |ix| offset + ix);
    let line_start = source[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    if let Some(above) = blocks.iter().rev().find(|block| block.end <= offset) {
        return above.start..line_end;
    }
    match blocks.first() {
        Some(below) => line_start..below.end,
        None => 0..source.len(),
    }
}

/// The block before `region` (to move up into), if any.
pub fn previous_region(source: &str, region: &Range<usize>) -> Option<Range<usize>> {
    blocks(source)
        .into_iter()
        .rev()
        .find(|block| block.end <= region.start && block.start < region.start)
}

/// The block after `region` (to move down into), if any.
pub fn next_region(source: &str, region: &Range<usize>) -> Option<Range<usize>> {
    blocks(source)
        .into_iter()
        .find(|block| block.start >= region.end && block.end > region.end)
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
/// the source being edited.
#[derive(Default)]
pub struct LiveState {
    region: Mutex<Option<Range<usize>>>,
}

impl LiveState {
    pub fn set_region(&self, region: Option<Range<usize>>) {
        *self.region.lock().expect("live state poisoned") = region;
    }

    pub fn region(&self) -> Option<Range<usize>> {
        self.region.lock().expect("live state poisoned").clone()
    }
}

/// The block editors of the previews showing Live Preview, by preview.
/// Plugins render on the main thread, where the editor entity lives.
#[derive(Default)]
pub struct LiveEditors(HashMap<EntityId, WeakEntity<TextareaState>>);

impl Global for LiveEditors {}

pub fn register_editor(preview: EntityId, editor: &Entity<TextareaState>, cx: &mut App) {
    let editors = &mut cx.default_global::<LiveEditors>().0;
    // Forget the editors of documents since closed.
    editors.retain(|_, editor| editor.upgrade().is_some());
    editors.insert(preview, editor.downgrade());
}

/// What the plugin keeps on a node.
#[derive(Debug, Clone)]
enum Part {
    /// The first block being edited, drawn as the editor; how many more
    /// blocks the editor covers, which are hidden.
    Editor {
        covered: usize,
    },
    Hidden,
}

pub struct LiveBlockPlugin {
    state: Arc<LiveState>,
    preview: WeakEntity<TextViewState>,
}

impl LiveBlockPlugin {
    pub fn new(state: Arc<LiveState>, preview: WeakEntity<TextViewState>) -> Self {
        Self { state, preview }
    }
}

impl MarkdownPlugin for LiveBlockPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "live-block"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let region = self.state.region()?;
        let position = node.position()?;
        let start = cx.offset() + position.start.offset;
        let end = cx.offset() + position.end.offset;
        // Blocks nested in another (list items, quotes) are the outer
        // block's business; only top-level ones count, which the region is
        // made of.
        let overlaps = start < region.end && end > region.start;
        if !overlaps {
            return None;
        }
        let source = cx.node_source(node).unwrap_or_default().to_string();
        if start <= region.start {
            let covered = blocks(
                cx.source()
                    .get(region.start.saturating_sub(cx.offset())..)?,
            )
            .into_iter()
            .filter(|block| block.start < region.len())
            .count()
            .saturating_sub(1);
            Some(MarkdownNode::new("live-block", Part::Editor { covered }).markdown(source))
        } else {
            Some(MarkdownNode::new("live-block", Part::Hidden).markdown(source))
        }
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(Part::Editor { covered }) = node.data::<Part>() else {
            return div().into_any_element();
        };
        let editor = self.preview.upgrade().and_then(|preview| {
            cx.try_global::<LiveEditors>()?
                .0
                .get(&preview.entity_id())?
                .upgrade()
        });
        let Some(editor) = editor else {
            return div().into_any_element();
        };
        let heading = heading_level(&editor.read(cx).value());
        div()
            .w_full()
            // GPUI Kit leaves a paragraph gap (1rem) under each hidden
            // block; take that space back.
            .mb(rems(-(*covered as f32)))
            .child(block_editor(&editor, heading))
            .into_any_element()
    }
}

/// The heading level of Markdown starting with `#` to `######` and a
/// space, if it does.
pub fn heading_level(text: &str) -> Option<u8> {
    let hashes = text.chars().take_while(|ch| *ch == '#').count();
    let rest = &text[hashes..];
    ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with([' ', '\t'])))
        .then_some(hashes as u8)
}

/// The block editor, set like the text around it: a heading being edited
/// keeps its heading's size and weight, as GPUI Kit draws headings.
pub fn block_editor(editor: &Entity<TextareaState>, heading: Option<u8>) -> Textarea {
    let (size_of_text, weight) = match heading {
        Some(1) => (AbsoluteLength::from(px(28.)), FontWeight::BOLD),
        Some(2) => (px(21.).into(), FontWeight::SEMIBOLD),
        Some(3) => (px(17.5).into(), FontWeight::SEMIBOLD),
        Some(4) => (px(15.75).into(), FontWeight::SEMIBOLD),
        Some(5) => (rems(1.).into(), FontWeight::SEMIBOLD),
        Some(6) => (rems(1.).into(), FontWeight::MEDIUM),
        _ => (rems(1.).into(), FontWeight::NORMAL),
    };
    // The editor pads its text by its size's input padding; take the
    // padding back so the text stands where the rendered text did.
    let size = Size::XSmall;
    let (x, y) = (size.input_px(), size.input_py());
    Textarea::new(editor)
        .appearance(false)
        .with_size(size)
        .ml(-x)
        .mr(-x)
        .mt(-y)
        .mb(-y)
        .text_size(size_of_text)
        .font_weight(weight)
        .line_height(relative(1.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Title\n\nFirst paragraph\nstill first.\n\n- a\n- b\n\n\nLast.";

    #[test]
    fn finds_regions() {
        let first = DOC.find("First").unwrap();
        assert_eq!(
            &DOC[region_at(DOC, first + 3)],
            "First paragraph\nstill first."
        );
        assert_eq!(&DOC[region_at(DOC, DOC.find("- b").unwrap())], "- a\n- b");
        assert_eq!(&DOC[region_at(DOC, 0)], "# Title");
        // A blank line between blocks joins the block above.
        let blank = DOC.find("\n\n\n").unwrap() + 2;
        assert_eq!(&DOC[region_at(DOC, blank)], "- a\n- b\n\n");
        assert_eq!(&"\n\nText"[region_at("\n\nText", 0)], "\n\nText");
        assert_eq!(region_at("", 0), 0..0);
        assert_eq!(&DOC[region_at(DOC, DOC.len())], "Last.");
        let doc = "- a\n- [ ] b\n\n> q\n";
        assert_eq!(&doc[region_at(doc, 1)], "- a\n- [ ] b");
    }

    #[test]
    fn moves_between_blocks() {
        let list = region_at(DOC, DOC.find("- a").unwrap());
        assert_eq!(
            &DOC[previous_region(DOC, &list).unwrap()],
            "First paragraph\nstill first."
        );
        assert_eq!(&DOC[next_region(DOC, &list).unwrap()], "Last.");
        let title = region_at(DOC, 0);
        assert!(previous_region(DOC, &title).is_none());
        assert!(next_region(DOC, &region_at(DOC, DOC.len())).is_none());
    }

    #[test]
    fn reads_heading_levels() {
        assert_eq!(heading_level("## Two"), Some(2));
        assert_eq!(heading_level("#"), Some(1));
        assert_eq!(heading_level("#hashtag"), None);
        assert_eq!(heading_level("####### seven"), None);
        assert_eq!(heading_level("Plain"), None);
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
