//! Project discovery for offcut.
//!
//! Walks each configured workspace root and identifies project folders by
//! marker files (`.git`, `package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`,
//! `pom.xml`, `build.gradle`, `*.csproj`, etc.). The marker list is overridable
//! via the `project_markers` config field.
//!
//! ## Nesting rule
//!
//! Each workspace root yields at most one git project. Any marker (git or not)
//! that sits inside an ancestor git worktree is suppressed — it is a subfolder
//! of that git project, not a new project (issue #15 and #26). A nested `.git`
//! — file (gitdir pointer) or directory — is also a subfolder of the ancestor;
//! it is a worktree or submodule of the repo, not a separate project.
//!
//! "Ancestor git worktree" means: walking up from the marker folder toward its
//! workspace root, some ancestor directory contains a `.git` entry (directory
//! or file). The workspace root is the discovery boundary; nothing above it is
//! scanned. A `.git` at the marker folder's own level counts as the marker
//! folder being the git project (the one git project for its descendants),
//! not as an ancestor — all deeper items inherit that git project.
//!
//! `discover_single` (the `offcut clean <PROJECT_PATH>` entry point) bypasses
//! the walk but not this rule: a path inside a git worktree that is not that
//! worktree's root is rejected outright, so single-project targeting cannot
//! reach a subfolder the walk would never have reported as a project.
//!
//! ## Walk pruning
//!
//! `WalkDir` is instructed via `filter_entry` to never descend into a `.git`
//! directory: a directory entry whose basename is `.git` is not yielded and
//! not descended into. We do not scan git internals (`HEAD`, `objects/`,
//! `refs/`, etc.) as candidate projects. Only markers on each directory's
//! direct children are checked — `find_marker_in` reads the parent's children
//! via `fs::read_dir`, so a parent containing a `.git` entry is still detected
//! as a project.
//!
//! Artifact directories are also pruned from descent. The prune-basename set
//! is derived from the safelist catalog (`BUILT_IN_DEFAULTS` plus the user's
//! `safe_delete`) — each `**/<name>` or bare `<name>` pattern whose `<name>`
//! is a single glob-free path segment contributes `<name>` to the set (the
//! two shapes are equivalent under gitignore semantics). A `WalkDir` entry
//! whose basename is in the set is not yielded and not descended into.
//! Multi-segment patterns like `**/bin/obj` are NOT added to the
//! descent-prune set (they remain clean-time-only via the matcher). The
//! workspace root itself (depth 0) is not pruned even if its basename
//! matches an artifact name.
//!
//! Safe-to-delete directories are non-project artifacts by definition (the
//! safelist contract), so discovery skipping descent into them is correct, not
//! a behavior loss: no real project lives inside `node_modules`/`target`/etc.
//! The monorepo nesting rule is unaffected — pruning artifact dirs only removes
//! candidates that `has_ancestor_git` would have suppressed anyway, strictly
//! less work, no new false negatives.
//!
//! The one "no double-count" invariant: the workspace root itself is reported
//! only if it has a marker; otherwise it is a plain folder and not reported.
//! So if a workspace root is empty and a marker appears only at depth 2, only
//! the depth-2 path is reported — no phantom "depth 0" entry.
//!
//! Depth is measured from each workspace root: depth 0 is the workspace root
//! itself, depth 1 is a direct child directory, etc. We descend up to
//! `max_depth` from each root, honoring each workspace root's boundary.
//!
//! ## Marker semantics
//!
//! Markers are classified into two kinds:
//!
//! - **Literal filenames** (no glob characters): the file must exist with that
//!   exact name at the folder's top level. Examples: `.git`, `package.json`,
//!   `Cargo.toml`.
//! - **Glob patterns** (contain a glob character, e.g. `*.csproj`): any file
//!   in the folder whose basename matches the pattern counts as a marker. The
//!   pattern matching uses the `glob` crate's parser so a user-supplied custom
//!   pattern also works.
//!
//! Both kinds are documented on the config surface and implemented uniformly.
//!
//! ## Path kind
//!
//! Discovered paths are returned as walked from their workspace root, so they
//! inherit the root's form: an absolute root yields absolute paths, a relative
//! root yields relative ones. Nothing here canonicalizes. Downstream tooling
//! (classification, cleaning) that needs a stable reference must resolve the
//! paths itself, or the roots must be made absolute before `discover` is
//! called.

use std::fs;
use std::path::{Path, PathBuf};

use glob::Pattern;
use walkdir::WalkDir;

use crate::config::Config;
use crate::progress::ProgressWriter;
use crate::safelist::BUILT_IN_DEFAULTS;

/// A single discovered project path and the marker that identified it.
///
/// Kept together so tooling can display both — "here is a path, and here is
/// what we found" — rather than showing paths with no context. Useful for
/// diagnostics (e.g. the `Discovery` subcommand); classification consumes the
/// `path` of each entry.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredProject {
    pub path: PathBuf,
    pub marker: String,
}

/// Build a marker matcher from the list of marker strings on the config.
///
/// Each entry is classified as a literal filename or a glob pattern. Literal
/// filenames are compared directly via `Path::file_name()`. Glob patterns
/// are compiled once with `glob::Pattern::new` and matched per-file-name at
/// scan time.
///
/// A malformed glob is propagated as `Err` so the caller can report it like
/// any other config problem. This keeps discovery deterministic: the config
/// is trusted, but each pattern is validated once at construction time.
pub struct MarkerSet {
    literals: Vec<String>,
    globs: Vec<Pattern>,
}

impl MarkerSet {
    /// Compile every entry in the caller's marker list into this matcher.
    /// Each entry must be a syntactically valid glob pattern (even if it is
    /// actually a literal filename: `*.csproj` is a glob, `.git` is a literal
    /// name that happens to also parse as a valid glob).
    ///
    /// Returns `Ok` for every valid entry; propagates the first parse error.
    pub fn from_entries(entries: &[String]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut literals: Vec<String> = Vec::new();
        let mut globs: Vec<Pattern> = Vec::new();
        for entry in entries {
            // Classify: an entry containing an unescaped glob character
            // (`*`, `?`, `[`, `{`) is treated as a glob pattern. Everything
            // else is a literal filename. This is conservative — a literal
            // filename that happens to contain `*` would be treated as a glob,
            // but that is the documented rule and callers can pick either form.
            if contains_glob_meta(entry) {
                globs.push(Pattern::new(entry)?);
            } else {
                literals.push(entry.clone());
            }
        }
        Ok(MarkerSet { literals, globs })
    }

