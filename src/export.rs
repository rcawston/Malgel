//! Export a document as a standalone HTML page.

use markdown::{CompileOptions, Options};

use crate::analysis::preview_parse_options;

const STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --fg: #1f2328; --muted: #59636e; --bg: #ffffff; --subtle: #f6f8fa;
  --border: #d1d9e0; --link: #0969da;
}
@media (prefers-color-scheme: dark) {
  :root {
    --fg: #e6edf3; --muted: #9198a1; --bg: #0d1117; --subtle: #151b23;
    --border: #3d444d; --link: #4493f8;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg); color: var(--fg);
  font: 16px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif;
}
main { max-width: 46rem; margin: 0 auto; padding: 3rem 1.5rem 5rem; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.6em 0 0.6em; font-weight: 600; }
h1 { font-size: 2em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h2 { font-size: 1.5em; padding-bottom: .3em; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.25em; }
main > :first-child { margin-top: 0; }
p, ul, ol, blockquote, pre, table { margin: 0 0 1em; }
a { color: var(--link); text-decoration: none; }
a:hover { text-decoration: underline; }
blockquote { margin-left: 0; padding: 0 1em; color: var(--muted); border-left: .25em solid var(--border); }
code, pre { font-family: ui-monospace, SFMono-Regular, "JetBrains Mono", Menlo, Consolas, monospace; font-size: .875em; }
code { background: var(--subtle); padding: .15em .35em; border-radius: 6px; }
pre { background: var(--subtle); padding: 1em; border-radius: 8px; overflow: auto; border: 1px solid var(--border); }
pre code { background: none; padding: 0; font-size: 1em; }
table { border-collapse: collapse; display: block; overflow: auto; }
th, td { border: 1px solid var(--border); padding: .4em .8em; }
th { background: var(--subtle); font-weight: 600; }
img { max-width: 100%; }
hr { border: 0; border-top: 1px solid var(--border); margin: 2em 0; }
li + li { margin-top: .25em; }
input[type=checkbox] { margin-right: .4em; }
"#;

/// Render `source` as a complete HTML document. Raw HTML in the source is
/// escaped, so the exported page cannot run scripts from the document.
pub fn to_html(source: &str, title: &str) -> String {
    let options = Options {
        parse: preview_parse_options(),
        compile: CompileOptions::gfm(),
    };
    let body = markdown::to_html_with_options(source, &options)
        .unwrap_or_else(|_| format!("<pre>{}</pre>", escape(source)));

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"generator\" content=\"Malgel\">\n<title>{}</title>\n<style>{STYLE}</style>\n\
         </head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n",
        escape(title)
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_gfm_and_escapes_html() {
        let html = to_html(
            "# Hi <there>\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<script>x</script>\n",
            "A & B",
        );
        assert!(html.contains("<title>A &amp; B</title>"));
        assert!(html.contains("<h1>Hi &lt;there&gt;</h1>"));
        assert!(html.contains("<table>"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn skips_frontmatter() {
        let html = to_html("---\ntitle: x\n---\n\nBody\n", "t");
        assert!(!html.contains("title: x"));
        assert!(html.contains("<p>Body</p>"));
    }
}
