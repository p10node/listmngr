use listmngr_mail::FsMessageStore;

#[test]
fn rejects_keys_before_access_and_detects_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("messages");
    let store = FsMessageStore::open(&root).unwrap();
    std::fs::write(temp.path().join("outside"), b"secret").unwrap();
    for key in [
        "../outside",
        "",
        ".",
        "/not-a-key",
        "a/b",
        "\\outside",
        &"A".repeat(64),
        &"g".repeat(64),
    ] {
        assert!(store.get(key).is_err(), "accepted {key}");
    }
    let key = store.put(b"original").unwrap();
    assert_eq!(store.put(b"original").unwrap(), key);
    std::fs::write(root.join(&key), b"corrupt").unwrap();
    assert!(store.get(&key).is_err());
    assert!(store.put(b"original").is_err());
    assert_eq!(std::fs::read(root.join(key)).unwrap(), b"corrupt");
}

#[test]
#[allow(clippy::needless_collect)] // Spawn every worker before joining: barrier requires all 12.
fn concurrent_publication_is_complete_and_idempotent() {
    use std::sync::{Arc, Barrier};
    let temp = tempfile::tempdir().unwrap();
    let raw = Arc::new(vec![0xa5; 2_000_000]);
    let barrier = Arc::new(Barrier::new(12));
    let handles: Vec<_> = (0..12)
        .map(|_| {
            let store = FsMessageStore::open(temp.path().join("messages")).unwrap();
            let raw = raw.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let key = store.put(&raw).unwrap();
                assert_eq!(store.get(&key).unwrap(), *raw);
                key
            })
        })
        .collect();
    let keys: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(keys.iter().all(|key| key == &keys[0]));
    assert_eq!(
        std::fs::read_dir(temp.path().join("messages"))
            .unwrap()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn private_permissions_and_symlink_rejection() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("messages");
    let store = FsMessageStore::open(&root).unwrap();
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let key = store.put(b"original").unwrap();
    assert_eq!(
        std::fs::metadata(root.join(&key))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let outside = temp.path().join("outside");
    std::fs::write(&outside, b"original").unwrap();
    std::fs::remove_file(root.join(&key)).unwrap();
    symlink(&outside, root.join(&key)).unwrap();
    assert!(store.get(&key).is_err());
    assert!(store.put(b"original").is_err());
    symlink(&root, temp.path().join("alias")).unwrap();
    assert!(FsMessageStore::open(temp.path().join("alias")).is_err());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(FsMessageStore::open(&root).is_err());
}

#[test]
fn binary_content_addressing_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("messages");
    let raw = b"Message-ID: <a@b>\r\n\r\n\0\xff\r\n.\r\n";
    let store = FsMessageStore::open(&root).unwrap();
    let key = store.put(raw).unwrap();
    assert_eq!(key.len(), 64);
    assert_eq!(store.get(&key).unwrap(), raw);
    assert_ne!(store.put(b"different").unwrap(), key);
    drop(store);
    assert_eq!(FsMessageStore::open(root).unwrap().get(&key).unwrap(), raw);
}
