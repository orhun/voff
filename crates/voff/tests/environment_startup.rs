use std::process::Command;

#[test]
fn first_launch_requests_interactive_setup_and_writes_a_safe_log() {
    let workspace = tempfile::tempdir().unwrap();
    let secret = "fake-secret-must-not-be-logged";
    std::fs::write(
        workspace.path().join(".env"),
        format!("OPENAI_API_KEY='{secret}"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_voff"))
        .current_dir(workspace.path())
        .env("OPENAI_API_KEY", secret)
        .env("RUST_LOG", "trace")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("team setup needs an interactive terminal"));
    assert!(!stderr.contains(secret));
    let log = std::fs::read_to_string(workspace.path().join(".voff/logs/voff.log")).unwrap();
    assert!(log.contains("application started"));
    assert!(!log.contains(secret));
    assert!(!log.contains('\u{1b}'));
}

#[test]
fn positional_workspace_owns_its_database_and_log() {
    let launch_directory = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_voff"))
        .arg(workspace.path())
        .current_dir(launch_directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("team setup needs"));
    assert!(workspace.path().join(".voff/voff.db").exists());
    assert!(workspace.path().join(".voff/logs/voff.log").exists());
    assert!(!launch_directory.path().join(".voff").exists());
}

#[test]
fn extra_arguments_are_rejected_without_opening_a_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_voff"))
        .arg(workspace.path())
        .arg("extra")
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage:") && stderr.contains("voff"));
    assert!(!workspace.path().join(".voff").exists());
}
