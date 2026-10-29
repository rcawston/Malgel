//! Checking GitHub for a newer release of Malgel.
//!
//! The check asks GitHub's API for the latest published release (drafts and
//! pre-releases aren't) and compares its version with this one. Nothing is
//! downloaded: a newer release is offered with a link to its page, where the
//! download for each system is. On startup the check is quiet unless there
//! is something to offer; checking from the menu always answers.

use std::{sync::Arc, time::Duration};

use futures::AsyncReadExt as _;
use gpui_kit::{
    App, ParentElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        h_flex,
        notification::Notification,
        v_flex,
    },
    div,
    http_client::{AsyncBody, HttpClient, Request, Result, anyhow},
};
use serde::Deserialize;

use crate::themes::AppSettings;

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/rcawston/Malgel/releases/latest";

/// How long after startup to check, leaving the window to settle (and any
/// recovered changes to be offered) first.
const STARTUP_DELAY: Duration = Duration::from_secs(3);

/// How long to wait for another dialog to close before offering a release
/// found on startup, and how often to look.
const DIALOG_WAIT_TRIES: u32 = 60;
const DIALOG_WAIT_STEP: Duration = Duration::from_secs(2);

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A published release of Malgel.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// Like `0.1.3`.
    pub version: String,
    /// The release's page on GitHub.
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// A version like `0.1.3` (or tag like `v0.1.3`) as numbers to compare;
/// missing parts count as 0, and a pre-release or build suffix is ignored.
pub fn parse_version(text: &str) -> Option<[u64; 3]> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let core = text.split(['-', '+']).next()?;
    let mut version = [0; 3];
    let mut parts = core.split('.');
    for (ix, part) in parts.by_ref().take(3).enumerate() {
        version[ix] = part.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

/// Whether `latest` is a later version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn parse_release(body: &[u8]) -> Result<Release> {
    let release: GithubRelease = serde_json::from_slice(body)?;
    let version = release.tag_name.trim_start_matches(['v', 'V']).to_string();
    if parse_version(&version).is_none() {
        return Err(anyhow!("unexpected release tag {}", release.tag_name));
    }
    Ok(Release {
        version,
        url: release.html_url,
    })
}

async fn latest_release(http: Arc<dyn HttpClient>) -> Result<Release> {
    let request = Request::builder()
        .uri(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .body(AsyncBody::empty())?;
    let mut response = http.send(request).await?;
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    if !response.status().is_success() {
        return Err(anyhow!("GitHub answered {}", response.status()));
    }
    parse_release(&body)
}

/// Check on startup, when the setting allows.
pub fn check_on_startup(cx: &mut App) {
    if AppSettings::get(cx).check_for_updates {
        check(false, cx);
    }
}

/// Check for a newer release. `manual` (from the menu) reports what was
/// found either way, and offers even a release skipped before.
pub fn check(manual: bool, cx: &mut App) {
    let http = cx.http_client();
    cx.spawn(async move |cx| {
        if !manual {
            cx.background_executor().timer(STARTUP_DELAY).await;
        }
        let result = latest_release(http).await;
        let release = match result {
            Ok(release) if is_newer(&release.version, CURRENT_VERSION) => release,
            Ok(_) => {
                if manual {
                    cx.update(|cx| {
                        notify(
                            Notification::success(tr!(
                                "update.up_to_date",
                                version = CURRENT_VERSION
                            )),
                            cx,
                        )
                    });
                }
                return;
            }
            Err(err) => {
                if manual {
                    cx.update(|cx| {
                        notify(Notification::error(tr!("update.failed", error = err)), cx)
                    });
                }
                return;
            }
        };
        let skipped = cx.update(|cx| {
            AppSettings::get(cx).skipped_version.as_deref() == Some(release.version.as_str())
        });
        if skipped && !manual {
            return;
        }
        // Wait for any dialog open on startup to be answered first.
        for _ in 0..DIALOG_WAIT_TRIES {
            let busy = cx.update(|cx| {
                let mut busy = false;
                crate::with_window(cx, |window, cx| busy = window.has_active_dialog(cx));
                busy
            });
            if !busy {
                break;
            }
            cx.background_executor().timer(DIALOG_WAIT_STEP).await;
        }
        cx.update(|cx| crate::with_window(cx, |window, cx| offer(release, window, cx)));
    })
    .detach();
}

fn notify(notification: Notification, cx: &mut App) {
    crate::with_window(cx, |window, cx| window.push_notification(notification, cx));
}

/// Offer `release`: open its page, ask again next time, or skip it.
fn offer(release: Release, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let Release { version, url } = release.clone();
        let skip = version.clone();
        alert
            .title(tr!("update.available_title", version = version))
            .description(
                v_flex()
                    .gap_2()
                    .child(tr!(
                        "update.available_description",
                        current = CURRENT_VERSION
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("update.settings_hint")),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .justify_between()
                    .child(
                        Button::new("skip")
                            .label(tr!("update.skip"))
                            .ghost()
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let skip = skip.clone();
                                AppSettings::update(cx, |settings| {
                                    settings.skipped_version = Some(skip)
                                });
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("later")
                                    .label(tr!("update.later"))
                                    .outline()
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("download")
                                    .label(tr!("update.download"))
                                    .primary()
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        cx.open_url(&url);
                                    }),
                            ),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(parse_version("v0.1.3"), Some([0, 1, 3]));
        assert_eq!(parse_version("1.2"), Some([1, 2, 0]));
        assert_eq!(parse_version("0.2.0-beta.1"), Some([0, 2, 0]));
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.3", "0.1.3"));
        assert!(!is_newer("0.1.2", "0.1.3"));
        assert!(!is_newer("nonsense", "0.1.3"));
    }

    #[test]
    fn reads_github_releases() {
        let body = br#"{"tag_name": "v0.2.0", "html_url": "https://github.com/rcawston/Malgel/releases/tag/v0.2.0", "draft": false}"#;
        assert_eq!(
            parse_release(body).unwrap(),
            Release {
                version: "0.2.0".into(),
                url: "https://github.com/rcawston/Malgel/releases/tag/v0.2.0".into(),
            }
        );
        assert!(parse_release(br#"{"tag_name": "nightly", "html_url": ""}"#).is_err());
        assert!(parse_release(br#"{"message": "Not Found"}"#).is_err());
    }
}
