//! Preview extensions beyond CommonMark and GFM: GitHub alerts and math.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use gpui_kit::assets::IconName;
use gpui_kit::{
    App, FontWeight, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Rgba, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, WeakEntity, Window,
    base::{TextView, TextViewState},
    component::{
        ActiveTheme as _, Icon, Sizable as _, h_flex,
        text::{
            InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode,
            MarkdownParseContext, MarkdownPlugin, markdown_ast::Node,
        },
        v_flex,
    },
    div, img,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    analysis::content_hash,
    math::{self, MathStyle},
};

/// The parser configuration and plugins the preview renders with.
pub fn markdown_extensions(preview: &gpui_kit::Entity<TextViewState>) -> MarkdownExtensions {
    let cache = MathCache::new(preview);
    let with_math = |extensions: MarkdownExtensions| {
        extensions
            .plugin(BlockMathPlugin {
                cache: cache.clone(),
            })
            .plugin(InlineMathPlugin {
                cache: cache.clone(),
            })
    };
    // Alert bodies are rendered by their own nested view, which needs math too.
    let alert = AlertPlugin {
        body_extensions: with_math(MarkdownExtensions::default()),
    };
    with_math(
        MarkdownExtensions::default()
            .frontmatter()
            .plugin(gpui_kit::component::text::FrontmatterPlugin::new())
            .plugin(alert),
    )
}

// -------------------------------------------------------------------------
// GitHub alerts

/// The five GitHub alert types, e.g. `> [!NOTE]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    fn from_marker(marker: &str) -> Option<Self> {
        let name = marker.strip_prefix("[!")?.strip_suffix(']')?;
        Some(match name.to_ascii_uppercase().as_str() {
            "NOTE" => Self::Note,
            "TIP" => Self::Tip,
            "IMPORTANT" => Self::Important,
            "WARNING" => Self::Warning,
            "CAUTION" => Self::Caution,
            _ => return None,
        })
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Note => IconName::Info,
            Self::Tip => IconName::Lightbulb,
            Self::Important => IconName::MessageSquareWarning,
            Self::Warning => IconName::TriangleAlert,
            Self::Caution => IconName::OctagonAlert,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Note => theme.info,
            Self::Tip => theme.success,
            Self::Important => theme.magenta,
            Self::Warning => theme.warning,
            Self::Caution => theme.danger,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Alert {
    kind: AlertKind,
    /// The alert's Markdown content, without the quote markers.
    body: SharedString,
}

/// Split a blockquote's source into an alert, if its first line is an alert
/// marker and nothing else, as GitHub requires.
pub fn parse_alert(blockquote_source: &str) -> Option<(AlertKind, String)> {
    let mut lines = blockquote_source.lines().map(|line| {
        let line = line.trim_start();
        match line.strip_prefix('>') {
            Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
            // A lazy continuation line.
            None => line,
        }
    });
    let kind = AlertKind::from_marker(lines.next()?.trim())?;
    let body = lines.collect::<Vec<_>>().join("\n");
    Some((kind, body.trim().to_string()))
}

struct AlertPlugin {
    body_extensions: MarkdownExtensions,
}

impl MarkdownPlugin for AlertPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "github-alert"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Blockquote(_) = node else {
            return None;
        };
        let source = cx.node_source(node)?;
        let (kind, body) = parse_alert(source)?;
        Some(
            MarkdownNode::new(
                "github-alert",
                Alert {
                    kind,
                    body: body.clone().into(),
                },
            )
            .text(format!("{}\n{body}", kind.title()))
            .markdown(source),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let alert = node.data::<Alert>().expect("alert node data");
        let color = alert.kind.color(cx);
        let id = SharedString::from(format!("alert-{}", content_hash(&alert.body)));

        v_flex()
            .w_full()
            .gap_1()
            .px_4()
            .py_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(color.opacity(0.35))
            .bg(color.opacity(0.06))
            .child(
                h_flex()
                    .gap_2()
                    .text_color(color)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(Icon::new(alert.kind.icon()).small())
                    .child(alert.kind.title()),
            )
            .when(!alert.body.is_empty(), |this| {
                this.child(
                    TextView::markdown(id, alert.body.clone())
                        .markdown_extensions(self.body_extensions.clone())
                        .w_full(),
                )
            })
    }
}

// -------------------------------------------------------------------------
// Math

#[derive(Debug, Clone, PartialEq, Eq)]
struct Formula {
    tex: String,
    style: MathStyle,
}

