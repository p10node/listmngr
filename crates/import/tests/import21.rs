//! `import21`: a Mailman 2.1 `config.pck` read and applied the way
//! Mailman 3's importer applies it — settings, rosters with their options,
//! bans, header filter rules, acceptable aliases and templates.
use listmngr_core::{DeliveryMode, DeliveryStatus, ListId, MemberRole, ModerationAction};
use listmngr_db::{AuditContext, Database, NewList};
use listmngr_import::{Config21, Plan, apply, plan};
use serde_json::{Value, json};

const FULL: &[u8] = include_bytes!("fixtures/mailman21-full.pck");
const MINIMAL: &[u8] = include_bytes!("fixtures/mailman21-minimal.pck");

async fn fixture(db: &Database, name: &str) -> ListId {
    db.migrate().await.unwrap();
    db.domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let list: ListId = format!("{name}.example.invalid").parse().unwrap();
    db.lists()
        .create(NewList {
            list_id: list.clone(),
            display_name: name.into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    list
}

async fn config(db: &Database, list: &ListId) -> Value {
    serde_json::to_value(db.lists().get(list).await.unwrap()).unwrap()
}

#[test]
fn a_config_pck_is_read_with_its_python_2_strings_as_text() {
    let config = Config21::from_pickle(FULL).unwrap();
    assert_eq!(config.text("real_name").as_deref(), Some("Rust-Users"));
    // 2.1's `_BounceInfo` instances (the `OBJ` opcode) read as None, and
    // nothing around them is disturbed.
    assert_eq!(
        config.dict("bounce_info").get("alice@example.invalid"),
        Some(&listmngr_import::config21::Value::None)
    );
    assert_eq!(config.dict("bounce_info").len(), 2);
    assert_eq!(config.text("host_name").as_deref(), Some("example.invalid"));
    assert_eq!(config.int("subscribe_policy"), Some(3));
    assert_eq!(config.bool("anonymous_list"), Some(true));
    assert_eq!(config.float("bounce_score_threshold"), Some(7.5));
    assert_eq!(
        config.text_list("pass_mime_types"),
        vec!["multipart/mixed", "text/plain"]
    );
    let members = config.dict("members");
    assert_eq!(members.len(), 4);
    assert_eq!(
        config
            .text_dict("usernames")
            .get("alice@example.invalid")
            .map(String::as_str),
        Some("Alice Nguyễn")
    );
    assert!(config.text("no_such_key").is_none());
    assert!(Config21::from_pickle(b"not a pickle").is_err());
    assert!(
        Config21::from_pickle(&serde_json::to_vec(&json!([1, 2])).unwrap()).is_err(),
        "a pickle that is not a dict"
    );
}

fn full_plan() -> Plan {
    let config = Config21::from_pickle(FULL).unwrap();
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    plan(&config, &list)
}

/// Mailman's importer mapping, as a plan: every 2.1 key that has a Mailman
/// 3 setting, with Mailman's conversions and renames.
#[test]
fn the_plan_maps_every_setting_as_mailmans_importer_does() {
    let plan = full_plan();
    let s = &plan.settings;
    let expected = [
        ("display_name", json!("Rust-Users")),
        ("description", json!("Rust users of Example")),
        ("info", json!("Long description\nwith two lines")),
        ("subject_prefix", json!("[Rust] ")), // a space after the prefix, as Mailman 3 wants it
        ("preferred_language", json!("vi")),
        ("advertised", json!(false)),
        ("anonymous_list", json!(true)),
        ("admin_immed_notify", json!(false)),
        ("admin_notify_mchanges", json!(true)),
        ("administrivia", json!(false)),
        ("require_explicit_destination", json!(false)),
        ("respond_to_post_requests", json!(false)),
        ("send_welcome_message", json!(false)),
        ("send_goodbye_message", json!(false)),
        ("allow_list_posts", json!(false)),
        ("include_rfc2369_headers", json!(false)),
        ("archive_policy", json!("private")),
        ("autorespond_owner", json!("respond_and_continue")),
        (
            "autoresponse_owner_text",
            json!("Owner reply $display_name"),
        ),
        ("autorespond_postings", json!("respond_and_continue")),
        ("autorespond_requests", json!("respond_and_discard")),
        ("autoresponse_grace_period", json!(3)),
        ("process_bounces", json!(false)),
        ("bounce_score_threshold", json!(7.5)),
        ("bounce_info_stale_after", json!(14)), // seconds become days
        ("bounce_you_are_disabled_warnings", json!(2)),
        ("bounce_you_are_disabled_warnings_interval", json!(3)),
        ("bounce_notify_owner_on_disable", json!(false)),
        ("bounce_notify_owner_on_removal", json!(false)),
        ("forward_unrecognized_bounces_to", json!("discard")),
        ("collapse_alternatives", json!(false)),
        ("convert_html_to_plaintext", json!(false)),
        ("filter_action", json!("forward")),
        ("filter_content", json!(true)),
        ("filter_extensions", json!(["exe", "bat"])),
        (
            "filter_types",
            json!(["image/jpeg", "application/octet-stream"]),
        ),
        ("pass_extensions", json!(["txt", "pdf"])),
        ("pass_types", json!(["multipart/mixed", "text/plain"])),
        ("default_member_action", json!("reject")), // moderated members, action 1
        ("default_nonmember_action", json!("discard")),
        // from_is_list (2) outranks dmarc_moderation_action (1): unconditional.
        ("dmarc_mitigate_action", json!("wrap_message")),
        ("dmarc_mitigate_unconditionally", json!(true)),
        (
            "dmarc_addresses",
            json!(["^.*@yahoo\\.com$", "friend@example.invalid"]),
        ),
        (
            "dmarc_moderation_notice",
            json!("Your domain publishes a DMARC policy."),
        ),
        (
            "dmarc_wrapped_message_text",
            json!("The original post is attached."),
        ),
        ("digest_send_periodic", json!(false)),
        ("digest_size_threshold", json!(45.0)),
        ("digest_volume_frequency", json!("weekly")),
        ("next_digest_number", json!(4)),
        ("emergency", json!(true)),
        ("first_strip_reply_to", json!(true)),
        ("reply_goes_to_list", json!("explicit_header")),
        ("reply_to_address", json!("replies@example.invalid")),
        ("personalize", json!("individual")),
        ("subscription_policy", json!("confirm_then_moderate")),
        ("member_roster_visibility", json!("moderators")),
        ("max_message_size", json!(120)),
        ("max_num_recipients", json!(25)),
        ("gateway_to_mail", json!(true)),
        ("gateway_to_news", json!(true)),
        ("linked_newsgroup", json!("comp.lang.rust.lists")),
        ("newsgroup_moderation", json!("moderated")),
        ("nntp_prefix_subject_too", json!(false)),
        ("topics_enabled", json!(true)),
        ("topics_bodylines_limit", json!(7)),
        (
            "topics",
            json!([ {"name": "release", "pattern": "^subject:.*release", "description": "Releases"}, {"name": "bugs", "pattern": "^subject:.*bug", "description": "Bug reports"} ]),
        ),
    ];
    for (key, value) in expected {
        assert_eq!(s[key], value, "{key}");
    }
}

/// Nonmember lists, aliases and the settings Mailman does not import.
#[test]
fn the_plan_keeps_patterns_on_the_list_and_warns_about_the_rest() {
    let plan = full_plan();
    let s = &plan.settings;
    // Regex and list entries stay on the list; addresses become nonmembers.
    assert_eq!(
        s["accept_these_nonmembers"],
        json!(["^.*@partner\\.invalid"])
    );
    assert_eq!(
        s["reject_these_nonmembers"],
        json!(["^.*@reject\\.invalid"])
    );
    assert!(
        s.get("hold_these_nonmembers")
            .is_none_or(|v| v == &json!([]))
    );
    // Acceptable aliases: each line anchored, the list's own name added.
    assert_eq!(
        s["acceptable_aliases"],
        json!([
            "^rust-users-alias@example.invalid",
            "^announce-.*@example\\.invalid",
            "^rust-users@"
        ])
    );
    // Not settings here: a hashed 2.1 moderator password, the unsubscribe
    // policy Mailman 3 does not import, the list's own name entries.
    assert!(s.get("moderator_password").is_none());
    assert!(s.get("unsubscription_policy").is_none());
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("moderator password")),
        "{:?}",
        plan.warnings
    );
    assert!(
        plan.warnings.iter().any(|w| w.contains("@otherlist")),
        "{:?}",
        plan.warnings
    );
}

