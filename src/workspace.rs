//! The window: title bar, open documents (as tabs when enabled), the file
//! and outline sidebars, and the status bar. It routes commands to the
//! document showing, asks before unsaved changes are lost, notices files
//! changed by other programs, and remembers the session.

use std::{
    cell::Cell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EntityId, ExternalPaths, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    PathPromptOptions, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, WindowBounds,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, IndexPath, Selectable as _, Sizable as _, TitleBar, WindowExt as _,
        button::{Button, ButtonGroup, ButtonVariants as _},
        checkbox::Checkbox,
        command::{Command, CommandItem, CommandState},
        dialog::DialogFooter,
        h_flex,
        input::Escape,
        link::Link,
        menu::AppMenuBar,
        menu::{DropdownMenu as _, PopupMenuItem},
        notification::Notification,
        radio::RadioGroup,
        resizable::{h_resizable, resizable_panel},
        scroll::ScrollableElement as _,
        status_bar::StatusBar,
        switch::Switch,
        v_flex,
    },
    div,
    prelude::FluentBuilder as _,
    px, rems,
};

use crate::{
    actions::*,
    analysis::{Analysis, content_hash},
    default_app, diagram_viewer,
    document::{self, LineEnding, display_name, is_markdown_path},
    document_view::{DocumentEvent, DocumentView},
    docx, export,
    file_tree::{self, FileTree, FileTreeEvent},
    format, i18n,
    links::{self, LinkTarget},
    pdf::{self, Paper},
    rich_copy,
    session::{self, DiskStamp, Session, SessionDocument, WindowPlacement},
    settings::{Appearance, Layout, LinkPlacement, clamp_font_size},
    spell,
    themes::{self, AppSettings},
};

/// Shown when Malgel starts for the first time.
const WELCOME: &str = include_str!("welcome.md");
/// How often open files and the file sidebar are checked for changes made
/// by other programs.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// What to do once unsaved changes have been saved or discarded.
#[derive(Clone)]
enum AfterConfirm {
    NewFile,
    OpenDialog,
    OpenPath(PathBuf),
    /// Open a linked document in place of the current one, at a heading.
    OpenLink(PathBuf, Option<String>),
    CloseDocument(EntityId),
    CloseWindow,
    Quit,
}

/// Where a document being opened goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// A new tab when tabs are on, otherwise in place of the current
    /// document.
    Default,
    NewTab,
    ReplaceActive,
}

/// What the watcher found for one open file.
enum DiskChange {
    Missing,
    Changed {
        text: String,
        line_ending: LineEnding,
        stamp: Option<DiskStamp>,
    },
    Touched(Option<DiskStamp>),
}

pub struct Workspace {
    focus_handle: FocusHandle,
    documents: Vec<Entity<DocumentView>>,
    active: usize,
    headings: Entity<CommandState>,
    file_tree: Entity<FileTree>,
    app_menu_bar: Entity<AppMenuBar>,

