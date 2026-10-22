//! Markdown of replies, thinking and messages: links open in Rainy or the browser, and LaTeX math is drawn as formulas
//! (KaTeX with `remark-math` in 3.x).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext, MarkdownPlugin, TextView, markdown_ast::Node,
};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;
use latex_rust::{Color, Dim, MathFont, MathStyle, SvgOptions};

/// Markdown text as the chat shows it: selectable, with links and math.
pub fn markdown(id: impl Into<ElementId>, text: &SharedString) -> TextView {
    let text = match normalize_math(text) {
        Cow::Borrowed(_) => text.clone(),
        Cow::Owned(converted) => converted.into(),
    };
    TextView::markdown(id, text)
        .markdown_extensions(extensions().clone())
        .selectable(true)
        .on_link_click(|url, _, window, cx| crate::ui::open_link(url, window, cx))
}

fn extensions() -> &'static MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS.get_or_init(|| MarkdownExtensions::default().plugin(InlineMath).plugin(DisplayMath))
}

/// `$…$` inside a paragraph.
struct InlineMath;

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &str {
        "math"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else { return None };
        let source = cx.node_source(node)?;
        let after = cx.source().get(node.position()?.end.offset..).and_then(|rest| rest.chars().next());
        // Copying a formula copies what was written.
        is_formula(source, &math.value, after).then(|| MarkdownNode::new("math", math.value.clone()).text(source).markdown(source))
    }

    fn render_inline(&self, node: &MarkdownNode, context: &InlineRenderContext, window: &mut Window, _: &mut App) -> Option<InlineElement> {
        let formula = formula(node.data::<String>()?, false, f32::from(context.font_size()), context.text_style().color, window.scale_factor())?;
        let image = img(formula.image.clone()).w(px(formula.width)).h(px(formula.height));
        Some(InlineElement::new(image).with_baseline(px(formula.baseline)))
    }
}

/// A `$$` block.
struct DisplayMath;

impl MarkdownPlugin for DisplayMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "math-block"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Math(math) = node else { return None };
        let source = cx.node_source(node).unwrap_or(&math.value);
        Some(MarkdownNode::new("math-block", math.value.clone()).text(math.value.clone()).markdown(source))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let latex = node.data::<String>().cloned().unwrap_or_default();
        let id = ElementId::Name(format!("math-{}", node.source_range().map_or(0, |range| range.start)).into());
        let size = f32::from(window.rem_size()) * 1.05;
        match formula(&latex, true, size, cx.theme().foreground, window.scale_factor()) {
            // Wide formulas scroll sideways instead of overflowing the reply.
            Some(formula) => div()
                .id(id)
                .w_full()
                .overflow_x_scroll()
                .py_1()
                .child(h_flex().min_w_full().justify_center().child(img(formula.image.clone()).flex_none().w(px(formula.width)).h(px(formula.height))))
                .into_any_element(),
            // What the renderer does not know stays readable as its source.
            None => div()
                .id(id)
                .w_full()
                .overflow_x_scroll()
                .py_1()
                .font_family(cx.theme().mono_font_family.clone())
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(latex)
                .into_any_element(),
        }
    }
}

/// Whether `$…$` is a formula rather than prose with dollar amounts: as in Pandoc, single dollars hug the formula
/// and are not followed by a digit, so "$5 and $10" stays text.
fn is_formula(source: &str, value: &str, after: Option<char>) -> bool {
    if value.trim().is_empty() || after.is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    source.starts_with("$$") || !(value.starts_with(char::is_whitespace) || value.ends_with(char::is_whitespace))
}

/// A drawn formula; sizes are in logical pixels.
struct Formula {
    image: Arc<Image>,
    width: f32,
    height: f32,
    /// Distance from the top to the baseline.
    baseline: f32,
}

type Key = (String, bool, u32, u32, u32);

