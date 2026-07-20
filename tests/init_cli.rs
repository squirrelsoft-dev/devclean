/// Integration tests for `offcut init`.
///
/// Each test spins up a fresh temp config dir, runs the binary against it, and
/// asserts the resulting file exists, parses, and behaves as expected.
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn init_creates_config_file_at_target() {
    let dir = std::env::temp_dir();
    let n: u64 = COUNTER.fetch_add(1, Ordering::SeqCst);
    let config_dir = dir.join(format!("offcut-init-cli-test-{}-{}", std::process::id(), n));
    let config_path = config_dir.join("offcut").join("config.toml");
    let workspace = "/tmp/cli-test-workspace";

    let out = Command::new(env!("CARGO_BIN_EXE_offcut"))
        .arg("--config")
        .arg(&config_path)
        .arg("init")
        .arg(workspace)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "init exited non-zero: {}",
        stderr(&out)
    );
    assert!(config_path.is_file(), "config file not created at target");
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(
        contents.contains("workspace_roots"),
        "template missing workspace_roots line"
    );
    assert!(
        contents.contains("cli-test-workspace"),
        "workspace root not injected into template"
    );
}

#[test]
fn init_exits_nonzero_when_file_already_exists() {
    let dir = std::env::temp_dir();
    let n: u64 = COUNTER.fetch_add(1, Ordering::SeqCst);
    let config_dir = dir.join(format!("offcut-init-cli-test-{}-{}", std::process::id(), n));
    let config_path = config_dir.join("offcut").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(&config_path, "existing = true\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_offcut"))
        .arg("--config")
        .arg(&config_path)
        .arg("init")
        .arg("/tmp/work")
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "init should exit non-zero when file exists"
    );
    let msg = stderr(&out);
    assert!(
        msg.contains("already exists"),
        "stderr must name the existing file: {}",
        msg
    );
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        "existing = true\n"
    );
}

#[test]
fn init_creates_parent_directories() {
    let dir = std::env::temp_dir();
    let n: u64 = COUNTER.fetch_add(1, Ordering::SeqCst);
    let config_dir = dir.join(format!("offcut-init-cli-test-{}-{}", std::process::id(), n));
    let _ = fs::remove_dir_all(&config_dir);

    let config_path = config_dir.join("offcut").join("config.toml");

    let out = Command::new(env!("CARGO_BIN_EXE_offcut"))
        .arg("--config")
        .arg(&config_path)
        .arg("init")
        .arg("/tmp/work")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "init exited non-zero: {}",
        stderr(&out)
    );
    assert!(
        config_path.is_file(),
        "config file not created after parent dirs were made"
    );
}

#[test]
fn init_file_parses_back_to_valid_config() {
    let dir = std::env::temp_dir();
    let n: u64 = COUNTER.fetch_add(1, Ordering::SeqCst);
    let config_dir = dir.join(format!("offcut-init-cli-test-{}-{}", std::process::id(), n));
    let config_path = config_dir.join("offcut").join("config.toml");
    let workspace = "/tmp/cli-test-workspace";

    let out = Command::new(env!("CARGO_BIN_EXE_offcut"))
        .arg("--config")
        .arg(&config_path)
        .arg("init")
        .arg(workspace)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "init exited non-zero: {}",
        stderr(&out)
    );

    let text = fs::read_to_string(&config_path).unwrap();
    let value: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("created file did not parse: {e}\n{text}"));
    let table = value.as_table().expect("top-level must be a table");
    let roots: Vec<&str> = table["workspace_roots"]
        .as_array()
        .expect("workspace_roots must be an array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        roots.iter().any(|r| r.contains("cli-test-workspace")),
        "workspace root not injected"
    );
    assert_eq!(
        table["max_depth"].as_integer().unwrap(),
        4,
        "max_depth must default to 4"
    );
    assert_eq!(
        table["default_mode"].as_str().unwrap(),
        "interactive",
        "default_mode must default to interactive"
    );
}

#[test]
fn init_prints_created_path_to_stdout() {
    let dir = std::env::temp_dir();
    let n: u64 = COUNTER.fetch_add(1, Ordering::SeqCst);
    let config_dir = dir.join(format!("offcut-init-cli-test-{}-{}", std::process::id(), n));
    let config_path = config_dir.join("offcut").join("config.toml");

    let out = Command::new(env!("CARGO_BIN_EXE_offcut"))
        .arg("--config")
        .arg(&config_path)
        .arg("init")
        .arg("/tmp/work")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "init exited non-zero: {}",
        stderr(&out)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("created config file"),
        "stdout must print the created path: {}",
        stdout
    );
    assert!(
        stdout.contains(config_path.to_str().unwrap()),
        "stdout must include the actual path: {}",
        stdout
    );
}
