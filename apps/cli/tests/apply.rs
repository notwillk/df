use std::fs;
use std::ops::Deref;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;
use std::process::{Command, Output};

mod support;
use support::{
    ManagedStateFixture, binary, create_unix_socket, dof, full_mode, mode, output, set_mode,
    stderr, stdout,
};

#[test]
fn apply_help_describes_a_zero_argument_command() {
    let root_help = output(Command::new(binary()).arg("--help"));
    assert!(root_help.status.success(), "{}", stderr(&root_help));
    assert!(stdout(&root_help).contains("Apply enabled feature resources to the home directory"));

    let help = output(Command::new(binary()).args(["apply", "--help"]));
    assert!(help.status.success(), "{}", stderr(&help));
    assert!(stdout(&help).contains("Usage: dof apply"));

    let extra = output(Command::new(binary()).args(["apply", "workspace"]));
    assert!(!extra.status.success());
    assert!(stderr(&extra).contains("unexpected argument 'workspace'"));
}

#[test]
fn applies_enabled_features_with_nested_hidden_and_empty_paths() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  explicitly-enabled: true
  disabled: false
"#,
    );

    let default = fixture.feature_home("default");
    fs::write(default.join(".profile"), "export DOF=1\n").unwrap();
    fs::create_dir_all(default.join(".config/tool/empty")).unwrap();
    fs::write(
        default.join(".config/tool/settings.yaml"),
        "enabled: true\n",
    )
    .unwrap();
    fixture.create_feature("no-home");

    let enabled = fixture.feature_home("explicitly-enabled");
    fs::create_dir_all(enabled.join("bin")).unwrap();
    let executable = enabled.join("bin/dof-helper");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    set_mode(&executable, 0o751);

    let disabled = fixture.feature_home("disabled");
    fs::write(disabled.join("disabled.txt"), "must not be installed\n").unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(fixture.home.join(".profile")).unwrap(),
        "export DOF=1\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join(".config/tool/settings.yaml")).unwrap(),
        "enabled: true\n"
    );
    assert!(fixture.home.join(".config/tool/empty").is_dir());
    assert_eq!(
        fs::read_to_string(fixture.home.join("bin/dof-helper")).unwrap(),
        "#!/bin/sh\nexit 0\n"
    );
    assert_eq!(mode(&fixture.home.join("bin/dof-helper")), 0o751);
    assert!(!fixture.home.join("disabled.txt").exists());
    assert_summary(&result, 3, 0);
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn apply_runs_only_default_and_explicitly_enabled_features() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  explicitly-enabled: true
  disabled: false
"#,
    );

    for (feature, filename) in [
        ("default", "default.txt"),
        ("explicitly-enabled", "explicit.txt"),
        ("macos-only", "macos.txt"),
        ("disabled", "disabled.txt"),
    ] {
        fs::write(fixture.feature_home(feature).join(filename), feature).unwrap();
        fixture.write_apply_script(
            feature,
            &format!("#!/bin/sh\nprintf '{feature}\\n' >> \"$HOME/script-runs\"\n"),
        );
    }
    fs::create_dir(fixture.feature_home("macos-only").join("macos-directory")).unwrap();
    fixture.write_snippets(
        "macos-only",
        "snippets:\n  macos-snippet:\n    - 'must not be appended'\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 2, 0);
    assert!(fixture.home.join("default.txt").is_file());
    assert!(fixture.home.join("explicit.txt").is_file());
    assert!(!fixture.home.join("macos.txt").exists());
    assert!(!fixture.home.join("disabled.txt").exists());
    assert!(!fixture.home.join("macos-directory").exists());
    assert!(!fixture.home.join("macos-snippet").exists());
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "default\nexplicitly-enabled\n"
    );
}

#[test]
fn explicit_false_disables_default_across_every_resource_kind() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  default: false
  opt-in: true
"#,
    );

    let default_home = fixture.feature_home("default");
    fs::write(default_home.join("default.txt"), "must not be copied\n").unwrap();
    fs::create_dir(default_home.join("default-directory")).unwrap();
    fixture.write_snippets(
        "default",
        "snippets:\n  default-snippet:\n    - 'must not be appended'\n",
    );
    fixture.write_apply_script(
        "default",
        "#!/bin/sh\nprintf 'default\\n' >> \"$HOME/script-runs\"\n",
    );

    fs::write(
        fixture.feature_home("opt-in").join("opt-in.txt"),
        "enabled\n",
    )
    .unwrap();
    fixture.write_apply_script(
        "opt-in",
        "#!/bin/sh\nprintf 'opt-in\\n' >> \"$HOME/script-runs\"\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 1, 0);
    assert_eq!(
        fs::read_to_string(fixture.home.join("opt-in.txt")).unwrap(),
        "enabled\n"
    );
    assert!(!fixture.home.join("default.txt").exists());
    assert!(!fixture.home.join("default-directory").exists());
    assert!(!fixture.home.join("default-snippet").exists());
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "opt-in\n"
    );
}

#[test]
fn omitted_features_do_not_contribute_to_shared_snippet_targets() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "default",
        "snippets:\n  .profile:\n    - 'default snippet'\n",
    );
    fixture.write_snippets(
        "macos-gui",
        "snippets:\n  .profile:\n    - 'macOS snippet'\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 1, 0);
    assert_eq!(
        fs::read_to_string(fixture.home.join(".profile")).unwrap(),
        "default snippet\n"
    );
}

