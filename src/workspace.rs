//! The document window: title bar, editor, preview and status bar.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::{
    App, AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ListOffset, MouseMoveEvent,
    ParentElement as _, PathPromptOptions, Pixels, Render, ScrollWheelEvent, SharedString,
    Styled as _, Subscription, Task, Window,
    base::{TextView, TextViewState},
    component::{
        ActiveTheme as _, ElementExt as _, Icon, IndexPath, RopeExt as _, Selectable as _,
        Sizable as _, TitleBar, WindowExt as _,
        button::{Button, ButtonGroup, ButtonVariants as _},
        clipboard::Clipboard,
        command::{Command, CommandItem, CommandState},
        dialog::DialogFooter,
        h_flex,
        input::{Editor, EditorState, InputEvent, TabSize},
        menu::AppMenuBar,
        notification::Notification,
        resizable::{h_resizable, resizable_panel},
        status_bar::StatusBar,
        text::MarkdownExtensions,
        v_flex,
    },
    div, point,
    prelude::FluentBuilder as _,
    px, rems,
};

use crate::{
    actions::*,
    analysis::{Analysis, content_hash},
    document::{self, LineEnding, display_name, is_markdown_path},
    export, format, images, preview_ext,
    settings::{Appearance, Layout, clamp_font_size},
    themes::{self, AppSettings},
};

/// Shown when Malgel starts without a file.
const WELCOME: &str = include_str!("welcome.md");

/// Widest the text column grows before extra width becomes margin.
const READABLE_WIDTH_REMS: f32 = 46.;
/// How long typing must pause before statistics and the outline refresh.
const ANALYSIS_DEBOUNCE: Duration = Duration::from_millis(120);

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

/// Estimated jumps allowed before giving up on landing a target exactly.
const MAX_EDITOR_SCROLL_ATTEMPTS: u8 = 4;

/// What to do once unsaved changes have been saved or discarded.
#[derive(Clone)]
enum AfterConfirm {
    NewFile,
    OpenDialog,
    OpenPath(PathBuf),
    CloseWindow,
    Quit,
}

pub struct Workspace {
    focus_handle: FocusHandle,
    editor: Entity<EditorState>,
    preview: Entity<TextViewState>,
    /// Parser configuration for the preview; built once so the preview can
    /// tell that it is unchanged between frames.
    markdown_extensions: MarkdownExtensions,
    headings: Entity<CommandState>,
    app_menu_bar: Entity<AppMenuBar>,

