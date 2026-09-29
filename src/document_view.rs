//! One open document: its text and where it lives, the editor and preview
//! showing it, and everything that follows its edits (statistics, scroll
//! sync, spelling, crash recovery).

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui_kit::{
    App, AppContext as _, ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter,
    Focusable as _, HighlightStyle, InteractiveElement as _, IntoElement, KeyDownEvent, ListOffset,
    MouseMoveEvent, ParentElement as _, Pixels, Render, ScrollWheelEvent, SharedString,
    Styled as _, Subscription, Task, Window,
    base::{
        TextView, TextViewState,
        input::{Diagnostic, DiagnosticSeverity},
    },
    component::{
        ActiveTheme as _, ElementExt as _, RopeExt as _,
        clipboard::Clipboard,
        input::{
            Copy, Cut, Editor, EditorState, Enter, IndentInline, InputEvent, OutdentInline, Paste,
            SelectAll, TabSize, TextDecoration, TextDecorationCollection,
        },
        native_menu::NativeMenu,
        resizable::{h_resizable, resizable_panel},
        v_flex,
    },
    div, point,
    prelude::FluentBuilder as _,
    px, rems,
};

use crate::{
    actions::{AddToDictionary, CopyRichText, ReplaceMisspelling},
    analysis::{Analysis, content_hash},
    document::{LineEnding, display_name},
    format::{self, Edit},
    images, preview_ext,
    session::{self, DiskStamp, Recovery},
    settings::Layout,
    smart_edit, spell,
    themes::AppSettings,
};

/// Widest the text column grows before extra width becomes margin.
const READABLE_WIDTH_REMS: f32 = 46.;
/// How long typing must pause before statistics and the outline refresh.
const ANALYSIS_DEBOUNCE: Duration = Duration::from_millis(120);
/// How long typing must pause before spelling is checked again.
const SPELL_DEBOUNCE: Duration = Duration::from_millis(400);
/// How long typing must pause before unsaved changes are written aside.
const RECOVERY_DEBOUNCE: Duration = Duration::from_secs(1);
/// Estimated jumps allowed before giving up on landing a target exactly.
const MAX_EDITOR_SCROLL_ATTEMPTS: u8 = 4;
/// How much of the text outside the current paragraph fades in focus mode.
const FOCUS_FADE: f32 = 0.7;

/// Which pane the other one follows while scroll sync is on.
///
/// The pane under the pointer, or the editor while typing, drives: its
/// scrolling moves the other pane, and scrolling it causes (the follower's
/// programmatic moves) never feed back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ScrollDriver {
    #[default]
    Editor,
    Preview,
}

/// A source position the editor is being scrolled to.
///
/// Lines off screen have no layout to measure, so the editor first jumps to
/// an estimate; once the line is laid out, the next pass lands on it exactly.
#[derive(Clone, Copy, Debug)]
struct EditorScrollTarget {
    /// Zero-based source line, with the fraction through it.
    line: f32,
    attempts: u8,
}

/// Something the workspace shows or acts on changed.
pub enum DocumentEvent {
    /// A message for the user, such as a failed image paste.
    Notify(SharedString),
}

pub struct DocumentView {
    editor: Entity<EditorState>,
    preview: Entity<TextViewState>,
    /// Parser configuration for the preview; built once so the preview can
    /// tell that it is unchanged between frames.
    markdown_extensions: gpui_kit::component::text::MarkdownExtensions,

    path: Option<PathBuf>,
    line_ending: LineEnding,
    /// The text last read from or written to disk.
    saved_hash: u64,
    dirty: bool,
    /// The file as last read or written, to notice other programs' changes.
    disk: Option<DiskStamp>,
    /// The current source, shared with the preview and background analysis.
    source: SharedString,
    revision: usize,
    analysis: Arc<Analysis>,
    analysis_task: Option<Task<()>>,

    recovery_id: String,
    recovery_written: bool,
    recovery_task: Option<Task<()>>,

    misspellings: Arc<Vec<spell::Misspelling>>,
    spell_task: Option<Task<()>>,
    /// Spelling was checked with this dictionary at this revision.
    spell_checked: Option<(usize, String)>,

    /// The caret as of the editor's last change, for code that runs while
    /// the editor is busy (its context menu).
    last_cursor: usize,
    focus_decorations: Option<TextDecorationCollection>,
    /// The paragraph left undimmed in focus mode.
    focus_block: Option<Range<usize>>,

