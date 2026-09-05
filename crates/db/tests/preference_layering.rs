use listmngr_core::{DeliveryMode, DeliveryStatus, MemberRole, Preferences, SubscriptionMode};
use listmngr_db::{Database, NewList, NewMember, NewUser};

const MODES: [DeliveryMode; 3] = [
    DeliveryMode::PlaintextDigests,
    DeliveryMode::MimeDigests,
    DeliveryMode::SummaryDigests,
];
const STATUSES: [DeliveryStatus; 3] = [
    DeliveryStatus::ByUser,
    DeliveryStatus::ByBounces,
    DeliveryStatus::ByModerator,
];

struct Graph {
    db: Database,
    user_id: listmngr_core::UserId,
    member_id: listmngr_core::MemberId,
}

async fn graph() -> Graph {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    db.domains()
        .create("layers.example", "", None)
        .await
        .unwrap();
    let list = db
        .lists()
        .create(NewList {
            list_id: "all.layers.example".parse().unwrap(),
            display_name: "Layers".into(),
            style: "legacy-default".into(),
        })
        .await
        .unwrap();
    let user = db
        .users()
        .create(NewUser {
            display_name: "Layer owner".into(),
            email: "layered@layers.example".into(),
            password: "Orbit!Cobalt7-River$Quartz".into(),
            server_owner: false,
        })
        .await
        .unwrap();
    let member = db
        .members()
        .create(NewMember {
            list_id: list.id,
            email: "layered@layers.example".into(),
            role: MemberRole::Member,
            subscription_mode: SubscriptionMode::AsUser,
            display_name: "Layered".into(),
        })
        .await
        .unwrap();
    assert_eq!(member.user_id, Some(user.id));
    Graph {
        db,
        user_id: user.id,
        member_id: member.id,
    }
}

fn layer_preferences(layer: usize, present: bool) -> Preferences {
    if !present {
        return Preferences::default();
    }
    Preferences {
        acknowledge_posts: Some(layer % 2 == 0),
        hide_address: Some(layer % 2 != 0),
        preferred_language: Some(format!("layer-{layer}")),
        receive_list_copy: Some(layer != 1),
        receive_own_postings: Some(layer == 1),
        delivery_mode: Some(MODES[layer]),
        delivery_status: Some(STATUSES[layer]),
    }
}

fn expected(system: &Preferences, layers: &[Preferences; 3]) -> Preferences {
    Preferences::resolve(std::iter::once(system).chain(layers.iter()))
}

#[tokio::test]
async fn sqlite_repository_exhaustively_resolves_nullable_user_address_member_layers() {
    let fixture = graph().await;
    let system = Preferences::system_defaults("system-language".into());

    for mask in 0_u8..8 {
        let layers: [Preferences; 3] =
            std::array::from_fn(|layer| layer_preferences(layer, mask & (1 << layer) != 0));
        fixture
            .db
            .preferences()
            .set_user(fixture.user_id, layers[0].clone())
            .await
            .unwrap();
        fixture
            .db
            .preferences()
            .set_address("layered@layers.example", layers[1].clone())
            .await
            .unwrap();
        fixture
            .db
            .preferences()
            .set_member(fixture.member_id, layers[2].clone())
            .await
            .unwrap();

        assert_eq!(
            fixture
                .db
                .preferences()
                .resolve_member(fixture.member_id, "system-language")
                .await
                .unwrap(),
            expected(&system, &layers),
            "incorrect system -> user -> address -> member resolution for mask {mask:03b}"
        );
        assert_eq!(
            fixture
                .db
                .preferences()
                .resolve_address("layered@layers.example", "system-language")
                .await
                .unwrap(),
            Preferences::resolve([&system, &layers[0], &layers[1]]),
            "incorrect address resolution for mask {mask:03b}"
        );
        assert_eq!(
            fixture
                .db
                .preferences()
                .resolve_user(fixture.user_id, "system-language")
                .await
                .unwrap(),
            Preferences::resolve([&system, &layers[0]]),
            "incorrect user resolution for mask {mask:03b}"
        );
    }
}