/// Bans and header filter rules, as Mailman parses them.
#[test]
fn the_plan_reads_bans_and_header_filter_rules_as_mailman_does() {
    let plan = full_plan();
    // Bans: the unparsable regex dropped with a warning, as Mailman drops it.
    assert_eq!(
        plan.bans,
        vec!["spammer@example.invalid", "^.*@spam\\.invalid"]
    );
    assert!(plan.warnings.iter().any(|w| w.contains("^[unclosed")));
    // Header filter rules in Mailman's parsing: one rule per line, the
    // separator found among `: `, `:.*`, `:.`, `:`, the chain from the
    // action (0 the site default, 2 reject, 3 discard, 6 accept, 7 hold),
    // an empty pattern `.*`, a line without a header skipped, an invalid
    // regex skipped.
    let rules: Vec<(String, String, Option<String>)> = plan
        .header_matches
        .iter()
        .map(|r| (r.header.clone(), r.pattern.clone(), r.chain.clone()))
        .collect();
    assert_eq!(
        rules,
        vec![
            ("x-spam-flag".into(), "YES".into(), Some("discard".into())),
            (
                "subject".into(),
                ".*viagra.*".into(),
                Some("discard".into())
            ),
            ("x-trusted".into(), "yes".into(), Some("accept".into())),
            ("list-post".into(), ".*".into(), None),
        ]
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("nonsense-without-separator"))
    );
    assert!(plan.warnings.iter().any(|w| w.contains("Subject: [")));
}