/// Formulas drawn so far, by source, style, size, color and screen scale; `None` when the source is not supported.
fn formula(latex: &str, display: bool, font_size: f32, color: Hsla, scale: f32) -> Option<Arc<Formula>> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<Arc<Formula>>>>> = OnceLock::new();
    let rgba = Rgba::from(color);
    let packed = u32::from_be_bytes([rgba.r, rgba.g, rgba.b, rgba.a].map(|channel| (channel * 255.0).round() as u8));
    let key = (latex.to_owned(), display, font_size.to_bits(), packed, scale.to_bits());
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(found) = cache.get(&key) {
        return found.clone();
    }
    let drawn = draw(latex, display, font_size, rgba, scale).map(Arc::new);
    if cache.len() >= 2048 {
        cache.clear();
    }
    cache.insert(key, drawn.clone());
    drawn
}

fn math_font() -> Option<&'static MathFont> {
    static FONT: OnceLock<Option<MathFont>> = OnceLock::new();
    FONT.get_or_init(|| MathFont::stix_two_math().ok()).as_ref()
}

fn draw(latex: &str, display: bool, font_size: f32, color: Rgba, scale: f32) -> Option<Formula> {
    let font = math_font()?;
    let (source, missing) = substitute(&rewrite(latex), font);
    let ast = latex_rust::parse(&source).ok()?;
    // Drawn at the screen's pixel size, so the image is sharp at any scaling.
    let device = font_size * scale;
    let em = Dim::ratio((device * 64.0).round() as i64, 64);
    let style = if display { MathStyle::Display } else { MathStyle::Text };
    let tree = latex_rust::layout_with_em_size_pt(&ast, font, style, &em).ok()?;
    let channel = |value: f32| (value * 255.0).round() as u8;
    let options = SvgOptions { font_size_pt: em, color: Color::rgb(channel(color.r), channel(color.g), channel(color.b)), display };
    let svg = latex_rust::render_svg(&tree, font, &options).ok()?;
    let body = &svg[svg.find("<g fill=")?..];
    let body = if missing.is_empty() { Cow::Borrowed(body) } else { Cow::Owned(overlay(body, &missing, font)?) };
    let to_em = |dim: &Dim| f32::from_bits(dim.to_ieee32_bits());
    let (width, height, depth) = (to_em(&tree.width), to_em(&tree.height), to_em(&tree.depth));
    // Room above for system-font characters, which reach higher than the boxes they replace.
    let pad = if missing.is_empty() { 0.0 } else { 0.12 };
    let (view_width, view_height) = (width * device, (pad + height + depth) * device);
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{view_width}" height="{view_height}" viewBox="0 {} {view_width} {view_height}">{}"#,
        -pad * device,
        body.replacen("<g fill=", &format!("<g fill-opacity=\"{:.3}\" fill=", color.a), 1)
    );
    (width * font_size > 0.5 && (height + depth) * font_size > 0.5).then(|| Formula {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width: width * font_size,
        height: (pad + height + depth) * font_size,
        baseline: (pad + height) * font_size,
    })
}

