//! The owner's member management: rosters of every role with search, an
//! htmx fragment with a full-page fallback, one member's options and bounce
//! state, mass subscription from a textarea or a file, mass removal, and a
//! CSV export. Every write is one audited transaction under the owner's
//! live authority.
use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::Database;
use tower::ServiceExt as _;

const ROSTER: &str = "/web/lists/public.example.com/members";

#[tokio::test]
async fn rosters_options_mass_changes_and_export() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_members_admin_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_members_admin")
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

fn encode(fields: &[(&str, &str)]) -> String {
    serde_urlencoded::to_string(fields).unwrap()
}

async fn page(app: &axum::Router, path: &str, cookie: &str) -> String {
    let response = call(app, "GET", path, cookie, "").await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    text(response).await
}

async fn post(
    app: &axum::Router,
    path: &str,
    cookie: &str,
    body: &str,
) -> axum::response::Response {
    call(app, "POST", path, cookie, body).await
}

async fn role_member(db: &Database, email: &str, role: MemberRole) -> listmngr_core::Member {
    db.members()
        .create(listmngr_db::NewMember {
            list_id: "public.example.com".parse().unwrap(),
            email: email.into(),
            role,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: email.into(),
        })
        .await
        .unwrap()
}

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "roster-owner@example.com", false).await;
    member(&db, "roster-owner@example.com", MemberRole::Owner).await;
    let owner = login_as(&app, "roster-owner@example.com").await;
    user(&db, "roster-member@example.com", false).await;
    let plain = member(&db, "roster-member@example.com", MemberRole::Member).await;
    let outsider = login_as(&app, "roster-member@example.com").await;
    role_member(&db, "roster-moderator@example.com", MemberRole::Moderator).await;
    role_member(&db, "roster-stranger@example.com", MemberRole::Nonmember).await;

    rosters(&app, &owner, &outsider).await;
    fragment(&app, &owner).await;
    options(&db, &app, &owner, plain.id).await;
    bounce(&db, &app, &owner, plain.id).await;
    mass_subscribe(&db, &app, &owner).await;
    mass_subscribe_file(&db, &app, &owner).await;
    mass_remove(&db, &app, &owner).await;
    export(&app, &owner, &outsider).await;
}

