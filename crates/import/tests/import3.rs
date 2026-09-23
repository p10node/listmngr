//! `import3 --rest`: a Mailman 3 site read over its REST API — domains,
//! lists with their configuration, the rosters with each member's own
//! preferences, bans and header matches — and applied here.
//!
//! The fixtures are recorded from a real GNU Mailman 3.3.10 core by
//! `tests/compat/generate_mailman3_rest.py`, so the shapes (pagination
//! envelopes, `http_etag`, sparse `preferences`, Mailman's `7d`
//! durations) are the server's own.
use listmngr_core::{
    DeliveryMode, DeliveryStatus, ListId, MemberRole, ModerationAction, SubscriptionMode,
};
use listmngr_db::{AuditContext, Database};
use listmngr_import::import3::{Site, Source, apply, fetch, plan};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The recorded answers of the real core, by the path the importer asks
/// for. A path this source does not know is a test failure, so the test
/// also pins which requests the importer makes.
struct Recorded {
    answers: BTreeMap<&'static str, &'static str>,
}

impl Recorded {
    fn site() -> Self {
        let answers = BTreeMap::from([
            ("domains", "domains"),
            ("users", "users"),
            ("lists?advertised=false", "lists"),
            ("bans", "bans-global"),
            ("lists/rust-users.example.invalid/config", "list-config"),
            (
                "lists/rust-users.example.invalid/roster/member",
                "roster-member",
            ),
            (
                "lists/rust-users.example.invalid/roster/owner",
                "roster-owner",
            ),
            (
                "lists/rust-users.example.invalid/roster/moderator",
                "roster-moderator",
            ),
            (
                "lists/rust-users.example.invalid/roster/nonmember",
                "roster-nonmember",
            ),
            ("lists/rust-users.example.invalid/bans", "bans-list"),
            (
                "lists/rust-users.example.invalid/header-matches",
                "header-matches",
            ),
            ("lists/rust-users.example.invalid/uris", "uris"),
            ("lists/rust-users.example.invalid/held", "held"),
            ("lists/rust-users.example.invalid/requests", "requests"),
            ("lists/announce.other.invalid/config", "announce-config"),
            (
                "lists/announce.other.invalid/roster/member",
                "announce-roster-member",
            ),
            (
                "lists/announce.other.invalid/roster/owner",
                "announce-roster-owner",
            ),
            (
                "lists/announce.other.invalid/roster/moderator",
                "announce-roster-moderator",
            ),
            (
                "lists/announce.other.invalid/roster/nonmember",
                "announce-roster-nonmember",
            ),
            ("lists/announce.other.invalid/bans", "announce-bans"),
            (
                "lists/announce.other.invalid/header-matches",
                "announce-header-matches",
            ),
            ("lists/announce.other.invalid/uris", "announce-uris"),
            ("lists/announce.other.invalid/held", "announce-held"),
            ("lists/announce.other.invalid/requests", "announce-requests"),
        ]);
        Self { answers }
    }
}

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mailman3")
        .join(format!("{name}.json"));
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

impl Source for Recorded {
    async fn get_optional(&self, path: &str) -> listmngr_import::Result<Option<Value>> {
        // Mailman answers 404 for a user with no preferred address.
        if let Some(id) = path
            .strip_prefix("users/")
            .and_then(|rest| rest.strip_suffix("/preferred_address"))
        {
            let all = fixture("user-preferred");
            return Ok(all.get(id).cloned().filter(|value| !value.is_null()));
        }
        self.get(path).await.map(Some)
    }

    async fn get(&self, path: &str) -> listmngr_import::Result<Value> {
        // A member's own preferences hang off its self link.
        if let Some(id) = path
            .strip_prefix("members/")
            .and_then(|rest| rest.strip_suffix("/preferences"))
        {
            let all = fixture("preferences");
            return Ok(all.get(id).cloned().unwrap_or_else(|| json!({})));
        }
        for (prefix, suffix, file) in [
            ("users/", "/addresses", "user-addresses"),
            ("users/", "/preferences", "user-preferences"),
        ] {
            if let Some(id) = path
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
            {
                let all = fixture(file);
                return Ok(all.get(id).cloned().unwrap_or_else(|| json!({})));
            }
        }
        let name = self
            .answers
            .get(path)
            .unwrap_or_else(|| panic!("the importer asked for an unrecorded path: {path}"));
        Ok(fixture(name))
    }
}