fn formula_node(tex: &str, style: MathStyle, source: &str) -> MarkdownNode {
    let name = match style {
        MathStyle::Inline => "inline-math",
        MathStyle::Display => "math",
    };
    MarkdownNode::new(
        name,
        Formula {
            tex: tex.to_string(),
            style,
        },
    )
    .text(tex.trim().to_string())
    .accessibility_label(format!("Formula: {}", tex.trim()))
    .markdown(source.to_string())
}

/// A rendered formula ready to paint.
struct PreparedMath {
    image: Arc<Image>,
    width: f32,
    height: f32,
    baseline: f32,
}

enum MathEntry {
    Pending,
    Ready(Arc<PreparedMath>),
    Failed(SharedString),
}

/// LaTeX source, style, font size (as bits) and RGBA color.
type MathKey = (String, MathStyle, u32, u32);

/// Formulas rendered for the preview, keyed by source, style, size and
/// color. Rendering runs on a background thread; the preview is asked to
/// lay out again once a formula is ready.
#[derive(Clone)]
struct MathCache {
    entries: Arc<Mutex<HashMap<MathKey, MathEntry>>>,
    preview: WeakEntity<TextViewState>,
}

/// Computer Modern has a smaller x-height than interface sans-serif fonts;
/// scale formulas so they read at the same size as the text around them.
pub(crate) const INLINE_MATH_SCALE: f32 = 1.15;
/// Display formulas are set a little larger again, as in print.
pub(crate) const DISPLAY_MATH_SCALE: f32 = 1.3;

/// Formulas kept before the cache starts over.
const MATH_CACHE_LIMIT: usize = 512;

enum Lookup {
    Ready(Arc<PreparedMath>),
    Waiting,
    Failed(SharedString),
}

impl MathCache {
    fn new(preview: &gpui_kit::Entity<TextViewState>) -> Self {
        Self {
            entries: Arc::default(),
            preview: preview.downgrade(),
        }
    }

    /// The prepared formula, starting its rendering when it is new.
    fn get(&self, formula: &Formula, font_size: f32, color: Hsla, cx: &mut App) -> Lookup {
        let rgba = Rgba::from(color);
        let key = (
            formula.tex.clone(),
            formula.style,
            font_size.to_bits(),
            u32::from(rgba),
        );
        let mut entries = self.entries.lock().expect("math cache poisoned");
        match entries.get(&key) {
            Some(MathEntry::Ready(math)) => return Lookup::Ready(math.clone()),
            Some(MathEntry::Pending) => return Lookup::Waiting,
            Some(MathEntry::Failed(error)) => return Lookup::Failed(error.clone()),
            None => {}
        }
        if entries.len() >= MATH_CACHE_LIMIT {
            entries.retain(|_, entry| matches!(entry, MathEntry::Pending));
        }
        entries.insert(key.clone(), MathEntry::Pending);
        drop(entries);

        let color = format!("#{:08x}", u32::from(rgba));
        let (tex, style) = (formula.tex.clone(), formula.style);
        let task = cx
            .background_executor()
            .spawn(async move { math::render(&tex, style, font_size, &color) });
        let cache = self.clone();
        cx.spawn(async move |cx| {
            let entry = match task.await {
                Ok(svg) => {
                    // Layout snaps sizes to whole pixels; round up so the
                    // formula is never clipped and its baseline, which sits
                    // at the very bottom of formulas without descenders,
                    // stays inside the box the inline layout validates.
                    let height = svg.height.ceil();
                    MathEntry::Ready(Arc::new(PreparedMath {
                        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.svg.into_bytes())),
                        width: svg.width.ceil(),
                        height,
                        baseline: svg.baseline.clamp(0., height),
                    }))
                }
                Err(error) => MathEntry::Failed(error.into()),
            };
            cache
                .entries
                .lock()
                .expect("math cache poisoned")
                .insert(key, entry);
            _ = cache
                .preview
                .update(cx, |preview, cx| preview.invalidate_inline_layout(cx));
        })
        .detach();
        Lookup::Waiting
    }
}

fn math_image(math: &PreparedMath) -> gpui_kit::Img {
    img(math.image.clone())
        .object_fit(ObjectFit::Contain)
        .flex_shrink_0()
        .w(px(math.width))
        .h(px(math.height))
}

/// The formula's source, shown while it renders or when it cannot.
fn math_source(tex: &str, color: Hsla, cx: &App) -> gpui_kit::Div {
    div()
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(color)
        .child(tex.trim().to_string())
}

