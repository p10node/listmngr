//! A signed-in reader adds addresses to their account, proves each from its
//! mailbox, chooses the primary one and lets go of the others.
use super::{call, cookie, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::MemberRole;
use listmngr_db::Database;

#[tokio::test]
async fn add_verify_promote_and_remove_addresses() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_addresses_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_addresses")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

async fn page(app: &axum::Router, cookie: &str) -> String {
    let response = call(app, "GET", "/web/account/addresses", cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    text(response).await
}

async fn add(app: &axum::Router, cookie: &str, email: &str) -> StatusCode {
    let html = page(app, cookie).await;
    let body =
        serde_urlencoded::to_string([("csrf", csrf(&html).as_str()), ("email", email)]).unwrap();
    call(app, "POST", "/web/account/addresses", cookie, &body)
        .await
        .status()
}

/// The row id of `email`; the page only offers forms for the actions that
/// apply, so the primary address shows none.
async fn address_id(db: &Database, email: &str) -> String {
    sqlx::query_scalar("SELECT id FROM addresses WHERE email=$1")
        .bind(email)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn act(app: &axum::Router, cookie: &str, id: &str, action: &str) -> StatusCode {
    let html = page(app, cookie).await;
    let body = serde_urlencoded::to_string([("csrf", csrf(&html).as_str())]).unwrap();
    call(
        app,
        "POST",
        &format!("/web/account/addresses/{id}/{action}"),
        cookie,
        &body,
    )
    .await
    .status()
}

async fn mailed_token(db: &Database, to: &str) -> String {
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT b.raw FROM message_blobs b JOIN messages m ON m.store_key=b.store_key ORDER BY m.created_at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let mail = String::from_utf8(raw).unwrap();
    assert!(mail.contains(&format!("To: {to}\r\n")), "{mail}");
    mail.split("enter this token:")
        .nth(1)
        .expect("token line")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .expect("a token")
        .to_owned()
}

async fn verify(app: &axum::Router, token: &str) -> StatusCode {
    let response = call(app, "GET", "/web/verify", "", "").await;
    let session = cookie(&response);
    let csrf_token = csrf(&text(response).await);
    let body =
        serde_urlencoded::to_string([("csrf", csrf_token.as_str()), ("token", token)]).unwrap();
    call(app, "POST", "/web/verify", &session, &body)
        .await
        .status()
}

async fn login(app: &axum::Router, email: &str) -> StatusCode {
    let response = call(app, "GET", "/web/login", "", "").await;
    let session = cookie(&response);
    let token = csrf(&text(response).await);
    let body = serde_urlencoded::to_string([
        ("csrf", token.as_str()),
        ("email", email),
        ("password", "very secure password"),
    ])
    .unwrap();
    call(app, "POST", "/web/login", &session, &body)
        .await
        .status()
}

async fn matrix(db: Database, app: axum::Router) {
    let reader = user(&db, "reader@example.com", false).await;
    user(&db, "other@example.com", false).await;
    let cookie = login_as(&app, "reader@example.com").await;
    let html = page(&app, &cookie).await;
    assert!(html.contains("<h2>reader@example.com</h2>"), "{html}");
    assert!(html.contains("Primary"), "{html}");

    added(&db, &app, &cookie).await;
    let token = mailed_token(&db, "second@example.org").await;
    assert_eq!(verify(&app, &token).await, StatusCode::OK);
    assert_eq!(
        login(&app, "second@example.org").await,
        StatusCode::SEE_OTHER
    );
    promoted(&db, &app, &cookie, reader.id).await;
    removed(&db, &app, &cookie).await;
}

/// Adding: a foreign address links nothing and mails nothing; an unowned one
/// created elsewhere is linked and mailed; a repeat within the hour mails
/// nothing more; nothing works without the session's CSRF token.
async fn added(db: &Database, app: &axum::Router, cookie: &str) {
    assert_eq!(
        call(
            app,
            "POST",
            "/web/account/addresses",
            cookie,
            "csrf=wrong&email=x@example.org"
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        add(app, cookie, "not a mailbox").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        add(app, cookie, "other@example.com").await,
        StatusCode::SEE_OTHER
    );
    assert!(!page(app, cookie).await.contains("other@example.com"));
    assert_eq!(count(db, "SELECT COUNT(*) FROM workflow_notices").await, 0);
    // An address a nonmember row created, owned by nobody.
    member(db, "Second@Example.org", MemberRole::Nonmember).await;
    assert_eq!(
        add(app, cookie, "second@example.org").await,
        StatusCode::SEE_OTHER
    );
    let html = page(app, cookie).await;
    assert!(html.contains("<h2>second@example.org</h2>"), "{html}");
    assert!(html.contains("Unverified"), "{html}");
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM workflow_notices", 1),
        (
            "SELECT COUNT(*) FROM account_tokens WHERE purpose='verify_address' AND consumed_at IS NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='address.add'",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    assert_eq!(
        add(app, cookie, "second@example.org").await,
        StatusCode::SEE_OTHER
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM workflow_notices").await, 1);
}

/// Promotion needs a verified address that is the reader's; the page only
/// offers the actions that apply.
async fn promoted(db: &Database, app: &axum::Router, cookie: &str, reader: listmngr_core::UserId) {
    let html = page(app, cookie).await;
    let first = address_id(db, "reader@example.com").await;
    let second = address_id(db, "second@example.org").await;
    assert!(
        html.contains(&format!("/web/account/addresses/{second}/primary")),
        "the verified non-primary address offers promotion: {html}"
    );
    assert!(
        !html.contains(&format!("/web/account/addresses/{first}/remove")),
        "the primary address offers no removal"
    );
    assert_eq!(
        act(app, cookie, &first, "remove").await,
        StatusCode::BAD_REQUEST,
        "primary stays"
    );
    assert_eq!(
        act(app, cookie, &second, "primary").await,
        StatusCode::SEE_OTHER
    );
    let promoted = db.users().get(reader).await.unwrap();
    assert_eq!(promoted.preferred_address_id.unwrap().to_string(), second);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='address.primary'"
        )
        .await,
        1
    );
    let stranger = address_id(db, "other@example.com").await;
    for action in ["primary", "remove"] {
        assert_eq!(
            act(app, cookie, &stranger, action).await,
            StatusCode::NOT_FOUND,
            "a stranger's address is not ours to {action}"
        );
    }
}

/// Removal unlinks and unverifies, keeps the address's memberships, and
/// refuses the last verified address.
async fn removed(db: &Database, app: &axum::Router, cookie: &str) {
    let first = address_id(db, "reader@example.com").await;
    let second = address_id(db, "second@example.org").await;
    assert_eq!(
        act(app, cookie, &first, "remove").await,
        StatusCode::SEE_OTHER
    );
    assert!(
        !page(app, cookie)
            .await
            .contains("<h2>reader@example.com</h2>")
    );
    for (sql, expected) in [
        (
            "SELECT COUNT(*) FROM addresses WHERE email='reader@example.com' AND user_id IS NULL AND verified_on IS NULL",
            1,
        ),
        (
            "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='second@example.org'",
            1,
        ),
        (
            "SELECT COUNT(*) FROM audit_log WHERE action='address.remove'",
            1,
        ),
    ] {
        assert_eq!(count(db, sql).await, expected, "{sql}");
    }
    assert_eq!(
        login(app, "reader@example.com").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(app, "second@example.org").await,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        act(app, cookie, &second, "remove").await,
        StatusCode::BAD_REQUEST,
        "the last verified address, now primary and alone, stays"
    );
}