    /// Test whether `file_name` (the basename of a file in some directory)
    /// matches any of the compiled markers. Returns the matched entry name
    /// (literal or glob source) so the caller can label each hit.
    pub fn matches(&self, file_name: &str) -> Option<&str> {
        for lit in &self.literals {
            if lit == file_name {
                return Some(lit.as_str());
            }
        }
        for pat in &self.globs {
            if pat.matches(file_name) {
                // Return the compiled source pattern so the output names the
                // marker, not an opaque compiled form.
                return Some(pat.as_str());
            }
        }
        None
    }
}

/// Test whether `name` looks like a glob pattern. Heuristic: presence of any
/// unescaped `*`, `?`, `[`, or `{`. The `glob` crate is lenient about most
/// characters, so this is the conservative rule — every filename that happens
/// to contain a glob meta character is treated as a glob and compiled once.
fn contains_glob_meta(s: &str) -> bool {
    for ch in s.chars() {
        match ch {
            '*' | '?' | '[' | '{' => return true,
            _ => continue,
        }
    }
    false
}

/// Discover projects under each workspace root configured on `cfg`.
///
/// Walks each workspace root up to `max_depth` (inclusive — depth 0 is the
/// root itself, depth N is the Nth descendant). Each folder that contains a
/// file matching any of `cfg.project_markers` is reported as a `DiscoveredProject`,
/// tagged with the matched marker name.
///
/// The walk stops at each workspace root's boundary: no path above the root
/// is visited, and the walk does not cross a root's boundary to descend into
/// a sibling root. Each root is walked independently.
///
/// Returns an empty list (with no error) when there are no workspace roots —
/// discovery has nothing to walk. An IO error (unreachable root, permission
/// denied) is propagated; the walk is fail-fast so a misconfigured root
/// surfaces as an error rather than silently reporting nothing.
pub fn discover(cfg: &Config) -> Result<Vec<DiscoveredProject>, Box<dyn std::error::Error>> {
    let markers = MarkerSet::from_entries(&cfg.project_markers)?;
    let max_depth = cfg.max_depth;
    let prune_basenames = build_prune_set(&cfg.safe_delete);

    if cfg.workspace_roots.is_empty() {
        println!(
            "discovery: no workspace roots configured; nothing to walk — try `offcut init <path>` to create a config file"
        );
        return Ok(Vec::new());
    }

    let mut results: Vec<DiscoveredProject> = Vec::new();
    let mut progress = ProgressWriter::new(std::io::stdout());
    for root in &cfg.workspace_roots {
        if let Err(e) = walk_root(
            root,
            max_depth,
            &markers,
            &prune_basenames,
            &mut results,
            &mut progress,
        ) {
            progress.finish();
            return Err(e);
        }
    }
    progress.finish();

    Ok(results)
}

/// Discover a single project at `path`, bypassing the workspace-root walk
/// entirely. Used by `offcut clean <PROJECT_PATH>` to scope discovery and
/// deletion strictly to the one project the caller named — no neighboring
/// project is discovered or cleaned, even if `path` sits inside a configured
/// workspace root.
///
/// `path` may be absolute or relative to the process's current directory.
/// It is canonicalized when it exists so downstream classification and
/// cleaning operate on a stable absolute path; a non-directory or missing
/// path is a hard error (fail-fast, mirroring `walk_root`'s missing-root
/// check). The directory is checked for a marker via the same `MarkerSet` as
/// the walk; a directory with no marker is still returned (tagged
/// `"(none)"`) so classification can report it — e.g. as `no-git` — rather
/// than silently dropping the caller's explicit target.
///
/// The walk's nesting rule applies here too: a path that sits *inside* a git
/// worktree without being its root is a subfolder of that project, not a
/// project, and is rejected with an error naming the root. This is
/// defense-in-depth, not the only guard: `classify::status_no_git` already
/// refuses such a path — a subdirectory has no `.git` of its own, so it
/// classifies as `NoGit`, which is never cleanable. This check rejects it
/// earlier, before any classification or deletion, and with an actionable
/// error naming the root instead of a confusing `no-git` report.
///
/// No progress indicator is rendered: there is no walk to report on.
pub fn discover_single(
    path: &Path,
    cfg: &Config,
) -> Result<DiscoveredProject, Box<dyn std::error::Error>> {
    if !path.is_dir() {
        let mut msg = format!(
            "project path not found or not a directory: {}",
            path.display()
        );
        // offcut does not expand `~` itself (the caller's shell does). A
        // quoted `~` or an invocation without a shell reaches this error
        // with the literal `~` intact; point the user at the cause rather
        // than leaving them to guess why a path that "looks right" failed.
        if path.to_string_lossy().starts_with('~') {
            msg.push_str(
                "\n  hint: `~` is expanded by your shell — pass an absolute \
                 path, or leave `~` unquoted so the shell expands it",
            );
        }
        return Err(msg.into());
    }
    // Canonicalize to a stable absolute path so the report and `git -C` use
    // the same form regardless of how the caller typed it. Canonicalization
    // resolves symlinks too, which is the safe direction for deletion. When
    // canonicalization fails (rare, but possible without breaking `is_dir`),
    // fall back to a lexically normalized absolute path so the
    // `enclosing_git_root` comparison below is absolute-vs-absolute rather
    // than relative-vs-absolute — a relative fallback would false-reject a
    // real project root because `git rev-parse --show-toplevel` is always
    // absolute. The shared `resolve_path` helper is used on both sides of
    // that comparison so the invariant is structural, not merely documented.
    let resolved = resolve_path(path)?;
    if let Some(git_root) = enclosing_git_root(&resolved)
        && git_root != resolved
    {
        return Err(format!(
            "project path is not a project root: {}\n\
             it is inside the git project at {} — pass that path instead",
            resolved.display(),
            git_root.display()
        )
        .into());
    }
    let markers = MarkerSet::from_entries(&cfg.project_markers)?;
    let marker = find_marker_in(&resolved, &markers)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(none)".to_string());
    Ok(DiscoveredProject {
        path: resolved,
        marker,
    })
}

