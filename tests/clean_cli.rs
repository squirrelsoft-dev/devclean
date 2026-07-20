//! Integration tests for the `offcut clean` subcommand (issues #7/#8).
//!
//! Each test creates a real workspace root on disk, configures it, and runs
//! the real `offcut` binary against it. `--dry-run` must never delete
//! anything (even combined with `--force`); a real run — interactive
//! approvals piped over stdin, or `--force` — must delete exactly the safe
//! and approved surfaced items while protected and unapproved items
//! survive. We use an explicit `HOME`/`XDG_CONFIG_HOME` override so the
//! child never picks up the developer's real config or global
//! `~/.offcutignore`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn root_for(label: &str) -> PathBuf {
    let d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let out = d.join(format!(
        "offcut-clean-cli-{label}-{}-{n}",
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
    run_with_stdin(args, "")
}

/// Run the real binary with `input` piped to stdin — the interactive flow
/// reads its approval answers from there. Returns stdout.
fn run_with_stdin<I, S>(args: I, input: &str) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = env!("CARGO_BIN_EXE_offcut");
    let mut child = std::process::Command::new(exe)
        .args(args)
        .env("HOME", "/nonexistent-home")
        .env("XDG_CONFIG_HOME", "/nonexistent-xdg")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A cleanable (status-5) project: committed + pushed, with untracked junk
/// that is not offcutignored. `offcut clean --dry-run` prints a preview of
/// each untracked item's classification and deletes nothing.
#[test]
fn clean_dry_run_prints_classifications_without_deleting() {
    let root = root_for("preview");
    init_repo_with_commit(&root);

    // Untracked junk: a safe-list dir, a protected file, an ambiguous file.
    // `.offcutignore` is committed BEFORE the push so the repo stays
    // pushed (cleanable); committing after the push would make it "ahead 1".
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
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

/// `--force --dry-run` previews the force run: each surfaced item is
/// auto-approved for display (`would delete`) and nothing is deleted.
#[test]
fn clean_force_dry_run_auto_approves_each_item() {
    let root = root_for("force");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run([
        "--force",
        "--dry-run",
        "--config",
        config.to_str().unwrap(),
        "clean",
    ]);

    assert!(
        out.contains("would delete"),
        "force preview should mark surfaced as would-delete: {out}"
    );
    assert!(
        !out.contains("would prompt"),
        "force preview auto-approves — nothing left to prompt about: {out}"
    );
    assert!(
        root.join("ambiguous.tmp").is_file(),
        "dry-run must not actually delete: {out}"
    );
}

/// A real interactive run, approvals piped over stdin: the safe item and
/// the approved surfaced item are deleted; the protected item, the
/// declined surfaced item, and tracked files survive.
#[test]
fn clean_interactive_deletes_only_approved_items() {
    let root = root_for("interactive");
    init_repo_with_commit(&root);
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);

    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep me").unwrap();
    std::fs::write(root.join("approve.tmp"), "surfaced, approved").unwrap();
    std::fs::write(root.join("keep.tmp"), "surfaced, declined").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    // Prompts in order: all-cleanup, then the surfaced items in enumeration
    // (sorted) order — approve.tmp ("y"), keep.tmp ("n") — then the
    // per-project confirmation.
    let out = run_with_stdin(
        ["--config", config.to_str().unwrap(), "clean"],
        "y\ny\nn\ny\n",
    );

    assert!(
        !root.join("target").exists(),
        "safe item must be deleted: {out}"
    );
    assert!(
        !root.join("approve.tmp").exists(),
        "approved surfaced item must be deleted: {out}"
    );
    assert!(
        root.join("keep.tmp").is_file(),
        "declined surfaced item must survive: {out}"
    );
    assert!(
        root.join("important.dat").is_file(),
        "protected item must survive: {out}"
    );
    assert!(
        root.join("initial.txt").is_file(),
        "tracked file must survive: {out}"
    );
    assert!(
        out.contains("(deleting)"),
        "real run should report deleting, not would-delete: {out}"
    );
    assert!(
        out.contains("deleted"),
        "expected a post-execution confirmation line: {out}"
    );
}

/// `--force` (no --dry-run) deletes safe and surfaced items without any
/// prompting; protected and tracked files survive.
#[test]
fn clean_force_deletes_without_prompting() {
    let root = root_for("force-real");
    init_repo_with_commit(&root);
    std::fs::write(root.join(".offcutignore"), "important.dat\n").unwrap();
    git_in(&root, &["add", ".offcutignore"]);
    git_in(&root, &["commit", "-m", "ignore rules"]);
    add_pushed_remote(&root);

    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), "safe junk").unwrap();
    std::fs::write(root.join("important.dat"), "keep me").unwrap();
    std::fs::write(root.join("ambiguous.tmp"), "surfaced").unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--force", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        !root.join("target").exists(),
        "force must delete safe items: {out}"
    );
    assert!(
        !root.join("ambiguous.tmp").exists(),
        "force must delete surfaced items: {out}"
    );
    assert!(
        root.join("important.dat").is_file(),
        "protected item must survive force: {out}"
    );
    assert!(
        root.join("initial.txt").is_file(),
        "tracked file must survive force: {out}"
    );
    assert!(
        out.contains("deleted"),
        "expected a post-execution confirmation line: {out}"
    );
}

/// `offcut clean --dry-run` skips non-cleanable projects (nothing
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

/// Issue #18: the dry-run report row for a cleanable project carries its
/// reclaimable size, and the summary line carries the aggregate.
#[test]
fn clean_dry_run_shows_per_project_and_aggregate_sizes() {
    let root = root_for("size-dryrun");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    // 4 KB of safe-list junk — big enough for a KB-unit size.
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), vec![0u8; 4096]).unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--dry-run", "--config", config.to_str().unwrap(), "clean"]);

    assert!(
        out.contains("cleanable (~"),
        "cleanable row should carry a size: {out}"
    );
    assert!(
        out.contains("reclaimable"),
        "summary should carry the aggregate reclaimable size: {out}"
    );
}

/// Issue #18: `offcut list` shows the same per-project size on each
/// cleanable row plus the aggregate in the summary line.
#[test]
fn list_shows_per_project_and_aggregate_sizes() {
    let root = root_for("size-list");
    init_repo_with_commit(&root);
    add_pushed_remote(&root);
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("target/bin"), vec![0u8; 4096]).unwrap();

    let home = root_for("home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let config = write_config(&home.join(".config"), &[root.to_str().unwrap()], 2);
    let out = run(["--config", config.to_str().unwrap(), "list"]);

    assert!(
        out.contains("reclaimable"),
        "list summary should carry the aggregate reclaimable size: {out}"
    );
    assert!(
        out.contains("cleanable (~"),
        "list cleanable row should carry a size: {out}"
    );
}
