//! Integration tests for the `devclean discovery` subcommand, which is the
//! observable hook for the project-discovery walker (issue #5). The walker
//! itself is unit-tested in `src/discovery.rs`; these tests exercise the full
//! CLI path: a real config file naming real workspace roots on disk, walked by
//! the real binary, with the printed report as the assertion surface.
//!
//! They use an explicit `HOME` override so the child never picks up the
//! developer's real `~/.config/devclean/config.toml`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    let mut d = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    d.push(format!(
        "devclean-discovery-cli-{}-{}-{}",
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

/// Build a workspace fixture that covers every built-in marker kind, a folder
/// with no marker at all, a nested project, and a project past `max_depth`.
fn workspace_fixture(label: &str) -> PathBuf {
    let ws = unique_dir(label);
    write_file(&ws, "web-app/package.json", "{}");
    std::fs::create_dir_all(ws.join("web-app/.git")).unwrap();
    write_file(&ws, "web-app/packages/ui/package.json", "{}");
    write_file(&ws, "rust-tool/Cargo.toml", "[package]");
    write_file(&ws, "api-service/go.mod", "module api");
    write_file(&ws, "ml-notebooks/pyproject.toml", "[project]");
    write_file(&ws, "legacy-java/pom.xml", "<project/>");
    write_file(&ws, "android-app/build.gradle", "apply plugin");
    write_file(&ws, "dotnet-svc/Svc.csproj", "<Project/>");
    write_file(&ws, "just-notes/README.md", "no marker here");
    write_file(&ws, "deep/a/b/c/too-far/Cargo.toml", "[package]");
    ws
}

