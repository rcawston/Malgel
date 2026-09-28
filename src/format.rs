//! Markdown formatting commands as pure text transforms.
//!
//! Each command reads the document and the current selection (byte offsets)
//! and returns one [`Edit`]: the range to replace, its replacement, and the
//! selection to restore afterwards. Keeping them pure makes them easy to test
//! and keeps the editor integration to a single replace call.

use std::ops::Range;

/// A single replacement produced by a formatting command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    /// The selection after the edit, in post-edit byte offsets.
    pub selection: Range<usize>,
}

/// An inline style that wraps text in a symmetric marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Strikethrough,
    Code,
}

impl Inline {
    fn marker(self) -> &'static str {
        match self {
            Inline::Bold => "**",
            Inline::Italic => "_",
            Inline::Strikethrough => "~~",
            Inline::Code => "`",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Inline::Bold => "bold text",
            Inline::Italic => "italic text",
            Inline::Strikethrough => "struck text",
            Inline::Code => "code",
        }
    }
}

/// A block style applied to the start of every selected line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Heading(u8),
    Quote,
    BulletList,
    NumberedList,
    TaskList,
}

/// Toggle an inline style on the selection.
///
/// A selection already wrapped in the marker, either inside or immediately
/// around it, is unwrapped. An empty selection inside a word styles the
/// word; elsewhere a placeholder is inserted and selected so typing
/// replaces it.
pub fn toggle_inline(text: &str, selection: Range<usize>, style: Inline) -> Edit {
    let marker = style.marker();
    let selection = clamp(text, selection);
    let selected = &text[selection.clone()];

    // `**word**` selected, including the markers.
    if selected.len() >= marker.len() * 2
        && selected.starts_with(marker)
        && selected.ends_with(marker)
        && !(style == Inline::Italic && selected.starts_with("__"))
    {
        let inner = &selected[marker.len()..selected.len() - marker.len()];
        return Edit {
            range: selection.clone(),
            text: inner.to_string(),
            selection: selection.start..selection.start + inner.len(),
        };
    }

    // `word` selected with the markers just outside it.
    let before = selection.start.checked_sub(marker.len());
    let after = selection.end + marker.len();
    if let Some(before) = before
        && after <= text.len()
        && &text[before..selection.start] == marker
        && &text[selection.end..after] == marker
    {
        return Edit {
            range: before..after,
            text: selected.to_string(),
            selection: before..before + selected.len(),
        };
    }

    let target = if selection.is_empty() {
        word_at(text, selection.start)
    } else {
        trim_whitespace(text, selection)
    };

    if target.is_empty() {
        let placeholder = style.placeholder();
        let start = target.start + marker.len();
        return Edit {
            range: target,
            text: format!("{marker}{placeholder}{marker}"),
            selection: start..start + placeholder.len(),
        };
    }

    let inner = &text[target.clone()];
    let start = target.start + marker.len();
    Edit {
        text: format!("{marker}{inner}{marker}"),
        selection: start..start + inner.len(),
        range: target,
    }
}

/// Turn the selection into a link and select the URL placeholder.
pub fn insert_link(text: &str, selection: Range<usize>) -> Edit {
    const URL: &str = "https://";
    let selection = clamp(text, selection);
    let label = &text[selection.clone()];
    let label = if label.is_empty() { "link text" } else { label };
    let url_start = selection.start + label.len() + 3;
    Edit {
        text: format!("[{label}]({URL})"),
        selection: url_start..url_start + URL.len(),
        range: selection,
    }
}

/// Insert a fenced code block around the selection.
pub fn insert_code_block(text: &str, selection: Range<usize>) -> Edit {
    let selection = clamp(text, selection);
    let body = &text[selection.clone()];
    let at_line_start = selection.start == 0 || text[..selection.start].ends_with('\n');
    let lead = if at_line_start { "" } else { "\n" };
    let body_start = selection.start + lead.len() + 4;
    Edit {
        text: format!("{lead}```\n{body}\n```\n"),
        selection: body_start..body_start + body.len(),
        range: selection,
    }
}