/// Every role has a roster; a member who owns nothing is refused.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn rosters(app: &axum::Router, owner: &str, outsider: &str) {
    let members = page(app, ROSTER, owner).await;
    for role in ["owner", "moderator", "nonmember"] {
        assert!(
            members.contains(&format!("{ROSTER}?role={role}")),
            "{role} tab: {members}"
        );
    }
    assert!(members.contains("roster-member@example.com"), "{members}");
    assert!(
        !members.contains("roster-moderator@example.com"),
        "{members}"
    );
    assert!(
        members.contains(&format!("{ROSTER}/subscribe")),
        "mass subscribe link: {members}"
    );
    assert!(
        members.contains(&format!("{ROSTER}/export.csv")),
        "export link: {members}"
    );
    assert!(
        members.contains("name=\"member\""),
        "bulk selection: {members}"
    );
    let moderators = page(app, &format!("{ROSTER}?role=moderator"), owner).await;
    assert!(
        moderators.contains("roster-moderator@example.com"),
        "{moderators}"
    );
    assert!(
        !moderators.contains("roster-member@example.com"),
        "{moderators}"
    );
    let owners = page(app, &format!("{ROSTER}?role=owner"), owner).await;
    assert!(owners.contains("roster-owner@example.com"), "{owners}");
    let nonmembers = page(app, &format!("{ROSTER}?role=nonmember"), owner).await;
    assert!(
        nonmembers.contains("roster-stranger@example.com"),
        "{nonmembers}"
    );
    assert!(
        nonmembers.contains("Posting policy"),
        "nonmembers have a posting policy: {nonmembers}"
    );
    assert_eq!(
        call(app, "GET", &format!("{ROSTER}?role=bogus"), owner, "")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    for path in [
        ROSTER.to_owned(),
        format!("{ROSTER}?role=owner"),
        format!("{ROSTER}/subscribe"),
        format!("{ROSTER}/export.csv"),
    ] {
        assert_eq!(
            call(app, "GET", &path, outsider, "").await.status(),
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
}

/// The search answers an htmx request with the roster alone and a plain
/// request with the whole page.
async fn fragment(app: &axum::Router, owner: &str) {
    let path = format!("{ROSTER}?q=roster-mem");
    let full = page(app, &path, owner).await;
    assert!(full.contains("<html"), "{full}");
    assert!(full.contains("roster-member@example.com"));
    assert!(
        full.contains("/web/htmx.min.js"),
        "the page loads htmx from this origin: {full}"
    );
    let request = Request::builder()
        .method("GET")
        .uri(&path)
        .header("host", "localhost")
        .header("cookie", owner)
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'"),
        "the page CSP allows the first-party script"
    );
    let partial = text(response).await;
    assert!(!partial.contains("<html"), "a fragment: {partial}");
    assert!(partial.contains("<article>"), "{partial}");
    assert!(partial.contains("roster-member@example.com"));
    assert!(!partial.contains("roster-owner@example.com"));
}

/// One member's page: the options form changes the member row and its
/// preferences in one audited transaction; the role can change too.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn options(db: &Database, app: &axum::Router, owner: &str, member: listmngr_core::MemberId) {
    let path = format!("{ROSTER}/{member}");
    let html = page(app, &path, owner).await;
    assert!(html.contains("roster-member@example.com"), "{html}");
    for name in [
        "moderation_action",
        "display_name",
        "delivery_mode",
        "delivery_status",
        "acknowledge_posts",
        "hide_address",
        "receive_list_copy",
        "receive_own_postings",
        "preferred_language",
        "role",
    ] {
        assert!(html.contains(&format!("name=\"{name}\"")), "{name}: {html}");
    }
    // Every language a notice can be written in, Mailman's included.
    assert!(html.contains("value=\"pt-BR\""), "{html}");
    assert!(html.contains("Português (Brasil)"), "{html}");
    let token = csrf(&html);
    let audits = count(
        db,
        "SELECT COUNT(*) FROM audit_log WHERE action='member.update'",
    )
    .await;
    let saved = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("moderation_action", "hold"),
            ("display_name", "Roster <Member>"),
            ("delivery_mode", "mime_digests"),
            ("delivery_status", "by_user"),
            ("acknowledge_posts", "true"),
            ("hide_address", "default"),
            ("receive_list_copy", "false"),
            ("receive_own_postings", "true"),
            ("preferred_language", "pt-BR"),
            ("role", "member"),
        ]),
    )
    .await;
    assert_eq!(
        saved.status(),
        StatusCode::SEE_OTHER,
        "{}",
        text(saved).await
    );
    let stored = db.members().get(member).await.unwrap();
    assert_eq!(
        stored.moderation_action,
        Some(listmngr_core::ModerationAction::Hold)
    );
    assert_eq!(stored.display_name, "Roster <Member>");
    let preferences = db.preferences().get(stored.preferences_id).await.unwrap();
    assert_eq!(
        preferences.delivery_mode,
        Some(listmngr_core::DeliveryMode::MimeDigests)
    );
    assert_eq!(
        preferences.delivery_status,
        Some(listmngr_core::DeliveryStatus::ByUser)
    );
    assert_eq!(preferences.acknowledge_posts, Some(true));
    assert_eq!(preferences.hide_address, None);
    assert_eq!(preferences.receive_list_copy, Some(false));
    assert_eq!(preferences.receive_own_postings, Some(true));
    assert_eq!(preferences.preferred_language.as_deref(), Some("pt-BR"));
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='member.update'"
        )
        .await,
        audits + 1
    );
    let html = page(app, &path, owner).await;
    assert!(html.contains("Roster &lt;Member&gt;"), "{html}");
    assert!(html.contains("value=\"mime_digests\" selected"), "{html}");
    // A bad value is refused inline and nothing changes.
    let refused = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("moderation_action", "hold"),
            ("display_name", "Roster <Member>"),
            ("delivery_mode", "carrier-pigeon"),
            ("delivery_status", "by_user"),
            ("acknowledge_posts", "true"),
            ("hide_address", "default"),
            ("receive_list_copy", "false"),
            ("receive_own_postings", "true"),
            ("preferred_language", "vi"),
            ("role", "member"),
        ]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(text(refused).await.contains("id=\"delivery_mode-error\""));
    // A role change moves the row to another roster.
    let moved = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("moderation_action", "default"),
            ("display_name", "Roster <Member>"),
            ("delivery_mode", "default"),
            ("delivery_status", "default"),
            ("acknowledge_posts", "default"),
            ("hide_address", "default"),
            ("receive_list_copy", "default"),
            ("receive_own_postings", "default"),
            ("preferred_language", "default"),
            ("role", "moderator"),
        ]),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        db.members().get(member).await.unwrap().role,
        MemberRole::Moderator
    );
    let moderators = page(app, &format!("{ROSTER}?role=moderator"), owner).await;
    assert!(
        moderators.contains("roster-member@example.com"),
        "{moderators}"
    );
    let preferences = db
        .preferences()
        .get(db.members().get(member).await.unwrap().preferences_id)
        .await
        .unwrap();
    assert_eq!(
        preferences.delivery_mode, None,
        "default clears the member-level value"
    );
    // Back to member for the rest of the matrix.
    assert_eq!(
        post(
            app,
            &path,
            owner,
            &encode(&[
                ("csrf", &token),
                ("moderation_action", "default"),
                ("display_name", "Roster <Member>"),
                ("delivery_mode", "default"),
                ("delivery_status", "default"),
                ("acknowledge_posts", "default"),
                ("hide_address", "default"),
                ("receive_list_copy", "default"),
                ("receive_own_postings", "default"),
                ("preferred_language", "default"),
                ("role", "member"),
            ]),
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    // A member of another list is not reachable through this one.
    let foreign = db
        .members()
        .create(listmngr_db::NewMember {
            list_id: "private.example.com".parse().unwrap(),
            email: "elsewhere@example.com".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsAddress,
            display_name: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        call(app, "GET", &format!("{ROSTER}/{}", foreign.id), owner, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// Bounce information is shown, and delivery disabled by bounces can be
/// re-enabled with the score reset.
async fn bounce(db: &Database, app: &axum::Router, owner: &str, member: listmngr_core::MemberId) {
    let stored = db.members().get(member).await.unwrap();
    sqlx::query("UPDATE members SET bounce_score=5.5,last_bounce_received='2026-09-01T00:00:00Z' WHERE id=$1")
        .bind(member.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE preferences SET delivery_status='by_bounces' WHERE id=$1")
        .bind(stored.preferences_id.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let path = format!("{ROSTER}/{member}");
    let html = page(app, &path, owner).await;
    assert!(html.contains("5.5"), "bounce score: {html}");
    assert!(html.contains("2026-09-01"), "last bounce: {html}");
    assert!(html.contains(&format!("{path}/bounce/reset")), "{html}");
    let roster = page(app, ROSTER, owner).await;
    assert!(
        roster.contains("by_bounces"),
        "the roster says delivery is off: {roster}"
    );
    let token = csrf(&html);
    assert_eq!(
        post(
            app,
            &format!("{path}/bounce/reset"),
            owner,
            &encode(&[("csrf", &token)])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let stored = db.members().get(member).await.unwrap();
    assert!(stored.bounce_score.abs() < f64::EPSILON);
    assert_eq!(stored.last_bounce_received, None);
    let preferences = db.preferences().get(stored.preferences_id).await.unwrap();
    assert_eq!(
        preferences.delivery_status,
        Some(listmngr_core::DeliveryStatus::Enabled)
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='bounce.recover' AND diff LIKE '%owner%'"
        )
        .await,
        1
    );
}

/// Mass subscription from a textarea: each address reports what happened,
/// the workflow flags are honoured, and bad lines are named.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn mass_subscribe(db: &Database, app: &axum::Router, owner: &str) {
    let path = format!("{ROSTER}/subscribe");
    let html = page(app, &path, owner).await;
    for name in [
        "addresses",
        "role",
        "pre_verified",
        "pre_confirmed",
        "pre_approved",
        "invitation",
        "file",
    ] {
        assert!(html.contains(&format!("name=\"{name}\"")), "{name}: {html}");
    }
    let token = csrf(&html);
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"subscription_policy": "moderate"}),
        )
        .await
        .unwrap();
    let members_before = count(
        db,
        "SELECT COUNT(*) FROM members WHERE list_id='public.example.com' AND role='member'",
    )
    .await;
    let result = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("addresses", "Mass One <mass-one@example.org>\r\nmass-two@example.org\nnot an address\n\nMASS-ONE@example.org"),
            ("role", "member"),
            ("pre_verified", "true"),
            ("pre_confirmed", "true"),
            ("pre_approved", "true"),
        ]),
    )
    .await;
    assert_eq!(result.status(), StatusCode::OK);
    let result = text(result).await;
    assert!(result.contains("mass-one@example.org"), "{result}");
    assert!(result.contains("Subscribed"), "{result}");
    assert!(
        result.contains("not an address"),
        "the bad line is named: {result}"
    );
    assert!(
        result.contains("already"),
        "the duplicate is named: {result}"
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM members WHERE list_id='public.example.com' AND role='member'"
        )
        .await,
        members_before + 2
    );
    let one: (String,) = sqlx::query_as("SELECT m.display_name FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.email='mass-one@example.org'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(one.0, "Mass One");
    // Without the approval on a moderated list the request is held.
    let held = text(
        post(
            app,
            &path,
            owner,
            &encode(&[
                ("csrf", &token),
                ("addresses", "mass-three@example.org"),
                ("role", "member"),
                ("pre_verified", "true"),
                ("pre_confirmed", "true"),
            ]),
        )
        .await,
    )
    .await;
    assert!(held.contains("Held"), "{held}");
    assert_eq!(count(db, "SELECT COUNT(*) FROM subscription_workflows WHERE list_id='public.example.com' AND consumed=0").await, 1);
    // An invitation is held too, waiting for the address.
    let invited = text(
        post(
            app,
            &path,
            owner,
            &encode(&[
                ("csrf", &token),
                ("addresses", "mass-four@example.org"),
                ("role", "member"),
                ("invitation", "true"),
            ]),
        )
        .await,
    )
    .await;
    assert!(invited.contains("Held"), "{invited}");
    // Other roles are added directly.
    let moderators_before = count(
        db,
        "SELECT COUNT(*) FROM members WHERE list_id='public.example.com' AND role='moderator'",
    )
    .await;
    let direct = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("addresses", "mass-mod@example.org"),
            ("role", "moderator"),
        ]),
    )
    .await;
    assert_eq!(direct.status(), StatusCode::OK);
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM members WHERE list_id='public.example.com' AND role='moderator'"
        )
        .await,
        moderators_before + 1
    );
    assert!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='subscription.subscribe'"
        )
        .await
            >= 2
    );
    assert!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='member.create'"
        )
        .await
            >= 1
    );
    db.lists()
        .update(
            &"public.example.com".parse().unwrap(),
            &serde_json::json!({"subscription_policy": "open"}),
        )
        .await
        .unwrap();
}