async fn recorded_site() -> Site {
    fetch(&Recorded::site(), None).await.unwrap()
}

#[tokio::test]
async fn a_real_cores_answers_read_into_a_snapshot() {
    let site = recorded_site().await;
    assert_eq!(
        site.domains
            .iter()
            .map(|domain| domain.mail_host.as_str())
            .collect::<Vec<_>>(),
        ["example.invalid", "other.invalid"]
    );
    assert_eq!(site.domains[0].description, "Example");
    assert_eq!(site.site_bans, ["global-spammer@example.invalid"]);
    assert_eq!(site.lists.len(), 2);
    let list = site
        .lists
        .iter()
        .find(|list| list.list_id.as_str() == "rust-users.example.invalid")
        .unwrap();
    assert_eq!(list.display_name, "Rust-Users");
    assert_eq!(list.mail_host, "example.invalid");
    assert_eq!(list.config["subject_prefix"], "[Rust] ");
    assert_eq!(list.bans.len(), 2);
    assert_eq!(list.header_matches.len(), 2);
    assert_eq!(
        list.uris,
        [(
            "list:member:regular:footer".to_owned(),
            "http://example.invalid/footer.txt".to_owned()
        )]
    );
    // Every roster, with the member's own preferences (sparse: only what
    // was set on the member itself).
    let member = |email: &str| {
        list.members
            .iter()
            .find(|member| member.email == email)
            .unwrap_or_else(|| panic!("{email}"))
    };
    assert_eq!(list.members.len(), 7);
    let alice = member("alice@example.invalid");
    assert_eq!(alice.role, MemberRole::Member);
    assert_eq!(alice.display_name, "Alice Nguyễn");
    assert_eq!(
        alice.preferences.delivery_mode,
        Some(DeliveryMode::PlaintextDigests)
    );
    assert_eq!(alice.preferences.acknowledge_posts, Some(true));
    assert_eq!(alice.preferences.hide_address, Some(true));
    assert_eq!(alice.preferences.receive_own_postings, Some(false));
    assert_eq!(
        member("bob@example.invalid").moderation_action,
        Some(ModerationAction::Hold)
    );
    assert_eq!(
        member("carol@elsewhere.invalid")
            .preferences
            .delivery_status,
        Some(DeliveryStatus::ByBounces)
    );
    assert_eq!(member("owner@example.invalid").role, MemberRole::Owner);
    assert_eq!(member("mod@example.invalid").role, MemberRole::Moderator);
    let stranger = member("stranger@example.invalid");
    assert_eq!(stranger.role, MemberRole::Nonmember);
    assert_eq!(stranger.moderation_action, Some(ModerationAction::Discard));
    // Mailman subscribes Dave as a user, not as an address.
    assert_eq!(member("dave@example.invalid").subscription_mode, "as_user");
}

#[tokio::test]
async fn the_accounts_come_with_their_addresses_and_the_one_they_prefer() {
    let site = recorded_site().await;
    // Mailman has a user behind every address, so the ones it made for
    // plain subscribers come too — eight in all for this site.
    assert_eq!(site.users.len(), 9, "{:?}", site.users);
    let user = |email: &str| {
        site.users
            .iter()
            .find(|user| user.addresses.iter().any(|address| address.email == email))
            .unwrap_or_else(|| panic!("{email}"))
    };
    let dave = user("dave@example.invalid");
    assert_eq!(dave.display_name, "Dave");
    assert!(!dave.is_server_owner);
    assert!(dave.has_password, "Mailman had a password hash for Dave");
    assert_eq!(
        dave.addresses
            .iter()
            .map(|address| address.email.as_str())
            .collect::<Vec<_>>(),
        ["dave@example.invalid", "dave@work.invalid"]
    );
    assert!(dave.addresses[0].verified);
    assert_eq!(dave.preferred.as_deref(), Some("dave@example.invalid"));
    assert_eq!(dave.preferences.hide_address, Some(true));
    let eve = user("eve@example.invalid");
    assert!(eve.is_server_owner);
    assert_eq!(eve.preferred, None, "Mailman answers 404 without one");
    assert!(!eve.addresses[1].verified);
    let alice = user("alice@example.invalid");
    assert_eq!(alice.display_name, "Alice Nguyễn");
    assert!(!alice.has_password);
}

