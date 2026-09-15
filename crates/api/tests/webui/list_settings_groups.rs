//! The nine settings groups, header rules, bans, templates, digest actions,
//! archivers and list deletion under `/web/lists/{id}/settings`: each group
//! is one form on the same patch engine as the REST configuration, refused
//! values come back inline, a preview shows the diff before anything is
//! written, and every write is one audited transaction.
use super::{call, csrf, fixture, login_as, member, seeded_fixture, text, user};
use axum::http::StatusCode;
use listmngr_core::{ListId, MemberRole};
use listmngr_db::Database;

const ROOT: &str = "/web/lists/public.example.com/settings";

#[tokio::test]
async fn settings_groups_rules_bans_templates_and_deletion() {
    let (db, app) = fixture().await;
    matrix(db, app).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_settings_groups_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("webui_settings_groups")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 3).await.unwrap();
    db.migrate().await.unwrap();
    let (db, app) = seeded_fixture(db).await;
    matrix(db, app).await;
    schema.drop().await.unwrap();
}

fn list_id() -> ListId {
    "public.example.com".parse().unwrap()
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

async fn matrix(db: Database, app: axum::Router) {
    user(&db, "groups-owner@example.com", false).await;
    member(&db, "groups-owner@example.com", MemberRole::Owner).await;
    let owner = login_as(&app, "groups-owner@example.com").await;
    user(&db, "groups-member@example.com", false).await;
    member(&db, "groups-member@example.com", MemberRole::Member).await;
    let outsider = login_as(&app, "groups-member@example.com").await;

    navigation(&app, &owner, &outsider).await;
    identity(&db, &app, &owner).await;
    every_group(&db, &app, &owner).await;
    header_rules(&db, &app, &owner).await;
    bans(&db, &app, &owner).await;
    templates(&db, &app, &owner).await;
    digest_and_archivers(&db, &app, &owner).await;
    deletion(&db, &app, &owner, &outsider).await;
}

/// The overview links every group; a member who owns nothing is refused.
async fn navigation(app: &axum::Router, owner: &str, outsider: &str) {
    let overview = page(app, ROOT, owner).await;
    for group in [
        "identity",
        "responses",
        "messages",
        "dmarc",
        "digest",
        "acceptance",
        "archiving",
        "members",
        "bounces",
        "header-matches",
        "bans",
        "templates",
        "delete",
    ] {
        assert!(
            overview.contains(&format!("{ROOT}/{group}")),
            "{group}: {overview}"
        );
    }
    for path in [
        format!("{ROOT}/identity"),
        format!("{ROOT}/header-matches"),
        format!("{ROOT}/bans"),
        format!("{ROOT}/templates"),
        format!("{ROOT}/delete"),
    ] {
        assert_eq!(
            call(app, "GET", &path, outsider, "").await.status(),
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    assert_eq!(
        call(app, "GET", &format!("{ROOT}/nope"), owner, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

/// Identity: read-only facts, a preview that writes nothing, an inline
/// refusal that writes nothing, then a save that audits once.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn identity(db: &Database, app: &axum::Router, owner: &str) {
    let path = format!("{ROOT}/identity");
    let html = page(app, &path, owner).await;
    assert!(
        html.contains("public@example.com"),
        "posting address: {html}"
    );
    assert!(
        html.contains("public-owner@example.com"),
        "owner address: {html}"
    );
    assert!(html.contains("name=\"info\""), "{html}");
    let token = csrf(&html);
    let audits_before = count(
        db,
        "SELECT COUNT(*) FROM audit_log WHERE action='list.config'",
    )
    .await;
    // Preview: the diff, nothing written.
    let preview = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("preview", "1"),
            ("display_name", "Previewed <name>"),
            ("description", ""),
            ("info", "Long <info>"),
            ("subject_prefix", "[pv] "),
            ("advertised", "true"),
            ("preferred_language", "vi"),
        ]),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let preview = text(preview).await;
    assert!(preview.contains("Previewed &lt;name&gt;"), "{preview}");
    assert!(
        preview.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "old value: {preview}"
    );
    assert!(preview.contains("preferred_language"), "{preview}");
    assert!(!preview.contains("<script>"));
    let unchanged: String = sqlx::query_scalar(
        "SELECT display_name FROM mailing_lists WHERE list_id='public.example.com'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        unchanged, "<script>alert(1)</script>",
        "preview writes nothing"
    );
    // Nothing changed: the preview says so.
    let same = text(
        post(
            app,
            &path,
            owner,
            &encode(&[
                ("csrf", &token),
                ("preview", "1"),
                ("display_name", "<script>alert(1)</script>"),
                ("description", ""),
                ("info", ""),
                ("subject_prefix", "[public] "),
                ("advertised", "true"),
                ("preferred_language", "en"),
            ]),
        )
        .await,
    )
    .await;
    assert!(same.contains("Nothing would change"), "{same}");
    // Refused inline: the language must be one the site knows.
    let refused = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("display_name", "Refused"),
            ("description", ""),
            ("info", ""),
            ("subject_prefix", "bad\nprefix"),
            ("advertised", "true"),
            ("preferred_language", "en"),
        ]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused = text(refused).await;
    assert!(
        refused.contains("id=\"subject_prefix-error\""),
        "inline error: {refused}"
    );
    assert!(
        refused.contains("value=\"Refused\""),
        "submitted values kept: {refused}"
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='list.config'"
        )
        .await,
        audits_before
    );
    // Saved.
    let saved = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("display_name", "Groups <list>"),
            ("description", "A description"),
            ("info", "Long <info>"),
            ("subject_prefix", "[grp] "),
            ("advertised", "true"),
            ("preferred_language", "vi"),
        ]),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    let landing = saved.headers()["location"].to_str().unwrap().to_owned();
    let list = db.lists().get(&list_id()).await.unwrap();
    assert_eq!(list.display_name, "Groups <list>");
    assert_eq!(list.info, "Long <info>");
    assert_eq!(list.subject_prefix, "[grp] ");
    assert_eq!(list.preferred_language, "vi");
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='list.config'"
        )
        .await,
        audits_before + 1
    );
    let html = page(app, &landing, owner).await;
    assert!(html.contains("Groups &lt;list&gt;"), "{html}");
    assert!(
        html.contains("Saved"),
        "a saved notice after the redirect: {html}"
    );
}

