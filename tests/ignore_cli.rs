//! Integration tests for the `devclean ignore` debug subcommand, which is the
//! observable hook for the `.devcleanignore` matcher (issue #3). The matcher
//! module itself is unit-tested in `src/ignore.rs`; these tests exercise the
//! full CLI path: loading real `.devcleanignore` files from a temp project tree
//! and reporting per-path results. They use an explicit `HOME` override so the
//! global `~/.devcleanignore` is never read from the real home directory.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn devclean() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devclean"))
}

fn unique_dir(label: &str) -> PathBuf {
    let mut d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    d.push(format!(
        "devclean-ignore-cli-{}-{}-{}",
        label,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_file(dir: &Path, name: &str, contents: &str) {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

/// Run `devclean ignore <path>` inside `cwd`, returning the trimmed stdout.
/// HOME is pointed at an empty temp dir so the child never reads the real
/// `~/.devcleanignore`.
fn run_in(cwd: &Path, path: &str) -> (bool, String) {
    let home = unique_dir("home");
    let out = devclean()
        .current_dir(cwd)
        .env("HOME", &home)
        .arg("ignore")
        .arg(path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&home);
    (out.status.success(), format!("{stdout}\n{stderr}"))
}

#[test]
fn ignore_subcommand_reports_ignored_and_not_ignored() {
    let root = unique_dir("basic");
    write_file(&root, ".devcleanignore", "*.log\n");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    write_file(&sub, ".devcleanignore", "!keep.log\n");

    let (ok, out) = run_in(&root, "debug.log");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "debug.log: ignored");

    let (ok, out) = run_in(&root, "sub/keep.log");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "sub/keep.log: not-ignored");

    let (ok, out) = run_in(&root, "README.md");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "README.md: not-ignored");
}

#[test]
fn ignore_subcommand_nested_precedence_overrides_global_anchoring() {
    let root = unique_dir("anchor");
    // Root-level anchored pattern ignores only top-level `build`.
    write_file(&root, ".devcleanignore", "/build\n");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // Nested file re-includes nothing but tests anchoring: /build in sub
    // matches sub/build, not sub/deep/build.
    write_file(&sub, ".devcleanignore", "/local\n");

    let (ok, out) = run_in(&root, "build");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "build: ignored");

    // Not top-level, so root `/build` does not match.
    let (ok, out) = run_in(&root, "sub/build");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "sub/build: not-ignored");

    // Nested anchored pattern matches within sub.
    let (ok, out) = run_in(&root, "sub/local");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "sub/local: ignored");
}

#[test]
fn ignore_subcommand_with_no_ignore_files_reports_not_ignored() {
    let root = unique_dir("none");
    let (ok, out) = run_in(&root, "anything.txt");
    assert!(ok, "{out}");
    assert_eq!(out.trim_end_matches('\n'), "anything.txt: not-ignored");
}
