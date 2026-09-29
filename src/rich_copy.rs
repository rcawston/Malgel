//! "Copy as rich text": Markdown on the clipboard as formatted HTML, with
//! the Markdown itself as the plain-text alternative.
//!
//! GPUI's clipboard carries text and images only, so this goes through
//! `arboard`, which writes the platform's HTML format.

use std::cell::RefCell;

thread_local! {
    /// On X11 the copied data is served by the clipboard's owner, so one
    /// clipboard lives as long as the app instead of per copy.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

/// Put `html` on the clipboard, with `plain` for apps that only take text.
pub fn copy_html(html: String, plain: &str) -> Result<(), String> {
    CLIPBOARD.with(|clipboard| {
        let mut clipboard = clipboard.borrow_mut();
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new().map_err(|err| err.to_string())?);
        }
        clipboard
            .as_mut()
            .map_or(Ok(()), |clipboard| {
                clipboard.set_html(html, Some(plain.to_string()))
            })
            .map_err(|err| err.to_string())
    })
}
