//! The mbox reader and writer: mboxrd quoting both ways, CRLF and LF
//! messages, a preamble, an unterminated last message, and the fixture
//! the threading test uses.
use listmngr_archive::mbox::{Reader, write_message};

fn messages(bytes: &[u8]) -> Vec<Vec<u8>> {
    Reader::new(std::io::Cursor::new(bytes))
        .map(|m| m.unwrap())
        .collect()
}

#[test]
fn writes_and_reads_back_with_from_lines_quoted_once() {
    let raw = b"Subject: x\r\n\r\nFrom the top\r\n>From a quote\r\nnot From here\r\nlast line without newline";
    let mut out = Vec::new();
    write_message(&mut out, raw).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.starts_with("From archive@localhost Thu Jan  1 00:00:00 1970\n"));
    assert!(text.contains("\n>From the top\r\n"), "{text}");
    assert!(text.contains("\n>>From a quote\r\n"), "{text}");
    assert!(text.contains("\nnot From here\r\n"), "{text}");
    assert!(text.ends_with("last line without newline\n\n"), "{text}");
    let back = messages(&out);
    assert_eq!(back.len(), 1);
    assert_eq!(
        String::from_utf8(back[0].clone()).unwrap(),
        "Subject: x\r\n\r\nFrom the top\r\n>From a quote\r\nnot From here\r\nlast line without newline\n"
    );
}

#[test]
fn several_messages_a_preamble_and_a_bare_last_message() {
    let mut out = b"junk before the first separator\n\n".to_vec();
    write_message(&mut out, b"Subject: one\n\nbody one\n").unwrap();
    write_message(&mut out, b"Subject: two\n\nbody two\n").unwrap();
    out.extend_from_slice(b"From x@y Thu Jan  1 00:00:00 1970\nSubject: three\n\nbody three");
    let back = messages(&out);
    assert_eq!(back.len(), 3);
    assert_eq!(back[0], b"Subject: one\n\nbody one\n");
    assert_eq!(back[1], b"Subject: two\n\nbody two\n");
    assert_eq!(back[2], b"Subject: three\n\nbody three");
    assert!(messages(b"").is_empty());
    assert!(messages(b"no separator at all\n").is_empty());
}

#[test]
fn the_threading_fixture_reads_as_its_messages() {
    let fixture = include_bytes!("fixtures/threading.mbox");
    let separators = String::from_utf8_lossy(fixture)
        .lines()
        .filter(|line| line.starts_with("From "))
        .count();
    let back = messages(fixture);
    assert_eq!(back.len(), separators);
    assert!(
        back.iter()
            .all(|m| String::from_utf8_lossy(m).contains("Message-ID: <"))
    );
}

/// The separator that ends a message must open the next one exactly once.
/// While it was only consumed for the first message, every later call read
/// it again and returned an empty message, so a two-message mbox streamed
/// for ever. Bounded with `take` so the regression fails rather than hangs.
#[test]
fn a_separator_opens_the_next_message_once_so_reading_ends() {
    let mut out = Vec::new();
    write_message(&mut out, b"Subject: one\n\nbody one\n").unwrap();
    write_message(&mut out, b"Subject: two\n\nbody two\n").unwrap();
    let bounded = Reader::new(std::io::Cursor::new(&out[..])).take(10).count();
    assert_eq!(bounded, 2, "the reader yielded empty messages");
    let mut reader = Reader::new(std::io::Cursor::new(&out[..]));
    assert!(reader.next().is_some());
    assert!(reader.next().is_some());
    assert!(reader.next().is_none(), "the reader ends after the last");
}
