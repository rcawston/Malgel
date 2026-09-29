//! Editing assists that make Markdown structure follow the keyboard: lists
//! continue on Enter and nest with Tab, tables stay aligned and Tab moves
//! between their cells, and a URL pasted over text links it.
//!
//! Like [`crate::format`], every assist is a pure function from the text and
//! selection (byte offsets) to an [`Edit`]. `None` means the assist doesn't
//! apply and the key should do what it normally does.

use std::ops::Range;

use unicode_width::UnicodeWidthStr as _;

use crate::format::Edit;

/// Columns a tab counts for in indentation.
const TAB_WIDTH: usize = 4;

// ---------------------------------------------------------------------------
// Lines

/// The line containing byte `offset`, without its newline.
fn line_at(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |ix| offset + ix);
    start..end
}

/// Byte ranges of every line, without newlines.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (ix, ch) in text.char_indices() {
        if ch == '\n' {
            lines.push(start..ix);
            start = ix + 1;
        }
    }
    lines.push(start..text.len());
    lines
}

fn line_index(lines: &[Range<usize>], offset: usize) -> usize {
    lines
        .partition_point(|line| line.end < offset)
        .min(lines.len() - 1)
}

/// Whether the line starting at `line_start` is inside a fenced code block,
/// where Markdown structure is just text.
fn in_code_fence(text: &str, line_start: usize) -> bool {
    let mut fence: Option<(char, usize)> = None;
    for line in text[..line_start].lines() {
        let trimmed = line.trim_start_matches([' ', '\t', '>']).trim_start();
        let ch = match trimmed.chars().next() {
            Some(ch @ ('`' | '~')) => ch,
            _ => continue,
        };
        let run = trimmed.len() - trimmed.trim_start_matches(ch).len();
        if run < 3 {
            continue;
        }
        match fence {
            None => fence = Some((ch, run)),
            Some((open, len)) if open == ch && run >= len && trimmed[run..].trim().is_empty() => {
                fence = None
            }
            Some(_) => {}
        }
    }
    fence.is_some()
}

fn columns(indent: &str) -> usize {
    indent
        .chars()
        .map(|ch| if ch == '\t' { TAB_WIDTH } else { 1 })
        .sum()
}

// ---------------------------------------------------------------------------
// Lists

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Bullet(char),
    Ordered { number: u64, delimiter: char },
}

/// The structure at the start of a line: quote markers, then optionally a
/// list item.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinePrefix<'a> {
    /// Leading `>` markers with their spacing, e.g. `"> > "`.
    quote: &'a str,
    /// Whitespace before the list marker.
    indent: &'a str,
    item: Option<Item>,
    /// Bytes from the line start to its content.
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Item {
    marker: Marker,
    /// `Some(checked)` for task items.
    task: Option<bool>,
}

impl LinePrefix<'_> {
    fn indent_columns(&self) -> usize {
        columns(self.indent)
    }
}

fn parse_prefix(line: &str) -> LinePrefix<'_> {
    // Quote markers: up to three spaces, `>`, an optional space.
    let mut quote_len = 0;
    loop {
        let rest = &line[quote_len..];
        let spaces = rest.len() - rest.trim_start_matches(' ').len();
        if spaces > 3 || !rest[spaces..].starts_with('>') {
            break;
        }
        quote_len += spaces + 1;
        if line[quote_len..].starts_with(' ') {
            quote_len += 1;
        }
    }
    let quote = &line[..quote_len];
    let rest = &line[quote_len..];
    let indent_len = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let indent = &rest[..indent_len];
    let body = &rest[indent_len..];

    let no_item = LinePrefix {
        quote,
        indent,
        item: None,
        len: quote_len,
    };
    if is_thematic_break(body) {
        return no_item;
    }
    let (marker, marker_len) = match body.chars().next() {
        Some(ch @ ('-' | '*' | '+')) => (Marker::Bullet(ch), 1),
        Some(ch) if ch.is_ascii_digit() => {
            let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let delimiter = match body[digits..].chars().next() {
                Some(ch @ ('.' | ')')) if digits <= 9 => ch,
                _ => return no_item,
            };
            let Ok(number) = body[..digits].parse() else {
                return no_item;
            };
            (Marker::Ordered { number, delimiter }, digits + 1)
        }
        _ => return no_item,
    };
    let after = &body[marker_len..];
    if !after.is_empty() && !after.starts_with(' ') {
        return no_item;
    }
    let mut len = quote_len + indent_len + marker_len + usize::from(after.starts_with(' '));
    let content = &line[len..];
    let task = ["[ ]", "[x]", "[X]"]
        .iter()
        .find(|box_| {
            content.starts_with(*box_) && matches!(content[3..].chars().next(), None | Some(' '))
        })
        .map(|box_| {
            len += 3 + usize::from(content[3..].starts_with(' '));
            *box_ != "[ ]"
        });
    LinePrefix {
        quote,
        indent,
        item: Some(Item { marker, task }),
        len,
    }
}