#[test]
fn feature_commands_drive_list_and_apply_selection_end_to_end() {
    let fixture = Fixture::new();
    let default_source = fixture.feature_home("default").join("default.txt");
    fs::write(&default_source, "default\n").unwrap();
    fixture.write_apply_script(
        "default",
        "#!/bin/sh\nprintf 'default\\n' >> \"$HOME/script-runs\"\n",
    );
    let source = fixture.feature_home("hostname").join("hostname.txt");
    fs::write(&source, "first\n").unwrap();
    fixture.write_apply_script(
        "hostname",
        "#!/bin/sh\nprintf 'hostname\\n' >> \"$HOME/script-runs\"\n",
    );

    let initial = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(initial.status.success(), "{}", stderr(&initial));
    assert_eq!(stdout(&initial), "[\"default\"]\n");

    let default_only = output(dof(&fixture.home).arg("apply"));
    assert!(default_only.status.success(), "{}", stderr(&default_only));
    assert_summary(&default_only, 1, 0);
    assert_eq!(
        fs::read_to_string(fixture.home.join("default.txt")).unwrap(),
        "default\n"
    );
    assert!(!fixture.home.join("hostname.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "default\n"
    );

    let enable = output(dof(&fixture.home).args(["feature", "enable", "hostname"]));
    assert!(enable.status.success(), "{}", stderr(&enable));
    let both = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(both.status.success(), "{}", stderr(&both));
    assert_eq!(stdout(&both), "[\"default\",\"hostname\"]\n");

    let default_and_opt_in = output(dof(&fixture.home).arg("apply"));
    assert!(
        default_and_opt_in.status.success(),
        "{}",
        stderr(&default_and_opt_in)
    );
    assert_summary(&default_and_opt_in, 1, 1);
    assert_eq!(
        fs::read_to_string(fixture.home.join("default.txt")).unwrap(),
        "default\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("hostname.txt")).unwrap(),
        "first\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "default\ndefault\nhostname\n"
    );

    fs::write(&default_source, "default changed while disabled\n").unwrap();
    fs::write(&source, "second\n").unwrap();
    let disable_default = output(dof(&fixture.home).args(["feature", "disable", "default"]));
    assert!(
        disable_default.status.success(),
        "{}",
        stderr(&disable_default)
    );
    let hostname_only = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(hostname_only.status.success(), "{}", stderr(&hostname_only));
    assert_eq!(stdout(&hostname_only), "[\"hostname\"]\n");

    let opt_in_only = output(dof(&fixture.home).arg("apply"));
    assert!(opt_in_only.status.success(), "{}", stderr(&opt_in_only));
    assert_summary(&opt_in_only, 1, 0);
    assert_eq!(
        fs::read_to_string(fixture.home.join("default.txt")).unwrap(),
        "default\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("hostname.txt")).unwrap(),
        "second\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "default\ndefault\nhostname\nhostname\n"
    );

    fs::write(&source, "third while disabled\n").unwrap();
    let disable = output(dof(&fixture.home).args(["feature", "disable", "hostname"]));
    assert!(disable.status.success(), "{}", stderr(&disable));
    let none = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(none.status.success(), "{}", stderr(&none));
    assert_eq!(stdout(&none), "[]\n");

    let disabled = output(dof(&fixture.home).arg("apply"));
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    assert_summary(&disabled, 0, 0);
    assert_eq!(
        fs::read_to_string(fixture.home.join("default.txt")).unwrap(),
        "default\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("hostname.txt")).unwrap(),
        "second\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "default\ndefault\nhostname\nhostname\n"
    );
}

#[test]
fn backs_up_changed_files_once_and_skips_identical_files() {
    let fixture = Fixture::new();
    let source = fixture.feature_home("default");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::create_dir_all(source.join(".local/share")).unwrap();

    let changed_one = source.join(".config/app/one.conf");
    fs::write(&changed_one, "new one\n").unwrap();
    set_mode(&changed_one, 0o644);
    let changed_two = source.join(".local/share/two.txt");
    fs::write(&changed_two, "new two\n").unwrap();
    set_mode(&changed_two, 0o600);
    let unchanged = source.join("unchanged.txt");
    fs::write(&unchanged, "same\n").unwrap();
    set_mode(&unchanged, 0o640);
    fs::write(source.join("brand-new.txt"), "new\n").unwrap();
    let mode_only = source.join("mode-only.txt");
    fs::write(&mode_only, "same contents\n").unwrap();
    set_mode(&mode_only, 0o600);

    fs::create_dir_all(fixture.home.join(".config/app")).unwrap();
    fs::create_dir_all(fixture.home.join(".local/share")).unwrap();
    let old_one = fixture.home.join(".config/app/one.conf");
    fs::write(&old_one, "old one\n").unwrap();
    set_mode(&old_one, 0o600);
    let old_two = fixture.home.join(".local/share/two.txt");
    fs::write(&old_two, "old two\n").unwrap();
    set_mode(&old_two, 0o644);
    let old_unchanged = fixture.home.join("unchanged.txt");
    fs::write(&old_unchanged, "same\n").unwrap();
    set_mode(&old_unchanged, 0o640);
    let old_mode_only = fixture.home.join("mode-only.txt");
    fs::write(&old_mode_only, "same contents\n").unwrap();
    set_mode(&old_mode_only, 0o4644);
    assert_eq!(full_mode(&old_mode_only), 0o4644);

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 4, 1);
    assert_eq!(fs::read_to_string(&old_one).unwrap(), "new one\n");
    assert_eq!(mode(&old_one), 0o644);
    assert_eq!(fs::read_to_string(&old_two).unwrap(), "new two\n");
    assert_eq!(mode(&old_two), 0o600);
    assert_eq!(fs::read_to_string(&old_unchanged).unwrap(), "same\n");
    assert_eq!(mode(&old_mode_only), 0o600);
    assert_eq!(
        fs::read_to_string(fixture.home.join("brand-new.txt")).unwrap(),
        "new\n"
    );

    let snapshots = backup_snapshots(&fixture);
    assert_eq!(snapshots.len(), 1, "expected one backup snapshot");
    let snapshot = &snapshots[0];
    assert_eq!(
        fs::read_to_string(snapshot.join(".config/app/one.conf")).unwrap(),
        "old one\n"
    );
    assert_eq!(
        fs::read_to_string(snapshot.join(".local/share/two.txt")).unwrap(),
        "old two\n"
    );
    assert!(!snapshot.join("unchanged.txt").exists());
    assert!(!snapshot.join("brand-new.txt").exists());
    assert_eq!(
        fs::read_to_string(snapshot.join("mode-only.txt")).unwrap(),
        "same contents\n"
    );
    assert_eq!(full_mode(&snapshot.join("mode-only.txt")), 0o4644);
    assert_eq!(mode(&fixture.home.join(".dof/backups")), 0o700);
    assert_eq!(mode(snapshot), 0o700);
    assert!(valid_snapshot_name(
        snapshot.file_name().unwrap().to_str().unwrap()
    ));
    assert!(stdout(&result).contains(&snapshot.display().to_string()));
}