/// The decorations, with Mailman's placeholder conversion.
#[test]
fn the_plan_converts_template_placeholders() {
    let plan = full_plan();
    // Templates with Mailman's placeholder conversion; the 2.1 default
    // digest footer equals Mailman 3's and is left to the default.
    let templates: std::collections::BTreeMap<_, _> = plan
        .templates
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    assert_eq!(
        templates.get("list:member:regular:header"),
        Some(&"Header of $listname")
    );
    assert_eq!(
        templates.get("list:member:regular:footer"),
        Some(&"Footer for $user_email of $display_name")
    );
    assert_eq!(
        templates.get("list:member:digest:header"),
        Some(&"Digest header for $display_name")
    );
    assert_eq!(
        templates.get("list:user:notice:goodbye"),
        Some(&"Goodbye from $display_name, $user_name")
    );
    assert!(
        !templates.contains_key("list:member:digest:footer"),
        "the 2.1 default footer is Mailman 3's default: {templates:?}"
    );
}

/// Rosters, as Mailman reads them: options bits, delivery status codes,
/// digest kinds, bans, owners, moderators and nonmembers.
#[test]
fn the_plan_reads_the_rosters_as_mailman_does() {
    let plan = full_plan();
    // Rosters, as Mailman reads them.
    let member = |email: &str| {
        plan.members
            .iter()
            .find(|m| m.email == email && m.role == MemberRole::Member)
            .unwrap_or_else(|| panic!("{email}"))
    };
    let alice = member("alice@example.invalid");
    assert_eq!(alice.display_name, "Alice Nguyễn");
    assert_eq!(alice.delivery_mode, DeliveryMode::Regular);
    assert_eq!(alice.delivery_status, DeliveryStatus::Enabled);
    assert_eq!(alice.preferred_language.as_deref(), Some("vi"));
    assert_eq!(
        alice.moderation_action,
        Some(ModerationAction::Reject),
        "the moderate bit"
    );
    assert_eq!(alice.acknowledge_posts, Some(true));
    assert_eq!(alice.hide_address, Some(true));
    assert_eq!(alice.receive_own_postings, Some(false));
    assert_eq!(alice.receive_list_copy, Some(true));
    let bob = member("bob@example.invalid");
    assert_eq!(
        bob.original_email, "Bob@Example.invalid",
        "the case the member wrote"
    );
    assert_eq!(bob.delivery_status, DeliveryStatus::ByUser);
    assert_eq!(
        bob.preferred_language, None,
        "an unknown language is dropped"
    );
    assert_eq!(bob.receive_list_copy, Some(false));
    assert_eq!(bob.moderation_action, Some(ModerationAction::Defer));
    let carol = member("carol@elsewhere.invalid");
    assert_eq!(carol.delivery_status, DeliveryStatus::ByBounces);
    let dave = member("dave@example.invalid");
    assert_eq!(
        dave.delivery_mode,
        DeliveryMode::PlaintextDigests,
        "DisableMime"
    );
    assert_eq!(dave.delivery_status, DeliveryStatus::ByModerator);
    let erin = member("erin@example.invalid");
    assert_eq!(erin.delivery_mode, DeliveryMode::MimeDigests);
}

