//! The list directory with its filters and role badges, the create-list form
//! and the list summary: `/web`, `/web/lists/new` and `/web/lists/{id}`.
//! Creating a list is one audited transaction that also seats its first
//! owner; an anonymous visitor can then find the list and ask to join it.
use super::{call, cookie, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::{ListId, MemberRole};
use listmngr_db::Database;

#[tokio::test]
async fn directory_filters_list_creation_and_summary() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_list_create_index_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_list_create_index")
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

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

fn encode(fields: &[(&str, &str)]) -> String {
    serde_urlencoded::to_string(fields).unwrap()
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

fn entries(html: &str) -> usize {
    html.matches("<li>").count()
}

/// The create form with every field, some overridden.
fn create_form(csrf: &str, overrides: &[(&str, &str)]) -> String {
    let mut fields: Vec<(&str, &str)> = vec![
        ("csrf", csrf),
        ("list_name", "announce"),
        ("domain", "example.com"),
        ("display_name", "Announcements <b>"),
        ("owner", "root@example.com"),
        ("style", "legacy-announce"),
        ("advertised", "false"),
        ("description", "Site news & notices"),
    ];
    for (name, value) in overrides {
        if let Some(slot) = fields.iter_mut().find(|(field, _)| field == name) {
            slot.1 = value;
        }
    }
    encode(&fields)
}

async fn matrix(db: Database, app: axum::Router) {
    db.domains()
        .create("other.example", "", None)
        .await
        .unwrap();
    user(&db, "root@example.com", true).await;
    let domain_owner = user(&db, "domain-owner@example.com", false).await;
    db.domains()
        .add_owner("other.example", domain_owner.id)
        .await
        .unwrap();
    user(&db, "plain@example.com", false).await;
    member(&db, "plain@example.com", MemberRole::Member).await;
    user(&db, "private-owner@example.com", false).await;
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "private.example.com".parse().unwrap(),
            email: "private-owner@example.com".into(),
            role: MemberRole::Owner,
            subscription_mode: listmngr_core::SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    let root = login_as(&app, "root@example.com").await;
    let domain_owner = login_as(&app, "domain-owner@example.com").await;
    let plain = login_as(&app, "plain@example.com").await;
    let private_owner = login_as(&app, "private-owner@example.com").await;

    directory_filters(&app, &plain, &private_owner, &root).await;
    create_form_authority(&app, &root, &domain_owner, &plain).await;
    create_refusals(&db, &app, &root, &domain_owner).await;
    create_then_subscribe(&db, &app, &root, &domain_owner).await;
    summary(&app, &private_owner, &plain, &root).await;
}

/// The directory searches and filters by domain, shows only advertised
/// lists to a visitor, adds the reader's own unadvertised lists on request,
/// and marks the reader's role on each list.
async fn directory_filters(app: &axum::Router, plain: &str, private_owner: &str, root: &str) {
    visitor_directory(app).await;
    reader_directory(app, plain, private_owner, root).await;
}

/// A visitor: advertised lists, searched and filtered, never an unadvertised
/// one, no create link.
async fn visitor_directory(app: &axum::Router) {
    let html = page(app, "/web", "").await;
    for (present, needle) in [
        (true, "public.example.com"),
        (false, "private.example.com"),
        (true, "Search lists"),
        (true, "name=\"domain\""),
        (false, "/web/lists/new"),
    ] {
        assert_eq!(html.contains(needle), present, "{needle}: {html}");
    }
    assert_eq!(entries(&html), 1);
    assert_eq!(entries(&page(app, "/web?q=PUBLIC", "").await), 1);
    assert_eq!(
        entries(&page(app, "/web?q=alert", "").await),
        1,
        "display name"
    );
    let html = page(app, "/web?q=absent", "").await;
    assert_eq!(entries(&html), 0);
    assert!(html.contains("No lists match"), "{html}");
    assert_eq!(
        entries(&page(app, "/web?q=%25", "").await),
        0,
        "a literal %"
    );
    assert_eq!(entries(&page(app, "/web?domain=example.com", "").await), 1);
    assert_eq!(
        entries(&page(app, "/web?domain=other.example", "").await),
        0
    );
    assert_eq!(
        entries(&page(app, "/web?q=public&domain=example.com&page=0", "").await),
        1
    );
    for query in [
        format!("q={}", "x".repeat(201)),
        format!("domain={}", "d".repeat(254)),
        "show=maybe".into(),
    ] {
        assert_eq!(
            call(app, "GET", &format!("/web?{query}"), "", "")
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    assert!(
        !page(app, "/web?show=all", "")
            .await
            .contains("private.example.com"),
        "a visitor never sees an unadvertised list"
    );
}

/// Signed-in readers: the role badge, the opt-in scope for their own
/// unadvertised lists, everything for a server owner, and the create link
/// only for those who may use it.
async fn reader_directory(app: &axum::Router, plain: &str, private_owner: &str, root: &str) {
    let html = page(app, "/web", plain).await;
    assert!(html.contains("Member"), "the reader's role badge: {html}");
    assert!(!html.contains("private.example.com"));
    assert!(
        !html.contains("/web/lists/new"),
        "a plain reader cannot create"
    );
    let html = page(app, "/web?show=all", plain).await;
    assert!(!html.contains("private.example.com"), "no role, no listing");
    assert!(
        !page(app, "/web", private_owner)
            .await
            .contains("private.example.com"),
        "unadvertised lists stay out of the default listing"
    );
    let html = page(app, "/web?show=all", private_owner).await;
    assert!(html.contains("private.example.com"), "{html}");
    assert!(html.contains("Owner"), "{html}");
    assert!(html.contains("not in the public directory"), "{html}");
    let html = page(app, "/web?show=all", root).await;
    assert!(
        html.contains("private.example.com"),
        "a server owner sees every list"
    );
    assert!(html.contains("/web/lists/new"), "the create link: {html}");
    assert!(
        page(app, "/web/admin", root)
            .await
            .contains("/web/lists/new")
    );
}

/// Only a server owner or the owner of a domain reaches the form, and the
/// domain choice is exactly the domains they may create on.
async fn create_form_authority(app: &axum::Router, root: &str, domain_owner: &str, plain: &str) {
    assert_eq!(
        call(app, "GET", "/web/lists/new", "", "").await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(app, "GET", "/web/lists/new", plain, "").await.status(),
        StatusCode::FORBIDDEN
    );
    let html = page(app, "/web/lists/new", domain_owner).await;
    assert!(html.contains("value=\"other.example\""), "{html}");
    assert!(!html.contains("value=\"example.com\""), "{html}");
    assert!(
        html.contains("value=\"domain-owner@example.com\""),
        "the owner field prefilled: {html}"
    );
    let html = page(app, "/web/lists/new", root).await;
    assert!(html.contains("value=\"other.example\""));
    assert!(html.contains("value=\"example.com\""));
    for style in ["legacy-default", "legacy-announce", "private-default"] {
        assert!(html.contains(&format!("value=\"{style}\"")), "{style}");
    }
}

/// A refused form comes back as a 400 with the refusal on its field and the
/// other values kept; authority is checked on the domain actually posted.
async fn create_refusals(db: &Database, app: &axum::Router, root: &str, domain_owner: &str) {
    let before = audit_count(db, "list.create").await;
    let token = csrf(&page(app, "/web/lists/new", root).await);
    for (overrides, field) in [
        (vec![("list_name", "bad name!")], "list_name"),
        (vec![("list_name", "")], "list_name"),
        (vec![("list_name", "public")], "list_name"),
        (vec![("domain", "unknown.example")], "domain"),
        (vec![("owner", "not-an-address")], "owner"),
        (vec![("style", "no-such-style")], "style"),
    ] {
        let response = call(
            app,
            "POST",
            "/web/lists/new",
            root,
            &create_form(&token, &overrides),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{field}");
        let html = text(response).await;
        assert!(
            html.contains(&format!("id=\"{field}-error\"")),
            "{field}: {html}"
        );
        assert!(
            html.contains("value=\"Announcements &lt;b&gt;\""),
            "the other values kept: {html}"
        );
    }
    let response = call(
        app,
        "POST",
        "/web/lists/new",
        root,
        &create_form(&token, &[("list_name", "public")]),
    )
    .await;
    assert!(text(response).await.contains("already exists"));
    let token = csrf(&page(app, "/web/lists/new", domain_owner).await);
    let response = call(
        app,
        "POST",
        "/web/lists/new",
        domain_owner,
        &create_form(&token, &[("domain", "example.com")]),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a domain owner cannot create on another domain"
    );
    for body in ["", "csrf=wrong"] {
        assert_eq!(
            call(app, "POST", "/web/lists/new", root, body)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(audit_count(db, "list.create").await, before);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM mailing_lists WHERE list_id='announce.example.com'"
        )
        .await,
        0
    );
}

/// A list is created with its settings and first owner in one transaction,
/// the creator lands on its settings, and a visitor can find it and ask to
/// join.
async fn create_then_subscribe(db: &Database, app: &axum::Router, root: &str, domain_owner: &str) {
    create_as_server_owner(db, app, root).await;
    create_as_domain_owner_then_subscribe(db, app, domain_owner).await;
}

/// The server owner creates an unadvertised announce list on the site's
/// domain: settings, owner and audit rows land together; only they and the
/// list's people can open it.
async fn create_as_server_owner(db: &Database, app: &axum::Router, root: &str) {
    let created = audit_count(db, "list.create").await;
    let members = audit_count(db, "member.create").await;
    let token = csrf(&page(app, "/web/lists/new", root).await);
    let response = call(
        app,
        "POST",
        "/web/lists/new",
        root,
        &create_form(&token, &[]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/web/lists/announce.example.com/settings"
    );
    stored_as_submitted(db).await;
    assert_eq!(audit_count(db, "list.create").await, created + 1);
    assert_eq!(audit_count(db, "member.create").await, members + 1);
    assert_eq!(
        call(app, "GET", "/web/lists/announce.example.com", "", "")
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "not advertised"
    );
    let html = page(app, "/web/lists/announce.example.com", root).await;
    assert!(html.contains("not in the public directory"), "{html}");
    assert!(
        page(app, "/web/lists/announce.example.com/settings", root)
            .await
            .contains("Announcements &lt;b&gt;")
    );
}

/// The announce list's row and owner as the form described them.
async fn stored_as_submitted(db: &Database) {
    let id: ListId = "announce.example.com".parse().unwrap();
    let list = db.lists().get(&id).await.unwrap();
    assert_eq!(list.display_name, "Announcements <b>");
    assert_eq!(list.description, "Site news & notices");
    assert_eq!(list.style_name, "legacy-announce");
    assert!(!list.advertised);
    let owners = db.members().roster(&id, MemberRole::Owner).await.unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(
        db.addresses().get("root@example.com").await.unwrap().id,
        owners[0].address_id
    );
}

/// The domain owner creates an advertised list on their domain, seating an
/// owner address the site has never seen; a visitor then finds it by domain
/// and asks to join.
async fn create_as_domain_owner_then_subscribe(
    db: &Database,
    app: &axum::Router,
    domain_owner: &str,
) {
    let token = csrf(&page(app, "/web/lists/new", domain_owner).await);
    let response = call(
        app,
        "POST",
        "/web/lists/new",
        domain_owner,
        &create_form(
            &token,
            &[
                ("list_name", "Team"),
                ("domain", "other.example"),
                ("display_name", "Team"),
                ("owner", "Lead@Example.ORG"),
                ("style", "legacy-default"),
                ("advertised", "true"),
                ("description", ""),
            ],
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let id: ListId = "team.other.example".parse().unwrap();
    let owners = db.members().roster(&id, MemberRole::Owner).await.unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(
        db.addresses().get("lead@example.org").await.unwrap().id,
        owners[0].address_id
    );
    let html = page(app, "/web?domain=other.example", "").await;
    assert_eq!(entries(&html), 1);
    assert!(html.contains("team.other.example"));
    let response = call(app, "GET", "/web/lists/team.other.example", "", "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let anonymous = cookie(&response);
    let html = text(response).await;
    assert!(
        html.contains("team@other.example"),
        "the posting address: {html}"
    );
    assert!(
        html.contains("team-owner@other.example"),
        "the owner address: {html}"
    );
    let token = csrf(&html);
    let response = call(
        app,
        "POST",
        "/web/lists/team.other.example/request",
        &anonymous,
        &encode(&[
            ("csrf", &token),
            ("email", "joiner@example.net"),
            ("action", "join"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM subscription_workflows WHERE list_id='team.other.example'"
        )
        .await,
        1,
        "the join request waits for its confirmation"
    );
}

/// The summary names the list's addresses and policies; an unadvertised
/// list is a page only for the people with a role on it.
async fn summary(app: &axum::Router, private_owner: &str, plain: &str, root: &str) {
    let html = page(app, "/web/lists/public.example.com", "").await;
    for expected in [
        "public@example.com",
        "public-owner@example.com",
        "Archive policy",
        "Subscription policy",
    ] {
        assert!(html.contains(expected), "{expected}: {html}");
    }
    assert!(!html.contains("Your role"), "a visitor has no role");
    let html = page(app, "/web/lists/public.example.com", plain).await;
    assert!(html.contains("Your role"), "{html}");
    assert!(html.contains("Member"), "{html}");
    assert_eq!(
        call(app, "GET", "/web/lists/private.example.com", plain, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let html = page(app, "/web/lists/private.example.com", private_owner).await;
    assert!(html.contains("Owner"), "{html}");
    assert!(html.contains("private@example.com"));
    assert!(
        html.contains("/web/lists/private.example.com/settings"),
        "{html}"
    );
    assert!(
        page(app, "/web/lists/private.example.com", root)
            .await
            .contains("private-owner@example.com")
    );
}