#[test]
fn apply_validates_disabled_feature_collisions_before_mutating_home() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  a-enabled: true
  z-disabled: false
"#,
    );
    let enabled = fixture.feature_home("a-enabled");
    let disabled = fixture.feature_home("z-disabled");
    fs::write(enabled.join("collision"), "enabled\n").unwrap();
    fs::write(disabled.join("collision"), "disabled\n").unwrap();
    fs::write(fixture.home.join("sentinel"), "old\n").unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("file in both feature"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("sentinel")).unwrap(),
        "old\n"
    );
    assert!(!fixture.home.join("collision").exists());
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn omitted_invalid_feature_is_validated_before_default_mutates_home() {
    let fixture = Fixture::new();
    fs::write(fixture.feature_home("default").join("sentinel"), "new\n").unwrap();
    fs::write(fixture.home.join("sentinel"), "old\n").unwrap();
    fixture.write_snippets(
        "z-omitted-invalid",
        "snippets:\n  .profile: this-must-be-an-array\n",
    );

    let enabled = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(enabled.status.success(), "{}", stderr(&enabled));
    assert_eq!(stdout(&enabled), "[\"default\"]\n");

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("z-omitted-invalid"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("sentinel")).unwrap(),
        "old\n"
    );
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn empty_workspace_applies_nothing() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.workspace.join("default/home")).unwrap();
    fs::write(
        fixture.workspace.join("default/home/legacy.txt"),
        "legacy layout\n",
    )
    .unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(stdout(&result), "applied: 0\nunchanged: 0\n");
    assert!(!fixture.home.join("legacy.txt").exists());
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn a_symlinked_features_directory_fails_before_mutating_home() {
    let fixture = Fixture::new();
    let external = fixture.root.path().join("external-features");
    fs::create_dir_all(external.join("default/home")).unwrap();
    fs::write(external.join("default/home/sentinel"), "new\n").unwrap();
    fs::write(fixture.home.join("sentinel"), "old\n").unwrap();
    symlink(&external, &fixture.features).unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("features directory"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("sentinel")).unwrap(),
        "old\n"
    );
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn enabled_feature_scripts_run_every_time_in_lexical_order() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  middle-disabled: false
  zeta: true
"#,
    );

    let alpha = fixture.feature("alpha");
    fs::create_dir(alpha.join("home")).unwrap();
    fs::write(alpha.join("home/managed.txt"), "managed\n").unwrap();
    fixture.write_apply_script(
        "alpha",
        "#!/bin/sh\nprintf 'alpha:%s\\n' \"$PWD\" >> \"$HOME/script-runs\"\n",
    );
    fixture.write_apply_script(
        "middle-disabled",
        "#!/bin/sh\nprintf 'disabled\\n' >> \"$HOME/script-runs\"\n",
    );
    fixture.write_apply_script(
        "zeta",
        "#!/bin/sh\nprintf 'zeta:%s\\n' \"$PWD\" >> \"$HOME/script-runs\"\n",
    );

    let first = output(dof(&fixture.home).arg("apply"));
    assert!(first.status.success(), "{}", stderr(&first));
    assert_summary(&first, 1, 0);

    let second = output(dof(&fixture.home).arg("apply"));
    assert!(second.status.success(), "{}", stderr(&second));
    assert_summary(&second, 0, 1);

    let alpha_directory = fixture.feature("alpha").canonicalize().unwrap();
    let zeta_directory = fixture.feature("zeta").canonicalize().unwrap();
    let expected = format!(
        "alpha:{}\nzeta:{}\nalpha:{}\nzeta:{}\n",
        alpha_directory.display(),
        zeta_directory.display(),
        alpha_directory.display(),
        zeta_directory.display(),
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        expected
    );
}

#[test]
fn feature_script_runs_with_a_relative_home() {
    let fixture = Fixture::new();
    let marker = fixture.home.join("script-ran");
    fixture.write_apply_script(
        "default",
        "#!/bin/sh\nprintf 'ran\n' > \"$DOF_APPLY_TEST_MARKER\"\n",
    );
    let current_directory = fixture.root.path();
    let relative_home = fixture.home.strip_prefix(current_directory).unwrap();

    let result = output(
        Command::new(binary())
            .current_dir(current_directory)
            .env("HOME", relative_home)
            .env("DOF_APPLY_TEST_MARKER", &marker)
            .arg("apply"),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(fs::read_to_string(marker).unwrap(), "ran\n");
}

#[test]
fn failing_feature_script_preserves_backups_and_stops_later_scripts_after_file_sync() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  zeta: true
"#,
    );
    let zeta = fixture.feature_home("zeta");
    let destination = fixture.home.join("managed-before-scripts.txt");
    fs::write(zeta.join("managed-before-scripts.txt"), "managed\n").unwrap();
    fs::write(&destination, "original\n").unwrap();
    fixture.write_apply_script(
        "alpha",
        "#!/bin/sh\nprintf 'alpha\\n' >> \"$HOME/script-runs\"\nexit 23\n",
    );
    fixture.write_apply_script(
        "zeta",
        "#!/bin/sh\nprintf 'zeta\\n' >> \"$HOME/script-runs\"\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("alpha"), "{}", stderr(&result));
    assert_eq!(fs::read_to_string(&destination).unwrap(), "managed\n");
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "alpha\n"
    );
    let snapshots = backup_snapshots(&fixture);
    assert_eq!(
        snapshots.len(),
        1,
        "failed applies must retain their backup"
    );
    assert_eq!(
        fs::read_to_string(snapshots[0].join("managed-before-scripts.txt")).unwrap(),
        "original\n"
    );
}