    window_title: String,
    closing: bool,
    /// Documents whose unsaved changes the user chose to discard while
    /// closing.
    discarded: HashSet<EntityId>,
    /// Files changed on disk that are waiting for the user to decide.
    pending_disk_prompt: HashSet<EntityId>,
    last_session: Option<Session>,
    _watch_task: Task<()>,
    document_subscriptions: Vec<(EntityId, Vec<Subscription>)>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// A workspace restoring the last session (when enabled) and opening
    /// `paths`.
    pub fn new(
        paths: Vec<PathBuf>,
        app_menu_bar: Entity<AppMenuBar>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = AppSettings::get(cx).clone();
        let headings = cx.new(|cx| CommandState::new(window, cx));
        let file_tree = cx.new(|_| FileTree::new());

        let subscriptions = vec![
            cx.subscribe_in(
                &file_tree,
                window,
                |this, _, event, window, cx| match event {
                    FileTreeEvent::Open(path) => this.open_paths(vec![path.clone()], window, cx),
                    FileTreeEvent::Moved { from, to } => this.files_moved(from, to, window, cx),
                    FileTreeEvent::Deleted(path) => this.files_deleted(path, window, cx),
                    FileTreeEvent::Error(message) => this.notify_error(message.clone(), window, cx),
                },
            ),
            cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
                this.on_settings_changed(window, cx);
                cx.notify();
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                if AppSettings::get(cx).appearance == Appearance::System {
                    themes::apply(Some(window), cx);
                }
            }),
            cx.observe_window_bounds(window, |this, window, cx| this.save_session(window, cx)),
        ];

        // Ask before a window with unsaved changes closes.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.request_close(window, cx))
                .unwrap_or(true)
        });

        let watch_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_INTERVAL).await;
                if this
                    .update_in(cx, |this, window, cx| this.check_disk(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            documents: Vec::new(),
            active: 0,
            headings,
            file_tree,
            app_menu_bar,
            window_title: String::new(),
            closing: false,
            discarded: HashSet::new(),
            pending_disk_prompt: HashSet::new(),
            last_session: None,
            _watch_task: watch_task,
            document_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.restore(paths, &settings, window, cx);
        this
    }

    /// Reopen the last session's documents and anything recovered after a
    /// crash, then the files asked for.
    fn restore(
        &mut self,
        paths: Vec<PathBuf>,
        settings: &crate::settings::Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = Session::load();
        let first_run = previous.is_none();
        let previous = previous.unwrap_or_default();
        let mut active = None;
        let mut referenced = HashSet::new();

        for (ix, entry) in previous.documents.iter().enumerate() {
            let recovery = entry.recovery.as_deref().and_then(|id| {
                referenced.insert(id.to_string());
                session::read_recovery(id).map(|recovery| (id.to_string(), recovery))
            });
            // Unsaved changes always come back; saved documents only when
            // the session is restored (just the active one without tabs).
            let wanted = settings.restore_session
                && paths.is_empty()
                && (settings.tabs || ix == previous.active);
            let doc = match recovery {
                Some((id, recovery)) => Some(self.restored_document(id, recovery, window, cx)),
                None if wanted => entry.path.as_ref().and_then(|path| {
                    let loaded = document::load(path).ok()?;
                    Some(cx.new(|cx| {
                        DocumentView::new(
                            loaded.text,
                            Some(path.clone()),
                            loaded.line_ending,
                            window,
                            cx,
                        )
                    }))
                }),
                None => None,
            };
            if let Some(doc) = doc {
                let cursor = entry.cursor;
                doc.update(cx, |doc, cx| doc.set_cursor(cursor, window, cx));
                if ix == previous.active {
                    active = Some(self.documents.len());
                }
                self.add_document(doc, false, window, cx);
            }
        }

        let orphans = session::orphaned_recoveries(&referenced);
        let recovered = orphans.len()
            + self
                .documents
                .iter()
                .filter(|doc| doc.read(cx).is_dirty())
                .count();
        for (id, recovery) in orphans {
            let doc = self.restored_document(id, recovery, window, cx);
            self.add_document(doc, false, window, cx);
        }
        if recovered > 0 {
            let message = if recovered == 1 {
                tr!("notify.recovered_one").to_string()
            } else {
                tr!("notify.recovered_many", count = recovered).to_string()
            };
            // The window can show notifications once it has finished opening.
            window.defer(cx, move |window, cx| {
                window.push_notification(Notification::info(message), cx)
            });
        }

        if settings.restore_session {
            self.file_tree
                .update(cx, |tree, cx| tree.set_root(previous.folder.clone(), cx));
        }
        self.last_session = Some(previous);

        // Files being opened replace this untouched placeholder.
        if self.documents.is_empty() {
            let text = if first_run && paths.is_empty() {
                WELCOME.to_string()
            } else {
                String::new()
            };
            let doc = cx.new(|cx| DocumentView::new(text, None, LineEnding::Lf, window, cx));
            self.add_document(doc, false, window, cx);
        }
        self.active = active
            .unwrap_or(0)
            .min(self.documents.len().saturating_sub(1));
        if !paths.is_empty() {
            self.open_paths(paths, window, cx);
        }
        self.focus_active(window, cx);
        self.sync_file_tree(cx);
    }

    fn restored_document(
        &mut self,
        id: String,
        recovery: session::Recovery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<DocumentView> {
        cx.new(|cx| {
            let mut doc = DocumentView::new(String::new(), None, LineEnding::Lf, window, cx);
            doc.restore_unsaved(id, recovery, window, cx);
            doc
        })
    }

    // ---------------------------------------------------------------------
    // Documents

    fn active_document(&self) -> &Entity<DocumentView> {
        &self.documents[self.active.min(self.documents.len() - 1)]
    }

    fn document_index(&self, id: EntityId) -> Option<usize> {
        self.documents.iter().position(|doc| doc.entity_id() == id)
    }

    fn shows_tabs(&self, cx: &App) -> bool {
        let settings = AppSettings::get(cx);
        !settings.focus_mode && (settings.tabs || self.documents.len() > 1)
    }

    fn add_document(
        &mut self,
        doc: Entity<DocumentView>,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscriptions = vec![
            cx.observe(&doc, |_, _, cx| cx.notify()),
            cx.subscribe_in(&doc, window, |_, _, event, window, cx| match event {
                DocumentEvent::Notify(message) => {
                    window.push_notification(Notification::info(message.clone()), cx)
                }
            }),
        ];
        self.document_subscriptions
            .push((doc.entity_id(), subscriptions));
        self.documents.push(doc);
        if activate {
            self.activate(self.documents.len() - 1, window, cx);
        }
    }

    fn remove_document(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.document_index(id) else {
            return;
        };
        let doc = self.documents.remove(ix);
        doc.update(cx, |doc, _| doc.discard_recovery());
        self.document_subscriptions
            .retain(|(subscribed, _)| *subscribed != id);
        if self.documents.is_empty() {
            let doc =
                cx.new(|cx| DocumentView::new(String::new(), None, LineEnding::Lf, window, cx));
            self.add_document(doc, false, window, cx);
        }
        if self.active > ix || self.active >= self.documents.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.activate(self.active, window, cx);
    }

    /// Put `doc` in place of the active document.
    fn replace_active(
        &mut self,
        doc: Entity<DocumentView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = self.active_document().entity_id();
        let position = self.active;
        self.add_document(doc, false, window, cx);
        let new = self.documents.pop().expect("just added");
        self.documents.insert(position, new);
        if let Some(ix) = self.document_index(old) {
            let removed = self.documents.remove(ix);
            removed.update(cx, |doc, _| doc.discard_recovery());
            self.document_subscriptions
                .retain(|(subscribed, _)| *subscribed != old);
        }
        self.activate(position, window, cx);
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.documents.is_empty() {
            return;
        }
        self.active = ix.min(self.documents.len() - 1);
        self.focus_active(window, cx);
        self.sync_file_tree(cx);
        self.save_session(window, cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        let focus = self.active_document().read(cx).focus_target(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    fn sync_file_tree(&self, cx: &mut App) {
        let path = self.active_document().read(cx).path().cloned();
        let show_folder = AppSettings::get(cx).file_sidebar;
        self.file_tree.update(cx, |tree, cx| {
            // With no folder open, show the document's.
            if show_folder && tree.root().is_none() {
                tree.set_root(
                    path.as_deref()
                        .and_then(Path::parent)
                        .map(Path::to_path_buf),
                    cx,
                );
            }
            tree.set_active(path, cx)
        });
    }

    /// Open `paths`: as tabs when tabs are on, otherwise in place of the
    /// current document (asking about unsaved changes first).
    fn open_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = AppSettings::get(cx).tabs;
        for path in paths {
            let path = path.canonicalize().unwrap_or(path);
            if let Some(ix) = self
                .documents
                .iter()
                .position(|doc| doc.read(cx).path() == Some(&path))
            {
                self.activate(ix, window, cx);
                continue;
            }
            let active = self.active_document().read(cx);
            if !tabs && active.is_dirty() {
                // One document at a time: settle this one first.
                self.confirm_unsaved(
                    self.active_document().clone(),
                    AfterConfirm::OpenPath(path),
                    window,
                    cx,
                );
                return;
            }
            self.load_path(path, Placement::Default, None, window, cx);
        }
    }

    /// Read `path` in the background and show it where `placement` says,
    /// scrolled to the heading `anchor` names, if any.
    fn load_path(
        &mut self,
        path: PathBuf,
        placement: Placement,
        anchor: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let read_path = path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { document::load(&read_path) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(loaded) => {
                    AppSettings::update(cx, |settings| settings.push_recent(&path));
                    let doc = cx.new(|cx| {
                        DocumentView::new(
                            loaded.text,
                            Some(path.clone()),
                            loaded.line_ending,
                            window,
                            cx,
                        )
                    });
                    let active = this.active_document().read(cx);
                    let replace = active.is_blank()
                        || match placement {
                            Placement::Default => !AppSettings::get(cx).tabs,
                            Placement::NewTab => false,
                            Placement::ReplaceActive => true,
                        };
                    if replace {
                        this.replace_active(doc.clone(), window, cx);
                    } else {
                        this.add_document(doc.clone(), true, window, cx);
                    }
                    if let Some(anchor) = anchor {
                        this.reveal_anchor(&doc, &anchor, window, cx);
                    }
                }
                Err(err) => {
                    AppSettings::update(cx, |settings| {
                        settings.recent_files.retain(|recent| recent != &path)
                    });
                    this.notify_error(
                        tr!("error.open", name = display_name(Some(&path)), error = err),
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn new_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let doc = cx.new(|cx| DocumentView::new(String::new(), None, LineEnding::Lf, window, cx));
        if AppSettings::get(cx).tabs {
            self.add_document(doc, true, window, cx);
        } else {
            self.replace_active(doc, window, cx);
        }
    }

    fn notify_error(&self, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        window.push_notification(Notification::error(message), cx);
    }

    fn update_window_title(&mut self, window: &mut Window, cx: &App) {
        let doc = self.active_document().read(cx);
        let title = format!(
            "{}{} — Malgel",
            doc.name(),
            if doc.is_dirty() { " (edited)" } else { "" }
        );
        if title != self.window_title {
            window.set_window_title(&title);
            window.set_window_edited(self.documents.iter().any(|doc| doc.read(cx).is_dirty()));
            self.window_title = title;
        }
    }

    // ---------------------------------------------------------------------
    // Settings

    fn on_settings_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = AppSettings::get(cx).clone();
        if settings.file_sidebar && self.file_tree.read(cx).root().is_none() {
            let dir = self.active_document().read(cx).dir();
            self.file_tree.update(cx, |tree, cx| tree.set_root(dir, cx));
            self.sync_file_tree(cx);
        }
        self.save_session(window, cx);
    }

    fn toggle(&mut self, update: impl FnOnce(&mut crate::settings::Settings), cx: &mut App) {
        AppSettings::update(cx, update);
    }

    // ---------------------------------------------------------------------
    // Session

    /// Remember the open documents, folder and window, when anything about
    /// them changed.
    fn save_session(&mut self, window: &mut Window, cx: &mut App) {
        if self.closing && self.last_session.is_none() {
            return;
        }
        let documents = self
            .documents
            .iter()
            .filter_map(|doc| {
                let doc = doc.read(cx);
                let recovery = doc
                    .is_dirty()
                    .then(|| doc.recovery_id().map(str::to_string))
                    .flatten();
                (doc.path().is_some() || recovery.is_some()).then(|| SessionDocument {
                    path: doc.path().cloned(),
                    cursor: doc.cursor(cx),
                    recovery,
                })
            })
            .collect::<Vec<_>>();
        let active_id = self.active_document().entity_id();
        let active = self
            .documents
            .iter()
            .filter(|doc| {
                let doc = doc.read(cx);
                doc.path().is_some() || doc.recovery_id().is_some()
            })
            .position(|doc| doc.entity_id() == active_id)
            .unwrap_or(0);
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (bounds, false),
            WindowBounds::Maximized(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, true),
        };
        let session = Session {
            documents,
            active,
            folder: self.file_tree.read(cx).root().map(Path::to_path_buf),
            window: Some(WindowPlacement {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
                maximized,
            }),
        };
        if self.last_session.as_ref() != Some(&session) {
            session.save();
            self.last_session = Some(session);
        }
    }

    // ---------------------------------------------------------------------
    // Watching files

    /// Look for open files other programs changed, moved or deleted, and
    /// refresh the file sidebar.
    fn check_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if AppSettings::get(cx).file_sidebar {
            self.file_tree.update(cx, |tree, cx| tree.refresh(cx));
        }
        // Save cursor positions and newly written recovery files.
        self.save_session(window, cx);

        let targets: Vec<(EntityId, PathBuf, Option<DiskStamp>, u64)> = self
            .documents
            .iter()
            .filter_map(|doc| {
                let id = doc.entity_id();
                let doc = doc.read(cx);
                let path = doc.path()?.clone();
                (!self.pending_disk_prompt.contains(&id))
                    .then(|| (id, path, doc.disk_stamp(), doc.saved_hash()))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let task = cx.background_executor().spawn(async move {
            targets
                .into_iter()
                .filter_map(|(id, path, known, saved_hash)| {
                    let stamp = DiskStamp::of(&path);
                    if stamp == known {
                        return None;
                    }
                    let change = match stamp {
                        None if known.is_some() => DiskChange::Missing,
                        None => return None,
                        Some(_) => match document::load(&path) {
                            Ok(loaded) if content_hash(&loaded.text) == saved_hash => {
                                DiskChange::Touched(stamp)
                            }
                            Ok(loaded) => DiskChange::Changed {
                                text: loaded.text,
                                line_ending: loaded.line_ending,
                                stamp,
                            },
                            Err(_) => return None,
                        },
                    };
                    Some((id, change))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let changes = task.await;
            _ = this.update_in(cx, |this, window, cx| {
                for (id, change) in changes {
                    this.apply_disk_change(id, change, window, cx);
                }
            });
        })
        .detach();
    }

    fn apply_disk_change(
        &mut self,
        id: EntityId,
        change: DiskChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.document_index(id).map(|ix| self.documents[ix].clone()) else {
            return;
        };
        let name = doc.read(cx).name();
        match change {
            DiskChange::Touched(stamp) => doc.update(cx, |doc, _| doc.set_disk_stamp(stamp)),
            DiskChange::Missing => {
                doc.update(cx, |doc, cx| doc.mark_missing(cx));
                window
                    .push_notification(Notification::warning(tr!("disk.missing", name = name)), cx);
            }
            DiskChange::Changed {
                text,
                line_ending,
                stamp,
            } => {
                if !doc.read(cx).is_dirty() {
                    doc.update(cx, |doc, cx| doc.reload(text, line_ending, window, cx));
                    return;
                }
                if window.has_active_dialog(cx) {
                    // Ask once the current dialog is answered.
                    return;
                }
                doc.update(cx, |doc, _| doc.set_disk_stamp(stamp));
                self.pending_disk_prompt.insert(id);
                if let Some(ix) = self.document_index(id) {
                    self.activate(ix, window, cx);
                }
                let workspace = cx.weak_entity();
                let text = Arc::new(text);
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let (keep, reload) = (workspace.clone(), workspace.clone());
                    let (reload_doc, text) = (doc.clone(), text.clone());
                    alert
                        .title(tr!("disk.changed_title", name = name))
                        .description(tr!("disk.changed_description"))
                        .footer(
                            DialogFooter::new()
                                .justify_end()
                                .child(
                                    Button::new("reload")
                                        .label(tr!("disk.use_disk"))
                                        .outline()
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let text = text.as_ref().clone();
                                            reload_doc.update(cx, |doc, cx| {
                                                doc.reload(text, line_ending, window, cx)
                                            });
                                            _ = reload.update(cx, |this, cx| {
                                                this.pending_disk_prompt.remove(&id);
                                                this.focus_active(window, cx);
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("keep")
                                        .label(tr!("disk.keep_mine"))
                                        .primary()
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            _ = keep.update(cx, |this, cx| {
                                                this.pending_disk_prompt.remove(&id);
                                                this.focus_active(window, cx);
                                            });
                                        }),
                                ),
                        )
                });
            }
        }
    }

    // ---------------------------------------------------------------------
    // File commands

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        if AppSettings::get(cx).tabs {
            self.new_document(window, cx);
        } else {
            self.confirm_unsaved(
                self.active_document().clone(),
                AfterConfirm::NewFile,
                window,
                cx,
            );
        }
    }

    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        if AppSettings::get(cx).tabs {
            self.prompt_open(window, cx);
        } else {
            self.confirm_unsaved(
                self.active_document().clone(),
                AfterConfirm::OpenDialog,
                window,
                cx,
            );
        }
    }

    fn open_recent(&mut self, action: &OpenRecent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = AppSettings::get(cx).recent_files.get(action.0).cloned() else {
            return;
        };
        self.open_paths(vec![path], window, cx);
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("dialog.open_folder")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.file_tree
                    .update(cx, |tree, cx| tree.set_root(Some(folder), cx));
                this.sync_file_tree(cx);
                AppSettings::update(cx, |settings| settings.file_sidebar = true);
                this.save_session(window, cx);
            });
        })
        .detach();
    }

    fn close_document(&mut self, _: &CloseDocument, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shows_tabs(cx) {
            self.confirm_close(AfterConfirm::CloseWindow, window, cx);
            return;
        }
        let doc = self.active_document().clone();
        let id = doc.entity_id();
        self.confirm_unsaved(doc, AfterConfirm::CloseDocument(id), window, cx);
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close(AfterConfirm::CloseWindow, window, cx);
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close(AfterConfirm::Quit, window, cx);
    }

    /// Called by the platform before the window closes.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return true;
        }
        let unsaved = self
            .documents
            .iter()
            .any(|doc| doc.read(cx).is_dirty() && !self.discarded.contains(&doc.entity_id()));
        if unsaved {
            self.confirm_close(AfterConfirm::CloseWindow, window, cx);
            return false;
        }
        self.save_session(window, cx);
        self.closing = true;
        true
    }

    /// Close the window or quit once every document with unsaved changes
    /// has been saved or discarded, asking about each in turn.
    fn confirm_close(&mut self, next: AfterConfirm, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self
            .documents
            .iter()
            .find(|doc| doc.read(cx).is_dirty() && !self.discarded.contains(&doc.entity_id()));
        match pending.cloned() {
            Some(doc) => {
                if let Some(ix) = self.document_index(doc.entity_id()) {
                    self.activate(ix, window, cx);
                }
                self.confirm_unsaved(doc, next, window, cx);
            }
            None => self.run_after_confirm(next, window, cx),
        }
    }

    /// Run `next` now, or once the user has saved or discarded `doc`'s
    /// changes.
    fn confirm_unsaved(
        &mut self,
        doc: Entity<DocumentView>,
        next: AfterConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !doc.read(cx).is_dirty() {
            self.run_after_confirm(next, window, cx);
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }

        let name = doc.read(cx).name();
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (save, discard, cancel) = (workspace.clone(), workspace.clone(), workspace.clone());
            let (save_next, discard_next) = (next.clone(), next.clone());
            let (save_doc, discard_doc) = (doc.clone(), doc.clone());
            alert
                .title(tr!("unsaved.title", name = name))
                .description(tr!("unsaved.description"))
                .footer(
                    DialogFooter::new()
                        .justify_end()
                        .child(
                            Button::new("discard")
                                .label(tr!("unsaved.dont_save"))
                                .outline()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let next = discard_next.clone();
                                    let id = discard_doc.entity_id();
                                    discard_doc.update(cx, |doc, _| doc.discard_recovery());
                                    _ = discard.update(cx, |this, cx| {
                                        this.discarded.insert(id);
                                        this.focus_active(window, cx);
                                        this.run_after_confirm(next, window, cx)
                                    });
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("cancel")
                                .label(tr!("button.cancel"))
                                .outline()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    _ = cancel.update(cx, |this, cx| {
                                        this.discarded.clear();
                                        this.focus_active(window, cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("save")
                                .label(tr!("button.save"))
                                .primary()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let next = save_next.clone();
                                    let doc = save_doc.clone();
                                    _ = save.update(cx, |this, cx| {
                                        this.focus_active(window, cx);
                                        this.save_document(doc, Some(next), window, cx)
                                    });
                                }),
                        ),
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
            AfterConfirm::NewFile => self.new_document(window, cx),
            AfterConfirm::OpenDialog => self.prompt_open(window, cx),
            AfterConfirm::OpenPath(path) => {
                self.load_path(path, Placement::Default, None, window, cx)
            }
            AfterConfirm::OpenLink(path, anchor) => {
                self.load_path(path, Placement::ReplaceActive, anchor, window, cx)
            }
            AfterConfirm::CloseDocument(id) => {
                self.discarded.remove(&id);
                self.remove_document(id, window, cx);
            }
            AfterConfirm::CloseWindow | AfterConfirm::Quit => {
                // More documents may still be waiting to be asked about.
                let pending = self.documents.iter().any(|doc| {
                    doc.read(cx).is_dirty() && !self.discarded.contains(&doc.entity_id())
                });
                if pending {
                    self.confirm_close(next, window, cx);
                    return;
                }
                self.save_session(window, cx);
                self.closing = true;
                if matches!(next, AfterConfirm::Quit) {
                    cx.quit();
                } else {
                    window.remove_window();
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // Files changed from the sidebar

    /// Documents (and recent files) in what moved follow it.
    fn files_moved(&mut self, from: &Path, to: &Path, window: &mut Window, cx: &mut Context<Self>) {
        for doc in self.documents.clone() {
            doc.update(cx, |doc, cx| {
                if let Some(path) = doc
                    .path()
                    .and_then(|path| file_tree::moved_path(path, from, to))
                {
                    doc.moved_to(path, cx);
                }
            });
        }
        AppSettings::update(cx, |settings| {
            for recent in &mut settings.recent_files {
                if let Some(path) = file_tree::moved_path(recent, from, to) {
                    *recent = path;
                }
            }
        });
        self.sync_file_tree(cx);
        self.save_session(window, cx);
        cx.notify();
    }

    /// Documents in what was deleted close, unless they have unsaved
    /// changes, which stay open to be saved elsewhere.
    fn files_deleted(&mut self, deleted: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let closing: Vec<EntityId> = self
            .documents
            .iter()
            .filter(|doc| {
                let doc = doc.read(cx);
                !doc.is_dirty() && doc.path().is_some_and(|path| path.starts_with(deleted))
            })
            .map(|doc| doc.entity_id())
            .collect();
        for id in closing {
            self.remove_document(id, window, cx);
        }
        AppSettings::update(cx, |settings| {
            settings
                .recent_files
                .retain(|recent| !recent.starts_with(deleted))
        });
    }

    // ---------------------------------------------------------------------
    // Links

    /// Follow a link clicked in the active document's preview.
    fn follow_link(&mut self, action: &FollowLink, window: &mut Window, cx: &mut Context<Self>) {
        let doc = self.active_document().clone();
        let base_dir = doc.read(cx).dir();
        let Some(target) = links::classify(&action.url, base_dir.as_deref()) else {
            return;
        };
        match target {
            LinkTarget::Heading(anchor) => self.reveal_anchor(&doc, &anchor, window, cx),
            LinkTarget::External(url) => {
                if AppSettings::get(cx).confirm_web_links {
                    confirm_external_link(url, window, cx);
                } else {
                    cx.open_url(&url);
                }
            }
            LinkTarget::NeedsSavedDocument => {
                self.notify_error(tr!("links.save_first"), window, cx)
            }
            LinkTarget::File { path, anchor } => {
                if !path.exists() {
                    // Name the file as the link does, where it can.
                    let shown = base_dir
                        .as_deref()
                        .and_then(|dir| path.strip_prefix(dir).ok())
                        .unwrap_or(&path);
                    self.notify_error(tr!("links.not_found", name = shown.display()), window, cx);
                } else if path.is_file() && is_markdown_path(&path) {
                    let mut new_tab = AppSettings::get(cx).open_links_in == LinkPlacement::NewTab;
                    if action.alternate {
                        new_tab = !new_tab;
                    }
                    self.open_linked_document(path, anchor, new_tab, window, cx);
                } else {
                    confirm_open_file(path, window, cx);
                }
            }
        }
    }

    /// Show a diagram of the active document in a window of its own.
    fn view_diagram(&mut self, action: &ViewDiagram, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.active_document().read(cx).name();
        diagram_viewer::open(
            &action.0,
            tr!("diagram.window_title", name = name).to_string(),
            cx,
        );
    }

    /// Show the Markdown file `path` (in a new tab, or in place of the
    /// active document), scrolled to `anchor`.
    fn open_linked_document(
        &mut self,
        path: PathBuf,
        anchor: Option<String>,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = path.canonicalize().unwrap_or(path);
        if let Some(ix) = self
            .documents
            .iter()
            .position(|doc| doc.read(cx).path() == Some(&path))
        {
            self.activate(ix, window, cx);
            if let Some(anchor) = anchor {
                let doc = self.documents[ix].clone();
                self.reveal_anchor(&doc, &anchor, window, cx);
            }
            return;
        }
        if new_tab {
            self.load_path(path, Placement::NewTab, anchor, window, cx);
        } else {
            self.confirm_unsaved(
                self.active_document().clone(),
                AfterConfirm::OpenLink(path, anchor),
                window,
                cx,
            );
        }
    }

    /// Scroll `doc` to the heading `anchor` names, once it is laid out.
    fn reveal_anchor(
        &mut self,
        doc: &Entity<DocumentView>,
        anchor: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let doc = doc.clone();
        let anchor = anchor.to_string();
        window.defer(cx, move |window, cx| {
            doc.update(cx, |doc, cx| doc.reveal_anchor(&anchor, window, cx));
        });
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: AppSettings::get(cx).tabs,
            prompt: Some(tr!("dialog.open")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                for path in paths {
                    this.load_path(path, Placement::Default, None, window, cx);
                }
            });
        })
        .detach();
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let doc = self.active_document().clone();
        self.save_document(doc, None, window, cx);
    }

    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        let doc = self.active_document().clone();
        self.prompt_save_path(doc, None, window, cx);
    }

    /// Save `doc`, asking for a location first when it has none, then
    /// continue with `next`.
    fn save_document(
        &mut self,
        doc: Entity<DocumentView>,
        next: Option<AfterConfirm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match doc.read(cx).path().cloned() {
            Some(path) => {
                if self.write(&doc, &path, window, cx)
                    && let Some(next) = next
                {
                    self.run_after_confirm(next, window, cx);
                }
            }
            None => self.prompt_save_path(doc, next, window, cx),
        }
    }

    fn prompt_save_path(
        &mut self,
        doc: Entity<DocumentView>,
        next: Option<AfterConfirm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (dir, name) = {
            let doc = doc.read(cx);
            let dir = default_dir(doc.dir());
            let name = match doc.path() {
                Some(_) => doc.name(),
                None => suggested_file_name(doc.analysis(), "md"),
            };
            (dir, name)
        };
        let chosen = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                if this.write(&doc, &path, window, cx) {
                    AppSettings::update(cx, |settings| settings.push_recent(&path));
                    this.sync_file_tree(cx);
                    if let Some(next) = next {
                        this.run_after_confirm(next, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Write `doc` to `path`; reports failures to the user.
    fn write(
        &mut self,
        doc: &Entity<DocumentView>,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match doc.update(cx, |doc, cx| doc.write_to(path, cx)) {
            Ok(()) => {
                self.save_session(window, cx);
                true
            }
            Err(err) => {
                self.notify_error(
                    tr!(
                        "error.save",
                        name = display_name(Some(&path.to_path_buf())),
                        error = err
                    ),
                    window,
                    cx,
                );
                false
            }
        }
    }

    fn export_html(&mut self, _: &ExportHtml, window: &mut Window, cx: &mut Context<Self>) {
        self.export(ExportFormat::Html, window, cx);
    }

    fn export_pdf(&mut self, _: &ExportPdf, window: &mut Window, cx: &mut Context<Self>) {
        self.export(ExportFormat::Pdf, window, cx);
    }

    fn export_docx(&mut self, _: &ExportDocx, window: &mut Window, cx: &mut Context<Self>) {
        self.export(ExportFormat::Docx, window, cx);
    }

    /// Ask where to save, then render and write the export in the
    /// background.
    fn export(&mut self, format: ExportFormat, window: &mut Window, cx: &mut Context<Self>) {
        let doc = self.active_document().read(cx);
        let dir = default_dir(doc.dir());
        let title = doc
            .analysis()
            .outline
            .first()
            .map(|heading| heading.text.clone())
            .unwrap_or_else(|| doc.name());
        let extension = format.extension();
        let name = match doc.path() {
            Some(path) => format!(
                "{}.{extension}",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ),
            None => suggested_file_name(doc.analysis(), extension),
        };
        let source = doc.source().clone();
        let base_dir = doc.dir();
        let chosen = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let target = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let paper = Paper::from_locale();
                    let base_dir = base_dir.as_deref();
                    let bytes = match format {
                        ExportFormat::Html => Ok(export::to_html(&source, &title).into_bytes()),
                        ExportFormat::Pdf => pdf::to_pdf(&source, &title, base_dir, paper),
                        ExportFormat::Docx => docx::to_docx(&source, &title, base_dir, paper),
                    }?;
                    std::fs::write(&target, bytes).map_err(|err| err.to_string())
                })
                .await;
            _ = cx.update(|window, cx| {
                let name = display_name(Some(&path));
                let note = match result {
                    Ok(()) => Notification::success(tr!("export.done", name = name)),
                    Err(err) => Notification::error(tr!("export.failed", name = name, error = err)),
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    /// Copy the selection, or the whole document, as formatted text.
    fn copy_rich_text(&mut self, _: &CopyRichText, window: &mut Window, cx: &mut Context<Self>) {
        let doc = self.active_document().read(cx);
        let editor = doc.editor().read(cx);
        let selection = editor.selected_range();
        let markdown = if selection.is_empty() || !AppSettings::get(cx).layout.shows_editor() {
            doc.source().to_string()
        } else {
            editor.value()[selection].to_string()
        };
        let base_dir = doc.dir();
        let task = cx.background_executor().spawn({
            let markdown = markdown.clone();
            async move { export::to_clipboard_html(&markdown, base_dir.as_deref()) }
        });
        cx.spawn_in(window, async move |_, cx| {
            let html = task.await;
            _ = cx.update(|window, cx| {
                let note = match rich_copy::copy_html(html, &markdown) {
                    Ok(()) => Notification::success(tr!("copy.rich_done")),
                    Err(err) => Notification::error(tr!("copy.failed", error = err)),
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
        match self.active_document().read(cx).path().cloned() {
            Some(path) => cx.reveal_path(&path),
            None => window.push_notification(Notification::info(tr!("reveal.save_first")), cx),
        }
    }

    fn on_drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let documents: Vec<PathBuf> = paths
            .paths()
            .iter()
            .filter(|path| is_markdown_path(path))
            .cloned()
            .collect();
        if !documents.is_empty() {
            self.open_paths(documents, window, cx);
            return;
        }
        if AppSettings::get(cx).layout.shows_editor() || AppSettings::get(cx).focus_mode {
            let paths = paths.paths().to_vec();
            self.active_document()
                .clone()
                .update(cx, |doc, cx| doc.drop_images(&paths, window, cx));
            return;
        }
        self.notify_error(tr!("drop.not_markdown"), window, cx);
    }

    // ---------------------------------------------------------------------
    // View commands

    fn set_layout(&mut self, action: &SetLayout, window: &mut Window, cx: &mut Context<Self>) {
        let layout = action.0;
        AppSettings::update(cx, |settings| {
            settings.layout = layout;
            settings.focus_mode = false;
        });
        self.focus_active(window, cx);
    }

    fn toggle_focus_mode(
        &mut self,
        _: &ToggleFocusMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle(|settings| settings.focus_mode = !settings.focus_mode, cx);
        self.focus_active(window, cx);
    }

    fn go_to_heading(&mut self, _: &GoToHeading, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        self.headings
            .update(cx, |state, cx| state.set_query("", window, cx));
        let headings = self.headings.clone();
        let doc = self.active_document().clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let focus = headings.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| focus.focus(window, cx));
            let outline = doc.read(cx).analysis().outline.clone();
            let items = outline.iter().map(|heading| {
                CommandItem::new()
                    .label(heading.text.clone())
                    .keywords([heading.text.clone()])
                    .icon(heading_icon(heading.level))
            });
            let confirm = doc.clone();
            dialog.close_button(false).p_0().child(
                Command::new(&headings)
                    .bordered(false)
                    .placeholder(tr!("heading.placeholder"))
                    .max_h(rems(24.))
                    .items(items)
                    .empty(|_, _, _| tr!("heading.none"))
                    .on_confirm(move |index: IndexPath, window, cx| {
                        window.close_dialog(cx);
                        confirm.update(cx, |doc, cx| doc.reveal_heading(index.row, window, cx));
                    })
                    .on_cancel(|window, cx| window.close_dialog(cx)),
            )
        });
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

    fn with_active(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut DocumentView, &mut Window, &mut Context<DocumentView>),
    ) {
        let doc = self.active_document().clone();
        doc.update(cx, |doc, cx| f(doc, window, cx));
    }

    fn recheck_spelling(&mut self, cx: &mut Context<Self>) {
        for doc in self.documents.clone() {
            doc.update(cx, |doc, cx| doc.recheck_spelling(cx));
        }
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        open_settings(window, cx);
    }

    // ---------------------------------------------------------------------
    // Rendering

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = AppSettings::get(cx);
        let layout = settings.layout;
        let is_dark = cx.theme().is_dark();
        let show_menu_bar = !cfg!(target_os = "macos");
        let workspace = cx.weak_entity();
        let doc = self.active_document().read(cx);

        let layout_button = |id: &'static str, icon: IconName, tooltip: SharedString, value| {
            Button::new(id)
                .icon(Icon::new(icon))
                .tooltip(tooltip)
                .selected(!settings.focus_mode && layout == value)
        };

        TitleBar::new()
            .on_close_window(move |_, window, cx| {
                _ = workspace.update(cx, |this, cx| {
                    this.confirm_close(AfterConfirm::CloseWindow, window, cx)
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
                            .child(doc.name()),
                    )
                    .when(doc.is_dirty(), |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("— Edited"),
                        )
                    }),
            )
            .child(
                h_flex()
                    .id("title-bar-buttons")
                    // Take clicks for these buttons rather than the title
                    // bar's drag area behind them, which on Windows would
                    // treat them as clicks on the caption.
                    .occlude()
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
                                tr!("menu.editor"),
                                Layout::Editor,
                            ))
                            .child(layout_button(
                                "layout-split",
                                IconName::Columns2,
                                tr!("menu.editor_and_preview"),
                                Layout::Split,
                            ))
                            .child(layout_button(
                                "layout-preview",
                                IconName::Eye,
                                tr!("menu.preview"),
                                Layout::Preview,
                            ))
                            .child(layout_button(
                                "layout-live",
                                IconName::SquarePen,
                                tr!("menu.live_preview"),
                                Layout::Live,
                            ))
                            .on_click(cx.listener(|_, clicked: &Vec<usize>, window, cx| {
                                let layout = match clicked.first() {
                                    Some(0) => Layout::Editor,
                                    Some(2) => Layout::Preview,
                                    Some(3) => Layout::Live,
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
                                tr!("title.light")
                            } else {
                                tr!("title.dark")
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleAppearance), cx)
                            }),
                    ),
            )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id("tabs")
            .h(px(34.))
            .flex_none()
            .px_1()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.tab_bar)
            .overflow_x_scrollbar()
            .children(self.documents.iter().enumerate().map(|(ix, doc)| {
                let doc = doc.read(cx);
                let active = ix == self.active;
                let dirty = doc.is_dirty();
                let theme = cx.theme();
                h_flex()
                    .id(("tab", ix))
                    .group("tab")
                    .h(px(28.))
                    .max_w(px(220.))
                    .flex_none()
                    .pl_3()
                    .pr_1()
                    .gap_1()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(active, |this| {
                        this.bg(theme.tab_active)
                            .text_color(theme.tab_active_foreground)
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .when(!active, |this| {
                        this.text_color(theme.muted_foreground)
                            .hover(|style| style.bg(theme.list_hover))
                    })
                    .child(div().truncate().child(doc.name()))
                    .child(
                        div()
                            .size(px(18.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .hover(|style| style.bg(theme.list_active))
                            .child(div().when(dirty, |this| {
                                this.child(
                                    div().size(px(7.)).rounded_full().bg(theme.muted_foreground),
                                )
                                .group_hover("tab", |style| style.invisible())
                            }))
                            .child(
                                div()
                                    .absolute()
                                    .invisible()
                                    .when(active && !dirty, |this| this.visible())
                                    .group_hover("tab", |style| style.visible())
                                    .child(Icon::new(IconName::Close).xsmall()),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    let doc = this.documents[ix].clone();
                                    let id = doc.entity_id();
                                    this.confirm_unsaved(
                                        doc,
                                        AfterConfirm::CloseDocument(id),
                                        window,
                                        cx,
                                    );
                                }),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            let doc = this.documents[ix].clone();
                            let id = doc.entity_id();
                            this.confirm_unsaved(doc, AfterConfirm::CloseDocument(id), window, cx);
                        }),
                    )
            }))
            .child(
                Button::new("new-tab")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Plus))
                    .tooltip(tr!("tabs.new"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewFile), cx)),
            )
    }

    fn render_outline(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let doc = self.active_document().read(cx);
        let outline = doc.analysis().outline.clone();
        let current = doc.current_heading(cx);
        let min_level = outline
            .iter()
            .map(|heading| heading.level)
            .min()
            .unwrap_or(1);
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_3()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("OUTLINE"),
            )
            .child(
                v_flex()
                    .id("outline")
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .pb_2()
                    .overflow_y_scrollbar()
                    .when(outline.is_empty(), |this| {
                        this.child(
                            div()
                                .p_3()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(tr!("outline.empty")),
                        )
                    })
                    .children(outline.into_iter().enumerate().map(|(ix, heading)| {
                        let theme = cx.theme();
                        let active = Some(ix) == current;
                        div()
                            .id(("heading", ix))
                            .flex_none()
                            .py_1()
                            .pr_2()
                            .pl(px(
                                10. + 12. * f32::from(heading.level.saturating_sub(min_level))
                            ))
                            .rounded_md()
                            .text_sm()
                            .truncate()
                            .cursor_pointer()
                            .when(heading.level <= min_level, |this| {
                                this.font_weight(FontWeight::MEDIUM)
                            })
                            .when(active, |this| {
                                this.bg(theme.list_active).text_color(theme.foreground)
                            })
                            .when(!active, |this| {
                                this.text_color(theme.muted_foreground)
                                    .hover(|style| style.bg(theme.list_hover))
                            })
                            .child(heading.text.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.with_active(window, cx, |doc, window, cx| {
                                    doc.reveal_heading(ix, window, cx)
                                });
                            }))
                    })),
            )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let doc = self.active_document().read(cx);
        let stats = doc.analysis().stats;
        let editor = doc.editor().read(cx);
        let position = editor.cursor_position();
        let selection = editor.selected_range();
        let line_ending = doc.line_ending();
        let settings = AppSettings::get(cx);
        let muted = cx.theme().muted_foreground;
        let item = |text: String| div().text_color(muted).child(text);

        StatusBar::new()
            .left(
                h_flex()
                    .gap_4()
                    .child(item(if stats.words == 1 {
                        tr!("status.words_one", count = 1).to_string()
                    } else {
                        tr!("status.words_other", count = group_thousands(stats.words)).to_string()
                    }))
                    .child(item(if stats.characters == 1 {
                        tr!("status.chars_one", count = 1).to_string()
                    } else {
                        tr!(
                            "status.chars_other",
                            count = group_thousands(stats.characters)
                        )
                        .to_string()
                    }))
                    .when(stats.words > 0, |this| {
                        this.child(item(
                            tr!("status.read", count = stats.reading_minutes()).to_string(),
                        ))
                    }),
            )
            .right(
                h_flex()
                    .gap_4()
                    .when(settings.layout.shows_editor(), |this| {
                        this.child(item(if selection.is_empty() {
                            tr!(
                                "status.position",
                                line = position.line + 1,
                                column = position.character + 1
                            )
                            .to_string()
                        } else {
                            tr!(
                                "status.position_selected",
                                line = position.line + 1,
                                column = position.character + 1,
                                count = group_thousands(selection.len())
                            )
                            .to_string()
                        }))
                    })
                    .when(settings.spell_check, |this| {
                        this.child(item(
                            settings
                                .spell_language
                                .clone()
                                .unwrap_or_else(spell::default_language)
                                .replace('_', "-"),
                        ))
                    })
                    .child(item(line_ending.label().to_string()))
                    .child(item("Markdown".to_string())),
            )
    }
}

