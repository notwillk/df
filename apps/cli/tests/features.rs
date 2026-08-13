use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;
use std::process::Command;

mod support;

use support::{ManagedStateFixture, binary, dof, output, stderr, stdout};

type Fixture = ManagedStateFixture;

#[test]
fn features_help_describes_json_and_rejects_arguments() {
    let help = output(Command::new(binary()).args(["features", "--help"]));
    assert!(help.status.success(), "{}", stderr(&help));
    let help = stdout(&help);
    assert!(help.contains("Usage: dof features [OPTIONS]"));
    assert!(help.contains("--json"));

    let extra = output(Command::new(binary()).args(["features", "default"]));
    assert!(!extra.status.success());
    assert!(stderr(&extra).contains("unexpected argument 'default'"));
}

#[test]
fn omitted_feature_settings_enable_sorted_workspace_directories() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
"#,
    );
    for feature in ["zeta", "default", "alpha"] {
        fs::create_dir(fixture.workspace.join(feature)).unwrap();
    }
    fs::create_dir(fixture.workspace.join(".git")).unwrap();
    fs::create_dir_all(fixture.workspace.join("alpha/nested")).unwrap();
    fs::write(fixture.workspace.join("README.md"), "not a feature\n").unwrap();

    let external = fixture.root.path().join("external-feature");
    fs::create_dir(&external).unwrap();
    symlink(&external, fixture.workspace.join("linked")).unwrap();
    symlink(
        fixture.root.path().join("missing-feature"),
        fixture.workspace.join("dangling"),
    )
    .unwrap();

    let result = output(dof(&fixture.home).arg("features"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(stdout(&result), "alpha\ndefault\nzeta\n");
}

#[test]
fn false_disables_a_feature_and_json_is_sorted() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  default: false
  laptop: true
  ghost: true
"#,
    );
    for feature in ["work", "default", "laptop"] {
        fs::create_dir(fixture.workspace.join(feature)).unwrap();
    }

    let result = output(dof(&fixture.home).args(["features", "--json"]));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(stdout(&result), "[\"laptop\",\"work\"]\n");
}

#[test]
fn empty_workspace_has_empty_text_and_json_output() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features: {}
"#,
    );

    let text = output(dof(&fixture.home).arg("features"));
    assert!(text.status.success(), "{}", stderr(&text));
    assert!(stdout(&text).is_empty());

    let json = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(json.status.success(), "{}", stderr(&json));
    assert_eq!(stdout(&json), "[]\n");
}

#[test]
fn invalid_state_fails_clearly() {
    let missing_home = output(Command::new(binary()).env_remove("HOME").arg("features"));
    assert!(!missing_home.status.success());
    assert!(stderr(&missing_home).contains("HOME is not set or is empty"));

    let missing_config = Fixture::new();
    let result = output(dof(&missing_config.home).arg("features"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to read dof config"));

    let malformed = Fixture::new();
    malformed.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  default: sometimes
"#,
    );
    let result = output(dof(&malformed.home).arg("features"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to parse dof config"));

    let non_directory = Fixture::new();
    non_directory.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
"#,
    );
    fs::remove_dir(&non_directory.workspace).unwrap();
    fs::write(&non_directory.workspace, "not a directory\n").unwrap();
    let result = output(dof(&non_directory.home).arg("features"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("is not a real directory"));
}

#[test]
fn symlinked_state_directory_is_rejected_without_following_it() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let external_state = fixture.path().join("external-state");
    fs::create_dir_all(external_state.join("workspace/default")).unwrap();
    fs::write(
        external_state.join("config.yaml"),
        "repo:\n  url: file:///dotfiles\n  branch: main\n",
    )
    .unwrap();
    fs::create_dir(&home).unwrap();
    symlink(&external_state, home.join(".dof")).unwrap();

    let result = output(dof(&home).arg("features"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("is not a real directory"));
    assert!(external_state.join("workspace/default").is_dir());
    assert!(external_state.join("config.yaml").is_file());
}

// macOS filesystems reject this byte sequence before dof can inspect it.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_feature_name_fails_clearly() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
"#,
    );
    let name = std::ffi::OsString::from_vec(vec![b'f', 0x80]);
    fs::create_dir(fixture.workspace.join(name)).unwrap();

    let result = output(dof(&fixture.home).arg("features"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("feature directory name is not valid UTF-8"));
}

#[test]
fn feature_name_with_control_characters_fails_clearly() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
"#,
    );
    fs::create_dir(fixture.workspace.join("first\nsecond")).unwrap();

    let result = output(dof(&fixture.home).arg("features"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("feature directory name contains control characters"));
}
