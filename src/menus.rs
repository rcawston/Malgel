//! The application menus.
//!
//! macOS shows them in the system menu bar; Linux and Windows show the same
//! menus in the window's title bar through `AppMenuBar`. Check marks follow
//! the settings and theme, so the menus are rebuilt whenever either changes.

use gpui_kit::{
    App, Entity, Menu, MenuItem, SharedString,
    component::{
        GlobalState, Theme, ThemeMode,
        input::{Copy, Cut, Paste, Redo, Replace, Search, SelectAll, Undo},
        menu::AppMenuBar,
    },
};

use crate::{
    actions::*,
    document::display_name,
    settings::{Appearance, Layout},
    themes::{AppSettings, theme_names},
};

pub fn init(cx: &mut App) -> Entity<AppMenuBar> {
    let app_menu_bar = AppMenuBar::new(cx);
    update(&app_menu_bar, cx);

    cx.observe_global::<AppSettings>({
        let app_menu_bar = app_menu_bar.clone();
        move |cx| update(&app_menu_bar, cx)
    })
    .detach();
    cx.observe_global::<Theme>({
        let app_menu_bar = app_menu_bar.clone();
        move |cx| update(&app_menu_bar, cx)
    })
    .detach();

    app_menu_bar
}

fn update(app_menu_bar: &Entity<AppMenuBar>, cx: &mut App) {
    cx.set_menus(build(cx));
    let menus = build(cx).into_iter().map(|menu| menu.owned()).collect();
    GlobalState::global_mut(cx).set_app_menus(menus);
    app_menu_bar.update(cx, |menu_bar, cx| menu_bar.reload(cx));
}

fn submenu(name: &str, items: Vec<MenuItem>) -> MenuItem {
    MenuItem::Submenu(Menu {
        name: SharedString::from(name.to_string()),
        items,
        disabled: false,
    })
}

fn menu(name: &str, items: Vec<MenuItem>) -> Menu {
    Menu {
        name: SharedString::from(name.to_string()),
        items,
        disabled: false,
    }
}

