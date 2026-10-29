//! The file sidebar: a folder's Markdown and text files as a tree.
//!
//! Folders are listed when first expanded and re-listed in the background
//! when the workspace refreshes, so the tree follows files created, renamed
//! and deleted by other programs.
//!
//! Files and folders can be created, renamed in place, deleted (to the
//! system's trash) and dragged onto a folder to move them there; the
//! workspace hears of each change, so open documents follow along.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, UniformListScrollHandle, WeakEntity, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        h_flex,
        input::{Escape, Input, InputEvent, InputState},
        menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
        scroll::ScrollableElement as _,
        v_flex,
    },
    div,
    prelude::FluentBuilder as _,
    px, uniform_list,
};

use crate::{actions::OpenFolder, document::is_markdown_path};

/// Folders never worth showing: tool caches and dependencies.
const SKIPPED_FOLDERS: &[&str] = &["node_modules", "target", "__pycache__", "venv", "dist"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

struct Row {
    entry: Entry,
    depth: usize,
}

pub enum FileTreeEvent {
    Open(PathBuf),
    /// A file or folder was renamed or moved.
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    /// A file or folder went to the trash.
    Deleted(PathBuf),
    /// Something went wrong, for the user to hear.
    Error(SharedString),
}

/// A file or folder being renamed in place.
struct Renaming {
    path: PathBuf,
    input: Entity<InputState>,
    /// It was just created: open it once it has its name.
    open_after: bool,
    _subscriptions: Vec<Subscription>,
}

/// A file or folder being dragged to another folder.
#[derive(Clone)]
pub struct DraggedEntry {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

impl Render for DraggedEntry {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .text_sm()
            .text_color(theme.popover_foreground)
            .child(
                Icon::new(if self.is_dir {
                    IconName::Folder
                } else {
                    IconName::FileText
                })
                .small()
                .text_color(theme.muted_foreground),
            )
            .child(self.name.clone())
    }
}

pub struct FileTree {
    root: Option<PathBuf>,
    expanded: HashSet<PathBuf>,
    listings: HashMap<PathBuf, Vec<Entry>>,
    rows: Vec<Row>,
    /// The file the workspace is showing.
    active: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    refresh_task: Option<Task<()>>,
    renaming: Option<Renaming>,
    /// A refresh asked for while one was running, to run when it ends.
    refresh_again: bool,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl FileTree {
    pub fn new() -> Self {
        Self {
            root: None,
            expanded: HashSet::new(),
            listings: HashMap::new(),
            rows: Vec::new(),
            active: None,
            scroll: UniformListScrollHandle::new(),
            refresh_task: None,
            renaming: None,
            refresh_again: false,
        }
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Show `root`, or nothing.
    pub fn set_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.root == root {
            return;
        }
        self.root = root;
        self.expanded.clear();
        self.listings.clear();
        self.rows.clear();
        if let Some(root) = self.root.clone() {
            self.expanded.insert(root);
        }
        self.refresh(cx);
        cx.notify();
    }

    /// Highlight `path`, expanding the folders above it.
    pub fn set_active(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.active == path {
            return;
        }
        self.active = path;
        let mut revealed = false;
        if let (Some(root), Some(active)) = (&self.root, &self.active) {
            let mut dir = active.parent();
            while let Some(folder) = dir {
                if !folder.starts_with(root) {
                    break;
                }
                revealed |= self.expanded.insert(folder.to_path_buf());
                dir = folder.parent();
            }
        }
        if revealed {
            self.refresh(cx);
        } else {
            self.scroll_to_active();
        }
        cx.notify();
    }

    /// Re-list the root and every expanded folder in the background.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_task.is_some() {
            self.refresh_again = true;
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let folders: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|folder| folder.starts_with(&root))
            .cloned()
            .collect();
        let listing = cx.background_executor().spawn(async move {
            folders
                .into_iter()
                .filter_map(|folder| list(&folder).map(|entries| (folder, entries)))
                .collect::<HashMap<_, _>>()
        });
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let listings = listing.await;
            _ = this.update(cx, |this, cx| {
                this.refresh_task = None;
                if std::mem::take(&mut this.refresh_again) {
                    this.refresh(cx);
                }
                // Folders that disappeared collapse.
                this.expanded.retain(|folder| listings.contains_key(folder));
                if listings != this.listings {
                    this.listings = listings;
                    this.rebuild_rows();
                    this.scroll_to_active();
                    cx.notify();
                }
            });
        }));
    }

    fn rebuild_rows(&mut self) {
        fn add(tree: &FileTree, folder: &Path, depth: usize, rows: &mut Vec<Row>) {
            for entry in tree.listings.get(folder).into_iter().flatten() {
                rows.push(Row {
                    entry: entry.clone(),
                    depth,
                });
                if entry.is_dir && tree.expanded.contains(&entry.path) {
                    add(tree, &entry.path, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::new();
        if let Some(root) = &self.root {
            add(self, root, 0, &mut rows);
        }
        self.rows = rows;
    }

    fn scroll_to_active(&self) {
        if let Some(ix) = self
            .rows
            .iter()
            .position(|row| Some(&row.entry.path) == self.active.as_ref())
        {
            self.scroll
                .scroll_to_item(ix, gpui_kit::ScrollStrategy::Nearest);
        }
    }

    fn toggle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path.clone());
            if !self.listings.contains_key(&path) {
                self.refresh(cx);
                return;
            }
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let theme = cx.theme();
        let (hover, active_bg, muted, drop_bg) = (
            theme.list_hover,
            theme.list_active,
            theme.muted_foreground,
            theme.drop_target,
        );
        let tree = cx.weak_entity();
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let entry = row.entry.clone();
                let is_active = Some(&entry.path) == self.active.as_ref();
                let expanded = entry.is_dir && self.expanded.contains(&entry.path);
                let icon = match (entry.is_dir, expanded) {
                    (true, true) => IconName::FolderOpen,
                    (true, false) => IconName::Folder,
                    (false, _) => IconName::FileText,
                };
                // Dropped on a file, an item goes into the file's folder.
                let drop_folder = if entry.is_dir {
                    entry.path.clone()
                } else {
                    entry.path.parent()?.to_path_buf()
                };
                let renaming = self
                    .renaming
                    .as_ref()
                    .filter(|renaming| renaming.path == entry.path)
                    .map(|renaming| renaming.input.clone());
                let dragged = DraggedEntry {
                    path: entry.path.clone(),
                    name: entry.name.clone().into(),
                    is_dir: entry.is_dir,
                };
                let (click_entry, menu_entry, menu_tree) =
                    (entry.clone(), entry.clone(), tree.clone());
                Some(
                    h_flex()
                        .id(ix)
                        .w_full()
                        .h(px(26.))
                        .pl(px(8. + 14. * row.depth as f32))
                        .pr_2()
                        .gap_1p5()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .when(is_active, |this| {
                            this.bg(active_bg).font_weight(FontWeight::MEDIUM)
                        })
                        .when(!is_active, |this| this.hover(move |style| style.bg(hover)))
                        .child(div().w(px(12.)).flex_none().when(entry.is_dir, |this| {
                            this.child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall()
                                .text_color(muted),
                            )
                        }))
                        .child(Icon::new(icon).small().text_color(muted).flex_none())
                        .map(|this| match renaming {
                            Some(input) => this.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                                        this.cancel_rename(window, cx)
                                    }))
                                    .child(Input::new(&input).xsmall().w_full()),
                            ),
                            None => this.child(
                                div()
                                    .truncate()
                                    .child(SharedString::from(entry.name.clone())),
                            ),
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.renaming.is_some() {
                                return;
                            }
                            if click_entry.is_dir {
                                this.toggle(click_entry.path.clone(), cx);
                            } else {
                                cx.emit(FileTreeEvent::Open(click_entry.path.clone()));
                            }
                        }))
                        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                        .drag_over::<DraggedEntry>(move |style, _, _, _| style.bg(drop_bg))
                        .on_drop(cx.listener(move |this, dragged: &DraggedEntry, _, cx| {
                            cx.stop_propagation();
                            this.move_into(&dragged.path, &drop_folder, cx);
                        }))
                        .context_menu(move |menu, _, _| {
                            entry_menu(menu, &menu_tree, Some(&menu_entry))
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }

    // ---------------------------------------------------------------------
    // Changing files

    /// The folder new files go in when asked for from `entry`'s menu.
    fn folder_for(&self, entry: Option<&Entry>) -> Option<PathBuf> {
        match entry {
            Some(entry) if entry.is_dir => Some(entry.path.clone()),
            Some(entry) => entry.path.parent().map(Path::to_path_buf),
            None => self.root.clone(),
        }
    }

    /// Create an empty Markdown file in `folder` and name it in place; it
    /// opens once named.
    fn new_file(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = unique_path(&folder, &tr!("doc.untitled"), Some("md"));
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match created {
            Ok(_) => {
                self.show(&folder, cx);
                self.start_rename(path, true, window, cx);
            }
            Err(err) => cx.emit(FileTreeEvent::Error(tr!(
                "files.create_failed",
                name = file_name(&path),
                error = err
            ))),
        }
    }

    fn new_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = unique_path(&folder, &tr!("files.new_folder"), None);
        match std::fs::create_dir(&path) {
            Ok(()) => {
                self.show(&folder, cx);
                self.start_rename(path, false, window, cx);
            }
            Err(err) => cx.emit(FileTreeEvent::Error(tr!(
                "files.create_failed",
                name = file_name(&path),
                error = err
            ))),
        }
    }

    /// Expand `folder` (and those above it) and list it afresh.
    fn show(&mut self, folder: &Path, cx: &mut Context<Self>) {
        if let Some(root) = self.root.clone() {
            let mut dir = Some(folder);
            while let Some(folder) = dir.filter(|folder| folder.starts_with(&root)) {
                self.expanded.insert(folder.to_path_buf());
                dir = folder.parent();
            }
        }
        self.refresh(cx);
    }

    fn start_rename(
        &mut self,
        path: PathBuf,
        open_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(file_name(&path)));
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } | InputEvent::Blur => {
                            this.commit_rename(window, cx)
                        }
                        _ => {}
                    }
                }),
            ];
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(Renaming {
            path,
            input,
            open_after,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn commit_rename(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        cx.notify();
        let name = renaming.input.read(cx).value().trim().to_string();
        let mut final_path = renaming.path.clone();
        if name != file_name(&renaming.path) {
            match rename_target(&renaming.path, &name) {
                Err(problem) => cx.emit(FileTreeEvent::Error(problem)),
                Ok(target) => match std::fs::rename(&renaming.path, &target) {
                    Ok(()) => {
                        self.follow_move(&renaming.path, &target);
                        cx.emit(FileTreeEvent::Moved {
                            from: renaming.path.clone(),
                            to: target.clone(),
                        });
                        final_path = target;
                    }
                    Err(err) => cx.emit(FileTreeEvent::Error(tr!(
                        "files.rename_failed",
                        name = file_name(&renaming.path),
                        error = err
                    ))),
                },
            }
            self.refresh(cx);
        }
        if renaming.open_after {
            cx.emit(FileTreeEvent::Open(final_path));
        }
    }

    fn cancel_rename(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(renaming) = self.renaming.take() {
            if renaming.open_after {
                cx.emit(FileTreeEvent::Open(renaming.path));
            }
            cx.notify();
        }
    }

    /// Move `item` into `folder`, as a drag and drop asks.
    fn move_into(&mut self, item: &Path, folder: &Path, cx: &mut Context<Self>) {
        let target = match move_target(item, folder) {
            Ok(Some(target)) => target,
            Ok(None) => return,
            Err(problem) => {
                cx.emit(FileTreeEvent::Error(problem));
                return;
            }
        };
        match std::fs::rename(item, &target) {
            Ok(()) => {
                self.follow_move(item, &target);
                cx.emit(FileTreeEvent::Moved {
                    from: item.to_path_buf(),
                    to: target,
                });
                self.show(folder, cx);
            }
            Err(err) => cx.emit(FileTreeEvent::Error(tr!(
                "files.move_failed",
                name = file_name(item),
                error = err
            ))),
        }
    }

    /// Keep a moved folder's expanded subfolders, and the active file,
    /// where they went.
    fn follow_move(&mut self, from: &Path, to: &Path) {
        self.expanded = self
            .expanded
            .drain()
            .map(|folder| moved_path(&folder, from, to).unwrap_or(folder))
            .collect();
        if let Some(active) = self
            .active
            .as_ref()
            .and_then(|active| moved_path(active, from, to))
        {
            self.active = Some(active);
        }
    }

    /// Ask, then move `path` to the system's trash.
    fn confirm_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let tree = cx.weak_entity();
        let name = file_name(&path);
        let is_dir = path.is_dir();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (tree, path) = (tree.clone(), path.clone());
            let trash = if cfg!(windows) {
                tr!("files.delete_to_bin")
            } else {
                tr!("files.delete_to_trash")
            };
            let description = if is_dir {
                format!("{trash} {}", tr!("files.delete_folder_note"))
            } else {
                trash.to_string()
            };
            alert
                .title(tr!("files.delete_title", name = name))
                .description(description)
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
                            Button::new("delete")
                                .label(tr!("files.delete_button"))
                                .danger()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    _ = tree.update(cx, |tree, cx| tree.delete(&path, cx));
                                }),
                        ),
                )
        });
    }

    fn delete(&mut self, path: &Path, cx: &mut Context<Self>) {
        match trash::delete(path) {
            Ok(()) => {
                cx.emit(FileTreeEvent::Deleted(path.to_path_buf()));
                self.refresh(cx);
            }
            Err(err) => cx.emit(FileTreeEvent::Error(tr!(
                "files.delete_failed",
                name = file_name(path),
                error = err
            ))),
        }
    }
}

