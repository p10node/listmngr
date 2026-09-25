use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

#[tokio::test]
async fn fatal_role_task_is_reported_and_siblings_cancelled() {
    let mut tasks = tokio::task::JoinSet::new();
    let (owned_tx, owned_rx) = tokio::sync::oneshot::channel::<()>();
    tasks.spawn(async move {
        let _owned = owned_tx;
        std::future::pending::<()>().await;
        Ok(())
    });
    tasks.spawn(async { Err(std::io::Error::other("owned fatal fixture")) });
    let (_tx, rx) = watch::channel(false);
    let result = tokio::time::timeout(
        Duration::from_millis(250),
        supervise_tasks(tasks, rx, Duration::from_millis(30)),
    )
    .await
    .expect("fatal role remained healthy");
    assert!(result.is_err());
    assert!(owned_rx.await.is_err(), "sibling task detached");
}

#[tokio::test]
async fn shutdown_closes_active_stalled_lmtp_session() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    let mut role = MailRoleConfig::from_core(&Config::default()).unwrap();
    role.command_timeout = Duration::from_secs(60);
    role.session_drain_timeout = Duration::from_millis(30);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = watch::channel(false);
    let task = tokio::spawn(run_acceptor(
        listmngr_mail::lmtp::Protocol::Lmtp,
        listener,
        db,
        role,
        rx,
    ));
    let mut client = BufReader::new(tokio::net::TcpStream::connect(address).await.unwrap());
    let mut greeting = String::new();
    client.read_line(&mut greeting).await.unwrap();
    assert!(greeting.starts_with("220"));
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_millis(250), task)
        .await
        .expect("acceptor did not drain")
        .unwrap()
        .unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(250), client.read(&mut byte))
            .await
            .expect("detached LMTP session survived shutdown")
            .unwrap(),
        0
    );
}