/// What the old site was still holding: a post waiting for a moderator
/// and a join nobody had answered.
#[tokio::test]
async fn what_was_waiting_on_the_old_site_comes_too() {
    let site = recorded_site().await;
    let announce = site
        .lists
        .iter()
        .find(|list| list.list_id.as_str() == "announce.other.invalid")
        .unwrap();
    assert_eq!(announce.held.len(), 1);
    let held = &announce.held[0];
    assert_eq!(held.sender, "stranger@example.invalid");
    assert_eq!(held.subject, "Please let me in");
    assert_eq!(held.reason, "The message is not from a list member");
    assert!(held.hold_date > 1_700_000_000_000, "{}", held.hold_date);
    assert!(
        String::from_utf8_lossy(&held.raw).contains("Message-ID: <held-1@example.invalid>"),
        "the message itself, as the core kept it"
    );
    assert_eq!(announce.requests.len(), 1);
    let request = &announce.requests[0];
    assert_eq!(request.email, "wanted@example.invalid");
    assert_eq!(request.display_name, "Wanted");
    assert_eq!(request.action, "subscription");
    assert_eq!(request.token_owner, "moderator");
    assert!(request.requested_at > 1_700_000_000_000);
    // The other list had nothing waiting.
    let rust = site
        .lists
        .iter()
        .find(|list| list.list_id.as_str() == "rust-users.example.invalid")
        .unwrap();
    assert!(rust.held.is_empty() && rust.requests.is_empty());
}

#[tokio::test]
async fn one_list_can_be_read_on_its_own() {
    let only: ListId = "rust-users.example.invalid".parse().unwrap();
    let site = fetch(&Recorded::site(), Some(&only)).await.unwrap();
    assert_eq!(site.lists.len(), 1);
    assert_eq!(site.lists[0].list_id, only);
    // The domains and the site bans still come, so the list's domain can
    // be made when it is missing.
    assert_eq!(site.domains.len(), 2);
}

#[tokio::test]
async fn the_snapshot_plans_this_sites_settings() {
    let plan = plan(&recorded_site().await);
    let list = &plan.lists[1];
    assert_eq!(list.list_id.as_str(), "rust-users.example.invalid");
    let s = &list.settings;
    for (key, value) in [
        ("display_name", json!("Rust-Users")),
        ("description", json!("Rust users of Example")),
        ("info", json!("Long description\nwith two lines")),
        ("subject_prefix", json!("[Rust] ")),
        ("preferred_language", json!("en")),
        ("advertised", json!(false)),
        ("anonymous_list", json!(true)),
        ("administrivia", json!(false)),
        ("archive_policy", json!("private")),
        ("archive_rendering_mode", json!("text")),
        ("autorespond_owner", json!("none")),
        ("autoresponse_grace_period", json!(90)),
        ("bounce_info_stale_after", json!(7)),
        ("bounce_you_are_disabled_warnings_interval", json!(7)),
        ("bounce_score_threshold", json!(5.0)),
        ("collapse_alternatives", json!(false)),
        ("convert_html_to_plaintext", json!(true)),
        ("default_member_action", json!("reject")),
        ("default_nonmember_action", json!("discard")),
        ("digest_size_threshold", json!(45.0)),
        ("digest_volume_frequency", json!("monthly")),
        ("dmarc_mitigate_action", json!("wrap_message")),
        ("dmarc_mitigate_unconditionally", json!(true)),
        ("emergency", json!(true)),
        ("filter_action", json!("discard")),
        ("first_strip_reply_to", json!(true)),
        ("forward_unrecognized_bounces_to", json!("administrators")),
        ("gateway_to_mail", json!(true)),
        ("linked_newsgroup", json!("comp.lang.rust.lists")),
        ("newsgroup_moderation", json!("moderated")),
        ("nntp_prefix_subject_too", json!(false)),
        ("max_message_size", json!(120)),
        ("member_roster_visibility", json!("moderators")),
        ("personalize", json!("individual")),
        ("posting_pipeline", json!("default-posting-pipeline")),
        ("reply_goes_to_list", json!("explicit_header")),
        ("reply_to_address", json!("replies@example.invalid")),
        ("subscription_policy", json!("confirm_then_moderate")),
        ("unsubscription_policy", json!("confirm")),
        // A multi-line alias value from Mailman is one alias per line.
        (
            "acceptable_aliases",
            json!([
                "rust-users-alias@example.invalid",
                "^announce-.*@example\\.invalid"
            ]),
        ),
    ] {
        assert_eq!(s[key], value, "{key}");
    }
    // The resources Mailman derives are not settings here, and neither is
    // a setting this site does not have.
    for absent in [
        "fqdn_listname",
        "list_name",
        "mail_host",
        "posting_address",
        "owner_address",
        "bounces_address",
        "join_address",
        "leave_address",
        "request_address",
        "no_reply_address",
        "created_at",
        "last_post_at",
        "digest_last_sent_at",
        "post_id",
        "volume",
        "usenet_watermark",
        "max_days_to_hold",
        "moderator_password",
        "http_etag",
    ] {
        assert!(!s.contains_key(absent), "{absent} in {s:?}");
    }
    // The template URIs cannot be imported as bodies: Mailman keeps only
    // the URI, so each is a warning naming it.
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("list:member:regular:footer")
                && warning.contains("http://example.invalid/footer.txt")),
        "{:?}",
        plan.warnings
    );
    // Dave is subscribed as a user in Mailman, and his account comes with
    // the import, so the membership keeps its mode instead of warning.
    assert!(
        !plan
            .warnings
            .iter()
            .any(|warning| warning.contains("as_user")),
        "{:?}",
        plan.warnings
    );
}

