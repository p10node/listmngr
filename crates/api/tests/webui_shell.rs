//! P4-SHELL: one template rendering path, a translated shell and vendored htmx.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use listmngr_core::Config;
use listmngr_db::{Database, NewList};
use tower::ServiceExt;

async fn fixture() -> axum::Router {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains().create("example.com", "", None).await.unwrap();
    db.lists()
        .create(NewList {
            list_id: "public.example.com".parse().unwrap(),
            display_name: "Public".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let mut config = Config::default();
    config.site.base_url = "http://localhost".into();
    listmngr_api::router(db, config)
}

async fn get(app: &axum::Router, path: &str, language: &str) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header("host", "localhost")
                .header("accept-language", language)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 4_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// Two rendering paths do not coexist after this work package (ADR-0004): no
/// handler in this crate may contain markup, only templates may. Since
/// `P1-API-DOCS-ORIGIN` that covers every page this server serves, `/api/docs`
/// included.
#[test]
fn browser_handlers_build_no_html_by_hand() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&source).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if literals(line).iter().any(|literal| markup(literal)) {
                offenders.push(format!("{name}:{}", number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "hand-built markup outside templates: {offenders:?}"
    );
}

/// The contents of every double-quoted string literal on one line of Rust.
fn literals(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    let mut escaped = false;
    for character in line.chars() {
        match current.as_mut() {
            Some(literal) if escaped => {
                literal.push(character);
                escaped = false;
            }
            Some(_) if character == '\\' => escaped = true,
            Some(_) if character == '"' => found.push(current.take().expect("open literal")),
            Some(literal) => literal.push(character),
            None if character == '"' => current = Some(String::new()),
            None => {}
        }
    }
    found
}

/// Whether a string literal carries HTML: an element, or a character
/// reference. A Rust generic or a placeholder such as `lm_<uuid>` is not one.
fn markup(literal: &str) -> bool {
    const ELEMENTS: &[&str] = &[
        "a", "article", "body", "br", "button", "dd", "details", "div", "dl", "doctype", "dt",
        "em", "footer", "form", "h1", "h2", "h3", "head", "header", "html", "input", "label", "li",
        "main", "nav", "ol", "option", "p", "pre", "script", "section", "select", "small", "span",
        "strong", "style", "summary", "table", "td", "textarea", "th", "title", "tr", "ul",
    ];
    literal.split('<').skip(1).any(|rest| {
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        let name: String = rest
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect();
        ELEMENTS.contains(&name.as_str())
            && rest[name.len()..].starts_with([' ', '>', '\t'].as_slice())
    }) || ["&amp;", "&quot;", "&lt;", "&gt;", "&#39;"]
        .iter()
        .any(|reference| literal.contains(reference))
}

#[tokio::test]
async fn the_shell_renders_in_the_negotiated_language() {
    let app = fixture().await;
    let english = body(get(&app, "/web", "en-US,en;q=0.9").await).await;
    assert!(english.contains("<html lang=\"en\""), "{english}");
    assert!(english.contains("Mailing lists"));
    assert!(english.contains("My subscriptions"));

    let vietnamese = body(get(&app, "/web", "vi-VN,vi;q=0.9,en;q=0.5").await).await;
    assert!(vietnamese.contains("<html lang=\"vi\""), "{vietnamese}");
    assert!(vietnamese.contains("Danh sách thư"));
    assert!(!vietnamese.contains("My subscriptions"));
    // An unshipped language falls back to English rather than to message ids.
    let klingon = body(get(&app, "/web", "tlh").await).await;
    assert!(klingon.contains("<html lang=\"en\""));
    assert!(!klingon.contains("web-nav-"));
}

#[tokio::test]
async fn navigation_marks_the_page_the_reader_is_on() {
    let app = fixture().await;
    let html = body(get(&app, "/web", "en").await).await;
    assert!(
        html.contains("aria-current=\"page\""),
        "the shell marks the active navigation item: {html}"
    );
}

#[tokio::test]
async fn the_stylesheet_carries_design_tokens_and_a_dark_scheme() {
    let app = fixture().await;
    let response = get(&app, "/web/style.css", "en").await;
    assert_eq!(response.status(), StatusCode::OK);
    let css = body(response).await;
    assert!(css.contains("--"), "design tokens");
    assert!(css.contains("color-scheme:light dark"));
    assert!(css.contains("prefers-color-scheme:dark"));
}

#[tokio::test]
async fn htmx_is_served_from_this_origin_and_no_page_loads_it_yet() {
    let app = fixture().await;
    let response = get(&app, "/web/htmx.min.js", "en").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    let script = body(response).await;
    assert!(script.len() > 10_000, "the vendored library, not a stub");
    // Until a package needs progressive enhancement, pages stay script-free and
    // keep `default-src 'none'`.
    let page = get(&app, "/web", "en").await;
    assert_eq!(
        page.headers()["content-security-policy"],
        "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
    );
    assert!(!body(page).await.contains("<script"));
}
