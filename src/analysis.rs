//! Document analysis that runs off the UI thread.
//!
//! One pass over the source produces everything the chrome needs besides the
//! rendered preview: word/character statistics for the status bar, the line
//! where every top-level block starts (so the preview can follow the editor),
//! the heading outline, and a content hash used to detect when an edit has
//! returned the document to its saved state.

use std::hash::{Hash as _, Hasher as _};

use markdown::{ParseOptions, mdast::Node};

/// Words per minute used for the reading-time estimate.
const READING_WORDS_PER_MINUTE: usize = 230;

/// Status bar statistics for a document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub words: usize,
    pub characters: usize,
    pub lines: usize,
}

impl Stats {
    /// Count words, characters and lines in `source`.
    ///
    /// A word is a run of letters, digits, apostrophes or connecting
    /// punctuation. Ideographic scripts do not separate words with spaces, so
    /// every CJK, Hiragana, Katakana or Hangul syllable counts as one word,
    /// which matches how those languages are usually measured.
    pub fn of(source: &str) -> Self {
        let mut words = 0;
        let mut characters = 0;
        let mut in_word = false;

        for ch in source.chars() {
            characters += 1;
            if is_ideograph(ch) {
                words += 1;
                in_word = false;
            } else if ch.is_alphanumeric() || (in_word && matches!(ch, '\'' | '’' | '_' | '-')) {
                if !in_word {
                    words += 1;
                    in_word = true;
                }
            } else {
                in_word = false;
            }
        }

        let lines = if source.is_empty() {
            1
        } else {
            source.lines().count() + usize::from(source.ends_with('\n'))
        };

        Self {
            words,
            characters,
            lines,
        }
    }

    /// Estimated reading time in whole minutes; at least one for any text.
    pub fn reading_minutes(&self) -> usize {
        if self.words == 0 {
            0
        } else {
            self.words.div_ceil(READING_WORDS_PER_MINUTE)
        }
    }
}

fn is_ideograph(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2FA1F)
}

/// A heading in the document outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    /// Zero-based source line of the heading.
    pub line: usize,
}

/// Maps between editor lines and preview blocks.
///
/// The preview renders every top-level Markdown block as one row of a
/// virtual list, so the start line of each block is enough to translate a
/// scroll position in either direction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockIndex {
    /// Zero-based start line of every top-level block, ascending.
    starts: Vec<usize>,
    /// Total number of lines in the document.
    line_count: usize,
}

impl BlockIndex {
    pub fn new(starts: Vec<usize>, line_count: usize) -> Self {
        debug_assert!(starts.windows(2).all(|pair| pair[0] <= pair[1]));
        Self {
            starts,
            line_count: line_count.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    /// The block containing `line`, and how far through it `line` is (0..1).
    pub fn block_at_line(&self, line: f32) -> Option<(usize, f32)> {
        if self.starts.is_empty() {
            return None;
        }
        let ix = self
            .starts
            .partition_point(|start| *start as f32 <= line)
            .saturating_sub(1);
        let start = self.starts[ix] as f32;
        let end = self.block_end(ix) as f32;
        let fraction = if end > start {
            ((line - start) / (end - start)).clamp(0., 1.)
        } else {
            0.
        };
        Some((ix, fraction))
    }

    /// The source line `fraction` of the way through block `ix`.
    pub fn line_at_block(&self, ix: usize, fraction: f32) -> Option<f32> {
        let start = *self.starts.get(ix)? as f32;
        let end = self.block_end(ix) as f32;
        Some(start + (end - start) * fraction.clamp(0., 1.))
    }

    /// The first line after block `ix`: where the next block starts, or the
    /// end of the document for the last block.
    fn block_end(&self, ix: usize) -> usize {
        self.starts
            .get(ix + 1)
            .copied()
            .unwrap_or(self.line_count)
            .max(self.starts[ix] + 1)
    }
}

/// Everything computed from one version of the source.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub stats: Stats,
    pub blocks: BlockIndex,
    pub outline: Vec<Heading>,
    pub content_hash: u64,
}

impl Analysis {
    pub fn of(source: &str) -> Self {
        let stats = Stats::of(source);
        let mut starts = Vec::new();
        let mut outline = Vec::new();

        if let Ok(Node::Root(root)) = markdown::to_mdast(source, &preview_parse_options()) {
            for node in &root.children {
                let Some(position) = node.position() else {
                    continue;
                };
                let line = position.start.line.saturating_sub(1);
                starts.push(line);
                if let Node::Heading(heading) = node {
                    let text = plain_text(&heading.children);
                    if !text.is_empty() {
                        outline.push(Heading {
                            level: heading.depth,
                            text,
                            line,
                        });
                    }
                }
            }
        }

        Self {
            stats,
            blocks: BlockIndex::new(starts, stats.lines),
            outline,
            content_hash: content_hash(source),
        }
    }
}

/// The parse options the preview uses, so block boundaries agree with the
/// rows the preview renders.
pub fn preview_parse_options() -> ParseOptions {
    let mut options = ParseOptions::gfm();
    options.constructs.frontmatter = true;
    options.constructs.math_text = true;
    options.constructs.math_flow = true;
    options
}

/// A fast, non-cryptographic hash of the document text.
pub fn content_hash(source: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

fn plain_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(&text.value),
            Node::InlineCode(code) => out.push_str(&code.value),
            Node::InlineMath(math) => out.push_str(&math.value),
            other => {
                if let Some(children) = other.children() {
                    out.push_str(&plain_text(children));
                }
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_words_across_scripts() {
        let stats = Stats::of("Hello, world! It's a **bold** move.\n");
        assert_eq!(stats.words, 6);
        assert_eq!(stats.lines, 2);

        // Each ideograph counts as one word; Latin runs still count once.
        assert_eq!(Stats::of("你好世界 GPUI").words, 5);
        assert_eq!(Stats::of("").words, 0);
        assert_eq!(Stats::of("").lines, 1);
    }

    #[test]
    fn reading_time_rounds_up() {
        assert_eq!(Stats::default().reading_minutes(), 0);
        let stats = Stats {
            words: 231,
            ..Default::default()
        };
        assert_eq!(stats.reading_minutes(), 2);
    }

    #[test]
    fn indexes_top_level_blocks_and_headings() {
        let source = "# Title\n\nFirst paragraph\nstill first.\n\n## Section `code`\n\n- a\n- b\n";
        let analysis = Analysis::of(source);
        assert_eq!(analysis.blocks.starts, vec![0, 2, 5, 7]);
        assert_eq!(
            analysis.outline,
            vec![
                Heading {
                    level: 1,
                    text: "Title".into(),
                    line: 0
                },
                Heading {
                    level: 2,
                    text: "Section code".into(),
                    line: 5
                },
            ]
        );
    }

    #[test]
    fn maps_lines_to_blocks_and_back() {
        let index = BlockIndex::new(vec![0, 2, 10], 20);
        assert_eq!(index.block_at_line(0.), Some((0, 0.)));
        assert_eq!(index.block_at_line(6.), Some((1, 0.5)));
        assert_eq!(index.block_at_line(15.), Some((2, 0.5)));
        assert_eq!(index.line_at_block(1, 0.5), Some(6.));
        assert_eq!(index.line_at_block(3, 0.), None);
        assert_eq!(BlockIndex::default().block_at_line(3.), None);
    }

    #[test]
    fn hash_tracks_content() {
        assert_eq!(content_hash("a"), content_hash("a"));
        assert_ne!(content_hash("a"), content_hash("b"));
    }
}