/// Return the root of the git worktree containing `dir`, or `None` when `dir`
/// is not inside a git worktree (or `git` is unavailable — classification then
/// reports the path as `no-git` and nothing is cleaned).
///
/// Shells out to `git rev-parse --show-toplevel` like the rest of the crate's
/// git inspection rather than scanning ancestors for a `.git` entry: git is
/// the authority on where a worktree starts, so a linked worktree or submodule
/// (whose `.git` is a file pointer) correctly reports itself as a root instead
/// of being mistaken for a subfolder of the repo above it.
fn enclosing_git_root(dir: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if top.is_empty() {
        return None;
    }
    let top = PathBuf::from(top);
    // Mirror `discover_single`'s resolution strategy via the shared
    // `resolve_path` helper so the `git_root != resolved` comparison in
    // `discover_single` is like-for-like even when canonicalization is
    // unavailable on either side. `--show-toplevel` always prints an
    // absolute path, so the helper's relative-path branch is not reached
    // here; `.ok()` maps a resolution failure to `None`, which the caller
    // treats as "not inside a worktree" (classification then reports the
    // path as `no-git` and nothing is cleaned).
    resolve_path(&top).ok()
}

/// Resolve `path` to a stable absolute path: `std::fs::canonicalize` when it
/// succeeds, otherwise a lexically normalized absolute path via
/// [`normalized_absolute`]. The single shared implementation of the
/// canonicalize-with-lexical-fallback strategy — used by both
/// `discover_single` (the target) and `enclosing_git_root` (the git root) —
/// makes the `git_root != resolved` comparison structurally like-for-like
/// rather than relying on two copies staying in sync.
///
/// Only a *relative* `path` needs the process cwd as a base, so `current_dir`
/// is read on that branch alone: an absolute target must not fail merely
/// because the cwd is gone or unreadable. A failure to read the cwd for a
/// relative path is propagated as an error naming the path.
fn resolve_path(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(_) if path.is_absolute() => Ok(normalized_absolute(path, Path::new(""))),
        Err(_) => {
            let cwd = std::env::current_dir().map_err(|e| {
                format!(
                    "cannot resolve relative path {}: the current directory is unavailable ({e})",
                    path.display()
                )
            })?;
            Ok(normalized_absolute(path, &cwd))
        }
    }
}

/// Convert `path` into an absolute, lexically normalized path using `cwd`
/// as the base when `path` is relative. Pure: no filesystem access, so it is
/// safe to call as a fallback when `std::fs::canonicalize` has already
/// failed and as the implementation under test in unit tests.
///
/// Normalization collapses `.` components, resolves `..` against the
/// preceding components (without dropping the leading root), and drops
/// trailing slashes — the same shape `git rev-parse --show-toplevel` and
/// `std::fs::canonicalize` yield on success. Symlinks are NOT resolved
/// (that needs the filesystem), so this is a fallback only; the primary
/// path is always `canonicalize`.
fn normalized_absolute(path: &Path, cwd: &Path) -> PathBuf {
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        cwd.to_path_buf()
    };
    for comp in path.components() {
        use std::path::Component::*;
        match comp {
            RootDir => {
                // An absolute `path` starts here; reset the accumulator to
                // the root so any leading components from `cwd` are dropped.
                // On Windows a `Prefix` (e.g. `C:`) may already be in `out`
                // from a prior component or the cwd base — preserve it so
                // `C:\foo` does not collapse to `\foo` (which would
                // false-reject a real root against git's absolute toplevel).
                let prefix = match out.components().next() {
                    Some(Prefix(p)) => Some(p.as_os_str().to_owned()),
                    _ => None,
                };
                out = PathBuf::new();
                if let Some(p) = prefix {
                    out.push(p);
                }
                out.push(comp.as_os_str());
            }
            CurDir => {} // collapse `.`
            ParentDir => match out.components().next_back() {
                // Pop a real component; otherwise drop the `..` rather than
                // pushing it (mirrors `Path::canonicalize`, which clamps at
                // the root instead of producing `/..` segments). `git
                // rev-parse --show-toplevel` never emits `..`, so the
                // comparison invariant only needs the canonicalize-shaped
                // form.
                Some(c) if !matches!(c, RootDir | ParentDir | CurDir) => {
                    out.pop();
                }
                _ => {}
            },
            Prefix(p) => {
                out = PathBuf::new();
                out.push(p.as_os_str());
            }
            Normal(s) => out.push(s),
        }
    }
    out
}

/// Derive the descent-prune basename set from the safelist catalog.
///
/// The catalog is `BUILT_IN_DEFAULTS` plus `user_patterns` (the caller's
/// `safe_delete`). This is not a glob parser: a literal `**/` prefix, if
/// present, is stripped, and what remains contributes to the prune set only
/// if it is a plain single path segment — no `/`, no glob metacharacters, no
/// leading `!`. Under gitignore semantics a bare `<name>` matches at any
/// depth exactly like `**/<name>`, so both shapes prune.
///
/// Everything else is skipped for descent pruning but still honored at clean
/// time (do not error on it): multi-segment patterns like `**/bin/obj`
/// (pruning on bare `obj` would be too broad and could skip a legitimate
/// project directory), glob patterns like `**/*.log` (the prune set matches
/// basenames by exact equality), and `!` negations.
///
/// The `.git` entry is always in the set regardless of the user patterns —
/// it is the discovery-internal invariant that `WalkDir` never descends into
/// a git repo's internals.
fn build_prune_set(user_patterns: &[String]) -> std::collections::HashSet<String> {
    use std::collections::HashSet;

    let mut set: HashSet<String> = HashSet::new();
    // `.git` is always pruned — the discovery-internal invariant.
    set.insert(".git".to_string());

    for pat in BUILT_IN_DEFAULTS
        .iter()
        .copied()
        .chain(user_patterns.iter().map(String::as_str))
    {
        let name = pat.strip_prefix("**/").unwrap_or(pat);
        if !name.is_empty()
            && !name.contains('/')
            && !name.starts_with('!')
            && !contains_glob_meta(name)
        {
            set.insert(name.to_string());
        }
    }
    set
}