#[test]
fn later_scripts_are_revalidated_immediately_before_execution() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  zeta: true
"#,
    );
    fixture.write_apply_script(
        "alpha",
        "#!/bin/sh\nprintf 'alpha\n' >> \"$HOME/script-runs\"\nchmod 600 \"$PWD/../zeta/apply\"\n",
    );
    fixture.write_apply_script(
        "zeta",
        "#!/bin/sh\nprintf 'zeta\n' >> \"$HOME/script-runs\"\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("zeta"), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("not executable"),
        "{}",
        stderr(&result)
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("script-runs")).unwrap(),
        "alpha\n"
    );
}

#[test]
fn symlinked_backup_root_fails_before_mutating_home() {
    let fixture = Fixture::new();
    let source = fixture.feature_home("default");
    fs::write(source.join("sentinel"), "new\n").unwrap();
    fs::create_dir(source.join("new-parent")).unwrap();
    fs::write(source.join("new-parent/new-file"), "new\n").unwrap();
    fs::write(fixture.home.join("sentinel"), "old\n").unwrap();
    let external = fixture.root.path().join("external-backups");
    fs::create_dir(&external).unwrap();
    symlink(&external, fixture.home.join(".dof/backups")).unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(stderr(&result).contains("backup path"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("sentinel")).unwrap(),
        "old\n"
    );
    assert!(!fixture.home.join("new-parent").exists());
    assert!(fs::read_dir(external).unwrap().next().is_none());
}

#[test]
fn managed_state_symlinks_are_rejected_by_apply() {
    let state_root = tempfile::tempdir().unwrap();
    let home = state_root.path().join("home");
    let external_state = state_root.path().join("external-state");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&external_state).unwrap();
    symlink(&external_state, home.join(".dof")).unwrap();
    let result = output(dof(&home).arg("apply"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("dof state directory"));

    let config = Fixture::new();
    let external_config = config.root.path().join("external-config.yaml");
    fs::write(&external_config, "features: {}\n").unwrap();
    fs::remove_file(&config.config).unwrap();
    symlink(&external_config, &config.config).unwrap();
    let result = output(dof(&config.home).arg("apply"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("config"));
    assert!(stderr(&result).contains("not a real file"));

    let workspace = Fixture::new();
    let external_workspace = workspace.root.path().join("external-workspace");
    fs::create_dir(&external_workspace).unwrap();
    fs::remove_dir(&workspace.workspace).unwrap();
    symlink(&external_workspace, &workspace.workspace).unwrap();
    let result = output(dof(&workspace.home).arg("apply"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("workspace"));
    assert!(stderr(&result).contains("not a real directory"));
}

#[test]
fn backs_up_a_destination_leaf_symlink_without_following_it() {
    let fixture = Fixture::new();
    let source = fixture.feature_home("default");
    fs::write(source.join("linked.txt"), "managed\n").unwrap();

    let external = fixture.root.path().join("external.txt");
    fs::write(&external, "external\n").unwrap();
    let destination = fixture.home.join("linked.txt");
    symlink(&external, &destination).unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 1, 0);
    assert_eq!(fs::read_to_string(&destination).unwrap(), "managed\n");
    assert!(
        !fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&external).unwrap(), "external\n");

    let snapshots = backup_snapshots(&fixture);
    assert_eq!(snapshots.len(), 1);
    let backed_up_link = snapshots[0].join("linked.txt");
    assert!(
        fs::symlink_metadata(&backed_up_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(backed_up_link).unwrap(), external);
}

#[test]
fn source_symlinks_and_special_files_fail_during_preflight() {
    let symlink_fixture = Fixture::new();
    symlink_fixture.write_config(config_with_disabled_bad_feature());
    prepare_preflight_sentinel(&symlink_fixture);
    let bad_home = symlink_fixture.feature_home("z-bad");
    let external = symlink_fixture.root.path().join("external-source");
    fs::write(&external, "outside\n").unwrap();
    symlink(&external, bad_home.join("linked")).unwrap();

    let result = output(dof(&symlink_fixture.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&symlink_fixture);

    let special_fixture = Fixture::new();
    special_fixture.write_config(config_with_disabled_bad_feature());
    prepare_preflight_sentinel(&special_fixture);
    let bad_home = special_fixture.feature_home("z-bad");
    let _socket = create_unix_socket(&bad_home.join("socket"));

    let result = output(dof(&special_fixture.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&special_fixture);
}

#[test]
fn destination_ancestor_symlinks_fail_during_preflight() {
    let fixture = Fixture::new();
    fixture.write_config(&config_with_enabled_safe_and("z-feature"));
    prepare_preflight_sentinel(&fixture);
    let source = fixture.feature_home("z-feature");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::write(source.join(".config/app/settings"), "managed\n").unwrap();

    let external = fixture.root.path().join("external-config");
    fs::create_dir(&external).unwrap();
    symlink(&external, fixture.home.join(".config")).unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert!(!external.join("app/settings").exists());
    assert_preflight_left_home_unchanged(&fixture);
}

#[test]
fn destination_file_directory_shape_conflicts_fail_during_preflight() {
    let ancestor_file = Fixture::new();
    ancestor_file.write_config(&config_with_enabled_safe_and("z-feature"));
    prepare_preflight_sentinel(&ancestor_file);
    let source = ancestor_file.feature_home("z-feature");
    fs::create_dir_all(source.join(".config/app")).unwrap();
    fs::write(source.join(".config/app/settings"), "managed\n").unwrap();
    fs::create_dir(ancestor_file.home.join(".config")).unwrap();
    fs::write(ancestor_file.home.join(".config/app"), "not a directory\n").unwrap();

    let result = output(dof(&ancestor_file.home).arg("apply"));
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(ancestor_file.home.join(".config/app")).unwrap(),
        "not a directory\n"
    );
    assert_preflight_left_home_unchanged(&ancestor_file);

    let leaf_directory = Fixture::new();
    leaf_directory.write_config(&config_with_enabled_safe_and("z-feature"));
    prepare_preflight_sentinel(&leaf_directory);
    let source = leaf_directory.feature_home("z-feature");
    fs::write(source.join("destination"), "managed\n").unwrap();
    fs::create_dir(leaf_directory.home.join("destination")).unwrap();

    let result = output(dof(&leaf_directory.home).arg("apply"));
    assert!(!result.status.success());
    assert!(leaf_directory.home.join("destination").is_dir());
    assert_preflight_left_home_unchanged(&leaf_directory);
}

#[test]
fn invalid_state_fails_without_changing_home() {
    let missing_home = output(Command::new(binary()).env_remove("HOME").arg("apply"));
    assert!(!missing_home.status.success());
    assert!(stderr(&missing_home).contains("HOME is not set or is empty"));

    let missing_config = Fixture::new();
    fs::remove_file(&missing_config.config).unwrap();
    let result = output(dof(&missing_config.home).arg("apply"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to read dof config"));
    assert!(backup_snapshots(&missing_config).is_empty());

    let malformed = Fixture::new();
    malformed.write_config("features: [not, an, object]\n");
    let result = output(dof(&malformed.home).arg("apply"));
    assert!(!result.status.success());
    assert!(stderr(&result).contains("failed to parse dof config"));
    assert!(backup_snapshots(&malformed).is_empty());
}

#[test]
fn applies_multiline_snippets_exactly_once() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "default",
        r#"snippets:
  .bashrc:
    - 'export EDITOR=vim'
    - |-
      if command -v mise >/dev/null 2>&1; then
        eval "$(mise activate bash)"
      fi
"#,
    );
    let target = fixture.home.join(".bashrc");
    fs::write(&target, "# existing configuration\nexport EDITOR=vim\n").unwrap();
    set_mode(&target, 0o640);

    let first = output(dof(&fixture.home).arg("apply"));

    assert!(first.status.success(), "{}", stderr(&first));
    let expected_block =
        "if command -v mise >/dev/null 2>&1; then\n  eval \"$(mise activate bash)\"\nfi";
    let after_first = fs::read_to_string(fixture.home.join(".bashrc")).unwrap();
    assert_eq!(after_first.matches("export EDITOR=vim").count(), 1);
    assert_eq!(after_first.matches(expected_block).count(), 1);
    assert_eq!(mode(&target), 0o640);

    let second = output(dof(&fixture.home).arg("apply"));

    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(
        fs::read_to_string(fixture.home.join(".bashrc")).unwrap(),
        after_first,
        "an exact multiline substring must make the second apply idempotent"
    );
    assert_eq!(mode(&target), 0o640);
}

#[test]
fn snippet_append_uses_newline_boundaries() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "default",
        r#"snippets:
  .profile:
    - 'export EDITOR=vim'
    - |-
      if command -v starship >/dev/null 2>&1; then
        eval "$(starship init bash)"
      fi
"#,
    );
    fs::write(fixture.home.join(".profile"), "existing-without-newline").unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(fixture.home.join(".profile")).unwrap(),
        concat!(
            "existing-without-newline\n",
            "export EDITOR=vim\n",
            "if command -v starship >/dev/null 2>&1; then\n",
            "  eval \"$(starship init bash)\"\n",
            "fi\n"
        )
    );
}