    path: Option<PathBuf>,
    line_ending: LineEnding,
    /// The text last read from or written to disk.
    saved_hash: u64,
    dirty: bool,
    /// The current source, shared with the preview and background analysis.
    source: SharedString,
    revision: usize,
    analysis: Arc<Analysis>,
    analysis_task: Option<Task<()>>,

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
    window_title: String,
    closing: bool,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        path: Option<PathBuf>,
        app_menu_bar: Entity<AppMenuBar>,
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
        let headings = cx.new(|cx| CommandState::new(window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_text_changed(true, cx);
                }
            }),
            // The editor notifies when it scrolls; keep the preview aligned.
            cx.observe(&editor, |this, _, cx| this.sync_preview(false, cx)),
            // A re-parse can rebuild the preview's rows, which resets its
            // scroll position; realign once the new rows exist.
            cx.observe(&preview, |this, preview, cx| {
                let rows = preview.read(cx).list_state().item_count();
                if rows != this.synced_preview_rows {
                    this.sync_preview(true, cx);
                }
            }),
            cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
                this.apply_editor_settings(window, cx);
                this.sync_preview(true, cx);
                cx.notify();
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                if AppSettings::get(cx).appearance == Appearance::System {
                    themes::apply(Some(window), cx);
                }
            }),
        ];

        // Ask before a window with unsaved changes closes.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.request_close(window, cx))
                .unwrap_or(true)
        });

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            editor,
            markdown_extensions: preview_ext::markdown_extensions(&preview),
            preview,
            headings,
            app_menu_bar,
            path: None,
            line_ending: LineEnding::Lf,
            saved_hash: content_hash(""),
            dirty: false,
            source: SharedString::default(),
            revision: 0,
            analysis: Arc::default(),
            analysis_task: None,
            scroll_driver: ScrollDriver::Editor,
            synced_editor_scroll: None,
            synced_preview_scroll: None,
            editor_scroll_target: None,
            synced_preview_rows: 0,
            editor_width: px(0.),
            preview_width: px(0.),
            window_title: String::new(),
            closing: false,
            _subscriptions: subscriptions,
        };

        match path {
            Some(path) => this.open_path(path, window, cx),
            None => this.load_text(WELCOME.to_string(), None, LineEnding::Lf, window, cx),
        }

        let initial_focus = if settings.layout.shows_editor() {
            this.editor.focus_handle(cx)
        } else {
            this.preview.read(cx).focus_handle().clone()
        };
        window.defer(cx, move |window, cx| initial_focus.focus(window, cx));
        this
    }

    // ---------------------------------------------------------------------
    // Document state

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
        self.path = path;
        self.line_ending = line_ending;
        self.dirty = false;
        self.synced_editor_scroll = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        if let Some(path) = &self.path {
            AppSettings::update(cx, |settings| settings.push_recent(path));
        }
        // `set_value` does not emit a change event.
        self.on_text_changed(false, cx);
        self.dirty = false;
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
                this.sync_preview(true, cx);
                cx.notify();
            });
        }));
    }

    fn document_name(&self) -> String {
        display_name(self.path.as_ref())
    }

    fn document_dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
    }

    fn update_window_title(&mut self, window: &mut Window) {
        let title = format!(
            "{}{} — Malgel",
            self.document_name(),
            if self.dirty { " (edited)" } else { "" }
        );
        if title != self.window_title {
            window.set_window_title(&title);
            window.set_window_edited(self.dirty);
            self.window_title = title;
        }
    }

    fn notify_error(&self, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        window.push_notification(Notification::error(message), cx);
    }

    // ---------------------------------------------------------------------
    // File commands

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_unsaved(AfterConfirm::NewFile, window, cx);
    }

    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_unsaved(AfterConfirm::OpenDialog, window, cx);
    }

    fn open_recent(&mut self, action: &OpenRecent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = AppSettings::get(cx).recent_files.get(action.0).cloned() else {
            return;
        };
        self.confirm_unsaved(AfterConfirm::OpenPath(path), window, cx);
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_unsaved(AfterConfirm::CloseWindow, window, cx);
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_unsaved(AfterConfirm::Quit, window, cx);
    }

    /// Called by the platform before the window closes.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing || !self.dirty {
            return true;
        }
        self.confirm_unsaved(AfterConfirm::CloseWindow, window, cx);
        false
    }

    /// Run `next` now, or once the user has saved or discarded their changes.
    fn confirm_unsaved(&mut self, next: AfterConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            self.run_after_confirm(next, window, cx);
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }

        let name = self.document_name();
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (save, discard) = (workspace.clone(), workspace.clone());
            let (save_next, discard_next) = (next.clone(), next.clone());
            alert
                .title(format!("Save changes to “{name}”?"))
                .description("Your changes will be lost if you don’t save them.")
                .footer(
                    DialogFooter::new()
                        .justify_end()
                        .child(
                            Button::new("discard")
                                .label("Don’t save")
                                .outline()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let next = discard_next.clone();
                                    _ = discard.update(cx, |this, cx| {
                                        this.run_after_confirm(next, window, cx)
                                    });
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .outline()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("save").label("Save").primary().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let next = save_next.clone();
                                _ = save
                                    .update(cx, |this, cx| this.save_then(Some(next), window, cx));
                            },
                        )),
                )
        });
    }

    fn run_after_confirm(
        &mut self,
        next: AfterConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match next {
            AfterConfirm::NewFile => {
                self.load_text(String::new(), None, LineEnding::Lf, window, cx);
                self.editor.focus_handle(cx).focus(window, cx);
            }
            AfterConfirm::OpenDialog => self.prompt_open(window, cx),
            AfterConfirm::OpenPath(path) => self.open_path(path, window, cx),
            AfterConfirm::CloseWindow => {
                self.closing = true;
                window.remove_window();
            }
            AfterConfirm::Quit => cx.quit(),
        }
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| this.open_path(path, window, cx));
        })
        .detach();
    }

    /// Read `path` in the background and show it.
    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let read_path = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { document::load(&read_path) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(loaded) => {
                    this.load_text(loaded.text, Some(path), loaded.line_ending, window, cx);
                }
                Err(err) => {
                    AppSettings::update(cx, |settings| {
                        settings.recent_files.retain(|recent| recent != &path)
                    });
                    this.notify_error(
                        format!("Couldn’t open “{}”. {err}", display_name(Some(&path))),
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        self.save_then(None, window, cx);
    }

    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_save_path(None, window, cx);
    }

    /// Save, asking for a location first when the document has none, then
    /// continue with `next`.
    fn save_then(
        &mut self,
        next: Option<AfterConfirm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.path.clone() {
            Some(path) => {
                if self.write_to(&path, window, cx)
                    && let Some(next) = next
                {
                    self.run_after_confirm(next, window, cx);
                }
            }
            None => self.prompt_save_path(next, window, cx),
        }
    }

    fn prompt_save_path(
        &mut self,
        next: Option<AfterConfirm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = self.default_dir();
        let name = match &self.path {
            Some(_) => self.document_name(),
            None => suggested_file_name(&self.analysis, "md"),
        };
        let chosen = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                if this.write_to(&path, window, cx) {
                    this.path = Some(path.clone());
                    AppSettings::update(cx, |settings| settings.push_recent(&path));
                    cx.notify();
                    if let Some(next) = next {
                        this.run_after_confirm(next, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Write the current text to `path`; reports failures to the user.
    fn write_to(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.editor.read(cx).value();
        match document::save(path, &text, self.line_ending) {
            Ok(()) => {
                self.saved_hash = content_hash(&text);
                self.dirty = self.source.as_ref() != text.as_ref();
                cx.notify();
                true
            }
            Err(err) => {
                self.notify_error(
                    format!(
                        "Couldn’t save “{}”. {err}",
                        display_name(Some(&path.to_path_buf()))
                    ),
                    window,
                    cx,
                );
                false
            }
        }
    }

    fn default_dir(&self) -> PathBuf {
        self.document_dir()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn export_html(&mut self, _: &ExportHtml, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.default_dir();
        let title = self
            .analysis
            .outline
            .first()
            .map(|heading| heading.text.clone())
            .unwrap_or_else(|| self.document_name());
        let name = match &self.path {
            Some(path) => format!(
                "{}.html",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ),
            None => suggested_file_name(&self.analysis, "html"),
        };
        let source = self.source.clone();
        let chosen = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let target = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let html = export::to_html(&source, &title);
                    std::fs::write(&target, html)
                })
                .await;
            _ = cx.update(|window, cx| {
                let name = display_name(Some(&path));
                let note = match result {
                    Ok(()) => Notification::success(format!("Exported “{name}”.")),
                    Err(err) => Notification::error(format!("Couldn’t export “{name}”. {err}")),
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    fn reveal_in_folder(
        &mut self,
        _: &RevealInFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.path {
            Some(path) => cx.reveal_path(path),
            None => window.push_notification(
                Notification::info("Save the document to give it a location."),
                cx,
            ),
        }
    }

    fn on_drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = paths
            .paths()
            .iter()
            .find(|path| is_markdown_path(path))
            .cloned()
        else {
            self.notify_error("Drop a Markdown or text file to open it.", window, cx);
            return;
        };
        self.confirm_unsaved(AfterConfirm::OpenPath(path), window, cx);
    }

    // ---------------------------------------------------------------------
    // View commands

    fn set_layout(&mut self, action: &SetLayout, window: &mut Window, cx: &mut Context<Self>) {
        let layout = action.0;
        AppSettings::update(cx, |settings| settings.layout = layout);
        if layout.shows_editor() {
            self.editor.focus_handle(cx).focus(window, cx);
        } else {
            self.preview
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
        }
    }

    fn apply_editor_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::get(cx).clone();
        self.editor.update(cx, |editor, cx| {
            editor.set_soft_wrap(settings.soft_wrap, window, cx);
            editor.set_line_number(settings.line_numbers, window, cx);
        });
    }

    fn toggle_scroll_sync(&mut self, _: &ToggleScrollSync, _: &mut Window, cx: &mut Context<Self>) {
        AppSettings::update(cx, |settings| settings.scroll_sync = !settings.scroll_sync);
    }

    fn toggle_soft_wrap(&mut self, _: &ToggleSoftWrap, _: &mut Window, cx: &mut Context<Self>) {
        AppSettings::update(cx, |settings| settings.soft_wrap = !settings.soft_wrap);
    }

    fn toggle_line_numbers(
        &mut self,
        _: &ToggleLineNumbers,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        AppSettings::update(cx, |settings| {
            settings.line_numbers = !settings.line_numbers
        });
    }

    fn go_to_heading(&mut self, _: &GoToHeading, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        self.headings
            .update(cx, |state, cx| state.set_query("", window, cx));
        let headings = self.headings.clone();
        let workspace = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            let focus = headings.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| focus.focus(window, cx));
            let outline = workspace
                .read_with(cx, |this, _| this.analysis.outline.clone())
                .unwrap_or_default();
            let items = outline.iter().map(|heading| {
                CommandItem::new()
                    .label(heading.text.clone())
                    .keywords([heading.text.clone()])
                    .icon(heading_icon(heading.level))
            });
            let confirm = workspace.clone();
            dialog.close_button(false).p_0().child(
                Command::new(&headings)
                    .bordered(false)
                    .placeholder("Go to heading")
                    .max_h(rems(24.))
                    .items(items)
                    .empty(|_, _, _| "No headings in this document")
                    .on_confirm(move |index: IndexPath, window, cx| {
                        window.close_dialog(cx);
                        _ = confirm
                            .update(cx, |this, cx| this.reveal_heading(index.row, window, cx));
                    })
                    .on_cancel(|window, cx| window.close_dialog(cx)),
            )
        });
    }

    fn reveal_heading(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(heading) = self.analysis.outline.get(ix).cloned() else {
            return;
        };
        let layout = AppSettings::get(cx).layout;
        if layout.shows_editor() {
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

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        AppSettings::update(cx, |settings| {
            settings.font_size = if delta == 0. {
                crate::settings::DEFAULT_FONT_SIZE
            } else {
                clamp_font_size(settings.font_size + delta)
            };
        });
    }

    fn toggle_appearance(&mut self, _: &ToggleAppearance, _: &mut Window, cx: &mut Context<Self>) {
        let dark = cx.theme().is_dark();
        AppSettings::update(cx, |settings| {
            settings.appearance = if dark {
                Appearance::Light
            } else {
                Appearance::Dark
            };
        });
    }

    // ---------------------------------------------------------------------
    // Formatting

    fn apply_edit(
        &mut self,
        build: impl FnOnce(&str, std::ops::Range<usize>) -> format::Edit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !AppSettings::get(cx).layout.shows_editor() {
            return;
        }
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let edit = build(&text, editor.selected_range());
            editor.set_selected_range(edit.range.clone(), cx);
            editor.replace(edit.text, window, cx);
            editor.set_selected_range(edit.selection, cx);
            editor.focus(window, cx);
        });
    }

    fn inline(&mut self, style: format::Inline, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_edit(
            |text, selection| format::toggle_inline(text, selection, style),
            window,
            cx,
        );
    }

    fn block(&mut self, style: format::Block, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_edit(
            |text, selection| format::toggle_block(text, selection, style),
            window,
            cx,
        );
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
        settings.scroll_sync && settings.layout == Layout::Split
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

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = AppSettings::get(cx);
        let layout = settings.layout;
        let is_dark = cx.theme().is_dark();
        let show_menu_bar = !cfg!(target_os = "macos");
        let workspace = cx.weak_entity();

        let layout_button = |id: &'static str, icon: IconName, tooltip: &'static str, value| {
            Button::new(id)
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .selected(layout == value)
        };

        TitleBar::new()
            .on_close_window(move |_, window, cx| {
                _ = workspace.update(cx, |this, cx| {
                    this.confirm_unsaved(AfterConfirm::CloseWindow, window, cx)
                });
            })
            .child(
                h_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .when(show_menu_bar, |this| this.child(self.app_menu_bar.clone())),
            )
            .child(
                // The document name, centered on the window like native title bars.
                h_flex()
                    .absolute()
                    .inset_0()
                    .justify_center()
                    .gap_1p5()
                    .text_sm()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(self.document_name()),
                    )
                    .when(self.dirty, |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("— Edited"),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .pr_2()
                    .child(
                        ButtonGroup::new("layout")
                            .ghost()
                            .small()
                            .compact()
                            .child(layout_button(
                                "layout-editor",
                                IconName::PenLine,
                                "Editor",
                                Layout::Editor,
                            ))
                            .child(layout_button(
                                "layout-split",
                                IconName::Columns2,
                                "Editor and preview",
                                Layout::Split,
                            ))
                            .child(layout_button(
                                "layout-preview",
                                IconName::Eye,
                                "Preview",
                                Layout::Preview,
                            ))
                            .on_click(cx.listener(|_, clicked: &Vec<usize>, window, cx| {
                                let layout = match clicked.first() {
                                    Some(0) => Layout::Editor,
                                    Some(2) => Layout::Preview,
                                    _ => Layout::Split,
                                };
                                window.dispatch_action(Box::new(SetLayout(layout)), cx);
                            })),
                    )
                    .child(
                        Button::new("appearance")
                            .ghost()
                            .small()
                            .compact()
                            .icon(Icon::new(if is_dark {
                                IconName::Sun
                            } else {
                                IconName::Moon
                            }))
                            .tooltip(if is_dark {
                                "Use light appearance"
                            } else {
                                "Use dark appearance"
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleAppearance), cx)
                            }),
                    ),
            )
    }

    fn render_editor(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = cx.weak_entity();
        let inset = column_inset(self.pane_width(self.editor_width, window, cx), cx);
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
                    .text_size(rems(0.875))
                    .line_height(rems(1.5))
                    .border_0()
                    .rounded_none()
                    .pl(inset)
                    .pr(inset)
                    .pt_4()
                    .aria_label("Markdown source"),
            )
            .on_prepaint(move |bounds, _, cx| {
                // Notifying mid-frame can be lost; update once the frame is done.
                cx.defer(move |cx| {
                    _ = workspace.update(cx, |this, cx| {
                        if (this.editor_width - bounds.size.width).abs() >= px(1.) {
                            this.editor_width = bounds.size.width;
                            cx.notify();
                        }
                    });
                });
            })
    }

    fn render_preview(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = cx.weak_entity();
        let base_dir = self.document_dir().map(Arc::<Path>::from);
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
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().foreground)
                                .child("Nothing to preview"),
                        )
                        .child("Markdown you write appears here."),
                )
            })
            .on_prepaint(move |bounds, _, cx| {
                // Notifying mid-frame can be lost; update once the frame is done.
                cx.defer(move |cx| {
                    _ = workspace.update(cx, |this, cx| {
                        if (this.preview_width - bounds.size.width).abs() >= px(1.) {
                            this.preview_width = bounds.size.width;
                            cx.notify();
                        }
                        this.sync_editor(cx);
                    });
                });
            })
    }

    /// A pane fills the window unless the editor and preview share it, in
    /// which case its last measured width is used.
    fn pane_width(&self, measured: Pixels, window: &Window, cx: &App) -> Pixels {
        match AppSettings::get(cx).layout {
            Layout::Split => measured,
            Layout::Editor | Layout::Preview => window.viewport_size().width,
        }
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let stats = self.analysis.stats;
        let position = self.editor.read(cx).cursor_position();
        let selection = self.editor.read(cx).selected_range();
        let muted = cx.theme().muted_foreground;
        let item = |text: String| div().text_color(muted).child(text);

        StatusBar::new()
            .left(
                h_flex()
                    .gap_4()
                    .child(item(format!(
                        "{} {}",
                        group_thousands(stats.words),
                        if stats.words == 1 { "word" } else { "words" }
                    )))
                    .child(item(format!(
                        "{} {}",
                        group_thousands(stats.characters),
                        if stats.characters == 1 {
                            "character"
                        } else {
                            "characters"
                        }
                    )))
                    .when(stats.words > 0, |this| {
                        this.child(item(format!("{} min read", stats.reading_minutes())))
                    }),
            )
            .right(
                h_flex()
                    .gap_4()
                    .when(AppSettings::get(cx).layout.shows_editor(), |this| {
                        this.child(item(if selection.is_empty() {
                            format!("Ln {}, Col {}", position.line + 1, position.character + 1)
                        } else {
                            format!(
                                "Ln {}, Col {} ({} selected)",
                                position.line + 1,
                                position.character + 1,
                                group_thousands(selection.len())
                            )
                        }))
                    })
                    .child(item(self.line_ending.label().to_string()))
                    .child(item("Markdown".to_string())),
            )
    }
}

