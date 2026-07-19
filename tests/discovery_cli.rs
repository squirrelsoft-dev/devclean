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
    // web-app has a `.git`, so its nested package.json marker is a subfolder
    // of that repo, not a separate project (issue #15).
    let suppressed = ws.join("web-app/packages/ui").display().to_string();
    assert!(
        !reported_paths(&out).contains(&suppressed),
        "nested non-git marker must be suppressed (issue #15):\n{out}"
    );
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
fn discovery_suppresses_nested_non_git_marker_under_git_root() {
    let ws = workspace_fixture("nested");
    let (ok, out) = run_discovery(&[&ws.join("web-app")], 4);
    assert!(ok, "discovery failed: {out}");

    let paths = reported_paths(&out);
    let parent = ws.join("web-app").display().to_string();
    let nested = ws.join("web-app/packages/ui").display().to_string();

    // The root (root has .git) is reported; the nested non-git marker
    // (packages/ui/package.json) is suppressed — it is a subfolder of
    // the parent repo, not a separate project (issue #15).
    assert!(paths.contains(&parent), "parent missing:\n{out}");
    assert!(!paths.contains(&nested), "nested project leaked:\n{out}");
    // And no double-counting: the parent matches two markers (.git and
    // package.json) but is still listed exactly once.
    assert_eq!(paths.iter().filter(|p| **p == parent).count(), 1);
    assert_eq!(paths.len(), 1, "unexpected extra entries:\n{out}");
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

/// Build a fixture that contains a real project alongside a `node_modules`
/// directory with a nested `package.json` — the discovery output should
/// report only the project, not the nested package.
fn artifact_fixture(label: &str) -> PathBuf {
    let ws = unique_dir(label);
    // Real project: git repo with a Cargo.toml.
    std::fs::create_dir_all(ws.join("myproject/.git")).unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(&ws.join("myproject"))
        .arg("init")
        .status()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(&ws.join("myproject"))
        .arg("config")
        .arg("user.email")
        .arg("test@test.dev")
        .status()
        .unwrap();
    std::process::Command::new("git")
        .arg("-C")
        .arg(&ws.join("myproject"))
        .arg("config")
        .arg("user.name")
        .arg("Test")
        .status()
        .unwrap();
    write_file(&ws.join("myproject"), "main.rs", "fn main() {}");
    std::process::Command::new("git")
        .arg("-C")
        .arg(&ws.join("myproject"))
        .arg("add")
        .arg("main.rs")
        .status()
        .unwrap();
    // node_modules with a nested package.json — should NOT be reported.
    let nm = ws.join("node_modules");
    std::fs::create_dir_all(&nm).unwrap();
    let pkg = nm.join("lodash");
    std::fs::create_dir_all(&pkg).unwrap();
    write_file(&pkg, "package.json", "{}");
    ws
}

#[test]
fn discovery_prunes_node_modules_from_descent() {
    // A project whose node_modules contains a nested package.json — only
    // the project is reported, not the nested package. The walker does
    // not descend into node_modules at all.
    let ws = artifact_fixture("artifact");
    let (ok, out) = run_discovery(&[&ws], 3);
    assert!(ok, "discovery failed: {out}");

    let paths = reported_paths(&out);
    let project = ws.join("myproject").display().to_string();
    let nested = ws.join("node_modules/lodash").display().to_string();

    assert!(paths.contains(&project), "project must be reported:\n{out}");
    assert!(
        !paths.contains(&nested),
        "nested package must not be reported:\n{out}"
    );
    assert_eq!(
        paths.len(),
        1,
        "only the project should be reported:\n{out}"
    );
}

#[test]
fn discovery_prunes_target_from_descent() {
    // A Rust project whose target/ contains a nested Cargo.toml — only the
    // project is reported, not the nested Cargo.toml.
    let ws = artifact_fixture("target");
    // Add a target/ with a Cargo.toml inside.
    let target = ws.join("target");
    std::fs::create_dir_all(target.join("debug/deep")).unwrap();
    write_file(&target.join("debug/deep"), "Cargo.toml", "[package]");

    let (ok, out) = run_discovery(&[&ws], 4);
    assert!(ok, "discovery failed: {out}");

    let paths = reported_paths(&out);
    let project = ws.join("myproject").display().to_string();
    let nested = ws.join("target/debug/deep").display().to_string();

    assert!(paths.contains(&project), "project must be reported:\n{out}");
    assert!(
        !paths.contains(&nested),
        "target/debug/deep must not be reported:\n{out}"
    );
}

#[test]
fn discovery_prunes_user_safe_delete_artifact() {
    // A user-added safe_delete pattern prunes the artifact from discovery.
    let ws = artifact_fixture("user_artifact");
    let art = ws.join(".my-artifacts");
    std::fs::create_dir_all(&art).unwrap();
    write_file(&art, "package.json", "{}");

    let home = unique_dir("home2");
    let cfg_dir = unique_dir("cfg2");
    let cfg = cfg_dir.join("config.toml");
    let root_list = format!("{:?}", ws.display().to_string());
    write_file(
        &cfg_dir,
        "config.toml",
        &format!(
            "workspace_roots = [{root_list}]\nmax_depth = 3\nsafe_delete = [\"**/.my-artifacts\"]\n"
        ),
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
    assert!(out.status.success(), "discovery failed: {text}");

    let paths = reported_paths(&text);
    let project = ws.join("myproject").display().to_string();
    let artifact = art.display().to_string();

    assert!(
        paths.contains(&project),
        "project must be reported:\n{text}"
    );
    assert!(
        !paths.contains(&artifact),
        ".my-artifacts must not be reported:\n{text}"
    );
}
