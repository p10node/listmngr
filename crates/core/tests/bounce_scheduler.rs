use listmngr_core::Config;

#[test]
fn bounce_scheduler_validates_disabled_bounds_and_requires_mail_role() {
    for (enabled, interval, batch, valid) in [
        (false, 0, 100, false),
        (false, 86401, 100, false),
        (false, 60, 0, false),
        (false, 60, 1001, false),
        (false, 1, 1, true),
        (false, 86400, 1000, true),
        (true, 60, 100, false),
    ] {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), format!("[mta]\nbounce_maintenance_enabled={enabled}\nbounce_maintenance_interval_secs={interval}\nbounce_maintenance_batch_size={batch}\n")).unwrap();
        assert_eq!(
            Config::load(Some(file.path())).is_ok(),
            valid,
            "{enabled}/{interval}/{batch}"
        );
    }
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "[mta]\nenabled=true\nsmtp_tls='plaintext_trusted_relay'\nbounce_maintenance_enabled=true\n").unwrap();
    assert!(
        Config::load(Some(file.path()))
            .unwrap()
            .mta
            .bounce_maintenance_enabled
    );
}

#[test]
fn bounce_scheduler_defaults_are_explicitly_off() {
    let value = serde_json::to_value(Config::default()).unwrap();
    assert_eq!(value["mta"]["bounce_maintenance_enabled"], false);
    assert_eq!(value["mta"]["bounce_maintenance_interval_secs"], 60);
    assert_eq!(value["mta"]["bounce_maintenance_batch_size"], 100);
    let legacy: Config = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap()["mta"], value["mta"]);
}