    scroll_driver: ScrollDriver,
    /// Editor scroll position the preview was last aligned with.
    synced_editor_scroll: Option<(usize, Pixels)>,
    /// Preview scroll position the editor was last aligned with.
    synced_preview_scroll: Option<ListOffset>,
    editor_scroll_target: Option<EditorScrollTarget>,
    /// Row count of the preview when it was last aligned.
    synced_preview_rows: usize,
    /// Measured widths used to center the text column.
    editor_width: Pixels,
    preview_width: Pixels,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DocumentEvent> for DocumentView {}

impl DocumentView {
    /// A document showing `text`, saved at `path` when it has one.
    pub fn new(
        text: String,
        path: Option<PathBuf>,
        line_ending: LineEnding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = AppSettings::get(cx).clone();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(settings.line_numbers)
                .soft_wrap(settings.soft_wrap)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .indent_guides(false)
                .searchable(true)
                .placeholder("Start writing…")
        });
        let preview = cx.new(|cx| TextViewState::markdown("", cx));

        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_text_changed(true, cx);
                }
            }),
            // The editor notifies when it scrolls or its caret moves.
            cx.observe(&editor, |this, editor, cx| {
                this.last_cursor = editor.read(cx).cursor();
                this.sync_preview(false, cx);
                this.update_focus_dimming(cx);
            }),
            // A re-parse can rebuild the preview's rows, which resets its
            // scroll position; realign once the new rows exist.
            cx.observe(&preview, |this, preview, cx| {
                let rows = preview.read(cx).list_state().item_count();
                if rows != this.synced_preview_rows {
                    this.sync_preview(true, cx);
                }
            }),
            cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
                this.apply_settings(window, cx);
                this.sync_preview(true, cx);
                cx.notify();
            }),
        ];

        let mut this = Self {
            editor,
            markdown_extensions: preview_ext::markdown_extensions(&preview),
            preview,
            path: None,
            line_ending: LineEnding::Lf,
            saved_hash: content_hash(""),
            dirty: false,
            disk: None,
            source: SharedString::default(),
            revision: 0,
            analysis: Arc::default(),
            analysis_task: None,
            recovery_id: session::new_recovery_id(),
            recovery_written: false,
            recovery_task: None,
            misspellings: Arc::default(),
            spell_task: None,
            spell_checked: None,
            last_cursor: 0,
            focus_decorations: None,
            focus_block: None,
            scroll_driver: ScrollDriver::Editor,
            synced_editor_scroll: None,
            synced_preview_scroll: None,
            editor_scroll_target: None,
            synced_preview_rows: 0,
            editor_width: px(0.),
            preview_width: px(0.),
            _subscriptions: subscriptions,
        };
        this.load_text(text, path, line_ending, window, cx);
        this
    }

    // ---------------------------------------------------------------------
    // State

    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    pub fn preview(&self) -> &Entity<TextViewState> {
        &self.preview
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn source(&self) -> &SharedString {
        &self.source
    }

    pub fn analysis(&self) -> &Arc<Analysis> {
        &self.analysis
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn disk_stamp(&self) -> Option<DiskStamp> {
        self.disk
    }

    pub fn saved_hash(&self) -> u64 {
        self.saved_hash
    }

    pub fn recovery_id(&self) -> Option<&str> {
        self.recovery_written.then_some(self.recovery_id.as_str())
    }

    pub fn name(&self) -> String {
        display_name(self.path.as_ref())
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
    }

    /// An untitled document nobody has typed into, which opening a file may
    /// replace without asking.
    pub fn is_blank(&self) -> bool {
        self.path.is_none() && !self.dirty
    }

    pub fn cursor(&self, cx: &App) -> usize {
        self.editor.read(cx).cursor()
    }

    pub fn set_cursor(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let offset = floor_char_boundary(&editor.value(), offset);
            editor.set_selected_range(offset..offset, cx);
            let position = editor.text().offset_to_position(offset);
            editor.set_cursor_position(position, window, cx);
        });
    }

    /// Replace the document with `text` read from `path`.
    fn load_text(
        &mut self,
        text: String,
        path: Option<PathBuf>,
        line_ending: LineEnding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saved_hash = content_hash(&text);
        self.disk = path.as_deref().and_then(DiskStamp::of);
        self.path = path;
        self.line_ending = line_ending;
        self.dirty = false;
        self.synced_editor_scroll = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        // `set_value` does not emit a change event.
        self.on_text_changed(false, cx);
        self.dirty = false;
    }

    /// Restore unsaved text recovered after a crash; it stays unsaved.
    pub fn restore_unsaved(
        &mut self,
        id: String,
        recovery: Recovery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = recovery
            .path
            .as_deref()
            .and_then(|path| crate::document::load(path).ok());
        self.path = recovery.path;
        self.disk = self.path.as_deref().and_then(DiskStamp::of);
        self.line_ending = if recovery.crlf {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        };
        self.saved_hash = saved.map_or(content_hash(""), |saved| content_hash(&saved.text));
        self.recovery_id = id;
        self.recovery_written = true;
        self.editor
            .update(cx, |editor, cx| editor.set_value(recovery.text, window, cx));
        self.on_text_changed(false, cx);
    }

    /// Show `text`, which another program saved to the document's file,
    /// keeping the caret where it was.
    pub fn reload(
        &mut self,
        text: String,
        line_ending: LineEnding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self.cursor(cx);
        let path = self.path.clone();
        self.load_text(text, path, line_ending, window, cx);
        self.set_cursor(cursor, window, cx);
        self.discard_recovery();
    }

    /// Accept the file on disk as it is now without reloading it.
    pub fn set_disk_stamp(&mut self, stamp: Option<DiskStamp>) {
        self.disk = stamp;
    }

    /// The file was deleted or moved away: the text only exists here now.
    pub fn mark_missing(&mut self, cx: &mut Context<Self>) {
        self.disk = None;
        self.saved_hash = content_hash("\u{0}missing");
        self.dirty = true;
        self.schedule_recovery(cx);
        cx.notify();
    }

    fn on_text_changed(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let source = self.editor.read(cx).value();
        self.preview
            .update(cx, |preview, cx| preview.set_text(&source, cx));
        self.source = source;
        // Assume an edit until the analysis can tell the text matches disk.
        self.dirty = true;
        self.set_scroll_driver(ScrollDriver::Editor);
        self.revision += 1;
        self.focus_block = None;
        self.schedule_analysis(debounce, cx);
        cx.notify();
    }

    /// Recompute statistics, the outline and the block index off the UI
    /// thread once typing pauses. Stale results are discarded.
    fn schedule_analysis(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let revision = self.revision;
        let source = self.source.clone();
        self.analysis_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(ANALYSIS_DEBOUNCE).await;
            }
            let analysis = cx
                .background_executor()
                .spawn(async move { Analysis::of(&source) })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                this.dirty = analysis.content_hash != this.saved_hash;
                this.analysis = Arc::new(analysis);
                this.analysis_task = None;
                if this.dirty {
                    this.schedule_recovery(cx);
                } else {
                    this.discard_recovery();
                }
                this.schedule_spell_check(true, cx);
                this.sync_preview(true, cx);
                cx.notify();
            });
        }));
    }

    // ---------------------------------------------------------------------
    // Saving and recovery

    /// Write the current text to `path`, which becomes the document's.
    pub fn write_to(&mut self, path: &Path, cx: &mut Context<Self>) -> std::io::Result<()> {
        let text = self.editor.read(cx).value();
        crate::document::save(path, &text, self.line_ending)?;
        self.saved_hash = content_hash(&text);
        self.dirty = self.source.as_ref() != text.as_ref();
        self.path = Some(path.to_path_buf());
        self.disk = DiskStamp::of(path);
        if !self.dirty {
            self.discard_recovery();
        }
        cx.notify();
        Ok(())
    }

    /// Write unsaved changes aside once typing pauses, so a crash can't lose
    /// them.
    fn schedule_recovery(&mut self, cx: &mut Context<Self>) {
        let revision = self.revision;
        self.recovery_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECOVERY_DEBOUNCE).await;
            let Ok(Some((id, recovery))) = this.read_with(cx, |this, _| {
                (this.dirty && this.revision == revision).then(|| {
                    (
                        this.recovery_id.clone(),
                        Recovery {
                            path: this.path.clone(),
                            text: this.source.to_string(),
                            crlf: this.line_ending == LineEnding::CrLf,
                        },
                    )
                })
            }) else {
                return;
            };
            let written = cx
                .background_executor()
                .spawn(async move { session::write_recovery(&id, &recovery).is_ok() })
                .await;
            _ = this.update(cx, |this, _| {
                this.recovery_written |= written;
                this.recovery_task = None;
            });
        }));
    }

    /// Forget the unsaved changes written aside: they were saved or thrown
    /// away.
    pub fn discard_recovery(&mut self) {
        self.recovery_task = None;
        if self.recovery_written {
            session::remove_recovery(&self.recovery_id);
            self.recovery_written = false;
        }
    }

    // ---------------------------------------------------------------------
    // Settings

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::get(cx).clone();
        self.editor.update(cx, |editor, cx| {
            editor.set_soft_wrap(settings.soft_wrap, window, cx);
            editor.set_line_number(settings.line_numbers && !settings.focus_mode, window, cx);
            editor.set_folding(!settings.focus_mode, window, cx);
        });
        self.schedule_spell_check(false, cx);
        self.focus_block = None;
        self.update_focus_dimming(cx);
    }

    // ---------------------------------------------------------------------
    // Spelling

    fn spell_language(cx: &App) -> Option<String> {
        let settings = AppSettings::get(cx);
        settings.spell_check.then(|| {
            settings
                .spell_language
                .clone()
                .unwrap_or_else(spell::default_language)
        })
    }

    /// Check spelling in the background, after a pause when `debounce`.
    fn schedule_spell_check(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let Some(language) = Self::spell_language(cx) else {
            if self.spell_checked.take().is_some() || !self.misspellings.is_empty() {
                self.spell_task = None;
                self.misspellings = Arc::default();
                self.show_misspellings(cx);
            }
            return;
        };
        if self.spell_checked.as_ref() == Some(&(self.revision, language.clone())) {
            return;
        }
        let revision = self.revision;
        let source = self.source.clone();
        self.spell_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(SPELL_DEBOUNCE).await;
            }
            let checked_language = language.clone();
            let found = cx
                .background_executor()
                .spawn(async move {
                    spell::dictionary(&language)
                        .map(|dictionary| spell::check(&source, &dictionary))
                        .unwrap_or_default()
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                this.spell_task = None;
                this.spell_checked = Some((revision, checked_language));
                this.misspellings = Arc::new(found);
                this.show_misspellings(cx);
            });
        }));
    }

    /// Check spelling again, now: the dictionary or the personal word list
    /// changed.
    pub fn recheck_spelling(&mut self, cx: &mut Context<Self>) {
        self.spell_checked = None;
        self.schedule_spell_check(false, cx);
    }

    fn show_misspellings(&mut self, cx: &mut Context<Self>) {
        let misspellings = self.misspellings.clone();
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().clone();
            let Some(diagnostics) = editor.diagnostics_mut() else {
                return;
            };
            diagnostics.reset(&text);
            diagnostics.extend(misspellings.iter().map(|misspelling| {
                Diagnostic::new(
                    text.offset_to_position(misspelling.range.start)
                        ..text.offset_to_position(misspelling.range.end),
                    format!("Unknown word “{}”", misspelling.word),
                )
                .with_severity(DiagnosticSeverity::Info)
                .with_source("Spelling")
            }));
            cx.notify();
        });
    }

    fn misspelling_at(&self, offset: usize) -> Option<&spell::Misspelling> {
        self.misspellings.iter().find(|misspelling| {
            misspelling.range.start <= offset && offset <= misspelling.range.end
        })
    }

    /// Replace a misspelled word with a suggestion, if the text there is
    /// still that word.
    pub fn replace_misspelling(
        &mut self,
        action: &ReplaceMisspelling,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = action.start..action.end;
        if self.source.get(range.clone()) != Some(action.word.as_ref()) {
            return;
        }
        self.editor.update(cx, |editor, cx| {
            editor.set_selected_range(range, cx);
            editor.replace(action.with.to_string(), window, cx);
        });
    }

    /// The editor's context menu: spelling suggestions for the word under
    /// the pointer, then the usual edit commands.
    fn context_menu(&self, menu: NativeMenu, cx: &App) -> NativeMenu {
        let mut menu = menu;
        // The editor is mid-update when it asks for its menu; a right-click
        // has already moved the caret to the word clicked.
        let cursor = self.last_cursor;
        if let Some(misspelling) = self.misspelling_at(cursor).cloned()
            && let Some(language) = Self::spell_language(cx)
            && let Some(dictionary) = spell::dictionary(&language)
        {
            let suggestions = spell::suggest(&dictionary, &misspelling.word);
            if suggestions.is_empty() {
                menu = menu.menu_with_disabled(
                    "No suggestions",
                    true,
                    Box::new(AddToDictionary(misspelling.word.clone().into())),
                );
            }
            for suggestion in suggestions {
                menu = menu.menu(
                    suggestion.clone(),
                    Box::new(ReplaceMisspelling {
                        start: misspelling.range.start,
                        end: misspelling.range.end,
                        word: misspelling.word.clone().into(),
                        with: suggestion.into(),
                    }),
                );
            }
            menu = menu
                .menu(
                    format!("Add “{}” to dictionary", misspelling.word),
                    Box::new(AddToDictionary(misspelling.word.into())),
                )
                .separator();
        }
        menu.menu("Cut", Box::new(Cut))
            .menu("Copy", Box::new(Copy))
            .menu("Paste", Box::new(Paste))
            .separator()
            .menu("Copy as rich text", Box::new(CopyRichText))
            .separator()
            .menu("Select all", Box::new(SelectAll))
    }

    // ---------------------------------------------------------------------
    // Focus mode

    /// In focus mode, fade everything but the paragraph with the caret.
    fn update_focus_dimming(&mut self, cx: &mut Context<Self>) {
        let focus_mode = AppSettings::get(cx).focus_mode;
        if !focus_mode {
            if let Some(decorations) = self.focus_decorations.take() {
                decorations.clear(cx);
            }
            self.focus_block = None;
            return;
        }
        let cursor = self.cursor(cx);
        if self
            .focus_block
            .as_ref()
            .is_some_and(|block| block.start <= cursor && cursor <= block.end)
        {
            return;
        }
        let block = paragraph_at(&self.source, cursor);
        let len = self.source.len();
        let fade = HighlightStyle {
            fade_out: Some(FOCUS_FADE),
            ..HighlightStyle::default()
        };
        let decorations: Vec<TextDecoration> = [0..block.start, block.end..len]
            .into_iter()
            .filter(|range| !range.is_empty())
            .map(|range| TextDecoration::new(range, fade))
            .collect();
        self.focus_block = Some(block);
        match &self.focus_decorations {
            Some(collection) => collection.set(decorations, cx),
            None => {
                let collection = self.editor.update(cx, |editor, cx| {
                    editor.create_decorations_collection(decorations, cx)
                });
                self.focus_decorations = Some(collection);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Editing

    fn shows_editor(cx: &App) -> bool {
        let settings = AppSettings::get(cx);
        settings.focus_mode || settings.layout.shows_editor()
    }

    fn apply(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_selected_range(edit.range.clone(), cx);
            editor.replace(edit.text, window, cx);
            editor.set_selected_range(edit.selection, cx);
        });
    }

    /// Run a formatting command on the selection.
    pub fn apply_edit(
        &mut self,
        build: impl FnOnce(&str, Range<usize>) -> Edit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shows_editor(cx) {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        let edit = build(&text, self.editor.read(cx).selected_range());
        self.apply(edit, window, cx);
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
    }

    pub fn inline(&mut self, style: format::Inline, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_edit(
            |text, selection| format::toggle_inline(text, selection, style),
            window,
            cx,
        );
    }

    pub fn block(&mut self, style: format::Block, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_edit(
            |text, selection| format::toggle_block(text, selection, style),
            window,
            cx,
        );
    }

    pub fn tidy_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let selection = self.editor.read(cx).selected_range();
        match smart_edit::tidy_table(&text, selection) {
            Some(edit) => self.apply(edit, window, cx),
            None => cx.emit(DocumentEvent::Notify(
                "Put the caret in a table to tidy it.".into(),
            )),
        }
    }

    /// Enter, Tab and Shift-Tab in the editor: continue lists, nest items,
    /// move between table cells. Returns whether the key was handled.
    fn smart_key(
        &mut self,
        assist: impl FnOnce(&str, Range<usize>) -> Option<Edit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.editor.focus_handle(cx).is_focused(window) {
            return false;
        }
        let editor = self.editor.read(cx);
        let text = editor.value().to_string();
        let Some(edit) = assist(&text, editor.selected_range()) else {
            return false;
        };
        self.apply(edit, window, cx);
        true
    }

    /// Paste images as files next to the document, and URLs over selected
    /// text as links. Returns whether the paste was handled.
    fn paste(&mut self, item: &ClipboardItem, window: &mut Window, cx: &mut Context<Self>) -> bool {
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let bytes = image.bytes.clone();
                    let extension = image.format.extension();
                    self.insert_images(vec![ImageSource::Bytes(bytes, extension)], window, cx);
                    return true;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    let images: Vec<_> = paths
                        .paths()
                        .iter()
                        .filter(|path| is_image_path(path))
                        .map(|path| ImageSource::File(path.clone()))
                        .collect();
                    if !images.is_empty() {
                        self.insert_images(images, window, cx);
                        return true;
                    }
                }
                ClipboardEntry::String(string) => {
                    let text = self.editor.read(cx).value().to_string();
                    let selection = self.editor.read(cx).selected_range();
                    if let Some(edit) = smart_edit::paste_link(&text, selection, string.text()) {
                        self.apply(edit, window, cx);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Insert dropped image files at the caret.
    pub fn drop_images(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let images = paths
            .iter()
            .filter(|path| is_image_path(path))
            .map(|path| ImageSource::File(path.clone()))
            .collect();
        self.insert_images(images, window, cx);
    }

    /// Store images in the document's image folder (files already beside
    /// the document are linked where they are) and link them at the caret.
    fn insert_images(
        &mut self,
        images: Vec<ImageSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.dir() else {
            cx.emit(DocumentEvent::Notify(
                "Save the document first, so images can be stored next to it.".into(),
            ));
            return;
        };
        let folder = AppSettings::get(cx).image_folder.clone();
        let stem = self
            .path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "image".to_string());
        let mut links = Vec::new();
        for image in images {
            match store_image(&image, &dir, &folder, &stem) {
                Ok((relative, alt)) => links.push((relative, alt)),
                Err(err) => {
                    cx.emit(DocumentEvent::Notify(
                        format!("Couldn’t add the image. {err}").into(),
                    ));
                }
            }
        }
        if links.is_empty() {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        let caret = self.editor.read(cx).selected_range();
        let mut markdown = String::new();
        for (ix, (relative, alt)) in links.iter().enumerate() {
            if ix > 0 {
                markdown.push('\n');
            }
            let offset = if ix == 0 { caret.start } else { 0 };
            markdown.push_str(&smart_edit::image_markdown(
                if ix == 0 { &text } else { "" },
                offset,
                relative,
                alt,
            ));
        }
        let end = caret.start + markdown.len();
        self.apply(
            Edit {
                range: caret,
                text: markdown,
                selection: end..end,
            },
            window,
            cx,
        );
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
    }

    // ---------------------------------------------------------------------
    // Navigation

    pub fn reveal_heading(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(heading) = self.analysis.outline.get(ix).cloned() else {
            return;
        };
        if Self::shows_editor(cx) {
            // The editor places the heading exactly; the preview follows it.
            self.set_scroll_driver(ScrollDriver::Editor);
            self.editor.update(cx, |editor, cx| {
                editor.set_cursor_position(
                    gpui_kit::component::input::Position::new(heading.line as u32, 0),
                    window,
                    cx,
                );
            });
            // Revealing the caret scrolls just far enough to show it, often
            // leaving the heading at the bottom edge. Once that frame is laid
            // out, scroll the heading to the top of the editor instead.
            let editor = self.editor.clone();
            window.on_next_frame(move |_, cx| {
                editor.update(cx, |editor, cx| {
                    let Some((caret, _)) = editor.cursor_layout() else {
                        return;
                    };
                    // Caret bounds are laid out before scrolling, so they
                    // are content positions measured from the input's top.
                    let offset = editor.scroll_offset();
                    let target = (editor.input_bounds().top() - caret.top()).min(px(0.));
                    editor.set_scroll_offset(point(offset.x, target), cx);
                });
            });
        }
        if let Some((block, _)) = self.analysis.blocks.block_at_line(heading.line as f32) {
            self.preview.update(cx, |preview, cx| {
                preview.list_state().scroll_to(ListOffset {
                    item_ix: block,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
        }
    }

    /// The outline entry the caret is in (the last heading above it).
    pub fn current_heading(&self, cx: &App) -> Option<usize> {
        let editor = self.editor.read(cx);
        let line = if Self::shows_editor(cx) {
            editor.cursor_position().line as usize
        } else {
            let top = self.preview.read(cx).list_state().logical_scroll_top();
            self.analysis
                .blocks
                .line_at_block(top.item_ix, 0.)
                .map_or(0, |line| line as usize)
        };
        self.analysis
            .outline
            .iter()
            .rposition(|heading| heading.line <= line)
    }

    // ---------------------------------------------------------------------
    // Scroll sync

    fn set_scroll_driver(&mut self, driver: ScrollDriver) {
        if self.scroll_driver != driver {
            self.scroll_driver = driver;
            // A pending jump belongs to the pane that stopped driving.
            self.editor_scroll_target = None;
        }
    }

    fn syncs_scrolling(&self, cx: &App) -> bool {
        let settings = AppSettings::get(cx);
        settings.scroll_sync && settings.layout == Layout::Split && !settings.focus_mode
    }

    /// Scroll the preview to the block at the top of the editor.
    ///
    /// Runs when the editor scrolls or the document's structure changes. While
    /// the preview drives, it only records where the editor is, so the
    /// editor's catching up is not mistaken for the user scrolling it.
    fn sync_preview(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.syncs_scrolling(cx) {
            return;
        }
        let editor = self.editor.read(cx);
        let Some(rows) = editor.visible_row_range() else {
            return;
        };
        let offset_y = editor.scroll_offset().y;
        let position = (rows.start, offset_y);
        if self.scroll_driver != ScrollDriver::Editor {
            self.synced_editor_scroll = Some(position);
            return;
        }
        if !force && self.synced_editor_scroll == Some(position) {
            return;
        }
        self.synced_editor_scroll = Some(position);

        let list = self.preview.read(cx).list_state().clone();
        let count = list.item_count();
        self.synced_preview_rows = count;
        if count == 0 {
            return;
        }

        if offset_y >= px(-0.5) {
            list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
        } else if self.analysis.blocks.len() == count
            && let Some((ix, fraction)) = self.analysis.blocks.block_at_line(rows.start as f32)
        {
            // Rows above the scroll top report no bounds, so land on the row
            // first, then measure it for the offset inside.
            list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            });
            let height = list
                .bounds_for_item(ix)
                .map_or(px(0.), |bounds| bounds.size.height);
            list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: height * fraction,
            });
        } else {
            // The structure is still being analyzed: approximate by position.
            let lines = self.analysis.stats.lines.max(1) as f32;
            let fraction = (rows.start as f32 / lines).clamp(0., 1.);
            let max = list.max_offset_for_scrollbar().y;
            list.set_offset_from_scrollbar(point(px(0.), -(max * fraction)));
        }
        self.synced_preview_scroll = Some(list.logical_scroll_top());
        self.preview.update(cx, |_, cx| cx.notify());
    }

    /// Scroll the editor to the source of the block at the top of the
    /// preview. Runs after every frame the preview paints, so it catches
    /// wheel, trackpad, scrollbar and keyboard scrolling alike.
    fn sync_editor(&mut self, cx: &mut Context<Self>) {
        if !self.syncs_scrolling(cx) || self.scroll_driver != ScrollDriver::Preview {
            return;
        }
        let list = self.preview.read(cx).list_state().clone();
        let top = list.logical_scroll_top();
        let moved = self.synced_preview_scroll.is_none_or(|synced| {
            synced.item_ix != top.item_ix
                || (synced.offset_in_item - top.offset_in_item).abs() > px(0.5)
        });
        if moved {
            self.synced_preview_scroll = Some(top);
            let blocks = &self.analysis.blocks;
            let line = if top.item_ix == 0 && top.offset_in_item <= px(0.5) {
                Some(0.)
            } else if blocks.len() == list.item_count() {
                // The top row is at the scroll top, so its bounds are known.
                let height = list
                    .bounds_for_item(top.item_ix)
                    .map_or(px(0.), |bounds| bounds.size.height);
                let fraction = if height > px(0.) {
                    top.offset_in_item / height
                } else {
                    0.
                };
                blocks.line_at_block(top.item_ix, fraction)
            } else {
                let max = list.max_offset_for_scrollbar().y;
                let scrolled = -list.scroll_px_offset_for_scrollbar().y;
                let fraction = if max > px(0.) { scrolled / max } else { 0. };
                Some(fraction.clamp(0., 1.) * self.analysis.stats.lines as f32)
            };
            self.editor_scroll_target = line.map(|line| EditorScrollTarget { line, attempts: 0 });
        }
        self.scroll_editor_to_target(cx);
    }

    /// Move the editor toward [`Self::editor_scroll_target`]: exactly when
    /// the target line is laid out, by estimate otherwise.
    fn scroll_editor_to_target(&mut self, cx: &mut Context<Self>) {
        let Some(mut target) = self.editor_scroll_target else {
            return;
        };
        let mut landed = false;
        self.editor.update(cx, |editor, cx| {
            let Some(rows) = editor.visible_row_range() else {
                return;
            };
            let text = editor.text();
            let last_line = text.lines_len().saturating_sub(1);
            let line = (target.line.max(0.).floor() as usize).min(last_line);
            let within_line = (target.line - line as f32).clamp(0., 1.);
            let line_range = text.line_start_offset(line)..text.line_end_offset(line);
            let offset = editor.scroll_offset();

            let desired = match (editor.range_to_bounds(&line_range), editor.text_bounds()) {
                (Some(line_bounds), Some(text_bounds)) => {
                    // Content position of the line: measured from the text
                    // origin, which moves with the scroll offset.
                    landed = true;
                    let top = line_bounds.top() - text_bounds.top();
                    -(top + line_bounds.size.height * within_line)
                }
                _ => {
                    // Off screen: estimate from the lines visible now.
                    let visible_lines = rows.len().max(1) as f32;
                    let per_line = editor.input_bounds().size.height / visible_lines;
                    offset.y - per_line * (line as f32 + within_line - rows.start as f32)
                }
            };
            let desired = desired.min(px(0.));
            if (desired - offset.y).abs() > px(0.5) {
                editor.set_scroll_offset(point(offset.x, desired), cx);
            }
        });
        target.attempts += 1;
        self.editor_scroll_target =
            (!landed && target.attempts < MAX_EDITOR_SCROLL_ATTEMPTS).then_some(target);
    }

    // ---------------------------------------------------------------------
    // Rendering

    fn render_editor(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let document = cx.weak_entity();
        let inset = column_inset(self.pane_width(self.editor_width, window, cx), cx);
        let focus_mode = AppSettings::get(cx).focus_mode;
        let (menu_document, paste_document) = (document.clone(), document.clone());
        div()
            .id("editor-pane")
            .size_full()
            .bg(cx.theme().background)
            .font_family(cx.theme().mono_font_family.clone())
            // The margins beside the text column belong to the editor too:
            // the editor handles the wheel over its text, this handles the
            // rest so the whole pane scrolls.
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, _| {
                this.set_scroll_driver(ScrollDriver::Editor)
            }))
            .capture_key_down(cx.listener(|this, _: &KeyDownEvent, _, _| {
                this.set_scroll_driver(ScrollDriver::Editor)
            }))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if !action.secondary
                    && !action.shift
                    && this.smart_key(smart_edit::enter, window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                let handled = this.smart_key(
                    |text, selection| {
                        smart_edit::table_tab(text, selection.clone(), false)
                            .or_else(|| smart_edit::indent(text, selection))
                    },
                    window,
                    cx,
                );
                if handled {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                let handled = this.smart_key(
                    |text, selection| {
                        smart_edit::table_tab(text, selection.clone(), true)
                            .or_else(|| smart_edit::outdent(text, selection))
                    },
                    window,
                    cx,
                );
                if handled {
                    cx.stop_propagation();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                this.set_scroll_driver(ScrollDriver::Editor);
                this.editor.update(cx, |editor, cx| {
                    let line_height = editor.line_height().unwrap_or(window.line_height());
                    let delta = event.delta.pixel_delta(line_height);
                    let offset = editor.scroll_offset();
                    editor.set_scroll_offset(point(offset.x, offset.y + delta.y), cx);
                });
            }))
            .child(
                Editor::new(&self.editor)
                    .h_full()
                    .text_size(rems(if focus_mode { 1. } else { 0.875 }))
                    .line_height(rems(if focus_mode { 1.75 } else { 1.5 }))
                    .border_0()
                    .rounded_none()
                    .pl(inset)
                    .pr(inset)
                    .pt(if focus_mode { rems(4.) } else { rems(1.) })
                    .aria_label("Markdown source")
                    .context_menu(move |menu, _, cx| {
                        menu_document
                            .read_with(cx, |this, cx| this.context_menu(NativeMenu::new(), cx))
                            .unwrap_or(menu)
                    })
                    .on_paste(move |item, window, cx| {
                        paste_document
                            .update(cx, |this, cx| this.paste(item, window, cx))
                            .unwrap_or(false)
                    }),
            )
            .on_prepaint(move |bounds, _, cx| {
                // Notifying mid-frame can be lost; update once the frame is done.
                cx.defer(move |cx| {
                    _ = document.update(cx, |this, cx| {
                        if (this.editor_width - bounds.size.width).abs() >= px(1.) {
                            this.editor_width = bounds.size.width;
                            cx.notify();
                        }
                    });
                });
            })
    }

    fn render_preview(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let document = cx.weak_entity();
        let base_dir = self.dir().map(Arc::<Path>::from);
        let inset = column_inset(self.pane_width(self.preview_width, window, cx), cx);
        let is_empty = self.source.trim().is_empty();

        div()
            .id("preview-pane")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            // Let the margins beside the text column scroll the preview; the
            // list scrolls itself when the pointer is over its viewport.
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, _| {
                this.set_scroll_driver(ScrollDriver::Preview)
            }))
            .capture_key_down(cx.listener(|this, _: &KeyDownEvent, _, _| {
                this.set_scroll_driver(ScrollDriver::Preview)
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                this.set_scroll_driver(ScrollDriver::Preview);
                let list = this.preview.read(cx).list_state().clone();
                if list.viewport_bounds().contains(&event.position) {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height());
                list.scroll_by(-delta.y);
                this.preview.update(cx, |_, cx| cx.notify());
            }))
            .child(
                TextView::new(&self.preview)
                    .size_full()
                    .scrollable(true)
                    .selectable(true)
                    .markdown_extensions(self.markdown_extensions.clone())
                    .pl(inset)
                    .pr(inset)
                    .py_8()
                    .image_source(move |url| images::resolve(url, base_dir.as_deref()))
                    .code_block_actions(|code_block, _, _| {
                        Clipboard::new("copy-code").value(code_block.code())
                    })
                    .on_link_click(|url, event, _, cx| {
                        if !event.is_right_click() {
                            cx.open_url(url);
                        }
                    }),
            )
            .when(is_empty, |this| {
                this.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            div()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child("Nothing to preview"),
                        )
                        .child("Markdown you write appears here."),
                )
            })
            .on_prepaint(move |bounds, _, cx| {
                // Notifying mid-frame can be lost; update once the frame is done.
                cx.defer(move |cx| {
                    _ = document.update(cx, |this, cx| {
                        if (this.preview_width - bounds.size.width).abs() >= px(1.) {
                            this.preview_width = bounds.size.width;
                            cx.notify();
                        }
                        this.sync_editor(cx);
                    });
                });
            })
    }

    /// A pane fills the view unless the editor and preview share it, in
    /// which case its last measured width is used.
    fn pane_width(&self, measured: Pixels, window: &Window, cx: &App) -> Pixels {
        let settings = AppSettings::get(cx);
        match settings.layout {
            Layout::Split if !settings.focus_mode => measured,
            _ if measured > px(0.) => measured,
            _ => window.viewport_size().width,
        }
    }
}