/// The settings dialog: Malgel's optional features and how it behaves.
pub fn open_settings(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    // Ask the system afresh each time, as the user may have changed the
    // default app elsewhere.
    default_app::refresh(cx);
    window.open_dialog(cx, |dialog, _, cx| {
        let settings = AppSettings::get(cx).clone();
        let default_status = default_app::status(cx);
        let row = |id: &'static str,
                   title: SharedString,
                   description: SharedString,
                   checked: bool,
                   update: fn(&mut crate::settings::Settings, bool),
                   cx: &App| {
            h_flex()
                .gap_4()
                .py_2()
                .justify_between()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        ),
                )
                .child(
                    Switch::new(id)
                        .checked(checked)
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            AppSettings::update(cx, |settings| update(settings, checked));
                        }),
                )
        };
        let section = |title: SharedString| {
            div()
                .pt_3()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(title)
        };
        dialog.title(tr!("settings.title")).w(px(560.)).child(
            v_flex()
                .child(section(tr!("settings.section.workspace")))
                .child(language_row(settings.language.clone(), cx))
                .child(row(
                    "tabs",
                    tr!("settings.tabs.title"),
                    tr!("settings.tabs.description"),
                    settings.tabs,
                    |settings, on| settings.tabs = on,
                    cx,
                ))
                .child(row(
                    "file-sidebar",
                    tr!("settings.sidebar.title"),
                    tr!("settings.sidebar.description"),
                    settings.file_sidebar,
                    |settings, on| settings.file_sidebar = on,
                    cx,
                ))
                .child(row(
                    "outline",
                    tr!("settings.outline.title"),
                    tr!("settings.outline.description"),
                    settings.outline_sidebar,
                    |settings, on| settings.outline_sidebar = on,
                    cx,
                ))
                .child(row(
                    "restore-session",
                    tr!("settings.restore.title"),
                    tr!("settings.restore.description"),
                    settings.restore_session,
                    |settings, on| settings.restore_session = on,
                    cx,
                ))
                .child(section(tr!("settings.section.writing")))
                .child(row(
                    "spell-check",
                    tr!("settings.spell.title"),
                    tr!("settings.spell.description"),
                    settings.spell_check,
                    |settings, on| settings.spell_check = on,
                    cx,
                ))
                .child(row(
                    "scroll-sync",
                    tr!("settings.sync.title"),
                    tr!("settings.sync.description"),
                    settings.scroll_sync,
                    |settings, on| settings.scroll_sync = on,
                    cx,
                ))
                .child(row(
                    "soft-wrap",
                    tr!("settings.wrap.title"),
                    tr!("settings.wrap.description"),
                    settings.soft_wrap,
                    |settings, on| settings.soft_wrap = on,
                    cx,
                ))
                .child(row(
                    "line-numbers",
                    tr!("settings.numbers.title"),
                    tr!("settings.numbers.description"),
                    settings.line_numbers,
                    |settings, on| settings.line_numbers = on,
                    cx,
                ))
                .child(section(tr!("settings.section.links")))
                .child(
                    h_flex()
                        .gap_4()
                        .py_2()
                        .justify_between()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(tr!("settings.links.open_in")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(if cfg!(target_os = "macos") {
                                            tr!("settings.links.mac_hint")
                                        } else {
                                            tr!("settings.links.hint")
                                        }),
                                ),
                        )
                        .child(
                            RadioGroup::horizontal("open-links-in")
                                .flex_none()
                                .child(tr!("settings.links.new_tab"))
                                .child(tr!("settings.links.same_tab"))
                                .selected_index(Some(match settings.open_links_in {
                                    LinkPlacement::NewTab => 0,
                                    LinkPlacement::SameTab => 1,
                                }))
                                .on_click(|ix, _, cx| {
                                    let placement = if *ix == 0 {
                                        LinkPlacement::NewTab
                                    } else {
                                        LinkPlacement::SameTab
                                    };
                                    AppSettings::update(cx, |settings| {
                                        settings.open_links_in = placement
                                    });
                                }),
                        ),
                )
                .child(row(
                    "confirm-web-links",
                    tr!("settings.links.confirm_title"),
                    tr!("settings.links.confirm_description"),
                    settings.confirm_web_links,
                    |settings, on| settings.confirm_web_links = on,
                    cx,
                ))
                .child(section(tr!("settings.section.files")))
                .child(default_app_row(default_status, cx))
                .child(section(tr!("settings.section.updates")))
                .child(row(
                    "check-for-updates",
                    tr!("settings.updates.title"),
                    tr!("settings.updates.description"),
                    settings.check_for_updates,
                    |settings, on| settings.check_for_updates = on,
                    cx,
                )),
        )
    });
}

