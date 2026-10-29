//! Live Preview's editor: a piece of Markdown drawn as the preview draws
//! it, edited where it stands. Its marks (`**`, `[…](…)`, `` ` ``) show only
//! around the caret; see [`crate::live_inline`].
//!
//! What it edits is drawn by GPUI Kit's own Markdown renderer, as the
//! preview draws it: the editor hands it the paragraph, heading or table as
//! a block tree, and gets back where each character went, for the caret,
//! clicks and input methods.

use std::{
    collections::HashMap,
    ops::{Range, RangeInclusive},
    time::Duration,
};

use gpui_kit::{
    AnyElement, App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, UTF16Selection, Window,
    base::{
        actions::{SelectDown, SelectLeft, SelectRight, SelectUp},
        text::editor_host::{
            BlockNode, CodeBlock, ColumnAlign, FlowPosition, InlineNode, LeafGeometry, LinkMark,
            Paragraph, RenderContext, Table, TextMark, collect_leaf_geometry,
        },
    },
    component::{
        ActiveTheme as _, Icon, IconName, Sizable as _,
        input::{
            Backspace, Copy, Cut, Delete, DeleteToBeginningOfLine, DeleteToEndOfLine,
            DeleteToNextWordEnd, DeleteToPreviousWordStart, Enter, MoveDown, MoveEnd, MoveHome,
            MoveLeft, MoveRight, MoveToEnd, MoveToEndOfLine, MoveToNextWord, MoveToPreviousWord,
            MoveToStart, MoveToStartOfLine, MoveUp, Paste, SelectAll, SelectToEnd,
            SelectToEndOfLine, SelectToNextWordEnd, SelectToPreviousWordStart, SelectToStart,
            SelectToStartOfLine,
        },
        menu::ContextMenuExt as _,
        tooltip::Tooltip,
    },
    div, fill, point, px, size,
};
use markdown::mdast::AlignKind;

use crate::{
    actions::{ClearCells, DeleteCells, FollowLink},
    live::Neighbor,
    live_inline::{self, Analysis, Display, Mode, Style},
    live_table::TableModel,
    smart_edit::TableChange,
};

const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// The key the text being edited is laid out under.
const ACTIVE_LEAF: u64 = 1;

/// The key a table cell not being edited is laid out under.
fn cell_leaf(row: usize, col: usize) -> u64 {
    2 + ((row as u64) << 16) + col as u64
}

/// What the editor draws around the text it edits.
#[derive(Debug, Clone)]
pub enum Frame {
    /// A paragraph, or text edited as written.
    Text,
    /// A heading, after a block of this kind.
    Heading { level: u8, before: Neighbor },
    /// A code block, fences and all.
    Code,
    /// A table; the text is one of its cells.
    Table(TableModel),
}

/// A table cell not being edited, drawn by the editor: where its text
/// starts in the document, and how it was drawn.
struct Cell {
    key: u64,
    row: usize,
    col: usize,
    start: usize,
    display: Display,
}

pub enum LiveEditorEvent {
    /// The text changed.
    Changed,
    /// Edit the document at this offset: an unedited table cell or list
    /// item was clicked.
    Activate(usize),
    /// Change the table's rows and columns, then edit the cell at this row
    /// and column.
    Table {
        change: TableChange,
        focus: (usize, usize),
    },
}

/// The padding of a table cell, as GPUI Kit draws it.
const CELL_PADDING_X: f32 = 12.;
const CELL_PADDING_Y: f32 = 7.;

/// Where to put the caret once the editor is drawn.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub x: Pixels,
    /// On the last row (coming from below) or the first.
    pub last_row: bool,
}

pub struct LiveEditor {
    focus_handle: FocusHandle,
    text: String,
    mode: Mode,
    analysis: Analysis,
    selection: Range<usize>,
    /// The caret is at the selection's start.
    reversed: bool,
    /// Text being composed with an input method.
    marked: Option<Range<usize>>,
    /// The column up and down keep to.
    goal_x: Option<Pixels>,
    /// What continuation lines start with: a list item's indentation, a
    /// quote's `> `.
    line_prefix: String,
    /// What is drawn around the text, and where the text starts in the
    /// document.
    frame: Frame,
    origin: usize,
    /// Where the last frame laid out the text, and how it maps.
    drawn: Option<(LeafGeometry, Display)>,
    /// The table cells drawn besides, and where they were laid out.
    cells: Vec<Cell>,
    cell_geometry: HashMap<u64, LeafGeometry>,
    /// Where the editor was drawn.
    bounds: Option<Bounds<Pixels>>,
    /// The other paragraphs of the list or quote being edited, drawn the
    /// same way, by where they start in the document: where they were laid
    /// out and how they map, for clicks on them.
    companions: HashMap<usize, (LeafGeometry, Display)>,
    /// Drawn as the preview draws it, marks and caret hidden: opened to
    /// learn where a click on it landed.
    concealed: bool,
    /// The table cells selected by dragging: where the drag began and where
    /// it is, by row and column.
    cell_anchor: Option<(usize, usize)>,
    cell_head: Option<(usize, usize)>,
    /// A button of the editor's own (adding a table row or column) was
    /// pressed, which isn't a click on the text.
    control_press: bool,
    selecting: bool,
    caret_on: bool,
    blink: Option<Task<()>>,
    placement: Option<Placement>,
}