/// Walk a single workspace root up to `max_depth` directories deep and tag
/// each folder with a marker.
///
/// `max_depth` is inclusive — depth 0 is the workspace root itself; depth 1
/// is a direct child directory; etc. `WalkDir::max_depth` is the correct API.
/// The first entry (depth 0) is the root itself, which is checked for markers
/// too.
///
/// `prune_basenames` is the set of directory basenames that `WalkDir` should
/// refuse to descend into. Each is a non-project artifact (the safelist
/// contract): `node_modules`, `target`, `dist`, etc. A `.git` entry is also
/// pruned — but `find_marker_in` reads the parent's children directly so a
/// parent containing a `.git` is still detected as a project.
///
/// `progress` is the live single-line progress writer: each
/// visited directory is rendered on one line that overwrites itself in place
/// via a carriage return on a TTY, giving the user feedback that offcut is
/// working on a large workspace. When not a TTY the writer is a no-op.
fn walk_root(
    root: &Path,
    max_depth: usize,
    markers: &MarkerSet,
    prune_basenames: &std::collections::HashSet<String>,
    out: &mut Vec<DiscoveredProject>,
    progress: &mut ProgressWriter<std::io::Stdout>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate the root exists before walking. An empty path or a non-existent
    // directory is a hard error — discovery must not silently absorb a typo.
    if !root.is_dir() {
        return Err(format!("workspace root not found: {}", root.display()).into());
    }

    for entry in WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Never prune the depth-0 root entry itself — even if it is
            // literally named `node_modules`, we want the user to see the
            // misconfiguration rather than a silent empty result.
            if e.depth() == 0 {
                return true;
            }
            // Refuse to descend into any directory whose basename is in the
            // prune set. The set includes `.git` (true prune — no readdir
            // on `.git/objects` etc.) plus each artifact basename derived
            // from the safelist catalog.
            let name = e.file_name().to_string_lossy();
            !prune_basenames.contains(name.as_ref())
        })
    {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }
        // Emit the current directory path on one line that overwrites itself
        // in place via a carriage return. Each visited directory
        // is reported so the user sees offcut working on a large workspace
        // rather than appearing hung. The writer is TTY-gated: when not a TTY
        // this is a no-op (no carriage-return garbage in a pipe or log file).
        progress.update(entry.path());
        // Check each direct child file for markers. Only look at *files* —
        // a marker is a file, never a directory (except `.git` which is a
        // directory on disk but is treated as a file-name marker).
        if let Some(marker) = find_marker_in(entry.path(), markers) {
            // Suppress when an ancestor git worktree exists: every marker
            // — git or not — is a subfolder of that git project, not a
            // new project. A nested `.git` (file or directory) is a worktree
            // or submodule of the ancestor, never a separate project.
            if !has_ancestor_git(entry.path(), root) {
                out.push(DiscoveredProject {
                    path: entry.path().to_path_buf(),
                    marker: marker.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Check whether any ancestor directory between `dir` and `root` (inclusive of
/// `root`, exclusive of `dir`) contains a `.git` entry.
///
/// Walks each parent of `dir` upward; stops when the parent equals the
/// workspace root (the discovery boundary). A `.git` at `dir`'s own level is
/// **not** an ancestor — it is itself the git project, and all deeper items
/// are suppressed as subfolders of that project. The workspace root itself
/// has no ancestors inside the boundary, so a marker at the root is never
/// suppressed by a git repo above the root.
fn has_ancestor_git(dir: &Path, root: &Path) -> bool {
    if dir == root {
        return false;
    }
    let mut current = dir.to_path_buf();
    while let Some(parent) = current.parent() {
        current = parent.to_path_buf();
        if current.join(".git").exists() {
            return true;
        }
        if current == root {
            break;
        }
    }
    false
}

/// Scan `dir`'s direct children for any file whose basename matches `markers`.
///
/// Walks only the immediate children — not recursive — because each
/// `DiscoveredProject` is about one directory at one depth. A deeper match
/// would surface via a subsequent `WalkDir` iteration.
///
/// For each marker kind:
/// - Literal: compare the basename string.
/// - Glob: compile once per call site, not per file (the MarkerSet caches).
///
/// `.git` wins over every other marker when a folder contains both, so the
/// reported marker does not depend on `read_dir`'s platform-specific entry
/// order — a git repo that also carries e.g. a `package.json` is always
/// reported with the `.git` marker. Suppression itself no longer keys on
/// which marker matched (issue #26): anything under an ancestor git worktree
/// is suppressed regardless.
fn find_marker_in<'a>(dir: &Path, markers: &'a MarkerSet) -> Option<&'a str> {
    let read = fs::read_dir(dir).ok()?;
    let mut first: Option<&'a str> = None;
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(m) = markers.matches(name_str.as_ref()) {
            if m == ".git" {
                return Some(m);
            }
            if first.is_none() {
                first = Some(m);
            }
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    use std::sync::LazyLock;

    static COUNTER: LazyLock<AtomicU64> =
        LazyLock::new(|| AtomicU64::new(std::process::id() as u64));

    fn mkconfig(root: &Path, markers: &[&str], depth: usize) -> Config {
        Config {
            workspace_roots: vec![root.to_path_buf()],
            max_depth: depth,
            project_markers: markers.iter().map(|s| s.to_string()).collect(),
            ..Config::default()
        }
    }

    fn mkfixture(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    fn tmp_root(label: &str) -> PathBuf {
        let d = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let out = d.join(format!("offcut-discovery-test-{}-{}", label, n));
        fs::create_dir_all(&out).unwrap();
        out
    }

    /// Initialize a real git repo at `dir` so tests can exercise the `.git`
    /// directory form (HEAD, refs, objects) rather than only the `.git` file
    /// form. Each fixture writes an initial file and commits so the repo has
    /// at least one ref.
    fn git_init(dir: &Path) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("init")
            .status()
            .unwrap();
        assert!(status.success(), "git init failed: {:?}", dir);
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .arg("symbolic-ref")
            .arg("HEAD")
            .arg("refs/heads/main");
        assert!(cmd.status().unwrap().success());
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .arg("config")
            .arg("user.email")
            .arg("test@test.dev");
        assert!(cmd.status().unwrap().success());
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .arg("config")
            .arg("user.name")
            .arg("Test");
        assert!(cmd.status().unwrap().success());
        fs::write(dir.join("initial.txt"), "initial").unwrap();
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(dir).arg("add").arg("initial.txt");
        assert!(cmd.status().unwrap().success());
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(dir).arg("status").arg("--porcelain");
        let out = cmd.output().unwrap();
        if !out.stdout.is_empty() {
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C")
                .arg(dir)
                .arg("commit")
                .arg("-m")
                .arg("initial");
            assert!(cmd.status().unwrap().success());
        }
    }

    /// Write a `.git` gitdir pointer file at `dir` pointing at `target`.
    fn write_gitdir(dir: &Path, target: &Path) -> PathBuf {
        let gitfile = dir.join(".git");
        fs::create_dir_all(dir).unwrap();
        fs::write(&gitfile, format!("gitdir: {}", target.display())).unwrap();
        gitfile
    }

    #[test]
    fn literal_marker_detected() {
        let root = tmp_root("literal");
        mkfixture(&root, ".git", "initial");
        mkfixture(&root, "README.md", "hi");
        let cfg = mkconfig(&root, &[".git"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
    }

    #[test]
    fn glob_marker_detected() {
        let root = tmp_root("glob");
        mkfixture(&root, "Foo.csproj", "<project/>");
        mkfixture(&root, "README.md", "hi");
        let cfg = mkconfig(&root, &["*.csproj"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].marker, "*.csproj");
    }

    #[test]
    fn nested_git_suppressed_inside_root() {
        // A nested `.git` (file or directory) inside an ancestor git repo
        // is a worktree / submodule of the ancestor — it is suppressed,
        // not reported as a separate project (issue #26).
        let root = tmp_root("nested_git");
        mkfixture(&root, ".git", "root repo");
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        mkfixture(&sub, ".git", "nested repo");
        let cfg = mkconfig(&root, &[".git"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
    }

    #[test]
    fn nested_non_git_suppressed_inside_root() {
        // A nested non-git marker inside an ancestor git worktree is
        // suppressed — it is a subfolder of root's git project, not a
        // separate project (issue #15).
        let root = tmp_root("nested_non_git");
        mkfixture(&root, ".git", "root repo");
        let pkg = root.join("packages/a");
        fs::create_dir_all(&pkg).unwrap();
        mkfixture(&pkg, "package.json", "{}");
        let cfg = mkconfig(&root, &[".git", "package.json"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        // The package folder is not reported — it is a subfolder of root's
        // git worktree, not a separate project.
    }

    #[test]
    fn max_depth_limits_walk() {
        let root = tmp_root("depth");
        let sub1 = root.join("a");
        fs::create_dir_all(&sub1).unwrap();
        let sub2 = sub1.join("b");
        fs::create_dir_all(&sub2).unwrap();
        mkfixture(&root, ".git", "root");
        mkfixture(&sub1, "package.json", "{}");
        mkfixture(&sub2, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "package.json", "Cargo.toml"], 1);
        let results = discover(&cfg).unwrap();
        // Depth 0 (root, which has .git) is reported; depth 1 (sub1) is suppressed
        // — its package.json is a non-git marker inside the ancestor git worktree.
        // Depth 2 (sub2) is beyond max_depth=1, so not visited.
        assert_eq!(results.len(), 1);
        let paths: Vec<_> = results
            .iter()
            .map(|r| r.path.as_os_str().to_string_lossy().to_string())
            .collect();
        assert!(paths.iter().any(|p| p == root.to_str().unwrap()));
        assert!(!paths.iter().any(|p| p.ends_with("a")));
        assert!(!paths.iter().any(|p| p.ends_with("b")));
    }

    #[test]
    fn empty_workspace_returns_clear_message() {
        let root = tmp_root("empty");
        let cfg = mkconfig(&root, &[".git"], 2);
        let mut cfg_clone = cfg.clone();
        cfg_clone.workspace_roots = Vec::new();
        let results = discover(&cfg_clone).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn no_marker_is_not_reported() {
        let root = tmp_root("plain");
        mkfixture(&root, "README.md", "plain");
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        mkfixture(&sub, "main.rs", "fn main() {}");
        let cfg = mkconfig(&root, &[".git", "package.json"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn workspace_root_boundary_respected() {
        let root = tmp_root("boundary");
        let outside = root.join("outside").join("deep");
        fs::create_dir_all(&outside).unwrap();
        mkfixture(&outside, ".git", "intruder");
        let cfg = mkconfig(&root, &[".git"], 2);
        // Depth 2 from root lands at outside/.git — still within max_depth,
        // so it is reported. The boundary is the root's own depth, not the
        // file's filesystem position.
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, outside);
    }

    #[test]
    fn missing_workspace_root_errors() {
        let root = tmp_root("missing");
        let cfg = mkconfig(&root, &[".git"], 2);
        let mut cfg_clone = cfg.clone();
        cfg_clone.workspace_roots = vec![PathBuf::from("/does/not/exist")];
        assert!(discover(&cfg_clone).is_err());
    }

    #[test]
    fn marker_set_ignores_non_listed_basename() {
        let ms = MarkerSet::from_entries(&[".git".to_string(), "*.csproj".to_string()]).unwrap();
        assert_eq!(ms.matches("README.md"), None);
        assert_eq!(ms.matches("main.rs"), None);
        assert_eq!(ms.matches(".git"), Some(".git"));
        assert_eq!(ms.matches("Foo.csproj"), Some("*.csproj"));
    }

    #[test]
    fn marker_set_rejects_malformed_glob() {
        // The `glob` crate treats a reversed character class as a literal,
        // but an unclosed bracket fails to parse.
        assert!(MarkerSet::from_entries(&["[unclosed".to_string()]).is_err());
    }

    #[test]
    fn discovered_paths_are_absolute() {
        let root = tmp_root("abs");
        mkfixture(&root, ".git", "abs");
        let cfg = mkconfig(&root, &[".git"], 1);
        let results = discover(&cfg).unwrap();
        assert!(results[0].path.is_absolute());
    }

    #[test]
    fn symlinked_dir_not_followed() {
        let root = tmp_root("sym");
        fs::remove_dir_all(&root).ok();
        mkfixture(&root, ".git", "sym");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        mkfixture(&sub, "Cargo.toml", "[package]");
        // Place a symlink that points at sub's Cargo.toml; the walk does not
        // follow into sub twice.
        let link = sub.join("link");
        symlink(&sub, &link).unwrap();
        // Add another marker outside sub — not reachable via the link.
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        mkfixture(&outside, "package.json", "{}");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml", "package.json"], 2);
        let results = discover(&cfg).unwrap();
        // Only the root (which has .git) is reported — sub's Cargo.toml and
        // outside's package.json are each suppressed as non-git markers inside
        // the ancestor git worktree (issue #15). Each appears exactly once
        // for the .git marker that actually defines a standalone project.
        assert_eq!(results.len(), 1);
        let names: Vec<_> = results.iter().map(|r| r.marker.as_str()).collect();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], ".git");
    }

    #[test]
    fn nested_git_with_other_marker_still_suppressed() {
        // A nested folder that carries BOTH `.git` and another marker
        // (e.g. a vendored JS repo with a package.json) is suppressed — it
        // is a subfolder of root's git project, not a separate project.
        let root = tmp_root("nested_git_dual");
        mkfixture(&root, ".git", "root repo");
        let sub = root.join("vendored");
        fs::create_dir_all(&sub).unwrap();
        mkfixture(&sub, "package.json", "{}");
        mkfixture(&sub, ".git", "nested repo");
        let cfg = mkconfig(&root, &["package.json", ".git"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        // The vendored folder is not reported — it is a subfolder of root's
        // git project, regardless of which marker read_dir yields first.
    }

    #[test]
    fn root_marker_not_suppressed_by_git_above_boundary() {
        // A git repo ABOVE the workspace root must not suppress a marker at
        // the root itself: the workspace root is the discovery boundary
        // (issue #15), so nothing above it is consulted.
        let outer = tmp_root("outer_git");
        mkfixture(&outer, ".git", "repo above the boundary");
        let root = outer.join("workspace");
        fs::create_dir_all(&root).unwrap();
        mkfixture(&root, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, "Cargo.toml");
    }

    #[test]
    fn monorepo_reports_only_root_git() {
        // A monorepo with a single root `.git` and nested package folders
        // each containing a non-git marker should report only the root.
        let root = tmp_root("monorepo");
        mkfixture(&root, ".git", "root repo");
        let pkg_a = root.join("packages/a");
        fs::create_dir_all(&pkg_a).unwrap();
        mkfixture(&pkg_a, "package.json", "{}");
        let pkg_b = root.join("packages/b");
        fs::create_dir_all(&pkg_b).unwrap();
        mkfixture(&pkg_b, "package.json", "{}");
        let cfg = mkconfig(&root, &[".git", "package.json"], 3);
        let results = discover(&cfg).unwrap();
        // Only the root (which has .git) is reported.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        // Neither packages/a nor packages/b is reported — they are subfolders
        // of the parent repo, not separate projects (issue #15).
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&pkg_a));
        assert!(!paths.contains(&pkg_b));
    }

    // -----------------------------------------------------------------------
    // Issue #26: nested git-worktree / .git-file / dangling-gitdir scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn nested_worktree_with_real_gitdir_suppressed() {
        // A real git init at root, plus a nested directory whose `.git` is
        // a file pointing at an existing gitdir (a real worktree). The
        // worktree is suppressed — it is a subfolder of root's git repo.
        let root = tmp_root("worktree_real_gitdir");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        let wt = root.join(".worktrees/work/issue-1");
        fs::create_dir_all(&wt).unwrap();
        write_gitdir(&wt, &root.join(".git"));
        let cfg = mkconfig(&root, &[".git", "package.json"], 3);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        // The worktree itself is not reported.
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&wt));
    }

    #[test]
    fn nested_worktree_with_dangling_gitdir_suppressed() {
        // A real git init at root, plus a nested directory whose `.git` is
        // a gitdir pointer to a path that does NOT exist on this filesystem.
        // The worktree is suppressed — it is a subfolder of root's git repo,
        // and even if it ever reaches classification the warning is silenced.
        let root = tmp_root("worktree_dangling_gitdir");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        let wt = root.join(".worktrees/work/issue-2");
        fs::create_dir_all(&wt).unwrap();
        write_gitdir(&wt, &PathBuf::from("/nope/nope/nope"));
        let cfg = mkconfig(&root, &[".git", "package.json"], 3);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&wt));
    }

    #[test]
    fn nested_git_file_with_valid_gitdir_suppressed() {
        // A root with a real git init, plus a sibling directory whose `.git`
        // is a file pointing at root's own `.git` — that sibling is itself
        // a worktree of root. Suppressed.
        let root = tmp_root("nested_git_valid_gitdir");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        let nested = root.join("submodule");
        fs::create_dir_all(&nested).unwrap();
        write_gitdir(&nested, &root.join(".git"));
        let cfg = mkconfig(&root, &[".git"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&nested));
    }

    #[test]
    fn nested_git_file_with_dangling_gitdir_suppressed() {
        // A root with a real git init, plus a sibling whose `.git` is a
        // file pointing at a path that does not exist locally. Suppressed.
        let root = tmp_root("nested_git_dangling_file");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        let nested = root.join("stray");
        fs::create_dir_all(&nested).unwrap();
        write_gitdir(&nested, &PathBuf::from("/does/not/exist"));
        let cfg = mkconfig(&root, &[".git"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&nested));
    }

    #[test]
    fn monorepo_with_deeply_nested_packages_suppressed() {
        // A monorepo where packages nest deeper than one level — each
        // non-git marker still suppressed because ancestor git reaches all
        // of them.
        let root = tmp_root("deep_monorepo");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        let pkg_a = root.join("packages/a");
        fs::create_dir_all(&pkg_a).unwrap();
        mkfixture(&pkg_a, "package.json", "{}");
        let pkg_b = root.join("packages/b/src");
        fs::create_dir_all(&pkg_b).unwrap();
        mkfixture(&pkg_b, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "package.json", "Cargo.toml"], 3);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&pkg_a));
        assert!(!paths.contains(&pkg_b));
    }

    #[test]
    fn git_directory_not_reported_inside_walk() {
        // The walker never descends into a `.git` directory. Each iteration
        // whose path includes a `.git` component is skipped, so no git
        // internal (HEAD, refs, objects) ever surfaces as a project.
        let root = tmp_root("git_dir_not_reported");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        // Plant a Cargo.toml somewhere inside .git/objects/ — it would be
        // a marker if we descended into .git, but we don't.
        let snek = root.join(".git/objects/deep");
        fs::create_dir_all(&snek).unwrap();
        mkfixture(&snek, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 3);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&snek));
    }

    // -----------------------------------------------------------------------
    // Artifact-directory pruning (issue #18 performance fix)
    // -----------------------------------------------------------------------

    #[test]
    fn node_modules_not_descended_into() {
        // A plain (non-git) workspace root whose `node_modules` contains
        // packages — nothing suppresses the nested package.json except the
        // prune itself, so this fails if the walker descends into
        // `node_modules` at all.
        let root = tmp_root("artifact_node_modules");
        // NOTE: no git_init — root is a plain workspace, so the nesting rule
        // cannot mask a pruning regression.
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        mkfixture(&app, "package.json", "{}");
        let nm = root.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        let pkg = nm.join("lodash");
        fs::create_dir_all(&pkg).unwrap();
        mkfixture(&pkg, "package.json", "{}");
        let cfg = mkconfig(&root, &[".git", "package.json"], 3);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, app);
        assert_eq!(results[0].marker, "package.json");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&pkg), "lodash must not be reported:");
    }

    #[test]
    fn target_not_descended_into() {
        // A plain (non-git) workspace root whose `target/` contains a nested
        // Cargo.toml — only the sibling project is reported; the nested
        // Cargo.toml would surface if pruning regressed.
        let root = tmp_root("artifact_target");
        // NOTE: no git_init — root is a plain workspace, so the nesting rule
        // cannot mask a pruning regression.
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        mkfixture(&app, "Cargo.toml", "[package]");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        let deep = target.join("debug/deep");
        fs::create_dir_all(&deep).unwrap();
        mkfixture(&deep, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 3);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, app);
        assert_eq!(results[0].marker, "Cargo.toml");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(
            !paths.contains(&deep),
            "target/debug/deep must not be reported:"
        );
    }

    #[test]
    fn user_safe_delete_artifact_pruned() {
        // A user-added `**/.my-artifacts` pattern prunes `.my-artifacts` from
        // discovery too — the safelist is the single source of truth.
        let root = tmp_root("user_artifact");
        // NOTE: no git_init — root is a plain workspace, so the nesting rule
        // cannot mask a pruning regression.
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        mkfixture(&app, "package.json", "{}");
        let art = root.join(".my-artifacts");
        fs::create_dir_all(&art).unwrap();
        mkfixture(&art, "package.json", "{}");
        let mut cfg = mkconfig(&root, &[".git", "package.json"], 3);
        cfg.safe_delete = vec!["**/.my-artifacts".to_string()];
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, app);
        assert_eq!(results[0].marker, "package.json");
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&art), ".my-artifacts must not be reported:");
    }

    #[test]
    fn bare_name_safe_delete_pruned() {
        // A bare single-segment `safe_delete` entry (`my-cache`, gitignore
        // shorthand for `**/my-cache`) prunes descent just like the `**/`
        // form — parity with the clean-time matcher.
        let root = tmp_root("bare_name");
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        mkfixture(&app, "package.json", "{}");
        let cache = root.join("my-cache");
        fs::create_dir_all(&cache).unwrap();
        mkfixture(&cache, "package.json", "{}");
        let mut cfg = mkconfig(&root, &[".git", "package.json"], 3);
        cfg.safe_delete = vec!["my-cache".to_string()];
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, app);
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(!paths.contains(&cache), "my-cache must not be reported:");
    }

    #[test]
    fn multi_segment_pattern_does_not_over_prune() {
        // A user pattern `**/bin/obj` does NOT cause pruning on the bare
        // `obj` basename. A real project sitting as `obj/` (with NO git
        // ancestor so the nesting rule does not suppress it) is still
        // discovered.
        let root = tmp_root("multi_seg");
        fs::create_dir_all(&root).unwrap();
        // NOTE: no git_init here — root is a plain dir, so obj/ has no git
        // ancestor and is NOT suppressed by the nesting rule.
        let obj = root.join("obj");
        fs::create_dir_all(&obj).unwrap();
        mkfixture(&obj, "package.json", "{}");
        let mut cfg = mkconfig(&root, &[".git", "package.json"], 3);
        cfg.safe_delete = vec!["**/bin/obj".to_string()];
        let results = discover(&cfg).unwrap();
        // obj/ is reported because **/bin/obj is multi-segment and does not
        // prune the bare `obj` basename, and there is no git ancestor to
        // suppress it.
        assert_eq!(results.len(), 1, "obj/ must be reported:");
        assert_eq!(results[0].path, obj, "obj/ must be reported:");
        assert_eq!(results[0].marker, "package.json");
    }

    #[test]
    fn depth_zero_root_still_walked() {
        // The workspace root itself (depth 0) is not pruned even if its
        // basename matches an artifact name. A workspace root literally
        // named `target` is still walked so the user sees the error.
        let root = tmp_root("d0").join("target");
        fs::create_dir_all(&root).unwrap();
        git_init(&root);
        mkfixture(&root, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, root);
        assert_eq!(results[0].marker, ".git");
    }

    #[test]
    fn sibling_project_still_found() {
        // A real project sitting as a sibling of a pruned dir is still found.
        // root is the workspace (NOT a git repo); root/myproject/ is a git
        // project with node_modules/ (pruned); root/subproject/ is a sibling
        // project with no git ancestor so the nesting rule does not suppress it.
        let root = tmp_root("sibling");
        fs::create_dir_all(&root).unwrap();
        // NOTE: do NOT git_init(root) — root is the plain workspace root.
        let myproject = root.join("myproject");
        fs::create_dir_all(&myproject).unwrap();
        git_init(&myproject);
        let nm = myproject.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        let pkg = nm.join("react");
        fs::create_dir_all(&pkg).unwrap();
        mkfixture(&pkg, "package.json", "{}");
        let sub = root.join("subproject");
        fs::create_dir_all(&sub).unwrap();
        mkfixture(&sub, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml", "package.json"], 3);
        let results = discover(&cfg).unwrap();
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(paths.contains(&myproject), "myproject must be reported:");
        assert!(paths.contains(&sub), "subproject must be reported:");
        assert!(
            !paths.contains(&pkg),
            "node_modules/react must not be reported:"
        );
    }

    // -----------------------------------------------------------------------
    // discover_single: single-project targeting for `offcut clean <path>`
    // -----------------------------------------------------------------------

    #[test]
    fn discover_single_returns_one_project_with_marker() {
        let root = tmp_root("single_marker");
        git_init(&root);
        mkfixture(&root, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 2);
        let result = discover_single(&root, &cfg).unwrap();
        assert_eq!(result.path, std::fs::canonicalize(&root).unwrap());
        // .git wins over Cargo.toml per the marker precedence rule.
        assert_eq!(result.marker, ".git");
    }

    #[test]
    fn discover_single_returns_none_marker_for_unmarked_dir() {
        let root = tmp_root("single_unmarked");
        fs::create_dir_all(&root).unwrap();
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 2);
        let result = discover_single(&root, &cfg).unwrap();
        assert_eq!(result.marker, "(none)");
        assert!(result.path.is_absolute());
    }

    #[test]
    fn discover_single_errors_on_missing_path() {
        let root = tmp_root("single_missing");
        let cfg = mkconfig(&root, &[".git"], 2);
        let missing = root.join("does-not-exist");
        assert!(discover_single(&missing, &cfg).is_err());
    }

    #[test]
    fn discover_single_errors_on_file_not_directory() {
        let root = tmp_root("single_file");
        let file = root.join("not-a-dir.txt");
        fs::write(&file, "hi").unwrap();
        let cfg = mkconfig(&root, &[".git"], 2);
        assert!(discover_single(&file, &cfg).is_err());
    }

    // Relative-path resolution is covered end-to-end by
    // `clean_project_path_relative_resolves_against_cwd` in
    // `tests/clean_cli.rs`, which sets the *child process's* cwd. A unit test
    // would have to mutate this process's global cwd while the harness runs
    // other tests in parallel.

    #[test]
    fn discover_single_rejects_subdirectory_of_git_project() {
        let root = tmp_root("single_subdir");
        git_init(&root);
        let sub = root.join("crates").join("inner");
        mkfixture(&sub, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 3);
        let err = discover_single(&sub, &cfg).unwrap_err().to_string();
        assert!(
            err.contains("not a project root"),
            "error should say the path is not a project root: {err}"
        );
        assert!(
            err.contains(
                std::fs::canonicalize(&root)
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            ),
            "error should name the enclosing project root: {err}"
        );
    }

    #[test]
    fn discover_single_accepts_nested_git_project_root() {
        let outer = tmp_root("single_nested");
        git_init(&outer);
        let inner = outer.join("vendor").join("lib");
        fs::create_dir_all(&inner).unwrap();
        git_init(&inner);
        let cfg = mkconfig(&outer, &[".git"], 3);
        // The inner repo is its own worktree root, so it is a valid target
        // even though the walk would have suppressed it as nested.
        let result = discover_single(&inner, &cfg).unwrap();
        assert_eq!(result.path, std::fs::canonicalize(&inner).unwrap());
        assert_eq!(result.marker, ".git");
    }

    // -----------------------------------------------------------------------
    // normalized_absolute: the canonicalize-failure fallback. Pure/unit-tested
    // directly so the comparison invariant is protected without an
    // unportable filesystem-failure fixture. End-to-end relative-path
    // behavior is still covered by
    // `clean_project_path_relative_resolves_against_cwd` in
    // `tests/clean_cli.rs` (subprocess cwd, no global-cwd mutation).
    // -----------------------------------------------------------------------

    #[test]
    fn normalized_absolute_joins_relative_path_against_cwd() {
        let cwd = PathBuf::from("/home/me/work");
        let got = normalized_absolute(Path::new("proj"), &cwd);
        assert_eq!(got, PathBuf::from("/home/me/work/proj"));
    }

    #[test]
    fn normalized_absolute_collapses_curdir_components() {
        let cwd = PathBuf::from("/home/me/work");
        // `./proj` must collapse to the same form as `proj` so the
        // enclosing-git-root comparison is not tripped up by a `.` segment.
        let got = normalized_absolute(Path::new("./proj"), &cwd);
        assert_eq!(got, PathBuf::from("/home/me/work/proj"));
        // A bare `.` collapses to the cwd itself.
        assert_eq!(normalized_absolute(Path::new("."), &cwd), cwd);
    }

    #[test]
    fn normalized_absolute_resolves_parent_dir_components() {
        let cwd = PathBuf::from("/home/me/work/a/b");
        let got = normalized_absolute(Path::new("../../proj"), &cwd);
        assert_eq!(got, PathBuf::from("/home/me/work/proj"));
    }

    #[test]
    fn normalized_absolute_drops_trailing_slash() {
        let cwd = PathBuf::from("/home/me/work");
        // A trailing slash (e.g. from `./my-project/`) must not survive —
        // `git rev-parse --show-toplevel` never emits one, so a trailing
        // slash on the fallback side would false-reject the root.
        let got = normalized_absolute(Path::new("proj/"), &cwd);
        assert_eq!(got, PathBuf::from("/home/me/work/proj"));
    }

    #[test]
    fn normalized_absolute_keeps_absolute_path_and_normalizes() {
        let cwd = PathBuf::from("/irrelevant");
        // An absolute input ignores `cwd` and normalizes in place.
        let got = normalized_absolute(Path::new("/srv/apps/./looper/../looper"), &cwd);
        assert_eq!(got, PathBuf::from("/srv/apps/looper"));
    }

    #[test]
    fn normalized_absolute_does_not_pop_past_root() {
        let cwd = PathBuf::from("/home/me");
        // Over-normalized `..` past the root stays at the root rather than
        // producing an invalid empty path.
        let got = normalized_absolute(Path::new("../../../../proj"), &cwd);
        assert_eq!(got, PathBuf::from("/proj"));
    }

    /// The invariant the fallback exists for: whatever shape it produces for
    /// a relative target must compare *equal* to `enclosing_git_root`'s
    /// output for that same project root, or `discover_single` would reject
    /// a real project root as "not a project root". Asserted against a real
    /// repo and real `git rev-parse --show-toplevel` output — the shapes the
    /// other `normalized_absolute` tests only describe literally. The final
    /// assertion pins the regression itself: the un-normalized relative path
    /// (what the fallback used to yield) does not compare equal, so it would
    /// have false-rejected.
    #[test]
    fn normalized_absolute_fallback_compares_equal_to_enclosing_git_root() {
        let root = tmp_root("single_fallback_cmp");
        git_init(&root);
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let parent = canonical_root.parent().unwrap().to_path_buf();
        let leaf = PathBuf::from(canonical_root.file_name().unwrap());
        let git_root = enclosing_git_root(&canonical_root).expect("repo has a git root");

        // Bare relative name resolved against the cwd, as `discover_single`
        // does on the canonicalize-failure branch.
        assert_eq!(normalized_absolute(&leaf, &parent), git_root);
        // The `./name/` form a shell tab-completion produces resolves the
        // same way — a stray `.` or trailing slash must not break equality.
        let dotted = PathBuf::from(format!("./{}/", leaf.display()));
        assert_eq!(normalized_absolute(&dotted, &parent), git_root);
        // Pre-fallback shape: the relative path left as typed. Unequal to
        // git's always-absolute answer — the false rejection being prevented.
        assert_ne!(leaf, git_root);
    }

    // Windows drive-prefix preservation: a `RootDir` component must not
    // discard a `Prefix` (e.g. `C:`) already accumulated in `out`. On Unix
    // `Path::new("C:\\foo")` parses as a single `Normal` component, not a
    // `Prefix`, so this code path is Windows-only and the test is gated
    // accordingly — it documents and pins the behavior for Windows builds.
    #[cfg(windows)]
    #[test]
    fn normalized_absolute_preserves_windows_drive_prefix() {
        let cwd = PathBuf::from("D:\\cwd");
        // `C:\proj` is absolute; the `C:` prefix must survive the
        // `RootDir` component so the result is `C:\proj`, not `\\proj`.
        let got = normalized_absolute(Path::new("C:\\proj"), &cwd);
        assert_eq!(got, PathBuf::from("C:\\proj"));
        // A drive-relative `\\proj` against a `C:` cwd base preserves the
        // inherited `C:` prefix too.
        let got = normalized_absolute(Path::new("\\proj"), &PathBuf::from("C:\\base"));
        assert_eq!(got, PathBuf::from("C:\\proj"));
    }
}
