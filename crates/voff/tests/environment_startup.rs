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