fn is_thematic_break(body: &str) -> bool {
    let body = body.trim_end();
    let Some(ch @ ('-' | '*' | '_')) = body.chars().next() else {
        return false;
    };
    body.chars().all(|c| c == ch || c == ' ') && body.chars().filter(|&c| c == ch).count() >= 3
}

fn marker_text(item: Item) -> String {
    let marker = match item.marker {
        Marker::Bullet(ch) => format!("{ch} "),
        Marker::Ordered { number, delimiter } => format!("{number}{delimiter} "),
    };
    match item.task {
        Some(_) => format!("{marker}[ ] "),
        None => marker,
    }
}

/// Handle Enter at the caret: continue the list item or quote the caret is
/// in, or end it when the item is empty.
pub fn enter(text: &str, selection: Range<usize>) -> Option<Edit> {
    if !selection.is_empty() || selection.end > text.len() {
        return None;
    }
    let caret = selection.start;
    let line = line_at(text, caret);
    if in_code_fence(text, line.start) {
        return None;
    }
    if let Some(edit) = table_enter(text, caret) {
        return Some(edit);
    }
    let prefix = parse_prefix(&text[line.clone()]);
    if prefix.len == 0 || caret < line.start + prefix.len {
        return None;
    }
    let content = &text[line.start + prefix.len..line.end];

    if content.trim().is_empty() && caret == line.end {
        // An empty item ends the list: a nested one steps out a level first.
        if prefix.item.is_some() && !prefix.indent.is_empty() {
            return outdent(text, selection);
        }
        // Drop the marker (or the innermost quote level), keeping the line.
        let kept = match prefix.item {
            Some(_) => prefix.quote.to_string(),
            None => {
                let inner = prefix.quote.trim_end();
                inner[..inner.len() - 1].to_string()
            }
        };
        let start = line.start;
        return Some(Edit {
            range: line,
            selection: start + kept.len()..start + kept.len(),
            text: kept,
        });
    }

    let continued = match prefix.item {
        Some(item) => {
            let marker = match item.marker {
                Marker::Ordered { number, delimiter } => Marker::Ordered {
                    number: number + 1,
                    delimiter,
                },
                bullet => bullet,
            };
            format!(
                "{}{}{}",
                prefix.quote,
                prefix.indent,
                marker_text(Item { marker, ..item })
            )
        }
        None => prefix.quote.to_string(),
    };
    // Text after the caret moves into the new item.
    let rest = &text[caret..line.end];
    let moved = caret + (rest.len() - rest.trim_start().len());
    let inserted = format!("\n{continued}");
    let caret_after = caret + inserted.len();

    let mut new_text = String::with_capacity(text.len() + inserted.len());
    new_text.push_str(&text[..caret]);
    new_text.push_str(&inserted);
    new_text.push_str(&text[moved..]);
    let edited = caret..moved;
    Some(renumbered(
        text,
        &new_text,
        edited,
        &inserted,
        caret_after..caret_after,
    ))
}

/// Nest the list items the selection touches under the item above them.
pub fn indent(text: &str, selection: Range<usize>) -> Option<Edit> {
    shift_items(text, selection, true)
}

/// Move the list items the selection touches out one level.
pub fn outdent(text: &str, selection: Range<usize>) -> Option<Edit> {
    shift_items(text, selection, false)
}

