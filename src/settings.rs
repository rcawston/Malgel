//! User preferences persisted between sessions.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const RECENT_LIMIT: usize = 10;
pub const DEFAULT_FONT_SIZE: f32 = 16.;
pub const MIN_FONT_SIZE: f32 = 12.;
pub const MAX_FONT_SIZE: f32 = 24.;

/// Light or dark appearance, or follow the system.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Which panes the workspace shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Editor,
    #[default]
    Split,
    Preview,
}

impl Layout {
    pub fn shows_editor(self) -> bool {
        self != Layout::Preview
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Theme used in light appearance; `None` is the built-in default.
    pub light_theme: Option<String>,
    /// Theme used in dark appearance; `None` is the built-in default.
    pub dark_theme: Option<String>,
    /// Base interface size in pixels; everything scales from it.
    pub font_size: f32,
    pub layout: Layout,
    pub soft_wrap: bool,
    pub line_numbers: bool,
    /// Keep the preview scrolled to the part of the document being edited.
    pub scroll_sync: bool,
    pub recent_files: Vec<PathBuf>,

    // Optional features. Off, Malgel is a single-document editor; each one
    // turned on adds to the window.
    /// Open documents in tabs instead of replacing the current one.
    pub tabs: bool,
    /// Show a folder's Markdown files beside the editor.
    pub file_sidebar: bool,
    /// Show the document's headings beside the editor.
    pub outline_sidebar: bool,
    /// Underline unknown words.
    pub spell_check: bool,
    /// Dictionary to check against, like `en_US`; `None` follows the system.
    pub spell_language: Option<String>,
    /// Reopen the documents, folder and window of the last session.
    pub restore_session: bool,
    /// Folder, next to the document, that pasted and dropped images are
    /// saved into.
    pub image_folder: String,

    /// Hide everything but the text, dimming all but the current paragraph.
    /// Lasts until turned off or Malgel quits.
    #[serde(skip)]
    pub focus_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            light_theme: None,
            dark_theme: None,
            font_size: DEFAULT_FONT_SIZE,
            layout: Layout::Split,
            soft_wrap: true,
            line_numbers: true,
            scroll_sync: true,
            recent_files: Vec::new(),
            tabs: false,
            file_sidebar: false,
            outline_sidebar: false,
            spell_check: false,
            spell_language: None,
            restore_session: true,
            image_folder: "assets".to_string(),
            focus_mode: false,
        }
    }
}

impl Settings {
    /// Load settings, falling back to defaults when the file is missing or
    /// unreadable. Out-of-range values are clamped.
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let mut settings = std::fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<Settings>(&json).ok())
            .unwrap_or_default();
        settings.font_size = clamp_font_size(settings.font_size);
        settings.recent_files.truncate(RECENT_LIMIT);
        let folder = settings.image_folder.trim().trim_matches(['/', '\\']);
        settings.image_folder = if folder.is_empty() || folder.contains("..") {
            "assets".to_string()
        } else {
            folder.to_string()
        };
        settings
    }

    /// Persist settings. Failures are not fatal: preferences simply are not
    /// remembered.
    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Move `path` to the top of the recent files list.
    pub fn push_recent(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.retain(|recent| recent != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(RECENT_LIMIT);
    }
}

pub fn clamp_font_size(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    } else {
        DEFAULT_FONT_SIZE
    }
}

fn settings_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.json"))
}

/// Where Malgel keeps its settings, session and recovered documents.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MALGEL_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Malgel"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/Malgel"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|dir| dir.join("malgel"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_use_defaults() {
        let settings: Settings = serde_json::from_str(r#"{ "layout": "preview" }"#).unwrap();
        assert_eq!(settings.layout, Layout::Preview);
        assert!(settings.soft_wrap);
        assert_eq!(settings.font_size, DEFAULT_FONT_SIZE);
        // Optional features start off; session restore starts on.
        assert!(!settings.tabs && !settings.file_sidebar && !settings.spell_check);
        assert!(settings.restore_session);
    }

    #[test]
    fn recent_files_are_unique_and_bounded() {
        let mut settings = Settings::default();
        for ix in 0..12 {
            settings.push_recent(Path::new(&format!("/tmp/{ix}.md")));
        }
        settings.push_recent(Path::new("/tmp/5.md"));
        assert_eq!(settings.recent_files.len(), RECENT_LIMIT);
        assert_eq!(settings.recent_files[0], PathBuf::from("/tmp/5.md"));
        assert_eq!(
            settings
                .recent_files
                .iter()
                .filter(|path| path.ends_with("5.md"))
                .count(),
            1
        );
    }

    #[test]
    fn font_size_is_clamped() {
        assert_eq!(clamp_font_size(100.), MAX_FONT_SIZE);
        assert_eq!(clamp_font_size(f32::NAN), DEFAULT_FONT_SIZE);
    }
}
