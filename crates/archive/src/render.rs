//! Safe HTML for an archived post's body.
//!
//! Every byte of the output is produced here: text is escaped, quoted runs
//! fold into `<details>`, URLs become links, and Markdown (when the list
//! renders it) goes through a writer that emits a fixed safe subset — no
//! raw HTML, no `javascript:` link, no image, no attribute beyond `href`,
//! `rel` and `class`. Addresses are obfuscated for readers who may not
//! see them.
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// How a list asks its posts to be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Text,
    Markdown,
}

/// What the reader may see of addresses in the body and the sender line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addresses {
    /// A signed-in reader: addresses as written.
    Shown,
    /// A visitor: `local at domain`, as Mailman's archives did.
    Obfuscated,
}

/// HTML-escape `text` for a text or attribute context.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// `local at domain` for every address in `text`, so a crawler reads no
/// mailbox; the domain keeps its dots.
#[must_use]
pub fn obfuscate(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let bytes = text.as_bytes();
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' && i > 0 && i + 1 < bytes.len() {
            let start = bytes[..i]
                .iter()
                .rposition(|b| !is_local(*b))
                .map_or(0, |p| p + 1);
            let end = bytes[i + 1..]
                .iter()
                .position(|b| !is_domain(*b))
                .map_or(bytes.len(), |p| i + 1 + p);
            let domain = &text[i + 1..end];
            if start < i
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
            {
                out.push_str(&text[last..i]);
                out.push_str(" at ");
                last = i + 1;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

const fn is_local(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b'%')
}

const fn is_domain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// The sender line's address as the reader may see it.
#[must_use]
pub fn sender_email(email: &str, addresses: Addresses) -> String {
    match addresses {
        Addresses::Shown => email.to_owned(),
        Addresses::Obfuscated => obfuscate(email),
    }
}

/// Escaped text with `http(s)://` runs turned into links.
fn linkify(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(pos) = rest.find("http://").or_else(|| rest.find("https://")) {
        let (before, after) = rest.split_at(pos);
        out.push_str(&escape(before));
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
            .unwrap_or(after.len());
        let (url, tail) = after.split_at(end);
        let url = url.trim_end_matches(['.', ',', ';', ')', ']']);
        let trailing = &after[url.len()..end];
        if url.len() > 8 && url.len() <= 2048 {
            out.push_str("<a href=\"");
            out.push_str(&escape(url));
            out.push_str("\" rel=\"nofollow noopener\">");
            out.push_str(&escape(url));
            out.push_str("</a>");
        } else {
            out.push_str(&escape(url));
        }
        out.push_str(&escape(trailing));
        rest = tail;
    }
    out.push_str(&escape(rest));
    out
}

/// A run of quoted lines, folded; `label` is the translated summary.
fn quote_block(lines: &[&str], label: &str) -> String {
    format!(
        "<details class=\"quote\"><summary>{}</summary><pre>{}</pre></details>",
        escape(label),
        lines
            .iter()
            .map(|l| linkify(l))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Plain text: paragraphs of escaped, linkified lines with quoted runs
/// folded. `quoted` names a run of `n` quoted lines.
fn text_html(body: &str, quoted: &dyn Fn(usize) -> String) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    let mut plain: Vec<&str> = Vec::new();
    let mut quote: Vec<&str> = Vec::new();
    let flush_plain = |plain: &mut Vec<&str>, out: &mut String| {
        if !plain.is_empty() {
            out.push_str("<pre>");
            out.push_str(
                &plain
                    .iter()
                    .map(|l| linkify(l))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            out.push_str("</pre>");
            plain.clear();
        }
    };
    let flush_quote = |quote: &mut Vec<&str>, out: &mut String| {
        if !quote.is_empty() {
            out.push_str(&quote_block(quote, &quoted(quote.len())));
            quote.clear();
        }
    };
    for line in body.lines() {
        if line.trim_start().starts_with('>') {
            flush_plain(&mut plain, &mut out);
            quote.push(line);
        } else {
            flush_quote(&mut quote, &mut out);
            plain.push(line);
        }
    }
    flush_plain(&mut plain, &mut out);
    flush_quote(&mut quote, &mut out);
    out
}

/// Whether a link target may be followed from the archive.
fn safe_href(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:"))
        && lower.len() <= 2048
}

/// A heading level demoted below the page's own headings.
fn demoted(level: pulldown_cmark::HeadingLevel) -> usize {
    (level as usize).clamp(1, 6).saturating_add(2).min(6)
}

/// The opening of one Markdown construct, or nothing for the kinds the
/// archive does not render. `skipped_link` remembers a dropped link so its
/// end is dropped too.
fn start_tag(tag: Tag<'_>, out: &mut String, skipped_link: &mut bool) {
    use std::fmt::Write as _;
    match tag {
        Tag::Paragraph => out.push_str("<p>"),
        Tag::Heading { level, .. } => {
            let _ = write!(out, "<h{}>", demoted(level));
        }
        Tag::BlockQuote(_) => out.push_str("<blockquote>"),
        Tag::CodeBlock(kind) => {
            if let CodeBlockKind::Fenced(language) = kind
                && !language.is_empty()
            {
                out.push_str("<pre><code class=\"language-");
                out.push_str(&escape(
                    &language
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .take(32)
                        .collect::<String>(),
                ));
                out.push_str("\">");
            } else {
                out.push_str("<pre><code>");
            }
        }
        Tag::List(Some(_)) => out.push_str("<ol>"),
        Tag::List(None) => out.push_str("<ul>"),
        Tag::Item => out.push_str("<li>"),
        Tag::Emphasis => out.push_str("<em>"),
        Tag::Strong => out.push_str("<strong>"),
        Tag::Strikethrough => out.push_str("<del>"),
        Tag::Link { dest_url, .. } => {
            if safe_href(&dest_url) {
                out.push_str("<a href=\"");
                out.push_str(&escape(&dest_url));
                out.push_str("\" rel=\"nofollow noopener\">");
                *skipped_link = false;
            } else {
                *skipped_link = true;
            }
        }
        Tag::Image { dest_url, .. } => {
            // Images become their address as text; nothing loads.
            out.push_str(&escape(&dest_url));
            out.push(' ');
        }
        Tag::Table(_) => out.push_str("<table>"),
        Tag::TableHead => out.push_str("<thead><tr>"),
        Tag::TableRow => out.push_str("<tr>"),
        Tag::TableCell => out.push_str("<td>"),
        Tag::HtmlBlock
        | Tag::FootnoteDefinition(_)
        | Tag::DefinitionList
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::MetadataBlock(_)
        | Tag::Superscript
        | Tag::Subscript => {}
    }
}

/// The closing of one Markdown construct.
fn end_tag(tag: TagEnd, out: &mut String, skipped_link: &mut bool) {
    use std::fmt::Write as _;
    match tag {
        TagEnd::Paragraph => out.push_str("</p>"),
        TagEnd::Heading(level) => {
            let _ = write!(out, "</h{}>", demoted(level));
        }
        TagEnd::BlockQuote(_) => out.push_str("</blockquote>"),
        TagEnd::CodeBlock => out.push_str("</code></pre>"),
        TagEnd::List(true) => out.push_str("</ol>"),
        TagEnd::List(false) => out.push_str("</ul>"),
        TagEnd::Item => out.push_str("</li>"),
        TagEnd::Emphasis => out.push_str("</em>"),
        TagEnd::Strong => out.push_str("</strong>"),
        TagEnd::Strikethrough => out.push_str("</del>"),
        TagEnd::Link => {
            if !*skipped_link {
                out.push_str("</a>");
            }
            *skipped_link = false;
        }
        TagEnd::Table => out.push_str("</table>"),
        TagEnd::TableHead => out.push_str("</tr></thead>"),
        TagEnd::TableRow => out.push_str("</tr>"),
        TagEnd::TableCell => out.push_str("</td>"),
        TagEnd::Image
        | TagEnd::HtmlBlock
        | TagEnd::FootnoteDefinition
        | TagEnd::DefinitionList
        | TagEnd::DefinitionListTitle
        | TagEnd::DefinitionListDefinition
        | TagEnd::MetadataBlock(_)
        | TagEnd::Superscript
        | TagEnd::Subscript => {}
    }
}

/// Markdown through a writer that emits only the safe subset.
fn markdown_html(body: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut out = String::with_capacity(body.len() + 64);
    let mut skipped_link = false;
    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(tag) => start_tag(tag, &mut out, &mut skipped_link),
            Event::End(tag) => end_tag(tag, &mut out, &mut skipped_link),
            Event::Code(code) => {
                out.push_str("<code>");
                out.push_str(&escape(&code));
                out.push_str("</code>");
            }
            // Raw HTML, inline or block, is shown as text, never emitted.
            Event::Text(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text)
            | Event::FootnoteReference(text) => out.push_str(&escape(&text)),
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr>"),
            Event::TaskListMarker(done) => out.push_str(if done { "[x] " } else { "[ ] " }),
        }
    }
    out
}

/// The body as HTML the archive page can include verbatim.
///
/// `quoted` translates the summary of a folded run of `n` quoted lines.
#[must_use]
pub fn body_html(
    body: &str,
    mode: Mode,
    addresses: Addresses,
    quoted: &dyn Fn(usize) -> String,
) -> String {
    let body = match addresses {
        Addresses::Shown => body.to_owned(),
        Addresses::Obfuscated => obfuscate(body),
    };
    match mode {
        Mode::Text => text_html(&body, quoted),
        Mode::Markdown => markdown_html(&body),
    }
}

/// The words a search page marks: the reader's query split into
/// alphanumeric runs, lowercased, at most twenty.
#[must_use]
pub fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(20)
    {
        let word = word.to_lowercase();
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

/// Wrap every whole word of `html`'s text that is one of `terms` in
/// `<mark>`. Tags and character references pass through untouched, so
/// the output is exactly as safe as the input.
#[must_use]
pub fn highlight(html: &str, terms: &[String]) -> String {
    if terms.is_empty() {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while !rest.is_empty() {
        let next = rest.find(['<', '&']).unwrap_or(rest.len());
        mark_words(&rest[..next], terms, &mut out);
        rest = &rest[next..];
        let Some(first) = rest.chars().next() else {
            break;
        };
        let end = if first == '<' {
            rest.find('>').map_or(rest.len(), |i| i + 1)
        } else {
            match rest.find(';') {
                Some(i) if i <= 12 => i + 1,
                _ => 1,
            }
        };
        out.push_str(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

fn mark_words(text: &str, terms: &[String], out: &mut String) {
    let mut rest = text;
    while !rest.is_empty() {
        let start = rest.find(char::is_alphanumeric).unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric())
            .unwrap_or(rest.len());
        let word = &rest[..end];
        if !word.is_empty() && terms.iter().any(|t| *t == word.to_lowercase()) {
            out.push_str("<mark>");
            out.push_str(word);
            out.push_str("</mark>");
        } else {
            out.push_str(word);
        }
        rest = &rest[end..];
    }
}
