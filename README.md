# Malgel

A fast, native Markdown editor and previewer written in pure Rust with
[GPUI Kit](https://github.com/longbridge/gpui-kit) (GPUI + GPUI Component).

![Malgel in the light appearance](docs/screenshot-light.png)

## Features

- **Editor and preview side by side.** A syntax-highlighted Markdown editor
  (tree-sitter) with line numbers, soft wrap, multi-cursor editing,
  find & replace and undo history, next to a live GitHub-flavored preview.
- **Two-way scroll sync.** Scroll or type in the editor and the preview
  stays on the same block; scroll the preview and the editor follows to the
  matching source line. Whichever pane you are pointing at or typing in
  leads, so the two never fight.
- **Three layouts.** Editor only, editor and preview, or preview only
  (Ctrl+1/2/3). Single-pane layouts center a comfortable reading column.
- **Rich preview.** Tables, task lists, strikethrough, fenced code with
  highlighting for 28 languages and a copy button, YAML front matter shown as
  a property list, local images (relative to the document), remote images
  and `data:` URLs.
- **GitHub alerts.** `> [!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]` and
  `[!CAUTION]` render as themed callouts.
- **Math.** LaTeX formulas, inline (`$…$`) and display (`$$…$$`), are
  typeset in pure Rust: LaTeX is translated to Typst math and laid out by the
  Typst compiler with its bundled New Computer Modern Math font. Formulas sit
  on the text baseline, follow the theme's text color, render on a background
  thread and are cached. Prices like "$5 and $10" stay text.
- **Formatting commands.** Bold, italic, strikethrough, inline code, links,
  headings, quotes, bulleted/numbered/task lists and code blocks — each
  toggles, and works on the selection or the word under the caret.
- **Go to heading.** An outline palette (Ctrl+Shift+O) jumps to any heading.
- **Documents done right.** Atomic saves, UTF-8 with byte-order-mark
  handling, CRLF files stay CRLF, unsaved-changes prompts on new, open,
  close and quit, recent files, drag and drop to open.
- **Export as HTML, PDF and Word.** All three render alerts and math as the
  preview does, embed local images, and work offline.
  - *HTML*: a standalone, self-styled page that follows the reader's
    light/dark preference, with formulas embedded as SVG. Raw HTML in the
    source is escaped.
  - *PDF*: typeset by the Typst compiler, in-process: Libertinus Serif body
    text, vector math, footnotes, repeating table headers, and installed
    fonts as a fallback for CJK, emoji and other scripts. Paper follows the
    system locale: US Letter where it's standard, A4 elsewhere.
  - *Word (.docx)*: real heading styles (so the navigation pane works), Word
    lists and footnotes, tables with repeating headers, code blocks and
    alerts as shaded boxes, and formulas as high-resolution images aligned
    to the text baseline.
- **Themes.** Light, dark or follow the system, with bundled themes (Ayu,
  Catppuccin, Everforest, Flexoki, Gruvbox, macOS Classic, Solarized,
  Tokyo Night) selectable separately for light and dark.
- **Zoom.** Ctrl+= / Ctrl+- scale the entire interface, not just the text.
- **Live statistics.** Words (CJK-aware), characters, reading time, caret
  position and selection size in the status bar.

| Tokyo Night | Outline palette on Catppuccin Mocha |
| --- | --- |
| ![Dark appearance](docs/screenshot-dark.png) | ![Go to heading](docs/screenshot-outline.png) |

![GitHub alerts and math on Catppuccin Mocha](docs/screenshot-alerts-math.png)

## Performance

- The editor is GPUI Component's rope-backed code editor: edits, layout and
  painting touch only the visible lines, so typing stays instant in
  multi-megabyte files.
- The preview parses on a background thread; bursts of keystrokes are
  coalesced into one parse, and only the visible blocks are laid out and
  painted (virtualized list).
- Scroll sync maps top-level blocks to source lines. Lines scrolled far out
  of view are reached by an estimated jump that is corrected on the next
  frame, once the target line is laid out.
- Statistics, the heading outline and the scroll-sync index are computed in
  one debounced background pass; results from stale revisions are dropped.
- Nothing polls: the UI redraws only when state changes, so an idle window
  uses no CPU.

On a software-rendered Linux VM the window appears in about 150 ms, and
typing into a 1.5 MB document keeps pace with the keyboard.

## Install

Download Malgel from the
[releases page](https://github.com/rcawston/Malgel/releases):

- **macOS 11+** (Apple silicon and Intel): open the `.dmg` and drag Malgel
  to Applications.
- **Windows 10+:** run the `-setup.exe` installer, which also adds Malgel to
  "Open with" for Markdown files, or unzip the portable `Malgel.exe`.
- **Linux x86_64:** make the `.AppImage` executable and run it, or unpack the
  tarball into `~/.local`:
  `tar -xzf malgel-*-linux-x86_64.tar.gz -C ~/.local --strip-components=1`.

Builds made without signing certificates are unsigned (ad-hoc signed on
macOS); [docs/RELEASING.md](docs/RELEASING.md) explains how to open them.

## Building

Malgel needs a recent stable Rust toolchain (developed with Rust 1.98; 1.94
is too old for GPUI).

On Linux, install the libraries GPUI needs first (Ubuntu/Debian):

```sh
sudo apt install libfontconfig-dev libwayland-dev libxkbcommon-x11-dev \
  libx11-xcb-dev libssl-dev libzstd-dev libvulkan1
```

Then:

```sh
cargo run --release                 # opens the welcome document
cargo run --release -- notes.md     # opens a file
```

GPUI Kit supports macOS, Linux (Wayland and X11) and Windows. CI builds and
tests Malgel on all three; it has been exercised mostly on Linux.

`packaging/` holds the app icon and the macOS, Windows and Linux packaging.
Pushing a `v*` tag builds the installable packages and publishes a release;
see [docs/RELEASING.md](docs/RELEASING.md), which also covers code signing.

## Keyboard shortcuts

Ctrl is ⌘ on macOS.

| Command | Shortcut |
| --- | --- |
| New, Open…, Save, Save as… | Ctrl+N, Ctrl+O, Ctrl+S, Ctrl+Shift+S |
| Export as HTML… | Ctrl+Shift+E |
| Close window, Quit | Ctrl+W, Ctrl+Q |
| Editor / Editor and preview / Preview | Ctrl+1 / Ctrl+2 / Ctrl+3 |
| Go to heading… | Ctrl+Shift+O |
| Find…, Replace… | Ctrl+F, Ctrl+H |
| Bold, Italic, Strikethrough | Ctrl+B, Ctrl+I, Ctrl+Shift+X |
| Inline code, Link, Code block | Ctrl+E, Ctrl+K, Ctrl+Shift+C |
| Heading 1–6 | Ctrl+Alt+1 … Ctrl+Alt+6 |
| Quote, Bulleted, Numbered, Task list | Ctrl+Shift+., Ctrl+Shift+8, Ctrl+Shift+7, Ctrl+Shift+9 |
| Zoom in / out / actual size | Ctrl+=, Ctrl+-, Ctrl+0 |
| Switch light and dark | Ctrl+Shift+L |

## Settings

Preferences (appearance, themes, zoom, layout, wrapping, line numbers, scroll
sync and recent files) are saved automatically to `settings.json` in:

- Linux: `$XDG_CONFIG_HOME/malgel` (usually `~/.config/malgel`)
- macOS: `~/Library/Application Support/Malgel`
- Windows: `%APPDATA%\Malgel`

Set `MALGEL_CONFIG_DIR` to use another folder.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/main.rs` | Startup: GPUI Kit, HTTP client for images, themes, key bindings, menus, window |
| `src/workspace.rs` | The window: title bar, editor, preview, status bar, file commands, scroll sync |
| `src/analysis.rs` | Background statistics, outline and block index |
| `src/format.rs` | Formatting commands as pure, tested text transforms |
| `src/document.rs` | Loading and atomically saving files, line endings |
| `src/export.rs` | HTML export |
| `src/pdf.rs` | PDF export: Markdown → Typst markup → PDF |
| `src/docx.rs` | Word export |
| `src/typst_world.rs` | The sandboxed Typst environment shared by math and PDF export |
| `src/images.rs` | Resolving and loading image URLs relative to the document |
| `src/preview_ext.rs` | Preview plugins: GitHub alerts, block and inline math |
| `src/math.rs` | LaTeX → Typst → SVG/PNG formula rendering |
| `src/menus.rs`, `src/actions.rs` | Menus, actions and key bindings |
| `src/settings.rs`, `src/themes.rs` | Preferences and theme application |
| `build.rs`, `packaging/` | Windows exe icon and version info; app icon, bundles and installers |

Run the tests with `cargo test`.

## Acknowledgements

Built on [GPUI](https://www.gpui.rs) from Zed Industries and
[GPUI Kit](https://github.com/longbridge/gpui-kit) by Longbridge. The bundled
themes in `themes/` come from GPUI Kit and are licensed under Apache-2.0.