/// The same form with the addresses in an uploaded file.
async fn mass_subscribe_file(db: &Database, app: &axum::Router, owner: &str) {
    let path = format!("{ROSTER}/subscribe");
    let token = csrf(&page(app, &path, owner).await);
    let boundary = "----listmngr-test-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"csrf\"\r\n\r\n{token}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"addresses\"\r\n\r\n\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"role\"\r\n\r\nmember\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"pre_verified\"\r\n\r\ntrue\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"pre_confirmed\"\r\n\r\ntrue\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"pre_approved\"\r\n\r\ntrue\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"members.txt\"\r\nContent-Type: text/plain\r\n\r\nfile-one@example.org\nFile Two <file-two@example.org>\n\r\n\
         --{boundary}--\r\n"
    );
    let request = Request::builder()
        .method("POST")
        .uri(&path)
        .header("host", "localhost")
        .header("cookie", owner)
        .header("origin", "http://localhost")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        text(response).await
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email IN ('file-one@example.org','file-two@example.org')").await,
        2
    );
}

/// Mass removal by selection and by pasted addresses, each an audited
/// deletion; a member of another list is left alone.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn mass_remove(db: &Database, app: &axum::Router, owner: &str) {
    let html = page(app, ROSTER, owner).await;
    let token = csrf(&html);
    let one: (String,) = sqlx::query_as("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email='mass-one@example.org'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let foreign: (String,) = sqlx::query_as("SELECT m.id FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='private.example.com' AND a.email='elsewhere@example.com'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let deletes = count(
        db,
        "SELECT COUNT(*) FROM audit_log WHERE action='member.delete'",
    )
    .await;
    let body = format!("csrf={token}&member={}&member={}", one.0, foreign.0);
    let removed = post(app, &format!("{ROSTER}/remove"), owner, &body).await;
    assert_eq!(removed.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members WHERE id=$1")
            .bind(&one.0)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "{}",
        one.0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM members WHERE id=$1")
            .bind(&foreign.0)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1,
        "the other list's member stays"
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='member.delete'"
        )
        .await,
        deletes + 1
    );
    // Pasted addresses.
    let removed = post(
        app,
        &format!("{ROSTER}/remove"),
        owner,
        &encode(&[
            ("csrf", &token),
            (
                "addresses",
                "mass-two@example.org\nfile-one@example.org\nnobody@example.org",
            ),
        ]),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id='public.example.com' AND a.email IN ('mass-two@example.org','file-one@example.org')").await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='member.delete'"
        )
        .await,
        deletes + 3
    );
    let landing = removed.headers()["location"].to_str().unwrap().to_owned();
    let html = page(app, &landing, owner).await;
    assert!(html.contains("Removed 2"), "the count is reported: {html}");
}

/// The export is CSV, per role, escaped, and closed to outsiders.
async fn export(app: &axum::Router, owner: &str, outsider: &str) {
    let response = call(app, "GET", &format!("{ROSTER}/export.csv"), owner, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("public.example.com-member.csv")
    );
    let csv = text(response).await;
    assert!(csv.starts_with("email,display_name,role,"), "{csv}");
    assert!(
        csv.contains("roster-member@example.com,Roster <Member>,member,"),
        "{csv}"
    );
    assert!(!csv.contains("roster-owner@example.com"), "{csv}");
    let owners = text(
        call(
            app,
            "GET",
            &format!("{ROSTER}/export.csv?role=owner"),
            owner,
            "",
        )
        .await,
    )
    .await;
    assert!(owners.contains("roster-owner@example.com"), "{owners}");
    assert_eq!(
        call(app, "GET", &format!("{ROSTER}/export.csv"), outsider, "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}