/// Write a config naming `roots` as the workspace roots, and run
/// `devclean --config <cfg> discovery`. Returns (success, stdout+stderr).
fn run_discovery(roots: &[&Path], max_depth: usize) -> (bool, String) {
    let home = unique_dir("home");
    let cfg_dir = unique_dir("cfg");
    let cfg = cfg_dir.join("config.toml");
    let root_list = roots
        .iter()
        .map(|r| format!("{:?}", r.display().to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    write_file(
        &cfg_dir,
        "config.toml",
        &format!("workspace_roots = [{root_list}]\nmax_depth = {max_depth}\n"),
    );

    let out = Command::new(env!("CARGO_BIN_EXE_devclean"))
        .env("HOME", &home)
        .arg("--config")
        .arg(&cfg)
        .arg("discovery")
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// The reported project paths, in the order the CLI printed them.
fn reported_paths(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter_map(|l| l.rsplit_once(" ("))
        .map(|(path, _)| path.to_string())
        .collect()
}

/// The marker label the CLI attributed to `path`.
fn marker_for(output: &str, path: &Path) -> Option<String> {
    let want = path.display().to_string();
    output
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter_map(|l| l.rsplit_once(" ("))
        .find(|(p, _)| *p == want)
        .map(|(_, m)| m.trim_end_matches(')').to_string())
}

#[test]
fn discovery_reports_every_builtin_marker_kind() {
    let ws = workspace_fixture("markers");
    let (ok, out) = run_discovery(&[&ws], 4);
    assert!(ok, "discovery failed: {out}");

    // One project per marker family, including the `*.csproj` glob marker.
    for project in [
        "web-app",
        "web-app/packages/ui",
        "rust-tool",
        "api-service",
        "ml-notebooks",
        "legacy-java",
        "android-app",
        "dotnet-svc",
    ] {
        let want = ws.join(project).display().to_string();
        assert!(
            reported_paths(&out).contains(&want),
            "expected {project} in output:\n{out}"
        );
    }
    assert_eq!(
        marker_for(&out, &ws.join("dotnet-svc")).as_deref(),
        Some("*.csproj"),
        "glob marker should be labelled by its pattern:\n{out}"
    );
}

#[test]
fn discovery_skips_folders_without_a_marker() {
    let ws = workspace_fixture("plain");
    let (ok, out) = run_discovery(&[&ws], 4);
    assert!(ok, "discovery failed: {out}");
    let unwanted = ws.join("just-notes").display().to_string();
    assert!(
        !reported_paths(&out).contains(&unwanted),
        "a folder with no marker must not be reported:\n{out}"
    );
}

#[test]
fn discovery_reports_nested_projects_separately_without_double_counting() {
    let ws = workspace_fixture("nested");
    let (ok, out) = run_discovery(&[&ws.join("web-app")], 4);
    assert!(ok, "discovery failed: {out}");

    let paths = reported_paths(&out);
    let parent = ws.join("web-app").display().to_string();
    let nested = ws.join("web-app/packages/ui").display().to_string();

    // Both are reported: no parent suppression.
    assert!(paths.contains(&parent), "parent missing:\n{out}");
    assert!(paths.contains(&nested), "nested project missing:\n{out}");
    // And no double-counting: the parent matches two markers (.git and
    // package.json) but is still listed exactly once.
    assert_eq!(paths.iter().filter(|p| **p == parent).count(), 1);
    assert_eq!(paths.iter().filter(|p| **p == nested).count(), 1);
    assert_eq!(paths.len(), 2, "unexpected extra entries:\n{out}");
}

#[test]
fn discovery_honors_max_depth_and_never_walks_above_the_root() {
    let ws = workspace_fixture("depth");
    let too_far = ws.join("deep/a/b/c/too-far").display().to_string();

    // `too-far` sits at depth 5; the default depth of 4 must exclude it.
    let (ok, shallow) = run_discovery(&[&ws], 4);
    assert!(ok, "discovery failed: {shallow}");
    assert!(
        !reported_paths(&shallow).contains(&too_far),
        "depth-5 project leaked past max_depth=4:\n{shallow}"
    );

    // Raising the limit brings it in, proving the exclusion was the depth
    // limit and not a marker-matching failure.
    let (ok, deep) = run_discovery(&[&ws], 5);
    assert!(ok, "discovery failed: {deep}");
    assert!(
        reported_paths(&deep).contains(&too_far),
        "depth-5 project missing at max_depth=5:\n{deep}"
    );

    // Rooted at one project, siblings above/beside it are never visited.
    let (ok, scoped) = run_discovery(&[&ws.join("rust-tool")], 4);
    assert!(ok, "discovery failed: {scoped}");
    assert_eq!(
        reported_paths(&scoped),
        vec![ws.join("rust-tool").display().to_string()],
        "walk escaped its workspace root:\n{scoped}"
    );
}

#[test]
fn discovery_walks_multiple_workspace_roots_independently() {
    let ws = workspace_fixture("multiroot");
    let (ok, out) = run_discovery(&[&ws.join("rust-tool"), &ws.join("api-service")], 4);
    assert!(ok, "discovery failed: {out}");
    let mut paths = reported_paths(&out);
    paths.sort();
    let mut want = vec![
        ws.join("rust-tool").display().to_string(),
        ws.join("api-service").display().to_string(),
    ];
    want.sort();
    assert_eq!(paths, want, "unexpected roots walked:\n{out}");
}

#[test]
fn discovery_with_no_workspace_roots_reports_nothing_and_succeeds() {
    let (ok, out) = run_discovery(&[], 4);
    assert!(ok, "empty roots should not be an error: {out}");
    assert!(
        out.contains("no workspace roots configured"),
        "expected an explanatory message:\n{out}"
    );
    assert!(
        reported_paths(&out).is_empty(),
        "unexpected projects:\n{out}"
    );
}

#[test]
fn discovery_with_a_missing_workspace_root_is_an_error() {
    let ws = workspace_fixture("missing");
    let (ok, out) = run_discovery(&[&ws.join("does-not-exist")], 4);
    assert!(!ok, "a bogus workspace root must fail loudly:\n{out}");
    assert!(
        out.contains("workspace root not found"),
        "expected a diagnostic naming the bad root:\n{out}"
    );
}

#[test]
fn discovery_honors_workspace_roots_supplied_on_the_cli() {
    // Regression: `discovery` used to load config but never apply the CLI
    // overrides, so `--workspace` was silently dropped and the command
    // exited 0 having walked nothing. The config here names no roots, so
    // every reported project must have come from the flag.
    let ws = workspace_fixture("cli-override");
    let home = unique_dir("home");
    let cfg_dir = unique_dir("cfg");
    write_file(
        &cfg_dir,
        "config.toml",
        "workspace_roots = []\nmax_depth = 4\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_devclean"))
        .env("HOME", &home)
        .arg("--config")
        .arg(cfg_dir.join("config.toml"))
        .arg("--workspace")
        .arg(ws.join("rust-tool"))
        .arg("discovery")
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));

    assert!(out.status.success(), "discovery failed: {text}");
    assert_eq!(
        reported_paths(&text),
        vec![ws.join("rust-tool").display().to_string()],
        "--workspace must be honored, not silently ignored:\n{text}"
    );
}