#[test]
fn snippet_target_aliases_are_normalized_before_filesystem_operations() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "default",
        "snippets:\n  aliased//path/./target:\n    - managed\n  trailing/.:\n    - normalized\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(fixture.home.join("aliased/path/target")).unwrap(),
        "managed\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("trailing")).unwrap(),
        "normalized\n"
    );
    assert!(
        fs::symlink_metadata(fixture.home.join("trailing"))
            .unwrap()
            .file_type()
            .is_file()
    );
}

#[test]
fn applies_enabled_snippets_and_ignores_disabled_features() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  explicit: true
  disabled: false
"#,
    );
    fixture.write_snippets(
        "default",
        "snippets:\n  .profile:\n    - 'enabled default'\n",
    );
    fixture.write_snippets(
        "explicit",
        "snippets:\n  .profile:\n    - 'enabled explicit'\n",
    );
    fixture.write_snippets(
        "disabled",
        "snippets:\n  .profile:\n    - 'must not be appended'\n",
    );

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    let contents = fs::read_to_string(fixture.home.join(".profile")).unwrap();
    assert!(contents.contains("enabled default"));
    assert!(contents.contains("enabled explicit"));
    assert!(!contents.contains("must not be appended"));
}

#[test]
fn multiple_features_can_append_snippets_to_one_target() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  zeta: true
"#,
    );
    fixture.write_snippets(
        "alpha",
        "snippets:\n  .config/tool.conf:\n    - 'alpha = true'\n    - 'shared = true'\n",
    );
    fixture.write_snippets(
        "zeta",
        "snippets:\n  .config/tool.conf:\n    - 'shared = true'\n    - 'zeta = true'\n",
    );

    let first = output(dof(&fixture.home).arg("apply"));

    assert!(first.status.success(), "{}", stderr(&first));
    let target = fixture.home.join(".config/tool.conf");
    let contents = fs::read_to_string(&target).unwrap();
    assert_eq!(contents.matches("alpha = true").count(), 1);
    assert_eq!(contents.matches("shared = true").count(), 1);
    assert_eq!(contents.matches("zeta = true").count(), 1);
    assert_eq!(mode(&target), 0o600);
    assert!(
        contents.find("alpha = true").unwrap() < contents.find("zeta = true").unwrap(),
        "features should contribute snippets in deterministic feature order"
    );
    assert!(
        backup_snapshots(&fixture).is_empty(),
        "creating a snippet target must not create a backup"
    );

    let second = output(dof(&fixture.home).arg("apply"));
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(fs::read_to_string(target).unwrap(), contents);
}

