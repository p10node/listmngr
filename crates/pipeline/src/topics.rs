//! Mailman's topic matcher, the `tagger` handler's brain.
//!
//! Each topic is a multi-line pattern whose lines are alternatives, searched
//! case-insensitively in the `Subject:` and `Keywords:` headers and in the
//! header-like lines that open the body. Hits become the `X-Topics` header.
use crate::chain::compile_header_pattern;
use listmngr_core::Topic;

/// Mailman's `_compile_pattern`: every line of the pattern is an alternative.
///
/// # Errors
/// Returns the regex error for a pattern that does not compile or exceeds
/// the size budget; owners' topics are validated with this same function.
pub fn compile_topic_pattern(pattern: &str) -> Result<regex::Regex, regex::Error> {
    let alternatives: Vec<String> = pattern
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("(?:{line})"))
        .collect();
    compile_header_pattern(&alternatives.join("|"))
}

/// The body lines the matcher looks at.
///
/// Leading lines that look like `Subject:` or `Keywords:` pseudo-headers, up
/// to `limit` lines (negative means unlimited, zero means none). Blank lines
/// are skipped; the first other line ends the scan, as Mailman's `scanbody`
/// does.
#[must_use]
pub fn header_like_body_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    limit: i32,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut matched = Vec::new();
    for (seen, line) in lines.into_iter().enumerate() {
        if limit > 0 && seen >= usize::try_from(limit).unwrap_or(usize::MAX) {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            break;
        };
        if name.trim().eq_ignore_ascii_case("subject")
            || name.trim().eq_ignore_ascii_case("keywords")
        {
            matched.push(value.trim().to_owned());
        } else {
            break;
        }
    }
    matched
}

/// The names of the topics whose pattern matches any of `lines`, in the
/// list's topic order. A topic whose pattern does not compile never matches.
#[must_use]
pub fn hits<'a>(topics: &'a [Topic], lines: &[String]) -> Vec<&'a str> {
    topics
        .iter()
        .filter(|topic| {
            compile_topic_pattern(&topic.pattern)
                .is_ok_and(|regex| lines.iter().any(|line| regex.is_match(line)))
        })
        .map(|topic| topic.name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str, pattern: &str) -> Topic {
        Topic {
            name: name.into(),
            pattern: pattern.into(),
            description: String::new(),
        }
    }

    #[test]
    fn pattern_lines_are_case_insensitive_alternatives() {
        let topics = [
            topic("rust", "cargo\nborrow checker"),
            topic("python", "pip$"),
        ];
        let lines = ["Re: BORROW CHECKER woes".to_owned()];
        assert_eq!(hits(&topics, &lines), ["rust"]);
        assert_eq!(hits(&topics, &["use pip".to_owned()]), ["python"]);
        assert!(hits(&topics, &["nothing".to_owned()]).is_empty());
        assert!(hits(&[topic("broken", "(")], &["(".to_owned()]).is_empty());
    }

    #[test]
    fn body_scan_stops_at_the_first_non_header_line_and_honours_the_limit() {
        let body = [
            "",
            "Subject: one",
            "Keywords: two, three",
            "  ",
            "hello",
            "Subject: late",
        ];
        assert_eq!(header_like_body_lines(body, -1), ["one", "two, three"]);
        assert_eq!(header_like_body_lines(body, 2), ["one"]);
        assert!(header_like_body_lines(body, 0).is_empty());
        assert_eq!(
            header_like_body_lines(["Keywords: x", "From: y", "Subject: z"], -1),
            ["x"]
        );
    }
}