/// Show the version and a one-line description of Malgel.
pub fn open_about(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, _| {
        alert
            .title("Malgel")
            .description(concat!(
                "Version ",
                env!("CARGO_PKG_VERSION"),
                ". A fast, native Markdown editor built with GPUI Kit."
            ))
            .ok_text("OK")
    });
}

/// Horizontal inset that keeps text within a readable column, centered in a
/// pane of `width`, but never tighter than the standard pane padding.
fn column_inset(width: Pixels, cx: &App) -> Pixels {
    let rem = cx.theme().font_size;
    let min = rem * 1.5;
    let column = rem * READABLE_WIDTH_REMS;
    ((width - column) / 2.).max(min)
}

fn heading_icon(level: u8) -> Icon {
    Icon::new(match level {
        1 => IconName::Heading1,
        2 => IconName::Heading2,
        3 => IconName::Heading3,
        _ => IconName::Heading,
    })
}

/// A file name from the first heading, for documents never saved.
fn suggested_file_name(analysis: &Analysis, extension: &str) -> String {
    let stem: String = analysis
        .outline
        .first()
        .map(|heading| {
            heading
                .text
                .chars()
                .filter(|ch| !matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
                .take(60)
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "Untitled".to_string());
    format!("{stem}.{extension}")
}

fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, ch) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_window_title(window);
        // Keyboard commands need a focused element inside the workspace; if
        // focus was lost (e.g. its element left the layout), take it back.
        if window.focused(cx).is_none() {
            let focus = self.focus_handle.clone();
            window.defer(cx, move |window, cx| {
                if window.focused(cx).is_none() {
                    focus.focus(window, cx);
                }
            });
        }
        let layout = AppSettings::get(cx).layout;

        let content = match layout {
            Layout::Editor => self.render_editor(window, cx).into_any_element(),
            Layout::Preview => self.render_preview(window, cx).into_any_element(),
            Layout::Split => h_resizable("workspace-split")
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
        };

        v_flex()
            .id("workspace")
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::open_recent))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::export_html))
            .on_action(cx.listener(Self::reveal_in_folder))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::set_layout))
            .on_action(cx.listener(Self::toggle_scroll_sync))
            .on_action(cx.listener(Self::toggle_soft_wrap))
            .on_action(cx.listener(Self::toggle_line_numbers))
            .on_action(cx.listener(Self::toggle_appearance))
            .on_action(cx.listener(Self::go_to_heading))
            .on_action(|_: &About, window, cx| open_about(window, cx))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom(1., cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom(-1., cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom(0., cx)))
            .on_action(cx.listener(|this, _: &Bold, window, cx| {
                this.inline(format::Inline::Bold, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Italic, window, cx| {
                this.inline(format::Inline::Italic, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Strikethrough, window, cx| {
                this.inline(format::Inline::Strikethrough, window, cx)
            }))
            .on_action(cx.listener(|this, _: &InlineCode, window, cx| {
                this.inline(format::Inline::Code, window, cx)
            }))
            .on_action(cx.listener(|this, _: &InsertLink, window, cx| {
                this.apply_edit(format::insert_link, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CodeBlock, window, cx| {
                this.apply_edit(format::insert_code_block, window, cx)
            }))
            .on_action(cx.listener(|this, action: &Heading, window, cx| {
                this.block(format::Block::Heading(action.0), window, cx)
            }))
            .on_action(cx.listener(|this, _: &Quote, window, cx| {
                this.block(format::Block::Quote, window, cx)
            }))
            .on_action(cx.listener(|this, _: &BulletList, window, cx| {
                this.block(format::Block::BulletList, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NumberedList, window, cx| {
                this.block(format::Block::NumberedList, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TaskList, window, cx| {
                this.block(format::Block::TaskList, window, cx)
            }))
            .on_drop(cx.listener(Self::on_drop_paths))
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .child(self.render_title_bar(cx))
            .child(div().flex_1().min_h_0().overflow_hidden().child(content))
            .child(self.render_status_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn suggests_file_name_from_first_heading() {
        let analysis = Analysis::of("# Q3: Plans/Goals?\n\nBody");
        assert_eq!(suggested_file_name(&analysis, "md"), "Q3 PlansGoals.md");
        assert_eq!(
            suggested_file_name(&Analysis::default(), "html"),
            "Untitled.html"
        );
    }
}