/// Owners, moderators, nonmembers, and a banned address left out.
#[test]
fn the_plan_reads_the_other_roles_as_mailman_does() {
    let plan = full_plan();
    assert!(
        !plan
            .members
            .iter()
            .any(|m| m.email == "spammer@example.invalid"),
        "a banned address is not imported"
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("spammer@example.invalid"))
    );
    let owners: Vec<_> = plan
        .members
        .iter()
        .filter(|m| m.role == MemberRole::Owner)
        .map(|m| m.email.as_str())
        .collect();
    assert_eq!(owners, ["owner@example.invalid", "alice@example.invalid"]);
    assert!(
        plan.members
            .iter()
            .any(|m| m.email == "mod@example.invalid" && m.role == MemberRole::Moderator)
    );
    let nonmember = |email: &str| {
        plan.members
            .iter()
            .find(|m| m.email == email && m.role == MemberRole::Nonmember)
            .unwrap_or_else(|| panic!("{email}"))
    };
    assert_eq!(
        nonmember("friend@example.invalid").moderation_action,
        Some(ModerationAction::Defer)
    );
    assert_eq!(
        nonmember("suspect@example.invalid").moderation_action,
        Some(ModerationAction::Hold)
    );
    assert_eq!(
        nonmember("noise@example.invalid").moderation_action,
        Some(ModerationAction::Discard)
    );
}

/// The 2.1 defaults map to a list at Mailman 3's defaults, plus the
/// alias for the list's own name.
#[test]
fn a_minimal_config_plans_mailmans_defaults() {
    let config = Config21::from_pickle(MINIMAL).unwrap();
    let list: ListId = "test.example.invalid".parse().unwrap();
    let plan = plan(&config, &list);
    let s = &plan.settings;
    assert_eq!(s["display_name"], "Test");
    assert_eq!(s["subject_prefix"], "[Test] ");
    assert_eq!(s["archive_policy"], "public");
    assert_eq!(s["default_member_action"], "defer");
    assert_eq!(s["default_nonmember_action"], "hold");
    assert_eq!(s["dmarc_mitigate_action"], "no_mitigation");
    assert_eq!(s["dmarc_mitigate_unconditionally"], false);
    assert_eq!(
        s["forward_unrecognized_bounces_to"], "site_owner",
        "Mailman's quirk: True is the enum's 1"
    );
    assert_eq!(s["member_roster_visibility"], "members");
    assert_eq!(s["acceptable_aliases"], json!(["^test@"]));
    assert!(plan.members.is_empty());
    assert!(plan.bans.is_empty());
    assert!(plan.header_matches.is_empty());
    assert!(plan.templates.is_empty(), "{:?}", plan.templates);
}