/// A confirmation the subscriber still owed cannot be carried over: the
/// token was issued by the other system.
#[tokio::test]
async fn a_confirmation_the_subscriber_owed_is_a_warning() {
    let mut site = recorded_site().await;
    let announce = site
        .lists
        .iter_mut()
        .find(|list| list.list_id.as_str() == "announce.other.invalid")
        .unwrap();
    let mut owed = announce.requests[0].clone();
    owed.email = "unconfirmed@example.invalid".into();
    owed.token_owner = "subscriber".into();
    announce.requests.push(owed);
    let plan = plan(&site);
    let announce = plan
        .lists
        .iter()
        .find(|list| list.list_id.as_str() == "announce.other.invalid")
        .unwrap();
    assert_eq!(announce.requests.len(), 1, "only the moderator's");
    assert_eq!(announce.requests[0].email, "wanted@example.invalid");
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("unconfirmed@example.invalid")
                && warning.contains("confirm")),
        "{:?}",
        plan.warnings
    );
}

/// What the accounts plan, and what cannot come with them.
#[tokio::test]
async fn the_plan_carries_the_accounts_without_their_passwords() {
    let plan = plan(&recorded_site().await);
    assert_eq!(plan.users.len(), 9);
    let dave = plan
        .users
        .iter()
        .find(|user| user.preferred.as_deref() == Some("dave@example.invalid"))
        .unwrap();
    assert_eq!(dave.display_name, "Dave");
    assert_eq!(dave.addresses.len(), 2);
    // A password nobody can carry over: a warning, never the hash.
    let warnings = plan.warnings.join("\n");
    assert!(warnings.contains("dave@example.invalid"), "{warnings}");
    assert!(warnings.contains("password"), "{warnings}");
    assert!(!warnings.contains('$'), "no hash material: {warnings}");
    assert_eq!(
        plan.warnings
            .iter()
            .filter(|warning| warning.contains("cannot be carried over"))
            .count(),
        2,
        "one per account that had a password: {warnings}"
    );
}

