//! Malgel — a fast, native Markdown editor and previewer.

// Keep release builds on Windows from opening a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod analysis;
mod diagram;
mod document;
mod document_view;
mod docx;
mod export;
mod file_tree;
mod format;
mod images;
mod math;
mod menus;
mod pdf;
mod preview_ext;
mod rich_copy;
mod session;
mod settings;
mod smart_edit;
mod spell;
mod themes;
mod typst_world;
mod workspace;

use std::{path::PathBuf, sync::Arc};

use futures::StreamExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Global, WeakEntity, WindowBounds, WindowKind,
    WindowOptions, component::TitleBar, px, size,
};

use crate::{actions::*, settings::Settings, workspace::Workspace};

const MARKDOWN_GUIDE_URL: &str = "https://commonmark.org/help/";

/// The window's workspace, for files the system asks Malgel to open.
struct MainWindow {
    handle: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
}

impl Global for MainWindow {}

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(PathBuf::from)
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect();

    // macOS delivers "Open with" and double-clicked files as URLs, outside
    // the app's context; they are handed over through a channel.
    let (open_tx, mut open_rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
    let application = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    application.on_open_urls(move |urls| {
        let paths: Vec<PathBuf> = urls
            .iter()
            .filter_map(|url| url::Url::parse(url).ok()?.to_file_path().ok())
            .collect();
        if !paths.is_empty() {
            let _ = open_tx.unbounded_send(paths);
        }
    });

    application.run(move |cx| {
        gpui_kit::init(cx);
        match reqwest_client::ReqwestClient::user_agent(concat!(
            "Malgel/",
            env!("CARGO_PKG_VERSION")
        )) {
            Ok(client) => cx.set_http_client(Arc::new(client)),
            Err(err) => eprintln!("malgel: remote images are unavailable: {err}"),
        }

        themes::init(Settings::load(), cx);
        // Bind keys before building menus so the menus show the shortcuts.
        actions::bind_keys(cx);
        let app_menu_bar = menus::init(cx);

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_action(|_: &OpenMarkdownGuide, cx: &mut App| cx.open_url(MARKDOWN_GUIDE_URL));
        cx.on_action(|action: &SetAppearance, cx: &mut App| {
            let appearance = action.0;
            themes::AppSettings::update(cx, |settings| settings.appearance = appearance);
        });
        cx.on_action(|action: &SetTheme, cx: &mut App| themes::select_theme(&action.0, cx));
        cx.on_action(|_: &About, cx: &mut App| with_window(cx, workspace::open_about));
        cx.on_action(|_: &OpenSettings, cx: &mut App| with_window(cx, workspace::open_settings));

        // Malgel is a single-window app: closing the window ends it on
        // every platform, as the menu offers no way to open a new one.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let menu_bar = app_menu_bar.clone();
        cx.spawn(async move |cx| {
            while let Some(paths) = open_rx.next().await {
                let menu_bar = menu_bar.clone();
                cx.update(|cx| open_paths(paths, menu_bar, cx));
            }
        })
        .detach();

        open_window(paths, app_menu_bar, cx);
        cx.activate(true);
    });
}

/// Open files in the window, or in a new one if it was closed.
fn open_paths(
    paths: Vec<PathBuf>,
    app_menu_bar: gpui_kit::Entity<gpui_kit::component::menu::AppMenuBar>,
    cx: &mut App,
) {
    if let Some(main) = cx.try_global::<MainWindow>() {
        let (handle, workspace) = (main.handle, main.workspace.clone());
        let opened = handle.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_external(paths.clone(), window, cx)
            })
        });
        if matches!(opened, Ok(Ok(()))) {
            _ = handle.update(cx, |_, window, _| window.activate_window());
            return;
        }
    }
    open_window(paths, app_menu_bar, cx);
}

fn open_window(
    paths: Vec<PathBuf>,
    app_menu_bar: gpui_kit::Entity<gpui_kit::component::menu::AppMenuBar>,
    cx: &mut App,
) {
    let window_bounds = workspace::restored_bounds(cx).unwrap_or_else(|| {
        let mut window_size = size(px(1280.), px(840.));
        if let Some(display) = cx.primary_display() {
            let display_size = display.bounds().size;
            window_size.width = window_size.width.min(display_size.width * 0.9);
            window_size.height = window_size.height.min(display_size.height * 0.9);
        }
        WindowBounds::Windowed(Bounds::centered(None, window_size, cx))
    });
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        window_min_size: Some(size(px(560.), px(360.))),
        kind: WindowKind::Normal,
        app_id: Some("dev.malgel.Malgel".into()),
        #[cfg(target_os = "linux")]
        window_background: gpui_kit::WindowBackgroundAppearance::Opaque,
        #[cfg(target_os = "linux")]
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    };

    let result = gpui_kit::open_window(options, cx, move |window, cx| {
        themes::apply(Some(window), cx);
        let workspace = cx.new(|cx| Workspace::new(paths, app_menu_bar, window, cx));
        cx.set_global(MainWindow {
            handle: window.window_handle(),
            workspace: workspace.downgrade(),
        });
        workspace
    });
    if let Err(err) = result {
        eprintln!("malgel: could not open a window: {err}");
        cx.quit();
    }
}

fn with_window(cx: &mut App, f: impl FnOnce(&mut gpui_kit::Window, &mut App)) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    _ = window.update(cx, |_, window, cx| f(window, cx));
}
