//! Heuristic bounce detection, after the model of `flufl.bounce`.
//!
//! A standards-conforming report (RFC 3464) is read exactly; everything
//! else is prose from a particular MTA family, matched by the phrases that
//! family writes and the way it lists the failed addresses. Detectors run
//! most specific first, and a warning that is only a delay wins over
//! nothing. Every address found is a claim about the *original* recipient,
//! never authority: the bounce runner still requires a list member.
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// What a bounce message says, as far as its prose can be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// Recipients that permanently failed, canonicalized to lower case.
    Failed(BTreeSet<String>),
    /// A delay or warning: recognized, but nothing failed yet.
    Temporary,
    /// Not a bounce this module knows.
    Unrecognized,
}

/// A detection and the detector that made it (`"none"` for none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub detection: Detection,
    pub detector: &'static str,
}

/// One MTA family's reading of the message lines.
type Detector = fn(&[&str]) -> Option<Detection>;

/// Bytes of decoded text considered; the rest of a report is ignored.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Addresses returned by one report.
pub const MAX_ADDRESSES: usize = 100;

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<?([a-z0-9!#$%&'*+/=?^_`{|}~.-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+)>?").unwrap()
});
/// Lines an MTA writes as `<address>: reason` (Postfix, qmail, Yahoo).
static BRACKETED_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*<([^<>\s]+@[^<>\s]+)>:").unwrap());
static PERMANENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(permanent(ly)? (fatal )?(error|fail)|user unknown|unknown user|no such user|does not exist|not a valid mailbox|address rejected|recipient rejected|delivery (to the following recipient )?failed|failed permanently|could ?n.t be (delivered|found)|couldn't be found|undeliverable|mailbox (is )?(full|unavailable)|unrouteable address|user doesn't have)").unwrap()
});
static TEMPORARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(this is a warning|warning only|delayed mail|has not (yet )?been delivered|has been delayed|will (be )?retr(y|ied)|still being retried|temporar(y|ily) (failure|unavailable|deferred)|could not be delivered for more than)").unwrap()
});

/// Classify `raw`.
#[must_use]
pub fn detect(raw: &[u8]) -> Detected {
    if let Some(report) = crate::dsn::parse_report(raw) {
        let failed: BTreeSet<String> = report
            .recipients
            .iter()
            .filter(|claim| claim.action == "failed")
            .filter_map(|claim| canonical(&claim.final_recipient))
            .take(MAX_ADDRESSES)
            .collect();
        let detection = if failed.is_empty() {
            Detection::Temporary
        } else {
            Detection::Failed(failed)
        };
        return Detected {
            detection,
            detector: "dsn",
        };
    }
    let Some(text) = text_of(raw) else {
        return Detected {
            detection: Detection::Unrecognized,
            detector: "none",
        };
    };
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let detectors: [(&'static str, Detector); 7] = [
        ("postfix", postfix),
        ("qmail", qmail),
        ("exim", exim),
        ("sendmail", sendmail),
        ("yahoo", yahoo),
        ("exchange", exchange),
        ("simplematch", simple_match),
    ];
    for (name, detector) in detectors {
        if let Some(detection) = detector(&lines) {
            return Detected {
                detection,
                detector: name,
            };
        }
    }
    if TEMPORARY.is_match(&text) {
        return Detected {
            detection: Detection::Temporary,
            detector: "warning",
        };
    }
    Detected {
        detection: Detection::Unrecognized,
        detector: "none",
    }
}

/// The decoded text of every `text/plain` part, bounded.
fn text_of(raw: &[u8]) -> Option<String> {
    let message = mail_parser::MessageParser::default().parse(raw)?;
    let mut text = String::new();
    for part in message.text_bodies() {
        if let mail_parser::PartType::Text(body) = &part.body {
            text.push_str(body);
            text.push('\n');
        }
        if text.len() >= MAX_TEXT_BYTES {
            break;
        }
    }
    if text.trim().is_empty() {
        return None;
    }
    // Truncate on a character boundary so a multibyte tail cannot split.
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    Some(text)
}

/// Lower-cased and bounded, never a daemon's own address.
fn canonical(address: &str) -> Option<String> {
    let address = address.trim().trim_matches(['<', '>']).to_ascii_lowercase();
    let (local, _) = address.split_once('@')?;
    if address.len() > 254 || matches!(local, "mailer-daemon" | "postmaster" | "") {
        return None;
    }
    Some(address)
}

fn found(addresses: impl IntoIterator<Item = String>) -> Option<Detection> {
    let set: BTreeSet<String> = addresses.into_iter().take(MAX_ADDRESSES).collect();
    (!set.is_empty()).then_some(Detection::Failed(set))
}

/// Addresses on `<address>: reason` lines after the first line matching
/// `intro`, until `stop` matches.
fn bracketed_after(
    lines: &[&str],
    intro: &dyn Fn(&str) -> bool,
    stop: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut addresses = Vec::new();
    let mut started = false;
    for line in lines {
        if !started {
            started = intro(line);
            continue;
        }
        if stop(line) {
            break;
        }
        if let Some(captures) = BRACKETED_LINE.captures(line)
            && let Some(address) = canonical(&captures[1])
        {
            addresses.push(address);
        }
    }
    addresses
}

fn postfix(lines: &[&str]) -> Option<Detection> {
    static INTRO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(This is the (mail system|Postfix program)|.*The (mail system|Postfix program)$)",
        )
        .unwrap()
    });
    static STOP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^-+ ?(Undelivered|Original)").unwrap());
    let text = lines.join("\n");
    if !lines.iter().any(|line| INTRO.is_match(line)) {
        return None;
    }
    // Postfix uses the same prose for a delay warning.
    if TEMPORARY.is_match(&text) {
        return Some(Detection::Temporary);
    }
    found(bracketed_after(
        lines,
        &|line| INTRO.is_match(line),
        &|line| STOP.is_match(line),
    ))
}