impl Render for DocumentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = AppSettings::get(cx);
        let layout = if settings.focus_mode {
            Layout::Editor
        } else {
            settings.layout
        };
        match layout {
            Layout::Editor => self.render_editor(window, cx).into_any_element(),
            Layout::Preview => self.render_preview(window, cx).into_any_element(),
            Layout::Split => h_resizable("document-split")
                .child(
                    resizable_panel()
                        .size_range(rems(16.).to_pixels(window.rem_size())..Pixels::MAX)
                        .child(self.render_editor(window, cx)),
                )
                .child(
                    resizable_panel()
                        .size_range(rems(16.).to_pixels(window.rem_size())..Pixels::MAX)
                        .child(self.render_preview(window, cx)),
                )
                .into_any_element(),
        }
    }
}

/// An image to add to the document.
enum ImageSource {
    /// Pasted image data, with its file extension.
    Bytes(Vec<u8>, &'static str),
    File(PathBuf),
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp"
            )
        })
}

/// Put `image` where the document can link it, returning the link path
/// (relative to `dir`) and alt text. Files already inside `dir` stay put.
fn store_image(
    image: &ImageSource,
    dir: &Path,
    folder: &str,
    stem: &str,
) -> std::io::Result<(String, String)> {
    if let ImageSource::File(path) = image
        && let Ok(relative) = path.strip_prefix(dir)
    {
        let alt = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default();
        return Ok((relative.to_string_lossy().to_string(), alt));
    }
    let target_dir = dir.join(folder);
    std::fs::create_dir_all(&target_dir)?;
    let (name, extension) = match image {
        ImageSource::Bytes(_, extension) => {
            let now = chrono_like_timestamp();
            (format!("{stem}-{now}"), extension.to_string())
        }
        ImageSource::File(path) => (
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_else(|| "image".to_string()),
            path.extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_else(|| "png".to_string()),
        ),
    };
    let name: String = name
        .chars()
        .map(|ch| {
            if ch.is_whitespace() || "/\\:*?\"<>|".contains(ch) {
                '-'
            } else {
                ch
            }
        })
        .collect();
    // Never overwrite: number the name until it is free.
    let mut target = target_dir.join(format!("{name}.{extension}"));
    let mut counter = 2;
    while target.exists() {
        target = target_dir.join(format!("{name}-{counter}.{extension}"));
        counter += 1;
    }
    match image {
        ImageSource::Bytes(bytes, _) => std::fs::write(&target, bytes)?,
        ImageSource::File(path) => {
            std::fs::copy(path, &target)?;
        }
    }
    let relative = target
        .strip_prefix(dir)
        .map(|relative| relative.to_string_lossy().to_string())
        .unwrap_or_else(|_| target.to_string_lossy().to_string());
    // Pasted images have no name worth describing them by.
    let alt = match image {
        ImageSource::Bytes(..) => "image".to_string(),
        ImageSource::File(_) => name,
    };
    Ok((relative, alt))
}

