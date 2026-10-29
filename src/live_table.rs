//! A table being edited in Live Preview, drawn as GPUI Kit draws tables
//! (columns sized to their text, the widest wrapping first), with the cell
//! holding the caret drawn by the editor. Clicking another cell edits it.

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, Entity, FontStyle, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Styled as _, TextRun, Window, component::ActiveTheme as _,
    div, prelude::FluentBuilder as _, px, rems,
};
use markdown::mdast::{AlignKind, Node};

use crate::{
    live::{Editable, Target, cell_range},
    live_editor::{self, LiveEditor, LiveEditorEvent},
    live_inline::{Display, Mode, Style},
};

#[derive(Debug, Clone)]
struct Cell {
    /// The cell's text in the document.
    range: Range<usize>,
    text: String,
}

#[derive(Debug, Clone)]
pub struct TableModel {
    rows: Vec<Vec<Cell>>,
    aligns: Vec<AlignKind>,
    /// The row and column being edited.
    active: (usize, usize),
}

/// The table `node` (from `source`, which starts at `offset` in the
/// document) with the cell `editable` edits.
pub fn model(node: &Node, source: &str, offset: usize, editable: &Editable) -> Option<TableModel> {
    let Node::Table(table) = node else {
        return None;
    };
    let Target::Cell { row, col, .. } = editable.target else {
        return None;
    };
    let rows = table
        .children
        .iter()
        .map(|row| {
            row.children()
                .map(|cells| {
                    cells
                        .iter()
                        .map(|cell| {
                            let range = cell_range(source, cell);
                            Cell {
                                text: source.get(range.clone()).unwrap_or_default().to_string(),
                                range: range.start + offset..range.end + offset,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    Some(TableModel {
        rows,
        aligns: table.align.clone(),
        active: (row, col),
    })
}

// Cell padding and border, as GPUI Kit's table draws them.
const CELL_PAD: f32 = 24.;
const CELL_BORDER: f32 = 1.;
const CELL_GROW: f32 = 1e6;
const SLACK_GROW: f32 = 1.0;

struct Column {
    max: f32,
    min: f32,
}

/// The width of text drawn with `styles`, on one line.
fn measure(text: &str, styles: &[(Range<usize>, Style)], window: &Window, cx: &App) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size()) * (14. / 15.);
    let base = style.font();
    let mono = cx.theme().mono_font_family.clone();
    let mut runs = Vec::new();
    let mut at = 0;
    let mut push = |len: usize, style: Style| {
        if len == 0 {
            return;
        }
        let mut font = base.clone();
        if style.bold {
            font.weight = FontWeight::BOLD;
        }
        if style.italic {
            font.style = FontStyle::Italic;
        }
        if style.code {
            font.family = mono.clone();
        }
        runs.push(TextRun {
            len,
            font,
            color: gpui_kit::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    };
    for (range, part) in styles {
        push(range.start.saturating_sub(at), Style::default());
        push(range.len(), *part);
        at = range.end;
    }
    push(text.len().saturating_sub(at), Style::default());
    let line = window
        .text_system()
        .shape_line(text.to_string().into(), font_size, &runs, None);
    f32::from(line.width)
}

pub fn render(
    model: &TableModel,
    editor: &Entity<LiveEditor>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let col_count = model.rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return div().into_any_element();
    }
    let body = window.text_style().font_size.to_pixels(window.rem_size());
    let text_size = body * (14. / 15.);

    // Each column as wide as its widest cell's text.
    let editing: Display = editor.read(cx).display();
    let mut widths = vec![0f32; col_count];
    for (row_ix, row) in model.rows.iter().enumerate() {
        for (col, cell) in row.iter().enumerate() {
            let width = if (row_ix, col) == model.active {
                measure(&editing.text, &editing.styles, window, cx)
            } else {
                let analysis = crate::live_inline::analyze(&cell.text, Mode::Prose);
                let display = Display::new(&cell.text, &analysis, None);
                measure(&display.text, &display.styles, window, cx)
            };
            widths[col] = widths[col].max(width + CELL_PAD + CELL_BORDER);
        }
    }
    let wrap_min = f32::from(rems(4.).to_pixels(window.rem_size()));
    let scale = window.scale_factor();
    let ceil = |width: f32| (width * scale).ceil() / scale;
    let columns: Vec<Column> = widths
        .iter()
        .enumerate()
        .map(|(ix, &width)| {
            let divider = if ix + 1 < col_count { CELL_BORDER } else { 0. };
            let max = ceil(width - divider);
            Column {
                min: ceil(width.min(wrap_min) - divider).min(max),
                max,
            }
        })
        .collect();

    let theme = cx.theme();
    let border = theme.border;
    let radius = (theme.radius - px(1.)).max(px(0.));
    let total = columns.iter().map(|column| column.max).sum::<f32>().max(1.);
    let row_count = model.rows.len();
    let mut rows = Vec::with_capacity(row_count);
    for (row_ix, row) in model.rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(col_count * 2);
        for (col, column) in columns.iter().enumerate() {
            let last_col = col + 1 == col_count;
            let align = model.aligns.get(col).copied().unwrap_or(AlignKind::None);
            let content = row.get(col).map(|cell| {
                if (row_ix, col) == model.active {
                    div().w_full().child(editor.clone()).into_any_element()
                } else {
                    cell_text(cell, editor, cx)
                }
            });
            let grow = SLACK_GROW * column.max / total;
            let (lead, trail) = match align {
                AlignKind::Center => (grow / 2., grow / 2.),
                AlignKind::Right => (grow, 0.),
                AlignKind::Left | AlignKind::None => (0., grow),
            };
            let slack = |grow: f32| {
                div()
                    .flex_basis(px(0.))
                    .flex_grow(grow)
                    .flex_shrink(0.)
                    .border_color(border)
            };
            if lead > 0. {
                cells.push(slack(lead).into_any_element());
            }
            cells.push(
                div()
                    .flex_basis(px(0.))
                    .flex_grow(CELL_GROW)
                    .min_w(px(column.min))
                    .max_w(px(column.max))
                    .overflow_hidden()
                    .when(align == AlignKind::Center, |this| this.text_center())
                    .when(align == AlignKind::Right, |this| this.text_right())
                    .px_3()
                    .py(px(7.))
                    .children(content)
                    .into_any_element(),
            );
            if trail > 0. || !last_col {
                cells.push(
                    slack(trail)
                        .when(!last_col, |this| this.border_r_1())
                        .into_any_element(),
                );
            }
        }
        rows.push(
            div()
                .w_full()
                .flex()
                .flex_row()
                .when(row_ix + 1 < row_count, |this| this.border_b_1())
                .border_color(border)
                .when(row_ix == 0, |this| {
                    this.bg(theme.table_head)
                        .text_color(theme.table_head_foreground)
                        .rounded_tl(radius)
                        .rounded_tr(radius)
                })
                .when(row_ix + 1 == row_count, |this| {
                    this.rounded_bl(radius).rounded_br(radius)
                })
                .children(cells)
                .into_any_element(),
        );
    }
    div()
        .w_full()
        .child(
            div()
                .w_full()
                .overflow_hidden()
                .text_size(text_size)
                .bg(theme.popover)
                .border_1()
                .border_color(border)
                .rounded(theme.radius)
                .children(rows),
        )
        .into_any_element()
}

/// A cell not being edited: its text formatted; a click edits it there.
fn cell_text(cell: &Cell, editor: &Entity<LiveEditor>, cx: &App) -> AnyElement {
    let (text, display) = live_editor::styled_text(&cell.text, Mode::Prose, cx);
    let layout = text.layout().clone();
    let start = cell.range.start;
    let editor = editor.downgrade();
    div()
        .w_full()
        .child(text)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let index = match layout.index_for_position(event.position) {
                Ok(index) | Err(index) => index,
            };
            let offset = start + display.to_source(index.min(display.text.len()));
            _ = editor.update(cx, |_, cx| cx.emit(LiveEditorEvent::Activate(offset)));
            cx.stop_propagation();
        })
        .into_any_element()
}
