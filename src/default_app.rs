//! Making Malgel the app that opens Markdown files.
//!
//! macOS and Linux let an app claim a file type itself. Windows doesn't (since
//! Windows 8 only the user can change a default), so there Malgel registers
//! itself for Markdown and opens Settings at its Default apps page.

use gpui_kit::{App, Global};

/// The file types Malgel opens, as MIME types.
#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
const MIME_TYPES: [&str; 2] = ["text/markdown", "text/x-markdown"];

/// Whether Malgel opens Markdown files, as of the last check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Default,
    NotDefault,
    /// This system has no way to tell.
    Unknown,
}

/// What asking to become the default did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Malgel now opens Markdown files.
    #[cfg_attr(windows, allow(dead_code))]
    Done,
    /// The system's settings are open for the user to choose Malgel.
    #[cfg_attr(not(windows), allow(dead_code))]
    SettingsOpened,
}

/// The last known status, so rendering the settings dialog doesn't ask
/// the system every frame.
struct KnownStatus(Status);

impl Global for KnownStatus {}

/// The status as last checked, checking now if it never was.
pub fn status(cx: &mut App) -> Status {
    if let Some(known) = cx.try_global::<KnownStatus>() {
        return known.0;
    }
    refresh(cx)
}

/// Ask the system again whether Malgel is the default.
pub fn refresh(cx: &mut App) -> Status {
    let status = match platform::is_default() {
        Some(true) => Status::Default,
        Some(false) => Status::NotDefault,
        None => Status::Unknown,
    };
    cx.set_global(KnownStatus(status));
    status
}

/// Make Malgel the default for Markdown files, or as close as the system
/// allows.
pub fn make_default(cx: &mut App) -> Result<Outcome, String> {
    let outcome = platform::make_default(cx)?;
    refresh(cx);
    Ok(outcome)
}