#[test]
fn snippet_changes_share_the_apply_backup_snapshot() {
    let fixture = Fixture::new();
    fixture.write_snippets(
        "default",
        r#"snippets:
  .bashrc:
    - 'export DOF=1'
  .config/tool/config:
    - 'managed = true'
"#,
    );
    fs::write(fixture.home.join(".bashrc"), "original bashrc\n").unwrap();
    fs::create_dir_all(fixture.home.join(".config/tool")).unwrap();
    fs::write(
        fixture.home.join(".config/tool/config"),
        "original config\n",
    )
    .unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    let snapshots = backup_snapshots(&fixture);
    assert_eq!(
        snapshots.len(),
        1,
        "all snippet edits should share a snapshot"
    );
    assert_eq!(
        fs::read_to_string(snapshots[0].join(".bashrc")).unwrap(),
        "original bashrc\n"
    );
    assert_eq!(
        fs::read_to_string(snapshots[0].join(".config/tool/config")).unwrap(),
        "original config\n"
    );

    let second = output(dof(&fixture.home).arg("apply"));
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(backup_snapshots(&fixture), snapshots);
}

#[test]
fn invalid_or_unsafe_snippets_fail_before_home_is_mutated() {
    let invalid = Fixture::new();
    invalid.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  a-safe: true
  z-invalid: false
"#,
    );
    let safe_home = invalid.feature_home("a-safe");
    fs::write(safe_home.join("sentinel"), "new\n").unwrap();
    fs::write(invalid.home.join("sentinel"), "old\n").unwrap();
    invalid.write_snippets("z-invalid", "snippets:\n  .bashrc: this-must-be-an-array\n");

    let result = output(dof(&invalid.home).arg("apply"));
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(invalid.home.join("sentinel")).unwrap(),
        "old\n"
    );
    assert!(backup_snapshots(&invalid).is_empty());

    let symlinked_ancestor = Fixture::new();
    symlinked_ancestor.write_snippets(
        "default",
        "snippets:\n  .config/tool.conf:\n    - 'managed = true'\n",
    );
    let external = symlinked_ancestor.root.path().join("external-config");
    fs::create_dir(&external).unwrap();
    symlink(&external, symlinked_ancestor.home.join(".config")).unwrap();

    let result = output(dof(&symlinked_ancestor.home).arg("apply"));
    assert!(!result.status.success());
    assert!(!external.join("tool.conf").exists());
    assert!(backup_snapshots(&symlinked_ancestor).is_empty());

    let symlinked_leaf = Fixture::new();
    symlinked_leaf.write_snippets(
        "default",
        "snippets:\n  .profile:\n    - 'managed = true'\n",
    );
    let external = symlinked_leaf.root.path().join("external-profile");
    fs::write(&external, "external\n").unwrap();
    symlink(&external, symlinked_leaf.home.join(".profile")).unwrap();

    let result = output(dof(&symlinked_leaf.home).arg("apply"));
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(external).unwrap(), "external\n");
    assert!(
        fs::symlink_metadata(symlinked_leaf.home.join(".profile"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(backup_snapshots(&symlinked_leaf).is_empty());
}

#[test]
fn copy_and_snippet_collision_fails_apply_before_mutation() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  copy-owner: true
  snippet-owner: false
"#,
    );
    fs::write(
        fixture.feature_home("copy-owner").join(".profile"),
        "copied\n",
    )
    .unwrap();
    fixture.write_snippets(
        "snippet-owner",
        "snippets:\n  .profile:\n    - 'snippet managed'\n",
    );
    fs::write(fixture.home.join(".profile"), "original\n").unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(fixture.home.join(".profile")).unwrap(),
        "original\n"
    );
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn drop_ins_compile_enabled_features_by_global_order_with_exact_bytes() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  disabled: false
  zeta: true
"#,
    );
    fixture.write_drop_in("zeta", ".Brewfile.d", "10-base", "tap \"base\"\r\n\n");
    fixture.write_drop_in(
        "disabled",
        ".Brewfile.d",
        "20-disabled",
        "brew \"disabled\"\n",
    );
    fixture.write_drop_in("omitted", ".Brewfile.d", "50-omitted", "brew \"omitted\"\n");
    fixture.write_drop_in("alpha", ".Brewfile.d", "90-personal", "brew \"personal\"\n");

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 1, 0);
    let target = fixture.home.join(".Brewfile");
    assert_eq!(
        fs::read(&target).unwrap(),
        b"tap \"base\"\r\n\nbrew \"personal\"\n"
    );
    assert_eq!(mode(&target), 0o600);
    assert!(backup_snapshots(&fixture).is_empty());
}

#[test]
fn drop_ins_rebuild_for_active_contributors_but_leave_the_last_output() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  beta: true
"#,
    );
    let alpha = fixture.write_drop_in("alpha", ".profile.d", "10-alpha", "alpha\n");
    let beta = fixture.write_drop_in("beta", ".profile.d", "20-beta", "beta\n");
    let target = fixture.home.join(".profile");

    let first = output(dof(&fixture.home).arg("apply"));
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(fs::read_to_string(&target).unwrap(), "alpha\nbeta\n");

    fs::write(&alpha, "updated alpha\n").unwrap();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: true
  beta: false
"#,
    );
    let second = output(dof(&fixture.home).arg("apply"));
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(fs::read_to_string(&target).unwrap(), "updated alpha\n");

    let retained_inode = fs::metadata(&target).unwrap().ino();
    let retained_snapshots = backup_snapshots(&fixture);
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  alpha: false
  beta: false
"#,
    );
    let disabled = output(dof(&fixture.home).arg("apply"));
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    assert_summary(&disabled, 0, 0);
    assert_eq!(fs::read_to_string(&target).unwrap(), "updated alpha\n");
    assert_eq!(fs::metadata(&target).unwrap().ino(), retained_inode);
    assert_eq!(backup_snapshots(&fixture), retained_snapshots);

    fs::remove_file(alpha).unwrap();
    fs::remove_file(beta).unwrap();
    fs::remove_dir(fixture.feature("alpha").join("drop-ins/.profile.d")).unwrap();
    fs::remove_dir(fixture.feature("beta").join("drop-ins/.profile.d")).unwrap();
    let removed = output(dof(&fixture.home).arg("apply"));
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert_summary(&removed, 0, 0);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "updated alpha\n",
        "dof must not delete an output after its final contributor disappears"
    );
    assert_eq!(fs::metadata(&target).unwrap().ino(), retained_inode);
    assert_eq!(backup_snapshots(&fixture), retained_snapshots);
}

