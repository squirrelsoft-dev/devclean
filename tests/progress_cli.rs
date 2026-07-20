//! Integration tests for the live progress indicator across classification
//! and cleaning phases (issue #30).
//!
//! Each test creates a real workspace root on disk, configures it, and runs
//! the real `devclean` binary against it. The printed output is the assertion
//! surface. We use an explicit `HOME` override so the child never picks up
//! the developer's real `~/.config/devclean/config.toml`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!(
        "devclean-progress-cli-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&out).unwrap();
    out
}

fn write_config(home: &Path, roots: &[&str], depth: usize) -> PathBuf {
    let p = home.join("config.toml");
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&p).unwrap();
    let quoted = roots
        .iter()
        .map(|r| format!("{:?}", r))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(f, "workspace_roots = [{quoted}]").unwrap();
    writeln!(f, "max_depth = {}", depth).unwrap();
    writeln!(
        f,
        "project_markers = [\".git\", \"package.json\", \"Cargo.toml\", \"go.mod\"]"
    )
    .unwrap();
    p
}

fn git_in(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git -C {} {:?} failed",
        root.display(),
        args
    );
}

fn init_repo_with_commit(root: &Path) {
    git_in(root, &["init"]);
    git_in(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git_in(root, &["config", "user.email", "test@test.dev"]);
    git_in(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("initial.txt"), "initial").unwrap();
    git_in(root, &["add", "initial.txt"]);
    git_in(root, &["commit", "-m", "initial"]);
}

fn add_pushed_remote(root: &Path) -> PathBuf {
    let bare = root_for(&format!(
        "{}-bare",
        root.file_stem().unwrap().to_string_lossy()
    ));
    git_in(&bare, &["init", "--bare"]);
    git_in(root, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git_in(root, &["config", "branch.main.remote", "origin"]);
    git_in(root, &["config", "branch.main.merge", "refs/heads/main"]);
    git_in(root, &["push", "origin", "main"]);
    bare
}

/// Run the devclean binary with the given args, returning stdout.
fn run<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = env!("CARGO_BIN_EXE_devclean");
    let child = std::process::Command::new(exe)
        .args(args)
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .output()
        .unwrap();
    String::from_utf8_lossy(&child.stdout).into_owned()
}

/// Test that classification emits nothing when stdout is piped (not a TTY).
/// The progress lines are absent; output is byte-identical to today for
/// piped/redirected runs.
#[test]
fn classification_emits_no_progress_when_piped() {
    let ws = root_for("classify-pipe");

    let p1 = ws.join("p1");
    std::fs::create_dir_all(&p1).unwrap();
    init_repo_with_commit(&p1);

    let p2 = ws.join("p2");
    std::fs::create_dir_all(&p2).unwrap();
    init_repo_with_commit(&p2);

    let p3 = ws.join("p3");
    std::fs::create_dir_all(&p3).unwrap();
    init_repo_with_commit(&p3);

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[ws.to_str().unwrap()], 2);

    // Run classification with stdout piped (not a TTY). The progress lines
    // must be absent; the output is the plain report only.
    let out = run(["--config", config.to_str().unwrap(), "classification"]);

    // No classifying lines — the TTY gate short-circuits each update.
    assert!(
        !out.contains("classifying"),
        "piped output must not contain classifying lines: {out}"
    );
    // The report still appears (the summary line).
    assert!(
        out.contains("classification"),
        "piped output must still contain the report: {out}"
    );
}

/// Test that `list` over a cleanable project emits no sizing progress when
/// piped, while the per-project size and aggregate still render.
#[test]
fn list_emits_no_sizing_progress_when_piped() {
    let ws = root_for("size-pipe");

    let p1 = ws.join("p1");
    std::fs::create_dir_all(&p1).unwrap();
    init_repo_with_commit(&p1);
    add_pushed_remote(&p1);
    std::fs::create_dir_all(p1.join("target")).unwrap();
    std::fs::write(p1.join("target/bin"), vec![0u8; 4096]).unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[ws.to_str().unwrap()], 2);

    let out = run(["--config", config.to_str().unwrap(), "list"]);

    // No sizing lines — the TTY gate short-circuits each update.
    assert!(
        !out.contains("sizing"),
        "piped output must not contain sizing lines: {out}"
    );
    // The sized listing still appears.
    assert!(
        out.contains("cleanable (~"),
        "piped output must still carry the per-project size: {out}"
    );
    assert!(
        out.contains("reclaimable"),
        "piped output must still carry the aggregate size: {out}"
    );
}

/// Test that the default run (no subcommand) emits no progress when piped.
#[test]
fn default_run_emits_no_progress_when_piped() {
    let ws = root_for("default-pipe");

    let p1 = ws.join("p1");
    std::fs::create_dir_all(&p1).unwrap();
    init_repo_with_commit(&p1);

    let p2 = ws.join("p2");
    std::fs::create_dir_all(&p2).unwrap();
    init_repo_with_commit(&p2);

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[ws.to_str().unwrap()], 2);

    let out = run(["--config", config.to_str().unwrap()]);

    // No classifying lines — the TTY gate short-circuits each update.
    assert!(
        !out.contains("classifying"),
        "piped output must not contain classifying lines: {out}"
    );
}
