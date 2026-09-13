//! Mailman's `/queues` on both prefixes: every runner queue with its
//! waiting jobs, one queue, one job, and injection into `in`.
use super::*;

const MESSAGE: &str = "From: Poster <poster@example.net>\nTo: dev@example.com\nSubject: injected\nMessage-ID: <injected@example.net>\n\nhello\n";

#[tokio::test]
async fn queues_list_every_queue_and_injection_lands_in_the_in_queue() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let listed = response_json(call(&app, "GET", "/3.1/queues", Some(&token), None).await).await;
    let names: Vec<&str> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "in", "pipeline", "out", "retry", "bounces", "command", "virgin", "archive", "digest",
            "nntp", "shunt", "bad"
        ]
    );
    assert_eq!(listed["total_size"], 12);
    assert_eq!(listed["entries"][0]["files"], serde_json::json!([]));
    assert_eq!(listed["entries"][0]["self_link"], "/3.1/queues/in");
    assert_eq!(listed["entries"][0]["directory"], "queue_jobs/in");

    // mailmanclient's `Queue.inject(list_id, text)` posts a form.
    let form =
        serde_urlencoded::to_string([("list_id", "dev.example.com"), ("text", MESSAGE)]).unwrap();
    let created = call_form(&app, "POST", "/3.1/queues/in", &token, &form).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let location = created.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let id = location.rsplit('/').next().unwrap().to_owned();
    assert_eq!(location, format!("/3.1/queues/in/{id}"));
    let body = response_json(created).await;
    assert_eq!(body["id"], id);
    assert_eq!(body["queue"], "in");

    let queue = response_json(call(&app, "GET", "/3.1/queues/in", Some(&token), None).await).await;
    assert_eq!(queue["files"], serde_json::json!([id]));
    assert_eq!(queue["count"], 1);
    assert_eq!(queue["http_etag"], "phase1");
    let job = response_json(call(&app, "GET", &location, Some(&token), None).await).await;
    assert_eq!(job["state"], "ready");
    assert_eq!(job["queue"], "in");
    assert_eq!(job["attempts"], 0);
    assert_eq!(job["max_attempts"], 5);
    assert_eq!(job["self_link"], location);
    assert!(job.get("raw").is_none() && job.get("text").is_none());
    // The job belongs to `in`, not to another queue; unknown queues are 404.
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/3.1/queues/out/{id}"),
            Some(&token),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    for uri in [
        "/3.1/queues/nope",
        "/3.1/queues/in/not-a-job",
        "/3.1/queues/in/00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(
            call(&app, "GET", uri, Some(&token), None).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    // The typed prefix has the same queues under its own links.
    let native =
        response_json(call(&app, "GET", "/api/v1/queues/in", Some(&token), None).await).await;
    assert_eq!(native["self_link"], "/api/v1/queues/in");
    assert_eq!(native["files"], serde_json::json!([id]));
    assert!(native.get("http_etag").is_none());
}

#[tokio::test]
async fn injection_validates_the_queue_list_and_message() {
    let (app, token, _) = setup(&["admin"]).await;
    create_configurable_list(&app, &token).await;
    let inject =
        |list: &str, text: &str| serde_json::json!({"list_id": list, "text": text}).to_string();
    for (uri, body, status) in [
        (
            "/api/v1/queues/out",
            inject("dev.example.com", MESSAGE),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/queues/nope",
            inject("dev.example.com", MESSAGE),
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/v1/queues/in",
            inject("missing.example.com", MESSAGE),
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/v1/queues/in",
            inject(
                "dev.example.com",
                "Subject: no from\nMessage-ID: <x@y>\n\nbody\n",
            ),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/queues/in",
            inject(
                "dev.example.com",
                "From: poster@example.net\nSubject: no id\n\nbody\n",
            ),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/queues/in",
            inject(
                "dev.example.com",
                "From: not a mailbox\nMessage-ID: <x@y>\n\nbody\n",
            ),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/queues/in",
            serde_json::json!({"list_id": "dev.example.com"}).to_string(),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            call(&app, "POST", uri, Some(&token), Some(&body))
                .await
                .status(),
            status,
            "{uri} {body}"
        );
    }
    // Mailman's fqdn spelling of the list works on the compatibility prefix.
    let body = inject("dev@example.com", MESSAGE);
    assert_eq!(
        call(&app, "POST", "/3.1/queues/in", Some(&token), Some(&body))
            .await
            .status(),
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn queues_need_an_unbound_reader_and_injection_the_list_writer() {
    let (app, token, _) = scoped_app().await;
    for prefix in ["/api/v1", "/3.1"] {
        for uri in [
            "/queues",
            "/queues/in",
            "/queues/in/00000000-0000-0000-0000-000000000000",
        ] {
            assert_eq!(
                call(&app, "GET", &format!("{prefix}{uri}"), Some(&token), None)
                    .await
                    .status(),
                StatusCode::FORBIDDEN,
                "{prefix}{uri}"
            );
        }
        // The bound writer may inject into its own list, not the other.
        let own = serde_json::json!({"list_id": "one.first.example", "text": MESSAGE}).to_string();
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("{prefix}/queues/in"),
                Some(&token),
                Some(&own)
            )
            .await
            .status(),
            StatusCode::CREATED
        );
        let other =
            serde_json::json!({"list_id": "two.second.example", "text": MESSAGE}).to_string();
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("{prefix}/queues/in"),
                Some(&token),
                Some(&other)
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    let (app, token, _) = setup(&["system:read"]).await;
    assert_eq!(
        call(&app, "GET", "/api/v1/queues", Some(&token), None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/queues", None, None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}