fn shift_items(text: &str, selection: Range<usize>, deeper: bool) -> Option<Edit> {
    if selection.end > text.len() {
        return None;
    }
    let lines = line_ranges(text);
    let first = line_index(&lines, selection.start);
    let mut last = line_index(&lines, selection.end);
    if last > first && selection.end == lines[last].start {
        last -= 1;
    }
    if in_code_fence(text, lines[first].start) {
        return None;
    }
    let prefixes: Vec<LinePrefix> = lines
        .iter()
        .map(|line| parse_prefix(&text[line.clone()]))
        .collect();
    let head = &prefixes[first];
    head.item?;
    let quote = head.quote;
    let head_indent = head.indent_columns();

    // Items bring their children (deeper lines that follow) with them.
    while last + 1 < lines.len() {
        let next = &prefixes[last + 1];
        let blank = text[lines[last + 1].clone()].trim().is_empty();
        let deeper_line = next.quote == quote && next.indent_columns() > head_indent;
        let continues = blank
            && prefixes
                .get(last + 2)
                .is_some_and(|after| after.quote == quote && after.indent_columns() > head_indent);
        if !(deeper_line && !blank || continues) {
            break;
        }
        last += 1;
    }

    // Siblings and parents: the nearest items above at the same or a
    // shallower level.
    let above = |predicate: &dyn Fn(usize) -> bool| {
        (0..first).rev().find(|&ix| {
            let prefix = &prefixes[ix];
            !text[lines[ix].clone()].trim().is_empty()
                && prefix.quote == quote
                && prefix.item.is_some()
                && predicate(prefix.indent_columns())
        })
    };
    let target = if deeper {
        let sibling = above(&|indent| indent <= head_indent)?;
        if prefixes[sibling].indent_columns() != head_indent {
            return None;
        }
        // The content column of the item above.
        let prefix = &prefixes[sibling];
        let item = prefix.item?;
        let marker = marker_text(Item { task: None, ..item });
        head_indent + marker.len()
    } else {
        if head_indent == 0 {
            return None;
        }
        above(&|indent| indent < head_indent).map_or(0, |parent| prefixes[parent].indent_columns())
    };

    // Rewrite the lines, tracking where the selection's ends move.
    let region = lines[first].start..lines[last].end;
    let mut out = String::new();
    let map_offset = |offset: usize, shifts: &[(usize, isize)]| -> usize {
        let mut moved = offset as isize;
        for &(line_start, delta) in shifts {
            if line_start <= offset {
                moved += delta;
            }
        }
        moved.max(0) as usize
    };
    let mut shifts: Vec<(usize, isize)> = Vec::new();
    let mut first_in_group = true;
    for ix in first..=last {
        let line = &text[lines[ix].clone()];
        if ix > first {
            out.push('\n');
        }
        let prefix = &prefixes[ix];
        if line.trim().is_empty() || prefix.quote != quote {
            out.push_str(line);
            continue;
        }
        let old = prefix.indent_columns();
        let new = if deeper {
            old + (target - head_indent)
        } else {
            old.saturating_sub(head_indent - target)
        };
        let rest = &line[prefix.quote.len() + prefix.indent.len()..];
        let mut rewritten = format!("{}{}{rest}", prefix.quote, " ".repeat(new));
        // A newly nested numbered item starts its own list at 1.
        if deeper
            && ix == first
            && first_in_group
            && let Some(Item {
                marker: Marker::Ordered { number, delimiter },
                ..
            }) = prefix.item
            && !has_sibling_above(text, &lines, &prefixes, first, quote, target)
        {
            let digits = number.to_string().len();
            let start = prefix.quote.len() + new;
            rewritten.replace_range(start..start + digits + 1, &format!("1{delimiter}"));
        }
        first_in_group = false;
        shifts.push((
            lines[ix].start + prefix.quote.len(),
            rewritten.len() as isize - line.len() as isize,
        ));
        out.push_str(&rewritten);
    }

    let mut new_text = String::with_capacity(text.len() + out.len());
    new_text.push_str(&text[..region.start]);
    new_text.push_str(&out);
    new_text.push_str(&text[region.end..]);
    let new_selection = map_offset(selection.start, &shifts)..map_offset(selection.end, &shifts);
    Some(renumbered(text, &new_text, region, &out, new_selection))
}

fn has_sibling_above(
    text: &str,
    lines: &[Range<usize>],
    prefixes: &[LinePrefix],
    line: usize,
    quote: &str,
    indent: usize,
) -> bool {
    for ix in (0..line).rev() {
        if text[lines[ix].clone()].trim().is_empty() {
            continue;
        }
        let prefix = &prefixes[ix];
        if prefix.quote != quote {
            return false;
        }
        let columns = prefix.indent_columns();
        if columns < indent {
            return false;
        }
        if columns == indent && prefix.item.is_some() {
            return true;
        }
    }
    false
}

