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
    spell,
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

/// The most entries (separators included) a menu shows in full; GPUI Kit
/// turns longer ones into short scrolling lists.
#[cfg(test)]
const MAX_MENU_ITEMS: usize = 20;

fn submenu(name: impl Into<SharedString>, items: Vec<MenuItem>) -> MenuItem {
    MenuItem::Submenu(Menu {
        name: name.into(),
        items,
        disabled: false,
    })
}

fn menu(name: impl Into<SharedString>, items: Vec<MenuItem>) -> Menu {
    Menu {
        name: name.into(),
        items,
        disabled: false,
    }
}

pub fn build(cx: &App) -> Vec<Menu> {
    let settings = AppSettings::get(cx);
    let macos = cfg!(target_os = "macos");

    let recent: Vec<MenuItem> = if settings.recent_files.is_empty() {
        vec![MenuItem::action(tr!("menu.no_recent"), OpenRecent(0)).disabled(true)]
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
        MenuItem::action(tr!("menu.new"), NewFile),
        MenuItem::action(tr!("menu.open"), Open),
        MenuItem::action(tr!("menu.open_folder"), OpenFolder),
        submenu(tr!("menu.open_recent"), recent),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.save"), Save),
        MenuItem::action(tr!("menu.save_as"), SaveAs),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.export_html"), ExportHtml),
        MenuItem::action(tr!("menu.export_pdf"), ExportPdf),
        MenuItem::action(tr!("menu.export_docx"), ExportDocx),
        MenuItem::separator(),
        MenuItem::action(
            if macos {
                tr!("menu.reveal_finder")
            } else {
                tr!("menu.show_in_folder")
            },
            RevealInFolder,
        ),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.close"), CloseDocument),
        MenuItem::action(tr!("menu.close_window"), CloseWindow),
    ];
    if !macos {
        file.push(MenuItem::action(tr!("menu.quit"), Quit));
    }

    let mut edit = vec![
        MenuItem::action(tr!("menu.undo"), Undo),
        MenuItem::action(tr!("menu.redo"), Redo),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.cut"), Cut),
        MenuItem::action(tr!("menu.copy"), Copy),
        MenuItem::action(tr!("menu.copy_rich"), CopyRichText),
        MenuItem::action(tr!("menu.paste"), Paste),
        MenuItem::action(tr!("menu.select_all"), SelectAll),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.find"), Search),
        MenuItem::action(tr!("menu.replace"), Replace),
    ];
    if !macos {
        edit.push(MenuItem::separator());
        edit.push(MenuItem::action(tr!("menu.settings"), OpenSettings));
    }

    let format = vec![
        MenuItem::action(tr!("menu.bold"), Bold),
        MenuItem::action(tr!("menu.italic"), Italic),
        MenuItem::action(tr!("menu.strikethrough"), Strikethrough),
        MenuItem::action(tr!("menu.inline_code"), InlineCode),
        MenuItem::action(tr!("menu.link"), InsertLink),
        MenuItem::separator(),
        submenu(
            tr!("menu.heading"),
            (1..=6)
                .map(|level| {
                    MenuItem::action(tr!("menu.heading_level", level = level), Heading(level))
                })
                .collect(),
        ),
        MenuItem::action(tr!("menu.quote"), Quote),
        MenuItem::action(tr!("menu.bulleted_list"), BulletList),
        MenuItem::action(tr!("menu.numbered_list"), NumberedList),
        MenuItem::action(tr!("menu.task_list"), TaskList),
        MenuItem::action(tr!("menu.code_block"), CodeBlock),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.tidy_table"), TidyTable),
    ];

    let current_language = settings
        .spell_language
        .clone()
        .unwrap_or_else(spell::default_language);
    let mut spelling = vec![
        MenuItem::action(tr!("menu.check_spelling"), ToggleSpellCheck)
            .checked(settings.spell_check),
        MenuItem::separator(),
    ];
    spelling.extend(spell::available_languages().into_iter().map(|language| {
        let checked = settings.spell_check && language == current_language;
        MenuItem::action(
            language.replace('_', "-"),
            SetSpellLanguage(language.into()),
        )
        .checked(checked)
    }));

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
        MenuItem::action(tr!("menu.editor"), SetLayout(Layout::Editor))
            .checked(settings.layout == Layout::Editor),
        MenuItem::action(tr!("menu.editor_and_preview"), SetLayout(Layout::Split))
            .checked(settings.layout == Layout::Split),
        MenuItem::action(tr!("menu.preview"), SetLayout(Layout::Preview))
            .checked(settings.layout == Layout::Preview),
        MenuItem::action(tr!("menu.live_preview"), SetLayout(Layout::Live))
            .checked(settings.layout == Layout::Live),
        MenuItem::action(tr!("menu.focus_mode"), ToggleFocusMode).checked(settings.focus_mode),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.tabs"), ToggleTabs).checked(settings.tabs),
        MenuItem::action(tr!("menu.file_sidebar"), ToggleFileSidebar)
            .checked(settings.file_sidebar),
        MenuItem::action(tr!("menu.outline"), ToggleOutline).checked(settings.outline_sidebar),
        MenuItem::action(tr!("menu.go_to_heading"), GoToHeading),
        MenuItem::separator(),
        MenuItem::action(tr!("menu.sync_scrolling"), ToggleScrollSync)
            .checked(settings.scroll_sync),
        MenuItem::action(tr!("menu.wrap_lines"), ToggleSoftWrap).checked(settings.soft_wrap),
        MenuItem::action(tr!("menu.line_numbers"), ToggleLineNumbers)
            .checked(settings.line_numbers),
        submenu(tr!("menu.spelling"), spelling),
        MenuItem::separator(),
        submenu(
            tr!("menu.zoom"),
            vec![
                MenuItem::action(tr!("menu.zoom_in"), ZoomIn),
                MenuItem::action(tr!("menu.zoom_out"), ZoomOut),
                MenuItem::action(tr!("menu.actual_size"), ZoomReset),
            ],
        ),
        // Themes live in the Appearance submenu: menus with more than
        // MAX_MENU_ITEMS entries turn into small scrolling lists.
        submenu(
            tr!("menu.appearance"),
            vec![
                MenuItem::action(tr!("menu.system"), SetAppearance(Appearance::System))
                    .checked(appearance == Appearance::System),
                MenuItem::action(tr!("menu.light"), SetAppearance(Appearance::Light))
                    .checked(appearance == Appearance::Light),
                MenuItem::action(tr!("menu.dark"), SetAppearance(Appearance::Dark))
                    .checked(appearance == Appearance::Dark),
                MenuItem::separator(),
                submenu(tr!("menu.light_theme"), theme_items(ThemeMode::Light)),
                submenu(tr!("menu.dark_theme"), theme_items(ThemeMode::Dark)),
            ],
        ),
    ];

    let mut help = vec![MenuItem::action(
        tr!("menu.markdown_guide"),
        OpenMarkdownGuide,
    )];
    if !macos {
        help.push(MenuItem::separator());
        help.push(MenuItem::action(tr!("menu.about"), About));
    }

    let mut menus = Vec::new();
    if macos {
        menus.push(menu(
            "Malgel",
            vec![
                MenuItem::action(tr!("menu.about"), About),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.settings"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.quit_malgel"), Quit),
            ],
        ));
    }
    menus.extend([
        menu(tr!("menu.file"), file),
        menu(tr!("menu.edit"), edit),
        menu(tr!("menu.format"), format),
        menu(tr!("menu.view"), view),
        menu(tr!("menu.help"), help),
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

    #[gpui_kit::test]
    fn no_menu_is_long_enough_to_scroll(cx: &mut gpui_kit::TestAppContext) {
        fn check(name: &str, items: &[MenuItem]) {
            assert!(
                items.len() <= MAX_MENU_ITEMS,
                "{name} has {} items; menus with more than {MAX_MENU_ITEMS} scroll",
                items.len()
            );
            for item in items {
                if let MenuItem::Submenu(submenu) = item {
                    check(&submenu.name, &submenu.items);
                }
            }
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::themes::init(Settings::default(), cx);
            cx.set_global(AppSettings(Settings::default()));
        });
        cx.read(|cx| {
            for menu in build(cx) {
                check(&menu.name, &menu.items);
            }
        });
    }
}
