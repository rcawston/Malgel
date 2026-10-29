//! Where a link in the preview leads.
//!
//! Links to headings scroll the document, links to Markdown files open them
//! in Malgel, and anything else (web pages, email addresses, other files)
//! is handed to the system once the user agrees.

use std::path::{Path, PathBuf};

use gpui_kit::{App, ClickEvent, MouseButton, SharedString, Window};

use crate::{actions::FollowLink, analysis::Heading};

/// What a link's URL refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A heading of this document, by its anchor (`#getting-started`).
    Heading(String),
    /// A local file or folder, with the heading to show if it's Markdown.
    File {
        path: PathBuf,
        anchor: Option<String>,
    },
    /// A file relative to a document that hasn't been saved anywhere yet.
    NeedsSavedDocument,
    /// A web page, email address or anything else for the system to open.
    External(String),
}

/// Work out where `url` leads from a document in `base_dir`.
pub fn classify(url: &str, base_dir: Option<&Path>) -> Option<LinkTarget> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if let Some(anchor) = url.strip_prefix('#') {
        return Some(LinkTarget::Heading(percent_decode(anchor)));
    }
    if has_scheme(url) {
        if url
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("file:"))
        {
            let parsed = url::Url::parse(url).ok()?;
            let anchor = parsed
                .fragment()
                .filter(|anchor| !anchor.is_empty())
                .map(percent_decode);
            let path = parsed.to_file_path().ok()?;
            return Some(LinkTarget::File { path, anchor });
        }
        return Some(LinkTarget::External(url.to_string()));
    }

    let (rest, anchor) = match url.split_once('#') {
        Some((rest, anchor)) => (rest, (!anchor.is_empty()).then(|| percent_decode(anchor))),
        None => (url, None),
    };
    let rest = rest.split_once('?').map_or(rest, |(path, _)| path);
    if rest.is_empty() {
        return anchor.map(LinkTarget::Heading);
    }
    let path = PathBuf::from(percent_decode(rest));
    let path = if path.is_absolute() {
        path
    } else {
        match base_dir {
            Some(dir) => dir.join(path),
            None => return Some(LinkTarget::NeedsSavedDocument),
        }
    };
    Some(LinkTarget::File {
        path: normalize(&path),
        anchor,
    })
}

/// Whether `url` starts with a URL scheme like `https:` or `mailto:`. A
/// single letter is a Windows drive (`C:\notes.md`), not a scheme.
fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    scheme.len() > 1
        && scheme.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// Decode `%20` and friends; anything malformed is left as it is.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'%'
            && let Some(byte) = text
                .get(ix + 1..ix + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            ix += 3;
            continue;
        }
        out.push(bytes[ix]);
        ix += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// Resolve `.` and `..` without touching the disk, so a link to a file that
/// doesn't exist still names it sensibly.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The anchor GitHub gives a heading: lowercase, punctuation dropped,
/// spaces as hyphens.
pub fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|ch| match ch {
            ' ' => Some('-'),
            '-' | '_' => Some(ch),
            _ if ch.is_alphanumeric() => Some(ch),
            _ => None,
        })
        .collect()
}

/// The outline index of the heading `anchor` names. Repeated headings are
/// told apart as GitHub does: `#notes`, `#notes-1`, `#notes-2`.
pub fn find_heading(outline: &[Heading], anchor: &str) -> Option<usize> {
    let wanted = anchor.trim().to_lowercase();
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut loose = None;
    for (ix, heading) in outline.iter().enumerate() {
        let base = slug(&heading.text);
        let count = seen.entry(base.clone()).or_default();
        let anchor = if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        if anchor == wanted {
            return Some(ix);
        }
        // Accept the heading's text itself too, as some editors link that.
        if loose.is_none() && heading.text.trim().to_lowercase() == wanted {
            loose = Some(ix);
        }
    }
    loose
}

