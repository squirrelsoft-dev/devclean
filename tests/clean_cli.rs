//! Integration tests for the `devclean clean` subcommand (issue #7).
//!
//! Each test creates a real workspace root on disk, configures it, and runs
//! the real `devclean` binary against it. The CLI hook is a non-destructive
//! preview: it must print classifications and never delete anything, even
//! with `--force`. We use an explicit `HOME`/`XDG_CONFIG_HOME` override so the
//! child never picks up the developer's real config or global
//! `~/.devcleanignore`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!(
        "devclean-clean-cli-{label}-{}-{n}",
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

/// A cleanable (status-5) project: committed + pushed, with untracked junk
/// that is not devcleanignored. `devclean clean --dry-run` prints a preview of
/// each untracked item's classification and deletes nothing.
#[test]
fn clean_dry_run_prints_classifications_without_deleting() {
    let root = root_for("preview");
    init_repo_with_commit(&root);

    // Untracked junk: a safe-list dir, a protected file, an ambiguous file.
    // `.devcleanignore` is committed BEFORE the push so the repo stays
    // pushed (cleanable); committing after the push would make it "ahead 1".
    std::fs::write(root.join(".devcleanignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".devcleanignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);

    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep me").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        out.contains("clean:") || out.contains("no projects found"),
        "expected clean header: {out}"
    );
    assert!(
        out.contains("safe-to-delete"),
        "expected safe-to-delete label: {out}"
    );
    assert!(out.contains("protected"), "expected protected label: {out}");
    assert!(out.contains("surfaced"), "expected surfaced label: {out}");

    // Non-destructive: every untracked item is still on disk.
    assert!(root.join("target").exists());
    assert!(root.join("important.dat").is_file());
    assert!(root.join("ambiguous.tmp").is_file());
}

/// `--force --dry-run` auto-approves each item for display and deletes nothing.
#[test]
fn clean_force_dry_run_auto_approves_each_item() {
    let root = root_for("force");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--force", "--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        out.contains("would delete"),
        "force dry-run should mark surfaced as would-delete: {out}"
    );
    assert!(
        root.join("ambiguous.tmp").is_file(),
        "force dry-run must not actually delete"
    );
}

/// `devclean clean --dry-run` skips non-cleanable projects (nothing
/// printed for them unless --verbose) and still deletes nothing.
#[test]
fn clean_dry_run_skips_non_cleanable_projects() {
    let root = root_for("skip");
    init_repo_with_commit(&root);
    // No remote → status 2 (no-remote), not cleanable.
    std::fs::write(root.join("junk.tmp"), "junk").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    // No cleanable project → each project report is printed, but each
    // per-project enumeration is not.
    // The file is untouched.
    assert!(
        root.join("junk.tmp").is_file(),
        "non-cleanable project must not be touched"
    );
    // Without --verbose, non-cleanable projects produce no per-project
    // enumeration lines.
    assert!(
        !out.contains("clean (dry-run):"),
        "non-cleanable project should be skipped silently: {out}"
    );
}
