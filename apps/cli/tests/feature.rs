use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::{Command, Stdio};

use serde::Deserialize;

mod support;

use support::{ManagedStateFixture, binary, dof, mode, output, set_mode, stderr, stdout};

type Fixture = ManagedStateFixture;

#[test]
fn feature_help_describes_nested_commands_and_rejects_invalid_arguments() {
    let help = output(Command::new(binary()).args(["feature", "--help"]));
    assert!(help.status.success(), "{}", stderr(&help));
    let help = stdout(&help);
    assert!(help.contains("Usage: dof feature <COMMAND>"));
    assert!(help.contains("enable"));
    assert!(help.contains("disable"));

    for action in ["enable", "disable"] {
        let help = output(Command::new(binary()).args(["feature", action, "--help"]));
        assert!(help.status.success(), "{}", stderr(&help));
        assert!(
            stdout(&help).contains(&format!("Usage: dof feature {action} <FEATURE>")),
            "unexpected {action} help: {}",
            stdout(&help)
        );

        let missing = output(Command::new(binary()).args(["feature", action]));
        assert!(!missing.status.success());
        assert!(stderr(&missing).contains("<FEATURE>"));

        let extra = output(Command::new(binary()).args(["feature", action, "default", "extra"]));
        assert!(!extra.status.success());
        assert!(stderr(&extra).contains("unexpected argument 'extra'"));
    }

    let missing_subcommand = output(Command::new(binary()).arg("feature"));
    assert!(!missing_subcommand.status.success());
    assert!(stderr(&missing_subcommand).contains("subcommand"));
}

#[test]
fn enable_and_disable_overwrite_explicit_values_for_real_features() {
    let fixture = Fixture::new();
    fixture.create_feature("default");
    fixture.create_feature("laptop");
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: custom
features:
  default: false
  laptop: true
"#,
    );

    let enabled = output(dof(&fixture.home).args(["feature", "enable", "default"]));
    assert!(enabled.status.success(), "{}", stderr(&enabled));
    assert!(stdout(&enabled).is_empty());
    let config: Config = fixture.read_config();
    assert_eq!(config.repo.url, "file:///dotfiles");
    assert_eq!(config.repo.branch, "custom");
    assert_eq!(config.features.get("default"), Some(&true));
    assert_eq!(config.features.get("laptop"), Some(&true));

    let disabled = output(dof(&fixture.home).args(["feature", "disable", "laptop"]));
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    assert!(stdout(&disabled).is_empty());
    let config: Config = fixture.read_config();
    assert_eq!(config.features.get("default"), Some(&true));
    assert_eq!(config.features.get("laptop"), Some(&false));
}

#[test]
fn omitted_features_map_is_created_and_config_mode_is_preserved() {
    let fixture = Fixture::new();
    fixture.create_feature("work");
    fixture.write_config(
        r#"# Formatting and comments need not survive a successful update.
repo:
  url: file:///dotfiles
  branch: main
"#,
    );
    set_mode(&fixture.config, 0o600);

    let disabled = output(dof(&fixture.home).args(["feature", "disable", "work"]));
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    assert_eq!(mode(&fixture.config), 0o600);
    assert_eq!(
        fixture.read_config::<Config>().features.get("work"),
        Some(&false)
    );

    let enabled = output(dof(&fixture.home).args(["feature", "enable", "work"]));
    assert!(enabled.status.success(), "{}", stderr(&enabled));
    assert_eq!(mode(&fixture.config), 0o600);
    assert_eq!(
        fixture.read_config::<Config>().features.get("work"),
        Some(&true)
    );
}

#[test]
fn unknown_config_fields_survive_a_semantic_rewrite() {
    let fixture = Fixture::new();
    fixture.create_feature("default");
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
  fetch-depth: 7
features:
  default: true
future-config:
  strategy: layered
  priorities:
    - local
    - upstream
"#,
    );

    let result = output(dof(&fixture.home).args(["feature", "disable", "default"]));

    assert!(result.status.success(), "{}", stderr(&result));
    let config: ExtendedConfig =
        serde_yaml_ng::from_str(&fs::read_to_string(&fixture.config).unwrap()).unwrap();
    assert_eq!(config.repo.fetch_depth, 7);
    assert_eq!(config.features.get("default"), Some(&false));
    assert_eq!(
        config.future_config,
        FutureConfig {
            strategy: "layered".to_owned(),
            priorities: vec!["local".to_owned(), "upstream".to_owned()],
        }
    );
}

#[test]
fn unknown_files_and_symlinks_are_rejected_without_changing_config() {
    for candidate in ["missing", "plain-file", "linked-directory"] {
        let fixture = Fixture::new();
        fixture.write_config(
            r#"repo:
  url: file:///dotfiles
  branch: main
features:
  existing: false
"#,
        );
        match candidate {
            "plain-file" => {
                fs::write(fixture.workspace.join(candidate), "not a feature\n").unwrap();
            }
            "linked-directory" => {
                let external = fixture.root.path().join("external-feature");
                fs::create_dir(&external).unwrap();
                symlink(external, fixture.workspace.join(candidate)).unwrap();
            }
            "missing" => {}
            _ => unreachable!(),
        }
        let before = fs::read(&fixture.config).unwrap();

        let result = output(dof(&fixture.home).args(["feature", "enable", candidate]));

        assert!(
            !result.status.success(),
            "{candidate} unexpectedly succeeded"
        );
        assert!(
            stderr(&result).contains("does not exist")
                || stderr(&result).contains("not a feature")
                || stderr(&result).contains("not found"),
            "unexpected error for {candidate}: {}",
            stderr(&result)
        );
        assert_eq!(fs::read(&fixture.config).unwrap(), before);
    }
}

