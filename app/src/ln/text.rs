//! Turns a chapter's HTML into paragraphs with bold and italic ranges. Chapters aren't Markdown, so `*` and `-` stay as typed.

use crate::rich::Block;

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "times" => '×',
        "copy" => '©',
        "deg" => '°',
        _ => return None,
    })
}

/// Builds one paragraph at a time, collapsing whitespace the way a browser would.
#[derive(Default)]
struct Builder {
    out: Vec<Block>,
    cur: Block,
    bold: Option<usize>,
    italic: Option<usize>,
}

impl Builder {
    fn text(&mut self, s: &str) {
        for c in s.chars() {
            if c.is_whitespace() || c == '\u{feff}' || c == '\u{200b}' {
                if !self.cur.text.is_empty() && !self.cur.text.ends_with(' ') {
                    self.cur.text.push(' ');
                }
            } else {
                self.cur.text.push(c);
            }
        }
    }

    fn style(&mut self, on: bool, italic: bool) {
        let at = self.cur.text.len();
        let (open, ranges) = if italic { (&mut self.italic, &mut self.cur.italic) } else { (&mut self.bold, &mut self.cur.bold) };
        match (on, open.take()) {
            (true, None) => *open = Some(at),
            (false, Some(start)) if start < at => ranges.push(start..at),
            _ => {}
        }
        if on && open.is_none() {
            *open = Some(at);
        }
    }

    /// Ends the paragraph. Styles still open carry on into the next one.
    fn flush(&mut self) {
        let (bold, italic) = (self.bold.is_some(), self.italic.is_some());
        if bold {
            self.style(false, false);
        }
        if italic {
            self.style(false, true);
        }
        let trimmed = self.cur.text.trim_end().len();
        self.cur.text.truncate(trimmed);
        for r in self.cur.bold.iter_mut().chain(self.cur.italic.iter_mut()) {
            r.end = r.end.min(trimmed);
        }
        self.cur.bold.retain(|r| r.start < r.end);
        self.cur.italic.retain(|r| r.start < r.end);
        if !self.cur.text.is_empty() {
            self.out.push(std::mem::take(&mut self.cur));
        }
        self.cur = Block::default();
        self.bold = bold.then_some(0);
        self.italic = italic.then_some(0);
    }
}

pub fn parse(src: &str) -> Vec<Block> {
    if !src.contains('<') {
        return src.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| Block { text: l.to_string(), ..Default::default() }).collect();
    }
    let mut b = Builder::default();
    let mut skip: Option<String> = None;
    let mut rest = src;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip.is_none() {
                text_with_entities(&mut b, rest);
            }
            break;
        };
        if skip.is_none() {
            text_with_entities(&mut b, &rest[..lt]);
        }
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |e| &after[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
        if let Some(until) = &skip {
            if closing && name == *until {
                skip = None;
            }
            continue;
        }
        match name.as_str() {
            "script" | "style" | "head" | "noscript" if !closing => skip = Some(name),
            "b" | "strong" => b.style(!closing, false),
            "i" | "em" | "cite" => b.style(!closing, true),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                b.flush();
                b.style(!closing, false);
            }
            "hr" => {
                b.flush();
                b.cur.text = "* * *".into();
                b.flush();
            }
            "br" | "p" | "div" | "li" | "blockquote" | "tr" | "section" | "article" | "pre" | "ul" | "ol" | "table" => b.flush(),
            _ => {}
        }
    }
    b.flush();
    b.out
}

fn text_with_entities(b: &mut Builder, s: &str) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        b.text(&rest[..amp]);
        let after = &rest[amp + 1..];
        match after.find(';').filter(|e| *e <= 10).and_then(|e| entity(&after[..e]).map(|c| (c, e))) {
            Some((c, e)) => {
                b.text(&c.to_string());
                rest = &after[e + 1..];
            }
            None => {
                b.text("&");
                rest = after;
            }
        }
    }
    b.text(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<String> {
        parse(src).into_iter().map(|b| b.text).collect()
    }

    #[test]
    fn paragraphs_and_entities() {
        assert_eq!(texts("<p>He said &ldquo;hi&rdquo; &amp; left&#8217;s</p><p>  Two\n lines </p>"), ["He said “hi” & left’s", "Two lines"]);
        assert_eq!(texts("<div>a<br><br>b</div>"), ["a", "b"]);
        assert_eq!(texts("A &bogus; B &"), ["A &bogus; B &"]);
    }

    #[test]
    fn asterisks_and_dashes_stay() {
        assert_eq!(texts("<p>- not a bullet</p><p>**stars** * 2</p>"), ["- not a bullet", "**stars** * 2"]);
        assert_eq!(texts("- plain\n* text\n\nlast"), ["- plain", "* text", "last"]);
    }

    #[test]
    fn styles_and_breaks() {
        let b = parse("<p>a <b>bold</b> <i>it</i></p><hr><h2>Title</h2>");
        assert_eq!(b[0].text, "a bold it");
        assert_eq!(b[0].bold, vec![2..6]);
        assert_eq!(b[0].italic, vec![7..9]);
        assert_eq!(b[1].text, "* * *");
        assert_eq!(b[2].bold, vec![0..5]);
    }

    #[test]
    fn style_spanning_paragraphs_and_junk() {
        let b = parse("<b><p>one</p><p>two</p></b><script>var x = '<p>no</p>';</script><style>p{}</style><!-- c --><p>end</p>");
        assert_eq!(b.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["one", "two", "end"]);
        assert_eq!(b[0].bold, vec![0..3]);
        assert_eq!(b[1].bold, vec![0..3]);
        assert!(b[2].bold.is_empty());
    }
}
