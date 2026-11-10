//! A token bound to a list is list-owner authority: it ends with the role.
use listmngr_core::{Member, MemberRole, Preferences, SubscriptionMode, User};
use listmngr_db::{AuditContext, Database, MemberOptions, NewList, NewMember, NewUser};

const PASSWORD: &str = "Orbit!Cobalt7-River$Quartz";

struct Fixture {
    db: Database,
    list: listmngr_core::MailingList,
    domain: listmngr_core::Domain,
}

async fn fixture() -> Fixture {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let domain = db.domains().create("example.com", "", None).await.unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "test.example.com".parse().unwrap(),
            display_name: "Test".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    Fixture { db, list, domain }
}

impl Fixture {
    async fn owner(&self, email: &str) -> (User, Member, String) {
        let user = self
            .db
            .users()
            .create_verified_with_context(
                NewUser {
                    display_name: email.into(),
                    email: email.into(),
                    password: PASSWORD.into(),
                    server_owner: false,
                },
                &AuditContext::system(),
            )
            .await
            .unwrap();
        let member = self
            .db
            .members()
            .create(NewMember {
                list_id: self.list.id.clone(),
                email: email.into(),
                role: MemberRole::Owner,
                subscription_mode: SubscriptionMode::AsAddress,
                display_name: email.into(),
            })
            .await
            .unwrap();
        let token = self
            .db
            .tokens()
            .create_scoped(
                user.id,
                "bound",
                &["lists:read", "lists:write", "members:read"],
                Some(&self.list.id),
                Some(self.domain.id),
                None,
            )
            .await
            .unwrap()
            .token;
        (user, member, token)
    }

    async fn valid(&self, token: &str) -> bool {
        self.db.tokens().authenticate(token).await.is_ok()
    }
}

#[tokio::test]
async fn list_bound_tokens_end_when_the_minter_stops_owning_the_list() {
    let f = fixture().await;
    let (_, stays, stays_token) = f.owner("stays@example.com").await;
    let (_, demoted, demoted_token) = f.owner("demoted@example.com").await;
    let (_, removed, removed_token) = f.owner("removed@example.com").await;
    let (_, rerolled, rerolled_token) = f.owner("rerolled@example.com").await;
    let (actor, _, _) = f.owner("actor@example.com").await;
    for token in [
        &stays_token,
        &demoted_token,
        &removed_token,
        &rerolled_token,
    ] {
        assert!(f.valid(token).await);
    }

    // REST/repository role change.
    f.db.members()
        .update(demoted.id, &serde_json::json!({"role": "moderator"}))
        .await
        .unwrap();
    assert!(!f.valid(&demoted_token).await);

    // Removal (REST DELETE, leave, unsubscribe all end in the same delete).
    f.db.members().delete(removed.id).await.unwrap();
    assert!(!f.valid(&removed_token).await);

    // Browser owner re-roles another owner.
    let session =
        f.db.create_web_session(Some(actor.id), None, chrono::Utc::now().timestamp_millis())
            .await
            .unwrap();
    f.db.browser_member_options(
        &session,
        &f.list.id,
        rerolled.id,
        &MemberOptions {
            moderation_action: None,
            display_name: "rerolled".into(),
            role: MemberRole::Member,
            preferences: Preferences::default(),
        },
    )
    .await
    .unwrap();
    assert!(!f.valid(&rerolled_token).await);

    // Owners who stay owners keep their tokens.
    assert!(f.valid(&stays_token).await);
    let _ = stays;
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action='token.revoke'")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(audited, 3);
}

#[tokio::test]
async fn a_server_owner_keeps_a_list_bound_token_after_leaving_the_owner_role() {
    let f = fixture().await;
    let (user, member, token) = f.owner("both@example.com").await;
    sqlx::query("UPDATE users SET is_server_owner=1 WHERE id=$1")
        .bind(user.id.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    f.db.members().delete(member.id).await.unwrap();
    assert!(f.valid(&token).await);
}