/// Display math: `$$` fences, or a paragraph that is only `$$ … $$`.
struct BlockMathPlugin {
    cache: MathCache,
}

pub(crate) fn display_math_source(source: &str) -> Option<&str> {
    let body = source.trim().strip_prefix("$$")?.strip_suffix("$$")?.trim();
    (!body.is_empty()).then_some(body)
}

impl MarkdownPlugin for BlockMathPlugin {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "math"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        match node {
            Node::Math(math) => Some(formula_node(
                &math.value,
                MathStyle::Display,
                cx.node_source(node).unwrap_or(&math.value),
            )),
            Node::Paragraph(_) => {
                let source = cx.node_source(node)?;
                let tex = display_math_source(source)?;
                Some(formula_node(tex, MathStyle::Display, source))
            }
            _ => None,
        }
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let formula = node.data::<Formula>().expect("math node data");
        let font_size = f32::from(window.text_style().font_size.to_pixels(window.rem_size()));
        let font_size = (font_size * DISPLAY_MATH_SCALE).round();
        let foreground = cx.theme().foreground;

        let content = match self.cache.get(formula, font_size, foreground, cx) {
            Lookup::Ready(math) => math_image(&math).into_any_element(),
            Lookup::Waiting => {
                math_source(&formula.tex, cx.theme().muted_foreground, cx).into_any_element()
            }
            Lookup::Failed(error) => v_flex()
                .items_center()
                .gap_1()
                .child(math_source(&formula.tex, cx.theme().danger, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(error),
                )
                .into_any_element(),
        };

        div()
            .id(SharedString::from(format!(
                "math-{}",
                content_hash(&formula.tex)
            )))
            .w_full()
            .flex()
            .justify_center()
            .py_1()
            .overflow_x_scroll()
            .child(content)
    }
}

/// Inline math: `$…$`.
struct InlineMathPlugin {
    cache: MathCache,
}

/// Whether an inline `$…$` span is math. Like GitHub, prose such as
/// "costs $5 and $10" is not: the content may not start or end with
/// whitespace, and the closing dollar may not be followed by a digit.
pub(crate) fn is_inline_math(value: &str, following: Option<char>) -> bool {
    !value.is_empty()
        && !value.starts_with(char::is_whitespace)
        && !value.ends_with(char::is_whitespace)
        && !following.is_some_and(|ch| ch.is_ascii_digit())
}

impl MarkdownPlugin for InlineMathPlugin {
    fn name(&self) -> &str {
        "inline-math"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else {
            return None;
        };
        let end = node.position()?.end.offset;
        let following = cx.source().get(end..).and_then(|rest| rest.chars().next());
        if !is_inline_math(&math.value, following) {
            return None;
        }
        let source = cx.node_source(node).unwrap_or(&math.value);
        Some(formula_node(&math.value, MathStyle::Inline, source))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        let font_size = (f32::from(context.font_size()) * INLINE_MATH_SCALE).round();
        let color = context.text_style().color;
        let lookup = self.cache.get(formula, font_size, color, cx);
        match lookup {
            Lookup::Ready(math) => {
                Some(InlineElement::new(math_image(&math)).with_baseline(px(math.baseline)))
            }
            // The preview shows the node's text until the formula is ready.
            Lookup::Waiting | Lookup::Failed(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_alerts() {
        let (kind, body) = parse_alert("> [!WARNING]\n> Back up first.\n>\n> - Really.").unwrap();
        assert_eq!(kind, AlertKind::Warning);
        assert_eq!(body, "Back up first.\n\n- Really.");

        assert_eq!(
            parse_alert("> [!tip]").unwrap(),
            (AlertKind::Tip, String::new())
        );
        // The marker must stand alone on the first line.
        assert!(parse_alert("> [!NOTE] inline text").is_none());
        assert!(parse_alert("> Just a quote").is_none());
        assert!(parse_alert("> [!UNKNOWN]\n> text").is_none());
    }

    #[test]
    fn tells_math_from_prices() {
        assert!(is_inline_math("x^2", Some(' ')));
        assert!(!is_inline_math("5 and ", Some('1')));
        assert!(!is_inline_math(" x", None));
        assert!(!is_inline_math("x", Some('0')));
        assert_eq!(display_math_source("$$ a + b $$"), Some("a + b"));
        assert_eq!(display_math_source("$$ $$"), None);
    }
}
