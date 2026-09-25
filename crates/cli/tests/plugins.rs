//! `listmngr plugins` on the real binary: one JSON line per plugin this
//! build carries — none by default, the example under `plugin-example`.
use assert_cmd::Command;
use serde_json::Value;

#[test]
fn the_plugins_of_this_build_are_printed() {
    let output = Command::cargo_bin("listmngr")
        .unwrap()
        .env_clear()
        .arg("plugins")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    if cfg!(feature = "plugin-example") {
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["name"], "example");
        assert_eq!(lines[0]["rules"], serde_json::json!(["keyword-hold"]));
        assert_eq!(lines[0]["archivers"], serde_json::json!(["example"]));
    } else {
        assert!(lines.is_empty(), "{lines:?}");
    }
}