/// Something a menu item does to the tree.
type TreeAction = Box<dyn Fn(&mut FileTree, &mut Window, &mut Context<FileTree>)>;

/// The menu for `entry` (a file or folder), or for the folder's empty
/// space.
fn entry_menu(menu: PopupMenu, tree: &WeakEntity<FileTree>, entry: Option<&Entry>) -> PopupMenu {
    let action = |label: SharedString, icon: IconName, f: TreeAction| {
        let tree = tree.clone();
        PopupMenuItem::new(label)
            .icon(Icon::new(icon))
            .on_click(move |_, window, cx: &mut App| {
                _ = tree.update(cx, |tree, cx| f(tree, window, cx));
            })
    };
    let entry = entry.cloned();
    let for_new = entry.clone();
    let for_folder = entry.clone();
    let mut menu = menu
        .item(action(
            tr!("files.new_file"),
            IconName::FilePlus,
            Box::new(move |tree, window, cx| {
                if let Some(folder) = tree.folder_for(for_new.as_ref()) {
                    tree.new_file(folder, window, cx);
                }
            }),
        ))
        .item(action(
            tr!("files.new_folder"),
            IconName::FolderPlus,
            Box::new(move |tree, window, cx| {
                if let Some(folder) = tree.folder_for(for_folder.as_ref()) {
                    tree.new_folder(folder, window, cx);
                }
            }),
        ));
    let Some(entry) = entry else {
        return menu;
    };
    let (rename, delete, reveal) = (entry.path.clone(), entry.path.clone(), entry.path.clone());
    menu = menu
        .separator()
        .item(action(
            tr!("files.rename"),
            IconName::Pencil,
            Box::new(move |tree, window, cx| tree.start_rename(rename.clone(), false, window, cx)),
        ))
        .item(action(
            if cfg!(target_os = "macos") {
                tr!("menu.reveal_finder")
            } else {
                tr!("menu.show_in_folder")
            },
            IconName::FolderOpen,
            Box::new(move |_, _, cx| cx.reveal_path(&reveal)),
        ))
        .separator()
        .item(action(
            tr!("files.delete"),
            IconName::Trash,
            Box::new(move |tree, window, cx| tree.confirm_delete(delete.clone(), window, cx)),
        ));
    menu
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A new path in `folder`: `base` with `extension`, or `base 2`, `base 3`…
/// if that's taken.
pub fn unique_path(folder: &Path, base: &str, extension: Option<&str>) -> PathBuf {
    let name = |n: usize| {
        let stem = if n == 1 {
            base.to_string()
        } else {
            format!("{base} {n}")
        };
        match extension {
            Some(extension) => format!("{stem}.{extension}"),
            None => stem,
        }
    };
    (1..)
        .map(|n| folder.join(name(n)))
        .find(|path| !path.exists())
        .expect("some name is free")
}

/// Where renaming `path` to `name` puts it. A file keeps its extension
/// when the new name has none.
fn rename_target(path: &Path, name: &str) -> Result<PathBuf, SharedString> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(tr!("files.bad_name"));
    }
    let mut name = name.to_string();
    if !path.is_dir()
        && Path::new(&name).extension().is_none()
        && let Some(extension) = path.extension()
    {
        name = format!("{name}.{}", extension.to_string_lossy());
    }
    let target = path.with_file_name(&name);
    // Only a change of case may "replace" the file itself.
    let same_file =
        target.to_string_lossy().to_lowercase() == path.to_string_lossy().to_lowercase();
    if target.exists() && !same_file {
        return Err(tr!("files.exists", name = name));
    }
    Ok(target)
}