/// Build the edit that turns `old` into `new` (which differ in `edited`,
/// replaced by `replacement`), after renumbering the numbered lists around
/// the change so they count up again.
fn renumbered(
    old: &str,
    new: &str,
    edited: Range<usize>,
    replacement: &str,
    selection: Range<usize>,
) -> Edit {
    let lines = line_ranges(new);
    let changed_start = line_index(&lines, edited.start);
    let changed_end = line_index(&lines, edited.start + replacement.len());
    let block = list_block(new, &lines, changed_start, changed_end);
    let numbers = renumber(new, &lines, block.clone());

    // Nothing to renumber: the plain edit.
    if numbers.is_empty() {
        return Edit {
            range: edited,
            text: replacement.to_string(),
            selection,
        };
    }

    // Apply the renumbering to the block, then express the whole change as
    // one replacement of the old text from the edit to the block's end.
    let block_range = lines[block.start].start..lines[block.end - 1].end;
    let mut rewritten = String::new();
    let mut selection = selection;
    let mut cursor = block_range.start;
    for (range, text) in numbers {
        rewritten.push_str(&new[cursor..range.start]);
        rewritten.push_str(&text);
        let delta = text.len() as isize - range.len() as isize;
        let shift = |offset: usize| {
            if offset > range.start {
                (offset as isize + delta).max(range.start as isize) as usize
            } else {
                offset
            }
        };
        selection = shift(selection.start)..shift(selection.end);
        cursor = range.end;
    }
    rewritten.push_str(&new[cursor..block_range.end]);

    // Old-text range: from the start of the edit (or block) to the block end.
    let start = edited.start.min(block_range.start);
    let new_block_end = block_range.end;
    let old_end = new_block_end as isize - (new.len() as isize - old.len() as isize);
    let old_end = (old_end.max(edited.end as isize) as usize).min(old.len());
    let mut text = new[start..block_range.start].to_string();
    text.push_str(&rewritten);
    Edit {
        range: start..old_end,
        text,
        selection,
    }
}

/// The lines around `first..=last` that belong to the same list.
fn list_block(text: &str, lines: &[Range<usize>], first: usize, last: usize) -> Range<usize> {
    let quote = parse_prefix(&text[lines[first].clone()]).quote.to_string();
    let belongs = |ix: usize| {
        let line = &text[lines[ix].clone()];
        let prefix = parse_prefix(line);
        prefix.quote == quote
            && (prefix.item.is_some()
                || line[prefix.quote.len()..].starts_with([' ', '\t'])
                || line.trim() == quote.trim())
    };
    let blank = |ix: usize| text[lines[ix].clone()].trim().is_empty();
    let mut start = first;
    while start > 0 && (belongs(start - 1) || blank(start - 1) && start > 1 && belongs(start - 2)) {
        start -= 1;
    }
    let mut end = last + 1;
    while end < lines.len()
        && (belongs(end) || blank(end) && end + 1 < lines.len() && belongs(end + 1))
    {
        end += 1;
    }
    start..end
}

/// Renumber every numbered list in the block so each counts up from its
/// first item. Returns the marker ranges to replace and their new text.
fn renumber(
    text: &str,
    lines: &[Range<usize>],
    block: Range<usize>,
) -> Vec<(Range<usize>, String)> {
    // Open lists, innermost last: (indent, next number, or None if bulleted).
    let mut open: Vec<(usize, Option<u64>)> = Vec::new();
    let mut changes = Vec::new();
    for ix in block {
        let line = &text[lines[ix].clone()];
        if line.trim().is_empty() {
            continue;
        }
        let prefix = parse_prefix(line);
        let indent = prefix.indent_columns();
        let Some(item) = prefix.item else {
            // A shallower paragraph closes the lists nested deeper than it.
            open.retain(|(list_indent, _)| *list_indent < indent);
            continue;
        };
        open.retain(|(list_indent, _)| *list_indent <= indent);
        let number = match item.marker {
            Marker::Ordered { number, .. } => Some(number),
            Marker::Bullet(_) => None,
        };
        match open.last_mut() {
            Some((list_indent, next)) if *list_indent == indent => {
                match (next.as_mut(), item.marker) {
                    (Some(expected), Marker::Ordered { number, delimiter }) => {
                        if number != *expected {
                            let start = lines[ix].start + prefix.quote.len() + prefix.indent.len();
                            let digits = number.to_string().len();
                            changes.push((
                                start..start + digits + 1,
                                format!("{expected}{delimiter}"),
                            ));
                        }
                        *expected += 1;
                    }
                    _ => *next = number.map(|number| number + 1),
                }
            }
            _ => open.push((indent, number.map(|number| number + 1))),
        }
    }
    changes
}

// ---------------------------------------------------------------------------
// Tables

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    None,
    Left,
    Center,
    Right,
}

struct Table {
    /// Line indices of the table's rows, header and delimiter included.
    lines: Range<usize>,
    /// Leading whitespace of the table.
    indent: String,
    rows: Vec<Vec<String>>,
    aligns: Vec<Align>,
}

/// Split a table row into trimmed cells, honoring `\|` escapes.
fn split_row(line: &str) -> Vec<String> {
    let mut row = line.trim();
    row = row.strip_prefix('|').unwrap_or(row);
    if row.ends_with('|') && !row.ends_with("\\|") {
        row = &row[..row.len() - 1];
    }
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for ch in row.chars() {
        if ch == '|' && !escaped {
            cells.push(cell.trim().to_string());
            cell.clear();
            continue;
        }
        escaped = ch == '\\' && !escaped;
        cell.push(ch);
    }
    cells.push(cell.trim().to_string());
    cells
}

