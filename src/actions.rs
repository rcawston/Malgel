//! Commands and their key bindings.
//!
//! Every command is an Action so the title bar, the menu bar, the editor's
//! context menu and the keyboard all dispatch the same thing.

use gpui_kit::{Action, App, KeyBinding, SharedString, actions};
use serde::Deserialize;

use crate::settings::{Appearance, Layout};

actions!(
    malgel,
    [
        NewFile,
        Open,
        OpenFolder,
        Save,
        SaveAs,
        ExportHtml,
        ExportPdf,
        ExportDocx,
        RevealInFolder,
        CloseDocument,
        CloseWindow,
        NextTab,
        PreviousTab,
        Quit,
        About,
        OpenMarkdownGuide,
        ToggleScrollSync,
        ToggleSoftWrap,
        ToggleLineNumbers,
        ToggleAppearance,
        ToggleTabs,
        ToggleFileSidebar,
        ToggleOutline,
        ToggleSpellCheck,
        ToggleFocusMode,
        ToggleRestoreSession,
        OpenSettings,
        CopyRichText,
        TidyTable,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        GoToHeading,
        Bold,
        Italic,
        Strikethrough,
        InlineCode,
        InsertLink,
        CodeBlock,
        Quote,
        BulletList,
        NumberedList,
        TaskList,
    ]
);

/// Show the editor, the preview, or both.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct SetLayout(pub Layout);

/// Turn the current lines into a heading of the given level.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct Heading(pub u8);

/// Follow the system appearance, or force light or dark.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct SetAppearance(pub Appearance);

/// Use the named theme for its appearance (light or dark).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct SetTheme(pub SharedString);

/// Open an entry of the recent files list.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct OpenRecent(pub usize);

/// Show the open document at this index.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct ActivateTab(pub usize);

/// Check spelling against the named dictionary, like `en_US`.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct SetSpellLanguage(pub SharedString);

/// Replace the misspelled `word` at `start..end` with `with`.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct ReplaceMisspelling {
    pub start: usize,
    pub end: usize,
    pub word: SharedString,
    pub with: SharedString,
}

/// Accept a word as correctly spelled from now on.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct AddToDictionary(pub SharedString);

/// The key context of the workspace view.
pub const WORKSPACE_CONTEXT: &str = "Workspace";

pub fn bind_keys(cx: &mut App) {
    let workspace = Some(WORKSPACE_CONTEXT);
    // `secondary` is Command on macOS and Control elsewhere.
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewFile, None),
        KeyBinding::new("secondary-o", Open, None),
        KeyBinding::new("secondary-s", Save, workspace),
        KeyBinding::new("secondary-shift-s", SaveAs, workspace),
        KeyBinding::new("secondary-shift-e", ExportHtml, workspace),
        KeyBinding::new("secondary-w", CloseDocument, workspace),
        KeyBinding::new("secondary-shift-w", CloseWindow, workspace),
        KeyBinding::new("ctrl-tab", NextTab, workspace),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, workspace),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-]", NextTab, workspace),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-[", PreviousTab, workspace),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-pagedown", NextTab, workspace),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-pageup", PreviousTab, workspace),
        KeyBinding::new("secondary-shift-b", ToggleFileSidebar, workspace),
        KeyBinding::new("secondary-alt-o", ToggleOutline, workspace),
        KeyBinding::new("secondary-shift-f", ToggleFocusMode, workspace),
        KeyBinding::new("secondary-alt-c", CopyRichText, workspace),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-1", SetLayout(Layout::Editor), workspace),
        KeyBinding::new("secondary-2", SetLayout(Layout::Split), workspace),
        KeyBinding::new("secondary-3", SetLayout(Layout::Preview), workspace),
        // The last binding for an action is the one menus display.
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ZoomReset, None),
        KeyBinding::new("secondary-shift-l", ToggleAppearance, None),
        KeyBinding::new("secondary-shift-o", GoToHeading, workspace),
        KeyBinding::new("secondary-b", Bold, workspace),
        KeyBinding::new("secondary-i", Italic, workspace),
        KeyBinding::new("secondary-shift-x", Strikethrough, workspace),
        KeyBinding::new("secondary-e", InlineCode, workspace),
        KeyBinding::new("secondary-k", InsertLink, workspace),
        KeyBinding::new("secondary-shift-c", CodeBlock, workspace),
        KeyBinding::new("secondary-shift-.", Quote, workspace),
        KeyBinding::new("secondary-shift-8", BulletList, workspace),
        KeyBinding::new("secondary-shift-7", NumberedList, workspace),
        KeyBinding::new("secondary-shift-9", TaskList, workspace),
        KeyBinding::new("secondary-alt-1", Heading(1), workspace),
        KeyBinding::new("secondary-alt-2", Heading(2), workspace),
        KeyBinding::new("secondary-alt-3", Heading(3), workspace),
        KeyBinding::new("secondary-alt-4", Heading(4), workspace),
        KeyBinding::new("secondary-alt-5", Heading(5), workspace),
        KeyBinding::new("secondary-alt-6", Heading(6), workspace),
    ]);
}