/// Whether becoming the default is up to the user, in the system's settings.
pub const CHOSEN_IN_SYSTEM_SETTINGS: bool = cfg!(windows);

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::{
        base::TCFType,
        string::{CFString, CFStringRef},
    };
    use core_foundation_sys::bundle::{CFBundleGetIdentifier, CFBundleGetMainBundle};

    use super::Outcome;

    /// The type macOS gives `.md` and `.markdown` files (Info.plist imports
    /// it for systems that don't declare it).
    const MARKDOWN_TYPE: &str = "net.daringfireball.markdown";
    /// kLSRolesAll: viewer and editor.
    const ALL_ROLES: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            handler_bundle_id: CFStringRef,
        ) -> i32;
    }

    /// Malgel.app's bundle identifier; none when running outside the app.
    fn bundle_id() -> Option<String> {
        // SAFETY: both return borrowed references (or null) that stay valid
        // for the life of the process; the string is retained before use.
        unsafe {
            let bundle = CFBundleGetMainBundle();
            if bundle.is_null() {
                return None;
            }
            let id = CFBundleGetIdentifier(bundle);
            (!id.is_null()).then(|| CFString::wrap_under_get_rule(id).to_string())
        }
    }

    pub fn is_default() -> Option<bool> {
        let id = bundle_id()?;
        let content_type = CFString::new(MARKDOWN_TYPE);
        // SAFETY: the content type outlives the call; a non-null result is
        // owned by us (the Copy rule) and released by CFString's Drop.
        let handler = unsafe {
            let handler = LSCopyDefaultRoleHandlerForContentType(
                content_type.as_concrete_TypeRef(),
                ALL_ROLES,
            );
            (!handler.is_null()).then(|| CFString::wrap_under_create_rule(handler).to_string())
        };
        Some(handler.is_some_and(|handler| handler.eq_ignore_ascii_case(&id)))
    }

    pub fn make_default(_: &mut gpui_kit::App) -> Result<Outcome, String> {
        let id = bundle_id().ok_or("Only Malgel.app can become the default app.")?;
        let content_type = CFString::new(MARKDOWN_TYPE);
        let handler = CFString::new(&id);
        // SAFETY: both strings outlive the call.
        let status = unsafe {
            LSSetDefaultRoleHandlerForContentType(
                content_type.as_concrete_TypeRef(),
                ALL_ROLES,
                handler.as_concrete_TypeRef(),
            )
        };
        if status == 0 {
            Ok(Outcome::Done)
        } else {
            Err(format!("macOS refused the change (error {status})."))
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows_registry::{CLASSES_ROOT, CURRENT_USER, LOCAL_MACHINE};

    use super::Outcome;

    const APP_NAME: &str = "Malgel";
    const APP_EXE: &str = "Malgel.exe";
    /// The same document type the installer registers.
    const PROG_ID: &str = "Malgel.Markdown";
    const EXTENSIONS: [&str; 4] = [".md", ".markdown", ".mdown", ".mkd"];

    /// The ProgId of the app the user chose for `.md` files.
    fn chosen_prog_id() -> Option<String> {
        let base = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.md";
        // Windows 11 keeps newer choices under UserChoiceLatest.
        ["UserChoiceLatest", "UserChoice"]
            .into_iter()
            .find_map(|key| {
                CURRENT_USER
                    .open(format!(r"{base}\{key}"))
                    .ok()?
                    .get_string("ProgId")
                    .ok()
            })
            .or_else(|| CLASSES_ROOT.open(".md").ok()?.get_string("").ok())
    }

    pub fn is_default() -> Option<bool> {
        let chosen = chosen_prog_id();
        Some(chosen.is_some_and(|prog_id| {
            prog_id.eq_ignore_ascii_case(PROG_ID)
                || prog_id.eq_ignore_ascii_case(&format!(r"Applications\{APP_EXE}"))
        }))
    }

    /// Whether an all-users install registered Malgel.
    fn registered_for_machine() -> bool {
        LOCAL_MACHINE
            .open(r"Software\RegisteredApplications")
            .and_then(|key| key.get_string(APP_NAME))
            .is_ok()
    }

    /// Register this copy of Malgel for Markdown files for the current user,
    /// as the installer does, so a portable copy is offered too.
    fn register_for_user() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        register_exe(&exe.display().to_string()).map_err(|err| err.message())
    }

    fn register_exe(exe: &str) -> windows_registry::Result<()> {
        let open = format!("\"{exe}\" \"%1\"");
        let icon = format!("{exe},0");

        let classes = CURRENT_USER.create(r"Software\Classes")?;
        let prog_id = classes.create(PROG_ID)?;
        prog_id.set_string("", "Markdown Document")?;
        prog_id.create("DefaultIcon")?.set_string("", &icon)?;
        prog_id
            .create(r"shell\open\command")?
            .set_string("", &open)?;
        for extension in EXTENSIONS {
            classes
                .create(format!(r"{extension}\OpenWithProgids"))?
                .set_string(PROG_ID, "")?;
        }

        let capabilities = CURRENT_USER.create(format!(r"Software\{APP_NAME}\Capabilities"))?;
        capabilities.set_string("ApplicationName", APP_NAME)?;
        capabilities.set_string("ApplicationDescription", "Write and preview Markdown")?;
        let associations = capabilities.create("FileAssociations")?;
        for extension in EXTENSIONS {
            associations.set_string(extension, PROG_ID)?;
        }
        CURRENT_USER
            .create(r"Software\RegisteredApplications")?
            .set_string(APP_NAME, format!(r"Software\{APP_NAME}\Capabilities"))?;

        // Tell Explorer the associations changed.
        // SAFETY: SHCNE_ASSOCCHANGED takes no items.
        unsafe {
            windows_sys::Win32::UI::Shell::SHChangeNotify(
                windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
                windows_sys::Win32::UI::Shell::SHCNF_IDLIST,
                std::ptr::null(),
                std::ptr::null(),
            );
        }
        Ok(())
    }

    pub fn make_default(cx: &mut gpui_kit::App) -> Result<Outcome, String> {
        let page = if registered_for_machine() {
            "registeredAppMachine"
        } else {
            register_for_user()
                .map_err(|err| format!("Couldn’t register Malgel for Markdown files: {err}"))?;
            "registeredAppUser"
        };
        // Default apps, on Malgel's own page (Windows 11; Windows 10 shows
        // the general page).
        cx.open_url(&format!("ms-settings:defaultapps?{page}={APP_NAME}"));
        Ok(Outcome::SettingsOpened)
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{MIME_TYPES, Outcome, desktop_entry, mimeapps};

    const DESKTOP_ID: &str = "dev.malgel.Malgel.desktop";
    const ICON: &[u8] = include_bytes!("../packaging/icon/malgel.svg");

    fn home() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn env_dir(name: &str, fallback: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(fallback)
    }

    fn config_home() -> Option<PathBuf> {
        env_dir("XDG_CONFIG_HOME", || Some(home()?.join(".config")))
    }

    fn data_home() -> Option<PathBuf> {
        env_dir("XDG_DATA_HOME", || Some(home()?.join(".local/share")))
    }

    fn dirs(name: &str, fallback: &str) -> Vec<PathBuf> {
        let value = std::env::var(name).unwrap_or_default();
        let value = if value.is_empty() { fallback } else { &value };
        value
            .split(':')
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .collect()
    }

    /// Where desktop entries live, most important first.
    fn application_dirs() -> Vec<PathBuf> {
        data_home()
            .into_iter()
            .chain(dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"))
            .map(|dir| dir.join("applications"))
            .collect()
    }

    fn installed(desktop_id: &str) -> bool {
        application_dirs()
            .iter()
            .any(|dir| dir.join(desktop_id).is_file())
    }

    /// mimeapps.list files in the order the spec consults them.
    fn mimeapps_files() -> Vec<PathBuf> {
        let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|name| !name.is_empty())
            .map(str::to_lowercase)
            .collect();
        let config_dirs = config_home()
            .into_iter()
            .chain(dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
        let data_dirs = application_dirs();
        let mut files = Vec::new();
        for dir in config_dirs.chain(data_dirs) {
            for desktop in &desktops {
                files.push(dir.join(format!("{desktop}-mimeapps.list")));
            }
            files.push(dir.join("mimeapps.list"));
        }
        files
    }

    pub fn is_default() -> Option<bool> {
        for file in mimeapps_files() {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let defaults = mimeapps::defaults(&text, MIME_TYPES[0]);
            if let Some(first) = defaults.iter().find(|id| installed(id)) {
                return Some(first == DESKTOP_ID);
            }
        }
        Some(false)
    }

    /// The command that starts this copy of Malgel: the AppImage when
    /// running from one, as the binary inside it moves on every start.
    fn launcher() -> Option<PathBuf> {
        std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
    }

    /// Add a desktop entry for this copy of Malgel, so the desktop can open
    /// files with it: needed for the AppImage and an unpacked tarball.
    fn install_desktop_entry(applications: &Path) -> std::io::Result<()> {
        let exe = launcher().ok_or_else(|| std::io::Error::other("no executable path"))?;
        std::fs::create_dir_all(applications)?;
        std::fs::write(
            applications.join(DESKTOP_ID),
            desktop_entry(&exe.display().to_string()),
        )?;
        if let Some(icons) = data_home().map(|dir| dir.join("icons/hicolor/scalable/apps")) {
            std::fs::create_dir_all(&icons)?;
            std::fs::write(icons.join("dev.malgel.Malgel.svg"), ICON)?;
        }
        // Refresh the MIME cache where the tool exists; desktops also
        // rescan on their own.
        let _ = std::process::Command::new("update-desktop-database")
            .arg(applications)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(())
    }

    pub fn make_default(_: &mut gpui_kit::App) -> Result<Outcome, String> {
        let applications = data_home()
            .map(|dir| dir.join("applications"))
            .ok_or("Couldn’t find your home folder.")?;
        let own_entry = applications.join(DESKTOP_ID);
        // Installed packages ship a desktop entry; other copies need one,
        // and an AppImage's own entry must point at the current AppImage.
        if std::env::var_os("APPIMAGE").is_some() || !installed(DESKTOP_ID) || own_entry.is_file() {
            install_desktop_entry(&applications)
                .map_err(|err| format!("Couldn’t add Malgel to your applications: {err}"))?;
        }

        let file = config_home()
            .map(|dir| dir.join("mimeapps.list"))
            .ok_or("Couldn’t find your settings folder.")?;
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let text = mimeapps::set_defaults(&text, &MIME_TYPES, DESKTOP_ID);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        std::fs::write(&file, text)
            .map_err(|err| format!("Couldn’t save {}: {err}", file.display()))?;
        Ok(Outcome::Done)
    }
}

#[cfg(not(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "freebsd"
)))]
mod platform {
    use super::Outcome;