/// The settings row that picks the interface language.
fn language_row(language: Option<String>, cx: &App) -> impl IntoElement {
    let name = |code: &str| {
        i18n::LANGUAGES
            .iter()
            .find(|(candidate, _)| *candidate == code)
            .map_or("English", |(_, name)| *name)
    };
    let system = tr!(
        "settings.language.system",
        language = name(i18n::system_language())
    );
    let chosen = language.as_deref().and_then(i18n::supported);
    let current = chosen.map_or(system.clone(), |code| SharedString::from(name(code)));
    h_flex()
        .gap_4()
        .py_2()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("settings.language.title")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("settings.language.description")),
                ),
        )
        .child(
            Button::new("language")
                .label(current)
                .small()
                .outline()
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.item(
                        PopupMenuItem::new(system.clone())
                            .checked(chosen.is_none())
                            .on_click(|_, _, cx| {
                                AppSettings::update(cx, |settings| settings.language = None)
                            }),
                    );
                    for (code, name) in i18n::LANGUAGES {
                        menu = menu.item(
                            PopupMenuItem::new(name)
                                .checked(chosen == Some(code))
                                .on_click(move |_, _, cx| {
                                    AppSettings::update(cx, |settings| {
                                        settings.language = Some(code.to_string())
                                    })
                                }),
                        );
                    }
                    menu
                }),
        )
}