fn qmail(lines: &[&str]) -> Option<Detection> {
    let intro = |line: &str| line.starts_with("Hi. This is the qmail-send program");
    if !lines.iter().any(|line| intro(line)) {
        return None;
    }
    found(bracketed_after(lines, &intro, &|line| {
        line.starts_with("--- Below this line is a copy")
    }))
}

fn exim(lines: &[&str]) -> Option<Detection> {
    static LIST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(following address(?:\(es\)|es)? failed:|could not be delivered to( one or more of)? (its )?recipients?)")
            .unwrap()
    });
    if !lines.iter().any(|line| {
        line.starts_with("This message was created automatically by mail delivery software")
    }) {
        return None;
    }
    let mut addresses = Vec::new();
    let mut listing = false;
    for line in lines {
        if line.starts_with("------ This is a copy") {
            break;
        }
        if !listing {
            listing = LIST.is_match(line);
            continue;
        }
        // Exim indents each failed address on its own line; the reason
        // lines below it are indented further and carry no address.
        if line.starts_with("  ") && !line.starts_with("    ") {
            addresses.extend(ADDRESS.captures_iter(line).filter_map(|c| canonical(&c[1])));
        }
    }
    found(addresses)
}

fn sendmail(lines: &[&str]) -> Option<Detection> {
    static INTRO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"-+ The following addresses had (permanent )?(fatal|delivery) errors -+")
            .unwrap()
    });
    static STOP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*-{3,}").unwrap());
    let start = lines.iter().position(|line| INTRO.is_match(line))?;
    let mut addresses = Vec::new();
    for line in &lines[start + 1..] {
        if STOP.is_match(line) {
            break;
        }
        addresses.extend(ADDRESS.captures_iter(line).filter_map(|c| canonical(&c[1])));
    }
    found(addresses)
}

fn yahoo(lines: &[&str]) -> Option<Detection> {
    let intro = |line: &str| {
        line.starts_with("Sorry, we were unable to deliver your message to the following address")
    };
    if !lines.iter().any(|line| intro(line)) {
        return None;
    }
    found(bracketed_after(lines, &intro, &|line| {
        line.starts_with("--- Original message follows")
    }))
}

fn exchange(lines: &[&str]) -> Option<Detection> {
    static INTRO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(Your message did not reach some or all of the intended recipients|Delivery has failed to these recipients or groups|.*did not reach the following recipient)").unwrap()
    });
    static STOP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(Diagnostic information for administrators|Original message headers|-+ ?Original)").unwrap()
    });
    let start = lines.iter().position(|line| INTRO.is_match(line))?;
    let mut addresses = Vec::new();
    for line in &lines[start + 1..] {
        if STOP.is_match(line) {
            break;
        }
        // The bounce's own summary quotes the original headers' subject
        // and date, never an address; anything else with an address is
        // a failed recipient or the diagnostic naming one.
        addresses.extend(ADDRESS.captures_iter(line).filter_map(|c| canonical(&c[1])));
    }
    found(addresses)
}

/// The generic fallback: a permanent-failure phrase, and the addresses
/// within a few lines of it.
fn simple_match(lines: &[&str]) -> Option<Detection> {
    let mut addresses = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !PERMANENT.is_match(line) {
            continue;
        }
        let window = &lines[index.saturating_sub(3)..(index + 6).min(lines.len())];
        for near in window {
            addresses.extend(ADDRESS.captures_iter(near).filter_map(|c| canonical(&c[1])));
        }
    }
    found(addresses)
}