    pub fn is_default() -> Option<bool> {
        None
    }

    pub fn make_default(_: &mut gpui_kit::App) -> Result<Outcome, String> {
        Err("This system has no default apps Malgel knows how to set.".into())
    }
}

/// A desktop entry that starts `exe`, quoted as the Desktop Entry spec
/// requires.
#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
fn desktop_entry(exe: &str) -> String {
    // Quote for the Exec key's own rules, then escape backslashes again for
    // the string value the key is.
    let mut quoted = String::from("\"");
    for ch in exe.chars() {
        if matches!(ch, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    let quoted = quoted.replace('\\', "\\\\").replace('%', "%%");
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Malgel\n\
         GenericName=Markdown Editor\n\
         Comment=Write and preview Markdown\n\
         Keywords=markdown;md;editor;preview;text;\n\
         Exec={quoted} %F\n\
         Icon=dev.malgel.Malgel\n\
         Terminal=false\n\
         Categories=Utility;TextEditor;\n\
         MimeType={};\n\
         StartupWMClass=dev.malgel.Malgel\n",
        MIME_TYPES.join(";")
    )
}

/// Reading and editing `mimeapps.list` files.
#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
mod mimeapps {
    const DEFAULTS: &str = "[Default Applications]";

    /// The desktop IDs a file lists as defaults for `mime_type`.
    pub fn defaults(text: &str, mime_type: &str) -> Vec<String> {
        let mut in_defaults = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_defaults = line == DEFAULTS;
            } else if in_defaults
                && let Some((key, value)) = line.split_once('=')
                && key.trim() == mime_type
            {
                return value
                    .split(';')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(String::from)
                    .collect();
            }
        }
        Vec::new()
    }

    /// `text` with `desktop_id` as the default for each of `mime_types`,
    /// keeping everything else.
    pub fn set_defaults(text: &str, mime_types: &[&str], desktop_id: &str) -> String {
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let is_ours = |line: &str| {
            line.split_once('=')
                .is_some_and(|(key, _)| mime_types.contains(&key.trim()))
        };
        // Drop existing defaults for these types, then add ours at the top
        // of the section (created at the end if missing).
        let mut in_defaults = false;
        lines.retain(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_defaults = trimmed == DEFAULTS;
                return true;
            }
            !(in_defaults && is_ours(trimmed))
        });
        let ours = mime_types
            .iter()
            .map(|mime_type| format!("{mime_type}={desktop_id};"));
        match lines.iter().position(|line| line.trim() == DEFAULTS) {
            Some(index) => {
                lines.splice(index + 1..index + 1, ours);
            }
            None => {
                if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(DEFAULTS.into());
                lines.extend(ours);
            }
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_defaults() {
        let text = "[Added Associations]\ntext/markdown=other.desktop;\n\n\
                    [Default Applications]\ntext/html=firefox.desktop\n\
                    text/markdown = a.desktop;b.desktop;\n";
        assert_eq!(
            mimeapps::defaults(text, "text/markdown"),
            ["a.desktop", "b.desktop"]
        );
        assert!(mimeapps::defaults(text, "text/plain").is_empty());
    }

    #[test]
    fn sets_defaults_in_an_existing_section() {
        let text = "[Default Applications]\ntext/html=firefox.desktop\n\
                    text/markdown=gedit.desktop;\n\n[Added Associations]\n\
                    text/markdown=gedit.desktop;\n";
        let updated = mimeapps::set_defaults(text, &MIME_TYPES, "m.desktop");
        assert_eq!(
            updated,
            "[Default Applications]\ntext/markdown=m.desktop;\ntext/x-markdown=m.desktop;\n\
             text/html=firefox.desktop\n\n[Added Associations]\ntext/markdown=gedit.desktop;\n"
        );
        assert_eq!(mimeapps::defaults(&updated, "text/markdown"), ["m.desktop"]);
    }

    #[test]
    fn sets_defaults_in_a_new_file() {
        assert_eq!(
            mimeapps::set_defaults("", &MIME_TYPES, "m.desktop"),
            "[Default Applications]\ntext/markdown=m.desktop;\ntext/x-markdown=m.desktop;\n"
        );
        assert_eq!(
            mimeapps::set_defaults("[Added Associations]\na=b;", &["t"], "m.desktop"),
            "[Added Associations]\na=b;\n\n[Default Applications]\nt=m.desktop;\n"
        );
    }

    #[test]
    fn quotes_the_executable_in_desktop_entries() {
        let entry = desktop_entry("/home/me/Apps/Malgel $1 \"x\" 100%.AppImage");
        assert!(
            entry
                .contains("Exec=\"/home/me/Apps/Malgel \\\\$1 \\\\\"x\\\\\" 100%%.AppImage\" %F\n")
        );
        assert!(entry.contains("MimeType=text/markdown;text/x-markdown;\n"));
    }
}