/// Toggle a block style on every line the selection touches.
///
/// When every non-blank line already has the style it is removed; otherwise
/// it is applied, replacing any other heading, quote or list marker so the
/// command switches a line between block types in one step.
pub fn toggle_block(text: &str, selection: Range<usize>, style: Block) -> Edit {
    let selection = clamp(text, selection);
    let start = text[..selection.start].rfind('\n').map_or(0, |ix| ix + 1);
    let end = if selection.end > selection.start && text[..selection.end].ends_with('\n') {
        selection.end - 1
    } else {
        text[selection.end..]
            .find('\n')
            .map_or(text.len(), |ix| selection.end + ix)
    };

    let lines: Vec<&str> = text[start..end].split('\n').collect();
    let has_style = |line: &str| {
        let (_, marker, _) = split_marker(line);
        marker_matches(marker, style)
    };
    let remove = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .all(|line| has_style(line))
        && lines.iter().any(|line| !line.trim().is_empty());

    let mut number = 0;
    let replaced: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            let (indent, _, rest) = split_marker(line);
            if remove {
                return format!("{indent}{rest}");
            }
            number += 1;
            let prefix = match style {
                Block::Heading(level) => format!("{} ", "#".repeat(level.clamp(1, 6) as usize)),
                Block::Quote => "> ".to_string(),
                Block::BulletList => "- ".to_string(),
                Block::NumberedList => format!("{number}. "),
                Block::TaskList => "- [ ] ".to_string(),
            };
            format!("{indent}{prefix}{rest}")
        })
        .collect();

    let replacement = replaced.join("\n");
    let caret = if selection.is_empty() {
        // Keep the caret at the end of its line's content.
        start + replacement.len()
    } else {
        start
    };
    Edit {
        selection: if selection.is_empty() {
            caret..caret
        } else {
            start..start + replacement.len()
        },
        range: start..end,
        text: replacement,
    }
}

/// Split a line into its indentation, its block marker and its content.
fn split_marker(line: &str) -> (&str, &str, &str) {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, body) = line.split_at(indent_len);

    let marker_len = if let Some(rest) = body.strip_prefix('#') {
        let hashes = 1 + rest.len() - rest.trim_start_matches('#').len();
        let after = &body[hashes..];
        if hashes <= 6 && (after.is_empty() || after.starts_with(' ')) {
            hashes + usize::from(after.starts_with(' '))
        } else {
            0
        }
    } else if body.starts_with("> ") {
        2
    } else if body.starts_with('>') {
        1
    } else if body.starts_with("- [ ] ") || body.starts_with("- [x] ") || body.starts_with("- [X] ")
    {
        6
    } else if body.starts_with("- ") || body.starts_with("* ") || body.starts_with("+ ") {
        2
    } else {
        let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && body[digits..].starts_with(". ") {
            digits + 2
        } else {
            0
        }
    };

    (indent, &body[..marker_len], &body[marker_len..])
}

fn marker_matches(marker: &str, style: Block) -> bool {
    let marker = marker.trim_end();
    match style {
        Block::Heading(level) => marker == "#".repeat(level.clamp(1, 6) as usize),
        Block::Quote => marker == ">",
        Block::BulletList => matches!(marker, "-" | "*" | "+"),
        Block::NumberedList => {
            marker.ends_with('.')
                && marker[..marker.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit())
        }
        Block::TaskList => marker.starts_with("- ["),
    }
}

/// `range` without leading and trailing whitespace: CommonMark does not
/// treat `** bold **` as emphasis, so the markers must hug the text.
fn trim_whitespace(text: &str, range: Range<usize>) -> Range<usize> {
    let selected = &text[range.clone()];
    let start = range.start + (selected.len() - selected.trim_start().len());
    let end = range.end - (selected.len() - selected.trim_end().len());
    if start >= end {
        range.start..range.start
    } else {
        start..end
    }
}

/// The word around `offset`, or an empty range at `offset`.
fn word_at(text: &str, offset: usize) -> Range<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(offset, |(ix, _)| ix);
    let end = text[offset..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(ix, _)| offset + ix);
    start..end
}