thread_local! {
    static LINK_CLICKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a link was clicked since this was last asked, so Live Preview
/// follows it instead of editing the block it's in.
pub fn take_link_clicked() -> bool {
    LINK_CLICKED.with(|clicked| clicked.replace(false))
}

/// The preview's link handler: follow the link in the workspace. A middle
/// click, or a click with Command (macOS) or Control held, opens a linked
/// document the other way from the setting (a new tab or the same one).
pub fn on_click(url: &SharedString, event: &ClickEvent, window: &mut Window, cx: &mut App) {
    let alternate = match event {
        ClickEvent::Mouse(click) => match click.up.button {
            MouseButton::Left => click.up.modifiers.secondary(),
            MouseButton::Middle => true,
            _ => return,
        },
        ClickEvent::Keyboard(_) => false,
        ClickEvent::Touch(click) => {
            if click.long_press {
                return;
            }
            false
        }
    };
    LINK_CLICKED.with(|clicked| clicked.set(true));
    window.dispatch_action(
        Box::new(FollowLink {
            url: url.clone(),
            alternate,
        }),
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heading(text: &str) -> Heading {
        Heading {
            level: 2,
            text: text.to_string(),
            line: 0,
        }
    }

    #[test]
    fn tells_web_links_from_files() {
        let dir = Path::new("/notes");
        assert_eq!(
            classify("https://www.malgel.com", Some(dir)),
            Some(LinkTarget::External("https://www.malgel.com".into()))
        );
        assert_eq!(
            classify("mailto:hi@example.com", Some(dir)),
            Some(LinkTarget::External("mailto:hi@example.com".into()))
        );
        assert_eq!(
            classify("guide.md", Some(dir)),
            Some(LinkTarget::File {
                path: PathBuf::from("/notes/guide.md"),
                anchor: None,
            })
        );
        assert_eq!(
            classify("../docs/Read%20me.md#set-up", Some(dir)),
            Some(LinkTarget::File {
                path: PathBuf::from("/docs/Read me.md"),
                anchor: Some("set-up".into()),
            })
        );
        assert_eq!(
            classify("./a.md?raw=1", Some(dir)),
            Some(LinkTarget::File {
                path: PathBuf::from("/notes/a.md"),
                anchor: None,
            })
        );
        assert_eq!(
            classify("#getting-started", None),
            Some(LinkTarget::Heading("getting-started".into()))
        );
        assert_eq!(
            classify("guide.md", None),
            Some(LinkTarget::NeedsSavedDocument)
        );
        assert_eq!(classify("  ", Some(dir)), None);
    }

    #[test]
    fn opens_file_urls_as_files() {
        #[cfg(not(windows))]
        assert_eq!(
            classify("file:///notes/a%20b.md#top", None),
            Some(LinkTarget::File {
                path: PathBuf::from("/notes/a b.md"),
                anchor: Some("top".into()),
            })
        );
        #[cfg(windows)]
        assert_eq!(
            classify("file:///C:/notes/a.md", None),
            Some(LinkTarget::File {
                path: PathBuf::from(r"C:\notes\a.md"),
                anchor: None,
            })
        );
    }

    #[test]
    fn a_drive_letter_is_not_a_scheme() {
        assert!(!has_scheme(r"C:\notes\a.md"));
        assert!(has_scheme("https://example.com"));
        assert!(has_scheme("x-devonthink-item://1234"));
        assert!(!has_scheme("notes/a:b.md"));
    }

    #[test]
    fn slugs_headings_like_github() {
        assert_eq!(slug("Getting Started"), "getting-started");
        assert_eq!(slug("What’s new in 0.2?"), "whats-new-in-02");
        assert_eq!(slug("C++ & Rust_notes"), "c--rust_notes");
        assert_eq!(slug("Über Größe"), "über-größe");
    }

    #[test]
    fn finds_repeated_headings() {
        let outline = [
            heading("Intro"),
            heading("Notes"),
            heading("Usage"),
            heading("Notes"),
        ];
        assert_eq!(find_heading(&outline, "intro"), Some(0));
        assert_eq!(find_heading(&outline, "notes"), Some(1));
        assert_eq!(find_heading(&outline, "notes-1"), Some(3));
        assert_eq!(find_heading(&outline, "Usage"), Some(2));
        assert_eq!(find_heading(&outline, "missing"), None);
    }
}