impl EventEmitter<LiveEditorEvent> for LiveEditor {}

impl Focusable for LiveEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl LiveEditor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            text: String::new(),
            mode: Mode::Prose,
            analysis: Analysis::default(),
            selection: 0..0,
            reversed: false,
            marked: None,
            goal_x: None,
            line_prefix: String::new(),
            frame: Frame::Text,
            origin: 0,
            drawn: None,
            cells: Vec::new(),
            cell_geometry: HashMap::new(),
            bounds: None,
            companions: HashMap::new(),
            concealed: false,
            cell_anchor: None,
            cell_head: None,
            control_press: false,
            selecting: false,
            caret_on: true,
            blink: None,
            placement: None,
        }
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    /// The text drawn, as it is now.
    pub fn display(&self) -> Display {
        Display::new(&self.text, &self.analysis, Some(&self.selection))
    }

    /// Where the text being edited starts in the document.
    pub fn origin(&self) -> usize {
        self.origin
    }

    /// Edit `text`, which starts at `origin` in the document, drawn as
    /// `mode` in `frame`. No change event.
    pub fn set_value(
        &mut self,
        text: String,
        origin: usize,
        mode: Mode,
        frame: Frame,
        line_prefix: String,
        cx: &mut Context<Self>,
    ) {
        // A drag across a table's cells goes on as the next cell opens.
        if !matches!(frame, Frame::Table(_)) {
            self.cell_anchor = None;
            self.selecting = false;
        }
        self.cell_head = None;
        self.text = text;
        self.origin = origin;
        self.frame = frame;
        self.companions.clear();
        self.mode = mode;
        self.line_prefix = line_prefix;
        self.analysis = live_inline::analyze(&self.text, mode);
        self.selection = 0..0;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        self.placement = None;
        self.drawn = None;
        self.bounds = None;
        self.concealed = false;
        cx.notify();
    }

    pub fn selected_range(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn set_selected_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let len = self.text.len();
        let clamp = |offset: usize| self.floor(offset.min(len));
        self.selection = clamp(range.start)..clamp(range.end);
        self.reversed = false;
        self.goal_x = None;
        self.show_caret(cx);
    }

    /// Put the caret at `x` on the first or last row once drawn.
    pub fn place(&mut self, placement: Placement, cx: &mut Context<Self>) {
        self.placement = Some(placement);
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    fn floor(&self, mut offset: usize) -> usize {
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    // ---------------------------------------------------------------------
    // Geometry, from the last frame

    /// The caret's place in the window, if drawn.
    fn caret_point(&self) -> Option<Point<Pixels>> {
        let (geometry, display) = self.drawn.as_ref()?;
        Some(geometry.position_for_index(display.to_display(self.cursor())))
    }

    fn line_height(&self) -> Option<Pixels> {
        Some(self.drawn.as_ref()?.0.line_height())
    }

    /// Whether the caret is on the first (or last) row, as wrapped.
    pub fn on_edge_row(&self, first: bool) -> bool {
        let (Some(caret), Some(line_height), Some((layout, _))) =
            (self.caret_point(), self.line_height(), self.drawn.as_ref())
        else {
            return true;
        };
        let bounds = layout.bounds();
        if first {
            caret.y - bounds.top() < line_height * 0.5
        } else {
            bounds.bottom() - (caret.y + line_height) < line_height * 0.5
        }
    }

    /// Draw what's edited as the preview draws it, marks and caret hidden,
    /// until opened again.
    pub fn conceal(&mut self, cx: &mut Context<Self>) {
        self.concealed = true;
        cx.notify();
    }

    /// The document offset drawn at `point`: in the text edited, a table
    /// cell or another paragraph of the list or quote, whichever is nearest.
    pub fn locate(&self, point: Point<Pixels>) -> Option<usize> {
        let distance = |bounds: Bounds<Pixels>| {
            let dy = if point.y < bounds.top() {
                bounds.top() - point.y
            } else if point.y > bounds.bottom() {
                point.y - bounds.bottom()
            } else {
                px(0.)
            };
            let dx = if point.x < bounds.left() {
                bounds.left() - point.x
            } else if point.x > bounds.right() {
                point.x - bounds.right()
            } else {
                px(0.)
            };
            (dy, dx)
        };
        let mut candidates: Vec<(&LeafGeometry, &Display, usize)> = Vec::new();
        if let Some((geometry, display)) = &self.drawn {
            candidates.push((geometry, display, self.origin));
        }
        for cell in &self.cells {
            if let Some(geometry) = self.cell_geometry.get(&cell.key) {
                candidates.push((geometry, &cell.display, cell.start));
            }
        }
        for (start, (geometry, display)) in &self.companions {
            candidates.push((geometry, display, *start));
        }
        let (geometry, display, start) = candidates.into_iter().min_by(|a, b| {
            distance(a.0.bounds())
                .partial_cmp(&distance(b.0.bounds()))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let index = geometry.index_for_position(point);
        Some(start + display.to_source(index.min(display.text.len())))
    }

    /// Where another paragraph of the list or quote was laid out.
    pub fn companion_drawn(&mut self, start: usize, geometry: LeafGeometry, display: Display) {
        self.companions.insert(start, (geometry, display));
    }

    /// Whether a button of the editor's own was pressed since last asked.
    pub fn take_control_press(&mut self) -> bool {
        std::mem::take(&mut self.control_press)
    }

    /// The table cell nearest `point` (when a table is being edited): its
    /// row and column, and the document offset nearest the point in it.
    fn cell_at(&self, point: Point<Pixels>) -> Option<((usize, usize), usize)> {
        let Frame::Table(model) = &self.frame else {
            return None;
        };
        let distance = |bounds: Bounds<Pixels>| {
            let dy = (bounds.top() - point.y)
                .max(point.y - bounds.bottom())
                .max(px(0.));
            let dx = (bounds.left() - point.x)
                .max(point.x - bounds.right())
                .max(px(0.));
            (dy, dx)
        };
        let mut candidates: Vec<((usize, usize), &LeafGeometry, &Display, usize)> = Vec::new();
        if let Some((geometry, display)) = &self.drawn {
            candidates.push((model.active, geometry, display, self.origin));
        }
        for cell in &self.cells {
            if let Some(geometry) = self.cell_geometry.get(&cell.key) {
                candidates.push(((cell.row, cell.col), geometry, &cell.display, cell.start));
            }
        }
        let (cell, geometry, display, start) = candidates.into_iter().min_by(|a, b| {
            distance(a.1.bounds())
                .partial_cmp(&distance(b.1.bounds()))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let index = geometry.index_for_position(point);
        Some((
            cell,
            start + display.to_source(index.min(display.text.len())),
        ))
    }

    /// The cells selected by dragging, as rows and columns.
    fn selected_cells(&self) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        let (anchor, head) = (self.cell_anchor?, self.cell_head?);
        (anchor != head).then(|| {
            (
                anchor.0.min(head.0)..=anchor.0.max(head.0),
                anchor.1.min(head.1)..=anchor.1.max(head.1),
            )
        })
    }

    /// Where the selected cells were drawn, padding included.
    fn selected_cell_bounds(&self) -> Vec<Bounds<Pixels>> {
        let (Some((rows, cols)), Frame::Table(model), Some(table)) =
            (self.selected_cells(), &self.frame, self.bounds)
        else {
            return Vec::new();
        };
        let leaf = |row: usize, col: usize| -> Option<Bounds<Pixels>> {
            if (row, col) == model.active {
                return Some(self.drawn.as_ref()?.0.bounds());
            }
            let cell = self
                .cells
                .iter()
                .find(|cell| (cell.row, cell.col) == (row, col))?;
            Some(self.cell_geometry.get(&cell.key)?.bounds())
        };
        let columns = model.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return Vec::new();
        }
        // Each column's text spans this much; where a column's cells start
        // and end follows from their alignment, as text sits at the padding
        // on the side it's aligned to.
        // (start, end, whether every cell's text box spans the column).
        let spans: Vec<Option<(Pixels, Pixels, bool)>> = (0..columns)
            .map(|col| {
                let all: Vec<Bounds<Pixels>> = (0..model.rows.len())
                    .filter_map(|row| leaf(row, col))
                    .collect();
                let start = all.iter().map(|bounds| bounds.left()).reduce(Pixels::min)?;
                let end = all
                    .iter()
                    .map(|bounds| bounds.right())
                    .reduce(Pixels::max)?;
                let full = all
                    .iter()
                    .all(|bounds| (bounds.right() - end).abs() < px(0.5));
                Some((start, end, full))
            })
            .collect();
        let align = |col: usize| model.aligns.get(col).copied().unwrap_or(AlignKind::None);
        let pad = px(CELL_PADDING_X);
        let mut edges = vec![table.left() + px(1.)];
        for col in 0..columns - 1 {
            let (left, right) = (spans[col], spans[col + 1]);
            let edge = match (align(col), align(col + 1), left, right) {
                (_, AlignKind::Left | AlignKind::None, _, Some((start, _, _))) => start - pad,
                (AlignKind::Right, _, Some((_, end, _)), _) => end + pad,
                (AlignKind::Left | AlignKind::None, _, Some((_, end, true)), _) => end + pad,
                (_, _, Some((_, end, _)), Some((start, _, _))) => (end + start) / 2.,
                _ => *edges.last().expect("one edge at least"),
            };
            edges.push(edge);
        }
        edges.push(table.right() - px(1.));
        let mut out = Vec::new();
        for row in rows {
            // A row is as tall as its tallest cell.
            let in_row: Vec<Bounds<Pixels>> =
                (0..columns).filter_map(|col| leaf(row, col)).collect();
            let (Some(top), Some(bottom)) = (
                in_row.iter().map(|bounds| bounds.top()).reduce(Pixels::min),
                in_row
                    .iter()
                    .map(|bounds| bounds.bottom())
                    .reduce(Pixels::max),
            ) else {
                continue;
            };
            for col in cols.clone() {
                if col + 1 < edges.len() {
                    out.push(Bounds::from_corners(
                        point(edges[col], top - px(CELL_PADDING_Y)),
                        point(edges[col + 1], bottom + px(CELL_PADDING_Y)),
                    ));
                }
            }
        }
        out
    }

    /// The cells selected, or the one being edited.
    fn cells_acted_on(&self) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        let Frame::Table(model) = &self.frame else {
            return None;
        };
        self.selected_cells().or(Some((
            model.active.0..=model.active.0,
            model.active.1..=model.active.1,
        )))
    }

    fn change_table(&mut self, delete: bool, cx: &mut Context<Self>) {
        let Some((rows, columns)) = self.cells_acted_on() else {
            return;
        };
        let focus = (*rows.start(), *columns.start());
        let change = if delete {
            TableChange::Delete { rows, columns }
        } else {
            TableChange::Clear { rows, columns }
        };
        self.cell_anchor = None;
        self.cell_head = None;
        cx.emit(LiveEditorEvent::Table { change, focus });
    }

    fn delete_cells(&mut self, _: &DeleteCells, _: &mut Window, cx: &mut Context<Self>) {
        self.change_table(true, cx);
    }

    fn clear_cells(&mut self, _: &ClearCells, _: &mut Window, cx: &mut Context<Self>) {
        self.change_table(false, cx);
    }

    /// Whether table cells are selected (and keys act on them).
    pub fn has_cell_selection(&self) -> bool {
        self.selected_cells().is_some()
    }

    /// Whether the editor was drawn at `point` (and handles a click there).
    pub fn contains(&self, point: Point<Pixels>) -> bool {
        self.bounds.is_some_and(|bounds| bounds.contains(&point))
    }

    /// Where the caret is drawn, if it is.
    pub fn caret_bounds(&self) -> Option<Bounds<Pixels>> {
        Some(Bounds::new(
            self.caret_point()?,
            size(px(2.), self.line_height()?),
        ))
    }

    /// Where the text is drawn, if it is.
    pub fn text_bounds(&self) -> Option<Bounds<Pixels>> {
        Some(self.drawn.as_ref()?.0.bounds())
    }

    /// The caret's distance from the left of the text.
    pub fn caret_x(&self) -> Option<Pixels> {
        let (layout, _) = self.drawn.as_ref()?;
        Some(self.caret_point()?.x - layout.bounds().left())
    }

    /// The source offset at a point in the window.
    fn offset_at(&self, position: Point<Pixels>) -> Option<usize> {
        let (geometry, display) = self.drawn.as_ref()?;
        let index = geometry.index_for_position(position);
        Some(display.to_source(index.min(display.text.len())))
    }

    /// The source offset on the row at `y` (relative to the caret's row,
    /// in rows) nearest `x`.
    fn offset_on_row(&self, rows: f32, x: Pixels) -> Option<usize> {
        let caret = self.caret_point()?;
        let line_height = self.line_height()?;
        let (layout, _) = self.drawn.as_ref()?;
        let y = caret.y + line_height * (rows + 0.5);
        let bounds = layout.bounds();
        if y < bounds.top() || y > bounds.bottom() {
            return None;
        }
        self.offset_at(point(bounds.left() + x, y))
    }

    // ---------------------------------------------------------------------
    // Editing

    fn replace(&mut self, range: Range<usize>, new: &str, cx: &mut Context<Self>) {
        let range = self.floor(range.start.min(self.text.len()))
            ..self.floor(range.end.min(self.text.len()));
        self.text.replace_range(range.clone(), new);
        self.cell_head = None;
        let caret = range.start + new.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        self.analysis = live_inline::analyze(&self.text, self.mode);
        self.show_caret(cx);
        cx.emit(LiveEditorEvent::Changed);
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selection = offset..offset;
        self.reversed = false;
        self.show_caret(cx);
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let anchor = if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        if offset < anchor {
            self.selection = offset..anchor;
            self.reversed = true;
        } else {
            self.selection = anchor..offset;
            self.reversed = false;
        }
        self.show_caret(cx);
    }

    fn previous(&self, offset: usize) -> usize {
        live_inline::previous_offset(&self.text, &self.analysis, offset)
    }

    fn next(&self, offset: usize) -> usize {
        live_inline::next_offset(&self.text, &self.analysis, offset)
    }

    /// The start and end of the caret's row.
    fn row_bounds(&self) -> Option<(usize, usize)> {
        let (layout, _) = self.drawn.as_ref()?;
        let caret = self.caret_point()?;
        let line_height = self.line_height()?;
        let y = caret.y + line_height * 0.5;
        let bounds = layout.bounds();
        let start = self.offset_at(point(bounds.left() - px(1.), y))?;
        let end = self.offset_at(point(bounds.right() + px(10_000.), y))?;
        Some((start, end))
    }

    // ---------------------------------------------------------------------
    // Blinking

    fn show_caret(&mut self, cx: &mut Context<Self>) {
        self.caret_on = true;
        let epoch = cx.entity().downgrade();
        self.blink = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let alive = epoch
                    .update(cx, |this, cx| {
                        this.caret_on = !this.caret_on;
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        }));
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Actions

    fn left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selection.is_empty() {
            self.previous(self.selection.start)
        } else {
            self.selection.start
        };
        self.goal_x = None;
        self.move_to(target, cx);
    }

    fn right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selection.is_empty() {
            self.next(self.selection.end)
        } else {
            self.selection.end
        };
        self.goal_x = None;
        self.move_to(target, cx);
    }

    fn vertical(&mut self, rows: f32, select: bool, cx: &mut Context<Self>) {
        let x = self.goal_x.or_else(|| self.caret_x()).unwrap_or_default();
        let target =
            self.offset_on_row(rows, x)
                .unwrap_or(if rows < 0. { 0 } else { self.text.len() });
        if select {
            self.select_to(target, cx);
        } else {
            self.move_to(target, cx);
        }
        self.goal_x = Some(x);
    }

    fn up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., false, cx);
    }

    fn down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., false, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., true, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.previous(self.cursor());
        self.goal_x = None;
        self.select_to(target, cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        let target = self.next(self.cursor());
        self.goal_x = None;
        self.select_to(target, cx);
    }

    fn home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        let start = self.row_bounds().map_or(0, |(start, _)| start);
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.row_bounds().map_or(self.text.len(), |(_, end)| end);
        self.move_to(end, cx);
    }

    fn line_start(&mut self, _: &MoveToStartOfLine, window: &mut Window, cx: &mut Context<Self>) {
        self.home(&MoveHome, window, cx);
    }

    fn line_end(&mut self, _: &MoveToEndOfLine, window: &mut Window, cx: &mut Context<Self>) {
        self.end(&MoveEnd, window, cx);
    }

    fn select_line_start(
        &mut self,
        _: &SelectToStartOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = self.row_bounds().map_or(0, |(start, _)| start);
        self.select_to(start, cx);
    }

    fn select_line_end(&mut self, _: &SelectToEndOfLine, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.row_bounds().map_or(self.text.len(), |(_, end)| end);
        self.select_to(end, cx);
    }

    fn start(&mut self, _: &MoveToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn finish(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), cx);
    }

    fn select_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.text.len(), cx);
    }

    fn word_left(&mut self, _: &MoveToPreviousWord, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(live_inline::previous_word(&self.text, self.cursor()), cx);
    }

    fn word_right(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(live_inline::next_word(&self.text, self.cursor()), cx);
    }

    fn select_word_left(
        &mut self,
        _: &SelectToPreviousWordStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(live_inline::previous_word(&self.text, self.cursor()), cx);
    }

    fn select_word_right(
        &mut self,
        _: &SelectToNextWordEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(live_inline::next_word(&self.text, self.cursor()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.text.len();
        self.reversed = false;
        self.show_caret(cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.has_cell_selection() {
            self.change_table(false, cx);
            return;
        }
        let range = if self.selection.is_empty() {
            self.previous(self.selection.start)..self.selection.start
        } else {
            self.selection.clone()
        };
        if !range.is_empty() {
            self.replace(range, "", cx);
        }
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.has_cell_selection() {
            self.change_table(false, cx);
            return;
        }
        let range = if self.selection.is_empty() {
            self.selection.end..self.next(self.selection.end)
        } else {
            self.selection.clone()
        };
        if !range.is_empty() {
            self.replace(range, "", cx);
        }
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteToPreviousWordStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selection.is_empty() {
            live_inline::previous_word(&self.text, self.selection.start)..self.selection.start
        } else {
            self.selection.clone()
        };
        self.replace(range, "", cx);
    }

    fn delete_word_right(
        &mut self,
        _: &DeleteToNextWordEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selection.is_empty() {
            self.selection.end..live_inline::next_word(&self.text, self.selection.end)
        } else {
            self.selection.clone()
        };
        self.replace(range, "", cx);
    }

    fn delete_line_left(
        &mut self,
        _: &DeleteToBeginningOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = self.row_bounds().map_or(0, |(start, _)| start);
        self.replace(start.min(self.selection.start)..self.selection.end, "", cx);
    }

    fn delete_line_right(&mut self, _: &DeleteToEndOfLine, _: &mut Window, cx: &mut Context<Self>) {
        let end = self.row_bounds().map_or(self.text.len(), |(_, end)| end);
        self.replace(self.selection.start..end.max(self.selection.end), "", cx);
    }

    /// Enter starts a new paragraph (in prose, where a lone line break
    /// would only show as a space); Shift-Enter breaks the line.
    fn enter(&mut self, action: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        let prefix = self.line_prefix.clone();
        let text = match (self.mode, action.shift) {
            (Mode::Raw { .. }, _) => format!("\n{prefix}"),
            (_, true) => format!("\\\n{prefix}"),
            (_, false) => format!("\n{}\n{prefix}", prefix.trim_end()),
        };
        self.replace(self.selection.clone(), &text, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            self.copy(&Copy, window, cx);
            self.replace(self.selection.clone(), "", cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n");
            self.replace(self.selection.clone(), &text, cx);
        }
    }

    // ---------------------------------------------------------------------
    // Mouse

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // In a table, the cell nearest the click (on its text or not);
        // another one is edited there, and a drag from it selects cells.
        let mut offset = None;
        if let Frame::Table(model) = &self.frame
            && let Some((cell, at)) = self.cell_at(event.position)
        {
            self.cell_anchor = Some(cell);
            self.cell_head = None;
            if cell != model.active {
                self.selecting = true;
                cx.emit(LiveEditorEvent::Activate(at));
                cx.stop_propagation();
                return;
            }
            offset = Some(at - self.origin);
        }
        let Some(offset) = offset.or_else(|| self.offset_at(event.position)) else {
            return;
        };
        // Command- or Control-click follows a link, as a click does in the
        // preview; a plain click edits it.
        if event.modifiers.secondary(cx)
            && let Some((_, url)) = self
                .analysis
                .links
                .iter()
                .find(|(range, _)| range.start <= offset && offset < range.end)
        {
            window.dispatch_action(
                Box::new(FollowLink {
                    url: url.clone().into(),
                    alternate: false,
                }),
                cx,
            );
            cx.stop_propagation();
            return;
        }
        self.focus(window, cx);
        self.goal_x = None;
        match event.click_count {
            2 => {
                self.selection = live_inline::word_at(&self.text, offset);
                self.reversed = false;
                self.show_caret(cx);
            }
            count if count >= 3 => {
                self.selection = 0..self.text.len();
                self.reversed = false;
                self.show_caret(cx);
            }
            _ if event.modifiers.shift => self.select_to(offset, cx),
            _ => self.move_to(offset, cx),
        }
        self.selecting = true;
        cx.stop_propagation();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        // Dragged into another table cell: select cells.
        if let Some(anchor) = self.cell_anchor
            && let Some((cell, _)) = self.cell_at(event.position)
        {
            let head = (cell != anchor).then_some(cell);
            if head != self.cell_head {
                self.cell_head = head;
                cx.notify();
            }
            if head.is_some() {
                return;
            }
        }
        if let Some(offset) = self.offset_at(event.position) {
            self.select_to(offset, cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    // ---------------------------------------------------------------------
    // Drawing

    /// Called while painting, with what was drawn.
    fn drawn(
        &mut self,
        bounds: Bounds<Pixels>,
        mut leaves: HashMap<u64, LeafGeometry>,
        cx: &mut Context<Self>,
    ) {
        self.bounds = Some(bounds);
        let display = if self.concealed {
            Display::new(&self.text, &self.analysis, None)
        } else {
            self.display()
        };
        self.drawn = leaves
            .remove(&ACTIVE_LEAF)
            .map(|geometry| (geometry, display));
        self.cell_geometry = leaves;
        if let Some(placement) = self.placement.take() {
            let rows = if placement.last_row {
                let (layout, _) = self.drawn.as_ref().expect("just set");
                let line_height = layout.line_height();
                let caret_y = self.caret_point().map_or(px(0.), |caret| caret.y);
                ((layout.bounds().bottom() - line_height - caret_y) / line_height).round()
            } else {
                let (layout, _) = self.drawn.as_ref().expect("just set");
                let caret_y = self.caret_point().map_or(px(0.), |caret| caret.y);
                ((layout.bounds().top() - caret_y) / layout.line_height()).round()
            };
            if let Some(offset) = self.offset_on_row(rows, placement.x) {
                self.selection = offset..offset;
                self.reversed = false;
                self.goal_x = Some(placement.x);
                cx.notify();
            }
        }
    }
}

/// The marks GPUI Kit formats `style` with.
fn text_mark(style: &Style, highlight: Hsla) -> TextMark {
    let mut mark = TextMark {
        bold: style.bold,
        italic: style.italic,
        strikethrough: style.strike,
        underline: style.underline,
        code: style.code,
        ..TextMark::default()
    };
    if style.link {
        mark.link = Some(LinkMark {
            url: "#".into(),
            ..LinkMark::default()
        });
    }
    if style.mark {
        mark.highlight = Some(highlight);
    }
    mark
}

/// The text drawn, as a paragraph for GPUI Kit to lay out under `key`.
pub fn paragraph(display: &Display, key: u64, highlight: Hsla) -> Paragraph {
    let marks = display
        .styles
        .iter()
        .filter_map(|(range, style)| {
            let mark = text_mark(style, highlight);
            (mark != TextMark::default()).then(|| (range.clone(), mark))
        })
        .collect();
    let mut paragraph = Paragraph::default();
    paragraph.push(InlineNode::new(display.text.clone()).marks(marks));
    paragraph.with_leaf_key(key)
}

/// Where a block stands: after a block of the kind `before`, ending its
/// flow (the preview leaves the gap after it).
pub fn render_after(
    block: &BlockNode,
    before: Neighbor,
    render: &RenderContext,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let previous = match before {
        Neighbor::None => None,
        Neighbor::Heading(level) => Some(BlockNode::Heading {
            level,
            children: Paragraph::default(),
            span: None,
        }),
        Neighbor::Rule => Some(BlockNode::HorizontalRule { span: None }),
        Neighbor::Other => Some(BlockNode::Paragraph(Paragraph::default())),
    };
    let flow = match &previous {
        Some(previous) => FlowPosition::at(&[previous, block], 1, true),
        None => FlowPosition::at(&[block], 0, true),
    };
    block.render(0, flow, render, window, cx)
}

/// How the preview renders, with link clicks left to the editor.
pub fn render_context(window: &Window, cx: &App) -> RenderContext {
    let body = window.text_style().font_size.to_pixels(window.rem_size());
    RenderContext::from_app(cx)
        .with_body_font_size(body)
        .on_link_click(|_, _, _, _| {})
}

fn column_align(align: AlignKind) -> ColumnAlign {
    match align {
        AlignKind::Left => ColumnAlign::Left,
        AlignKind::Center => ColumnAlign::Center,
        AlignKind::Right => ColumnAlign::Right,
        AlignKind::None => ColumnAlign::None,
    }
}

/// The table `model` as the editor draws it, with `active` in the cell
/// being edited, and every other cell a leaf of its own. A leaf is laid out
/// even when empty, so a row of empty cells keeps the height of a line.
fn table_with(
    model: &TableModel,
    active: Option<&Paragraph>,
    highlight: Hsla,
) -> (BlockNode, Vec<Cell>) {
    let mut rows = Vec::with_capacity(model.rows.len());
    let mut leaves = Vec::new();
    for (row_ix, row) in model.rows.iter().enumerate() {
        let mut cells = Vec::with_capacity(row.len());
        for (col, cell) in row.iter().enumerate() {
            if let Some(active) = active.filter(|_| (row_ix, col) == model.active) {
                cells.push(active.clone());
                continue;
            }
            let analysis = live_inline::analyze(&cell.text, Mode::Prose);
            let shown = Display::new(&cell.text, &analysis, None);
            let key = cell_leaf(row_ix, col);
            cells.push(paragraph(&shown, key, highlight));
            leaves.push(Cell {
                key,
                row: row_ix,
                col,
                start: cell.range.start,
                display: shown,
            });
        }
        rows.push(cells);
    }
    let aligns = model.aligns.iter().copied().map(column_align).collect();
    (BlockNode::Table(Table::new(rows, aligns)), leaves)
}

/// The table `model`, with no cell being edited, as the editor draws it.
pub fn table(model: &TableModel, highlight: Hsla) -> BlockNode {
    table_with(model, None, highlight).0
}

impl Render for LiveEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window) && !self.concealed;
        let display = if self.concealed {
            Display::new(&self.text, &self.analysis, None)
        } else {
            self.display()
        };
        let theme = cx.theme();
        let (selection_color, caret_color) = (theme.selection, theme.caret);
        let highlight = theme.warning.opacity(0.3);
        let render = render_context(window, cx);
        let text = paragraph(&display, ACTIVE_LEAF, highlight);
        self.cells.clear();
        let (block, before) = match &self.frame {
            Frame::Text => (BlockNode::Paragraph(text), Neighbor::None),
            Frame::Heading { level, before } => (
                BlockNode::Heading {
                    level: *level,
                    children: text,
                    span: None,
                },
                *before,
            ),
            Frame::Code => (
                BlockNode::CodeBlock(
                    CodeBlock::from_code(display.text.clone(), None::<SharedString>)
                        .with_leaf_key(ACTIVE_LEAF),
                ),
                Neighbor::None,
            ),
            Frame::Table(model) => {
                let (table, cells) = table_with(model, Some(&text), highlight);
                self.cells = cells;
                (table, Neighbor::None)
            }
        };
        let element = render_after(&block, before, &render, window, cx);
        let selection =
            display.to_display(self.selection.start)..display.to_display(self.selection.end);
        let mut decorations = HashMap::new();
        if !selection.is_empty() && !self.concealed {
            decorations.insert(ACTIVE_LEAF, vec![(selection, selection_color)]);
        }
        let caret = (focused && self.caret_on && !self.has_cell_selection())
            .then(|| (display.to_display(self.cursor()), caret_color));
        let table = matches!(self.frame, Frame::Table(_));
        let probe = Probe {
            child: Some(element),
            decorations,
            editor: cx.entity(),
            caret,
            selection: selection_color,
        };
        let content = if table {
            self.table_controls(probe, cx).into_any_element()
        } else {
            probe.into_any_element()
        };

        let editor = div()
            .id("live-editor")
            .key_context("Input")
            .track_focus(&self.focus_handle)
            .w_full()
            .cursor_text()
            .on_action(cx.listener(Self::delete_cells))
            .on_action(cx.listener(Self::clear_cells))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::start))
            .on_action(cx.listener(Self::finish))
            .on_action(cx.listener(Self::select_start))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_line_left))
            .on_action(cx.listener(Self::delete_line_right))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(content);
        if table {
            editor
                .context_menu(|menu, _, _| {
                    menu.menu(tr!("table.delete_cells"), Box::new(DeleteCells))
                        .menu(tr!("table.clear_cells"), Box::new(ClearCells))
                })
                .into_any_element()
        } else {
            editor.into_any_element()
        }
    }
}