#[test]
fn invalid_home_config_and_workspace_fail_without_rewriting_config() {
    let missing_home = output(
        Command::new(binary())
            .env_remove("HOME")
            .args(["feature", "enable", "default"]),
    );
    assert!(!missing_home.status.success());
    assert!(stderr(&missing_home).contains("HOME is not set or is empty"));

    let missing_config = Fixture::new();
    missing_config.create_feature("default");
    let result = output(dof(&missing_config.home).args(["feature", "enable", "default"]));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to read dof config"));

    let malformed = Fixture::new();
    malformed.create_feature("default");
    malformed.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features: [not, an, object]
"#,
    );
    let malformed_before = fs::read(&malformed.config).unwrap();
    let result = output(dof(&malformed.home).args(["feature", "disable", "default"]));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to parse dof config"));
    assert_eq!(fs::read(&malformed.config).unwrap(), malformed_before);

    let missing_workspace = Fixture::new();
    missing_workspace.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features: {}
"#,
    );
    fs::remove_dir(&missing_workspace.workspace).unwrap();
    let workspace_before = fs::read(&missing_workspace.config).unwrap();
    let result = output(dof(&missing_workspace.home).args(["feature", "enable", "default"]));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("workspace"));
    assert_eq!(
        fs::read(&missing_workspace.config).unwrap(),
        workspace_before
    );
}

#[test]
fn managed_state_symlinks_are_rejected_without_touching_their_targets() {
    let state_fixture = tempfile::tempdir().unwrap();
    let home = state_fixture.path().join("home");
    let external_state = state_fixture.path().join("external-state");
    fs::create_dir(&home).unwrap();
    fs::create_dir_all(external_state.join("workspace/default")).unwrap();
    let external_config = external_state.join("config.yaml");
    fs::write(
        &external_config,
        "repo:\n  url: file:///dotfiles\n  branch: main\nfeatures: {}\n",
    )
    .unwrap();
    symlink(&external_state, home.join(".dof")).unwrap();
    let before = fs::read(&external_config).unwrap();

    let result = output(dof(&home).args(["feature", "disable", "default"]));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("not a real directory"));
    assert_eq!(fs::read(&external_config).unwrap(), before);

    let workspace_fixture = Fixture::new();
    workspace_fixture
        .write_config("repo:\n  url: file:///dotfiles\n  branch: main\nfeatures: {}\n");
    fs::remove_dir(&workspace_fixture.workspace).unwrap();
    let external_workspace = workspace_fixture.root.path().join("external-workspace");
    fs::create_dir_all(external_workspace.join("default")).unwrap();
    symlink(&external_workspace, &workspace_fixture.workspace).unwrap();
    let before = fs::read(&workspace_fixture.config).unwrap();

    let result = output(dof(&workspace_fixture.home).args(["feature", "disable", "default"]));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("not a real directory"));
    assert_eq!(fs::read(&workspace_fixture.config).unwrap(), before);
    assert!(external_workspace.join("default").is_dir());

    let config_fixture = Fixture::new();
    config_fixture.create_feature("default");
    let external_config = config_fixture.root.path().join("external-config.yaml");
    fs::write(
        &external_config,
        "repo:\n  url: file:///dotfiles\n  branch: main\nfeatures: {}\n",
    )
    .unwrap();
    symlink(&external_config, &config_fixture.config).unwrap();
    let before = fs::read(&external_config).unwrap();

    let result = output(dof(&config_fixture.home).args(["feature", "disable", "default"]));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("not a real file"));
    assert_eq!(fs::read(&external_config).unwrap(), before);
    assert!(
        fs::symlink_metadata(&config_fixture.config)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn concurrent_feature_updates_do_not_overwrite_each_other() {
    let fixture = Fixture::new();
    fixture.write_config("repo:\n  url: file:///dotfiles\n  branch: main\nfeatures: {}\n");
    let names = (0..12)
        .map(|index| format!("feature-{index:02}"))
        .collect::<Vec<_>>();
    for name in &names {
        fixture.create_feature(name);
    }

    let children = names
        .iter()
        .map(|name| {
            let mut command = dof(&fixture.home);
            command
                .args(["feature", "disable", name])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.spawn().unwrap()
        })
        .collect::<Vec<_>>();

    for child in children {
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success(), "{}", stderr(&result));
    }

    let config: Config = fixture.read_config();
    for name in names {
        assert_eq!(config.features.get(&name), Some(&false), "missing {name}");
    }
}

#[derive(Debug, Deserialize)]
struct Config {
    repo: Repo,
    #[serde(default)]
    features: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize)]
struct Repo {
    url: String,
    branch: String,
}

#[derive(Debug, Deserialize)]
struct ExtendedConfig {
    repo: ExtendedRepo,
    features: BTreeMap<String, bool>,
    #[serde(rename = "future-config")]
    future_config: FutureConfig,
}

#[derive(Debug, Deserialize)]
struct ExtendedRepo {
    #[serde(rename = "fetch-depth")]
    fetch_depth: u64,
}

#[derive(Debug, Deserialize, PartialEq)]
struct FutureConfig {
    strategy: String,
    priorities: Vec<String>,
}