/// The settings row that makes Malgel the app for Markdown files.
fn default_app_row(status: default_app::Status, cx: &App) -> impl IntoElement {
    let is_default = status == default_app::Status::Default;
    let description = if is_default {
        tr!("default_app.is_default")
    } else if default_app::CHOSEN_IN_SYSTEM_SETTINGS {
        tr!("default_app.windows_hint")
    } else {
        tr!("default_app.description")
    };
    let action = if is_default {
        h_flex()
            .gap_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::Check).small())
            .child(tr!("default_app.default"))
            .into_any_element()
    } else {
        let label = if default_app::CHOSEN_IN_SYSTEM_SETTINGS {
            tr!("default_app.open_settings")
        } else {
            tr!("default_app.make_default")
        };
        Button::new("make-default")
            .label(label)
            .small()
            .outline()
            .on_click(|_, window, cx| {
                let note = match default_app::make_default(cx) {
                    Ok(default_app::Outcome::Done) => {
                        Notification::success(tr!("default_app.done"))
                    }
                    Ok(default_app::Outcome::SettingsOpened) => {
                        Notification::info(tr!("default_app.settings_opened"))
                    }
                    Err(message) => Notification::error(message),
                };
                window.push_notification(note, cx);
                window.refresh();
            })
            .into_any_element()
    };
    h_flex()
        .gap_4()
        .py_2()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("default_app.title")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .child(action)
}

