//! MTA lookup maps for Postfix (regexp or hash) and Exim (lsearch), published
//! as immutable generations behind a `current` symlink, with the permission
//! and retention policy the operator configured.
use listmngr_core::{ListId, MtaConfig};
use listmngr_mail::mta::{LmtpTarget, MapWriter, Mta, PostfixMapType, Readers};
use std::path::Path;

fn lists() -> Vec<ListId> {
    [
        "alpha.example.invalid",
        "alpha-join.example.invalid",
        "beta.other.invalid",
    ]
    .iter()
    .map(|id| id.parse().unwrap())
    .collect()
}

fn writer(dir: &Path, mta: Mta) -> MapWriter {
    MapWriter {
        mta,
        directory: dir.to_path_buf(),
        lmtp_target: LmtpTarget::parse("127.0.0.1:8024").unwrap(),
        map_type: PostfixMapType::Regexp,
        postmap_command: "/nonexistent/postmap".into(),
        verp_delimiter: "+".into(),
        readers: Readers::Group,
        keep: 5,
    }
}

fn read(generation: &Path, name: &str) -> String {
    std::fs::read_to_string(generation.join(name)).unwrap()
}

#[test]
fn postfix_regexp_maps_cover_every_exact_address_and_verp_bounces() {
    let dir = tempfile::tempdir().unwrap();
    let writer = writer(dir.path(), Mta::Postfix);
    let files = writer.render(&lists());
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["domains.regexp", "recipients.regexp", "transport.regexp"]
    );
    let recipients = &files[1].1;
    // Sorted bytewise: `alpha-bounces@` precedes `alpha@`.
    assert!(recipients.starts_with("/^alpha-bounces@example\\.invalid$/ OK\n"));
    assert!(recipients.contains("/^alpha@example\\.invalid$/ OK\n"));
    assert!(recipients.contains("/^alpha-join-owner@example\\.invalid$/ OK\n"));
    assert!(recipients.contains("/^beta-request@other\\.invalid$/ OK\n"));
    assert!(recipients.ends_with("/^beta-bounces\\+[^@=]+=[^@=]+@other\\.invalid$/ OK\n"));
    assert_eq!(
        files[0].1,
        "/^example\\.invalid$/ OK\n/^other\\.invalid$/ OK\n"
    );
    assert!(
        files[2]
            .1
            .lines()
            .all(|line| line.ends_with(" lmtp:[127.0.0.1]:8024"))
    );
    // `alpha-join@` is both a list and `alpha`'s join command: one row.
    assert_eq!(files[2].1.lines().count(), 3 * 9 - 1 + 3);
}

#[test]
fn postfix_hash_maps_use_mailman_file_names_and_a_named_lmtp_host() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = writer(dir.path(), Mta::Postfix);
    writer.map_type = PostfixMapType::Hash;
    writer.lmtp_target = LmtpTarget::parse("listmngr:8024").unwrap();
    let files = writer.render(&lists());
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["postfix_domains", "postfix_lmtp"]);
    assert_eq!(
        files[0].1,
        "example.invalid example.invalid\nother.invalid other.invalid\n"
    );
    assert!(
        files[1]
            .1
            .contains("alpha@example.invalid lmtp:[listmngr]:8024\n")
    );
    assert!(
        files[1]
            .1
            .contains("alpha-bounces@example.invalid lmtp:[listmngr]:8024\n")
    );
    assert!(
        !files[1].1.contains('+'),
        "hash maps leave VERP to recipient_delimiter: {}",
        files[1].1
    );
    assert_eq!(files[1].1.lines().count(), 3 * 9 - 1);
}

#[test]
fn exim_maps_are_lsearch_files_of_domains_and_exact_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let files = writer(dir.path(), Mta::Exim).render(&lists());
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["exim_domains", "exim_recipients"]);
    assert_eq!(files[0].1, "example.invalid\nother.invalid\n");
    assert!(files[1].1.contains("alpha@example.invalid\n"));
    assert!(files[1].1.contains("beta-bounces@other.invalid\n"));
    assert_eq!(files[1].1.lines().count(), 3 * 9 - 1);
}

