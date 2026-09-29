//! The file sidebar: a folder's Markdown and text files as a tree.
//!
//! Folders are listed when first expanded and re-listed in the background
//! when the workspace refreshes, so the tree follows files created, renamed
//! and deleted by other programs.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use gpui_kit::{
    Context, EventEmitter, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, Styled as _, Task, UniformListScrollHandle, Window,
    assets::IconName,
    component::{
        ActiveTheme as _, Icon, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
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
        let (hover, active_bg, muted) =
            (theme.list_hover, theme.list_active, theme.muted_foreground);
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
                let path = entry.path.clone();
                Some(
                    h_flex()
                        .id(ix)
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
                        .child(
                            div()
                                .truncate()
                                .child(SharedString::from(entry.name.clone())),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if entry.is_dir {
                                    this.toggle(path.clone(), cx);
                                } else {
                                    cx.emit(FileTreeEvent::Open(path.clone()));
                                }
                            }),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let name = self
            .root
            .as_ref()
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "Files".to_string());

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
                Button::new("open-folder")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::FolderOpen))
                    .tooltip("Open folder…")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
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
                .child("No folder open")
                .child(
                    Button::new("open-folder-empty")
                        .small()
                        .label("Open folder…")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
                )
                .into_any_element()
        } else if self.rows.is_empty() && self.refresh_task.is_none() {
            div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No Markdown files here")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .px_1()
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
