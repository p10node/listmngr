//! The server owner's domains and accounts: `/web/admin/domains` with an
//! add form, one domain with its owners, template overrides, DKIM record
//! and deletion; `/web/admin/users` with a search and one account's page
//! with its display name, server-owner flag, addresses and memberships.
use super::{call, csrf, login_as, member, seeded_fixture_configured, text, user};
use axum::http::StatusCode;
use listmngr_core::{Config, MemberRole};
use listmngr_db::Database;

#[tokio::test]
async fn domains_and_accounts_administration() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_domains_users_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_domains_users")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    matrix(db).await;
    schema.drop().await.unwrap();
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// The page contains `needle`; the whole page on failure.
fn has(html: &str, needle: &str) {
    assert!(html.contains(needle), "expected {needle:?} in: {html}");
}

/// The page does not contain `needle`; the whole page on failure.
fn lacks(html: &str, needle: &str) {
    assert!(!html.contains(needle), "unexpected {needle:?} in: {html}");
}

fn encode(fields: &[(&str, &str)]) -> String {
    serde_urlencoded::to_string(fields).unwrap()
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

async fn status(
    app: &axum::Router,
    method: &str,
    path: &str,
    cookie: &str,
    body: &str,
) -> StatusCode {
    call(app, method, path, cookie, body).await.status()
}

/// A fresh RSA key the way an operator would make one, readable by nobody
/// else, so the domain page can show its DNS record.
fn dkim_key(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("dkim.pem");
    let generated = std::process::Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&path)
        .status()
        .expect("openssl");
    assert!(generated.success(), "RSA fixture generation failed");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

/// An Ed25519 key as `listmngr dkim gen` writes one.
fn ed25519_key(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("ed25519.pem");
    std::fs::write(
        &path,
        listmngr_mail::dkim::generate_key(listmngr_mail::dkim::Algorithm::Ed25519, 0).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

async fn matrix(db: Database) {
    let keys = tempfile::tempdir().unwrap();
    let key = dkim_key(keys.path());
    let ed_key = ed25519_key(keys.path());
    let (db, app) = seeded_fixture_configured(db, |config: &mut Config| {
        config
            .mta
            .dkim_signing
            .push(listmngr_core::DkimSigningConfig {
                domain: "example.com".into(),
                selector: "sel".into(),
                private_key_file: key.clone(),
            });
        config
            .mta
            .dkim_signing
            .push(listmngr_core::DkimSigningConfig {
                domain: "example.com".into(),
                selector: "ed".into(),
                private_key_file: ed_key.clone(),
            });
        // The ARC sealing key is published the same way, on its domain.
        config.mta.arc = listmngr_core::ArcConfig {
            enabled: true,
            domain: "example.com".into(),
            selector: "arc".into(),
            private_key_file: Some(key.clone()),
        };
    })
    .await;
    let root_user = user(&db, "root@example.com", true).await;
    let plain_user = user(&db, "plain@example.com", false).await;
    member(&db, "plain@example.com", MemberRole::Member).await;
    user(&db, "downer@example.com", false).await;
    let root = login_as(&app, "root@example.com").await;
    let plain = login_as(&app, "plain@example.com").await;

    doors(&app, &root, &plain).await;
    domain_index_and_add(&db, &app, &root).await;
    domain_owners(&db, &app, &root).await;
    domain_templates(&db, &app, &root).await;
    domain_delete(&db, &app, &root).await;
    user_search(&app, &root, &plain_user.id.to_string()).await;
    user_page_addresses(&db, &app, &root, &plain_user.id.to_string()).await;
    user_page_roles(
        &db,
        &app,
        &root,
        &root_user.id.to_string(),
        &plain_user.id.to_string(),
    )
    .await;
}

/// The site-wide pages open to server owners only; the administration index
/// links them for those readers alone.
async fn doors(app: &axum::Router, root: &str, plain: &str) {
    let html = page(app, "/web/admin", root).await;
    has(&html, "/web/admin/domains");
    has(&html, "/web/admin/users");
    let html = page(app, "/web/admin", plain).await;
    lacks(&html, "/web/admin/domains");
    for path in ["/web/admin/domains", "/web/admin/users"] {
        assert_eq!(
            status(app, "GET", path, "", "").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(app, "GET", path, plain, "").await,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        status(app, "GET", "/web/admin/domains/example.com", plain, "").await,
        StatusCode::FORBIDDEN
    );
}

/// The index lists every domain with its list count; the add form refuses
/// a bad host and a taken one inline and lands on the new domain's page.
async fn domain_index_and_add(db: &Database, app: &axum::Router, root: &str) {
    let html = page(app, "/web/admin/domains", root).await;
    has(&html, "example.com");
    has(&html, "<td>2</td>");
    has(&html, "id=\"mail_host\"");
    let token = csrf(&html);
    let before = audit_count(db, "domain.create").await;
    for (host, alias, needle) in [
        ("not a host!", "", "Enter a valid mail host"),
        ("example.com", "", "already a domain"),
        ("lists.example.org", "bad alias!", "Enter a valid mail host"),
    ] {
        let response = call(
            app,
            "POST",
            "/web/admin/domains",
            root,
            &encode(&[
                ("csrf", &token),
                ("mail_host", host),
                ("description", "Kept <b>"),
                ("alias_domain", alias),
            ]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{host}");
        let html = text(response).await;
        has(&html, "id=\"mail_host-error\"");
        assert!(html.contains(needle), "{host}: {html}");
        has(&html, "value=\"Kept &lt;b&gt;\"");
    }
    assert_eq!(audit_count(db, "domain.create").await, before);
    let response = call(
        app,
        "POST",
        "/web/admin/domains",
        root,
        &encode(&[
            ("csrf", &token),
            ("mail_host", "Lists.Example.ORG"),
            ("description", "Second domain <b>"),
            ("alias_domain", "mx.example.org"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/web/admin/domains/lists.example.org?saved=added"
    );
    assert_eq!(audit_count(db, "domain.create").await, before + 1);
    let html = page(
        app,
        "/web/admin/domains/lists.example.org?saved=added",
        root,
    )
    .await;
    has(&html, "The domain was added");
    has(&html, "Second domain &lt;b&gt;");
    has(&html, "mx.example.org");
    has(&html, "No owners yet");
    has(&html, "No DKIM signing key");
    assert_eq!(
        status(app, "GET", "/web/admin/domains/nowhere.example", root, "").await,
        StatusCode::NOT_FOUND
    );
    let html = page(app, "/web/admin/domains/example.com", root).await;
    has(&html, "sel._domainkey.example.com");
    has(&html, "arc._domainkey.example.com");
    has(&html, "v=DKIM1; k=rsa; p=");
    has(&html, "ed._domainkey.example.com");
    has(&html, "v=DKIM1; k=ed25519; p=");
    lacks(&html, "PRIVATE KEY");
}

/// Owners are added by an address that belongs to an account and removed
/// one at a time, each an audited write.
async fn domain_owners(db: &Database, app: &axum::Router, root: &str) {
    let base = "/web/admin/domains/lists.example.org";
    let token = csrf(&page(app, base, root).await);
    let response = call(
        app,
        "POST",
        &format!("{base}/owners"),
        root,
        &encode(&[("csrf", &token), ("email", "nobody@example.org")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let html = text(response).await;
    has(&html, "id=\"email-error\"");
    has(&html, "No account has this address");
    has(&html, "value=\"nobody@example.org\"");
    let added = audit_count(db, "domain.owner.add").await;
    let response = call(
        app,
        "POST",
        &format!("{base}/owners"),
        root,
        &encode(&[("csrf", &token), ("email", "Downer@Example.com")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(audit_count(db, "domain.owner.add").await, added + 1);
    let html = page(app, &format!("{base}?saved=owner"), root).await;
    has(&html, "The owner was added");
    has(&html, "downer@example.com");
    let response = call(
        app,
        "POST",
        &format!("{base}/owners"),
        root,
        &encode(&[("csrf", &token), ("email", "downer@example.com")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    has(&text(response).await, "already owns");
    let html = page(app, "/web/admin/domains", root).await;
    has(&html, "downer@example.com");
    domain_owner_removal(db, app, root, &token).await;
}

/// Removing an owner is audited; a second removal finds nobody; a write
/// without the session's CSRF token is refused.
async fn domain_owner_removal(db: &Database, app: &axum::Router, root: &str, token: &str) {
    let base = "/web/admin/domains/lists.example.org";
    let owner = db.users().get_by_email("downer@example.com").await.unwrap();
    let removed = audit_count(db, "domain.owner.remove").await;
    let response = call(
        app,
        "POST",
        &format!("{base}/owners/{}/remove", owner.id),
        root,
        &encode(&[("csrf", token)]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(audit_count(db, "domain.owner.remove").await, removed + 1);
    lacks(&page(app, base, root).await, "downer@example.com");
    assert_eq!(
        status(
            app,
            "POST",
            &format!("{base}/owners/{}/remove", owner.id),
            root,
            &encode(&[("csrf", token)])
        )
        .await,
        StatusCode::NOT_FOUND,
        "not an owner any more"
    );
    assert_eq!(
        status(app, "POST", &format!("{base}/owners"), root, "email=x").await,
        StatusCode::FORBIDDEN,
        "no CSRF"
    );
}

/// A domain template override applies to a list on the domain without its
/// own, shows in the catalogue, and its removal restores the built-in.
async fn domain_templates(db: &Database, app: &axum::Router, root: &str) {
    let editor = "/web/admin/domains/lists.example.org/templates/list:user:notice:welcome";
    let list = db
        .lists()
        .create(listmngr_db::NewList {
            list_id: "news.lists.example.org".parse().unwrap(),
            display_name: "News".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let html = page(app, editor, root).await;
    has(&html, "builtin:en");
    let token = csrf(&html);
    let response = call(
        app,
        "POST",
        editor,
        root,
        &encode(&[
            ("csrf", &token),
            ("preview", "1"),
            ("language", "en"),
            ("body", "Welcome to $domain <b>"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = text(response).await;
    has(&html, "Welcome to lists.example.org &lt;b&gt;");
    let stored = count(db, "SELECT COUNT(*) FROM templates WHERE scope='domain'").await;
    assert_eq!(stored, 0, "a preview writes nothing");
    let response = call(
        app,
        "POST",
        editor,
        root,
        &encode(&[
            ("csrf", &token),
            ("language", "en"),
            ("body", "Domain welcome for $domain"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let html = page(app, &format!("{editor}?language=en"), root).await;
    has(&html, "Domain welcome for $domain");
    has(&html, "domain:en");
    let resolved = db
        .templates()
        .resolve("list:user:notice:welcome", &list, "en")
        .await
        .unwrap();
    assert_eq!(resolved.body, "Domain welcome for $domain");
    assert_eq!(resolved.source, "domain:en");
    let html = page(app, "/web/admin/domains/lists.example.org", root).await;
    has(
        &html,
        "list:user:notice:welcome</code></a></td><td><code>en</code>",
    );
    assert_eq!(
        status(
            app,
            "GET",
            "/web/admin/domains/lists.example.org/templates/no:such",
            root,
            ""
        )
        .await,
        StatusCode::NOT_FOUND
    );
    let response = call(
        app,
        "POST",
        &format!("{editor}/remove"),
        root,
        &encode(&[("csrf", &token)]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let resolved = db
        .templates()
        .resolve("list:user:notice:welcome", &list, "en")
        .await
        .unwrap();
    assert_eq!(resolved.source, "builtin:en");
}

/// Deletion needs the host typed back and an empty domain.
async fn domain_delete(db: &Database, app: &axum::Router, root: &str) {
    let base = "/web/admin/domains/lists.example.org";
    let token = csrf(&page(app, base, root).await);
    let response = call(
        app,
        "POST",
        &format!("{base}/delete"),
        root,
        &encode(&[("csrf", &token), ("confirm", "lists.example.com")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    has(&text(response).await, "does not match");
    let response = call(
        app,
        "POST",
        &format!("{base}/delete"),
        root,
        &encode(&[("csrf", &token), ("confirm", "lists.example.org")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    has(&text(response).await, "still carries lists");
    db.lists()
        .delete(&"news.lists.example.org".parse().unwrap())
        .await
        .unwrap();
    let deleted = audit_count(db, "domain.delete").await;
    let response = call(
        app,
        "POST",
        &format!("{base}/delete"),
        root,
        &encode(&[("csrf", &token), ("confirm", "Lists.Example.org")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/web/admin/domains?saved=deleted"
    );
    assert_eq!(audit_count(db, "domain.delete").await, deleted + 1);
    assert_eq!(
        status(app, "GET", base, root, "").await,
        StatusCode::NOT_FOUND
    );
    has(
        &page(app, "/web/admin/domains?saved=deleted", root).await,
        "The domain was deleted",
    );
}

/// The search finds accounts by name or address and pages with the search.
async fn user_search(app: &axum::Router, root: &str, plain_id: &str) {
    let html = page(app, "/web/admin/users", root).await;
    has(&html, "plain@example.com");
    has(&html, "root@example.com");
    has(&html, &format!("/web/admin/users/{plain_id}"));
    let html = page(app, "/web/admin/users?q=PLAIN", root).await;
    has(&html, "plain@example.com");
    lacks(&html, "root@example.com");
    let html = page(app, "/web/admin/users?q=absent", root).await;
    has(&html, "No accounts match");
    assert_eq!(
        status(
            app,
            "GET",
            &format!("/web/admin/users?q={}", "x".repeat(201)),
            root,
            ""
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        status(
            app,
            "GET",
            "/web/admin/users/00000000-0000-0000-0000-000000000000",
            root,
            ""
        )
        .await,
        StatusCode::NOT_FOUND
    );
}

/// The account page lists addresses and memberships; an address can be
/// marked verified or not without a mailbox proof, each an audited write.
async fn user_page_addresses(db: &Database, app: &axum::Router, root: &str, plain_id: &str) {
    let path = format!("/web/admin/users/{plain_id}");
    let html = page(app, &path, root).await;
    has(&html, "plain@example.com");
    has(&html, "public.example.com");
    has(&html, "Member");
    has(&html, "Mark unverified");
    let token = csrf(&html);
    let address = db.addresses().get("plain@example.com").await.unwrap();
    let unverified = audit_count(db, "address.unverify").await;
    let response = call(
        app,
        "POST",
        &format!("{path}/addresses/{}/verify", address.id),
        root,
        &encode(&[("csrf", &token), ("verified", "false")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(audit_count(db, "address.unverify").await, unverified + 1);
    assert!(
        db.addresses()
            .get("plain@example.com")
            .await
            .unwrap()
            .verified_on
            .is_none()
    );
    let html = page(app, &format!("{path}?saved=address"), root).await;
    has(&html, "The address was updated");
    has(&html, "Mark verified");
    let response = call(
        app,
        "POST",
        &format!("{path}/addresses/{}/verify", address.id),
        root,
        &encode(&[("csrf", &token), ("verified", "true")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        db.addresses()
            .get("plain@example.com")
            .await
            .unwrap()
            .verified_on
            .is_some()
    );
    let other = db.addresses().get("root@example.com").await.unwrap();
    assert_eq!(
        status(
            app,
            "POST",
            &format!("{path}/addresses/{}/verify", other.id),
            root,
            &encode(&[("csrf", &token), ("verified", "false")])
        )
        .await,
        StatusCode::NOT_FOUND,
        "another account's address"
    );
    assert_eq!(
        status(
            app,
            "POST",
            &format!("{path}/addresses/{}/verify", address.id),
            root,
            &encode(&[("csrf", &token), ("verified", "maybe")])
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

/// The display name and server-owner flag save together; the last server
/// owner cannot be demoted; an empty name is refused inline.
async fn user_page_roles(
    db: &Database,
    app: &axum::Router,
    root: &str,
    root_id: &str,
    plain_id: &str,
) {
    let path = format!("/web/admin/users/{plain_id}");
    let token = csrf(&page(app, &path, root).await);
    let updated = audit_count(db, "user.update").await;
    let response = call(
        app,
        "POST",
        &path,
        root,
        &encode(&[
            ("csrf", &token),
            ("display_name", "Plain <b>"),
            ("server_owner", "true"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(audit_count(db, "user.update").await, updated + 1);
    let plain = db.users().get(plain_id.parse().unwrap()).await.unwrap();
    assert_eq!(plain.display_name, "Plain <b>");
    assert!(plain.is_server_owner);
    let html = page(app, &format!("{path}?saved=1"), root).await;
    has(&html, "The account was saved");
    has(&html, "value=\"Plain &lt;b&gt;\"");
    let response = call(
        app,
        "POST",
        &path,
        root,
        &encode(&[
            ("csrf", &token),
            ("display_name", "   "),
            ("server_owner", "true"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    has(&text(response).await, "Enter a display name");
    // Demote the plain account again (root still owns the server), then
    // try to demote root, now the last owner.
    let response = call(
        app,
        "POST",
        &path,
        root,
        &encode(&[
            ("csrf", &token),
            ("display_name", "Plain"),
            ("server_owner", "false"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let root_path = format!("/web/admin/users/{root_id}");
    let token = csrf(&page(app, &root_path, root).await);
    let response = call(
        app,
        "POST",
        &root_path,
        root,
        &encode(&[
            ("csrf", &token),
            ("display_name", "Root"),
            ("server_owner", "false"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    has(&text(response).await, "last server owner");
    assert!(
        db.users()
            .get(root_id.parse().unwrap())
            .await
            .unwrap()
            .is_server_owner
    );
}