fn parse_delimiter(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') {
        return None;
    }
    split_row(line)
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let dashes = cell.trim_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|c| c == '-')).then_some(
                match (left, right) {
                    (true, true) => Align::Center,
                    (true, false) => Align::Left,
                    (false, true) => Align::Right,
                    (false, false) => Align::None,
                },
            )
        })
        .collect()
}

/// The table containing line `line`, if any.
fn table_at(text: &str, lines: &[Range<usize>], line: usize) -> Option<Table> {
    let is_row = |ix: usize| {
        let row = text[lines[ix].clone()].trim();
        !row.is_empty() && row.contains('|')
    };
    if !is_row(line) || in_code_fence(text, lines[line].start) {
        return None;
    }
    let mut start = line;
    while start > 0 && is_row(start - 1) {
        start -= 1;
    }
    let mut end = line + 1;
    while end < lines.len() && is_row(end) {
        end += 1;
    }
    if end - start < 2 {
        return None;
    }
    let aligns = parse_delimiter(&text[lines[start + 1].clone()])?;
    let first = &text[lines[start].clone()];
    let indent = first[..first.len() - first.trim_start().len()].to_string();
    let rows = (start..end)
        .filter(|ix| *ix != start + 1)
        .map(|ix| split_row(&text[lines[ix].clone()]))
        .collect();
    Some(Table {
        lines: start..end,
        indent,
        rows,
        aligns,
    })
}

impl Table {
    fn columns(&self) -> usize {
        self.rows
            .iter()
            .map(Vec::len)
            .chain([self.aligns.len()])
            .max()
            .unwrap_or(1)
    }