/// Where moving `item` into `folder` puts it: `None` when it's there
/// already.
fn move_target(item: &Path, folder: &Path) -> Result<Option<PathBuf>, SharedString> {
    if item.parent() == Some(folder) {
        return Ok(None);
    }
    if folder.starts_with(item) {
        return Err(tr!("files.into_itself"));
    }
    let name = item.file_name().ok_or_else(|| tr!("files.bad_name"))?;
    let target = folder.join(name);
    if target.exists() {
        return Err(tr!("files.exists", name = name.to_string_lossy()));
    }
    Ok(Some(target))
}

/// Where `path` is after `from` moved to `to`: `None` if it wasn't in it.
pub fn moved_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let name = self
            .root
            .as_ref()
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| tr!("files.title").to_string());

        let header = h_flex()
            .h(px(32.))
            .pl_3()
            .pr_1()
            .flex_none()
            .justify_between()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(div().truncate().child(name.to_uppercase()))
            .child(
                h_flex()
                    .flex_none()
                    .when(self.root.is_some(), |this| {
                        this.child(
                            Button::new("new-file")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::FilePlus))
                                .tooltip(tr!("files.new_file"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(root) = this.root.clone() {
                                        this.new_file(root, window, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("new-folder")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::FolderPlus))
                                .tooltip(tr!("files.new_folder"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(root) = this.root.clone() {
                                        this.new_folder(root, window, cx);
                                    }
                                })),
                        )
                    })
                    .child(
                        Button::new("open-folder")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::FolderOpen))
                            .tooltip(tr!("menu.open_folder"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(OpenFolder), cx)
                            }),
                    ),
            );

        let body = if self.root.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(tr!("files.no_folder"))
                .child(
                    Button::new("open-folder-empty")
                        .small()
                        .label(tr!("menu.open_folder"))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
                )
                .into_any_element()
        } else if self.rows.is_empty() && self.refresh_task.is_none() {
            let tree = cx.weak_entity();
            div()
                .id("file-tree-empty")
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(tr!("files.empty"))
                .context_menu(move |menu, _, _| entry_menu(menu, &tree, None))
                .into_any_element()
        } else {
            // No menu for the space around the rows: its listener would
            // open over every row's own menu. The header's buttons create
            // files and folders at the top.
            // Faint, so the folder under the pointer stands out.
            let drop_bg = theme.drop_target.opacity(0.3);
            div()
                .id("file-tree-body")
                .flex_1()
                .min_h_0()
                .px_1()
                // Dropped below the files, an item goes to the top folder.
                .drag_over::<DraggedEntry>(move |style, _, _, _| style.bg(drop_bg))
                .on_drop(cx.listener(|this, dragged: &DraggedEntry, _, cx| {
                    if let Some(root) = this.root.clone() {
                        this.move_into(&dragged.path, &root, cx);
                    }
                }))
                .child(
                    uniform_list(
                        "file-tree-rows",
                        self.rows.len(),
                        cx.processor(Self::render_rows),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };

        v_flex()
            .id("file-tree")
            .size_full()
            .bg(theme.sidebar)
            .child(header)
            .child(body)
    }
}

