//! Series descriptions come as plain text, Markdown or HTML. This renders the common parts and drops the rest.

use crate::{ACCENT_TEXT, TEXT, u};
use gpui::{
    FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement, ParentElement, SharedString, Styled, StyledText,
    UnderlineStyle, div, prelude::FluentBuilder, px, rgb,
};
use std::ops::Range;

#[derive(Debug, Default, PartialEq)]
pub struct Block {
    pub text: String,
    pub bullet: bool,
    /// A blank line came before it.
    pub gap: bool,
    pub bold: Vec<Range<usize>>,
    pub italic: Vec<Range<usize>>,
    pub links: Vec<(Range<usize>, String)>,
}

/// Turns HTML into the bit of Markdown `parse` reads, and decodes entities.
fn html_to_markdown(src: &str) -> String {
    let mut out = String::new();
    let mut href: Vec<String> = vec![];
    let mut rest = src;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let is_tag = after.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!');
        let Some(end) = after.find('>').filter(|_| is_tag) else {
            out.push('<');
            rest = after;
            continue;
        };
        let tag = &after[..end];
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
        let closing = tag.starts_with('/');
        match (name.as_str(), closing) {
            ("br" | "p" | "div" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4", _) => out.push('\n'),
            ("li", false) => out.push_str("\n- "),
            ("b" | "strong", _) => out.push_str("**"),
            ("i" | "em", _) => out.push('*'),
            ("a", false) => {
                let url = tag.split("href=").nth(1).map(|v| {
                    let v = v.trim_start_matches(['"', '\'']);
                    v[..v.find(['"', '\'', ' ']).unwrap_or(v.len())].to_string()
                });
                href.push(url.unwrap_or_default());
                out.push('[');
            }
            ("a", true) => {
                let url = href.pop().unwrap_or_default();
                out.push_str(&format!("]({url})"));
            }
            _ => {}
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

fn looks_like_html(s: &str) -> bool {
    ["<br", "<p", "<b>", "<i>", "<a ", "<div", "<li", "<strong", "<em", "</"].iter().any(|t| s.to_ascii_lowercase().contains(t))
}

/// Bold, italic, links and bare URLs within one line.
fn inline(line: &str, b: &mut Block) {
    let (mut bold, mut italic): (Option<usize>, Option<usize>) = (None, None);
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        if rest.starts_with('\\') && rest.len() > 1 {
            let c = rest[1..].chars().next().unwrap();
            b.text.push(c);
            i += 1 + c.len_utf8();
        } else if rest.starts_with("**") || rest.starts_with("__") {
            match bold.take() {
                Some(s) => b.bold.push(s..b.text.len()),
                None => bold = Some(b.text.len()),
            }
            i += 2;
        } else if rest.starts_with('*') && (italic.is_some() || !rest[1..].starts_with(' ')) {
            match italic.take() {
                Some(s) => b.italic.push(s..b.text.len()),
                None => italic = Some(b.text.len()),
            }
            i += 1;
        } else if let Some((text, url, used)) = link(rest) {
            let s = b.text.len();
            b.text.push_str(text);
            b.links.push((s..b.text.len(), url.to_string()));
            i += used;
        } else if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ')', ';']);
            let s = b.text.len();
            b.text.push_str(url);
            b.links.push((s..b.text.len(), url.to_string()));
            i += url.len();
        } else {
            let c = rest.chars().next().unwrap();
            b.text.push(c);
            i += c.len_utf8();
        }
    }
    // An unclosed marker formats to the end of the line.
    if let Some(s) = bold.filter(|s| *s < b.text.len()) {
        b.bold.push(s..b.text.len());
    }
    if let Some(s) = italic.filter(|s| *s < b.text.len()) {
        b.italic.push(s..b.text.len());
    }
}

/// Returns (text, url, bytes used).
fn link(s: &str) -> Option<(&str, &str, usize)> {
    let close = s.strip_prefix('[')?.find("](")? + 1;
    let url_end = s[close + 2..].find(')')? + close + 2;
    Some((&s[1..close], &s[close + 2..url_end], url_end + 1))
}

