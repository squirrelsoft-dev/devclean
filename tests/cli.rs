use std::process::Command;

fn devclean() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devclean"))
}

const BANNER: &str = "devclean - development environment cleanup CLI (scaffold)";

#[test]
fn version_flag_prints_crate_version() {
    let out = devclean().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("devclean {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn no_args_prints_placeholder_banner() {
    let out = devclean().output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), BANNER);
}

#[test]
fn help_flag_lists_version_and_help_options() {
    let out = devclean().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(BANNER));
    assert!(stdout.contains("--version"));
    assert!(stdout.contains("--help"));
}

#[test]
fn unknown_argument_is_rejected() {
    let out = devclean().arg("--bogus").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"));
}
