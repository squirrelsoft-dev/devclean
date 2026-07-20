//! Project classification for offcut.
//!
//! Determines each discovered project's dirty status by inspecting git state
//! via git plumbing commands. The ignore matcher (issue #3) is consulted for
//! status 5 / Clean decisions: untracked junk that is offcutignored does not
//! push a repo into Cleanable.
//!
//! ## Git backend choice
//!
//! This module shells out to `git` via `std::process::Command`. No `git2`
//! dependency is pulled in — every other git inspection in this crate uses
//! the `git` CLI, and this is no different. Reasons:
//!
//! - The `ignore` crate already wraps gitignore semantics; we follow the same
//!   pattern for git plumbing.
//! - `git` plumbing is the source of truth: if `git status --porcelain` says a
//!   path is untracked, so are we. libgit2 semantics can diverge on edge cases
//!   (detached HEAD, worktree subtrees) while the CLI does not.
//! - No dependency adds surface for this narrow scope.
//!
//! All commands run `git -C <project>` so they operate against the project's
//! working tree regardless of the process's current directory.

use std::path::Path;
use std::process::Command;

use crate::ignore::IgnoreSet;

/// The five dirty statuses, ordered by severity (most-needs-attention first).
///
/// Status 5 is the *cleanable* state: everything committed+pushed, with
/// untracked junk that is not offcutignored. Statuses 1..4 are all states
/// where cleaning must wait — the repo still has outstanding git state.
///
/// Precedence: a repo matching multiple conditions is reported by its
/// lowest-numbered (most-severe) status. A repo with uncommitted work (4)
/// that also has untracked junk is NOT 5; it must not be cleaned while
/// it has uncommitted work.
///
/// The special "Clean" branch signals a committed+pushed repo with **no**
/// untracked non-ignored junk. It is not dirty — report it as clean/ok, not
/// as a dirty status. (Status 5 only applies when such untracked junk exists.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Project has a marker but no `.git` directory — it looks like a project
    /// but is not git-initialized.
    NoGit,
    /// Git repo with no remote configured — never pushes anything.
    NoRemote,
    /// Git repo with a remote, but has commits not yet pushed. Either the
    /// tracked branch has unpushed local commits, or the branch has no
    /// upstream tracking branch at all.
    Unpushed,
    /// Git repo with uncommitted work-in-progress — modified or staged tracked
    /// changes sitting in the working tree or index.
    Wip,
    /// Git repo that is committed + pushed, and has untracked junk that is
    /// **not** offcutignored. Eligible for cleaning.
    Cleanable,
    /// All committed+pushed with **no** untracked non-ignored junk. Clean.
    Clean,
}

impl Status {
    /// Human-readable label for display (plain text — colors are owned by
    /// issue #9, not this module). Labels are sorted by severity so the
    /// label that sorts "first" is the most-needs-attention label.
    pub fn label(&self) -> &'static str {
        match self {
            Status::NoGit => "no-git",
            Status::NoRemote => "no-remote",
            Status::Unpushed => "unpushed",
            Status::Wip => "wip",
            Status::Cleanable => "cleanable",
            Status::Clean => "clean",
        }
    }

    /// Numeric rank for sort (1..5, most-severe first; Clean is last).
    pub fn rank(&self) -> u8 {
        match self {
            Status::NoGit => 1,
            Status::NoRemote => 2,
            Status::Unpushed => 3,
            Status::Wip => 4,
            Status::Cleanable => 5,
            Status::Clean => 6,
        }
    }
}

/// Classify a discovered project.
///
/// Walks precedence in order 1 → 5. Each branch returns the first matching
/// dirty status; if none match, returns `Clean`.
///
/// - `project_path` is the absolute path of the project to classify.
/// - `ignore_set` is the project's loaded `.offcutignore` rules — consulted
///   only for untracked-junk decisions (status 5 vs Clean). The matcher's
///   contract is the source of truth: `is_ignored == true` means the path is
///   protected, so an untracked file that is `is_ignored` does not push the
///   repo into Cleanable.
///
/// Returns the first matching status by severity; a repo matching multiple
/// conditions is reported by its most-severe (lowest-numbered) status.
pub fn classify(project_path: &Path, ignore_set: &IgnoreSet) -> Status {
    // Precedence in order: 1, 2, 3, 4, 5. Stop at the first match.
    if status_no_git(project_path) {
        return Status::NoGit;
    }
    if status_no_remote(project_path) {
        return Status::NoRemote;
    }
    if status_unpushed(project_path) {
        return Status::Unpushed;
    }
    if status_wip(project_path) {
        return Status::Wip;
    }
    if status_cleanable(project_path, ignore_set) {
        return Status::Cleanable;
    }
    Status::Clean
}