/// LaTeX the renderer does not know, written as what it does: starred environments as plain ones, `\pmod{n}`
/// spelled out, size switches dropped.
fn rewrite(latex: &str) -> String {
    let mut out = String::with_capacity(latex.len());
    let mut rest = latex;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let length = after.bytes().take_while(u8::is_ascii_alphabetic).count();
        if length == 0 {
            // `\\`, `\{` and the like stay as they are.
            let symbol = after.chars().next().map_or(0, char::len_utf8);
            out.push_str(&rest[at..at + 1 + symbol]);
            rest = &after[symbol..];
            continue;
        }
        let name = &after[..length];
        let mut tail = &after[length..];
        match name {
            "bm" => out.push_str("\\boldsymbol"),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "degree" => out.push_str("{}^{\\circ}"),
            "bmod" => out.push_str("\\;\\operatorname{mod}\\;"),
            "mod" => out.push_str("\\quad\\operatorname{mod}\\;"),
            "mathop" => out.push_str("\\operatorname"),
            "operatorname" => {
                out.push_str("\\operatorname");
                tail = tail.strip_prefix('*').unwrap_or(tail);
            }
            "pmod" => match group(tail) {
                Some((argument, next)) => {
                    out.push_str("\\quad(\\operatorname{mod}\\;");
                    out.push_str(argument);
                    out.push(')');
                    tail = next;
                }
                None => out.push_str("\\pmod"),
            },
            "tiny" | "scriptsize" | "footnotesize" | "small" | "normalsize" | "large" | "Large" | "LARGE" | "huge" | "Huge" => {}
            "begin" | "end" => match group(tail) {
                Some((environment, next)) => {
                    let begin = name == "begin";
                    let plain = environment.trim_end_matches('*');
                    tail = next;
                    match plain {
                        "alignat" | "alignedat" => {
                            out.push_str(if begin { "\\begin{aligned}" } else { "\\end{aligned}" });
                            if begin && let Some((_, next)) = group(tail) {
                                tail = next;
                            }
                        }
                        "eqnarray" => out.push_str(if begin { "\\begin{array}{rcl}" } else { "\\end{array}" }),
                        "rcases" => out.push_str(if begin { "\\left.\\begin{array}{ll}" } else { "\\end{array}\\right\\}" }),
                        _ => {
                            let known = match plain {
                                "gathered" => "gather",
                                "smallmatrix" => "matrix",
                                "dcases" => "cases",
                                "align" | "equation" | "gather" | "multline" => plain,
                                _ => environment,
                            };
                            out.push_str(&format!("\\{name}{{{known}}}"));
                        }
                    }
                }
                None => out.push_str(&rest[at..at + 1 + length]),
            },
            _ => out.push_str(&rest[at..at + 1 + length]),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The `{…}` group at the start of `text`, and what follows it.
fn group(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_prefix('{')?;
    let mut depth = 1;
    let mut escaped = false;
    for (index, c) in inner.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&inner[..index], &inner[index + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Boxes from the math font that stand in for characters it lacks; each one is told apart by its outline.
const PLACEHOLDERS: [char; 10] = ['\u{25A0}', '\u{25A1}', '\u{25A2}', '\u{25A3}', '\u{25A4}', '\u{25A5}', '\u{25A6}', '\u{25A7}', '\u{25A8}', '\u{25A9}'];

/// The system fonts that draw what the math font lacks, mostly Chinese in `\text{}`.
const TEXT_FONTS: &str = "Microsoft YaHei UI, Microsoft YaHei, PingFang SC, Noto Sans CJK SC, sans-serif";

/// `latex` with every character the math font lacks replaced by a placeholder box, and those characters in order.
fn substitute(latex: &str, font: &MathFont) -> (String, Vec<char>) {
    if latex.chars().any(|c| PLACEHOLDERS.contains(&c)) {
        return (latex.to_owned(), Vec::new());
    }
    let mut missing = Vec::new();
    let source = latex
        .chars()
        .map(|c| {
            if c.is_ascii() || font.glyph(c).is_ok() {
                return c;
            }
            missing.push(c);
            PLACEHOLDERS[(missing.len() - 1) % PLACEHOLDERS.len()]
        })
        .collect();
    (source, missing)
}

/// The SVG body with each placeholder box replaced by its character in a system font, on the same baseline and
/// as wide as the box.
fn overlay(body: &str, missing: &[char], font: &MathFont) -> Option<String> {
    static OUTLINES: OnceLock<Option<Vec<String>>> = OnceLock::new();
    let outlines = OUTLINES
        .get_or_init(|| {
            PLACEHOLDERS
                .iter()
                .map(|c| {
                    let svg = latex_rust::latex_to_svg(&c.to_string(), font, &SvgOptions::new()).ok()?;
                    let start = svg.find("<path d=\"")? + 9;
                    Some(svg[start..start + svg[start..].find('"')?].to_owned())
                })
                .collect()
        })
        .as_ref()?;
    let advance = f32::from(font.glyph(PLACEHOLDERS[0]).ok()?.advance_fu);
    let mut queues: Vec<std::collections::VecDeque<char>> = vec![Default::default(); PLACEHOLDERS.len()];
    for (index, c) in missing.iter().enumerate() {
        queues[index % PLACEHOLDERS.len()].push_back(*c);
    }
    let mut out = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("<path d=\"")
            && let Some(end) = rest.find('"')
            && let Some(slot) = outlines.iter().position(|outline| *outline == rest[..end])
            && let Some((x, y, scale)) = placement(&rest[end..])
            && let Some(c) = queues[slot].pop_front()
        {
            let escaped = match c {
                '&' => "&amp;".to_owned(),
                '<' => "&lt;".to_owned(),
                other => other.to_string(),
            };
            out.push_str(&format!("<text x=\"{x}\" y=\"{y}\" font-size=\"{}\" font-family=\"{TEXT_FONTS}\">{escaped}</text>\n", scale * advance));
            continue;
        }
        out.push_str(line);
    }
    Some(out)
}

/// Origin and scale of a glyph from its ` transform="translate(x y) scale(s -s)"`.
fn placement(attributes: &str) -> Option<(f32, f32, f32)> {
    let translate = &attributes[attributes.find("translate(")? + 10..];
    let (x, rest) = translate.split_once(' ')?;
    let (y, rest) = rest.split_once(')')?;
    let scale = &rest[rest.find("scale(")? + 6..];
    let (scale, _) = scale.split_once(' ')?;
    Some((x.parse().ok()?, y.parse().ok()?, scale.parse().ok()?))
}

/// The delimiters many models write, `\( \)` and `\[ \]`, as the `$` and `$$` that Markdown math uses; code is left alone.
pub fn normalize_math(text: &str) -> Cow<'_, str> {
    if !text.contains("\\(") && !text.contains("\\[") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut fence: Option<(u8, usize)> = None;
    let (mut start, mut offset) = (0, 0);
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.bytes().next().filter(|c| *c == b'`' || *c == b'~');
        let run = marker.map_or(0, |c| trimmed.bytes().take_while(|x| *x == c).count());
        match (fence, marker) {
            (None, Some(c)) if run >= 3 => {
                out.push_str(&convert(&text[start..offset]));
                fence = Some((c, run));
                start = offset;
            }
            (Some((open, length)), Some(c)) if c == open && run >= length && trimmed[run..].trim().is_empty() => {
                out.push_str(&text[start..offset + line.len()]);
                fence = None;
                start = offset + line.len();
            }
            _ => {}
        }
        offset += line.len();
    }
    match fence {
        None => out.push_str(&convert(&text[start..])),
        Some(_) => out.push_str(&text[start..]),
    }
    Cow::Owned(out)
}

/// `\( \)` and `\[ \]` in prose outside code spans, within one paragraph.
fn convert(chunk: &str) -> Cow<'_, str> {
    let bytes = chunk.as_bytes();
    let mut out = String::new();
    let (mut index, mut copied) = (0, 0);
    while index < bytes.len() {
        match bytes[index] {
            b'`' => {
                let run = bytes[index..].iter().take_while(|b| **b == b'`').count();
                let mut search = index + run;
                let mut close = None;
                while let Some(found) = chunk[search..].find(&chunk[index..index + run]) {
                    let at = search + found;
                    let length = bytes[at..].iter().take_while(|b| **b == b'`').count();
                    if length == run {
                        close = Some(at + run);
                        break;
                    }
                    search = at + length;
                }
                index = close.unwrap_or(index + run);
            }
            b'\\' if matches!(bytes.get(index + 1), Some(b'(' | b'[')) => {
                let display = bytes[index + 1] == b'[';
                let body = index + 2;
                let limit = chunk[body..].find("\n\n").map_or(chunk.len(), |at| body + at);
                let found = chunk[body..limit].find(if display { "\\]" } else { "\\)" });
                let Some(end) = found.map(|at| body + at).filter(|end| !chunk[body..*end].trim().is_empty()) else {
                    index = body;
                    continue;
                };
                let inner = chunk[body..end].trim();
                out.push_str(&chunk[copied..index]);
                let line_start = chunk[..index].rfind('\n').map_or(0, |at| at + 1);
                let indent = &chunk[line_start..index];
                let line_end = chunk[end + 2..].find('\n').map_or(chunk.len(), |at| end + 2 + at);
                if display && indent.trim().is_empty() && chunk[end + 2..line_end].trim().is_empty() {
                    out.push_str("$$\n");
                    for line in inner.lines() {
                        out.push_str(indent);
                        out.push_str(line.trim_start());
                        out.push('\n');
                    }
                    out.push_str(indent);
                    out.push_str("$$");
                } else {
                    out.push('$');
                    out.push_str(inner);
                    out.push('$');
                }
                index = end + 2;
                copied = index;
            }
            // An escaped character, such as the `\\` before `[2pt]` in a matrix.
            b'\\' => index += 2,
            _ => index += 1,
        }
    }
    if copied == 0 {
        return Cow::Borrowed(chunk);
    }
    out.push_str(&chunk[copied..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn delimiters() {
        assert_eq!(normalize_math(r"能量 \(E=mc^2\) 守恒"), "能量 $E=mc^2$ 守恒");
        assert_eq!(normalize_math("前\n\\[\n  \\frac{a}{b}\n\\]\n后"), "前\n$$\n\\frac{a}{b}\n$$\n后");
        assert_eq!(normalize_math(r"行内 \[x\] 公式"), "行内 $x$ 公式");
        assert_eq!(normalize_math("`\\(x\\)` 和\n```\n\\[y\\]\n```\n"), "`\\(x\\)` 和\n```\n\\[y\\]\n```\n");
        assert_eq!(normalize_math(r"$$a \\[2pt] b$$"), r"$$a \\[2pt] b$$");
        assert_eq!(normalize_math(r"未闭合 \(x"), r"未闭合 \(x");
    }

    #[test]
    fn dollar_amounts_stay_text() {
        assert!(is_formula("$x^2$", "x^2", Some(' ')));
        assert!(!is_formula("$5 and $", "5 and ", None));
        assert!(!is_formula("$5-$", "5-", Some('1')));
        assert!(is_formula("$$ x $$", " x ", None));
    }

    #[test]
    fn formulas_draw() {
        let white = Hsla::white();
        for latex in [r"e^{i\pi}+1=0", r"\frac{a}{b}", r"\sum_{i=1}^{n} i^2", r"\int_0^\infty e^{-x}\,dx", r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}", r"\sqrt{x}", r"\mathbb{R}", r"\text{speed}=\frac{s}{t}"] {
            let drawn = formula(latex, false, 14.0, white, 2.25).unwrap_or_else(|| panic!("{latex}"));
            assert!(drawn.width > 1.0 && drawn.baseline > 1.0, "{latex}");
        }
        assert!(formula(r"\frac{", false, 14.0, white, 1.0).is_none());
        assert!(formula(r"\ce{H2O}", false, 14.0, white, 1.0).is_none());
    }

    #[test]
    fn common_latex_and_chinese_draw() {
        let white = Hsla::white();
        for latex in [
            r"\begin{align*}a&=b\\c&=d\end{align*}",
            r"\begin{gathered}a\\b\end{gathered}",
            r"\begin{equation*}a=b\end{equation*}",
            r"\begin{smallmatrix}a&b\end{smallmatrix}",
            r"\begin{alignat}{2}a&=b\end{alignat}",
            r"\begin{eqnarray}a&=&b\end{eqnarray}",
            r"\begin{dcases}a&b\\c&d\end{dcases}",
            r"\begin{rcases}a&b\end{rcases}",
            r"a \bmod b + c \mod d",
            r"x \equiv 1 \pmod{3}",
            r"\bm{x} \lt \gt 37\degree",
            r"\mathop{lim} \operatorname*{arg\,max}_x f",
            r"\Large x \small y",
            r"\frac{\text{路程}}{\text{时间}}",
            r"速度 = v",
            r"x_{\text{最大}}^{\text{上}} \text{（括号）}",
        ] {
            assert!(formula(latex, false, 14.0, white, 1.0).is_some(), "{latex}");
        }
        assert_eq!(rewrite(r"\\[2pt] \models \pmod{n}"), r"\\[2pt] \models \quad(\operatorname{mod}\;n)");
    }

    #[test]
    fn chinese_is_drawn_as_text_in_place() {
        let font = math_font().unwrap();
        let (source, missing) = substitute(r"\text{路程}+x", font);
        assert_eq!(missing, ['路', '程']);
        let svg = latex_rust::latex_to_svg(&source, font, &SvgOptions::new()).unwrap();
        let body = overlay(&svg[svg.find("<g fill=").unwrap()..], &missing, font).unwrap();
        assert!(body.contains(">路</text>") && body.contains(">程</text>"));
        assert!(body.find(">路<").unwrap() < body.find(">程<").unwrap());
    }
}
