//! A small, dependency-free HTML to plain text projection.
//!
//! Used by the `convert_html_to_plaintext` content filter: Mailman shells out
//! to `lynx -dump`; this keeps the same intent (readable text, links shown
//! after their anchor text, block structure as blank lines) inside the
//! runtime.
//!
//! The input is untrusted mail. Nothing here allocates more than a small
//! multiple of the input and no external process runs.

/// Elements whose content is never text for a reader.
const SKIPPED: &[&str] = &["script", "style", "head", "title", "noscript", "template"];
/// Elements that end the current line.
const LINE_BREAKS: &[&str] = &[
    "br",
    "li",
    "tr",
    "dd",
    "dt",
    "hr",
    "figcaption",
    "caption",
    "option",
    "summary",
];
/// Elements that stand as their own paragraph.
const PARAGRAPHS: &[&str] = &[
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "address",
    "dl",
    "form",
    "fieldset",
    "figure",
    "main",
    "nav",
    "aside",
    "details",
];

#[derive(Default)]
struct Writer {
    out: String,
    preformatted: usize,
    skipped: usize,
    link: Option<(String, usize)>,
}

impl Writer {
    fn trailing_newlines(&self) -> usize {
        self.out
            .trim_end_matches([' ', '\t'])
            .chars()
            .rev()
            .take_while(|c| *c == '\n')
            .count()
    }

    /// Guarantee at least `count` line ends after the text so far; never at
    /// the very start.
    fn ensure_newlines(&mut self, count: usize) {
        if self.out.trim().is_empty() {
            self.out.clear();
            return;
        }
        let trimmed = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(trimmed);
        for _ in self.trailing_newlines()..count {
            self.out.push('\n');
        }
    }

    fn text(&mut self, text: &str) {
        if self.skipped > 0 {
            return;
        }
        if self.preformatted > 0 {
            self.out.push_str(text);
            return;
        }
        let mut pending_space = false;
        for ch in text.chars() {
            if ch.is_whitespace() {
                pending_space = true;
                continue;
            }
            if pending_space && !self.out.is_empty() && !self.out.ends_with(['\n', ' ']) {
                self.out.push(' ');
            }
            pending_space = false;
            self.out.push(ch);
        }
        if pending_space && !self.out.is_empty() && !self.out.ends_with(['\n', ' ']) {
            self.out.push(' ');
        }
    }