/// Status 1: project has no `.git` directory (or a file whose gitdir target
/// does not exist).
///
/// Checks for the literal `.git` path under the project root. If it is absent,
/// the project is not git-initialized and we stop here — every subsequent
/// git-state check is moot. If `.git` is a gitdir pointer pointing at a path
/// that does not exist on this filesystem, also treat as NoGit — that is a
/// broken / dangling worktree, not a real git project, so every subsequent
/// git-state check would fail loudly. We short-circuit here with NoGit so
/// callers do not emit a noisy warning about missing remotes for a repo that
/// no longer exists on this box.
fn status_no_git(project_path: &Path) -> bool {
    let git_path = project_path.join(".git");
    if !git_path.exists() {
        return true;
    }
    // A `.git` file whose `gitdir:` target does not exist locally is a
    // dangling worktree — not a real git project on this box. Treat it as
    // NoGit rather than letting each subsequent git-state check blow up and
    // emit a warning about missing remotes for a repo that is half-stale.
    if git_path.is_file() {
        return git_cmd(project_path, &["rev-parse", "--git-dir"]).is_err();
    }
    false
}

/// Status 2: git repo with no remote configured.
///
/// Runs `git remote -v`. An empty output means no remotes are configured. A
/// failed exit code means git could not read the repo at all — the project is
/// not a git repo (already caught by status 1, but callers can invoke this
/// branch defensively), git is missing from `PATH`, or the repo is unreadable.
/// That case still reports `no-remote`, which is fail-safe because `no-remote`
/// is never cleanable, but it warns on stderr so the operator can tell a
/// genuinely remote-less repo apart from a git that never ran.
fn status_no_remote(project_path: &Path) -> bool {
    let output = git_cmd(project_path, &["remote", "-v"]);
    match output {
        Ok(out) => out.trim().is_empty(),
        Err(e) => {
            eprintln!(
                "warning: {}: could not read remotes, reporting as no-remote: {e}",
                project_path.display()
            );
            true
        }
    }
}

/// Status 3: git repo with a remote, but has commits not pushed.
///
/// Two sub-conditions each qualify:
///
/// - The tracked branch has no upstream and no commits at all → we treat that
///   as unpushed (nothing has been pushed anywhere).
/// - The tracked branch has an upstream and `rev-list @{upstream}..HEAD`
///   returns commits → unpushed local changes.
/// - The tracked branch has **no** upstream but has local commits → unpushed
///   (no upstream to push against, so nothing is pushed).
///
/// We shell out to `git` plumbing, which is the authoritative answer on what
/// is pushed and what is not.
fn status_unpushed(project_path: &Path) -> bool {
    // Check whether the project is a git repo at all; if not, nothing about
    // git state matters (covered by status 1 already, but callers may not
    // have short-circuited yet).
    if !is_git_repo(project_path) {
        return false;
    }

    // Try to find the upstream tracking branch. If `@{u}` fails, there is no
    // upstream; that alone qualifies as unpushed when local commits exist. A
    // detached HEAD has no upstream, so it falls out of this branch as
    // unpushed without needing a separate check.
    let upstream = git_cmd(project_path, &["rev-parse", "@{u}"]);
    let upstream_exists = upstream.is_ok();

    // Check if there are local commits. `rev-parse HEAD` failing means no
    // commits yet; that is unpushed (nothing ever pushed).
    let has_commits = git_cmd(project_path, &["rev-parse", "HEAD"]).is_ok();

    if !has_commits {
        return true; // no commits = unpushed
    }

    if upstream_exists {
        // There is an upstream; check if there are unpushed commits on it.
        let unpushed = git_cmd(project_path, &["rev-list", "@{u}..HEAD", "--"]);
        // `rev-list` returns 0 even when the range is empty; we check stdout.
        match unpushed {
            Ok(out) => !out.trim().is_empty(),
            Err(_) => true, // error reading = unpushed (nothing pushed)
        }
    } else {
        // No upstream at all, but has local commits: that is unpushed.
        true
    }
}

