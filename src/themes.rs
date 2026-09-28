//! Themes bundled with Malgel and the application-wide preferences global.

use gpui_kit::{
    App, Global, SharedString, Window,
    component::{Theme, ThemeMode, ThemeRegistry},
    px,
};

use crate::settings::{Appearance, Settings, clamp_font_size};

/// Theme sets shipped in the binary, from the GPUI Kit theme collection.
const BUNDLED_THEMES: &[&str] = &[
    include_str!("../themes/ayu.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/everforest.json"),
    include_str!("../themes/flexoki.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/macos-classic.json"),
    include_str!("../themes/solarized.json"),
    include_str!("../themes/tokyonight.json"),
];

/// The user's preferences, shared by every window.
pub struct AppSettings(pub Settings);

impl Global for AppSettings {}

impl AppSettings {
    pub fn get(cx: &App) -> &Settings {
        &cx.global::<AppSettings>().0
    }

    /// Change the settings, persist them, and re-apply the appearance.
    pub fn update(cx: &mut App, f: impl FnOnce(&mut Settings)) {
        let mut settings = Self::get(cx).clone();
        f(&mut settings);
        settings.font_size = clamp_font_size(settings.font_size);
        if &settings == Self::get(cx) {
            return;
        }
        let appearance_changed = {
            let current = Self::get(cx);
            current.appearance != settings.appearance
                || current.light_theme != settings.light_theme
                || current.dark_theme != settings.dark_theme
                || current.font_size != settings.font_size
        };
        settings.save();
        cx.set_global(AppSettings(settings));
        if appearance_changed {
            apply(None, cx);
        }
    }
}

pub fn init(settings: Settings, cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for theme_set in BUNDLED_THEMES {
        if let Err(err) = registry.load_themes_from_str(theme_set) {
            eprintln!("malgel: could not load a bundled theme: {err}");
        }
    }
    cx.set_global(AppSettings(settings));
    apply(None, cx);
}

/// Apply the preferred themes, appearance and interface size.
///
/// `window` supplies the system appearance when the preference is to follow
/// it; without one the application-wide appearance is used.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    let settings = AppSettings::get(cx).clone();
    let registry = ThemeRegistry::global(cx);
    let pick = |name: &Option<String>, mode: ThemeMode| {
        name.as_deref()
            .and_then(|name| registry.themes().get(name))
            .filter(|theme| theme.mode == mode)
            .cloned()
            .unwrap_or_else(|| registry.default_themes()[&mode].clone())
    };
    let light = pick(&settings.light_theme, ThemeMode::Light);
    let dark = pick(&settings.dark_theme, ThemeMode::Dark);

    let mode = match settings.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => window
            .as_ref()
            .map(|window| window.appearance())
            .unwrap_or_else(|| cx.window_appearance())
            .into(),
    };

    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| theme.font_size = px(settings.font_size));
}

/// Names of the registered themes for `mode`, sorted for display.
pub fn theme_names(mode: ThemeMode, cx: &App) -> Vec<SharedString> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| theme.name.clone())
        .collect()
}

/// Remember `name` as the theme for its mode and switch to that mode.
pub fn select_theme(name: &SharedString, cx: &mut App) {
    let Some(mode) = ThemeRegistry::global(cx)
        .themes()
        .get(name)
        .map(|theme| theme.mode)
    else {
        return;
    };
    // Following the system stays in effect when the theme suits the current
    // appearance; otherwise switching to the theme means switching modes.
    let follows_system =
        AppSettings::get(cx).appearance == Appearance::System && Theme::global(cx).mode == mode;
    AppSettings::update(cx, |settings| {
        if mode.is_dark() {
            settings.dark_theme = Some(name.to_string());
        } else {
            settings.light_theme = Some(name.to_string());
        }
        if !follows_system {
            settings.appearance = if mode.is_dark() {
                Appearance::Dark
            } else {
                Appearance::Light
            };
        }
    });
}
