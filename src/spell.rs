//! Spell checking against Hunspell dictionaries.
//!
//! American English is bundled; other languages come from dictionaries
//! installed for the system (Hunspell's folders on Linux, `~/Library/Spelling`
//! on macOS) or dropped into Malgel's `dictionaries` folder as `xx_YY.aff` and
//! `xx_YY.dic`. Only prose is checked: code, math, URLs, HTML and front
//! matter are skipped, as are words with digits and all-caps acronyms.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, RwLock},
};

use markdown::mdast::Node;
use spellbook::Dictionary;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::{analysis::preview_parse_options, settings::config_dir};

const BUNDLED_LANGUAGE: &str = "en_US";
const BUNDLED_AFF: &str = include_str!("../dictionaries/en_US.aff");
const BUNDLED_DIC: &str = include_str!("../dictionaries/en_US.dic");
const MAX_SUGGESTIONS: usize = 6;

/// A word the dictionary doesn't know, as a byte range of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misspelling {
    pub range: Range<usize>,
    pub word: String,
}

/// Folders searched for installed dictionaries, most specific first.
fn dictionary_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = config_dir()
        .map(|dir| dir.join("dictionaries"))
        .into_iter()
        .collect();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.extend(home.map(|home| home.join("Library/Spelling")));
        dirs.push(PathBuf::from("/Library/Spelling"));
    } else if cfg!(unix) {
        dirs.extend(home.map(|home| home.join(".local/share/hunspell")));
        for dir in [
            "/usr/share/hunspell",
            "/usr/local/share/hunspell",
            "/usr/share/myspell",
            "/usr/share/myspell/dicts",
        ] {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs
}

/// The dictionaries that can be used, like `en_US`, sorted.
pub fn available_languages() -> Vec<String> {
    let mut languages: Vec<String> = dictionary_dirs()
        .into_iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "dic" && path.with_extension("aff").exists()).then_some(stem)
        })
        .filter(|name| is_language_name(name))
        .collect();
    languages.push(BUNDLED_LANGUAGE.to_string());
    languages.sort();
    languages.dedup();
    languages
}

fn is_language_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 16
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// The dictionary matching the system language, or the bundled one.
pub fn default_language() -> String {
    let available = available_languages();
    let locale = sys_locale::get_locale()
        .unwrap_or_default()
        .replace('-', "_");
    let language = locale.split('_').next().unwrap_or_default().to_string();
    available
        .iter()
        .find(|name| name.eq_ignore_ascii_case(&locale))
        .or_else(|| {
            available
                .iter()
                .find(|name| name.split('_').next() == Some(language.as_str()))
        })
        .cloned()
        .unwrap_or_else(|| BUNDLED_LANGUAGE.to_string())
}

static DICTIONARIES: LazyLock<Mutex<HashMap<String, Option<Arc<Dictionary>>>>> =
    LazyLock::new(Default::default);

/// The dictionary for `language`, loading it on first use. Loading takes a
/// moment, so call this off the UI thread.
pub fn dictionary(language: &str) -> Option<Arc<Dictionary>> {
    if let Some(loaded) = DICTIONARIES.lock().ok()?.get(language) {
        return loaded.clone();
    }
    let loaded = load(language).map(Arc::new);
    DICTIONARIES
        .lock()
        .ok()?
        .insert(language.to_string(), loaded.clone());
    loaded
}

fn load(language: &str) -> Option<Dictionary> {
    if !is_language_name(language) {
        return None;
    }
    for dir in dictionary_dirs() {
        let dic = dir.join(format!("{language}.dic"));
        if let (Ok(aff), Ok(dic)) = (
            std::fs::read_to_string(dir.join(format!("{language}.aff"))),
            std::fs::read_to_string(dic),
        ) && let Ok(dictionary) = Dictionary::new(&aff, &dic)
        {
            return Some(dictionary);
        }
    }
    (language == BUNDLED_LANGUAGE)
        .then(|| Dictionary::new(BUNDLED_AFF, BUNDLED_DIC).ok())
        .flatten()
}

/// Words the user added, in every language.
static PERSONAL: LazyLock<RwLock<HashSet<String>>> = LazyLock::new(|| {
    let words = personal_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|word| !word.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    RwLock::new(words)
});

fn personal_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("dictionary.txt"))
}

/// Accept `word` from now on, remembering it between sessions.
pub fn add_to_personal(word: &str) {
    let word = word.trim().to_string();
    if word.is_empty() {
        return;
    }
    let Ok(mut words) = PERSONAL.write() else {
        return;
    };
    if !words.insert(word) {
        return;
    }
    if let Some(path) = personal_path() {
        let mut sorted: Vec<&String> = words.iter().collect();
        sorted.sort();
        let text: String = sorted.iter().map(|word| format!("{word}\n")).collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = crate::document::save(&path, &text, crate::document::LineEnding::Lf);
    }
}

fn is_personal(word: &str) -> bool {
    PERSONAL
        .read()
        .is_ok_and(|words| words.contains(word) || words.contains(&word.to_lowercase()))
}

