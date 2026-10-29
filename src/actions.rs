//! Commands and their key bindings.
//!
//! Every command is an Action so the title bar, the menu bar, the editor's
//! context menu and the keyboard all dispatch the same thing.

use std::rc::Rc;

use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate, SharedString, actions};
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
        CheckForUpdates,
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

/// Follow a link clicked in the preview. `alternate` opens a linked
/// document the other way from the setting (a new tab or the same one).
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct FollowLink {
    pub url: SharedString,
    pub alternate: bool,
}

/// Show the Mermaid diagram with this source in its own window.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct ViewDiagram(pub SharedString);

/// Accept a word as correctly spelled from now on.
#[derive(Action, Clone, PartialEq, Deserialize)]
#[action(namespace = malgel, no_json)]
pub struct AddToDictionary(pub SharedString);

/// The key context of the workspace view.
pub const WORKSPACE_CONTEXT: &str = "Workspace";

/// A key binding. `secondary-` is Command on macOS and Control elsewhere,
/// which GPUI resolves only for bindings loaded with the app.
pub fn key<A: Action>(keys: &str, action: A, context: Option<&str>, cx: &App) -> KeyBinding {
    let context = context.map(|context| {
        Rc::new(KeyBindingContextPredicate::parse(context).expect("a valid key context"))
    });
    KeyBinding::load(keys, Box::new(action), context, false, None, cx).expect("a valid key binding")
}

pub fn bind_keys(cx: &mut App) {
    let workspace = Some(WORKSPACE_CONTEXT);
    // `secondary` is Command on macOS and Control elsewhere.
    let bindings = [
        key("secondary-n", NewFile, None, cx),
        key("secondary-o", Open, None, cx),
        key("secondary-s", Save, workspace, cx),
        key("secondary-shift-s", SaveAs, workspace, cx),
        key("secondary-shift-e", ExportHtml, workspace, cx),
        key("secondary-w", CloseDocument, workspace, cx),
        key("secondary-shift-w", CloseWindow, workspace, cx),
        key("ctrl-tab", NextTab, workspace, cx),
        key("ctrl-shift-tab", PreviousTab, workspace, cx),
        #[cfg(target_os = "macos")]
        key("cmd-shift-]", NextTab, workspace, cx),
        #[cfg(target_os = "macos")]
        key("cmd-shift-[", PreviousTab, workspace, cx),
        #[cfg(not(target_os = "macos"))]
        key("ctrl-pagedown", NextTab, workspace, cx),
        #[cfg(not(target_os = "macos"))]
        key("ctrl-pageup", PreviousTab, workspace, cx),
        key("secondary-shift-b", ToggleFileSidebar, workspace, cx),
        key("secondary-alt-o", ToggleOutline, workspace, cx),
        key("secondary-shift-f", ToggleFocusMode, workspace, cx),
        key("secondary-alt-c", CopyRichText, workspace, cx),
        key("secondary-,", OpenSettings, None, cx),
        key("secondary-q", Quit, None, cx),
        key("secondary-1", SetLayout(Layout::Editor), workspace, cx),
        key("secondary-2", SetLayout(Layout::Split), workspace, cx),
        key("secondary-3", SetLayout(Layout::Preview), workspace, cx),
        key("secondary-4", SetLayout(Layout::Live), workspace, cx),
        // The last binding for an action is the one menus display.
        key("secondary-+", ZoomIn, None, cx),
        key("secondary-=", ZoomIn, None, cx),
        key("secondary--", ZoomOut, None, cx),
        key("secondary-0", ZoomReset, None, cx),
        key("secondary-shift-l", ToggleAppearance, None, cx),
        key("secondary-shift-o", GoToHeading, workspace, cx),
        key("secondary-b", Bold, workspace, cx),
        key("secondary-i", Italic, workspace, cx),
        key("secondary-shift-x", Strikethrough, workspace, cx),
        key("secondary-e", InlineCode, workspace, cx),
        key("secondary-k", InsertLink, workspace, cx),
        key("secondary-shift-c", CodeBlock, workspace, cx),
        key("secondary-shift-.", Quote, workspace, cx),
        key("secondary-shift-8", BulletList, workspace, cx),
        key("secondary-shift-7", NumberedList, workspace, cx),
        key("secondary-shift-9", TaskList, workspace, cx),
        key("secondary-alt-1", Heading(1), workspace, cx),
        key("secondary-alt-2", Heading(2), workspace, cx),
        key("secondary-alt-3", Heading(3), workspace, cx),
        key("secondary-alt-4", Heading(4), workspace, cx),
        key("secondary-alt-5", Heading(5), workspace, cx),
        key("secondary-alt-6", Heading(6), workspace, cx),
    ];
    cx.bind_keys(bindings);
}