/// One representative field per remaining group, saved through its own form.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn every_group(db: &Database, app: &axum::Router, owner: &str) {
    let cases: [(&str, Vec<(&str, &str)>); 8] = [
        (
            "responses",
            vec![
                ("autorespond_owner", "respond_and_discard"),
                ("autoresponse_owner_text", "Owner reply $display_name"),
                ("autorespond_postings", "none"),
                ("autoresponse_postings_text", ""),
                ("autorespond_requests", "respond"),
                ("autoresponse_request_text", "Request reply"),
                ("autoresponse_grace_period", "3"),
                ("respond_to_post_requests", "true"),
                ("send_welcome_message", "false"),
                ("send_goodbye_message", "true"),
                ("admin_immed_notify", "false"),
                ("admin_notify_mchanges", "true"),
            ],
        ),
        (
            "messages",
            vec![
                ("filter_content", "true"),
                ("filter_types", "image/jpeg\r\napplication/pdf"),
                ("pass_types", "multipart\ntext/plain"),
                ("filter_extensions", "exe\nbat"),
                ("pass_extensions", ""),
                ("collapse_alternatives", "true"),
                ("convert_html_to_plaintext", "true"),
                ("filter_action", "reject"),
                ("anonymous_list", "true"),
                ("include_rfc2369_headers", "false"),
                ("allow_list_posts", "true"),
                ("reply_goes_to_list", "point_to_list"),
                ("reply_to_address", ""),
                ("first_strip_reply_to", "true"),
                ("personalize", "individual"),
                ("include_sender_header", "false"),
            ],
        ),
        (
            "dmarc",
            vec![
                ("dmarc_mitigate_action", "munge_from"),
                ("dmarc_mitigate_unconditionally", "true"),
                ("dmarc_addresses", "one@example.org\n^.*@dmarc\\.example"),
                ("dmarc_moderation_notice", "Rejected for DMARC"),
                ("dmarc_wrapped_message_text", "Wrapped"),
            ],
        ),
        (
            "digest",
            vec![
                ("digests_enabled", "true"),
                ("digest_size_threshold", "45.5"),
                ("digest_send_periodic", "false"),
                ("digest_volume_frequency", "weekly"),
                ("next_digest_number", "7"),
            ],
        ),
        (
            "acceptance",
            vec![
                ("default_member_action", "hold"),
                ("default_nonmember_action", "default"),
                ("accept_these_nonmembers", "friend@example.org"),
                ("hold_these_nonmembers", ""),
                ("reject_these_nonmembers", "^spam@"),
                ("discard_these_nonmembers", ""),
                ("require_explicit_destination", "true"),
                ("acceptable_aliases", "alias@example.com"),
                ("administrivia", "false"),
                ("max_message_size", "512"),
                ("max_num_recipients", "9"),
                ("emergency", "false"),
                ("posting_pipeline", "default-posting-pipeline"),
            ],
        ),
        (
            "archiving",
            vec![
                ("archive_policy", "private"),
                ("archive_rendering_mode", "markdown"),
            ],
        ),
        (
            "members",
            vec![
                ("subscription_policy", "moderate"),
                ("unsubscription_policy", "confirm"),
                ("member_roster_visibility", "moderators"),
            ],
        ),
        (
            "bounces",
            vec![
                ("process_bounces", "true"),
                ("bounce_score_threshold", "7.5"),
                ("bounce_info_stale_after", "10"),
                ("bounce_you_are_disabled_warnings", "2"),
                ("bounce_you_are_disabled_warnings_interval", "5"),
                ("bounce_notify_owner_on_disable", "false"),
                ("bounce_notify_owner_on_removal", "false"),
                ("bounce_notify_owner_on_bounce_increment", "true"),
                ("forward_unrecognized_bounces_to", "administrators"),
            ],
        ),
    ];
    for (group, fields) in &cases {
        let path = format!("{ROOT}/{group}");
        let html = page(app, &path, owner).await;
        for (name, _) in fields {
            assert!(
                html.contains(&format!("name=\"{name}\"")),
                "{group} lacks {name}: {html}"
            );
        }
        let token = csrf(&html);
        let mut body = vec![("csrf", token.as_str())];
        body.extend(fields.iter().copied());
        let response = post(app, &path, owner, &encode(&body)).await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "{group}: {}",
            text(response).await
        );
    }
    let list = db.lists().get(&list_id()).await.unwrap();
    assert_eq!(
        list.automatic_responses.autorespond_owner.as_str(),
        "respond_and_discard"
    );
    assert_eq!(list.automatic_responses.autoresponse_grace_period, 3);
    assert!(list.admin_notify_mchanges);
    assert_eq!(
        list.alter_messages.filter_types,
        ["image/jpeg", "application/pdf"]
    );
    assert_eq!(list.alter_messages.pass_types, ["multipart", "text/plain"]);
    assert_eq!(list.alter_messages.filter_extensions, ["exe", "bat"]);
    assert!(list.alter_messages.pass_extensions.is_empty());
    assert_eq!(
        list.alter_messages.reply_goes_to_list.as_str(),
        "point_to_list"
    );
    assert_eq!(list.alter_messages.personalize.as_str(), "individual");
    assert!(list.anonymous_list);
    assert_eq!(
        list.dmarc.action,
        listmngr_core::DmarcMitigateAction::MungeFrom
    );
    assert!(list.dmarc.unconditional);
    assert_eq!(
        list.dmarc.dmarc_addresses,
        ["one@example.org", "^.*@dmarc\\.example"]
    );
    assert!(list.digests_enabled);
    assert!((list.digest_size_threshold - 45.5).abs() < f64::EPSILON);
    assert_eq!(list.digest_volume_frequency.as_str(), "weekly");
    assert_eq!(list.next_digest_number, 7);
    assert_eq!(
        list.default_member_action
            .map(listmngr_core::ModerationAction::as_str),
        Some("hold")
    );
    assert_eq!(list.default_nonmember_action, None);
    assert_eq!(list.accept_these_nonmembers, ["friend@example.org"]);
    assert_eq!(list.reject_these_nonmembers, ["^spam@"]);
    assert_eq!(list.acceptable_aliases, ["alias@example.com"]);
    assert!(list.require_explicit_destination);
    assert_eq!(list.max_message_size, 512);
    assert_eq!(list.max_num_recipients, 9);
    assert_eq!(list.archive_policy.as_str(), "private");
    assert_eq!(list.archive_rendering_mode.as_str(), "markdown");
    assert_eq!(list.member_policy.subscription_policy.as_str(), "moderate");
    assert_eq!(
        list.member_policy.member_roster_visibility.as_str(),
        "moderators"
    );
    assert!((list.bounce_score_threshold - 7.5).abs() < f64::EPSILON);
    assert_eq!(list.bounce_info_stale_after, 10);
    assert_eq!(
        list.forward_unrecognized_bounces_to.as_str(),
        "administrators"
    );
    // A refused token list names its field inline and writes nothing.
    let path = format!("{ROOT}/messages");
    let token = csrf(&page(app, &path, owner).await);
    let refused = post(
        app,
        &path,
        owner,
        &encode(&[
            ("csrf", &token),
            ("filter_content", "true"),
            ("filter_types", "not a mime type"),
            ("pass_types", ""),
            ("filter_extensions", ""),
            ("pass_extensions", ""),
            ("collapse_alternatives", "true"),
            ("convert_html_to_plaintext", "true"),
            ("filter_action", "reject"),
            ("anonymous_list", "true"),
            ("include_rfc2369_headers", "false"),
            ("allow_list_posts", "true"),
            ("reply_goes_to_list", "point_to_list"),
            ("reply_to_address", ""),
            ("first_strip_reply_to", "true"),
            ("personalize", "individual"),
            ("include_sender_header", "false"),
        ]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused = text(refused).await;
    assert!(refused.contains("id=\"filter_types-error\""), "{refused}");
    let list = db.lists().get(&list_id()).await.unwrap();
    assert_eq!(
        list.alter_messages.filter_types,
        ["image/jpeg", "application/pdf"]
    );
}

/// Header rules: add, refuse a bad pattern inline, reorder, edit, test a
/// value against the rules, remove.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn header_rules(db: &Database, app: &axum::Router, owner: &str) {
    let path = format!("{ROOT}/header-matches");
    let html = page(app, &path, owner).await;
    let token = csrf(&html);
    let add = |header: &str, pattern: &str, action: &str, tag: &str| {
        encode(&[
            ("csrf", &token),
            ("header", header),
            ("pattern", pattern),
            ("action", action),
            ("tag", tag),
        ])
    };
    assert_eq!(
        post(
            app,
            &format!("{path}/add"),
            owner,
            &add("X-Spam-Flag", "^YES", "discard", "spam")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        post(
            app,
            &format!("{path}/add"),
            owner,
            &add("Subject", "(?i)urgent", "hold", "")
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let refused = post(
        app,
        &format!("{path}/add"),
        owner,
        &add("Subject", "([", "hold", ""),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused = text(refused).await;
    assert!(refused.contains("id=\"pattern-error\""), "{refused}");
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM header_matches WHERE list_id='public.example.com'"
        )
        .await,
        2
    );
    let html = page(app, &path, owner).await;
    assert!(html.contains("X-Spam-Flag"), "{html}");
    assert!(html.contains("(?i)urgent"), "{html}");
    assert!(html.find("X-Spam-Flag").unwrap() < html.find("(?i)urgent").unwrap());
    // Move the second rule up.
    assert_eq!(
        post(
            app,
            &format!("{path}/1/move"),
            owner,
            &encode(&[("csrf", &token), ("direction", "up")])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let html = page(app, &path, owner).await;
    assert!(
        html.find("(?i)urgent").unwrap() < html.find("X-Spam-Flag").unwrap(),
        "{html}"
    );
    // Edit the first rule in place.
    assert_eq!(
        post(
            app,
            &format!("{path}/0"),
            owner,
            &encode(&[
                ("csrf", &token),
                ("header", "Subject"),
                ("pattern", "(?i)urgent|asap"),
                ("action", "reject"),
                ("tag", "shouting"),
            ]),
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let first: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT pattern, chain, tag FROM header_matches WHERE list_id='public.example.com' AND position=0",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        first,
        (
            "(?i)urgent|asap".into(),
            Some("reject".into()),
            Some("shouting".into())
        )
    );
    // Test a value: the first rule that matches is named, and each rule says.
    let tested = post(
        app,
        &format!("{path}/test"),
        owner,
        &encode(&[
            ("csrf", &token),
            ("header", "subject"),
            ("value", "Please read ASAP"),
        ]),
    )
    .await;
    assert_eq!(tested.status(), StatusCode::OK);
    let tested = text(tested).await;
    assert!(tested.contains("would reject"), "{tested}");
    assert!(tested.contains("Please read ASAP"), "{tested}");
    let untested = text(
        post(
            app,
            &format!("{path}/test"),
            owner,
            &encode(&[("csrf", &token), ("header", "X-Spam-Flag"), ("value", "NO")]),
        )
        .await,
    )
    .await;
    assert!(untested.contains("No rule matches"), "{untested}");
    // Remove.
    assert_eq!(
        post(
            app,
            &format!("{path}/1/remove"),
            owner,
            &encode(&[("csrf", &token)])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM header_matches WHERE list_id='public.example.com'"
        )
        .await,
        1
    );
    assert!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='list.header_matches'"
        )
        .await
            >= 4
    );
}

/// List bans and, for a server owner, site bans.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn bans(db: &Database, app: &axum::Router, owner: &str) {
    let path = format!("{ROOT}/bans");
    let token = csrf(&page(app, &path, owner).await);
    assert_eq!(
        post(
            app,
            &format!("{path}/add"),
            owner,
            &encode(&[("csrf", &token), ("email_or_regex", "Spammer@Example.org")])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        post(
            app,
            &format!("{path}/add"),
            owner,
            &encode(&[("csrf", &token), ("email_or_regex", "^.*@bad\\.example")])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let refused = post(
        app,
        &format!("{path}/add"),
        owner,
        &encode(&[("csrf", &token), ("email_or_regex", "^([")]),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(text(refused).await.contains("id=\"email_or_regex-error\""));
    let html = page(app, &path, owner).await;
    assert!(html.contains("spammer@example.org"), "normalized: {html}");
    assert!(html.contains("^.*@bad\\.example"), "{html}");
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM bans WHERE list_id='public.example.com'"
        )
        .await,
        2
    );
    assert_eq!(
        post(
            app,
            &format!("{path}/remove"),
            owner,
            &encode(&[("csrf", &token), ("email_or_regex", "spammer@example.org")])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM bans WHERE list_id='public.example.com'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action IN ('ban.create','ban.delete')"
        )
        .await,
        3
    );
    // Site bans: server owners only.
    assert_eq!(
        call(app, "GET", "/web/admin/bans", owner, "")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    user(db, "site-owner@example.com", true).await;
    let site = login_as(app, "site-owner@example.com").await;
    let html = page(app, "/web/admin/bans", &site).await;
    let token = csrf(&html);
    assert_eq!(
        post(
            app,
            "/web/admin/bans/add",
            &site,
            &encode(&[("csrf", &token), ("email_or_regex", "global@example.org")])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert!(
        page(app, "/web/admin/bans", &site)
            .await
            .contains("global@example.org")
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM bans WHERE list_id IS NULL").await,
        1
    );
    assert_eq!(
        post(
            app,
            "/web/admin/bans/remove",
            &site,
            &encode(&[("csrf", &token), ("email_or_regex", "global@example.org")])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM bans WHERE list_id IS NULL").await,
        0
    );
}

/// Templates: the catalogue, an editor with a preview, a save, a removal.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn templates(db: &Database, app: &axum::Router, owner: &str) {
    let path = format!("{ROOT}/templates");
    let html = page(app, &path, owner).await;
    assert!(html.contains("list:user:notice:welcome"), "{html}");
    assert!(
        html.contains(&format!("{path}/list:user:notice:welcome")),
        "{html}"
    );
    let editor = page(app, &format!("{path}/list:user:notice:welcome"), owner).await;
    assert!(
        editor.contains("$display_name"),
        "placeholder help: {editor}"
    );
    assert!(editor.contains("name=\"language\""), "{editor}");
    let token = csrf(&editor);
    let preview = post(
        app,
        &format!("{path}/list:user:notice:welcome"),
        owner,
        &encode(&[
            ("csrf", &token),
            ("preview", "1"),
            ("language", "en"),
            ("body", "Welcome to $display_name <$listname>\n$$ stays"),
        ]),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let preview = text(preview).await;
    assert!(
        preview.contains("Welcome to Groups &lt;list&gt; &lt;public@example.com&gt;"),
        "{preview}"
    );
    assert!(preview.contains("$ stays"), "{preview}");
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM templates WHERE scope='list'").await,
        0
    );
    assert_eq!(
        post(
            app,
            &format!("{path}/list:user:notice:welcome"),
            owner,
            &encode(&[
                ("csrf", &token),
                ("language", "vi"),
                ("body", "Chào mừng $display_name")
            ]),
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(count(db, "SELECT COUNT(*) FROM templates WHERE scope='list' AND language='vi' AND body='Chào mừng $display_name'").await, 1);
    let html = page(app, &path, owner).await;
    assert!(html.contains("vi"), "{html}");
    let editor = page(
        app,
        &format!("{path}/list:user:notice:welcome?language=vi"),
        owner,
    )
    .await;
    assert!(editor.contains("Chào mừng $display_name"), "{editor}");
    assert_eq!(
        call(app, "GET", &format!("{path}/list:nope"), owner, "")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(
            app,
            &format!("{path}/list:user:notice:welcome/remove"),
            owner,
            &encode(&[("csrf", &token)])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        count(db, "SELECT COUNT(*) FROM templates WHERE scope='list'").await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='template.set'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='template.delete'"
        )
        .await,
        1
    );
}

/// Digest actions and archiver toggles from their groups.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn digest_and_archivers(db: &Database, app: &axum::Router, owner: &str) {
    let digest = format!("{ROOT}/digest");
    let token = csrf(&page(app, &digest, owner).await);
    let before = db.lists().get(&list_id()).await.unwrap().volume;
    assert_eq!(
        post(
            app,
            &format!("{digest}/bump"),
            owner,
            &encode(&[("csrf", &token)])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let list = db.lists().get(&list_id()).await.unwrap();
    assert_eq!(list.volume, before + 1);
    assert_eq!(list.next_digest_number, 1);
    assert_eq!(
        post(
            app,
            &format!("{digest}/send"),
            owner,
            &encode(&[("csrf", &token)])
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM audit_log WHERE action='digest.bump'"
        )
        .await,
        1
    );
    let archiving = format!("{ROOT}/archiving");
    let html = page(app, &archiving, owner).await;
    assert!(html.contains("name=\"archiver\""), "{html}");
    let token = csrf(&html);
    assert_eq!(
        post(
            app,
            &format!("{archiving}/archiver"),
            owner,
            &encode(&[
                ("csrf", &token),
                ("archiver", "mail-archive"),
                ("enabled", "true")
            ]),
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    let archivers = db.lists().archivers(&list_id()).await.unwrap();
    assert!(
        archivers.contains(&("mail-archive".to_owned(), true)),
        "{archivers:?}"
    );
    assert_eq!(
        post(
            app,
            &format!("{archiving}/archiver"),
            owner,
            &encode(&[
                ("csrf", &token),
                ("archiver", "not-an-archiver"),
                ("enabled", "true")
            ]),
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
}

/// Deleting the list needs its id typed back; then everything of it is gone.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // One ordered contract per page.
async fn deletion(db: &Database, app: &axum::Router, owner: &str, outsider: &str) {
    let path = format!("{ROOT}/delete");
    let html = page(app, &path, owner).await;
    assert!(
        html.contains("private"),
        "the archive policy is stated: {html}"
    );
    let token = csrf(&html);
    assert_eq!(
        post(
            app,
            &path,
            owner,
            &encode(&[("csrf", &token), ("confirm", "wrong.example.com")])
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            app,
            &path,
            outsider,
            &encode(&[("csrf", &token), ("confirm", "public.example.com")])
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM mailing_lists WHERE list_id='public.example.com'"
        )
        .await,
        1
    );
    let done = post(
        app,
        &path,
        owner,
        &encode(&[("csrf", &token), ("confirm", "public.example.com")]),
    )
    .await;
    assert_eq!(done.status(), StatusCode::SEE_OTHER);
    assert_eq!(done.headers()["location"], "/web/admin");
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM mailing_lists WHERE list_id='public.example.com'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM members WHERE list_id='public.example.com'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM header_matches WHERE list_id='public.example.com'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            db,
            "SELECT COUNT(*) FROM bans WHERE list_id='public.example.com'"
        )
        .await,
        0
    );
    let actor: Option<String> =
        sqlx::query_scalar("SELECT actor_user_id FROM audit_log WHERE action='list.delete' AND target_id='public.example.com'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(actor.is_some(), "the deletion names the owner");
    // A list that is gone looks like one the reader never owned.
    assert_eq!(
        call(app, "GET", ROOT, owner, "").await.status(),
        StatusCode::FORBIDDEN
    );
}