pub fn parse(src: &str) -> Vec<Block> {
    let md = if looks_like_html(src) { html_to_markdown(src) } else { src.replace("\r\n", "\n") };
    let mut blocks = vec![];
    let mut gap = false;
    for line in md.lines().map(str::trim) {
        if line.is_empty() {
            gap = !blocks.is_empty();
            continue;
        }
        let (bullet, body) = match line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).or_else(|| line.strip_prefix("• ")) {
            Some(b) => (true, b),
            None => (false, line.trim_start_matches('#').trim_start()),
        };
        let mut b = Block { bullet, gap, ..Default::default() };
        inline(body, &mut b);
        if line.starts_with('#') {
            b.bold.push(0..b.text.len()); // a heading
        }
        gap = false;
        if !b.text.trim().is_empty() {
            blocks.push(b);
        }
    }
    blocks
}

/// GPUI wants highlight ranges in order and not overlapping, so split at every edge.
fn highlights(b: &Block) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut edges: Vec<usize> = b.bold.iter().chain(b.italic.iter()).chain(b.links.iter().map(|l| &l.0)).flat_map(|r| [r.start, r.end]).collect();
    edges.sort();
    edges.dedup();
    let link_ranges: Vec<Range<usize>> = b.links.iter().map(|l| l.0.clone()).collect();
    let within = |rs: &[Range<usize>], s: usize| rs.iter().any(|r| r.start <= s && s < r.end);
    edges
        .windows(2)
        .filter_map(|w| {
            let (s, e) = (w[0], w[1]);
            let (bold, italic, link) = (within(&b.bold, s), within(&b.italic, s), within(&link_ranges, s));
            (bold || italic || link).then(|| {
                let mut h = HighlightStyle::default();
                if bold {
                    h.font_weight = Some(FontWeight::SEMIBOLD);
                    h.color = Some(rgb(TEXT).into());
                }
                if italic {
                    h.font_style = Some(FontStyle::Italic);
                }
                if link {
                    h.color = Some(rgb(ACCENT_TEXT).into());
                    h.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(rgb(ACCENT_TEXT).into()), wavy: false });
                }
                (s..e, h)
            })
        })
        .collect()
}

fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// True when the paragraph is mostly right-to-left letters.
fn is_rtl_block(s: &str) -> bool {
    let (mut rtl, mut other) = (0usize, 0usize);
    for c in s.chars().filter(|c| c.is_alphabetic()) {
        if is_rtl_char(c) { rtl += 1 } else { other += 1 }
    }
    rtl > 0 && rtl >= other
}

/// Byte ranges of visual tokens: whitespace-separated words, with adjacent Latin words merged.
fn rtl_tokens(s: &str) -> Vec<Range<usize>> {
    let mut words: Vec<Range<usize>> = vec![];
    let mut start = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(st) = start.take() {
                words.push(st..i);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        words.push(st..s.len());
    }
    let is_ltr = |r: &Range<usize>| {
        let w = &s[r.clone()];
        w.chars().any(|c| c.is_ascii_alphanumeric()) && !w.chars().any(is_rtl_char)
    };
    let mut out: Vec<Range<usize>> = vec![];
    let mut prev_ltr = false;
    for w in words {
        let ltr = is_ltr(&w);
        if ltr && prev_ltr {
            out.last_mut().unwrap().end = w.end;
        } else {
            out.push(w);
        }
        prev_ltr = ltr;
    }
    out
}

fn clip(rs: &[Range<usize>], r: &Range<usize>) -> Vec<Range<usize>> {
    rs.iter()
        .filter_map(|x| {
            let (s, e) = (x.start.max(r.start), x.end.min(r.end));
            (s < e).then(|| s - r.start..e - r.start)
        })
        .collect()
}