/// Status 4: git repo with uncommitted work-in-progress.
///
/// `git status --porcelain` enumerates every change:
/// - Lines starting with ` M`, ` M`, `MM`, `R`, etc. (two columns of status
///   codes) indicate tracked files that have been modified in the working
///   tree or staged in the index.
/// - Lines starting with `??` are untracked.
/// - Lines starting with ` D`, `A`, etc. are also tracked changes.
///
/// We scan for any non-`??` line: that means a tracked file has changes
/// (modified or staged). If only `??` lines appear, it is **not** status 4 —
/// only untracked.
fn status_wip(project_path: &Path) -> bool {
    let output = git_cmd(project_path, &["status", "--porcelain"]);
    match output {
        Ok(out) => {
            for line in out.lines() {
                // Porcelain status columns are always two characters, like
                // ` M`, ` M`, `MM`, `??`, ` D`, `A `, etc. Untracked lines
                // are `??` only; everything else is a tracked-file change.
                let bytes = line.as_bytes();
                if bytes.len() >= 2 {
                    let first = bytes[0];
                    let second = bytes[1];
                    if first != b'?' || second != b'?' {
                        return true; // tracked file has changes
                    }
                }
            }
            false // only untracked lines present — not status 4
        }
        Err(_) => false, // not a git repo (covered by status 1)
    }
}

/// Status 5 vs Clean: git repo that is committed+pushed, with untracked junk
/// that is either offcutignored or absent entirely.
///
/// - Enumerate all untracked files, **including** gitignored ones. This is
///   deliberate: junk like `node_modules`/`target/` is gitignored by the
///   project, but offcut exists to clean exactly that, so it must be
///   visible here. We use `git ls-files --others` (NO `--exclude-standard`,
///   which would hide gitignored junk) so the offcutignore matcher is the
///   sole judge of what is protected.
/// - For each untracked path, run it through the ignore set.
/// - If ANY untracked path is **not** offcutignored → Cleanable.
/// - If ALL untracked paths are offcutignored (or there are none) → Clean.
///
/// Three `ls-files` flags are load-bearing and must stay:
///
/// - `-z` emits NUL-separated raw paths. Without it git quotes and C-escapes
///   any path with non-ASCII or special characters (`café.txt` arrives as
///   `"caf\303\251.txt"`), which matches no offcutignore pattern, and a path
///   containing a newline splits into several bogus entries. Either one
///   silently reports a protected path as cleanable.
/// - `--directory` collapses a wholly-untracked directory to one entry
///   (`node_modules/`) instead of recursing into every file beneath it. The
///   common `Clean` case — everything offcutignored — is exactly the worst
///   case for the recursive form, since no entry short-circuits the loop.
/// - `--no-empty-directory` keeps `--directory` from newly surfacing empty
///   untracked directories, which the recursive form never reported at all.
///
/// `--directory` is a cost optimization **only** where it does not change the
/// granularity ignore patterns are evaluated at. A collapsed `logs/` entry
/// hides the files beneath it, so a content-matching pattern like `*.js` would
/// be tested against `logs` alone, never match, and report a directory of
/// wholly-protected files as cleanable. So the collapsed entry is only trusted
/// when the directory itself is ignored — the common `node_modules/` case,
/// where nothing beneath it can change the verdict. A directory that is *not*
/// itself ignored is re-listed at file granularity and each file is matched
/// individually, which is the pattern granularity users write against.
///
/// The trailing `/` that `--directory` puts on directory entries tells us the
/// entry's kind, so we call `is_ignored_path` directly and skip the stat that
/// `is_ignored` would do per entry.
///
/// Precedence: this branch is only reached after status 1..4 all fail, so the
/// repo is already committed+pushed and has no uncommitted tracked changes.
fn status_cleanable(project_path: &Path, ignore_set: &IgnoreSet) -> bool {
    let output = git_cmd(
        project_path,
        &[
            "ls-files",
            "--others",
            "--directory",
            "--no-empty-directory",
            "-z",
        ],
    );
    let untracked = match output {
        Ok(out) => out,
        Err(_) => return false, // not a git repo (covered by status 1)
    };

    // Each entry is a path relative to the project root, directories carrying
    // a trailing `/`. Test it against the ignore set (not the project
    // .gitignore).
    for entry in untracked.split('\0').filter(|e| !e.is_empty()) {
        match entry.strip_suffix('/') {
            Some(dir) => {
                if ignore_set.is_ignored_path(Path::new(dir), true) {
                    continue;
                }
                if dir_holds_non_ignored(project_path, dir, ignore_set) {
                    return true;
                }
            }
            None => {
                if !ignore_set.is_ignored_path(Path::new(entry), false) {
                    return true;
                }
            }
        }
    }

    false
}

