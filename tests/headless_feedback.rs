use std::{
    path::Path,
    process::{Command, Output},
};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dymus"));
    command
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("NO_COLOR", "1");
    command
}

fn run(root: &Path, args: &[&str]) -> Output {
    command(root).args(args).output().unwrap()
}

#[test]
fn idle_status_is_successful_but_controls_require_playback() {
    let root = tempfile::tempdir().unwrap();
    let status = run(root.path(), &["control", "status"]);
    assert!(status.status.success());
    assert!(status.stderr.is_empty());
    assert!(String::from_utf8_lossy(&status.stdout).contains("No headless player is running"));

    let next = run(root.path(), &["control", "next"]);
    assert!(!next.status.success());
    let text = String::from_utf8_lossy(&next.stderr);
    assert!(text.contains("No headless player is running"));
    assert!(text.contains("dymus play song"));
    assert!(!text.contains("os error"));
}

#[test]
fn empty_local_library_has_feedback_and_json_remains_machine_readable() {
    let root = tempfile::tempdir().unwrap();
    let media = root.path().join("media");
    std::fs::create_dir(&media).unwrap();
    let media = media.to_str().unwrap();
    let listing = run(root.path(), &["local", "--path", media]);
    assert!(listing.status.success());
    assert!(String::from_utf8_lossy(&listing.stdout).contains("No local media found"));

    let json = run(root.path(), &["local", "--path", media, "--json"]);
    assert!(json.status.success());
    let library: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(library["collections"], serde_json::json!([]));

    let playback = run(root.path(), &["play", "local", "--path", media]);
    assert!(!playback.status.success());
    let text = String::from_utf8_lossy(&playback.stderr);
    assert!(text.contains("No playable local media found"));
    assert!(text.contains("dymus local --json"));
}

#[test]
fn missing_local_path_keeps_the_reason_and_returns_failure() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let result = run(
        root.path(),
        &["play", "local", "--path", missing.to_str().unwrap()],
    );
    assert!(!result.status.success());
    let text = String::from_utf8_lossy(&result.stderr);
    assert!(text.contains("Read local library failed"));
    assert!(text.contains("Local path does not exist"));
    assert!(text.contains(missing.to_str().unwrap()));
}

#[test]
fn rejected_stream_reports_recovery_without_exposing_signed_urls() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let mpv = tools.join("mpv");
    std::fs::write(&mpv, "#!/bin/sh\nprintf '%s\\n' '[ffmpeg] https: HTTP error 403 Forbidden' 'Failed to open https://example.com/audio?token=secret' >&2\nexit 2\n").unwrap();
    std::fs::set_permissions(&mpv, std::fs::Permissions::from_mode(0o755)).unwrap();
    let media = root.path().join("audio.wav");
    std::fs::write(&media, b"placeholder media").unwrap();
    let output = command(root.path())
        .args(["play", "local", "--path", media.to_str().unwrap()])
        .env("PATH", &tools)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("YouTube rejected the stream"));
    assert!(text.contains("403 Forbidden"));
    assert!(text.contains("Update yt-dlp"));
    assert!(text.contains("[stream URL]"));
    assert!(!text.contains("token=secret"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Start playback failed"));
}