/// Malgel's website, linked from the About dialog.
const WEBSITE: &str = "https://www.malgel.com";

/// Show the version, a one-line description, the website and the license.
pub fn open_about(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, cx| {
        let muted = cx.theme().muted_foreground;
        alert
            .title("Malgel")
            .description(
                v_flex()
                    .gap_2()
                    .child(tr!("about.version", version = env!("CARGO_PKG_VERSION")))
                    .child(
                        Link::new("website")
                            .href(WEBSITE)
                            .child(WEBSITE.trim_start_matches("https://")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(tr!("about.license")),
                    ),
            )
            .ok_text(tr!("button.ok"))
    });
}

/// Ask before opening a web link (or an email address, or another app's
/// link) outside Malgel, as a document's links can lead anywhere.
fn confirm_external_link(url: String, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let web = url.starts_with("http://") || url.starts_with("https://");
    let dont_ask = Rc::new(Cell::new(false));
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (url, open_url) = (url.clone(), url.clone());
        let (checkbox, remember) = (dont_ask.clone(), dont_ask.clone());
        alert
            .title(if web {
                tr!("web_link.title_browser")
            } else {
                tr!("web_link.title")
            })
            .description(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .px_2()
                            .py_1p5()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(url.chars().take(400).collect::<String>()),
                    )
                    .when(web, |this| {
                        this.child(
                            Checkbox::new("dont-ask")
                                .label(tr!("web_link.dont_ask"))
                                .checked(dont_ask.get())
                                .on_click(move |checked, window, _| {
                                    checkbox.set(*checked);
                                    window.refresh();
                                }),
                        )
                    }),
            )
            .footer(
                DialogFooter::new()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label(tr!("button.cancel"))
                            .outline()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("open")
                            .label(if web {
                                tr!("web_link.open_browser")
                            } else {
                                tr!("button.open")
                            })
                            .primary()
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                if remember.get() {
                                    AppSettings::update(cx, |settings| {
                                        settings.confirm_web_links = false
                                    });
                                }
                                cx.open_url(&open_url);
                            }),
                    ),
            )
    });
}