/// Whether an untracked directory holds at least one file that is not
/// offcutignored.
///
/// Re-lists `dir` without `--directory` so every file beneath it is matched at
/// the granularity patterns are written against: a `logs/` holding only `*.js`
/// files is protected even though `*.js` does not match `logs` itself. Only
/// reached for directories that are not themselves ignored, so the collapsed
/// fast path still covers the common ignored-build-dir case.
///
/// The pathspec is `:(literal)`-prefixed so a directory name containing glob
/// metacharacters (`w[t]d`) is matched as the literal name, not as a pattern.
/// A listing failure reports "no non-ignored files", matching the fail-safe
/// `Err` handling in [`status_cleanable`]: never invent a cleanable verdict.
fn dir_holds_non_ignored(project_path: &Path, dir: &str, ignore_set: &IgnoreSet) -> bool {
    let pathspec = format!(":(literal){dir}");
    let output = git_cmd(
        project_path,
        &["ls-files", "--others", "-z", "--", &pathspec],
    );
    match output {
        Ok(files) => files
            .split('\0')
            .filter(|f| !f.is_empty())
            // `ls-files` without `--directory` lists only files, never
            // directories, so every entry here is a file.
            .any(|f| !ignore_set.is_ignored_path(Path::new(f), false)),
        Err(_) => false,
    }
}

/// Test whether `project_path` is a git repository — i.e. `git rev-parse
/// --git-dir` succeeds.
fn is_git_repo(project_path: &Path) -> bool {
    git_cmd(project_path, &["rev-parse", "--git-dir"]).is_ok()
}