    /// Render aligned, returning the text and each cell's content range
    /// (relative to the text) by row (delimiter excluded) and column.
    fn render(&self) -> (String, Vec<Vec<Range<usize>>>) {
        let columns = self.columns();
        let cell = |row: &Vec<String>, column: usize| row.get(column).cloned().unwrap_or_default();
        let widths: Vec<usize> = (0..columns)
            .map(|column| {
                self.rows
                    .iter()
                    .map(|row| cell(row, column).width())
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect();
        let align = |column: usize| self.aligns.get(column).copied().unwrap_or(Align::None);

        let mut out = String::new();
        let mut spans = Vec::new();
        for (row_ix, row) in self.rows.iter().enumerate() {
            if row_ix > 0 {
                out.push('\n');
            }
            if row_ix == 1 {
                out.push_str(&self.indent);
                out.push('|');
                for (column, width) in widths.iter().enumerate() {
                    let dashes = match align(column) {
                        Align::None => "-".repeat(*width),
                        Align::Left => format!(":{}", "-".repeat(width - 1)),
                        Align::Right => format!("{}:", "-".repeat(width - 1)),
                        Align::Center => format!(":{}:", "-".repeat(width - 2)),
                    };
                    out.push_str(&format!(" {dashes} |"));
                }
                out.push('\n');
            }
            out.push_str(&self.indent);
            out.push('|');
            let mut row_spans = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                let content = cell(row, column);
                let pad = width - content.width();
                let (before, after) = match align(column) {
                    Align::Right => (pad, 0),
                    Align::Center => (pad / 2, pad - pad / 2),
                    Align::None | Align::Left => (0, pad),
                };
                out.push(' ');
                out.push_str(&" ".repeat(before));
                let start = out.len();
                out.push_str(&content);
                row_spans.push(start..out.len());
                out.push_str(&" ".repeat(after));
                out.push_str(" |");
            }
            spans.push(row_spans);
        }
        (out, spans)
    }
}

/// Where the caret is in a table: row (delimiter excluded), column, and
/// offset into the trimmed cell content.
fn table_position(
    text: &str,
    lines: &[Range<usize>],
    table: &Table,
    caret: usize,
) -> (usize, usize, usize) {
    let line = line_index(lines, caret);
    let row = if line <= table.lines.start + 1 {
        0
    } else {
        line - table.lines.start - 1
    };
    let range = lines[line].clone();
    let line_text = &text[range.clone()];
    let before = &line_text[..caret - range.start];
    let trimmed_start = line_text.len() - line_text.trim_start().len();
    let leading_pipe = line_text.trim_start().starts_with('|');
    let mut column = 0;
    let mut cell_start = trimmed_start + usize::from(leading_pipe);
    let mut escaped = false;
    for (ix, ch) in before.char_indices() {
        if ix < cell_start {
            continue;
        }
        if ch == '|' && !escaped {
            column += 1;
            cell_start = ix + 1;
        }
        escaped = ch == '\\' && !escaped;
    }
    let raw = &line_text[cell_start.min(before.len())..before.len()];
    let within = raw.trim_start().len();
    (row, column, within)
}

fn table_edit(
    lines: &[Range<usize>],
    table: &Table,
    caret: (usize, usize, usize),
    select_cell: bool,
) -> Edit {
    let (rendered, spans) = table.render();
    let range = lines[table.lines.start].start..lines[table.lines.end - 1].end;
    let (row, column, within) = caret;
    let row = row.min(spans.len() - 1);
    let span = spans[row][column.min(spans[row].len() - 1)].clone();
    let selection = if select_cell {
        range.start + span.start..range.start + span.end
    } else {
        let at = range.start + span.start + within.min(span.len());
        at..at
    };
    Edit {
        range,
        text: rendered,
        selection,
    }
}

/// Align the table around the caret, keeping the caret in its cell.
pub fn tidy_table(text: &str, selection: Range<usize>) -> Option<Edit> {
    let lines = line_ranges(text);
    let caret = selection.start.min(text.len());
    let table = table_at(text, &lines, line_index(&lines, caret))?;
    let position = table_position(text, &lines, &table, caret);
    Some(table_edit(&lines, &table, position, false))
}

/// Tab in a table: tidy it and move to the next cell (the previous one when
/// `backwards`), adding a row after the last cell.
pub fn table_tab(text: &str, selection: Range<usize>, backwards: bool) -> Option<Edit> {
    let lines = line_ranges(text);
    let caret = selection.start.min(text.len());
    let mut table = table_at(text, &lines, line_index(&lines, caret))?;
    let (row, column, _) = table_position(text, &lines, &table, caret);
    let columns = table.columns();
    let (row, column) = if backwards {
        match (row, column) {
            (0, 0) => (0, 0),
            (row, 0) => (row - 1, columns - 1),
            (row, column) => (row, column.min(columns) - 1),
        }
    } else if column + 1 < columns {
        (row, column + 1)
    } else {
        if row + 1 == table.rows.len() {
            table.rows.push(vec![String::new(); columns]);
        }
        (row + 1, 0)
    };
    Some(table_edit(&lines, &table, (row, column, 0), true))
}

/// Enter in a table row adds a row below it; Enter on an empty last row
/// leaves the table. (Shift-Enter still breaks the line.)
fn table_enter(text: &str, caret: usize) -> Option<Edit> {
    let lines = line_ranges(text);
    let line = line_index(&lines, caret);
    let mut table = table_at(text, &lines, line)?;
    let (row, _, _) = table_position(text, &lines, &table, caret);
    let columns = table.columns();
    let is_last = line + 1 == table.lines.end;
    if is_last && row > 0 && table.rows[row].iter().all(String::is_empty) {
        // Leave the table: drop the empty row, continue below it.
        table.rows.pop();
        let (rendered, _) = table.render();
        let range = lines[table.lines.start].start..lines[line].end;
        let end = range.start + rendered.len() + 1;
        return Some(Edit {
            range,
            text: format!("{rendered}\n"),
            selection: end..end,
        });
    }
    table.rows.insert(row + 1, vec![String::new(); columns]);
    Some(table_edit(&lines, &table, (row + 1, 0, 0), false))
}

// ---------------------------------------------------------------------------
// Paste

/// `text` as a URL, if it is exactly one.
pub fn as_url(text: &str) -> Option<&str> {
    let url = text.trim();
    let scheme_ok = ["http://", "https://", "mailto:", "ftp://", "file://"]
        .iter()
        .any(|scheme| url.len() > scheme.len() && url.to_ascii_lowercase().starts_with(scheme));
    (scheme_ok && !url.contains(char::is_whitespace)).then_some(url)
}

/// Pasting a URL over selected text on one line links the text.
pub fn paste_link(text: &str, selection: Range<usize>, pasted: &str) -> Option<Edit> {
    let url = as_url(pasted)?;
    if selection.is_empty() || selection.end > text.len() {
        return None;
    }
    let selected = &text[selection.clone()];
    // Whitespace at the edges of the selection stays outside the link.
    let label = selected.trim();
    if label.is_empty() || label.contains('\n') || as_url(label).is_some() {
        return None;
    }
    let start = selection.start + (selected.len() - selected.trim_start().len());
    let replacement = format!("[{label}]({url})");
    let end = start + replacement.len();
    Some(Edit {
        range: start..start + label.len(),
        text: replacement,
        selection: end..end,
    })
}

/// Markdown linking `relative_path` as an image at `offset`, set apart as
/// its own paragraph unless the caret is on an empty line between blank
/// ones already.
pub fn image_markdown(text: &str, offset: usize, relative_path: &str, alt: &str) -> String {
    let offset = offset.min(text.len());
    let path = relative_path.replace('\\', "/").replace(' ', "%20");
    let image = format!("![{alt}]({path})");
    let line = line_at(text, offset);
    if !text[line.start..offset].trim().is_empty() {
        return format!("\n\n{image}\n");
    }
    let previous_blank = line.start == 0 || {
        let previous = line_at(text, line.start - 1);
        text[previous].trim().is_empty()
    };
    let rest_blank = text[offset..line.end].trim().is_empty();
    format!(
        "{}{image}{}",
        if previous_blank { "" } else { "\n" },
        if rest_blank { "" } else { "\n\n" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edit: &Edit) -> String {
        let mut out = text.to_string();
        out.replace_range(edit.range.clone(), &edit.text);
        out
    }

    /// Run an assist with the caret at `‸` and return the result with `‸` at
    /// the new caret (or `«`…`»` around a selection).
    fn run(marked: &str, assist: impl Fn(&str, Range<usize>) -> Option<Edit>) -> Option<String> {
        let caret = marked.find('‸').unwrap();
        let text = marked.replacen('‸', "", 1);
        let edit = assist(&text, caret..caret)?;
        let mut out = apply(&text, &edit);
        if edit.selection.is_empty() {
            out.insert(edit.selection.start, '‸');
        } else {
            out.insert(edit.selection.end, '»');
            out.insert(edit.selection.start, '«');
        }
        Some(out)
    }

    #[test]
    fn continues_bullets_tasks_and_quotes() {
        assert_eq!(run("- one‸", enter).unwrap(), "- one\n- ‸");
        assert_eq!(run("  * one‸", enter).unwrap(), "  * one\n  * ‸");
        assert_eq!(run("- [x] done‸", enter).unwrap(), "- [x] done\n- [ ] ‸");
        assert_eq!(run("> said‸", enter).unwrap(), "> said\n> ‸");
        assert_eq!(run("> - quoted‸", enter).unwrap(), "> - quoted\n> - ‸");
        // Splitting an item carries the rest of the line along.
        assert_eq!(run("- one‸ two", enter).unwrap(), "- one\n- ‸two");
    }

    #[test]
    fn continues_and_renumbers_numbered_lists() {
        assert_eq!(run("1. a‸", enter).unwrap(), "1. a\n2. ‸");
        assert_eq!(run("3) a‸", enter).unwrap(), "3) a\n4) ‸");
        assert_eq!(
            run("1. a‸\n2. b\n3. c", enter).unwrap(),
            "1. a\n2. ‸\n3. b\n4. c"
        );
        // Nested lists and following paragraphs keep their own numbers.
        assert_eq!(
            run("1. a‸\n   1. x\n2. b\n\nText\n\n1. other", enter).unwrap(),
            "1. a\n2. ‸\n   1. x\n3. b\n\nText\n\n1. other"
        );
    }

    #[test]
    fn empty_items_end_the_list() {
        assert_eq!(run("- a\n- ‸", enter).unwrap(), "- a\n‸");
        assert_eq!(run("> a\n> ‸", enter).unwrap(), "> a\n‸");
        assert_eq!(run("- a\n  - ‸", enter).unwrap(), "- a\n- ‸");
        assert_eq!(run("> - a\n> - ‸", enter).unwrap(), "> - a\n> ‸");
    }

    #[test]
    fn leaves_other_lines_and_code_alone() {
        assert_eq!(run("plain‸", enter), None);
        assert_eq!(run("‸- item", enter), None);
        assert_eq!(run("---‸", enter), None);
        assert_eq!(run("```\n- in code‸\n```", enter), None);
        assert_eq!(run("-not a list‸", enter), None);
    }

    #[test]
    fn nests_and_unnests_items_with_their_children() {
        assert_eq!(run("- a\n- b‸", indent).unwrap(), "- a\n  - b‸");
        assert_eq!(run("- a‸", indent), None);
        assert_eq!(run("1. a\n2. b‸", indent).unwrap(), "1. a\n   1. b‸");
        assert_eq!(
            run("- a\n- b‸\n  - child\n- c", indent).unwrap(),
            "- a\n  - b‸\n    - child\n- c"
        );
        assert_eq!(run("- a\n  - b‸", outdent).unwrap(), "- a\n- b‸");
        assert_eq!(run("- a‸", outdent), None);
        // Un-nesting a numbered item slots it into the parent list's count.
        assert_eq!(
            run("1. a\n   1. b‸\n2. c", outdent).unwrap(),
            "1. a\n2. b‸\n3. c"
        );
    }

    #[test]
    fn tidies_tables_keeping_the_caret_in_its_cell() {
        let marked = "| N‸ame | Qty |\n|:-|-:|\n| apple| 3|\n| kiwi fruit | 12 |";
        assert_eq!(
            run(marked, tidy_table).unwrap(),
            "| N‸ame       | Qty |\n\
             | :--------- | --: |\n\
             | apple      |   3 |\n\
             | kiwi fruit |  12 |"
        );
        // Wide characters count as two columns.
        let out = run("| a | b |\n|---|---|\n| 日本| x‸|", tidy_table).unwrap();
        assert!(out.ends_with("| 日本 | x‸   |"), "{out}");
        assert_eq!(run("no | table‸", tidy_table), None);
    }

    #[test]
    fn tab_moves_between_cells_and_adds_rows() {
        let tab = |text: &str, sel| table_tab(text, sel, false);
        let out = run("| a | b |\n|---|---|\n| 1‸ | 2 |", tab).unwrap();
        assert_eq!(out, "| a   | b   |\n| --- | --- |\n| 1   | «2»   |");
        // After the last cell comes a new row.
        let out = run("| a | b |\n|---|---|\n| 1 | 2‸ |", tab).unwrap();
        assert_eq!(
            out,
            "| a   | b   |\n| --- | --- |\n| 1   | 2   |\n| ‸    |     |"
        );
        // Tab from the header skips the delimiter row.
        let out = run("| a | b‸ |\n|---|---|\n| 1 | 2 |", tab).unwrap();
        assert_eq!(out, "| a   | b   |\n| --- | --- |\n| «1»   | 2   |");
        let back = run("| a | b |\n|---|---|\n| 1 | ‸2 |", |t, s| {
            table_tab(t, s, true)
        })
        .unwrap();
        assert_eq!(back, "| a   | b   |\n| --- | --- |\n| «1»   | 2   |");
    }

    #[test]
    fn enter_in_tables_adds_rows_and_leaves_empty_ones() {
        let out = run("| a | b |\n|---|---|\n| 1 | 2 |‸", enter).unwrap();
        assert_eq!(
            out,
            "| a   | b   |\n| --- | --- |\n| 1   | 2   |\n| ‸    |     |"
        );
        let out = run("| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |‸", enter).unwrap();
        assert_eq!(out, "| a   | b   |\n| --- | --- |\n| 1   | 2   |\n‸");
        // Mid-row too: the row stays whole.
        let out = run("| a | b |\n|---|---|\n| 1‸ | 2 |", enter).unwrap();
        assert_eq!(
            out,
            "| a   | b   |\n| --- | --- |\n| 1   | 2   |\n| ‸    |     |"
        );
        let out = run("| a | b |\n|---|---|\n| 1 | 2 |\n| ‸ |  |", enter).unwrap();
        assert_eq!(out, "| a   | b   |\n| --- | --- |\n| 1   | 2   |\n‸");
        // From the header, the new row goes under the delimiter.
        let out = run("| a‸ | b |\n|---|---|\n| 1 | 2 |", enter).unwrap();
        assert_eq!(
            out,
            "| a   | b   |\n| --- | --- |\n| ‸    |     |\n| 1   | 2   |"
        );
    }
    #[test]
    fn pasting_a_url_over_text_links_it() {
        let edit = paste_link("see docs", 4..8, " https://x.dev/a ").unwrap();
        assert_eq!(apply("see docs", &edit), "see [docs](https://x.dev/a)");
        assert!(paste_link("see docs", 4..8, "not a url").is_none());
        assert!(paste_link("see docs", 4..4, "https://x.dev").is_none());
        assert!(paste_link("a\nb", 0..3, "https://x.dev").is_none());
        // Spaces around the selection stay outside the link.
        let edit = paste_link("see docs now", 3..9, "https://x.dev").unwrap();
        assert_eq!(
            apply("see docs now", &edit),
            "see [docs](https://x.dev) now"
        );
    }

    #[test]
    fn places_images_on_their_own_line() {
        assert_eq!(
            image_markdown("", 0, "assets/a b.png", "a b"),
            "![a b](assets/a%20b.png)"
        );
        assert_eq!(
            image_markdown("text", 4, "assets/x.png", "x"),
            "\n\n![x](assets/x.png)\n"
        );
        // Right under a paragraph, a blank line keeps it apart.
        assert_eq!(image_markdown("text\n", 5, "a.png", ""), "\n![](a.png)");
        assert_eq!(image_markdown("text\n\n", 6, "a.png", ""), "![](a.png)");
    }
}