#[test]
fn lmtp_targets_are_hosts_or_literals_with_a_port() {
    assert_eq!(
        LmtpTarget::parse("[::1]:9124").unwrap().postfix_transport(),
        "lmtp:[::1]:9124"
    );
    assert_eq!(
        LmtpTarget::parse("listmngr:8024")
            .unwrap()
            .postfix_transport(),
        "lmtp:[listmngr]:8024"
    );
    assert_eq!(
        LmtpTarget::parse("mail.example.invalid:24")
            .unwrap()
            .postfix_transport(),
        "lmtp:[mail.example.invalid]:24"
    );
    for bad in [
        "0.0.0.0:8024",
        "[::]:8024",
        "127.0.0.1:0",
        "[ff02::1]:8024",
        "224.0.0.1:8024",
        "[fe80::1%3]:8024",
        "127.0.0.1",
        "host name:8024",
        "listmngr:99999",
        "-bad.host:8024",
        "",
    ] {
        assert!(LmtpTarget::parse(bad).is_err(), "{bad}");
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn publish_writes_a_generation_switches_current_and_prunes_old_ones() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mta");
    let mut writer = writer(&root, Mta::Postfix);
    writer.keep = 2;
    let first = writer.publish(&lists()).unwrap();
    assert!(first.starts_with(root.canonicalize().unwrap()));
    assert!(
        first
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("generation-")
    );
    let current = root.join("current");
    assert_eq!(
        std::fs::read_link(&current).unwrap(),
        Path::new(first.file_name().unwrap()),
        "the symlink is relative so another mount of the directory resolves it"
    );
    assert_eq!(
        read(&current, "recipients.regexp"),
        read(&first, "recipients.regexp")
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&first), 0o750);
        assert_eq!(mode(&first.join("recipients.regexp")), 0o640);
    }

    let second = writer.publish(&lists()).unwrap();
    let third = writer.publish(&lists()[..1]).unwrap();
    assert_eq!(
        std::fs::read_link(&current).unwrap(),
        Path::new(third.file_name().unwrap())
    );
    assert!(!first.exists(), "beyond `keep` generations are pruned");
    assert!(second.exists());
    assert!(third.exists());
    assert!(!read(&current, "recipients.regexp").contains("beta"));
    let mut entries: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort();
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert!(entries.iter().all(|name| !name.starts_with(".staging")));

    #[cfg(unix)]
    {
        writer.readers = Readers::World;
        let world = writer.publish(&lists()).unwrap();
        assert_eq!(mode(&world), 0o755);
        assert_eq!(mode(&world.join("domains.regexp")), 0o644);
        writer.readers = Readers::Owner;
        let owner = writer.publish(&lists()).unwrap();
        assert_eq!(mode(&owner), 0o700);
        assert_eq!(mode(&owner.join("domains.regexp")), 0o600);
    }
}

#[cfg(unix)]
#[test]
fn hash_maps_are_compiled_with_the_configured_postmap_and_its_failure_publishes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let postmap = dir.path().join("postmap");
    // A stand-in that records its arguments and produces the `.db` files.
    std::fs::write(
        &postmap,
        "#!/bin/sh\nfor f in \"$@\"; do echo \"$f\" >> \"$(dirname \"$f\")/../postmap.log\"; : > \"$f.db\"; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&postmap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = dir.path().join("mta");
    let mut writer = writer(&root, Mta::Postfix);
    writer.map_type = PostfixMapType::Hash;
    writer.postmap_command = postmap.clone();
    let generation = writer.publish(&lists()).unwrap();
    assert!(generation.join("postfix_lmtp.db").exists());
    assert!(generation.join("postfix_domains.db").exists());
    let log = std::fs::read_to_string(root.join("postmap.log")).unwrap();
    assert!(log.contains("postfix_lmtp"), "{log}");
    assert!(log.contains("postfix_domains"), "{log}");
    std::fs::remove_file(root.join("postmap.log")).unwrap();

    std::fs::write(&postmap, "#!/bin/sh\nexit 3\n").unwrap();
    let error = writer.publish(&lists()).unwrap_err();
    assert!(error.to_string().contains("postmap"), "{error}");
    assert_eq!(
        std::fs::read_link(root.join("current")).unwrap(),
        Path::new(generation.file_name().unwrap()),
        "a failed compile leaves the published generation selected"
    );
    let generations = std::fs::read_dir(&root)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("generation-")
        })
        .count();
    assert_eq!(generations, 1);
}

#[test]
fn the_writer_follows_the_mta_configuration() {
    let mut config = MtaConfig::default();
    assert!(MapWriter::from_config(&config).unwrap().is_none());
    config.incoming = "postfix".into();
    config.map_directory = "/var/lib/listmngr/mta".into();
    let writer = MapWriter::from_config(&config).unwrap().unwrap();
    assert_eq!(writer.mta, Mta::Postfix);
    assert_eq!(writer.directory, Path::new("/var/lib/listmngr/mta"));
    assert_eq!(
        writer.lmtp_target.postfix_transport(),
        "lmtp:[127.0.0.1]:8024"
    );
    assert_eq!(writer.map_type, PostfixMapType::Regexp);
    assert_eq!(writer.readers, Readers::Group);
    assert_eq!(writer.keep, 5);

    config.lmtp_listen = "0.0.0.0:8024".into();
    assert!(
        MapWriter::from_config(&config).is_err(),
        "a wildcard listener needs an explicit map target"
    );
    config.lmtp_map_target = Some("listmngr:8024".into());
    config.transport_file_type = "hash".into();
    config.map_permissions = "world".into();
    config.incoming = "exim".into();
    let writer = MapWriter::from_config(&config).unwrap().unwrap();
    assert_eq!(writer.mta, Mta::Exim);
    assert_eq!(
        writer.lmtp_target.postfix_transport(),
        "lmtp:[listmngr]:8024"
    );
    assert_eq!(writer.map_type, PostfixMapType::Hash);
    assert_eq!(writer.readers, Readers::World);

    for (field, value) in [
        ("incoming", "sendmail"),
        ("transport_file_type", "btree"),
        ("map_permissions", "everyone"),
    ] {
        let mut broken = config.clone();
        match field {
            "incoming" => broken.incoming = value.into(),
            "transport_file_type" => broken.transport_file_type = value.into(),
            _ => broken.map_permissions = value.into(),
        }
        assert!(broken.validate().is_err(), "{field}={value}");
    }
    config.map_generations_kept = 0;
    assert!(config.validate().is_err());
}