/// Run a git command against `project_path` and return its stdout, or `Err`
/// carrying the formatted stderr on non-zero exit. We always pass `-C` to lock
/// the command at the project root so the result is independent of the
/// process's cwd.
fn git_cmd(project_path: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(project_path);
    for a in args {
        cmd.arg(a);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("git command failed: {e}"))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git command failed (exit {}): {} — {}",
            out.status.code().unwrap_or(-1),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_dir(label: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        d.push(format!(
            "offcut-classify-{}-{}-{}",
            label,
            std::process::id(),
            n
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Set up a fixture repo under `root` with the given args, returning the
    /// path of the project root so the test can classify it.
    fn fixture(label: &str) -> PathBuf {
        let root = unique_dir(label);
        // Initialize a git repo, set the global user for the test, commit an
        // initial file so we have at least one ref.
        git_run(&root, &["init"]);
        // Pin the branch name so the fixture does not depend on the ambient
        // `init.defaultBranch`; every `push origin main` below assumes `main`.
        // `symbolic-ref` works on every git version, unlike `init -b`.
        git_run(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_run(&root, &["config", "user.email", "test@test.dev"]);
        git_run(&root, &["config", "user.name", "Test"]);
        fs::write(root.join("initial.txt"), "initial contents").unwrap();
        git_run(&root, &["add", "initial.txt"]);
        git_run(&root, &["commit", "-m", "initial"]);
        root
    }

    /// Configure a fixture to have a remote (a local bare repo as the remote).
    fn fixture_with_remote(root: &Path) -> PathBuf {
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(root, &["config", "branch.main.remote", "origin"]);
        git_run(root, &["config", "branch.main.merge", "refs/heads/main"]);
        // Push the initial commit so the upstream is satisfied.
        git_run(root, &["push", "origin", "main"]);
        root.to_path_buf()
    }

    fn git_run(root: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(root);
        for a in args {
            cmd.arg(a);
        }
        let status = cmd.status().unwrap();
        assert!(
            status.success(),
            "fixture setup failed: git -C {} {:?}",
            root.display(),
            args
        );
    }

    fn write_file(dir: &Path, name: &str, contents: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
    }

    #[test]
    fn no_git_when_no_git_dir() {
        let root = unique_dir("nogit");
        write_file(&root, "package.json", "{}");
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::NoGit);
    }

    #[test]
    fn no_git_not_reported_when_git_dir_present() {
        let root = fixture("has_git");
        fixture_with_remote(&root);
        // Leave an untracked non-ignored file.
        fs::write(root.join("secret.txt"), "secret").unwrap();
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Cleanable); // has git, has remote, has untracked junk
    }

    #[test]
    fn no_remote_when_no_remote_configured() {
        let root = fixture("no_remote");
        // The fixture has init but no remote.
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::NoRemote);
    }

    #[test]
    fn unpushed_when_no_upstream() {
        // A repo with a remote and a pushed initial commit, plus a new local
        // commit on top that has not been pushed -> Unpushed (status 3).
        let root = fixture("unpushed_upstream");
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(&root, &["config", "branch.main.remote", "origin"]);
        git_run(&root, &["config", "branch.main.merge", "refs/heads/main"]);
        // Push initial, then add a local unpushed change.
        git_run(&root, &["push", "origin", "main"]);
        write_file(&root, "extra.txt", "unpushed");
        git_run(&root, &["add", "extra.txt"]);
        git_run(&root, &["commit", "-m", "local change"]);
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Unpushed);
    }

    #[test]
    fn wip_when_modified_tracked_file() {
        let root = fixture("wip");
        fixture_with_remote(&root);
        write_file(&root, "initial.txt", "modified");
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Wip);
    }

    #[test]
    fn cleanable_when_untracked_non_ignored_file() {
        let root = fixture("cleanable");
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(&root, &["config", "branch.main.remote", "origin"]);
        git_run(&root, &["config", "branch.main.merge", "refs/heads/main"]);
        git_run(&root, &["push", "origin", "main"]);
        // Leave an untracked file that is not offcutignored.
        write_file(&root, "secret.txt", "top secret");
        let ignore_set = IgnoreSet::empty(); // nothing is ignored
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Cleanable);
    }

    #[test]
    fn clean_when_all_untracked_are_ignored() {
        let root = fixture("clean");
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(&root, &["config", "branch.main.remote", "origin"]);
        git_run(&root, &["config", "branch.main.merge", "refs/heads/main"]);
        git_run(&root, &["push", "origin", "main"]);
        // Leave an untracked file that IS offcutignored.
        write_file(&root, "node_modules", "ignored");
        let ignore_set =
            IgnoreSet::from_layers(&root, &[(&PathBuf::new(), &["**/node_modules"])]).unwrap();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }

    #[test]
    fn cleanable_when_junk_is_gitignored_but_not_offcutignored() {
        // The whole point of offcut: build junk like `node_modules` is
        // gitignored by the *project*, so `git ls-files --exclude-standard`
        // would hide it. We must still see it (and, since it is NOT
        // offcutignored, classify as Cleanable) so it can be cleaned.
        // Use `ls-files --others` (no --exclude-standard) for this.
        let root = fixture("gitignored_junk");
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(&root, &["config", "branch.main.remote", "origin"]);
        git_run(&root, &["config", "branch.main.merge", "refs/heads/main"]);
        git_run(&root, &["push", "origin", "main"]);
        // Project ignores node_modules via its own .gitignore (NOT offcutignore).
        write_file(&root, ".gitignore", "node_modules\n");
        git_run(&root, &["add", ".gitignore"]);
        git_run(&root, &["commit", "-m", "ignore node_modules"]);
        git_run(&root, &["push", "origin", "main"]);
        // Drop build junk that the project gitignores but offcut does not.
        write_file(&root, "node_modules/junk.txt", "junk");
        let ignore_set = IgnoreSet::empty(); // nothing offcutignored
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Cleanable);
    }

    #[test]
    fn clean_when_ignored_untracked_path_is_non_ascii() {
        // `ls-files --others` without `-z` quotes and C-escapes a non-ASCII
        // path (`café.txt` → `"caf\303\251.txt"`), which matches no
        // offcutignore pattern and wrongly reports a protected file as
        // Cleanable. With `-z` the raw path reaches the matcher.
        let root = fixture("non_ascii");
        fixture_with_remote(&root);
        write_file(&root, "café.txt", "protected");
        let ignore_set =
            IgnoreSet::from_layers(&root, &[(&PathBuf::new(), &["café.txt"])]).unwrap();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }

    #[test]
    fn clean_when_ignored_untracked_directory_has_contents() {
        // `--directory` collapses a wholly-untracked directory to a single
        // `node_modules/` entry, so the pattern must still match it as a
        // directory rather than as each file beneath it.
        let root = fixture("ignored_dir");
        fixture_with_remote(&root);
        write_file(&root, "node_modules/pkg/index.js", "junk");
        let ignore_set =
            IgnoreSet::from_layers(&root, &[(&PathBuf::new(), &["node_modules/"])]).unwrap();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }

    #[test]
    fn clean_when_untracked_directory_contents_match_a_content_pattern() {
        // `--directory` collapses `logs/` to one entry, but `*.js` is written
        // against the files inside it, not the directory name. The directory
        // is not itself ignored, so it must be re-listed at file granularity;
        // every file beneath it is protected, so the repo is Clean.
        let root = fixture("content_pattern");
        fixture_with_remote(&root);
        write_file(&root, "logs/a.js", "junk");
        write_file(&root, "logs/b.js", "junk");
        let ignore_set = IgnoreSet::from_layers(&root, &[(&PathBuf::new(), &["*.js"])]).unwrap();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }

    #[test]
    fn cleanable_when_untracked_directory_holds_one_non_ignored_file() {
        // Same shape as above, but one file beneath the collapsed directory is
        // not protected. Matching is per file, so the unprotected file alone
        // makes the repo Cleanable — the cleaning engine excludes the rest.
        let root = fixture("mixed_dir");
        fixture_with_remote(&root);
        write_file(&root, "logs/a.js", "protected");
        write_file(&root, "logs/keep.txt", "not protected");
        let ignore_set = IgnoreSet::from_layers(&root, &[(&PathBuf::new(), &["*.js"])]).unwrap();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Cleanable);
    }

    #[test]
    fn clean_when_untracked_directory_name_has_glob_metacharacters() {
        // The re-listing pathspec must be `:(literal)`-quoted, or a directory
        // named `w[t]d` is read as a glob and matches nothing.
        let root = fixture("glob_dir");
        fixture_with_remote(&root);
        write_file(&root, "w[t]d/a.js", "junk");
        let ignore_set = IgnoreSet::from_layers(&root, &[(&PathBuf::new(), &["*.js"])]).unwrap();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }

    #[test]
    fn clean_when_only_untracked_directory_is_empty() {
        // `--directory` alone would newly surface an empty untracked directory
        // that the recursive form never reported; `--no-empty-directory` keeps
        // an empty dir from flipping a clean repo to Cleanable.
        let root = fixture("empty_dir");
        fixture_with_remote(&root);
        fs::create_dir_all(root.join("scratch")).unwrap();
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }

    #[test]
    fn precedence_wip_over_cleanable() {
        // A repo with uncommitted work (4) that also has untracked junk is
        // status 4, not 5. Precedence: most severe wins.
        let root = fixture("precedence");
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(&root, &["config", "branch.main.remote", "origin"]);
        git_run(&root, &["config", "branch.main.merge", "refs/heads/main"]);
        git_run(&root, &["push", "origin", "main"]);
        // Modify a tracked file (WIP) and leave an untracked non-ignored file.
        write_file(&root, "initial.txt", "modified");
        write_file(&root, "junk.txt", "junk");
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Wip);
    }

    #[test]
    fn clean_when_no_untracked_files() {
        // Committed+pushed, no untracked files at all → Clean.
        let root = fixture("no_untracked");
        let bare = unique_dir(&format!(
            "{}-bare",
            root.file_stem().unwrap().to_str().unwrap()
        ));
        git_run(&bare, &["init", "--bare"]);
        git_run(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git_run(&root, &["config", "branch.main.remote", "origin"]);
        git_run(&root, &["config", "branch.main.merge", "refs/heads/main"]);
        git_run(&root, &["push", "origin", "main"]);
        let ignore_set = IgnoreSet::empty();
        let status = classify(&root, &ignore_set);
        assert_eq!(status, Status::Clean);
    }
}
