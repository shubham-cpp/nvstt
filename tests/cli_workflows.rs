use std::{path::Path, process::Command};

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nvstt"))
        .args(args)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
        .unwrap()
}

#[test]
fn personal_corrections_work_without_a_daemon_or_history_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for args in [
        vec!["vocabulary", "add", "para keet", "Parakeet"],
        vec!["vocabulary", "add", "nv stt", "nvstt"],
    ] {
        let output = run(root, &args);
        assert!(output.status.success(), "{:?}", output);
    }
    let output = run(root, &["vocabulary", "list", "--json"]);
    assert!(output.status.success());
    let rules: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rules["para keet"], "Parakeet");
    let output = run(
        root,
        &[
            "vocabulary",
            "test",
            "Use para keet, with twenty one files in nv stt.",
        ],
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Use Parakeet, with 21 files in nvstt.\n"
    );

    let path = root.join("config/nvstt/vocabulary.json");
    let before = std::fs::read(&path).unwrap();
    assert!(
        !run(root, &["vocabulary", "add", "", "invalid"])
            .status
            .success()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        run(root, &["vocabulary", "remove", "PARA KEET"])
            .status
            .success()
    );
    let output = run(root, &["vocabulary", "test", "para keet"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "para keet\n");
    assert!(!root.join("state").exists());
    assert!(!root.join("runtime").exists());
}

#[test]
fn recovery_reports_empty_storage_without_loading_a_model() {
    let directory = tempfile::tempdir().unwrap();
    let output = run(directory.path(), &["recover"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("no complete recording")
    );
    assert!(!directory.path().join("state").exists());
}