/// Find the prose words in `source` the dictionary doesn't know.
pub fn check(source: &str, dictionary: &Dictionary) -> Vec<Misspelling> {
    let Ok(root) = markdown::to_mdast(source, &preview_parse_options()) else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    prose_ranges(&root, source, &mut ranges);
    let mut found = Vec::new();
    for range in ranges {
        let text = &source[range.clone()];
        for (offset, token) in text.split_word_bound_indices() {
            let Some((trimmed, lead)) = checkable(token) else {
                continue;
            };
            if !known(dictionary, trimmed) {
                let start = range.start + offset + lead;
                found.push(Misspelling {
                    range: start..start + trimmed.len(),
                    word: trimmed.to_string(),
                });
            }
        }
    }
    found
}

/// Suggested replacements for `word`, best first.
pub fn suggest(dictionary: &Dictionary, word: &str) -> Vec<String> {
    let mut out = Vec::new();
    dictionary.suggest(&word.replace('’', "'"), &mut out);
    out.truncate(MAX_SUGGESTIONS);
    out
}

fn known(dictionary: &Dictionary, word: &str) -> bool {
    let word = word.replace('’', "'");
    if dictionary.check(&word) || is_personal(&word) {
        return true;
    }
    // Possessives of words the dictionary knows: "Malgel's" → "Malgel".
    word.strip_suffix("'s")
        .is_some_and(|stem| dictionary.check(stem) || is_personal(stem))
}

/// The part of a word-boundary token worth checking, and its offset in the
/// token. Numbers, acronyms, identifiers and single letters are skipped.
fn checkable(token: &str) -> Option<(&str, usize)> {
    let trimmed = token.trim_matches(['\'', '’']);
    let lead = token.len() - token.trim_start_matches(['\'', '’']).len();
    let letters = trimmed.chars().filter(|ch| ch.is_alphabetic()).count();
    if letters < 2
        || trimmed
            .chars()
            .any(|ch| ch.is_ascii_digit() || ch == '_' || ch == '.')
    {
        return None;
    }
    // Scripts without spaces between words (CJK, Thai) aren't split into
    // words here; leave them alone.
    if trimmed.chars().any(|ch| {
        matches!(ch as u32, 0x0E00..=0x0EFF | 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF)
    }) {
        return None;
    }
    let mut chars = trimmed.chars();
    let rest_upper = chars.next().is_some_and(char::is_uppercase)
        && trimmed
            .chars()
            .filter(|ch| ch.is_alphabetic())
            .all(char::is_uppercase);
    // camelCase and ACRONYMS are names, not words.
    let inner_upper = trimmed.chars().skip(1).any(char::is_uppercase);
    if rest_upper || inner_upper {
        return None;
    }
    Some((trimmed, lead))
}

/// Byte ranges of the document's prose.
fn prose_ranges(node: &Node, source: &str, out: &mut Vec<Range<usize>>) {
    let range = || {
        node.position()
            .map(|position| position.start.offset..position.end.offset)
            .filter(|range| source.get(range.clone()).is_some())
    };
    match node {
        Node::Text(_) => out.extend(range()),
        Node::Code(_)
        | Node::InlineCode(_)
        | Node::Math(_)
        | Node::InlineMath(_)
        | Node::Html(_)
        | Node::Yaml(_)
        | Node::Toml(_)
        | Node::Image(_)
        | Node::ImageReference(_)
        | Node::Definition(_) => {}
        // Bare URLs become links whose text is the URL itself.
        Node::Link(link)
            if link.children.iter().all(|child| {
                matches!(child, Node::Text(text) if link.url.ends_with(text.value.as_str()))
            }) => {}
        // GitHub alert markers aren't words.
        Node::Paragraph(paragraph)
            if range().is_some_and(|range| source[range].trim_start().starts_with("[!")) =>
        {
            for child in paragraph.children.iter().skip(1) {
                prose_ranges(child, source, out);
            }
            if let Some(Node::Text(first)) = paragraph.children.first()
                && let Some(position) = &first.position
            {
                let text = &source[position.start.offset..position.end.offset];
                if let Some(end) = text.find(']') {
                    out.push(position.start.offset + end + 1..position.end.offset);
                }
            }
        }
        _ => {
            for child in node.children().into_iter().flatten() {
                prose_ranges(child, source, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn misspelled(source: &str) -> Vec<String> {
        let dictionary = dictionary(BUNDLED_LANGUAGE).unwrap();
        check(source, &dictionary)
            .into_iter()
            .map(|misspelling| {
                assert_eq!(&source[misspelling.range.clone()], misspelling.word);
                misspelling.word
            })
            .collect()
    }

    #[test]
    fn finds_misspelled_prose_only() {
        assert_eq!(
            misspelled(
                "# Teh title\n\nThe quick brwn fox doesn't jump.\n\n\
                 `mispeled` and $\\alfa$ and https://exampel.com and [a link](http://x.y/badwurd)\n\n\
                 ```\nnot chekced\n```\n\nNASA and iPhone and v2 and x.\n"
            ),
            ["Teh", "brwn"]
        );
    }

    #[test]
    fn skips_markup_and_accepts_possessives() {
        assert_eq!(
            misspelled("---\ntitle: Frnt\n---\n\n> [!NOTE]\n> Water's edge, it’s fine.\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            misspelled("<span class=\"tagg\">fine</span> 日本語のテキスト"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn suggests_corrections() {
        let dictionary = dictionary(BUNDLED_LANGUAGE).unwrap();
        assert!(
            suggest(&dictionary, "helo")
                .iter()
                .any(|word| word == "hello")
        );
        assert!(available_languages().iter().any(|name| name == "en_US"));
    }
}
