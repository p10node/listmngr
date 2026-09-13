use super::*;

#[tokio::test]
async fn signing_lookup_outage_is_dependency_not_invalid_mail() {
    let directory =
        tempfile::tempdir_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .unwrap();
    let key = dkim_tests::key(directory.path());
    let role =
        MailRoleConfig::from_core(&dkim_tests::signing_config(&key, "example.invalid")).unwrap();
    let raw = b"From: author@example.invalid\r\nSubject: outage\r\n\r\nbody";
    let (db, lease, _, _) = tests::fixture_at(
        "sqlite::memory:",
        "{\"list_id\":\"test.example.invalid\"}",
        raw,
        vec!["member@example.invalid".into()],
    )
    .await;
    let (cooked, _) = prepare_delivery(&db, &lease, raw, "{\"list_id\":\"test.example.invalid\"}")
        .await
        .unwrap();
    db.pool().close().await;
    let error = sign_delivery(&db, &role, &lease, cooked).await.unwrap_err();
    assert_eq!(format!("{error:?}"), "Dependency");
}

#[tokio::test]
async fn signing_dependency_retries_but_invalid_mail_shunts() {
    use listmngr_db::mail_queue::JobState;
    for (error, expected) in [
        (PrepareError::Dependency, JobState::Ready),
        (PrepareError::Invalid, JobState::Shunted),
    ] {
        let (db, lease, role, sink) = tests::fixture_at(
            "sqlite::memory:",
            "{\"list_id\":\"test.example.invalid\"}",
            b"From: author@example.invalid\r\n\r\nbody",
            vec!["member@example.invalid".into()],
        )
        .await;
        let before = chrono::Utc::now().timestamp_millis();
        assert!(
            local_delivery_result::<Vec<u8>>(&db, &role, &lease, Err(error))
                .await
                .is_none()
        );
        let job = db.mail_queue().job(lease.job.id).await.unwrap();
        assert_eq!(job.state, expected);
        if expected == JobState::Ready {
            // Exponential backoff with ±20% jitter around the initial delay.
            assert!(job.run_after >= before + role.backoff.initial_ms * 4 / 5);
        }
        let recipients: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT email,status,attempt_token FROM delivery_recipients WHERE job_id=$1",
        )
        .bind(lease.job.id.0.to_string())
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(
            recipients,
            vec![("member@example.invalid".into(), "pending".into(), None)]
        );
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM bounce_events")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(events, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), sink.accept())
                .await
                .is_err()
        );
    }
}