impl LiveEditor {
    /// A table with a strip at its right edge to add a column, shown when
    /// the pointer reaches it, and one below it to add a row, shown while
    /// the pointer is over the table.
    fn table_controls(&self, table: Probe, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (hover, radius) = (theme.muted, theme.radius);
        let control = |id: &'static str, label: SharedString, change: TableChange| {
            div()
                .id(id)
                .absolute()
                .flex()
                .items_center()
                .justify_center()
                .rounded(radius)
                .cursor_pointer()
                .opacity(0.)
                .hover(|style| style.opacity(1.).bg(hover))
                .child(Icon::new(IconName::Plus).xsmall())
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.control_press = true;
                        cx.stop_propagation();
                    }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Frame::Table(model) = &this.frame else {
                        return;
                    };
                    cx.emit(LiveEditorEvent::Table {
                        change: change.clone(),
                        focus: model.active,
                    });
                }))
        };
        div()
            .relative()
            .w_full()
            .group("live-table")
            .child(table)
            .child(
                // Over the last column's padding: the preview clips what
                // stands beside a block.
                control(
                    "add-column",
                    tr!("table.add_column"),
                    TableChange::AddColumn,
                )
                .top(px(1.))
                .bottom(px(1.))
                .right(px(1.))
                .w(px(12.)),
            )
            .child(
                control("add-row", tr!("table.add_row"), TableChange::AddRow)
                    .group_hover("live-table", |style| style.opacity(1.))
                    .left_0()
                    .right_0()
                    .bottom(-px(18.))
                    .h(px(16.)),
            )
    }
}

