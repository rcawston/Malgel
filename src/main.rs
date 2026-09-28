//! Malgel — a fast, native Markdown editor and previewer.

// Keep release builds on Windows from opening a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod analysis;
mod document;
mod export;
mod format;
mod images;
mod math;
mod menus;
mod preview_ext;
mod settings;
mod themes;
mod workspace;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::{
    App, AppContext as _, Bounds, WindowBounds, WindowKind, WindowOptions, component::TitleBar, px,
    size,
};

use crate::{actions::*, settings::Settings, workspace::Workspace};

const MARKDOWN_GUIDE_URL: &str = "https://commonmark.org/help/";

fn main() {
    let path = std::env::args_os()
        .skip(1)
        .find(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(PathBuf::from)
        .map(|path| path.canonicalize().unwrap_or(path));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
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
            cx.on_action(|_: &About, cx: &mut App| show_about(cx));

            // Malgel is a single-window app: closing the window ends it on
            // every platform, as the menu offers no way to open a new one.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            open_window(path, app_menu_bar, cx);
            cx.activate(true);
        });
}

fn open_window(
    path: Option<PathBuf>,
    app_menu_bar: gpui_kit::Entity<gpui_kit::component::menu::AppMenuBar>,
    cx: &mut App,
) {
    let mut window_size = size(px(1280.), px(840.));
    if let Some(display) = cx.primary_display() {
        let display_size = display.bounds().size;
        window_size.width = window_size.width.min(display_size.width * 0.9);
        window_size.height = window_size.height.min(display_size.height * 0.9);
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            window_size,
            cx,
        ))),
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
        cx.new(|cx| Workspace::new(path, app_menu_bar, window, cx))
    });
    if let Err(err) = result {
        eprintln!("malgel: could not open a window: {err}");
        cx.quit();
    }
}

fn show_about(cx: &mut App) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    _ = window.update(cx, |_, window, cx| workspace::open_about(window, cx));
}