/// Ask before opening a linked file Malgel doesn't show itself (or a
/// folder) with the app the system uses for it.
fn confirm_open_file(path: PathBuf, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let folder = path.is_dir();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let path = path.clone();
        alert
            .title(tr!("file_link.title", name = name))
            .description(if folder {
                tr!("file_link.folder")
            } else {
                tr!("file_link.other")
            })
            .footer(
                DialogFooter::new()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label(tr!("button.cancel"))
                            .outline()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("open")
                            .label(tr!("button.open"))
                            .primary()
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                cx.open_with_system(&path);
                            }),
                    ),
            )
    });
}

fn default_dir(dir: Option<PathBuf>) -> PathBuf {
    dir.or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn heading_icon(level: u8) -> Icon {
    Icon::new(match level {
        1 => IconName::Heading1,
        2 => IconName::Heading2,
        3 => IconName::Heading3,
        _ => IconName::Heading,
    })
}

#[derive(Clone, Copy)]
enum ExportFormat {
    Html,
    Pdf,
    Docx,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            ExportFormat::Html => "html",
            ExportFormat::Pdf => "pdf",
            ExportFormat::Docx => "docx",
        }
    }
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
        .unwrap_or_else(|| tr!("doc.untitled").to_string());
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
        self.update_window_title(window, cx);
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
        let settings = AppSettings::get(cx).clone();
        let focus_mode = settings.focus_mode;
        let show_files = settings.file_sidebar && !focus_mode;
        let show_outline = settings.outline_sidebar && !focus_mode;
        let document = self.active_document().clone();
        let rem = window.rem_size();
        let sidebar_range = rems(12.).to_pixels(rem)..rems(28.).to_pixels(rem);
        let files_width = rems(15.).to_pixels(rem);
        let outline_width = rems(14.).to_pixels(rem);
        let mut document_width = window.viewport_size().width;
        if show_files {
            document_width -= files_width;
        }
        if show_outline {
            document_width -= outline_width;
        }

        // Each arrangement of panes keeps its own sizes, so adding or
        // removing a sidebar starts it at its natural width.
        let mut main = h_resizable(match (show_files, show_outline) {
            (true, true) => "panes-files-outline",
            (true, false) => "panes-files",
            (false, true) => "panes-outline",
            (false, false) => "panes",
        });
        if show_files {
            main = main.child(
                resizable_panel()
                    .size(files_width)
                    .size_range(sidebar_range.clone())
                    .child(self.file_tree.clone()),
            );
        }
        main = main.child(
            resizable_panel()
                .size(document_width)
                .size_range(rems(20.).to_pixels(rem)..Pixels::MAX)
                .child(
                    v_flex()
                        .size_full()
                        .when(self.shows_tabs(cx), |this| this.child(self.render_tabs(cx)))
                        .child(div().flex_1().min_h_0().child(document)),
                ),
        );
        if show_outline {
            main = main.child(
                resizable_panel()
                    .size(outline_width)
                    .size_range(sidebar_range)
                    .child(self.render_outline(cx)),
            );
        }

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
            .on_action(cx.listener(Self::follow_link))
            .on_action(cx.listener(Self::view_diagram))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::export_html))
            .on_action(cx.listener(Self::export_pdf))
            .on_action(cx.listener(Self::export_docx))
            .on_action(cx.listener(Self::copy_rich_text))
            .on_action(cx.listener(Self::reveal_in_folder))
            .on_action(cx.listener(Self::close_document))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::set_layout))
            .on_action(cx.listener(Self::go_to_heading))
            .on_action(cx.listener(Self::toggle_appearance))
            .on_action(cx.listener(Self::toggle_focus_mode))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                let next = (this.active + 1) % this.documents.len();
                this.activate(next, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                let count = this.documents.len();
                this.activate((this.active + count - 1) % count, window, cx);
            }))
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                this.activate(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if AppSettings::get(cx).focus_mode {
                    this.toggle(|settings| settings.focus_mode = false, cx);
                    this.focus_active(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleScrollSync, _, cx| {
                this.toggle(|settings| settings.scroll_sync = !settings.scroll_sync, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleSoftWrap, _, cx| {
                this.toggle(|settings| settings.soft_wrap = !settings.soft_wrap, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleLineNumbers, _, cx| {
                this.toggle(
                    |settings| settings.line_numbers = !settings.line_numbers,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &ToggleTabs, _, cx| {
                this.toggle(|settings| settings.tabs = !settings.tabs, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleFileSidebar, _, cx| {
                this.toggle(
                    |settings| settings.file_sidebar = !settings.file_sidebar,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &ToggleOutline, _, cx| {
                this.toggle(
                    |settings| settings.outline_sidebar = !settings.outline_sidebar,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &ToggleSpellCheck, _, cx| {
                this.toggle(|settings| settings.spell_check = !settings.spell_check, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRestoreSession, _, cx| {
                this.toggle(
                    |settings| settings.restore_session = !settings.restore_session,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, action: &SetSpellLanguage, _, cx| {
                let language = action.0.to_string();
                this.toggle(
                    |settings| {
                        settings.spell_language = Some(language);
                        settings.spell_check = true;
                    },
                    cx,
                );
            }))
            .on_action(
                cx.listener(|this, action: &ReplaceMisspelling, window, cx| {
                    let action = action.clone();
                    this.with_active(window, cx, |doc, window, cx| {
                        doc.replace_misspelling(&action, window, cx)
                    });
                }),
            )
            .on_action(cx.listener(|this, action: &AddToDictionary, _, cx| {
                spell::add_to_personal(&action.0);
                this.recheck_spelling(cx);
            }))
            .on_action(|_: &About, window, cx| open_about(window, cx))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom(1., cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom(-1., cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom(0., cx)))
            .on_action(cx.listener(|this, _: &TidyTable, window, cx| {
                this.with_active(window, cx, |doc, window, cx| doc.tidy_table(window, cx))
            }))
            .on_action(cx.listener(|this, _: &Bold, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.inline(format::Inline::Bold, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &Italic, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.inline(format::Inline::Italic, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &Strikethrough, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.inline(format::Inline::Strikethrough, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &InlineCode, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.inline(format::Inline::Code, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &InsertLink, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.apply_edit(format::insert_link, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &CodeBlock, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.apply_edit(format::insert_code_block, window, cx)
                })
            }))
            .on_action(cx.listener(|this, action: &Heading, window, cx| {
                let level = action.0;
                this.with_active(window, cx, |doc, window, cx| {
                    doc.block(format::Block::Heading(level), window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &Quote, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.block(format::Block::Quote, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &BulletList, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.block(format::Block::BulletList, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &NumberedList, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.block(format::Block::NumberedList, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &TaskList, window, cx| {
                this.with_active(window, cx, |doc, window, cx| {
                    doc.block(format::Block::TaskList, window, cx)
                })
            }))
            .on_drop(cx.listener(Self::on_drop_paths))
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .child(self.render_title_bar(cx))
            .child(div().flex_1().min_h_0().overflow_hidden().child(main))
            .when(!focus_mode, |this| this.child(self.render_status_bar(cx)))
    }
}

impl Workspace {
    /// Open files handed over by the system while running (macOS "Open
    /// with", a second launch).
    pub fn open_external(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let documents: Vec<PathBuf> = paths.into_iter().filter(|path| path.is_file()).collect();
        if !documents.is_empty() {
            self.open_paths(documents, window, cx);
        }
    }
}

/// Where the last session's window was, if it still fits a display.
pub fn restored_bounds(cx: &App) -> Option<WindowBounds> {
    let placement = Session::load()?.window?;
    if !AppSettings::get(cx).restore_session {
        return None;
    }
    let bounds = Bounds::new(
        gpui_kit::point(px(placement.x), px(placement.y)),
        gpui_kit::size(
            px(placement.width.max(560.)),
            px(placement.height.max(360.)),
        ),
    );
    let visible = cx.displays().iter().any(|display| {
        let area = display.bounds();
        area.intersects(&bounds)
    });
    let bounds = if placement.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    visible.then_some(bounds)
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