/// A folder's subfolders and Markdown files, folders first, by name.
fn list(folder: &Path) -> Option<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter_map(|item| {
            let name = item.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let path = item.path();
            // Follow links, so linked folders and files show as what they point to.
            let is_dir = path.is_dir();
            let keep = if is_dir {
                !SKIPPED_FOLDERS.contains(&name.as_str())
            } else {
                is_markdown_path(&path)
            };
            keep.then_some(Entry { name, path, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_new_names_and_moves() {
        let dir = std::env::temp_dir().join(format!("malgel-tree-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("notes/inner")).unwrap();
        std::fs::write(dir.join("Untitled.md"), "").unwrap();
        std::fs::write(dir.join("a.md"), "").unwrap();
        std::fs::write(dir.join("notes/a.md"), "").unwrap();

        assert_eq!(
            unique_path(&dir, "Untitled", Some("md")),
            dir.join("Untitled 2.md")
        );
        assert_eq!(
            unique_path(&dir, "New folder", None),
            dir.join("New folder")
        );

        // A file keeps its extension; a taken name or a path is refused.
        assert_eq!(
            rename_target(&dir.join("a.md"), "b").unwrap(),
            dir.join("b.md")
        );
        assert_eq!(
            rename_target(&dir.join("a.md"), "b.txt").unwrap(),
            dir.join("b.txt")
        );
        assert_eq!(
            rename_target(&dir.join("a.md"), "A").unwrap(),
            dir.join("A.md")
        );
        assert!(rename_target(&dir.join("a.md"), "Untitled").is_err());
        assert!(rename_target(&dir.join("a.md"), "x/y").is_err());
        assert!(rename_target(&dir.join("a.md"), "").is_err());
        assert_eq!(
            rename_target(&dir.join("notes"), "old").unwrap(),
            dir.join("old")
        );

        assert_eq!(move_target(&dir.join("a.md"), &dir).unwrap(), None);
        assert!(move_target(&dir.join("a.md"), &dir.join("notes")).is_err());
        assert_eq!(
            move_target(&dir.join("Untitled.md"), &dir.join("notes")).unwrap(),
            Some(dir.join("notes/Untitled.md"))
        );
        assert!(move_target(&dir.join("notes"), &dir.join("notes/inner")).is_err());

        assert_eq!(
            moved_path(
                &dir.join("notes/inner/x.md"),
                &dir.join("notes"),
                &dir.join("old")
            ),
            Some(dir.join("old/inner/x.md"))
        );
        assert_eq!(
            moved_path(&dir.join("notes"), &dir.join("notes"), &dir.join("old")),
            Some(dir.join("old"))
        );
        assert_eq!(
            moved_path(&dir.join("a.md"), &dir.join("notes"), &dir.join("old")),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lists_folders_first_and_only_documents() {
        let dir = std::env::temp_dir().join(format!("malgel-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for folder in ["b-notes", "node_modules", ".git"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for file in [
            "Zed.md",
            "alpha.markdown",
            "image.png",
            ".hidden.md",
            "todo.txt",
        ] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        let names: Vec<String> = list(&dir).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["b-notes", "alpha.markdown", "todo.txt", "Zed.md"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
