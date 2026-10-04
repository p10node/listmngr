//! The master key: what it seals opens only under the same key, purpose
//! and owner; a site with a key stores new TOTP secrets sealed and still
//! reads the rows an earlier release left in the clear; `secrets encrypt`
//! seals those, `secrets rewrap` moves everything to a new key, and a
//! sealed row without a key is an error, never a silent pass — on `SQLite`
//! (a file, so two handles can hold two keys) and on `PostgreSQL`.
use listmngr_core::Error;
use listmngr_db::{
    AuditContext, Database, NewUser,
    keyring::{MasterKey, TOTP_PURPOSE, is_sealed},
    web_sessions::{LoginOutcome, WebSession},
};

const KEY_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const KEY_B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const PASSWORD: &str = "a very secure fixture password";

#[test]
fn a_sealed_value_opens_only_under_the_same_key_purpose_and_owner() {
    let key = MasterKey::from_hex(KEY_A).unwrap();
    let other = MasterKey::from_hex(KEY_B).unwrap();
    let sealed = key
        .seal(TOTP_PURPOSE, b"user-1", b"JBSWY3DPEHPK3PXP")
        .unwrap();
    assert!(is_sealed(&sealed));
    assert_ne!(
        sealed,
        key.seal(TOTP_PURPOSE, b"user-1", b"JBSWY3DPEHPK3PXP")
            .unwrap(),
        "a fresh nonce every time"
    );
    assert_eq!(
        key.open(TOTP_PURPOSE, b"user-1", &sealed)
            .unwrap()
            .as_slice(),
        b"JBSWY3DPEHPK3PXP"
    );
    assert!(matches!(
        other.open(TOTP_PURPOSE, b"user-1", &sealed),
        Err(Error::Database(_))
    ));
    assert!(matches!(
        key.open(TOTP_PURPOSE, b"user-2", &sealed),
        Err(Error::Database(_))
    ));
    assert!(matches!(
        key.open(b"listmngr/other/v1", b"user-1", &sealed),
        Err(Error::Database(_))
    ));
    let mut tampered = sealed;
    tampered.pop();
    tampered.push('A');
    assert!(key.open(TOTP_PURPOSE, b"user-1", &tampered).is_err());
    assert!(matches!(
        key.open(TOTP_PURPOSE, b"user-1", "JBSWY3DPEHPK3PXP"),
        Err(Error::Validation(_))
    ));
    for bad in ["", "abc", &KEY_A[..63], &format!("{}g", &KEY_A[..63])] {
        assert!(MasterKey::from_hex(bad).is_err(), "{bad:?}");
    }
    let minted = MasterKey::generate_hex().unwrap();
    assert_eq!(minted.len(), 64);
    assert!(MasterKey::from_hex(&minted).is_ok());
    assert!(!format!("{key:?}").contains(KEY_A));
}

async fn user_with_session(db: &Database, email: &str) -> WebSession {
    db.users()
        .create(NewUser {
            email: email.into(),
            display_name: "Member".into(),
            password: PASSWORD.into(),
            server_owner: false,
        })
        .await
        .unwrap();
    db.addresses().verify(email, true).await.unwrap();
    signed_in(db, email, 0).await
}

/// Log in `minutes` from now (the second step refuses a replayed time
/// step, so every sign-in happens later than the one before); when a
/// second factor is enrolled, complete it with the code of that moment.
async fn signed_in(db: &Database, email: &str, minutes: i64) -> WebSession {
    let now = chrono::Utc::now().timestamp_millis() + minutes * 60_000;
    let anon = db.create_web_session(None, None, now).await.unwrap();
    match db.browser_login(email, PASSWORD, &anon).await.unwrap() {
        LoginOutcome::Complete(session) => session,
        LoginOutcome::SecondFactor(pending) => {
            let code = current_code(db, email, now / 1000).await;
            db.browser_second_factor(&pending, &code, now)
                .await
                .unwrap()
        }
    }
}