/// A configuration with values this site cannot take is not silently
/// dropped: each one is a warning, and the rest of the list still plans.
#[tokio::test]
async fn unknown_settings_and_values_become_warnings() {
    let mut site = recorded_site().await;
    let list = &mut site.lists[1];
    list.config
        .insert("some_new_mailman_setting".into(), json!(true));
    list.config.insert("preferred_language".into(), json!("ar"));
    list.config.insert("max_days_to_hold".into(), json!(3));
    list.config
        .insert("moderator_password".into(), json!("{plaintext}secret"));
    let plan = plan(&site);
    let warnings = plan.warnings.join("\n");
    assert!(warnings.contains("some_new_mailman_setting"), "{warnings}");
    assert!(warnings.contains("max_days_to_hold"), "{warnings}");
    assert!(warnings.contains("moderator password"), "{warnings}");
    assert!(warnings.contains("\"ar\""), "{warnings}");
    assert!(!warnings.contains("secret"), "a password is never logged");
    assert!(!plan.lists[1].settings.contains_key("preferred_language"));
    assert_eq!(plan.lists[1].settings["display_name"], "Rust-Users");
}

/// Mailman spells a language `pt_BR`; a notice language here is `pt-BR`.
#[tokio::test]
async fn a_mailman_language_code_becomes_this_sites_tag() {
    let mut site = recorded_site().await;
    site.lists[1]
        .config
        .insert("preferred_language".into(), json!("pt_BR"));
    let plan = plan(&site);
    assert_eq!(plan.lists[1].settings["preferred_language"], "pt-BR");
    assert!(
        !plan.warnings.join("\n").contains("preferred_language"),
        "{:?}",
        plan.warnings
    );
}

async fn scenario(db: &Database) {
    db.migrate().await.unwrap();
    let site = recorded_site().await;
    let plan = plan(&site);
    let report = apply(db, &plan, &AuditContext::system()).await.unwrap();
    for (counted, expected) in [
        (report.domains, 2),
        (report.users, 9),
        (report.lists, 2),
        (report.members, 4),
        (report.owners, 1),
        (report.moderators, 1),
        (report.nonmembers, 2),
        (report.bans, 2),
        (report.site_bans, 1),
        (report.header_matches, 2),
        (report.held, 1),
        (report.requests, 1),
        (report.skipped, 0),
    ] {
        assert_eq!(counted, expected, "{report:?}");
    }
    domains_and_lists_applied(db).await;
    accounts_applied(db).await;
    rosters_applied(db).await;
    lists_applied(db).await;
    waiting_applied(db).await;
    // A second import changes nothing and says what it skipped.
    let again = apply(db, &plan, &AuditContext::system()).await.unwrap();
    assert_eq!(again.domains, 0, "{again:?}");
    assert_eq!(again.users, 0);
    assert_eq!(again.lists, 0);
    assert_eq!(again.members, 0);
    assert_eq!(again.bans, 0);
    assert_eq!(again.site_bans, 0);
    assert_eq!(again.header_matches, 0);
    assert_eq!(again.held, 0, "a message already held is not held twice");
    assert_eq!(again.requests, 0);
    // Two domains, nine accounts, two lists, eight members, the held
    // message and the waiting request were all there already.
    assert_eq!(again.skipped, 23);
}

/// The domains and both lists, with the settings the core gave.
async fn domains_and_lists_applied(db: &Database) {
    assert_eq!(
        db.domains()
            .get("example.invalid")
            .await
            .unwrap()
            .description,
        "Example"
    );
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    let saved: Value = serde_json::to_value(db.lists().get(&list).await.unwrap()).unwrap();
    for (key, value) in [
        ("display_name", json!("Rust-Users")),
        ("subject_prefix", json!("[Rust] ")),
        ("dmarc_mitigate_action", json!("wrap_message")),
        ("linked_newsgroup", json!("comp.lang.rust.lists")),
        ("bounce_info_stale_after", json!(7)),
    ] {
        assert_eq!(saved[key], value, "{key}");
    }
    assert_eq!(
        saved["acceptable_aliases"][0],
        "rust-users-alias@example.invalid"
    );
    let announce: ListId = "announce.other.invalid".parse().unwrap();
    assert_eq!(
        db.lists().get(&announce).await.unwrap().display_name,
        "Announce"
    );
}