#[test]
fn drop_in_targets_use_modes_backups_atomic_noops_and_a_shared_snapshot() {
    let fixture = Fixture::new();
    fixture.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  copy: true
  snippets: true
"#,
    );
    let new_fragment = fixture.write_drop_in("default", "new.conf.d", "10-base", "new\n");
    set_mode(&new_fragment, 0o777);
    fixture.write_drop_in("default", "changed.conf.d", "10-base", "managed\n");
    fixture.write_drop_in("default", "same.conf.d", "10-base", "same\n");
    fixture.write_drop_in("default", "linked.conf.d", "10-base", "replacement\n");

    let changed = fixture.home.join("changed.conf");
    fs::write(&changed, "original\n").unwrap();
    set_mode(&changed, 0o640);
    let same = fixture.home.join("same.conf");
    fs::write(&same, "same\n").unwrap();
    set_mode(&same, 0o604);
    let same_inode = fs::metadata(&same).unwrap().ino();

    let external = fixture.root.path().join("external-linked.conf");
    fs::write(&external, "external\n").unwrap();
    let linked = fixture.home.join("linked.conf");
    symlink(&external, &linked).unwrap();

    let copy_source = fixture.feature_home("copy").join("copied.conf");
    fs::write(&copy_source, "new copy\n").unwrap();
    fs::write(fixture.home.join("copied.conf"), "old copy\n").unwrap();
    fixture.write_snippets(
        "snippets",
        "snippets:\n  snippet.conf:\n    - 'managed snippet'\n",
    );
    fs::write(fixture.home.join("snippet.conf"), "old snippet\n").unwrap();

    let result = output(dof(&fixture.home).arg("apply"));

    assert!(result.status.success(), "{}", stderr(&result));
    assert_summary(&result, 5, 1);
    assert_eq!(
        fs::read_to_string(fixture.home.join("new.conf")).unwrap(),
        "new\n"
    );
    assert_eq!(mode(&fixture.home.join("new.conf")), 0o600);
    assert_eq!(fs::read_to_string(&changed).unwrap(), "managed\n");
    assert_eq!(mode(&changed), 0o640);
    assert_eq!(fs::metadata(&same).unwrap().ino(), same_inode);
    assert_eq!(mode(&same), 0o604);
    assert_eq!(fs::read_to_string(&linked).unwrap(), "replacement\n");
    assert_eq!(mode(&linked), 0o600);
    assert_eq!(fs::read_to_string(&external).unwrap(), "external\n");

    let snapshots = backup_snapshots(&fixture);
    assert_eq!(
        snapshots.len(),
        1,
        "all resource kinds must share a snapshot"
    );
    let snapshot = &snapshots[0];
    assert_eq!(
        fs::read_to_string(snapshot.join("changed.conf")).unwrap(),
        "original\n"
    );
    assert_eq!(mode(&snapshot.join("changed.conf")), 0o640);
    assert!(!snapshot.join("new.conf").exists());
    assert!(!snapshot.join("same.conf").exists());
    let backed_up_link = snapshot.join("linked.conf");
    assert!(
        fs::symlink_metadata(&backed_up_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(backed_up_link).unwrap(), external);
    assert_eq!(
        fs::read_to_string(snapshot.join("copied.conf")).unwrap(),
        "old copy\n"
    );
    assert_eq!(
        fs::read_to_string(snapshot.join("snippet.conf")).unwrap(),
        "old snippet\n"
    );
}

#[test]
fn nested_drop_ins_apply_with_relative_home_before_feature_hooks() {
    let fixture = Fixture::new();
    fixture.write_drop_in(
        "default",
        ".config/systemd/user/example.service.d/override.conf.d",
        "10-base",
        "[Service]\nEnvironment=DOF=1\n",
    );
    let target = fixture
        .home
        .join(".config/systemd/user/example.service.d/override.conf");
    let marker = fixture.home.join("hook-observed-drop-in");
    fixture.write_apply_script(
        "default",
        "#!/bin/sh\ncmp -s \"$DOF_DROP_IN_TARGET\" \"$DOF_DROP_IN_EXPECTED\" && printf 'observed\\n' > \"$DOF_DROP_IN_MARKER\"\n",
    );
    let expected = fixture.root.path().join("expected-drop-in");
    fs::write(&expected, "[Service]\nEnvironment=DOF=1\n").unwrap();
    let relative_home = fixture.home.strip_prefix(fixture.root.path()).unwrap();

    let result = output(
        Command::new(binary())
            .current_dir(fixture.root.path())
            .env("HOME", relative_home)
            .env("DOF_DROP_IN_TARGET", &target)
            .env("DOF_DROP_IN_EXPECTED", &expected)
            .env("DOF_DROP_IN_MARKER", &marker)
            .arg("apply"),
    );

    assert!(result.status.success(), "{}", stderr(&result));
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "[Service]\nEnvironment=DOF=1\n"
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "observed\n");
}

