//! Live Preview's editor: a piece of Markdown drawn as the preview draws
//! it, edited where it stands. Its marks (`**`, `[…](…)`, `` ` ``) show only
//! around the caret; see [`crate::live_inline`].
//!
//! The text is drawn with GPUI's own styled text, inheriting the preview's
//! font, size and line height; the caret and selection are painted under
//! it from the same layout, which also maps clicks to characters.

use std::{ops::Range, time::Duration};

use gpui_kit::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, FontStyle, FontWeight, HighlightStyle, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Render, SharedString, StrikethroughStyle, Styled as _, StyledText, Task,
    TextLayout, UTF16Selection, UnderlineStyle, Window,
    base::actions::{SelectDown, SelectLeft, SelectRight, SelectUp},
    canvas,
    component::{
        ActiveTheme as _,
        input::{
            Backspace, Copy, Cut, Delete, DeleteToBeginningOfLine, DeleteToEndOfLine,
            DeleteToNextWordEnd, DeleteToPreviousWordStart, Enter, MoveDown, MoveEnd, MoveHome,
            MoveLeft, MoveRight, MoveToEnd, MoveToEndOfLine, MoveToNextWord, MoveToPreviousWord,
            MoveToStart, MoveToStartOfLine, MoveUp, Paste, SelectAll, SelectToEnd,
            SelectToEndOfLine, SelectToNextWordEnd, SelectToPreviousWordStart, SelectToStart,
            SelectToStartOfLine,
        },
    },
    div, fill, point, px, size,
};

use crate::{
    actions::FollowLink,
    live_inline::{self, Analysis, Display, Mode, Style},
};

const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Shown in place of empty text, so it still has a line to stand on.
const EMPTY: &str = "\u{200B}";

pub enum LiveEditorEvent {
    /// The text changed.
    Changed,
    /// Edit the document at this offset: an unedited table cell or list
    /// item was clicked.
    Activate(usize),
}

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
    /// The last frame's layout of the text drawn, and how it maps.
    drawn: Option<(TextLayout, Display)>,
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
            drawn: None,
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

    /// Show `text` drawn as `mode`, without a change event.
    pub fn set_value(
        &mut self,
        text: String,
        mode: Mode,
        line_prefix: String,
        cx: &mut Context<Self>,
    ) {
        self.text = text;
        self.mode = mode;
        self.line_prefix = line_prefix;
        self.analysis = live_inline::analyze(&self.text, mode);
        self.selection = 0..0;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        self.placement = None;
        self.drawn = None;
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
        let (layout, display) = self.drawn.as_ref()?;
        layout.position_for_index(display.to_display(self.cursor()).min(layout.len()))
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
        let (layout, display) = self.drawn.as_ref()?;
        let index = match layout.index_for_position(position) {
            Ok(index) | Err(index) => index,
        };
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
        let Some(offset) = self.offset_at(event.position) else {
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
    fn drawn(&mut self, layout: TextLayout, display: Display, cx: &mut Context<Self>) {
        self.drawn = Some((layout, display));
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

/// Highlights over ranges of text, and the ranges drawn in another font.
type Highlights = (
    Vec<(Range<usize>, HighlightStyle)>,
    Vec<(Range<usize>, SharedString)>,
);

/// The highlights and font changes for text drawn with `styles`.
pub fn highlights(styles: &[(Range<usize>, Style)], cx: &App) -> Highlights {
    let theme = cx.theme();
    let mut highlights = Vec::new();
    let mut fonts = Vec::new();
    for (range, style) in styles {
        let mut highlight = HighlightStyle::default();
        if style.bold {
            highlight.font_weight = Some(FontWeight::BOLD);
        }
        if style.italic {
            highlight.font_style = Some(FontStyle::Italic);
        }
        if style.strike {
            highlight.strikethrough = Some(StrikethroughStyle {
                thickness: px(1.),
                ..Default::default()
            });
        }
        if style.underline || style.link {
            highlight.underline = Some(UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            });
        }
        if style.link {
            highlight.color = Some(theme.link);
        }
        if style.code {
            highlight.background_color = Some(theme.accent);
            fonts.push((range.clone(), theme.mono_font_family.clone()));
        }
        if style.mark {
            highlight.background_color = Some(theme.warning.opacity(0.3));
        }
        if style.syntax {
            highlight.color = Some(theme.muted_foreground);
        }
        highlights.push((range.clone(), highlight));
    }
    (highlights, fonts)
}

/// Markdown drawn formatted, without editing (a table cell not being
/// edited), and how its offsets map.
pub fn styled_text(source: &str, mode: Mode, cx: &App) -> (StyledText, Display) {
    let analysis = live_inline::analyze(source, mode);
    let display = Display::new(source, &analysis, None);
    let (highlights, fonts) = highlights(&display.styles, cx);
    let shown = if display.text.is_empty() {
        SharedString::from(EMPTY)
    } else {
        SharedString::from(display.text.clone())
    };
    let text = StyledText::new(shown)
        .with_highlights(highlights)
        .with_font_family_overrides(fonts);
    (text, display)
}

impl Render for LiveEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let display = Display::new(&self.text, &self.analysis, Some(&self.selection));
        let (highlights, fonts) = highlights(&display.styles, cx);
        let shown = if display.text.is_empty() {
            SharedString::from(EMPTY)
        } else {
            SharedString::from(display.text.clone())
        };
        let text = StyledText::new(shown)
            .with_highlights(highlights)
            .with_font_family_overrides(fonts);
        let layout = text.layout().clone();
        let entity = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let selection =
            display.to_display(self.selection.start)..display.to_display(self.selection.end);
        let caret = display.to_display(self.cursor());
        let show_caret = focused && self.caret_on;
        let theme = cx.theme();
        let (selection_color, caret_color) = (theme.selection, theme.caret);

        div()
            .key_context("Input")
            .track_focus(&self.focus_handle)
            .w_full()
            .relative()
            .cursor_text()
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
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        let line_height = layout.line_height();
                        let text_bounds = layout.bounds();
                        let position = |index: usize| layout.position_for_index(index);
                        if !selection.is_empty()
                            && let (Some(start), Some(end)) =
                                (position(selection.start), position(selection.end))
                        {
                            let rows = ((end.y - start.y) / line_height).round() as i32;
                            for row in 0..=rows {
                                let top = start.y + line_height * row as f32;
                                let left = if row == 0 {
                                    start.x
                                } else {
                                    text_bounds.left()
                                };
                                let right = if row == rows {
                                    end.x
                                } else {
                                    text_bounds.right()
                                };
                                if right > left {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(left, top),
                                            size(right - left, line_height),
                                        ),
                                        selection_color,
                                    ));
                                }
                            }
                        }
                        if show_caret && let Some(at) = position(caret) {
                            window.paint_quad(fill(
                                Bounds::new(at, size(px(2.), line_height)),
                                caret_color,
                            ));
                        }
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                        let drawn = layout.clone();
                        entity.update(cx, |this, cx| this.drawn(drawn, display, cx));
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(text)
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
        let (layout, display) = self.drawn.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_for_index(display.to_display(range.start))?;
        let end = layout
            .position_for_index(display.to_display(range.end))
            .unwrap_or(start);
        let line_height = layout.line_height();
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
