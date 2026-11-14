//! Offline installer checks: no root, no unit activation, HOME-prefixed paths.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn script() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"))
}

fn current_uid() -> String {
    let output = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn dry_run_reports_plan_without_writing() {
    let home = tempfile::tempdir().expect("tempdir");
    let output = Command::new("sh")
        .arg(script())
        .arg("--dry-run")
        .env("HOME", home.path())
        .env_remove("XDG_STATE_HOME")
        .output()
        .expect("run installer");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!(
        "app={}/.local/share/sama-display",
        home.path().display()
    )));
    assert!(stdout.contains(&format!(
        "install={}/.local/share/sama-display/bin/sama-display",
        home.path().display()
    )));
    assert!(stdout.contains("service_activation=not performed"));
    assert!(stdout.contains("privilege_escalation=not performed"));
    assert_eq!(
        std::fs::read_dir(home.path()).expect("home").count(),
        0,
        "dry run must not write files"
    );
}

#[test]
fn installs_binary_unit_and_uid_rule_without_python() {
    let home = tempfile::tempdir().expect("tempdir");
    let binary = env!("CARGO_BIN_EXE_sama-display");
    let output = Command::new("sh")
        .arg(script())
        .args(["--binary", binary])
        .env("HOME", home.path())
        .output()
        .expect("run installer");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let app = home.path().join(".local/share/sama-display");
    let installed = app.join("bin/sama-display");
    let mode = std::fs::metadata(&installed)
        .expect("installed binary")
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "installed binary is executable");

    let unit = std::fs::read_to_string(
        home.path()
            .join(".config/systemd/user/sama-display.service"),
    )
    .expect("unit");
    assert!(unit.contains("ExecStart=%h/.local/share/sama-display/bin/sama-display"));
    assert!(unit.contains("WantedBy=default.target"));
    assert!(!unit.contains("venv"), "unit must not require Python");
    assert!(!unit.contains("sama_display.py"));

    let rule = std::fs::read_to_string(app.join("70-sama-display.rules")).expect("udev rule");
    assert!(!rule.contains("@UID@"));
    assert!(rule.contains(&format!("OWNER=\"{}\"", current_uid())));
    assert!(!app.join("venv").exists(), "no Python runtime is installed");

    let help = Command::new(&installed)
        .arg("--help")
        .output()
        .expect("installed --help");
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("--state-dir"));
    assert!(stdout.contains("--rotation"));
    assert!(stdout.contains("--frames"));
    assert!(stdout.contains("--once"));
}

#[test]
fn missing_binary_is_rejected_with_build_instructions() {
    let home = tempfile::tempdir().expect("tempdir");
    let output = Command::new("sh")
        .arg(script())
        .args(["--binary", "/nonexistent/sama-display"])
        .env("HOME", home.path())
        .output()
        .expect("run installer");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cargo build --release"));
    assert!(!home.path().join(".local/share/sama-display").exists());
}