/// An RTL paragraph as short, separately shaped tokens laid out right-to-left,
/// so gpui never has to wrap RTL text itself.
fn render_rtl_block(block_ix: usize, b: &Block) -> impl IntoElement {
    let items: Vec<_> =
        rtl_tokens(&b.text)
            .into_iter()
            .enumerate()
            .map(|(ti, r)| {
                let sub = Block {
                    text: b.text[r.clone()].to_string(),
                    bold: clip(&b.bold, &r),
                    italic: clip(&b.italic, &r),
                    links: b
                        .links
                        .iter()
                        .filter_map(|(lr, url)| {
                            let (s, e) = (lr.start.max(r.start), lr.end.min(r.end));
                            (s < e).then(|| (s - r.start..e - r.start, url.clone()))
                        })
                        .collect(),
                    ..Default::default()
                };
                let urls: Vec<String> = sub.links.iter().map(|l| l.1.clone()).collect();
                let ranges: Vec<Range<usize>> = sub.links.iter().map(|l| l.0.clone()).collect();
                let text = StyledText::new(SharedString::from(sub.text.clone()))
                    .with_highlights(highlights(&sub));
                let text = InteractiveText::new(("desc-rtl", (block_ix << 16) | ti), text)
                    .on_click(ranges, move |ix, _, cx| {
                        let url = &urls[ix];
                        if url.starts_with("https://") || url.starts_with("http://") {
                            cx.open_url(url);
                        }
                    });
                div().child(text)
            })
            .collect();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_wrap()
        .flex_row_reverse()
        .gap_x(u(4.))
        .children(items)
}

pub fn render_blocks(blocks: &[Block]) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(u(2.))
        .children(blocks.iter().enumerate().map(|(i, b)| {
            let row = div().w_full().flex().when(b.gap, |d| d.mt(u(12.)));
            if is_rtl_block(&b.text) {
                let content = render_rtl_block(i, b);
                return if b.bullet {
                    row.flex_row_reverse()
                        .gap(u(8.))
                        .pr(u(4.))
                        .child("•")
                        .child(content)
                        .into_any_element()
                } else {
                    row.child(content).into_any_element()
                };
            }
            let urls: Vec<String> = b.links.iter().map(|l| l.1.clone()).collect();
            let ranges: Vec<Range<usize>> = b.links.iter().map(|l| l.0.clone()).collect();
            let text =
                StyledText::new(SharedString::from(b.text.clone())).with_highlights(highlights(b));
            let text =
                InteractiveText::new(("desc", i), text).on_click(ranges, move |ix, _, cx| {
                    let url = &urls[ix];
                    if url.starts_with("https://") || url.starts_with("http://") {
                        cx.open_url(url);
                    }
                });
            if b.bullet {
                row.gap(u(8.))
                    .pl(u(4.))
                    .child("•")
                    .child(div().flex_1().min_w_0().child(text))
                    .into_any_element()
            } else {
                row.child(div().flex_1().min_w_0().child(text))
                    .into_any_element()
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtl_detection() {
        assert!(is_rtl_block("حكاية الفارس الأسود الغامض غاتس."));
        assert!(is_rtl_block("قصة مانجا Blue Lock في اليابان"));
        assert!(!is_rtl_block(
            "After a disastrous defeat at the 2018 World Cup."
        ));
    }

    #[test]
    fn rtl_tokens_keep_latin_together() {
        let s = "قصة مانجا Blue Lock (2018) في";
        let t: Vec<&str> = rtl_tokens(s).into_iter().map(|r| &s[r]).collect();
        assert_eq!(t, ["قصة", "مانجا", "Blue Lock (2018)", "في"]);
    }

    #[test]
    fn markdown() {
        let b = parse(
            "Intro *soft* text.\n\n**Source:**\n- [Webtoon](https://x.moe/s) and https://a.b/c.",
        );
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].text, "Intro soft text.");
        assert_eq!(b[0].italic, vec![6..10]);
        assert!(b[1].gap && b[1].text == "Source:" && b[1].bold == vec![0..7]);
        assert!(b[2].bullet && b[2].text == "Webtoon and https://a.b/c.");
        assert_eq!(
            b[2].links,
            vec![
                (0..7, "https://x.moe/s".into()),
                (12..25, "https://a.b/c".into())
            ]
        );
        assert_eq!(parse("2 * 3 = 6")[0].text, "2 * 3 = 6");
    }

    #[test]
    fn html() {
        let b = parse(
            "<p>A &amp; <b>B</b><br>See <a href=\"https://x.y\">site</a></p><ul><li>one</li></ul>",
        );
        assert_eq!(
            b.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
            ["A & B", "See site", "one"]
        );
        assert_eq!(b[0].bold, vec![4..5]);
        assert_eq!(b[1].links, vec![(4..8, "https://x.y".into())]);
        assert!(b[2].bullet);
    }
}
