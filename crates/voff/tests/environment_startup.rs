use std::process::{Command, Output};

fn help(directory: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voff"))
        .arg("--help")
        .current_dir(directory)
        .env_clear()
        .output()
        .unwrap()
}

#[test]
fn optional_file_and_no_parent_discovery() {
    let parent = tempfile::tempdir().unwrap();
    std::fs::write(parent.path().join(".env"), "malformed fake-parent-secret").unwrap();
    let child = parent.path().join("child");
    std::fs::create_dir(&child).unwrap();
    assert!(help(&child).status.success());
}

#[test]
fn malformed_file_fails_without_leaking_source() {
    let directory = tempfile::tempdir().unwrap();
    let secret = "fake-malformed-secret";
    std::fs::write(
        directory.path().join(".env"),
        format!("OPENAI_API_KEY='{secret}"),
    )
    .unwrap();
    let output = help(directory.path());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not load .env"));
    assert!(!stderr.contains(secret));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
}

#[test]
fn unreadable_file_is_a_safe_startup_error() {
    let directory = tempfile::tempdir().unwrap();
    // A directory cannot be read as dotenv input, even under privileged CI.
    std::fs::create_dir(directory.path().join(".env")).unwrap();
    let output = help(directory.path());
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not load .env"));
}

#[test]
fn diagnostics_are_written_to_the_workspace_log_without_secrets_or_ansi() {
    let invocation = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join(".voff")).unwrap();
    std::fs::write(
        workspace.path().join(".voff/config.toml"),
        r#"
config_version = 1
[workspace]
root = "."
state_file = ".voff/voff.db"
[runtime]
default_timeout_seconds = 60
event_channel_capacity = 64
[provider]
kind = "mock"
[agent]
name = "Team Lead"
role = "team_lead"
system_prompt = "Test fixture"
tools = []
"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_voff"))
        .args(["--workspace", workspace.path().to_str().unwrap(), "doctor"])
        .current_dir(invocation.path())
        .env("OPENAI_API_KEY", "fake-secret-must-not-be-logged")
        .env("RUST_LOG", "trace")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(workspace.path().join(".voff/logs/voff.log")).unwrap();
    assert!(log.contains("application started"));
    assert!(!log.contains("fake-secret"));
    assert!(!log.contains('\u{1b}'));
    assert!(!invocation.path().join(".voff/logs/voff.log").exists());
}
