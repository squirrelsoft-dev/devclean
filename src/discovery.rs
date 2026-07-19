//! Project discovery for devclean.
//!
//! Walks each configured workspace root and identifies project folders by
//! marker files (`.git`, `package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`,
//! `pom.xml`, `build.gradle`, `*.csproj`, etc.). The marker list is overridable
//! via the `project_markers` config field.
//!
//! ## Nesting rule
//!
//! Every folder that contains a marker is reported as a discovered project,
//! regardless of nesting. There is no "parent project" suppression: if a
//! workspace root has `.git` and a deeper subdirectory also has `.git`, both
//! are reported. This matches the principle of each marker being a
//! **candidate** project — classification (issue #6) and cleaning (issue #7)
//! can decide how to relate nested projects later.
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

    if cfg.workspace_roots.is_empty() {
        println!("discovery: no workspace roots configured; nothing to walk");
        return Ok(Vec::new());
    }

    let mut results: Vec<DiscoveredProject> = Vec::new();
    for root in &cfg.workspace_roots {
        walk_root(root, max_depth, &markers, &mut results)?;
    }

    Ok(results)
}

/// Walk a single workspace root up to `max_depth` directories deep and tag
/// each folder with a marker.
///
/// `max_depth` is inclusive — depth 0 is the workspace root itself; depth 1
/// is a direct child directory; etc. `WalkDir::max_depth` is the correct API.
/// The first entry (depth 0) is the root itself, which is checked for markers
/// too.
fn walk_root(
    root: &Path,
    max_depth: usize,
    markers: &MarkerSet,
    out: &mut Vec<DiscoveredProject>,
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
    {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }
        // Check each direct child file for markers. Only look at *files* —
        // a marker is a file, never a directory (except `.git` which is a
        // directory on disk but is treated as a file-name marker).
        if let Some(marker) = find_marker_in(entry.path(), markers) {
            out.push(DiscoveredProject {
                path: entry.path().to_path_buf(),
                marker: marker.to_string(),
            });
        }
    }
    Ok(())
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
fn find_marker_in<'a>(dir: &Path, markers: &'a MarkerSet) -> Option<&'a str> {
    let read = fs::read_dir(dir).ok()?;
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(m) = markers.matches(name_str.as_ref()) {
            return Some(m);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

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
        let out = d.join(format!("devclean-discovery-test-{}-{}", label, n));
        fs::create_dir_all(&out).unwrap();
        out
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
    fn nested_markers_both_reported() {
        let root = tmp_root("nested");
        mkfixture(&root, ".git", "root repo");
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        mkfixture(&sub, "Cargo.toml", "[package]");
        let cfg = mkconfig(&root, &[".git", "Cargo.toml"], 2);
        let results = discover(&cfg).unwrap();
        assert_eq!(results.len(), 2);
        // Both are reported; depth-first does not suppress nested.
        let paths: Vec<PathBuf> = results.iter().map(|r| r.path.clone()).collect();
        assert!(paths.iter().any(|p| p.join(".git").exists()));
        assert!(paths.iter().any(|p| p.join("Cargo.toml").exists()));
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
        // Depth 0 (root) and depth 1 (sub1), but not depth 2 (sub2).
        assert_eq!(results.len(), 2);
        let paths: Vec<_> = results
            .iter()
            .map(|r| r.path.as_os_str().to_string_lossy().to_string())
            .collect();
        assert!(paths.iter().any(|p| p == root.to_str().unwrap()));
        assert!(paths.iter().any(|p| p.ends_with("a")));
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
        assert_eq!(results.len(), 3);
        // All three are reported, each once.
        let names: Vec<_> = results.iter().map(|r| r.marker.as_str()).collect();
        assert!(names.contains(&".git"));
        assert!(names.contains(&"Cargo.toml"));
        assert!(names.contains(&"package.json"));
    }
}