async fn scenario(db: &Database) {
    let list = fixture(db, "rust-users").await;
    let config = Config21::from_pickle(FULL).unwrap();
    let plan = plan(&config, &list);
    let report = apply(db, &list, &plan, &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(report.members, 5, "{report:?}");
    assert_eq!(report.owners, 2);
    assert_eq!(report.moderators, 1);
    assert_eq!(report.nonmembers, 3);
    assert_eq!(report.bans, 2);
    assert_eq!(report.header_matches, 4);
    assert_eq!(report.templates, 4);
    // The list carries the settings.
    let saved = config_of(db, &list).await;
    for (key, value) in [
        ("display_name", json!("Rust-Users")),
        ("subject_prefix", json!("[Rust] ")),
        ("preferred_language", json!("vi")),
        ("archive_policy", json!("private")),
        ("dmarc_mitigate_action", json!("wrap_message")),
        ("dmarc_mitigate_unconditionally", json!(true)),
        ("default_member_action", json!("reject")),
        ("linked_newsgroup", json!("comp.lang.rust.lists")),
        ("newsgroup_moderation", json!("moderated")),
    ] {
        assert_eq!(saved[key], value, "{key}");
    }
    assert_eq!(saved["acceptable_aliases"][2], "^rust-users@");
    assert_eq!(saved["topics"][1]["name"], "bugs");
    rosters_applied(db, &list).await;
    lists_applied(db, &list).await;
    // A second import is a no-op for what is there already.
    let again = apply(db, &list, &plan, &AuditContext::system())
        .await
        .unwrap();
    assert_eq!(again.members, 0, "{again:?}");
    assert_eq!(again.owners, 0);
    assert_eq!(again.bans, 0);
    assert_eq!(roster(db, &list, MemberRole::Member).await.len(), 5);
    assert_eq!(db.header_matches().list(&list).await.unwrap().len(), 4);
}

/// The rosters with their options.
async fn rosters_applied(db: &Database, list: &ListId) {
    let list = list.clone();
    let members = roster(db, &list, MemberRole::Member).await;
    let mut emails: Vec<_> = members.iter().map(|(email, _)| email.as_str()).collect();
    emails.sort_unstable();
    assert_eq!(
        emails,
        [
            "Bob@Example.invalid",
            "alice@example.invalid",
            "carol@elsewhere.invalid",
            "dave@example.invalid",
            "erin@example.invalid"
        ]
    );
    let alice = &members
        .iter()
        .find(|(e, _)| e == "alice@example.invalid")
        .unwrap()
        .1;
    assert_eq!(alice.display_name, "Alice Nguyễn");
    assert_eq!(alice.moderation_action, Some(ModerationAction::Reject));
    let prefs = db.preferences().get(alice.preferences_id).await.unwrap();
    assert_eq!(prefs.delivery_mode, Some(DeliveryMode::Regular));
    assert_eq!(prefs.acknowledge_posts, Some(true));
    assert_eq!(prefs.hide_address, Some(true));
    assert_eq!(prefs.receive_own_postings, Some(false));
    assert_eq!(prefs.preferred_language.as_deref(), Some("vi"));
    let dave = &members
        .iter()
        .find(|(e, _)| e == "dave@example.invalid")
        .unwrap()
        .1;
    let prefs = db.preferences().get(dave.preferences_id).await.unwrap();
    assert_eq!(prefs.delivery_mode, Some(DeliveryMode::PlaintextDigests));
    assert_eq!(prefs.delivery_status, Some(DeliveryStatus::ByModerator));
    assert_eq!(roster(db, &list, MemberRole::Owner).await.len(), 2);
    let nonmembers = roster(db, &list, MemberRole::Nonmember).await;
    let noise = &nonmembers
        .iter()
        .find(|(e, _)| e == "noise@example.invalid")
        .unwrap()
        .1;
    assert_eq!(noise.moderation_action, Some(ModerationAction::Discard));
}

/// Bans, rules, templates, and the audit trail.
async fn lists_applied(db: &Database, list: &ListId) {
    let list = list.clone();
    assert!(
        db.bans()
            .is_banned(&list, "spammer@example.invalid")
            .await
            .unwrap()
    );
    assert!(
        db.bans()
            .is_banned(&list, "anyone@spam.invalid")
            .await
            .unwrap()
    );
    let rules = db.header_matches().list(&list).await.unwrap();
    assert_eq!(rules.len(), 4);
    assert_eq!(rules[0].header, "x-spam-flag");
    let row = db.lists().get(&list).await.unwrap();
    let footer = db
        .templates()
        .resolve("list:member:regular:footer", &row, "vi")
        .await
        .unwrap();
    assert_eq!(footer.body, "Footer for $user_email of $display_name");
    // Every write left its audit event, and the import its summary.
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT action FROM audit_log ORDER BY action")
            .fetch_all(db.pool())
            .await
            .unwrap();
    for expected in [
        "list.config",
        "member.create",
        "preferences.update",
        "ban.create",
        "list.header_matches",
        "template.set",
        "list.import21",
    ] {
        assert!(
            actions.iter().any(|a| a == expected),
            "{expected} in {actions:?}"
        );
    }
}

async fn config_of(db: &Database, list: &ListId) -> Value {
    config(db, list).await
}

/// The role's members with the address each subscribed as.
async fn roster(
    db: &Database,
    list: &ListId,
    role: MemberRole,
) -> Vec<(String, listmngr_core::Member)> {
    let mut out = Vec::new();
    for member in db.members().roster(list, role).await.unwrap() {
        let address = db.addresses().get_by_id(member.address_id).await.unwrap();
        out.push((address.original_email, member));
    }
    out
}

#[tokio::test]
async fn the_full_config_applies_to_a_list_on_sqlite() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_import21_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("import21")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}