/// `YYYYMMDD-HHMMSS` in UTC, for naming pasted images.
fn chrono_like_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The paragraph (run of non-blank lines) around `offset`.
fn paragraph_at(text: &str, offset: usize) -> Range<usize> {
    let offset = floor_char_boundary(text, offset);
    let mut start = text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    while start > 0 {
        let previous = text[..start - 1].rfind('\n').map_or(0, |ix| ix + 1);
        if text[previous..start - 1].trim().is_empty() {
            break;
        }
        start = previous;
    }
    let mut end = text[offset..]
        .find('\n')
        .map_or(text.len(), |ix| offset + ix);
    while end < text.len() {
        let next_end = text[end + 1..]
            .find('\n')
            .map_or(text.len(), |ix| end + 1 + ix);
        if text[end + 1..next_end].trim().is_empty() {
            break;
        }
        end = next_end;
    }
    start..end
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Horizontal inset that keeps text within a readable column, centered in a
/// pane of `width`, but never tighter than the standard pane padding.
fn column_inset(width: Pixels, cx: &App) -> Pixels {
    let rem = cx.theme().font_size;
    let min = rem * 1.5;
    let column = rem * READABLE_WIDTH_REMS;
    ((width - column) / 2.).max(min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_paragraph_around_the_caret() {
        let text = "one\n\ntwo\nlines\n\nthree";
        assert_eq!(&text[paragraph_at(text, 6)], "two\nlines");
        assert_eq!(&text[paragraph_at(text, 0)], "one");
        assert_eq!(&text[paragraph_at(text, text.len())], "three");
    }

    #[test]
    fn stores_images_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("malgel-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = ImageSource::Bytes(vec![1, 2, 3], "png");
        let (first, _) = store_image(&bytes, &dir, "assets", "My notes").unwrap();
        assert!(
            first.starts_with("assets/My-notes-") && first.ends_with(".png"),
            "{first}"
        );
        let outside = std::env::temp_dir().join(format!("malgel-pic-{}.png", std::process::id()));
        std::fs::write(&outside, [0]).unwrap();
        let (copied, alt) =
            store_image(&ImageSource::File(outside.clone()), &dir, "assets", "x").unwrap();
        assert!(copied.starts_with("assets/malgel-pic-"));
        assert!(alt.starts_with("malgel-pic-"));
        let (again, _) =
            store_image(&ImageSource::File(outside.clone()), &dir, "assets", "x").unwrap();
        assert_ne!(copied, again);
        // A file already next to the document is linked in place.
        let (linked, _) =
            store_image(&ImageSource::File(dir.join(&copied)), &dir, "assets", "x").unwrap();
        assert_eq!(linked, copied);
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn formats_timestamps() {
        let stamp = chrono_like_timestamp();
        assert_eq!(stamp.len(), 15);
        assert!(stamp.starts_with("20"));
    }
}