fn clamp(text: &str, range: Range<usize>) -> Range<usize> {
    let floor = |mut ix: usize| {
        ix = ix.min(text.len());
        while !text.is_char_boundary(ix) {
            ix -= 1;
        }
        ix
    };
    let start = floor(range.start.min(range.end));
    let end = floor(range.start.max(range.end));
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edit: &Edit) -> String {
        let mut out = text.to_string();
        out.replace_range(edit.range.clone(), &edit.text);
        out
    }

    #[test]
    fn wraps_and_unwraps_selection() {
        let text = "make this bold";
        let edit = toggle_inline(text, 5..9, Inline::Bold);
        let bold = apply(text, &edit);
        assert_eq!(bold, "make **this** bold");
        assert_eq!(&bold[edit.selection.clone()], "this");

        // Toggling the same selection removes the markers again.
        let edit = toggle_inline(&bold, edit.selection, Inline::Bold);
        let plain = apply(&bold, &edit);
        assert_eq!(plain, text);
        assert_eq!(&plain[edit.selection], "this");

        // Selecting the markers too also unwraps.
        let edit = toggle_inline("**x**", 0..5, Inline::Bold);
        assert_eq!(apply("**x**", &edit), "x");
    }

    #[test]
    fn keeps_selected_whitespace_outside_the_markers() {
        let text = "## Section 7 ";
        let edit = toggle_inline(text, 3..13, Inline::Bold);
        assert_eq!(apply(text, &edit), "## **Section 7** ");
    }

    #[test]
    fn styles_word_under_caret_or_inserts_placeholder() {
        let edit = toggle_inline("say hello now", 6..6, Inline::Code);
        assert_eq!(apply("say hello now", &edit), "say `hello` now");

        let edit = toggle_inline("a  b", 2..2, Inline::Italic);
        let out = apply("a  b", &edit);
        assert_eq!(out, "a _italic text_ b");
        assert_eq!(&out[edit.selection], "italic text");
    }

    #[test]
    fn inserts_link_with_url_selected() {
        let edit = insert_link("see docs", 4..8);
        let out = apply("see docs", &edit);
        assert_eq!(out, "see [docs](https://)");
        assert_eq!(&out[edit.selection], "https://");
    }

    #[test]
    fn toggles_headings_and_switches_block_types() {
        let text = "Title\nbody";
        let edit = toggle_block(text, 2..2, Block::Heading(2));
        let heading = apply(text, &edit);
        assert_eq!(heading, "## Title\nbody");

        let edit = toggle_block(&heading, 3..3, Block::Heading(2));
        assert_eq!(apply(&heading, &edit), text);

        // A different level replaces the marker instead of stacking.
        let edit = toggle_block(&heading, 3..3, Block::Heading(1));
        assert_eq!(apply(&heading, &edit), "# Title\nbody");

        let edit = toggle_block("- item", 0..0, Block::Quote);
        assert_eq!(apply("- item", &edit), "> item");
    }

    #[test]
    fn numbers_and_toggles_list_lines() {
        let text = "one\n\ntwo\nthree";
        let edit = toggle_block(text, 0..text.len(), Block::NumberedList);
        let list = apply(text, &edit);
        assert_eq!(list, "1. one\n\n2. two\n3. three");

        let edit = toggle_block(&list, 0..list.len(), Block::NumberedList);
        assert_eq!(apply(&list, &edit), text);

        let edit = toggle_block("  todo", 0..0, Block::TaskList);
        assert_eq!(apply("  todo", &edit), "  - [ ] todo");
    }

    #[test]
    fn wraps_code_block_on_its_own_lines() {
        let edit = insert_code_block("x = 1", 0..5);
        let out = apply("x = 1", &edit);
        assert_eq!(out, "```\nx = 1\n```\n");
        assert_eq!(&out[edit.selection], "x = 1");
    }

    #[test]
    fn clamps_to_char_boundaries() {
        let text = "é";
        let edit = toggle_inline(text, 1..1, Inline::Bold);
        assert_eq!(apply(text, &edit), "**é**");
    }
}