/// Mailman 3's own importer fixture (`mailman/testing/config.pck`, a real
/// Mailman 2.1 pickle): read and planned, when the installed package's
/// `testing/` directory is named.
#[test]
#[ignore = "requires MAILMAN_TESTING_DIR (the installed mailman package's testing/ directory)"]
fn import21_reads_mailman3s_own_fixture() {
    let dir = std::env::var("MAILMAN_TESTING_DIR").expect("MAILMAN_TESTING_DIR");
    let list: ListId = "test.heresy.example.org".parse().unwrap();
    // `config-with-instances.pck` is another list, whose `bounce_info`
    // holds `Mailman.Bouncer._BounceInfo` instances (the `OBJ` opcode).
    let bytes = std::fs::read(format!("{dir}/config-with-instances.pck")).unwrap();
    let config = Config21::from_pickle(&bytes).unwrap();
    let instances = plan(
        &config,
        &"eclipse-sig.lists.examplexxxxxx.org".parse().unwrap(),
    );
    assert_eq!(instances.settings["display_name"], "eclipse-sig");
    assert_eq!(instances.settings["subject_prefix"], "[eclipse-sig] ");
    assert_eq!(instances.members.len(), 8, "{:?}", instances.members);
    assert_eq!(
        instances
            .members
            .iter()
            .filter(|m| m.role == MemberRole::Member)
            .count(),
        3
    );
    assert_eq!(
        config.dict("bounce_info").values().next(),
        Some(&listmngr_import::config21::Value::None),
        "an instance reads as None, as Mailman's importer ignores it"
    );
    println!(
        "config-with-instances.pck: {} settings, {} members, warnings {:?}",
        instances.settings.len(),
        instances.members.len(),
        instances.warnings
    );
    for name in ["config.pck", "config-greek.pck"] {
        let bytes = std::fs::read(format!("{dir}/{name}")).unwrap();
        let config = Config21::from_pickle(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let plan = plan(&config, &list);
        assert_eq!(plan.settings["display_name"], "Test", "{name}");
        assert_eq!(plan.settings["subject_prefix"], "[Test] ", "{name}");
        assert_eq!(plan.settings["archive_policy"], "public", "{name}");
        assert_eq!(plan.settings["default_nonmember_action"], "hold", "{name}");
        assert_eq!(
            plan.members
                .iter()
                .filter(|m| m.role == MemberRole::Member)
                .count(),
            3,
            "{name}"
        );
        assert_eq!(
            plan.members
                .iter()
                .filter(|m| m.role == MemberRole::Owner)
                .map(|m| m.email.as_str())
                .collect::<Vec<_>>(),
            ["anne@example.com"],
            "{name}"
        );
        assert_eq!(
            plan.settings["acceptable_aliases"],
            json!(["^test@"]),
            "{name}"
        );
        println!(
            "{name}: {} settings, {} members, warnings {:?}",
            plan.settings.len(),
            plan.members.len(),
            plan.warnings
        );
    }
}