pub fn build(cx: &App) -> Vec<Menu> {
    let settings = AppSettings::get(cx);
    let macos = cfg!(target_os = "macos");

    let recent: Vec<MenuItem> = if settings.recent_files.is_empty() {
        vec![MenuItem::action("No recent files", OpenRecent(0)).disabled(true)]
    } else {
        settings
            .recent_files
            .iter()
            .enumerate()
            .map(|(ix, path)| {
                let name = display_name(Some(path));
                let folder = path
                    .parent()
                    .and_then(|dir| dir.file_name())
                    .map(|dir| format!("  —  {}", dir.to_string_lossy()))
                    .unwrap_or_default();
                MenuItem::action(format!("{name}{folder}"), OpenRecent(ix))
            })
            .collect()
    };

    let mut file = vec![
        MenuItem::action("New", NewFile),
        MenuItem::action("Open…", Open),
        submenu("Open recent", recent),
        MenuItem::separator(),
        MenuItem::action("Save", Save),
        MenuItem::action("Save as…", SaveAs),
        MenuItem::action("Export as HTML…", ExportHtml),
        MenuItem::separator(),
        MenuItem::action(
            if macos {
                "Reveal in Finder"
            } else {
                "Show in folder"
            },
            RevealInFolder,
        ),
        MenuItem::separator(),
        MenuItem::action("Close window", CloseWindow),
    ];
    if !macos {
        file.push(MenuItem::action("Quit", Quit));
    }

    let edit = vec![
        MenuItem::action("Undo", Undo),
        MenuItem::action("Redo", Redo),
        MenuItem::separator(),
        MenuItem::action("Cut", Cut),
        MenuItem::action("Copy", Copy),
        MenuItem::action("Paste", Paste),
        MenuItem::action("Select all", SelectAll),
        MenuItem::separator(),
        MenuItem::action("Find…", Search),
        MenuItem::action("Replace…", Replace),
    ];

    let format = vec![
        MenuItem::action("Bold", Bold),
        MenuItem::action("Italic", Italic),
        MenuItem::action("Strikethrough", Strikethrough),
        MenuItem::action("Inline code", InlineCode),
        MenuItem::action("Link", InsertLink),
        MenuItem::separator(),
        submenu(
            "Heading",
            (1..=6)
                .map(|level| MenuItem::action(format!("Heading {level}"), Heading(level)))
                .collect(),
        ),
        MenuItem::action("Quote", Quote),
        MenuItem::action("Bulleted list", BulletList),
        MenuItem::action("Numbered list", NumberedList),
        MenuItem::action("Task list", TaskList),
        MenuItem::action("Code block", CodeBlock),
    ];

    let appearance = settings.appearance;
    let theme = Theme::global(cx);
    let theme_items = |mode: ThemeMode| {
        let selected = if mode.is_dark() {
            &theme.dark_theme.name
        } else {
            &theme.light_theme.name
        };
        theme_names(mode, cx)
            .into_iter()
            .map(|name| {
                let checked = &name == selected;
                MenuItem::action(name.clone(), SetTheme(name)).checked(checked)
            })
            .collect::<Vec<_>>()
    };

    let view = vec![
        MenuItem::action("Editor", SetLayout(Layout::Editor))
            .checked(settings.layout == Layout::Editor),
        MenuItem::action("Editor and preview", SetLayout(Layout::Split))
            .checked(settings.layout == Layout::Split),
        MenuItem::action("Preview", SetLayout(Layout::Preview))
            .checked(settings.layout == Layout::Preview),
        MenuItem::separator(),
        MenuItem::action("Go to heading…", GoToHeading),
        MenuItem::separator(),
        MenuItem::action("Sync scrolling", ToggleScrollSync).checked(settings.scroll_sync),
        MenuItem::action("Wrap lines", ToggleSoftWrap).checked(settings.soft_wrap),
        MenuItem::action("Line numbers", ToggleLineNumbers).checked(settings.line_numbers),
        MenuItem::separator(),
        MenuItem::action("Zoom in", ZoomIn),
        MenuItem::action("Zoom out", ZoomOut),
        MenuItem::action("Actual size", ZoomReset),
        MenuItem::separator(),
        submenu(
            "Appearance",
            vec![
                MenuItem::action("System", SetAppearance(Appearance::System))
                    .checked(appearance == Appearance::System),
                MenuItem::action("Light", SetAppearance(Appearance::Light))
                    .checked(appearance == Appearance::Light),
                MenuItem::action("Dark", SetAppearance(Appearance::Dark))
                    .checked(appearance == Appearance::Dark),
            ],
        ),
        submenu("Light theme", theme_items(ThemeMode::Light)),
        submenu("Dark theme", theme_items(ThemeMode::Dark)),
    ];

    let mut help = vec![MenuItem::action("Markdown guide", OpenMarkdownGuide)];
    if !macos {
        help.push(MenuItem::separator());
        help.push(MenuItem::action("About Malgel", About));
    }

    let mut menus = Vec::new();
    if macos {
        menus.push(menu(
            "Malgel",
            vec![
                MenuItem::action("About Malgel", About),
                MenuItem::separator(),
                MenuItem::action("Quit Malgel", Quit),
            ],
        ));
    }
    menus.extend([
        menu("File", file),
        menu("Edit", edit),
        menu("Format", format),
        menu("View", view),
        menu("Help", help),
    ]);
    menus
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[gpui_kit::test]
    fn menus_reflect_settings(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(AppSettings(Settings {
                layout: Layout::Preview,
                soft_wrap: false,
                ..Settings::default()
            }));
        });
        cx.read(|cx| {
            let menus = build(cx);
            let view = menus.iter().find(|menu| menu.name == "View").unwrap();
            let checked: Vec<_> = view
                .items
                .iter()
                .filter(|item| item.is_checked())
                .map(|item| match item {
                    MenuItem::Action { name, .. } => name.to_string(),
                    _ => String::new(),
                })
                .collect();
            assert_eq!(checked, ["Preview", "Sync scrolling", "Line numbers"]);
        });
    }
}