/// The accounts, their addresses and the membership that belongs to one.
async fn accounts_applied(db: &Database) {
    let dave = db.addresses().get("dave@example.invalid").await.unwrap();
    let user_id = dave.user_id.expect("an account behind the address");
    let user = db.users().get(user_id).await.unwrap();
    assert_eq!(user.display_name, "Dave");
    assert_eq!(user.preferred_address_id, Some(dave.id));
    let spare = db.addresses().get("dave@work.invalid").await.unwrap();
    assert_eq!(spare.user_id, Some(user_id));
    let preferences = db.preferences().get_user(user_id).await.unwrap();
    assert_eq!(preferences.hide_address, Some(true));
    // The account that Mailman made a server owner is one here too, and
    // its unverified address stays unverified.
    let eve = db.addresses().get("eve@example.invalid").await.unwrap();
    assert!(
        db.users()
            .get(eve.user_id.unwrap())
            .await
            .unwrap()
            .is_server_owner
    );
    assert!(eve.verified_on.is_none());
    // No account can be signed into: every imported one needs a reset.
    let usable: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_credentials WHERE usable=1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(usable, 0);
    // Dave's membership is the account's, as Mailman had it.
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    let member = db
        .members()
        .roster(&list, MemberRole::Member)
        .await
        .unwrap()
        .into_iter()
        .find(|member| member.address_id == dave.id)
        .unwrap();
    assert_eq!(member.subscription_mode, SubscriptionMode::AsUser);
    assert_eq!(member.user_id, Some(user_id));
}

/// The rosters with their own preferences.
async fn rosters_applied(db: &Database) {
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    let members = db
        .members()
        .roster(&list, MemberRole::Member)
        .await
        .unwrap();
    assert_eq!(members.len(), 4);
    let mut alice = None;
    for member in &members {
        let address = db.addresses().get_by_id(member.address_id).await.unwrap();
        if address.email == "alice@example.invalid" {
            alice = Some(member.clone());
        }
    }
    let alice = alice.unwrap();
    assert_eq!(alice.display_name, "Alice Nguyễn");
    let prefs = db.preferences().get(alice.preferences_id).await.unwrap();
    assert_eq!(prefs.delivery_mode, Some(DeliveryMode::PlaintextDigests));
    assert_eq!(prefs.acknowledge_posts, Some(true));
    assert_eq!(prefs.hide_address, Some(true));
    assert_eq!(prefs.receive_own_postings, Some(false));
    let nonmembers = db
        .members()
        .roster(&list, MemberRole::Nonmember)
        .await
        .unwrap();
    assert_eq!(
        nonmembers[0].moderation_action,
        Some(ModerationAction::Discard)
    );
}

/// What was waiting, waiting here now.
async fn waiting_applied(db: &Database) {
    let announce: ListId = "announce.other.invalid".parse().unwrap();
    let held = db.moderation().list_pending(&announce).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].sender, "stranger@example.invalid");
    assert_eq!(held[0].subject, "Please let me in");
    assert_eq!(held[0].reason, "The message is not from a list member");
    let stored = db.mail_queue().message(held[0].message_id).await.unwrap();
    assert!(String::from_utf8_lossy(&stored.raw).contains("Please let me in"));
    let requests = db
        .workflows()
        .pending(&announce, listmngr_db::workflows::RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].email, "wanted@example.invalid");
    assert_eq!(
        requests[0].token_owner,
        listmngr_db::workflows::TokenOwner::Moderator
    );
    // An import mails nobody: no notice was queued for either of them.
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
}

/// Bans (list and site), header matches, and the audit trail.
async fn lists_applied(db: &Database) {
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    assert!(
        db.bans()
            .is_banned(&list, "spammer@example.invalid")
            .await
            .unwrap()
    );
    assert!(
        db.bans()
            .is_banned(&list, "global-spammer@example.invalid")
            .await
            .unwrap(),
        "a site ban binds every list"
    );
    let matches = db.header_matches().list(&list).await.unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].header, "x-spam-flag");
    assert_eq!(matches[0].chain.as_deref(), Some("discard"));
    // Every write left its audit event, and the import its summary.
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT action FROM audit_log ORDER BY action")
            .fetch_all(db.pool())
            .await
            .unwrap();
    for expected in [
        "domain.create",
        "list.create",
        "list.config",
        "member.create",
        "preferences.update",
        "ban.create",
        "list.header_matches",
        "moderation.import",
        "subscription.import",
        "site.import3",
    ] {
        assert!(
            actions.iter().any(|action| action == expected),
            "{expected} in {actions:?}"
        );
    }
}

#[tokio::test]
async fn a_recorded_site_applies_on_sqlite() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_import3_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("import3")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