/// Lays out `child` and hands where its text leaves went to `drawn`.
pub struct LeafProbe {
    child: Option<AnyElement>,
    #[allow(clippy::type_complexity)]
    drawn: Option<Box<dyn FnOnce(HashMap<u64, LeafGeometry>, &mut App)>>,
}

impl LeafProbe {
    pub fn new(
        child: AnyElement,
        drawn: impl FnOnce(HashMap<u64, LeafGeometry>, &mut App) + 'static,
    ) -> Self {
        Self {
            child: Some(child),
            drawn: Some(Box::new(drawn)),
        }
    }
}

impl IntoElement for LeafProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LeafProbe {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let mut child = self.child.take().expect("laid out once");
        (child.request_layout(window, cx), child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (_, leaves) = collect_leaf_geometry(HashMap::new(), || child.prepaint(window, cx));
        if let Some(drawn) = self.drawn.take() {
            drawn(leaves, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

/// Lays out the editor's blocks, keeping where its text went, and paints
/// the caret over them.
struct Probe {
    child: Option<AnyElement>,
    decorations: HashMap<u64, Vec<(Range<usize>, Hsla)>>,
    editor: Entity<LiveEditor>,
    /// Where the caret is in the text drawn, and its color.
    caret: Option<(usize, Hsla)>,
    /// The color selected table cells are washed with.
    selection: Hsla,
}

impl IntoElement for Probe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Probe {
    type RequestLayoutState = AnyElement;
    /// The caret's top and height, and the selected table cells.
    type PrepaintState = (Option<(Point<Pixels>, Pixels)>, Vec<Bounds<Pixels>>);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let mut child = self.child.take().expect("laid out once");
        (child.request_layout(window, cx), child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let decorations = std::mem::take(&mut self.decorations);
        let (_, leaves) = collect_leaf_geometry(decorations, || child.prepaint(window, cx));
        let caret = self.caret.and_then(|(index, _)| {
            let geometry = leaves.get(&ACTIVE_LEAF)?;
            let at = geometry.position_for_index(index);
            let height = geometry.caret_height();
            let offset = (geometry.line_height() - height) / 2.;
            Some((point(at.x, at.y + offset), height))
        });
        let cells = self.editor.update(cx, |editor, cx| {
            editor.drawn(bounds, leaves, cx);
            editor.selected_cell_bounds()
        });
        (caret, cells)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut AnyElement,
        (caret, cells): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
        for cell in cells.iter() {
            window.paint_quad(fill(*cell, self.selection));
        }
        if let (Some((at, height)), Some((_, color))) = (*caret, self.caret) {
            window.paint_quad(fill(Bounds::new(at, size(px(2.), height)), color));
        }
        let focus = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
    }
}

// -------------------------------------------------------------------------
// Input methods

impl LiveEditor {
    fn to_utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .encode_utf16()
            .count()
    }

    fn byte_offset(&self, offset: usize) -> usize {
        utf16_to_byte(&self.text, offset)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.byte_offset(range.start)..self.byte_offset(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.to_utf16(range.start)..self.to_utf16(range.end)
    }
}

impl EntityInputHandler for LiveEditor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        adjusted_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selection),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        self.replace(range, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selection.clone());
        let start = range.start;
        self.replace(range, new_text, cx);
        if !new_text.is_empty() {
            self.marked = Some(start..start + new_text.len());
        }
        if let Some(selected) = new_selected_range {
            let byte = |units: usize| utf16_to_byte(new_text, units);
            self.selection = start + byte(selected.start)..start + byte(selected.end);
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (geometry, display) = self.drawn.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = geometry.position_for_index(display.to_display(range.start));
        let end = geometry.position_for_index(display.to_display(range.end));
        let line_height = geometry.line_height();
        Some(Bounds::from_corners(
            start,
            point(end.x.max(start.x + px(1.)), end.y + line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_at(point)?;
        Some(self.to_utf16(offset))
    }
}

/// The byte offset in `text` of an offset in UTF-16 code units.
fn utf16_to_byte(text: &str, units: usize) -> usize {
    let mut count = 0;
    for (ix, ch) in text.char_indices() {
        if count >= units {
            return ix;
        }
        count += ch.len_utf16();
    }
    text.len()
}