/// The code the account's authenticator would show at `at_secs`, read
/// from the stored secret through whatever key the handle holds.
async fn current_code(db: &Database, email: &str, at_secs: i64) -> String {
    let (user_id, stored): (String, String) = sqlx::query_as(
        "SELECT t.user_id, t.secret FROM user_totp t JOIN users u ON u.id=t.user_id JOIN addresses a ON a.id=u.preferred_address_id WHERE a.email=$1",
    )
    .bind(email)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let base32 = if is_sealed(&stored) {
        let key = db.master_key().expect("a key to open the stored secret");
        String::from_utf8(
            key.open(TOTP_PURPOSE, user_id.as_bytes(), &stored)
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    } else {
        stored
    };
    let bytes = listmngr_db::totp::decode(&base32).unwrap();
    listmngr_db::totp::code(&bytes, at_secs / 30)
}

async fn stored_secret(db: &Database, email: &str) -> String {
    sqlx::query_scalar(
        "SELECT t.secret FROM user_totp t JOIN users u ON u.id=t.user_id JOIN addresses a ON a.id=u.preferred_address_id WHERE a.email=$1",
    )
    .bind(email)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

/// Enrol a second factor through the browser API and confirm it.
async fn enrol(db: &Database, session: &WebSession) {
    let now = chrono::Utc::now().timestamp_millis();
    let status = db.browser_totp_status(session, &[], now).await.unwrap();
    let (secret, _, _) = status.pending.expect("an enrolment in progress");
    let bytes = listmngr_db::totp::decode(&secret).unwrap();
    let code = listmngr_db::totp::code(&bytes, now / 1000 / 30);
    let codes = db.browser_totp_confirm(session, &code, now).await.unwrap();
    assert_eq!(codes.len(), listmngr_db::web_totp::RECOVERY_CODES);
}

async fn audit_count(db: &Database, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action=$1")
        .bind(action)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Step 1: with a key, a new enrolment is stored sealed and signs in;
/// a row left in the clear by an earlier release still signs in.
async fn sealed_and_legacy(keyed: &Database) {
    keyed
        .domains()
        .create("example.invalid", "", None)
        .await
        .unwrap();
    let alice = user_with_session(keyed, "alice@example.invalid").await;
    enrol(keyed, &alice).await;
    let stored = stored_secret(keyed, "alice@example.invalid").await;
    assert!(is_sealed(&stored), "{stored}");
    signed_in(keyed, "alice@example.invalid", 2).await;
    // Bob enrolled before the site had a key: the row is base32 in the clear.
    let bob = user_with_session(keyed, "bob@example.invalid").await;
    enrol_plain(keyed, &bob, "bob@example.invalid").await;
    assert!(!is_sealed(
        &stored_secret(keyed, "bob@example.invalid").await
    ));
    signed_in(keyed, "bob@example.invalid", 2).await;
    let state = keyed.secrets().status().await.unwrap();
    assert_eq!((state.totp_sealed, state.totp_plain), (1, 1));
}

/// Enrol as a site without a key would have stored it: the sealed row is
/// replaced by its base32 text.
async fn enrol_plain(keyed: &Database, session: &WebSession, email: &str) {
    enrol(keyed, session).await;
    let (user_id, stored): (String, String) = sqlx::query_as(
        "SELECT t.user_id, t.secret FROM user_totp t JOIN users u ON u.id=t.user_id JOIN addresses a ON a.id=u.preferred_address_id WHERE a.email=$1",
    )
    .bind(email)
    .fetch_one(keyed.pool())
    .await
    .unwrap();
    let key = keyed.master_key().unwrap();
    let base32 = String::from_utf8(
        key.open(TOTP_PURPOSE, user_id.as_bytes(), &stored)
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    sqlx::query("UPDATE user_totp SET secret=$1 WHERE user_id=$2")
        .bind(base32)
        .bind(user_id)
        .execute(keyed.pool())
        .await
        .unwrap();
}

/// Step 2: `encrypt` seals the plain row and audits; a second run is idle.
async fn encrypted(keyed: &Database) {
    assert_eq!(
        keyed
            .secrets()
            .encrypt(&AuditContext::system())
            .await
            .unwrap(),
        1
    );
    let state = keyed.secrets().status().await.unwrap();
    assert_eq!((state.totp_sealed, state.totp_plain), (2, 0));
    assert_eq!(audit_count(keyed, "security.encrypt_secrets").await, 1);
    signed_in(keyed, "bob@example.invalid", 4).await;
    assert_eq!(
        keyed
            .secrets()
            .encrypt(&AuditContext::system())
            .await
            .unwrap(),
        0
    );
}

/// Step 3: without a key a sealed row is an error at the second step; with
/// the wrong key too; `rewrap` under the new key moves both rows.
async fn rewrapped(keyed: &Database, bare: &Database, rekeyed: &Database) {
    let now = chrono::Utc::now().timestamp_millis() + 6 * 60_000;
    for (handle, what) in [(bare, "no key"), (rekeyed, "another key")] {
        let anon = handle.create_web_session(None, None, now).await.unwrap();
        let LoginOutcome::SecondFactor(pending) = handle
            .browser_login("alice@example.invalid", PASSWORD, &anon)
            .await
            .unwrap()
        else {
            panic!("alice has a second factor")
        };
        let code = current_code(keyed, "alice@example.invalid", now / 1000).await;
        assert!(
            handle
                .browser_second_factor(&pending, &code, now)
                .await
                .is_err(),
            "{what}: a sealed secret must not open"
        );
    }
    assert!(matches!(
        bare.secrets().encrypt(&AuditContext::system()).await,
        Err(Error::Validation(_))
    ));
    let previous = MasterKey::from_hex(KEY_A).unwrap();
    assert!(
        matches!(
            rekeyed
                .secrets()
                .rewrap(
                    &MasterKey::from_hex(KEY_B).unwrap(),
                    &AuditContext::system()
                )
                .await,
            Err(Error::Validation(_))
        ),
        "the wrong previous key opens nothing"
    );
    assert_eq!(
        rekeyed
            .secrets()
            .rewrap(&previous, &AuditContext::system())
            .await
            .unwrap(),
        2
    );
    assert_eq!(audit_count(rekeyed, "security.rewrap_secrets").await, 1);
    signed_in(rekeyed, "alice@example.invalid", 8).await;
    signed_in(rekeyed, "bob@example.invalid", 8).await;
    let state = rekeyed.secrets().status().await.unwrap();
    assert_eq!((state.totp_sealed, state.totp_plain), (2, 0));
}

async fn scenario(url: &str) {
    let keyed = Database::connect(url, 2)
        .await
        .unwrap()
        .with_master_key(Some(MasterKey::from_hex(KEY_A).unwrap()));
    keyed.migrate().await.unwrap();
    sealed_and_legacy(&keyed).await;
    encrypted(&keyed).await;
    let bare = Database::connect(url, 2).await.unwrap();
    let rekeyed = Database::connect(url, 2)
        .await
        .unwrap()
        .with_master_key(Some(MasterKey::from_hex(KEY_B).unwrap()));
    rewrapped(&keyed, &bare, &rekeyed).await;
    bare.pool().close().await;
    rekeyed.pool().close().await;
    keyed.pool().close().await;
}

#[tokio::test]
async fn sqlite_master_key_seals_totp_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("keyed.db").display()
    );
    scenario(&url).await;
}

#[tokio::test]
#[ignore = "requires TEST_POSTGRES_URL; owns an isolated schema"]
async fn postgres_master_key_contract() {
    let schema = listmngr_db::test_support::IsolatedSchema::create("master_key")
        .await
        .unwrap();
    scenario(&schema.url).await;
    schema.drop().await.unwrap();
}
