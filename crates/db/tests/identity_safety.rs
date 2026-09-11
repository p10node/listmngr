use listmngr_core::{MemberRole, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember, NewUser};

#[test]
fn legacy_admin_scope_does_not_override_resource_bounds() {
    let domain = listmngr_core::DomainId::new();
    let other_domain = listmngr_core::DomainId::new();
    let inside: listmngr_core::ListId = "inside.example.com".parse().unwrap();
    let outside = "outside.example.com".parse().unwrap();
    let mut auth = listmngr_db::TokenAuth {
        id: listmngr_core::TokenId::new(),
        user_id: listmngr_core::UserId::new(),
        scopes: std::iter::once("admin".into()).collect(),
        list_id: Some(inside.clone()),
        domain_id: Some(domain),
    };
    assert!(auth.allows_list(&inside, domain));
    assert!(!auth.allows_list(&outside, domain));
    assert!(!auth.allows_domain(other_domain));
    auth.list_id = None;
    auth.domain_id = None;
    assert!(auth.allows_list(&outside, other_domain));
}

async fn fixture() -> (Database, listmngr_core::User, listmngr_core::User) {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut users = Vec::new();
    for email in ["a@example.com", "b@example.com"] {
        users.push(
            db.users()
                .create(NewUser {
                    display_name: email.into(),
                    email: email.into(),
                    password: "Orbit!Cobalt7-River$Quartz".into(),
                    server_owner: false,
                })
                .await
                .unwrap(),
        );
    }
    db.domains().create("example.com", "", None).await.unwrap();
    for (name, mode) in [
        ("address", SubscriptionMode::AsAddress),
        ("user", SubscriptionMode::AsUser),
    ] {
        let list = db
            .lists()
            .create(NewList {
                list_id: format!("{name}.example.com").parse().unwrap(),
                display_name: name.into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        db.members()
            .create(NewMember {
                list_id: list.id,
                email: "a@example.com".into(),
                role: MemberRole::Member,
                subscription_mode: mode,
                display_name: "A".into(),
            })
            .await
            .unwrap();
    }
    (db, users.remove(0), users.remove(0))
}

#[tokio::test]
async fn relink_updates_live_ownership_without_rewriting_history() {
    let (db, a, b) = fixture().await;
    let history = db.audit().list().await.unwrap();
    let original = db.members().find("a@example.com").await.unwrap();
    db.addresses()
        .link("a@example.com", Some(b.id))
        .await
        .unwrap();
    let members = db.members().find("a@example.com").await.unwrap();
    for member in &members {
        assert_eq!(member.user_id, Some(b.id));
        let before = original.iter().find(|m| m.id == member.id).unwrap();
        assert_eq!(member.subscription_mode, before.subscription_mode);
        assert_eq!(member.address_id, before.address_id);
    }
    assert_eq!(
        db.users().get(a.id).await.unwrap().preferred_address_id,
        None
    );
    assert_eq!(
        db.users().get(b.id).await.unwrap().preferred_address_id,
        b.preferred_address_id
    );
    let after = db.audit().list().await.unwrap();
    assert_eq!(after.len(), history.len() + 1);
    assert_eq!(
        serde_json::to_value(&after[..history.len()]).unwrap(),
        serde_json::to_value(history).unwrap()
    );
}

#[tokio::test]
async fn bounded_admin_issuance_is_rejected_without_writes() {
    let (db, a, _) = fixture().await;
    let list = "address.example.com".parse().unwrap();
    let domain = db.domains().get("example.com").await.unwrap();
    let before = db.audit().list().await.unwrap().len();
    for (list, domain) in [(Some(&list), None), (None, Some(domain.id))] {
        assert!(matches!(
            db.tokens()
                .create_scoped(a.id, "ambiguous", &["admin"], list, domain, None)
                .await,
            Err(listmngr_core::Error::Validation(_))
        ));
    }
    assert_eq!(db.audit().list().await.unwrap().len(), before);
    assert!(
        db.tokens()
            .create(a.id, "explicit global admin", &["admin"], None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn unlink_clears_live_identity_and_preference_with_atomic_rollback() {
    let (db, a, b) = fixture().await;
    let before = db.audit().list().await.unwrap().len();
    for sabotage in [
        "CREATE TRIGGER sabotage BEFORE UPDATE ON members BEGIN SELECT RAISE(ABORT, 'member sabotage'); END",
        "CREATE TRIGGER sabotage BEFORE UPDATE ON users BEGIN SELECT RAISE(ABORT, 'preference sabotage'); END",
        "CREATE TRIGGER sabotage BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'audit sabotage'); END",
    ] {
        sqlx::query(sabotage).execute(db.pool()).await.unwrap();
        for owner in [Some(b.id), None] {
            assert!(db.addresses().link("a@example.com", owner).await.is_err());
            assert_eq!(
                db.addresses().get("a@example.com").await.unwrap().user_id,
                Some(a.id)
            );
            assert_eq!(
                db.users().get(a.id).await.unwrap().preferred_address_id,
                a.preferred_address_id
            );
            for member in db.members().find("a@example.com").await.unwrap() {
                assert_eq!(member.user_id, Some(a.id));
            }
            assert_eq!(db.audit().list().await.unwrap().len(), before);
        }
        sqlx::query("DROP TRIGGER sabotage")
            .execute(db.pool())
            .await
            .unwrap();
    }
    db.addresses().link("a@example.com", None).await.unwrap();
    assert_eq!(
        db.users().get(a.id).await.unwrap().preferred_address_id,
        None
    );
    for member in db.members().find("a@example.com").await.unwrap() {
        assert_eq!(member.user_id, None);
    }
    assert_eq!(db.audit().list().await.unwrap().len(), before + 1);
}
