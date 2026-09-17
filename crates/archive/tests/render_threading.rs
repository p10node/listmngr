//! Thread order and body rendering as pure functions: the fixture mbox
//! lays out the way `HyperKitty` threads it, and the renderer emits only the
//! safe subset whatever the post contains.
use listmngr_archive::render::{Addresses, Mode, body_html, obfuscate};
use listmngr_archive::threading::{Node, Placed, order};

/// The fixture's messages, parsed one by one.
fn fixture_nodes() -> (Vec<Node>, std::collections::BTreeMap<String, String>) {
    let mbox = include_str!("fixtures/threading.mbox");
    let mut nodes = Vec::new();
    let mut ids = std::collections::BTreeMap::new();
    for chunk in mbox.split("\nFrom ").map(|c| c.trim_start_matches("From ")) {
        let body = chunk.split_once('\n').map_or("", |x| x.1);
        let raw = body.replace('\n', "\r\n");
        let message = mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .unwrap();
        let id = message.message_id().unwrap().to_owned();
        let identity = listmngr_archive::identity(&message, &format!("<{id}>")).unwrap();
        ids.insert(identity.hash.clone(), format!("<{id}>"));
        nodes.push(Node {
            hash: identity.hash,
            parent: identity.parent,
            date_ms: identity.date_ms.unwrap(),
        });
    }
    (nodes, ids)
}

#[test]
fn the_fixture_mbox_threads_as_hyperkitty_lays_it_out() {
    let (nodes, ids) = fixture_nodes();
    let placed: Vec<serde_json::Value> = order(&nodes)
        .into_iter()
        .map(|Placed { hash, depth }| serde_json::json!({"id": ids[&hash], "depth": depth}))
        .collect();
    let expected: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/threading.expected.json")).unwrap();
    assert_eq!(placed, expected);
}

#[test]
fn a_cycle_after_reattachment_places_every_post_once() {
    let nodes = vec![
        Node {
            hash: "x".into(),
            parent: Some("y".into()),
            date_ms: 2,
        },
        Node {
            hash: "y".into(),
            parent: Some("x".into()),
            date_ms: 1,
        },
        Node {
            hash: "z".into(),
            parent: Some("x".into()),
            date_ms: 3,
        },
    ];
    let placed = order(&nodes);
    let hashes: Vec<&str> = placed.iter().map(|p| p.hash.as_str()).collect();
    assert_eq!(hashes.len(), 3);
    assert_eq!(
        hashes[0], "y",
        "the earliest member of the cycle stands as root"
    );
    assert!(hashes.contains(&"x") && hashes.contains(&"z"));
}

#[test]
fn addresses_are_obfuscated_with_the_domain_kept() {
    assert_eq!(
        obfuscate("write to alice@example.org today"),
        "write to alice at example.org today"
    );
    assert_eq!(
        obfuscate("a@b"),
        "a@b",
        "not an address without a dot in the domain"
    );
    assert_eq!(obfuscate("@example.org"), "@example.org");
    assert_eq!(obfuscate("x@y.z, x@y.z"), "x at y.z, x at y.z");
}

#[test]
fn text_bodies_fold_quotes_link_urls_and_escape() {
    let quoted = |n: usize| format!("{n} quoted lines");
    let html = body_html(
        "Hello <b>world</b> https://example.org/a?b=1&c=2.\n> first quoted\n> second quoted\nBack to bob@example.org",
        Mode::Text,
        Addresses::Obfuscated,
        &quoted,
    );
    assert!(html.contains("Hello &lt;b&gt;world&lt;/b&gt; <a href=\"https://example.org/a?b=1&amp;c=2\" rel=\"nofollow noopener\">https://example.org/a?b=1&amp;c=2</a>."), "{html}");
    assert!(html.contains("<details class=\"quote\"><summary>2 quoted lines</summary><pre>&gt; first quoted\n&gt; second quoted</pre></details>"), "{html}");
    assert!(html.contains("bob at example.org"), "{html}");
    assert!(!html.contains("bob@example.org"));
    let shown = body_html("bob@example.org", Mode::Text, Addresses::Shown, &quoted);
    assert!(shown.contains("bob@example.org"));
}

#[test]
fn markdown_renders_the_safe_subset_only() {
    let quoted = |n: usize| format!("{n}");
    let html = body_html(
        "# Title\n\n**bold** and *em* with `code` <script>alert(1)</script>\n\n[ok](https://example.org) [bad](javascript:alert(1)) ![img](https://example.org/x.png)\n\n- one\n- two\n\n> quote\n\n```rust\nfn main() {}\n```\n\n<div onclick=\"x\">raw</div>",
        Mode::Markdown,
        Addresses::Shown,
        &quoted,
    );
    assert!(html.contains("<h3>Title</h3>"), "{html}");
    assert!(
        html.contains("<strong>bold</strong>")
            && html.contains("<em>em</em>")
            && html.contains("<code>code</code>"),
        "{html}"
    );
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
    assert!(!html.contains("<script"), "{html}");
    assert!(
        html.contains("<a href=\"https://example.org\" rel=\"nofollow noopener\">ok</a>"),
        "{html}"
    );
    assert!(!html.contains("javascript:"), "{html}");
    assert!(html.contains("bad"), "the unsafe link's text stays: {html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(html.contains("<ul><li>one</li>"), "{html}");
    assert!(html.contains("<blockquote>"), "{html}");
    assert!(
        html.contains("<pre><code class=\"language-rust\">fn main() {}\n</code></pre>"),
        "{html}"
    );
    assert!(
        html.contains("&lt;div onclick=&quot;x&quot;&gt;raw&lt;/div&gt;"),
        "{html}"
    );
    assert!(!html.contains("<div"), "{html}");
}
