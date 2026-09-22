//! `import3 --db`: the same Mailman 3 site read straight from the core's
//! own database and message store, with no core running.
//!
//! `fixtures/mailman3/mailman.db` is the `SQLite` database of the real
//! GNU Mailman 3.3.10 core whose REST answers the other fixtures record,
//! copied after `tests/compat/generate_mailman3_rest.py` ran; so the site
//! it yields must plan exactly what the REST answers plan.
use listmngr_core::{ListId, MemberRole};
use listmngr_db::{AuditContext, Database};
use listmngr_import::db3::{fetch_db, render_message};
use listmngr_import::import3::{Site, Source, apply, fetch, plan};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mailman3")
}

fn database_url() -> String {
    format!(
        "sqlite://{}?mode=ro",
        fixtures().join("mailman.db").display()
    )
}

/// The recorded REST answers, the way `import3.rs` replays them.
struct Recorded;

fn fixture(name: &str) -> Value {
    let path = fixtures().join(format!("{name}.json"));
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

impl Source for Recorded {
    async fn get_optional(&self, path: &str) -> listmngr_import::Result<Option<Value>> {
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
        let keyed = [
            ("members/", "/preferences", "preferences"),
            ("users/", "/addresses", "user-addresses"),
            ("users/", "/preferences", "user-preferences"),
        ];
        for (prefix, suffix, file) in keyed {
            if let Some(id) = path
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
            {
                return Ok(fixture(file).get(id).cloned().unwrap_or_else(|| json!({})));
            }
        }
        let name = match path {
            "domains" => "domains".to_owned(),
            "users" => "users".to_owned(),
            "bans" => "bans-global".to_owned(),
            "lists?advertised=false" => "lists".to_owned(),
            path => {
                let rest = path.trim_start_matches("lists/");
                let (list, tail) = rest.split_once('/').unwrap();
                let prefix = if list == "announce.other.invalid" {
                    "announce-"
                } else {
                    ""
                };
                match tail {
                    "config" if prefix.is_empty() => "list-config".to_owned(),
                    "bans" if prefix.is_empty() => "bans-list".to_owned(),
                    tail if tail.starts_with("roster/") => {
                        format!("{prefix}roster-{}", tail.trim_start_matches("roster/"))
                    }
                    tail => format!("{prefix}{tail}"),
                }
            }
        };
        Ok(fixture(&name))
    }
}

async fn from_rest() -> Site {
    fetch(&Recorded, None).await.unwrap()
}

async fn from_db() -> Site {
    fetch_db(&database_url(), Some(&fixtures().join("var")))
        .await
        .unwrap()
}

/// Plans keyed so their order does not matter.
fn settings_of(site: &Site) -> BTreeMap<String, serde_json::Map<String, Value>> {
    plan(site)
        .lists
        .iter()
        .map(|list| (list.list_id.to_string(), list.settings.clone()))
        .collect()
}

#[tokio::test]
async fn the_database_plans_what_the_rest_answers_plan() {
    let rest = from_rest().await;
    let db = from_db().await;
    let (rest_settings, db_settings) = (settings_of(&rest), settings_of(&db));
    for (list, wanted) in &rest_settings {
        let got = &db_settings[list];
        for (key, value) in wanted {
            assert_eq!(got.get(key), Some(value), "{list}: {key}");
        }
        for key in got.keys() {
            assert!(
                wanted.contains_key(key),
                "{list}: {key} only from the database"
            );
        }
    }
    assert_eq!(db.domains, rest.domains);
    assert_eq!(db.site_bans, rest.site_bans);
    let by_id = |site: &Site| -> BTreeMap<String, _> {
        site.lists
            .iter()
            .map(|list| (list.list_id.to_string(), list.clone()))
            .collect()
    };
    let (rest_lists, db_lists) = (by_id(&rest), by_id(&db));
    assert_eq!(
        db_lists.keys().collect::<Vec<_>>(),
        rest_lists.keys().collect::<Vec<_>>()
    );
    for (id, wanted) in &rest_lists {
        let got = &db_lists[id];
        assert_eq!(got.display_name, wanted.display_name, "{id}");
        assert_eq!(got.mail_host, wanted.mail_host, "{id}");
        assert_eq!(got.bans, wanted.bans, "{id}");
        assert_eq!(got.header_matches, wanted.header_matches, "{id}");
        assert_eq!(got.uris, wanted.uris, "{id}");
        assert_eq!(got.requests, wanted.requests, "{id}");
        let mut got_members = got.members.clone();
        let mut wanted_members = wanted.members.clone();
        got_members.sort_by_key(|m| (m.email.clone(), m.role.as_str()));
        wanted_members.sort_by_key(|m| (m.email.clone(), m.role.as_str()));
        assert_eq!(got_members, wanted_members, "{id}");
        // The held message: same sender, subject, reason and date, and
        // the same message once rendered from the store's pickle.
        assert_eq!(got.held.len(), wanted.held.len(), "{id}");
        for (mine, theirs) in got.held.iter().zip(&wanted.held) {
            assert_eq!(mine.sender, theirs.sender);
            assert_eq!(mine.subject, theirs.subject);
            assert_eq!(mine.reason, theirs.reason);
            assert_eq!(mine.hold_date, theirs.hold_date);
            assert_eq!(
                String::from_utf8_lossy(&mine.raw),
                String::from_utf8_lossy(&theirs.raw)
            );
        }
    }
    // The accounts, by their first address.
    let key = |site: &Site| -> BTreeMap<String, _> {
        site.users
            .iter()
            .map(|user| (user.addresses[0].email.clone(), user.clone()))
            .collect()
    };
    let (rest_users, db_users) = (key(&rest), key(&db));
    assert_eq!(db_users.len(), rest_users.len());
    for (email, wanted) in &rest_users {
        let got = &db_users[email];
        assert_eq!(got.display_name, wanted.display_name, "{email}");
        assert_eq!(got.is_server_owner, wanted.is_server_owner, "{email}");
        assert_eq!(got.has_password, wanted.has_password, "{email}");
        assert_eq!(got.addresses, wanted.addresses, "{email}");
        assert_eq!(got.preferred, wanted.preferred, "{email}");
        assert_eq!(got.preferences, wanted.preferences, "{email}");
    }
    assert!(db.warnings.is_empty(), "{:?}", db.warnings);
}

/// Without the message store, the held message cannot come — and says so.
#[tokio::test]
async fn without_the_message_store_a_held_message_is_a_warning() {
    let site = fetch_db(&database_url(), None).await.unwrap();
    let announce = site
        .lists
        .iter()
        .find(|list| list.list_id.as_str() == "announce.other.invalid")
        .unwrap();
    assert!(announce.held.is_empty());
    assert!(
        site.warnings
            .iter()
            .any(|warning| warning.contains("<held-1@example.invalid>")
                && warning.contains("var-dir")),
        "{:?}",
        site.warnings
    );
    assert_eq!(announce.requests.len(), 1, "the request needs no store");
    let plan = plan(&site);
    assert!(
        plan.warnings
            .iter()
            .any(|warning| warning.contains("<held-1@example.invalid>"))
    );
}

/// The message store keeps a pickled `email.message.Message`; rendering
/// it back gives what Python's generator gives.
#[test]
fn a_pickled_message_renders_as_python_would() {
    let pickled = std::fs::read(fixtures().join("var/multipart.pck")).unwrap();
    let rendered = render_message(&pickled).unwrap();
    let expected = "From: alice@example.invalid\nTo: dev@example.invalid\nSubject: =?utf-8?b?WGluIGNow6Bv?=\nMessage-ID: <multi@example.invalid>\nMIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"=-=frontier=-=\"\n\npreamble text\n--=-=frontier=-=\nContent-Type: text/plain; charset=\"utf-8\"\nContent-Transfer-Encoding: 8bit\n\nXin chào mọi người.\n--=-=frontier=-=\nContent-Type: application/octet-stream\nContent-Transfer-Encoding: base64\nContent-Disposition: attachment; filename=\"a.bin\"\n\nAAECAw==\n--=-=frontier=-=--\nepilogue text\n";
    assert_eq!(String::from_utf8_lossy(&rendered), expected);
    assert!(render_message(b"not a pickle").is_err());
}

/// A database URL nobody can open is an error that names the failure,
/// not a panic.
#[tokio::test]
async fn an_unreadable_database_is_an_error() {
    let error = fetch_db("sqlite:///nonexistent/dir/mailman.db?mode=ro", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("database"), "{error}");
}

/// A real core's database, when one is named — the PostgreSQL path, with
/// its real booleans and intervals, populated by the same script as the
/// SQLite fixture and so planning the same site.
#[tokio::test]
#[ignore = "requires MAILMAN3_DB_URL (a disposable Mailman 3 core's database) and MAILMAN3_VAR_DIR"]
async fn import3_reads_a_real_cores_database() {
    let url = std::env::var("MAILMAN3_DB_URL").expect("MAILMAN3_DB_URL");
    let var_dir = PathBuf::from(std::env::var("MAILMAN3_VAR_DIR").expect("MAILMAN3_VAR_DIR"));
    let live = fetch_db(&url, Some(&var_dir)).await.unwrap();
    let recorded = from_db().await;
    println!(
        "{} domains, {} users, {} lists, {} held, {} requests, warnings {:?}",
        live.domains.len(),
        live.users.len(),
        live.lists.len(),
        live.lists.iter().map(|list| list.held.len()).sum::<usize>(),
        live.lists
            .iter()
            .map(|list| list.requests.len())
            .sum::<usize>(),
        live.warnings
    );
    assert_eq!(live.domains, recorded.domains);
    assert_eq!(live.site_bans, recorded.site_bans);
    assert_eq!(live.users.len(), recorded.users.len());
    let (live_settings, recorded_settings) = (settings_of(&live), settings_of(&recorded));
    assert_eq!(
        live_settings, recorded_settings,
        "the same configuration, backend for backend"
    );
    for (mine, theirs) in live.lists.iter().zip(&recorded.lists) {
        assert_eq!(mine.list_id, theirs.list_id);
        assert_eq!(mine.bans, theirs.bans);
        assert_eq!(mine.header_matches, theirs.header_matches);
        assert_eq!(mine.members.len(), theirs.members.len());
        assert_eq!(mine.held.len(), theirs.held.len());
        assert_eq!(mine.requests.len(), theirs.requests.len());
    }
    assert!(live.warnings.is_empty(), "{:?}", live.warnings);
}

async fn scenario(db: &Database) {
    db.migrate().await.unwrap();
    let site = from_db().await;
    let plan = plan(&site);
    let report = apply(db, &plan, &AuditContext::system()).await.unwrap();
    assert_eq!(report.domains, 2, "{report:?}");
    assert_eq!(report.users, 9);
    assert_eq!(report.lists, 2);
    assert_eq!(report.members, 4);
    assert_eq!(report.held, 1);
    assert_eq!(report.requests, 1);
    let list: ListId = "rust-users.example.invalid".parse().unwrap();
    let saved: Value = serde_json::to_value(db.lists().get(&list).await.unwrap()).unwrap();
    assert_eq!(saved["subject_prefix"], "[Rust] ");
    assert_eq!(saved["bounce_info_stale_after"], 7);
    assert_eq!(saved["autoresponse_grace_period"], 90);
    assert_eq!(
        db.members()
            .roster(&list, MemberRole::Member)
            .await
            .unwrap()
            .len(),
        4
    );
    let announce: ListId = "announce.other.invalid".parse().unwrap();
    let held = db.moderation().list_pending(&announce).await.unwrap();
    assert_eq!(held.len(), 1);
    let stored = db.mail_queue().message(held[0].message_id).await.unwrap();
    assert!(String::from_utf8_lossy(&stored.raw).contains("A post that waits for a moderator."));
}

#[tokio::test]
async fn the_database_applies_on_sqlite() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    scenario(&db).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_import3_db_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("import3db")
        .await
        .unwrap();
    let db = Database::connect(&schema.url, 2).await.unwrap();
    scenario(&db).await;
    db.pool().close().await;
    schema.drop().await.unwrap();
}
