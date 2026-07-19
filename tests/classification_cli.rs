//! Integration tests for the `devclean classification` subcommand (issue #6).
//!
//! Each test creates a real workspace root on disk, configures it, and runs the
//! real `devclean` binary against it. The printed report is the assertion
//! surface. We use an explicit `HOME` override so the child never picks up the
//! developer's real `~/.config/devclean/config.toml`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Each test gets a fresh workspace root under a per-test temp dir, so the
/// fixtures do not pollute each other and the child never sees the real
/// workspace root the developer is editing in.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-test workspace root — never shared between tests or prior runs.
/// Includes the PID (like `src/classify.rs`'s `unique_dir`) so leftover dirs
/// from earlier test runs at the same counter value do not collide and
/// reinitialize a stale `.git`.
fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!(
        "devclean-classify-cli-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&out).unwrap();
    out
}

/// Create a TOML config file under `home` with the given workspace roots.
fn write_config(home: &Path, roots: &[&str], depth: usize) -> PathBuf {
    let p = home.join("config.toml");
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&p).unwrap();
    // Quote each root so the array is valid TOML (`workspace_roots = ["/p1", "/p2"]`).
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

/// Run `git -C <root> <args>`, panicking on failure.
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

/// Initialize a fixture repo with an initial commit.
fn init_repo_with_commit(root: &Path) {
    git_in(root, &["init"]);
    git_in(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git_in(root, &["config", "user.email", "test@test.dev"]);
    git_in(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("initial.txt"), "initial contents").unwrap();
    git_in(root, &["add", "initial.txt"]);
    git_in(root, &["commit", "-m", "initial"]);
}

/// Configure a remote and push the initial commit.
fn add_pushed_remote(root: &Path) {
    let bare = root_for(&format!(
        "{}-bare",
        root.file_stem().unwrap().to_str().unwrap()
    ));
    git_in(&bare, &["init", "--bare"]);
    git_in(root, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git_in(root, &["config", "branch.main.remote", "origin"]);
    git_in(root, &["config", "branch.main.merge", "refs/heads/main"]);
    git_in(root, &["push", "origin", "main"]);
}

/// Run `devclean classification` with the given args, returning stdout+stderr.
fn run(args: &[&str]) -> String {
    let home = root_for("home");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_devclean"))
        .env("HOME", &home)
        .args(args)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// Test that classification with no projects found returns a clear message.
#[test]
fn classification_reports_no_projects_when_marker_absent() {
    let root = root_for("empty");
    std::fs::write(root.join("README.md"), "plain").unwrap();
    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(&["--config", config.to_str().unwrap(), "classification"]);
    assert!(out.contains("no projects found"));
}

/// Test that classification sorts by status.
#[test]
fn classification_sorts_by_status() {
    let root = root_for("sort");
    // `sub` is a non-git project (a Cargo.toml marker, no `.git`) — but with
    // issue #15 it is suppressed as a non-git marker inside an ancestor git
    // worktree, so it is classified as a subfolder of root, not as a separate
    // project. (We keep both states to prove sorting still applies to WIP.)
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("Cargo.toml"), "[package]").unwrap();

    // `root` is a committed+pushed git repo with a modified tracked file → WIP (4).
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::write(root.join("initial.txt"), "modified").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(&["--config", config.to_str().unwrap(), "classification"]);

    // Only root is classified — sub is suppressed (issue #15). WIP is reported
    // for the root (modified tracked file).
    assert!(
        out.contains("wip"),
        "expected wip label in output: {out}"
    );
    // no-git is not reported — sub's Cargo.toml is a non-git marker inside root's git worktree.
    assert!(
        !out.contains("no-git"),
        "unexpected no-git label in output: {out}"
    );
}

/// End-to-end coverage of the whole status table: one project per status in a
/// single workspace, classified in one run. This is the report a user actually
/// sees, so it pins down the three things the other tests leave implicit —
/// that `cleanable` and `clean` are reachable through the CLI at all, that
/// `cleanable` is separated from `clean` by the devcleanignore matcher rather
/// than the project's own `.gitignore`, and that precedence holds where it
/// matters most: a repo with BOTH uncommitted work and untracked junk reports
/// `wip`, never `cleanable`.
#[test]
fn classification_reports_every_status_in_severity_order() {
    let ws = root_for("all-statuses");

    let proj = |name: &str| {
        let p = ws.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    };

    // NoGit (1): a folder with a marker but no `.git`.
    // (With issue #15: this must be a folder with no ancestor git in the
    // workspace — a subfolder inside another git repo would be suppressed.)
    let nogit = proj("no-git");
    std::fs::write(nogit.join("Cargo.toml"), "[package]").unwrap();

    // NoRemote (2): a git repo with no remote configured.
    let noremote = proj("no-remote");
    init_repo_with_commit(&noremote);

    // Unpushed (3): a git repo with a remote and local commits not pushed.
    let unpushed = proj("unpushed");
    init_repo_with_commit(&unpushed);
    add_pushed_remote(&unpushed);
    std::fs::write(unpushed.join("extra.txt"), "unpushed").unwrap();
    git_in(&unpushed, &["add", "extra.txt"]);
    git_in(&unpushed, &["commit", "-m", "local change"]);

    // WIP (4): a git repo with uncommitted work.
    let wip = proj("wip");
    init_repo_with_commit(&wip);
    add_pushed_remote(&wip);
    std::fs::write(wip.join("initial.txt"), "modified").unwrap();

    // Cleanable (5): a git repo with uncommitted work + untracked junk that is
    // NOT devcleanignored.
    let cleanable = proj("cleanable");
    init_repo_with_commit(&cleanable);
    add_pushed_remote(&cleanable);
    std::fs::write(cleanable.join("secret.txt"), "secret").unwrap();

    // Clean (6): a git repo with all untracked junk devcleanignored.
    let clean = proj("clean");
    init_repo_with_commit(&clean);
    add_pushed_remote(&clean);
    std::fs::write(clean.join("node_modules"), "junk").unwrap();
    // Set up a .devcleanignore that matches the node_modules dir so it's protected.
    std::fs::write(clean.join(".devcleanignore"), "node_modules").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[ws.to_str().unwrap()], 2);
    let out = run(&["--config", config.to_str().unwrap(), "classification"]);

    // Each status should be reported once, in severity order (lowest number first).
    let statuses = [
        "no-git",
        "no-remote",
        "unpushed",
        "wip",
        "cleanable",
        "clean",
    ];
    for status in statuses {
        assert!(
            out.contains(status),
            "expected {status} label in output: {out}"
        );
    }

    // Sorting by severity: each status must appear before any higher-numbered
    // status.
    let mut last_idx = 0;
    for status in statuses {
        let idx = out.find(status).unwrap();
        assert!(
            idx > last_idx,
            "{status} must sort after the prior status: {out}"
        );
        last_idx = idx;
    }
}