#[test]
fn invalid_disabled_drop_ins_and_ownership_conflicts_fail_before_mutation() {
    let invalid = Fixture::new();
    invalid.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  a-safe: true
  z-invalid: false
"#,
    );
    prepare_preflight_sentinel(&invalid);
    let invalid_root = invalid.feature("z-invalid").join("drop-ins");
    fs::create_dir_all(&invalid_root).unwrap();
    fs::write(invalid_root.join("orphan"), "invalid\n").unwrap();

    let result = output(dof(&invalid.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&invalid);

    let direct = Fixture::new();
    direct.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  a-safe: true
"#,
    );
    prepare_preflight_sentinel(&direct);
    fs::write(direct.feature_home("copy-owner").join(".profile"), "copy\n").unwrap();
    direct.write_drop_in("drop-owner", ".profile.d", "10-base", "drop-in\n");

    let result = output(dof(&direct.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&direct);

    let case_alias = Fixture::new();
    case_alias.write_config(
        r#"repo:
  url: file:///dotfiles
  branch: main
features:
  a-safe: true
"#,
    );
    prepare_preflight_sentinel(&case_alias);
    let copy_home = case_alias.feature_home("copy-owner");
    fs::create_dir_all(copy_home.join(".config")).unwrap();
    fs::write(copy_home.join(".config/other"), "copy\n").unwrap();
    case_alias.write_drop_in("drop-owner", ".Config/tool.conf.d", "10-base", "drop-in\n");

    let result = output(dof(&case_alias.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&case_alias);
}

#[test]
fn invalid_drop_in_destination_shapes_fail_before_any_home_mutation() {
    let ancestor_link = Fixture::new();
    ancestor_link.write_config(&config_with_enabled_safe_and("z-drop-in"));
    prepare_preflight_sentinel(&ancestor_link);
    ancestor_link.write_drop_in("z-drop-in", ".config/tool.conf.d", "10-base", "managed\n");
    let external = ancestor_link.root.path().join("external-config");
    fs::create_dir(&external).unwrap();
    symlink(&external, ancestor_link.home.join(".config")).unwrap();
    let result = output(dof(&ancestor_link.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&ancestor_link);

    let leaf_directory = Fixture::new();
    leaf_directory.write_config(&config_with_enabled_safe_and("z-drop-in"));
    prepare_preflight_sentinel(&leaf_directory);
    leaf_directory.write_drop_in("z-drop-in", "settings.conf.d", "10-base", "managed\n");
    fs::create_dir(leaf_directory.home.join("settings.conf")).unwrap();
    let result = output(dof(&leaf_directory.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&leaf_directory);

    let special_leaf = Fixture::new();
    special_leaf.write_config(&config_with_enabled_safe_and("z-drop-in"));
    prepare_preflight_sentinel(&special_leaf);
    special_leaf.write_drop_in("z-drop-in", "agent.sock.d", "10-base", "managed\n");
    let _socket = create_unix_socket(&special_leaf.home.join("agent.sock"));
    let result = output(dof(&special_leaf.home).arg("apply"));
    assert!(!result.status.success());
    assert_preflight_left_home_unchanged(&special_leaf);
}

fn prepare_preflight_sentinel(fixture: &Fixture) {
    let source = fixture.feature_home("a-safe");
    fs::write(source.join("sentinel"), "new\n").unwrap();
    fs::write(fixture.home.join("sentinel"), "old\n").unwrap();

    let enabled = output(dof(&fixture.home).args(["features", "--json"]));
    assert!(enabled.status.success(), "{}", stderr(&enabled));
    let enabled_features: Vec<String> = serde_json::from_str(&stdout(&enabled)).unwrap();
    assert!(
        enabled_features.iter().any(|feature| feature == "a-safe"),
        "preflight sentinel owner must be enabled; got {enabled_features:?}"
    );
}

fn config_with_disabled_bad_feature() -> &'static str {
    r#"repo:
  url: file:///dotfiles
  branch: main
features:
  a-safe: true
  z-bad: false
"#
}

fn config_with_enabled_safe_and(feature: &str) -> String {
    format!(
        "repo:\n  url: file:///dotfiles\n  branch: main\nfeatures:\n  a-safe: true\n  {feature}: true\n"
    )
}

fn assert_preflight_left_home_unchanged(fixture: &Fixture) {
    assert_eq!(
        fs::read_to_string(fixture.home.join("sentinel")).unwrap(),
        "old\n"
    );
    assert!(backup_snapshots(fixture).is_empty());
}

fn assert_summary(result: &Output, applied: usize, unchanged: usize) {
    let output = stdout(result);
    let mut lines = output.lines();
    let expected_applied = format!("applied: {applied}");
    let expected_unchanged = format!("unchanged: {unchanged}");
    assert_eq!(lines.next(), Some(expected_applied.as_str()), "{output:?}");
    assert_eq!(
        lines.next(),
        Some(expected_unchanged.as_str()),
        "{output:?}"
    );
    let remaining = lines.collect::<Vec<_>>();
    assert!(
        remaining.is_empty()
            || (remaining.len() == 1
                && remaining[0]
                    .strip_prefix("backup: ")
                    .is_some_and(|path| !path.is_empty())),
        "unexpected apply summary fields: {output:?}"
    );
}

fn valid_snapshot_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 26
        && bytes[0..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'.'
        && bytes[16..25].iter().all(u8::is_ascii_digit)
        && bytes[25] == b'Z'
}

fn backup_snapshots(fixture: &Fixture) -> Vec<PathBuf> {
    let backups = fixture.home.join(".dof/backups");
    let entries = match fs::read_dir(backups) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("failed to inspect backups: {error}"),
    };
    let mut snapshots = entries
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    snapshots.sort();
    snapshots
}

struct Fixture(ManagedStateFixture);

impl Deref for Fixture {
    type Target = ManagedStateFixture;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Fixture {
    fn new() -> Self {
        Self(ManagedStateFixture::with_default_config())
    }

    fn write_apply_script(&self, feature: &str, contents: &str) {
        let path = self.feature(feature).join("apply");
        fs::write(&path, contents).unwrap();
        set_mode(&path, 0o700);
    }

    fn write_snippets(&self, feature: &str, contents: &str) {
        fs::write(self.feature(feature).join("snippets.yaml"), contents).unwrap();
    }

    fn write_drop_in(
        &self,
        feature: &str,
        target_directory: &str,
        fragment: &str,
        contents: &str,
    ) -> PathBuf {
        let directory = self
            .feature(feature)
            .join("drop-ins")
            .join(target_directory);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(fragment);
        fs::write(&path, contents).unwrap();
        path
    }
}