    fn open(&mut self, name: &str, attributes: &str) {
        if SKIPPED.contains(&name) {
            self.skipped += 1;
            return;
        }
        if self.skipped > 0 {
            return;
        }
        match name {
            "pre" => {
                self.ensure_newlines(2);
                self.preformatted += 1;
            }
            "li" => {
                self.ensure_newlines(1);
                self.out.push_str("- ");
            }
            "a" => {
                if let Some(href) = attribute(attributes, "href") {
                    self.link = Some((href, self.out.len()));
                }
            }
            "img" => {
                if let Some(alt) = attribute(attributes, "alt").filter(|alt| !alt.is_empty()) {
                    self.text(&format!("[{alt}]"));
                }
            }
            "hr" => {
                self.ensure_newlines(1);
                self.out.push_str("----");
                self.ensure_newlines(1);
            }
            "td" | "th" => {
                if !self.out.ends_with(['\n', ' ']) && !self.out.is_empty() {
                    self.out.push(' ');
                }
            }
            _ if PARAGRAPHS.contains(&name) => self.ensure_newlines(2),
            _ if LINE_BREAKS.contains(&name) => self.ensure_newlines(1),
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        if SKIPPED.contains(&name) {
            self.skipped = self.skipped.saturating_sub(1);
            return;
        }
        if self.skipped > 0 {
            return;
        }
        match name {
            "pre" => {
                self.preformatted = self.preformatted.saturating_sub(1);
                self.ensure_newlines(2);
            }
            "a" => {
                if let Some((href, start)) = self.link.take() {
                    let label = self.out.get(start..).unwrap_or_default().trim();
                    if !href.is_empty() && label != href && !label.is_empty() {
                        self.out.push_str(" <");
                        self.out.push_str(&href);
                        self.out.push('>');
                    } else if label.is_empty() && !href.is_empty() {
                        self.out.push_str(&href);
                    }
                }
            }
            "br" | "hr" | "img" => {}
            _ if PARAGRAPHS.contains(&name) => self.ensure_newlines(2),
            _ if LINE_BREAKS.contains(&name) => self.ensure_newlines(1),
            _ => {}
        }
    }
}

/// The value of `name="..."` (or `name=value`) inside a tag's attribute text.
fn attribute(attributes: &str, name: &str) -> Option<String> {
    let lower = attributes.to_ascii_lowercase();
    let mut search = 0;
    while let Some(found) = lower[search..].find(name) {
        let start = search + found;
        let before_ok = start == 0
            || lower.as_bytes()[start - 1].is_ascii_whitespace()
            || lower.as_bytes()[start - 1] == b'"'
            || lower.as_bytes()[start - 1] == b'\'';
        let rest = &attributes[start + name.len()..];
        let rest_trimmed = rest.trim_start();
        if before_ok && let Some(value) = rest_trimmed.strip_prefix('=') {
            let value = value.trim_start();
            let raw = match value.chars().next() {
                Some(quote @ ('"' | '\'')) => value[1..].split(quote).next().unwrap_or_default(),
                _ => value.split(char::is_whitespace).next().unwrap_or_default(),
            };
            return Some(decode_entities(raw.trim()));
        }
        search = start + name.len();
    }
    None
}

/// Decode numeric references and the named entities that appear in mail.
#[must_use]
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest
            .char_indices()
            .take(12)
            .find_map(|(index, c)| (c == ';').then_some(index))
        else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = entity.strip_prefix('#').map_or_else(
            || named_entity(entity).map(str::to_owned),
            |number| {
                let code = number.strip_prefix(['x', 'X']).map_or_else(
                    || number.parse::<u32>().ok(),
                    |hex| u32::from_str_radix(hex, 16).ok(),
                );
                code.and_then(char::from_u32)
                    .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                    .map(String::from)
            },
        );
        if let Some(text) = decoded {
            out.push_str(&text);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "bull" => "•",
        "middot" => "·",
        "euro" => "€",
        "pound" => "£",
        "yen" => "¥",
        "cent" => "¢",
        "deg" => "°",
        "times" => "×",
        "divide" => "÷",
        "eacute" => "é",
        "egrave" => "è",
        "agrave" => "à",
        "aacute" => "á",
        "ccedil" => "ç",
        "ntilde" => "ñ",
        "ouml" => "ö",
        "uuml" => "ü",
        "auml" => "ä",
        "szlig" => "ß",
        _ => return None,
    })
}

/// Render `html` as plain text.
///
/// Block elements become line or paragraph breaks, list items get a dash,
/// links are followed by their target in angle brackets, scripts and styles
/// disappear, whitespace collapses outside `<pre>`, and entities are decoded.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let mut writer = Writer::default();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        writer.text(&decode_entities(&rest[..open]));
        rest = &rest[open..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let after = &rest[9..];
            let end = after.find("]]>");
            writer.text(end.map_or(after, |end| &after[..end]));
            rest = end.map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some(close) = tag_end(rest) else {
            // An unterminated tag: nothing after it is renderable text.
            rest = "";
            break;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        let tag = tag.trim();
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let (closing, tag) = tag
            .strip_prefix('/')
            .map_or((false, tag), |name| (true, name));
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tag.len());
        let name = tag[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        let attributes = tag[name_end..].trim_end_matches('/');
        if closing {
            writer.close(&name);
        } else {
            writer.open(&name, attributes);
            if tag.ends_with('/') && !matches!(name.as_str(), "br" | "hr" | "img") {
                writer.close(&name);
            }
        }
    }
    writer.text(&decode_entities(rest));
    finish(&writer.out)
}

/// Index of the `>` that ends the tag at the start of `rest`, honouring
/// quoted attribute values.
fn tag_end(rest: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, ch) in rest.char_indices().skip(1) {
        match (quote, ch) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

/// Trim each line's trailing blanks, cap runs of blank lines at one, trim.
fn finish(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_owned()
}
